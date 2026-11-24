//! Reattach replayed JSONL cursors to verified native bytes before cold catch-up.
//! The ledger knows record positions and payloads, but not native file identity
//! or line endings. The ordinary tailer supplies those only after verification.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use rosetta_adapter_support::{AgentAdapter, StreamDriver};
use rosetta_capture::{TailCheckpoint, TailItem, TailRead, Tailer, Transition};
use rosetta_core::rows::{RuntimeRow, StreamState};
use rosetta_store::{Batch, CheckpointUpdate, Store, queries};

use crate::{IngestOptions, Instrument, Metric, PipeCheckpoint, PipelineError, SessionFile};

#[derive(Debug, Default)]
pub struct RestoredPrefixes(BTreeMap<String, Prefix>);

#[derive(Debug, Default)]
struct Prefix {
    // Fixed-size per source even when thousands of histories are interleaved.
    hash: [u8; 32],
    payload_end: u64,
    records: u64,
    payload_bytes: u64,
    generation: u64,
}

impl Prefix {
    fn record(&mut self, start: u64, payload: &[u8]) {
        let len = payload.len() as u64;
        let mut next = blake3::Hasher::new();
        next.update(&self.hash);
        next.update(&start.to_le_bytes());
        next.update(&len.to_le_bytes());
        next.update(payload);
        self.hash = *next.finalize().as_bytes();
        self.payload_end = start.saturating_add(len);
        self.records += 1;
        self.payload_bytes += len;
    }
}

impl RestoredPrefixes {
    pub(super) fn record(&mut self, path: &str, generation: u64, offset: u64, payload: &[u8]) {
        let prefix = match self.0.get_mut(path) {
            Some(prefix) => prefix,
            None => self.0.entry(path.to_owned()).or_default(),
        };
        prefix.generation = generation;
        prefix.record(offset, payload);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn bind(
        self,
        adapter: &dyn AgentAdapter,
        runtime: &RuntimeRow,
        root: &Path,
        roster: &[SessionFile],
        store: &mut Store,
        opts: &IngestOptions,
        instrument: &Instrument,
    ) -> Result<crate::StreamReport, PipelineError> {
        self.bind_positions(
            adapter, runtime, root, roster, store, opts, instrument, None,
        )
    }

    /// Catch up all current sources after ledger replay. If any JSONL member
    /// of a multi-source session changed, apply its members once in the
    /// fresh import's path order. This also restores the winning value when
    /// two transcripts contain different versions of the same message.
    #[allow(clippy::too_many_arguments)]
    pub fn catch_up(
        self,
        adapter: &dyn AgentAdapter,
        runtime: &RuntimeRow,
        root: &Path,
        roster: &mut Vec<SessionFile>,
        store: &mut Store,
        opts: &IngestOptions,
        instrument: &Instrument,
        ledger: &mut rosetta_capture::Ledger,
        withheld: &mut BTreeMap<String, StreamState>,
    ) -> Result<crate::StreamReport, PipelineError> {
        *roster = crate::order_runtime_roster(std::mem::take(roster));
        let mut replay_sessions = BTreeSet::new();
        let verified = self.bind_positions(
            adapter,
            runtime,
            root,
            roster,
            store,
            opts,
            instrument,
            Some(&mut replay_sessions),
        )?;
        let mut report = crate::stream::stream_backfill_inner(
            adapter,
            runtime,
            root,
            store,
            roster,
            opts,
            instrument,
            Some(ledger),
            crate::stream::Readiness::Withhold {
                decisions: withheld,
                replay_sessions: &replay_sessions,
            },
        )?;
        report.merge_from(&verified);
        Ok(report)
    }

    #[allow(clippy::too_many_arguments)]
    fn bind_positions(
        self,
        adapter: &dyn AgentAdapter,
        runtime: &RuntimeRow,
        root: &Path,
        roster: &[SessionFile],
        store: &mut Store,
        opts: &IngestOptions,
        instrument: &Instrument,
        mut replay_sessions: Option<&mut BTreeSet<String>>,
    ) -> Result<crate::StreamReport, PipelineError> {
        let mut verified = crate::StreamReport::default();
        let mut credits = Vec::new();
        let tailer = Tailer::new(opts.tail.clone())?;
        for group in roster.chunk_by(|left, right| left.session_id == right.session_id) {
            let mut changed = false;
            let mut sources = 0;
            credits.clear();
            for file in group
                .iter()
                .filter(|file| file.driver == StreamDriver::JsonLines)
            {
                opts.check_cancelled()?;
                sources += 1;
                let Some(prefix) = self.0.get(&file.relative_path) else {
                    changed |= file.size != 0;
                    continue;
                };
                if let Some(offset) = prefix.bind(
                    adapter, runtime, root, file, store, &tailer, opts, instrument,
                )? {
                    changed |= file.cut_len.unwrap_or(file.size) > offset;
                    credits.push((file, prefix));
                } else {
                    changed = true;
                }
            }
            if changed
                && sources > 1
                && let Some(replay) = replay_sessions.as_deref_mut()
            {
                replay.insert(group[0].session_id.clone());
                continue;
            }
            // Credit a settled session immediately. A family scheduled for a
            // full reread will report its own bytes and records during capture.
            for &(file, prefix) in &credits {
                verified.payload_bytes += prefix.payload_bytes;
                verified.records += prefix.records;
                instrument.add(Metric::PayloadBytes, prefix.payload_bytes);
                instrument.add(Metric::RecordsFramed, prefix.records);
                if let Some(progress) = &opts.progress {
                    progress.source_bytes(file, prefix.payload_bytes);
                }
            }
        }
        Ok(verified)
    }
}

impl Prefix {
    #[allow(clippy::too_many_arguments)]
    fn bind(
        &self,
        adapter: &dyn AgentAdapter,
        runtime: &RuntimeRow,
        root: &Path,
        file: &SessionFile,
        store: &mut Store,
        tailer: &Tailer,
        opts: &IngestOptions,
        instrument: &Instrument,
    ) -> Result<Option<u64>, PipelineError> {
        let Some(cursor) = queries::cursor_state(store.conn(), &runtime.id, &file.relative_path)?
        else {
            return Ok(None);
        };
        let Some(mut checkpoint) = PipeCheckpoint::decode(&cursor.checkpoint) else {
            return Ok(None);
        };
        if cursor.active_generation != self.generation
            || checkpoint.source_generation != self.generation
            || checkpoint.normalizer_version != adapter.normalizer_version()
            || checkpoint.session_id != file.session_id
            || checkpoint.stream_id != file.stream_id
            || checkpoint.tail.is_some()
        {
            return Ok(None);
        }
        let offset = match self.verify(root, file, tailer, opts, instrument)? {
            Some((tail, stamp)) => {
                checkpoint.tail = Some(tail.encode_hex());
                checkpoint.snapshot_stamp = stamp;
                Some(tail.committed_offset)
            }
            None => {
                // Keep the old rows visible until the replacement activates.
                checkpoint = PipeCheckpoint::fresh(
                    &file.session_id,
                    &file.stream_id,
                    adapter.normalizer_version(),
                );
                None
            }
        };
        instrument.apply(
            store,
            &Batch {
                ops: vec![],
                source: None,
                checkpoints: vec![CheckpointUpdate {
                    runtime_id: runtime.id.clone(),
                    stream: file.stream_id.clone(),
                    path: file.relative_path.clone(),
                    session_id: file.projected_session_id(),
                    expected: Some(cursor.checkpoint),
                    next: checkpoint.encode(),
                    updated_at: None,
                }],
            },
        )?;
        Ok(offset)
    }

