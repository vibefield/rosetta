//! The asynchronous ledger: one worker thread owns a private cold ledger's
//! segment IO and checksumming, fed in order through a queue bounded by the
//! bytes it holds rather than by a command count, so payload-free commands
//! (checkpoints, seals, roster keys) never take a producer's room.

use std::collections::HashMap;
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

use super::{Ledger, LedgerWriteStats, PrebuiltFrames, SourceDef};
use crate::CaptureError;

/// Bookkeeping charged per queued command beyond the bytes it carries, so a
/// run of payload-free commands is bounded too.
const COMMAND_BYTES: u64 = 64;

pub(super) enum AsyncCommand {
    Raw {
        source: SourceDef,
        generation: u64,
        records: Vec<(u64, Vec<u8>)>,
    },
    Prebuilt {
        source: SourceDef,
        frames: PrebuiltFrames,
    },
    Complete {
        source: SourceDef,
        generation: u64,
        normalizer_version: u32,
        normalized_clean: bool,
    },
    Meta {
        schema_version: u32,
        payload: String,
    },
    SourceKeys(Arc<HashMap<SourceDef, u32>>),
    Checkpoint,
}

impl AsyncCommand {
    fn queued_bytes(&self) -> u64 {
        let carried = match self {
            Self::Raw { records, .. } => records
                .iter()
                .map(|(_, payload)| payload.len() as u64)
                .sum(),
            Self::Prebuilt { frames, .. } => frames.held_bytes(),
            Self::Meta { payload, .. } => payload.len() as u64,
            Self::Complete { .. } | Self::SourceKeys(_) | Self::Checkpoint => 0,
        };
        carried.saturating_add(COMMAND_BYTES)
    }
}

pub(super) struct AsyncBackend {
    sender: Option<Sender<AsyncCommand>>,
    queue: Arc<QueuedBytes>,
    worker: Option<JoinHandle<Result<Ledger, CaptureError>>>,
}

/// Bytes queued between a ledger's owner and its worker. The owner waits
/// while a command would push them past the limit — unless nothing is queued,
/// so one command larger than the limit still moves alone.
struct QueuedBytes {
    limit: u64,
    state: Mutex<QueueState>,
    drained: Condvar,
}

#[derive(Default)]
struct QueueState {
    queued: u64,
    /// The worker exited; nothing will drain the queue again.
    closed: bool,
}

impl QueuedBytes {
    fn new(limit: u64) -> Self {
        Self {
            limit: limit.max(1),
            state: Mutex::new(QueueState::default()),
            drained: Condvar::new(),
        }
    }

    fn reserve(&self, bytes: u64) {
        let mut state = self.state.lock().expect("ledger queue poisoned");
        while !state.closed && state.queued > 0 && state.queued.saturating_add(bytes) > self.limit {
            state = self.drained.wait(state).expect("ledger queue poisoned");
        }
        state.queued = state.queued.saturating_add(bytes);
    }

    fn release(&self, bytes: u64) {
        let mut state = self.state.lock().expect("ledger queue poisoned");
        state.queued = state.queued.saturating_sub(bytes);
        self.drained.notify_all();
    }

    fn close(&self) {
        let mut state = self.state.lock().expect("ledger queue poisoned");
        state.closed = true;
        self.drained.notify_all();
    }
}

/// Closes the queue however the worker exits — finished, failed or
/// panicked — so an owner waiting for room learns there is none coming.
struct CloseOnExit(Arc<QueuedBytes>);

impl Drop for CloseOnExit {
    fn drop(&mut self) {
        self.0.close();
    }
}