    fn verify(
        &self,
        root: &Path,
        file: &SessionFile,
        tailer: &Tailer,
        opts: &IngestOptions,
        instrument: &Instrument,
    ) -> Result<Option<(TailCheckpoint, Option<String>)>, PipelineError> {
        let mut input = match &file.input {
            Some(source) => match source.open(root, || opts.check_cancelled().is_err()) {
                Ok(Some(mut input)) => {
                    input.cancel = opts.cancel.clone();
                    Some(input)
                }
                Ok(None) => return Ok(None),
                Err(error) if error.incomplete_input() => return Ok(None),
                Err(error) => {
                    opts.check_cancelled()?;
                    return Err(error.into());
                }
            },
            None => None,
        };
        let path = root.join(&file.relative_path);
        let lf = self.payload_end.saturating_add(1);
        let mut end = lf;
        let mut previous = None;
        let mut actual = Prefix::default();
        loop {
            opts.check_cancelled()?;
            let read = match &mut input {
                Some(input) => tailer.read_input(input, previous.as_ref(), false, Some(end)),
                None => tailer.read_cut(&path, previous.as_ref(), false, Some(end)),
            }?;
            let TailRead::Batch(batch) = read else {
                return Ok(None);
            };
            instrument.add(Metric::BytesRead, batch.bytes_read);
            if previous.is_some() && batch.transition != Transition::Continued {
                return Ok(None);
            }
            for item in batch.items {
                let TailItem::Record(record) = item else {
                    return Ok(None);
                };
                actual.record(record.start_offset, &record.payload);
            }
            if batch.checkpoint.committed_offset == end {
                return Ok((actual.hash == self.hash)
                    .then(|| (batch.checkpoint, input.map(|input| input.stamp))));
            }
            if !batch.more_available {
                // CRLF normalization removes one extra physical byte. Permit
                // that delimiter without consuming any following record.
                if end != lf || batch.snapshot_len != end {
                    return Ok(None);
                }
                end = lf.saturating_add(1);
            }
            previous = Some(batch.checkpoint);
        }
    }
}