impl Ledger {
    /// Move cold ledger IO/checksumming onto a worker thread. Source bytes
    /// remain ordered per runtime, `max_queued_bytes` bounds the payload RAM
    /// queued ahead of the worker, and [`Ledger::flush`] joins the worker
    /// before publication. This mode is intentionally one-shot and only useful
    /// while the canonical projection is still private. With deferred sync,
    /// the worker flushes at metadata, source-seal and rotation boundaries
    /// instead of every raw batch.
    pub fn into_async_bounded(self, max_queued_bytes: u64) -> Self {
        assert!(
            self.async_backend.is_none(),
            "ledger is already asynchronous"
        );
        let dir = self.dir.clone();
        let max_segment_bytes = self.max_segment_bytes;
        let segment_seq = self.segment_seq;
        let defer_checkpoint_sync = self.defer_checkpoint_sync;
        let codec = self.codec;
        let queue = Arc::new(QueuedBytes::new(max_queued_bytes));
        let drained = Arc::clone(&queue);
        let (sender, receiver) = channel::<AsyncCommand>();
        let worker = std::thread::spawn(move || {
            let _close = CloseOnExit(Arc::clone(&drained));
            let mut ledger = self;
            let mut recovery_boundary = false;
            while let Ok(command) = receiver.recv() {
                let bytes = command.queued_bytes();
                let applied = ledger.apply_async(command, &mut recovery_boundary);
                drained.release(bytes);
                applied?;
            }
            ledger.flush()?;
            Ok(ledger)
        });
        Self {
            dir,
            max_segment_bytes,
            segment_seq,
            writer: None,
            codec,
            disk_sampled: 0,
            written: 0,
            defined: HashMap::new(),
            segment_generations: HashMap::new(),
            keys: None,
            unkeyed: 0,
            defer_checkpoint_sync,
            unsynced_segments: Vec::new(),
            unsynced_directories: Vec::new(),
            write_stats: LedgerWriteStats::default(),
            async_backend: Some(AsyncBackend {
                sender: Some(sender),
                queue,
                worker: Some(worker),
            }),
        }
    }

    fn apply_async(
        &mut self,
        command: AsyncCommand,
        recovery_boundary: &mut bool,
    ) -> Result<(), CaptureError> {
        match command {
            AsyncCommand::Raw {
                source,
                generation,
                records,
            } => self.append_raw_owned(&source, generation, records),
            AsyncCommand::Prebuilt { source, frames } => {
                let seals = frames.seals();
                self.append_prebuilt(&source, frames)?;
                *recovery_boundary |= seals;
                Ok(())
            }
            AsyncCommand::Complete {
                source,
                generation,
                normalizer_version,
                normalized_clean,
            } => {
                self.complete_generation(
                    &source,
                    generation,
                    normalizer_version,
                    normalized_clean,
                )?;
                *recovery_boundary = true;
                Ok(())
            }
            AsyncCommand::Meta {
                schema_version,
                payload,
            } => {
                self.append_meta(schema_version, &payload)?;
                *recovery_boundary = true;
                Ok(())
            }
            AsyncCommand::SourceKeys(keys) => self.set_source_keys(keys),
            AsyncCommand::Checkpoint => {
                // A private source is recoverable only after its seal.
                // Intermediate batches already drain when the bounded
                // writer fills; flushing zstd here would close blocks
                // without making that source eligible for recovery.
                // Keep metadata/seal boundaries and segment rotation,
                // and never weaken the ordinary durable-checkpoint path.
                if !self.defer_checkpoint_sync
                    || *recovery_boundary
                    || self.written >= self.max_segment_bytes
                {
                    self.checkpoint()?;
                    *recovery_boundary = false;
                }
                Ok(())
            }
        }
    }

    pub(super) fn send_async(&mut self, command: AsyncCommand) -> Result<(), CaptureError> {
        let backend = self
            .async_backend
            .as_mut()
            .filter(|backend| backend.sender.is_some())
            .ok_or_else(|| {
                CaptureError::InvalidConfig("asynchronous ledger is already closed".to_string())
            })?;
        backend.queue.reserve(command.queued_bytes());
        backend
            .sender
            .as_ref()
            .expect("checked above")
            .send(command)
            .map_err(|_| {
                CaptureError::InvalidConfig("asynchronous ledger worker stopped".to_string())
            })
    }

    pub(super) fn finish_async(&mut self) -> Result<(), CaptureError> {
        let mut backend = self
            .async_backend
            .take()
            .ok_or_else(|| CaptureError::InvalidConfig("ledger is not asynchronous".to_string()))?;
        drop(backend.sender.take());
        let worker = backend.worker.take().ok_or_else(|| {
            CaptureError::InvalidConfig("asynchronous ledger worker is missing".to_string())
        })?;
        let finished = worker.join().map_err(|_| {
            CaptureError::InvalidConfig("asynchronous ledger worker panicked".to_string())
        })??;
        self.adopt_sync(finished);
        Ok(())
    }

    fn adopt_sync(&mut self, mut finished: Ledger) {
        self.dir = std::mem::take(&mut finished.dir);
        self.max_segment_bytes = finished.max_segment_bytes;
        self.segment_seq = finished.segment_seq;
        self.writer = finished.writer.take();
        self.written = finished.written;
        self.codec = finished.codec;
        self.disk_sampled = finished.disk_sampled;
        self.defined = std::mem::take(&mut finished.defined);
        self.segment_generations = std::mem::take(&mut finished.segment_generations);
        self.keys = finished.keys.take();
        self.unkeyed = finished.unkeyed;
        self.defer_checkpoint_sync = finished.defer_checkpoint_sync;
        self.unsynced_segments = std::mem::take(&mut finished.unsynced_segments);
        self.unsynced_directories = std::mem::take(&mut finished.unsynced_directories);
        self.write_stats = std::mem::take(&mut finished.write_stats);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::channel;
    use std::time::Duration;

    use super::*;

    fn waiter(queue: &Arc<QueuedBytes>, bytes: u64) -> std::sync::mpsc::Receiver<()> {
        let (admitted, waiting) = channel();
        let queue = Arc::clone(queue);
        std::thread::spawn(move || {
            queue.reserve(bytes);
            admitted.send(()).unwrap();
        });
        waiting
    }

    #[test]
    fn the_queue_admits_by_bytes_and_an_oversized_command_alone() {
        let queue = Arc::new(QueuedBytes::new(100));
        queue.reserve(60);
        queue.reserve(40);
        let blocked = waiter(&queue, 1);
        assert!(
            blocked.recv_timeout(Duration::from_millis(200)).is_err(),
            "a full queue holds the next command back"
        );
        queue.release(60);
        blocked.recv_timeout(Duration::from_secs(30)).unwrap();
        queue.release(41);
        // Larger than the whole budget, admitted because nothing is queued.
        queue.reserve(1_000);
        let blocked = waiter(&queue, 1);
        assert!(blocked.recv_timeout(Duration::from_millis(200)).is_err());
        queue.release(1_000);
        blocked.recv_timeout(Duration::from_secs(30)).unwrap();
    }

    #[test]
    fn a_closed_queue_releases_every_waiter() {
        let queue = Arc::new(QueuedBytes::new(10));
        queue.reserve(10);
        let first = waiter(&queue, 5);
        let second = waiter(&queue, 5);
        assert!(first.recv_timeout(Duration::from_millis(200)).is_err());
        queue.close();
        first.recv_timeout(Duration::from_secs(30)).unwrap();
        second.recv_timeout(Duration::from_secs(30)).unwrap();
    }

    #[test]
    fn payload_free_commands_cost_only_their_bookkeeping() {
        assert_eq!(AsyncCommand::Checkpoint.queued_bytes(), COMMAND_BYTES);
        let source = SourceDef {
            ledger_only: false,
            relative_path: "a.jsonl".into(),
            stream_id: "s".into(),
            session_id: "s".into(),
            native_session_id: "n".into(),
            agent_version: None,
        };
        let raw = AsyncCommand::Raw {
            source: source.clone(),
            generation: 1,
            records: vec![(0, vec![0; 1000]), (1000, vec![0; 24])],
        };
        assert_eq!(raw.queued_bytes(), 1024 + COMMAND_BYTES);
        let mut frames = PrebuiltFrames::new(0, 1);
        frames.push_record(0, &[0; 100]).unwrap();
        let expected = frames.frame_bytes() + COMMAND_BYTES;
        let prebuilt = AsyncCommand::Prebuilt { source, frames };
        assert_eq!(prebuilt.queued_bytes(), expected);
    }
}
