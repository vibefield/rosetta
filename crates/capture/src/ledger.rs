//! The append-only raw ledger — the FIRST of the two data representations
//! (iron rule 4). The projection is disposable because this is not: raw
//! records land here before the SQLite transaction that projects them
//! (docs/ingest-engine.html, crash protocol). A live/public projection uses
//! a durable checkpoint before every store transaction. A brand-new private
//! projection may defer those syncs only while it is unpublished, then must
//! cross one durable [`Ledger::flush`] barrier before publication. Recovery
//! may find an orphan ledger tail (harmless — no published projection
//! referenced it); it must never find a published projection row whose raw
//! bytes were not made durable first.
//!
//! Layout: one directory per runtime, numbered segment files, rotated by
//! size. Segments are self-contained: every source a segment references is
//! (re)defined inside it, so a scan never needs another segment's state and
//! deleting old segments (future GC) cannot dangle references.
//!
//! Frames: `tag ∙ u32 body-len ∙ body ∙ 8-byte blake3 checksum`. A torn or
//! corrupt frame ends that segment's scan — everything before it is intact.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{SyncSender, sync_channel};
use std::thread::JoinHandle;
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::segment::{MAGIC_LEN, SegmentCodec, SegmentReader, SegmentWriter};
use crate::segment_index::{self, IndexedSource, SegmentIndexLine};
use crate::{CaptureError, io_error};

const CHECKSUM_BYTES: usize = 8;

const TAG_DEFINE: u8 = 1;
const TAG_RAW: u8 = 2;
/// Legacy unversioned canonical-op meta payload.
const TAG_META: u8 = 3;
const TAG_GENERATION_COMPLETE: u8 = 4;
const TAG_META_VERSIONED: u8 = 5;
/// Multiple records from one source/generation under one checksum. Readers
/// retain `TAG_RAW` support forever; new writers amortize framing and syscalls
/// across the capture batch they already received.
const TAG_RAW_BATCH: u8 = 6;
const MAX_FRAME_BYTES: usize = 256 * 1024 * 1024;
// One frozen mixed-corpus cold batch averaged about 1.10 MiB of ledger frames.
// A 1 MiB buffer amortizes writes within that checkpoint boundary while
// preserving the caller's explicit flush/durability cadence.
const LEDGER_WRITE_BUFFER_BYTES: usize = 1024 * 1024;
const META_HEAD: &str = ".meta-head";

/// Everything replay needs to rebuild a normalization context for a source
/// file. Written into each segment the first time the source appears there.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SourceDef {
    /// This source is lossless ledger evidence only, not a canonical session.
    /// Old ledgers predate session admission policy and default to projection.
    #[serde(default)]
    pub ledger_only: bool,
    pub relative_path: String,
    pub stream_id: String,
    pub session_id: String,
    pub native_session_id: String,
    pub agent_version: Option<String>,
}

impl SourceDef {
    pub fn projected_session_id(&self) -> Option<String> {
        (!self.ledger_only).then(|| self.session_id.clone())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LedgerEntry {
    /// One raw source record, exactly as framed from the agent's file.
    Raw {
        source: SourceDef,
        generation: u64,
        start_offset: u64,
        payload: Vec<u8>,
    },
    /// The raw capture for this generation reached a complete source boundary.
    /// Normalization may still reject it; replay decides cleanliness using its
    /// current adapter and activates only if every record is acceptable.
    GenerationComplete {
        source: SourceDef,
        generation: u64,
        normalizer_version: u32,
        normalized_clean: bool,
    },
    /// A versioned serialized batch of meta-phase ops. Version routing keeps
    /// the durable ledger decodable when the canonical op JSON evolves.
    Meta {
        schema_version: u32,
        payload: String,
    },
}

/// Non-overlapping append-side measurements. The pipeline drains these after
/// each durable ledger batch so profiling never changes the write ordering.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LedgerWriteStats {
    /// Bytes that reached segment files, container included. Below
    /// `frame_bytes` whenever the segment codec compresses.
    pub disk_bytes: u64,
    pub frames: u64,
    pub frame_bytes: u64,
    pub flushes: u64,
    pub syncs: u64,
    /// Directory durability barriers, kept separate from segment-file syncs.
    pub directory_syncs: u64,
    pub segments_opened: u64,
    /// Body construction/copy plus frame-header and checksum work.
    pub frame_nanos: u64,
    /// Bytes handed to the buffered writer, including any writes it triggers.
    pub buffer_write_nanos: u64,
    pub flush_nanos: u64,
    pub sync_nanos: u64,
    pub directory_sync_nanos: u64,
    /// Segment create plus the magic-header write.
    pub segment_open_nanos: u64,
}

fn elapsed_nanos(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

/// Append side. One ledger per runtime; the pipeline owns the write order.
pub struct Ledger {
    dir: PathBuf,
    max_segment_bytes: u64,
    segment_seq: u64,
    writer: Option<SegmentWriter>,
    codec: SegmentCodec,
    /// Container bytes of the open segment already folded into `write_stats`.
    disk_sampled: u64,
    written: u64,
    /// Sources already defined in the CURRENT segment → their local id.
    defined: HashMap<SourceDef, u32>,
    /// Generation range seen per local source id in the CURRENT segment,
    /// for the segment index line written when it closes.
    segment_generations: HashMap<u32, (u64, u64)>,
    /// Private cold builds may flush each projection batch to the kernel and
    /// defer physical sync until their pre-publication barrier. Live writers
    /// leave this false and retain the per-batch durability contract.
    defer_checkpoint_sync: bool,
    /// Rotated segments that were buffer-flushed but not yet synced. Paths
    /// keep the writer lightweight; the final barrier reopens each file.
    unsynced_segments: Vec<PathBuf>,
    /// Directories whose entries changed since the last durable barrier.
    /// Ordered child-first so newly created directory trees become durable
    /// from their leaves through the first pre-existing ancestor.
    unsynced_directories: Vec<PathBuf>,
    write_stats: LedgerWriteStats,
    async_backend: Option<AsyncBackend>,
}

enum AsyncCommand {
    Raw {
        source: SourceDef,
        generation: u64,
        records: Vec<(u64, Vec<u8>)>,
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
    Checkpoint,
}

struct AsyncBackend {
    sender: Option<SyncSender<AsyncCommand>>,
    worker: Option<JoinHandle<Result<Ledger, CaptureError>>>,
}

impl Ledger {
    /// Open (or create) the runtime's ledger directory and continue after
    /// the highest existing segment — existing segments are never reopened
    /// for writing, so a previous crash's torn tail stays untouched.
    pub fn open(dir: &Path) -> Result<Self, CaptureError> {
        let unsynced_directories = directory_creation_barrier(dir);
        std::fs::create_dir_all(dir).map_err(|error| io_error("creating", dir, error))?;
        let next = segment_files(dir)?
            .last()
            .map(|(seq, _)| seq + 1)
            .unwrap_or(0);
        Ok(Self {
            dir: dir.to_path_buf(),
            max_segment_bytes: 128 * 1024 * 1024,
            segment_seq: next,
            writer: None,
            codec: SegmentCodec::default(),
            disk_sampled: 0,
            written: 0,
            defined: HashMap::new(),
            segment_generations: HashMap::new(),
            defer_checkpoint_sync: false,
            unsynced_segments: Vec::new(),
            unsynced_directories,
            write_stats: LedgerWriteStats::default(),
            async_backend: None,
        })
    }

    /// Move cold ledger IO/checksumming onto a bounded worker. Source bytes
    /// remain ordered per runtime, queue capacity bounds owned payload RAM,
    /// and [`Ledger::flush`] joins the worker before publication. This mode is
    /// intentionally one-shot and only useful while the canonical projection
    /// is still private.
    pub fn into_async(self, queue_capacity: usize) -> Self {
        assert!(
            self.async_backend.is_none(),
            "ledger is already asynchronous"
        );
        let dir = self.dir.clone();
        let max_segment_bytes = self.max_segment_bytes;
        let segment_seq = self.segment_seq;
        let defer_checkpoint_sync = self.defer_checkpoint_sync;
        let codec = self.codec;
        let (sender, receiver) = sync_channel(queue_capacity.max(1));
        let worker = std::thread::spawn(move || {
            let mut ledger = self;
            while let Ok(command) = receiver.recv() {
                match command {
                    AsyncCommand::Raw {
                        source,
                        generation,
                        records,
                    } => ledger.append_raw_owned(&source, generation, records)?,
                    AsyncCommand::Complete {
                        source,
                        generation,
                        normalizer_version,
                        normalized_clean,
                    } => ledger.complete_generation(
                        &source,
                        generation,
                        normalizer_version,
                        normalized_clean,
                    )?,
                    AsyncCommand::Meta {
                        schema_version,
                        payload,
                    } => ledger.append_meta(schema_version, &payload)?,
                    AsyncCommand::Checkpoint => ledger.checkpoint()?,
                }
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
            defer_checkpoint_sync,
            unsynced_segments: Vec::new(),
            unsynced_directories: Vec::new(),
            write_stats: LedgerWriteStats::default(),
            async_backend: Some(AsyncBackend {
                sender: Some(sender),
                worker: Some(worker),
            }),
        }
    }

    pub fn with_max_segment_bytes(mut self, max: u64) -> Self {
        self.max_segment_bytes = max.max(1);
        self
    }

    /// Choose the container for segments this ledger creates from now on.
    /// Existing segments keep theirs; readers accept every container. Like
    /// the other builders, this must precede [`Ledger::into_async`], which
    /// hands the worker its settings once.
    pub fn with_codec(mut self, codec: SegmentCodec) -> Self {
        self.codec = codec;
        self
    }

    /// Defer physical sync at projection checkpoints. This is safe only for
    /// a new private projection that cannot be observed or published until a
    /// final [`Ledger::flush`] succeeds. The ordinary/live default remains
    /// durable per checkpoint.
    pub fn defer_sync_until_flush(mut self) -> Self {
        self.defer_checkpoint_sync = true;
        self
    }

    /// Drain measurements accumulated since the previous call. This does not
    /// flush or otherwise affect ledger durability.
    pub fn take_write_stats(&mut self) -> LedgerWriteStats {
        std::mem::take(&mut self.write_stats)
    }

    /// Append one batch of raw records for one source. Buffered — nothing is
    /// durable until [`Ledger::flush`] returns.
    pub fn append_raw(
        &mut self,
        source: &SourceDef,
        generation: u64,
        records: impl IntoIterator<Item = (u64, impl AsRef<[u8]>)>,
    ) -> Result<(), CaptureError> {
        if self.async_backend.is_some() {
            let records = records
                .into_iter()
                .map(|(offset, payload)| (offset, payload.as_ref().to_vec()))
                .collect();
            return self.send_async(AsyncCommand::Raw {
                source: source.clone(),
                generation,
                records,
            });
        }
        let file_id = self.define(source)?;
        for (start_offset, payload) in records {
            self.raw_frame(file_id, generation, start_offset, payload.as_ref())?;
        }
        Ok(())
    }

    /// Owned variant for cold pipelines that can move framed payloads out of
    /// their tail batch. The bytes go straight through checksum/write and are
    /// never copied into an intermediate frame body.
    pub fn append_raw_owned(
        &mut self,
        source: &SourceDef,
        generation: u64,
        records: Vec<(u64, Vec<u8>)>,
    ) -> Result<(), CaptureError> {
        if self.async_backend.is_some() {
            return self.send_async(AsyncCommand::Raw {
                source: source.clone(),
                generation,
                records,
            });
        }
        let file_id = self.define(source)?;
        for (start_offset, payload) in records {
            self.raw_frame(file_id, generation, start_offset, &payload)?;
        }
        Ok(())
    }

    /// Seal a fully captured source generation. The marker may be flushed
    /// before the projection transaction: it certifies raw completeness, not
    /// that a particular projection accepted the records.
    pub fn complete_generation(
        &mut self,
        source: &SourceDef,
        generation: u64,
        normalizer_version: u32,
        normalized_clean: bool,
    ) -> Result<(), CaptureError> {
        if self.async_backend.is_some() {
            return self.send_async(AsyncCommand::Complete {
                source: source.clone(),
                generation,
                normalizer_version,
                normalized_clean,
            });
        }
        let file_id = self.define(source)?;
        self.note_generation(file_id, generation);
        let started = Instant::now();
        let mut body = Vec::with_capacity(17);
        body.extend_from_slice(&file_id.to_be_bytes());
        body.extend_from_slice(&generation.to_be_bytes());
        body.extend_from_slice(&normalizer_version.to_be_bytes());
        body.push(u8::from(normalized_clean));
        self.write_stats.frame_nanos = self
            .write_stats
            .frame_nanos
            .saturating_add(elapsed_nanos(started));
        self.frame(TAG_GENERATION_COMPLETE, &body)
    }

    /// Append a version-routed serialized meta-ops batch.
    pub fn append_meta(&mut self, schema_version: u32, ops_json: &str) -> Result<(), CaptureError> {
        if self.async_backend.is_some() {
            return self.send_async(AsyncCommand::Meta {
                schema_version,
                payload: ops_json.to_string(),
            });
        }
        let started = Instant::now();
        let mut body = Vec::with_capacity(4 + ops_json.len());
        body.extend_from_slice(&schema_version.to_be_bytes());
        body.extend_from_slice(ops_json.as_bytes());
        self.write_stats.frame_nanos = self
            .write_stats
            .frame_nanos
            .saturating_add(elapsed_nanos(started));
        self.frame(TAG_META_VERSIONED, &body)
    }

    /// Journal a metadata snapshot only when its caller-supplied semantic key
    /// changed. The sidecar is merely an optimization: the ledger checkpoint
    /// becomes durable first, then the key is atomically installed. A crash in
    /// between can cause one duplicate meta frame on restart, never missing
    /// recovery evidence. Private/deferred and async ledgers conservatively
    /// append because their one final durability barrier has not happened yet.
    pub fn append_meta_checkpoint_if_changed(
        &mut self,
        schema_version: u32,
        ops_json: &str,
        semantic_key: &str,
    ) -> Result<bool, CaptureError> {
        if self.async_backend.is_some() || self.defer_checkpoint_sync {
            self.append_meta(schema_version, ops_json)?;
            self.checkpoint()?;
            return Ok(true);
        }

        let head = format!("{schema_version}:{semantic_key}\n");
        let path = self.dir.join(META_HEAD);
        if std::fs::read(&path).is_ok_and(|current| current == head.as_bytes()) {
            return Ok(false);
        }

        self.append_meta(schema_version, ops_json)?;
        self.checkpoint()?;
        let temporary = self
            .dir
            .join(format!("{META_HEAD}.tmp-{}", std::process::id()));
        match std::fs::remove_file(&temporary) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(io_error("removing stale meta head", &temporary, error)),
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|error| io_error("creating meta head", &temporary, error))?;
        file.write_all(head.as_bytes())
            .and_then(|_| file.sync_all())
            .map_err(|error| io_error("syncing meta head", &temporary, error))?;
        drop(file);
        std::fs::rename(&temporary, &path)
            .map_err(|error| io_error("installing meta head", &path, error))?;
        sync_directory(&self.dir)
            .map_err(|error| io_error("syncing meta head directory", &self.dir, error))?;
        Ok(true)
    }

    /// Cross one projection checkpoint. Live/default ledgers make it durable
    /// immediately. A deliberately deferred private ledger only drains its
    /// userspace buffer and rotates at the same boundaries; its caller must
    /// invoke [`Ledger::flush`] before publication.
    pub fn checkpoint(&mut self) -> Result<(), CaptureError> {
        if self.async_backend.is_some() {
            return self.send_async(AsyncCommand::Checkpoint);
        }
        if !self.defer_checkpoint_sync {
            return self.flush();
        }
        self.flush_buffer()?;
        self.rotate_if_needed(false)
    }

    /// Durably flush every segment appended so far. This is always a physical
    /// sync barrier, including for private ledgers with deferred checkpoints.
    pub fn flush(&mut self) -> Result<(), CaptureError> {
        if self.async_backend.is_some() {
            return self.finish_async();
        }
        self.flush_buffer()?;

        // A segment due for rotation closes before the barrier so its
        // container epilogue is inside the sync, not after it.
        let current = self
            .writer
            .as_ref()
            .map(|_| self.dir.join(segment_name(self.segment_seq)));
        if self.written >= self.max_segment_bytes {
            self.close_segment()?;
        }
        for path in self.unsynced_segments.iter().chain(current.iter()) {
            let file =
                File::open(path).map_err(|error| io_error("opening for sync", path, error))?;
            self.write_stats.syncs = self.write_stats.syncs.saturating_add(1);
            let started = Instant::now();
            let synced = file.sync_data();
            self.write_stats.sync_nanos = self
                .write_stats
                .sync_nanos
                .saturating_add(elapsed_nanos(started));
            synced.map_err(|error| io_error("syncing", path, error))?;
        }
        self.unsynced_segments.clear();

        for path in &self.unsynced_directories {
            self.write_stats.directory_syncs = self.write_stats.directory_syncs.saturating_add(1);
            let started = Instant::now();
            let synced = sync_directory(path);
            self.write_stats.directory_sync_nanos = self
                .write_stats
                .directory_sync_nanos
                .saturating_add(elapsed_nanos(started));
            synced.map_err(|error| io_error("syncing directory", path, error))?;
        }
        self.unsynced_directories.clear();
        self.rotate_if_needed(true)
    }

    fn send_async(&mut self, command: AsyncCommand) -> Result<(), CaptureError> {
        self.async_backend
            .as_mut()
            .and_then(|backend| backend.sender.as_mut())
            .ok_or_else(|| {
                CaptureError::InvalidConfig("asynchronous ledger is already closed".to_string())
            })?
            .send(command)
            .map_err(|_| {
                CaptureError::InvalidConfig("asynchronous ledger worker stopped".to_string())
            })
    }

    fn finish_async(&mut self) -> Result<(), CaptureError> {
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
        self.defer_checkpoint_sync = finished.defer_checkpoint_sync;
        self.unsynced_segments = std::mem::take(&mut finished.unsynced_segments);
        self.unsynced_directories = std::mem::take(&mut finished.unsynced_directories);
        self.write_stats = std::mem::take(&mut finished.write_stats);
    }

    fn flush_buffer(&mut self) -> Result<(), CaptureError> {
        let Some(writer) = &mut self.writer else {
            return Ok(());
        };
        let path = self.dir.join(segment_name(self.segment_seq));
        self.write_stats.flushes = self.write_stats.flushes.saturating_add(1);
        let started = Instant::now();
        let flushed = writer.flush();
        self.write_stats.flush_nanos = self
            .write_stats
            .flush_nanos
            .saturating_add(elapsed_nanos(started));
        flushed.map_err(|error| io_error("flushing", &path, error))?;
        self.sample_disk_bytes();
        Ok(())
    }

    /// Fold container bytes written since the last sample into the stats.
    fn sample_disk_bytes(&mut self) {
        let Some(writer) = &self.writer else {
            return;
        };
        let now = writer.disk_bytes();
        self.write_stats.disk_bytes = self
            .write_stats
            .disk_bytes
            .saturating_add(now.saturating_sub(self.disk_sampled));
        self.disk_sampled = now;
    }

    /// Close the open segment's container (a zstd epilogue, a plain drain).
    /// Idempotent; the next append opens the next segment.
    fn close_segment(&mut self) -> Result<(), CaptureError> {
        let Some(writer) = self.writer.take() else {
            return Ok(());
        };
        let path = self.dir.join(segment_name(self.segment_seq));
        let started = Instant::now();
        let finished = writer.finish();
        self.write_stats.flush_nanos = self
            .write_stats
            .flush_nanos
            .saturating_add(elapsed_nanos(started));
        let total = finished.map_err(|error| io_error("closing segment", &path, error))?;
        self.write_stats.disk_bytes = self
            .write_stats
            .disk_bytes
            .saturating_add(total.saturating_sub(self.disk_sampled));
        self.disk_sampled = 0;
        self.index_closed_segment();
        Ok(())
    }

    fn note_generation(&mut self, file_id: u32, generation: u64) {
        let range = self
            .segment_generations
            .entry(file_id)
            .or_insert((generation, generation));
        range.0 = range.0.min(generation);
        range.1 = range.1.max(generation);
    }

    /// Record which sources and generations the just-closed segment holds so
    /// scoped reads can skip it. Best effort by design: the index is a
    /// derived cache, and a missing line only widens a read to this segment.
    fn index_closed_segment(&mut self) {
        let mut sources: Vec<IndexedSource> = self
            .defined
            .iter()
            .map(|(source, id)| {
                let (lo, hi) = self
                    .segment_generations
                    .get(id)
                    .copied()
                    .unwrap_or((0, u64::MAX));
                IndexedSource {
                    session_id: source.session_id.clone(),
                    relative_path: source.relative_path.clone(),
                    generations: [lo, hi],
                }
            })
            .collect();
        sources.sort_by(|a, b| {
            (&a.session_id, &a.relative_path, a.generations).cmp(&(
                &b.session_id,
                &b.relative_path,
                b.generations,
            ))
        });
        let line = SegmentIndexLine {
            segment: self.segment_seq,
            sources,
        };
        let _ = segment_index::append_line(&self.dir, &line);
    }

    fn rotate_if_needed(&mut self, current_is_synced: bool) -> Result<(), CaptureError> {
        if self.written < self.max_segment_bytes {
            return Ok(());
        }
        let path = self.dir.join(segment_name(self.segment_seq));
        self.close_segment()?;
        if !current_is_synced {
            self.unsynced_segments.push(path);
        }
        self.defined.clear();
        self.segment_generations.clear();
        self.segment_seq += 1;
        self.written = 0;
        Ok(())
    }

    fn define(&mut self, source: &SourceDef) -> Result<u32, CaptureError> {
        // Ensure the segment exists before deciding the id: rotation clears
        // the map, and ids are segment-local.
        self.ensure_writer()?;
        if let Some(id) = self.defined.get(source) {
            return Ok(*id);
        }
        let id = self.defined.len() as u32;
        let started = Instant::now();
        let body = serde_encode(&[
            (
                "ledger_only",
                if source.ledger_only { "true" } else { "false" },
            ),
            ("file_id", &id.to_string()),
            ("relative_path", &source.relative_path),
            ("stream_id", &source.stream_id),
            ("session_id", &source.session_id),
            ("native_session_id", &source.native_session_id),
            (
                "agent_version",
                source.agent_version.as_deref().unwrap_or(""),
            ),
        ]);
        self.write_stats.frame_nanos = self
            .write_stats
            .frame_nanos
            .saturating_add(elapsed_nanos(started));
        self.frame(TAG_DEFINE, body.as_bytes())?;
        self.defined.insert(source.clone(), id);
        Ok(id)
    }

    fn frame(&mut self, tag: u8, body: &[u8]) -> Result<(), CaptureError> {
        if body.len() > MAX_FRAME_BYTES {
            return Err(CaptureError::LimitExceeded(format!(
                "raw ledger frame is {} bytes, limit is {MAX_FRAME_BYTES}",
                body.len()
            )));
        }
        self.ensure_writer()?;
        let path = self.dir.join(segment_name(self.segment_seq));
        let started = Instant::now();
        let mut head = [0_u8; 5];
        head[0] = tag;
        head[1..5].copy_from_slice(&(body.len() as u32).to_be_bytes());
        let mut hasher = blake3::Hasher::new();
        hasher.update(&head);
        hasher.update(body);
        let digest = hasher.finalize();
        let checksum = &digest.as_bytes()[..CHECKSUM_BYTES];
        self.write_stats.frame_nanos = self
            .write_stats
            .frame_nanos
            .saturating_add(elapsed_nanos(started));

        let started = Instant::now();
        let written = {
            let writer = self.writer.as_mut().expect("ensure_writer just ran");
            writer
                .write_all(&head)
                .and_then(|_| writer.write_all(body))
                .and_then(|_| writer.write_all(checksum))
        };
        self.write_stats.buffer_write_nanos = self
            .write_stats
            .buffer_write_nanos
            .saturating_add(elapsed_nanos(started));
        written.map_err(|error| io_error("appending to", &path, error))?;

        let frame_bytes = (head.len() + body.len() + CHECKSUM_BYTES) as u64;
        self.write_stats.frames = self.write_stats.frames.saturating_add(1);
        self.write_stats.frame_bytes = self.write_stats.frame_bytes.saturating_add(frame_bytes);
        self.written += frame_bytes;
        Ok(())
    }

    fn raw_frame(
        &mut self,
        file_id: u32,
        generation: u64,
        start_offset: u64,
        payload: &[u8],
    ) -> Result<(), CaptureError> {
        const FIXED_BYTES: usize = 20;
        let body_len = FIXED_BYTES.checked_add(payload.len()).ok_or_else(|| {
            CaptureError::LimitExceeded("raw ledger record size overflowed usize".to_string())
        })?;
        if body_len > MAX_FRAME_BYTES {
            return Err(CaptureError::LimitExceeded(format!(
                "raw ledger record is {} bytes, frame limit is {}",
                payload.len(),
                MAX_FRAME_BYTES - FIXED_BYTES
            )));
        }
        self.note_generation(file_id, generation);
        self.ensure_writer()?;
        let path = self.dir.join(segment_name(self.segment_seq));
        let started = Instant::now();
        let mut head = [0_u8; 5];
        head[0] = TAG_RAW;
        head[1..5].copy_from_slice(&(body_len as u32).to_be_bytes());
        let mut fixed = [0_u8; FIXED_BYTES];
        fixed[0..4].copy_from_slice(&file_id.to_be_bytes());
        fixed[4..12].copy_from_slice(&generation.to_be_bytes());
        fixed[12..20].copy_from_slice(&start_offset.to_be_bytes());
        let mut hasher = blake3::Hasher::new();
        hasher.update(&head);
        hasher.update(&fixed);
        hasher.update(payload);
        let digest = hasher.finalize();
        let checksum = &digest.as_bytes()[..CHECKSUM_BYTES];
        self.write_stats.frame_nanos = self
            .write_stats
            .frame_nanos
            .saturating_add(elapsed_nanos(started));

        let started = Instant::now();
        let written = {
            let writer = self.writer.as_mut().expect("ensure_writer just ran");
            writer
                .write_all(&head)
                .and_then(|_| writer.write_all(&fixed))
                .and_then(|_| writer.write_all(payload))
                .and_then(|_| writer.write_all(checksum))
        };
        self.write_stats.buffer_write_nanos = self
            .write_stats
            .buffer_write_nanos
            .saturating_add(elapsed_nanos(started));
        written.map_err(|error| io_error("appending to", &path, error))?;

        let frame_bytes = (head.len() + body_len + CHECKSUM_BYTES) as u64;
        self.write_stats.frames = self.write_stats.frames.saturating_add(1);
        self.write_stats.frame_bytes = self.write_stats.frame_bytes.saturating_add(frame_bytes);
        self.written += frame_bytes;
        Ok(())
    }

    fn ensure_writer(&mut self) -> Result<(), CaptureError> {
        if self.writer.is_some() {
            return Ok(());
        }
        let path = self.dir.join(segment_name(self.segment_seq));
        let started = Instant::now();
        let opened = SegmentWriter::create(&path, self.codec, LEDGER_WRITE_BUFFER_BYTES)
            .map_err(|error| io_error("creating segment", &path, error));
        self.write_stats.segment_open_nanos = self
            .write_stats
            .segment_open_nanos
            .saturating_add(elapsed_nanos(started));
        let writer = opened?;
        self.queue_directory_sync(self.dir.clone());
        self.write_stats.segments_opened = self.write_stats.segments_opened.saturating_add(1);
        self.write_stats.disk_bytes = self.write_stats.disk_bytes.saturating_add(MAGIC_LEN as u64);
        self.disk_sampled = MAGIC_LEN as u64;
        self.written = MAGIC_LEN as u64;
        self.writer = Some(writer);
        Ok(())
    }

    fn queue_directory_sync(&mut self, path: PathBuf) {
        if !self.unsynced_directories.contains(&path) {
            self.unsynced_directories.push(path);
        }
    }
}

impl Drop for Ledger {
    fn drop(&mut self) {
        // A failed private build may unwind before its explicit durability
        // barrier. Close and join the worker so it cannot keep writing into a
        // staging directory after its owner starts cleanup.
        if self.async_backend.is_some() {
            let _ = self.finish_async();
        }
        // A synchronous ledger dropped mid-segment drains as a buffered file
        // always did; a compressed container additionally needs its epilogue.
        if self.writer.is_some() {
            let _ = self.close_segment();
        }
    }
}

/// Return the directory barriers needed if `dir` is about to be created.
/// The first pre-existing ancestor is included because its child entry also
/// changes. Existing ledger directories need no barrier until a segment is
/// created, at which point `ensure_writer` queues the ledger directory.
fn directory_creation_barrier(dir: &Path) -> Vec<PathBuf> {
    let mut barriers = Vec::new();
    let mut cursor = dir;
    while !cursor.exists() {
        barriers.push(cursor.to_path_buf());
        let Some(parent) = cursor.parent() else {
            return barriers;
        };
        cursor = if parent.as_os_str().is_empty() {
            Path::new(".")
        } else {
            parent
        };
    }
    if !barriers.is_empty() {
        barriers.push(cursor.to_path_buf());
    }
    barriers
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> std::io::Result<()> {
    File::open(path)?.sync_all()
}

// Windows persists directory-entry metadata through the file handle barrier;
// opening a directory with std::fs::File is not portable there.
#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

/// Scan every entry across all segments in order. Torn or corrupt tails end
/// their segment silently (`report.torn_segments` says how many); corruption
/// BEFORE the end is still just a stop — the ledger is append-only, so
/// everything prior remains trustworthy.
pub struct LedgerScan {
    pub entries: Vec<LedgerEntry>,
    pub segments: usize,
    pub torn_segments: usize,
}

pub fn scan(dir: &Path) -> Result<LedgerScan, CaptureError> {
    let mut entries = Vec::new();
    let report = visit::<CaptureError, _>(dir, |entry| {
        entries.push(entry);
        Ok(())
    })?;
    Ok(LedgerScan {
        entries,
        segments: report.segments,
        torn_segments: report.torn_segments,
    })
}

/// Aggregate-free scan report. [`visit`] retains at most one frame body and
/// one buffered filesystem page, so rebuild memory is independent of ledger
/// lifetime and segment size.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LedgerVisit {
    pub segments: usize,
    pub torn_segments: usize,
    pub entries: u64,
}

/// Visit every ledger entry in segment order without retaining payloads.
/// Torn/corrupt tails stop only their segment; callback errors are propagated.
pub fn visit<E, F>(dir: &Path, visitor: F) -> Result<LedgerVisit, E>
where
    E: From<CaptureError>,
    F: FnMut(LedgerEntry) -> Result<(), E>,
{
    visit_segments(dir, |_| true, visitor)
}

/// [`visit`] restricted to the segments `keep` admits by sequence number,
/// still in ascending order. `report.segments` counts only the visited ones.
pub fn visit_segments<E, F>(
    dir: &Path,
    keep: impl Fn(u64) -> bool,
    mut visitor: F,
) -> Result<LedgerVisit, E>
where
    E: From<CaptureError>,
    F: FnMut(LedgerEntry) -> Result<(), E>,
{
    let mut report = LedgerVisit::default();
    for (seq, path) in segment_files(dir).map_err(E::from)? {
        if !keep(seq) {
            continue;
        }
        report.segments += 1;
        let mut counted = |entry| {
            report.entries += 1;
            visitor(entry)
        };
        if !scan_segment(&path, &mut counted)? {
            report.torn_segments += 1;
        }
    }
    Ok(report)
}

/// Returns false if the segment ended in a torn/corrupt frame.
pub(crate) fn scan_segment<E, F>(path: &Path, visitor: &mut F) -> Result<bool, E>
where
    E: From<CaptureError>,
    F: FnMut(LedgerEntry) -> Result<(), E>,
{
    let Some(mut reader) = SegmentReader::open(path)
        .map_err(|error| E::from(io_error("opening segment", path, error)))?
    else {
        return Ok(false);
    };
    let clean = scan_frames(&mut reader, path, visitor)?;
    Ok(clean && !reader.corrupt())
}

/// What a frame walk finds before the first torn or corrupt frame: how many
/// container-decoded bytes were intact, whether the walk ended cleanly on a
/// frame boundary, and a digest of those bytes — segment recompression proves
/// its copy against it.
pub(crate) struct FramePrefix {
    pub bytes: u64,
    pub clean: bool,
    pub digest: blake3::Hash,
}

pub(crate) fn frame_prefix(reader: &mut impl Read) -> std::io::Result<FramePrefix> {
    let mut digest = blake3::Hasher::new();
    let mut bytes = 0_u64;
    let torn = |bytes, digest: blake3::Hasher| FramePrefix {
        bytes,
        clean: false,
        digest: digest.finalize(),
    };
    loop {
        let mut head = [0_u8; 5];
        if reader.read(&mut head[..1])? == 0 {
            return Ok(FramePrefix {
                bytes,
                clean: true,
                digest: digest.finalize(),
            });
        }
        if !read_exact_or_short(reader, &mut head[1..])? {
            return Ok(torn(bytes, digest));
        }
        let body_len = u32::from_be_bytes(head[1..5].try_into().expect("4 bytes")) as usize;
        let known = matches!(
            head[0],
            TAG_DEFINE
                | TAG_RAW
                | TAG_META
                | TAG_GENERATION_COMPLETE
                | TAG_META_VERSIONED
                | TAG_RAW_BATCH
        );
        if !known || body_len > MAX_FRAME_BYTES {
            return Ok(torn(bytes, digest));
        }
        let mut body = vec![0_u8; body_len];
        if !read_exact_or_short(reader, &mut body)? {
            return Ok(torn(bytes, digest));
        }
        let mut checksum = [0_u8; CHECKSUM_BYTES];
        if !read_exact_or_short(reader, &mut checksum)? {
            return Ok(torn(bytes, digest));
        }
        let mut hasher = blake3::Hasher::new();
        hasher.update(&head);
        hasher.update(&body);
        if hasher.finalize().as_bytes()[..CHECKSUM_BYTES] != checksum {
            return Ok(torn(bytes, digest));
        }
        digest.update(&head);
        digest.update(&body);
        digest.update(&checksum);
        bytes += (head.len() + body_len + CHECKSUM_BYTES) as u64;
    }
}

fn read_exact_or_short(reader: &mut impl Read, buffer: &mut [u8]) -> std::io::Result<bool> {
    match reader.read_exact(buffer) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == ErrorKind::UnexpectedEof => Ok(false),
        Err(error) => Err(error),
    }
}

/// The frame loop over any container. Returns false at a torn or corrupt
/// frame; the caller folds in the container's own verdict.
fn scan_frames<E, F>(reader: &mut impl Read, path: &Path, visitor: &mut F) -> Result<bool, E>
where
    E: From<CaptureError>,
    F: FnMut(LedgerEntry) -> Result<(), E>,
{
    let mut defined: HashMap<u32, SourceDef> = HashMap::new();
    loop {
        let mut head = [0_u8; 5];
        match reader.read(&mut head[..1]) {
            Ok(0) => return Ok(true),
            Ok(1) => {}
            Ok(_) => unreachable!("one-byte read returned more than one byte"),
            Err(error) => {
                return Err(E::from(io_error("reading segment", path, error)));
            }
        }
        if !read_frame_part(reader, &mut head[1..], path)? {
            return Ok(false);
        }
        let tag = head[0];
        let body_len = u32::from_be_bytes(head[1..5].try_into().expect("4 bytes")) as usize;
        if body_len > MAX_FRAME_BYTES {
            return Ok(false);
        }
        let mut body = vec![0_u8; body_len];
        if !read_frame_part(reader, &mut body, path)? {
            return Ok(false);
        }
        let mut checksum = [0_u8; CHECKSUM_BYTES];
        if !read_frame_part(reader, &mut checksum, path)? {
            return Ok(false);
        }
        let mut hasher = blake3::Hasher::new();
        hasher.update(&head);
        hasher.update(&body);
        if hasher.finalize().as_bytes()[..CHECKSUM_BYTES] != checksum {
            return Ok(false);
        }
        match tag {
            TAG_DEFINE => {
                let Some((id, source)) = decode_define(&body) else {
                    return Ok(false);
                };
                defined.insert(id, source);
            }
            TAG_RAW => {
                if body_len < 20 {
                    return Ok(false);
                }
                let file_id = u32::from_be_bytes(body[0..4].try_into().expect("4 bytes"));
                let generation = u64::from_be_bytes(body[4..12].try_into().expect("8 bytes"));
                let start_offset = u64::from_be_bytes(body[12..20].try_into().expect("8 bytes"));
                let Some(source) = defined.get(&file_id) else {
                    // A raw record before its define: not a valid segment.
                    return Ok(false);
                };
                visitor(LedgerEntry::Raw {
                    source: source.clone(),
                    generation,
                    start_offset,
                    payload: body[20..].to_vec(),
                })?;
            }
            TAG_RAW_BATCH => {
                if body_len < 16 {
                    return Ok(false);
                }
                let file_id = u32::from_be_bytes(body[0..4].try_into().expect("4 bytes"));
                let generation = u64::from_be_bytes(body[4..12].try_into().expect("8 bytes"));
                let count = u32::from_be_bytes(body[12..16].try_into().expect("4 bytes"));
                if count == 0 {
                    return Ok(false);
                }
                let Some(source) = defined.get(&file_id) else {
                    return Ok(false);
                };
                let mut at = 16_usize;
                for _ in 0..count {
                    let Some(header_end) = at.checked_add(12) else {
                        return Ok(false);
                    };
                    if header_end > body_len {
                        return Ok(false);
                    }
                    let start_offset =
                        u64::from_be_bytes(body[at..at + 8].try_into().expect("8 bytes"));
                    let payload_len =
                        u32::from_be_bytes(body[at + 8..header_end].try_into().expect("4 bytes"))
                            as usize;
                    let Some(payload_end) = header_end.checked_add(payload_len) else {
                        return Ok(false);
                    };
                    if payload_end > body_len {
                        return Ok(false);
                    }
                    visitor(LedgerEntry::Raw {
                        source: source.clone(),
                        generation,
                        start_offset,
                        payload: body[header_end..payload_end].to_vec(),
                    })?;
                    at = payload_end;
                }
                if at != body_len {
                    return Ok(false);
                }
            }
            TAG_GENERATION_COMPLETE => {
                if body_len != 17 {
                    return Ok(false);
                }
                let file_id = u32::from_be_bytes(body[0..4].try_into().expect("4 bytes"));
                let generation = u64::from_be_bytes(body[4..12].try_into().expect("8 bytes"));
                let normalizer_version =
                    u32::from_be_bytes(body[12..16].try_into().expect("4 bytes"));
                let normalized_clean = match body[16] {
                    0 => false,
                    1 => true,
                    _ => return Ok(false),
                };
                let Some(source) = defined.get(&file_id) else {
                    return Ok(false);
                };
                visitor(LedgerEntry::GenerationComplete {
                    source: source.clone(),
                    generation,
                    normalizer_version,
                    normalized_clean,
                })?;
            }
            TAG_META => {
                let Ok(text) = std::str::from_utf8(&body) else {
                    return Ok(false);
                };
                visitor(LedgerEntry::Meta {
                    schema_version: 0,
                    payload: text.to_string(),
                })?;
            }
            TAG_META_VERSIONED => {
                if body_len < 4 {
                    return Ok(false);
                }
                let schema_version = u32::from_be_bytes(body[0..4].try_into().expect("4 bytes"));
                let Ok(text) = std::str::from_utf8(&body[4..]) else {
                    return Ok(false);
                };
                visitor(LedgerEntry::Meta {
                    schema_version,
                    payload: text.to_string(),
                })?;
            }
            _ => return Ok(false),
        }
    }
}

fn read_frame_part<E: From<CaptureError>>(
    reader: &mut impl Read,
    buffer: &mut [u8],
    path: &Path,
) -> Result<bool, E> {
    match reader.read_exact(buffer) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == ErrorKind::UnexpectedEof => Ok(false),
        Err(error) => Err(E::from(io_error("reading segment", path, error))),
    }
}

fn segment_name(seq: u64) -> String {
    format!("{seq:08}.rledger")
}

pub(crate) fn segment_files(dir: &Path) -> Result<Vec<(u64, PathBuf)>, CaptureError> {
    let mut out = Vec::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(error) => return Err(io_error("listing", dir, error)),
    };
    for entry in entries {
        let entry = entry.map_err(|error| io_error("listing", dir, error))?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(stem) = name.strip_suffix(".rledger") else {
            continue;
        };
        let Ok(seq) = stem.parse::<u64>() else {
            continue;
        };
        out.push((seq, entry.path()));
    }
    out.sort();
    Ok(out)
}

/// Tiny flat JSON encoder for defines — string fields only, no serde dep in
/// this crate; the decoder below is the only consumer.
fn serde_encode(fields: &[(&str, &str)]) -> String {
    let mut out = String::from("{");
    for (index, (key, value)) in fields.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push('"');
        out.push_str(key);
        out.push_str("\":\"");
        for ch in value.chars() {
            match ch {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                ch if (ch as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", ch as u32)),
                ch => out.push(ch),
            }
        }
        out.push('"');
    }
    out.push('}');
    out
}

fn decode_define(body: &[u8]) -> Option<(u32, SourceDef)> {
    let text = std::str::from_utf8(body).ok()?;
    let get = |key: &str| -> Option<String> {
        let needle = format!("\"{key}\":\"");
        let start = text.find(&needle)? + needle.len();
        let mut out = String::new();
        let mut chars = text[start..].chars();
        loop {
            match chars.next()? {
                '"' => break,
                '\\' => match chars.next()? {
                    '"' => out.push('"'),
                    '\\' => out.push('\\'),
                    'n' => out.push('\n'),
                    'u' => {
                        let code: String = (0..4).filter_map(|_| chars.next()).collect();
                        out.push(char::from_u32(u32::from_str_radix(&code, 16).ok()?)?);
                    }
                    other => out.push(other),
                },
                ch => out.push(ch),
            }
        }
        Some(out)
    };
    let file_id = get("file_id")?.parse().ok()?;
    let agent_version = get("agent_version")?;
    Some((
        file_id,
        SourceDef {
            ledger_only: match get("ledger_only").as_deref() {
                None | Some("false") => false,
                Some("true") => true,
                _ => return None,
            },
            relative_path: get("relative_path")?,
            stream_id: get("stream_id")?,
            session_id: get("session_id")?,
            native_session_id: get("native_session_id")?,
            agent_version: (!agent_version.is_empty()).then_some(agent_version),
        },
    ))
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    fn source(path: &str) -> SourceDef {
        SourceDef {
            ledger_only: false,
            relative_path: path.to_string(),
            stream_id: "transcript".to_string(),
            session_id: "s-canonical".to_string(),
            native_session_id: "s-native".to_string(),
            agent_version: Some("1.2.3".to_string()),
        }
    }

    #[test]
    fn source_admission_is_backward_compatible_and_round_trips() {
        let legacy = br#"{"file_id":"0","relative_path":"a.jsonl","stream_id":"rollout","session_id":"s","native_session_id":"n","agent_version":"1"}"#;
        assert!(!decode_define(legacy).unwrap().1.ledger_only);

        let dir = TempDir::new().unwrap();
        let mut ledger = Ledger::open(dir.path()).unwrap();
        let mut internal = source("internal.jsonl");
        internal.ledger_only = true;
        ledger
            .append_raw(&internal, 1, [(0, b"private".as_slice())])
            .unwrap();
        ledger.flush().unwrap();
        drop(ledger);
        let scanned = scan(dir.path()).unwrap();
        assert_eq!(scanned.torn_segments, 0);
        assert!(
            matches!(&scanned.entries[0], LedgerEntry::Raw { source, .. } if source == &internal)
        );
        assert_eq!(internal.projected_session_id(), None);
    }

    #[test]
    fn round_trips_raw_and_meta_entries_in_order() {
        let dir = TempDir::new().unwrap();
        let mut ledger = Ledger::open(dir.path()).unwrap();
        ledger.append_meta(1, "[{\"op\":\"runtime\"}]").unwrap();
        ledger
            .append_raw(
                &source("a.jsonl"),
                1,
                [(0_u64, b"one".as_slice()), (4, b"two")],
            )
            .unwrap();
        ledger
            .append_raw(&source("b.jsonl"), 3, [(10_u64, b"three".as_slice())])
            .unwrap();
        ledger.checkpoint().unwrap();

        let scanned = scan(dir.path()).unwrap();
        assert_eq!(scanned.segments, 1);
        assert_eq!(scanned.torn_segments, 0);
        assert_eq!(scanned.entries.len(), 4);
        assert_eq!(
            scanned.entries[0],
            LedgerEntry::Meta {
                schema_version: 1,
                payload: "[{\"op\":\"runtime\"}]".to_string(),
            }
        );
        let LedgerEntry::Raw {
            source: from_scan,
            generation,
            start_offset,
            payload,
        } = &scanned.entries[2]
        else {
            panic!("expected a raw entry");
        };
        assert_eq!(from_scan, &source("a.jsonl"));
        assert_eq!((*generation, *start_offset), (1, 4));
        assert_eq!(payload, b"two");
    }

    #[test]
    fn semantic_meta_head_skips_identical_warm_snapshots() {
        let dir = TempDir::new().unwrap();
        let mut ledger = Ledger::open(dir.path()).unwrap();
        assert!(
            ledger
                .append_meta_checkpoint_if_changed(2, "[{\"run\":1}]", "stable-a")
                .unwrap()
        );
        assert!(
            !ledger
                .append_meta_checkpoint_if_changed(2, "[{\"run\":2}]", "stable-a")
                .unwrap()
        );
        assert!(
            ledger
                .append_meta_checkpoint_if_changed(2, "[{\"run\":3}]", "stable-b")
                .unwrap()
        );

        let meta = scan(dir.path())
            .unwrap()
            .entries
            .into_iter()
            .filter_map(|entry| match entry {
                LedgerEntry::Meta { payload, .. } => Some(payload),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(meta, ["[{\"run\":1}]", "[{\"run\":3}]"]);
    }

    #[test]
    fn write_stats_split_frames_flush_and_sync_without_changing_the_ledger() {
        let dir = TempDir::new().unwrap();
        let mut ledger = Ledger::open(dir.path()).unwrap();
        ledger
            .append_raw(
                &source("a.jsonl"),
                1,
                [(0_u64, b"one".as_slice()), (4, b"two")],
            )
            .unwrap();
        ledger.checkpoint().unwrap();

        let stats = ledger.take_write_stats();
        assert_eq!(stats.frames, 3, "one define plus two raw frames");
        assert!(stats.frame_bytes > 6);
        assert_eq!(stats.flushes, 1);
        assert_eq!(stats.syncs, 1, "default checkpoints stay live-durable");
        assert_eq!(stats.directory_syncs, 1);
        assert_eq!(stats.segments_opened, 1);
        assert_eq!(ledger.take_write_stats(), LedgerWriteStats::default());

        let scanned = scan(dir.path()).unwrap();
        assert_eq!(scanned.entries.len(), 2);
        assert_eq!(scanned.torn_segments, 0);
    }

    #[test]
    fn asynchronous_owned_writer_preserves_order_and_joins_at_flush() {
        let dir = TempDir::new().unwrap();
        let source = source("async.jsonl");
        let mut ledger = Ledger::open(dir.path())
            .unwrap()
            .defer_sync_until_flush()
            .into_async(2);
        ledger.append_meta(2, "[]").unwrap();
        ledger.checkpoint().unwrap();
        ledger
            .append_raw_owned(&source, 1, vec![(0, b"one".to_vec()), (4, b"two".to_vec())])
            .unwrap();
        ledger.complete_generation(&source, 1, 7, true).unwrap();
        ledger.checkpoint().unwrap();
        ledger.flush().unwrap();

        let stats = ledger.take_write_stats();
        assert_eq!(stats.syncs, 1);
        let scanned = scan(dir.path()).unwrap();
        assert_eq!(scanned.torn_segments, 0);
        assert!(matches!(scanned.entries[0], LedgerEntry::Meta { .. }));
        assert!(matches!(
            scanned.entries.last(),
            Some(LedgerEntry::GenerationComplete { generation: 1, .. })
        ));
        let payloads = scanned
            .entries
            .iter()
            .filter_map(|entry| match entry {
                LedgerEntry::Raw { payload, .. } => Some(payload.as_slice()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(payloads, [b"one".as_slice(), b"two".as_slice()]);
    }

    #[test]
    fn deferred_checkpoints_sync_every_rotated_segment_at_the_final_barrier() {
        let dir = TempDir::new().unwrap();
        let mut ledger = Ledger::open(dir.path())
            .unwrap()
            .with_max_segment_bytes(64)
            .defer_sync_until_flush();
        for index in 0..8_u64 {
            ledger
                .append_raw(
                    &source("a.jsonl"),
                    1,
                    [(index * 8, format!("record-{index}").into_bytes())],
                )
                .unwrap();
            ledger.checkpoint().unwrap();
        }

        let checkpoints = ledger.take_write_stats();
        assert_eq!(checkpoints.flushes, 8);
        assert_eq!(checkpoints.syncs, 0);
        assert_eq!(checkpoints.directory_syncs, 0);
        assert_eq!(checkpoints.segments_opened, 8);

        ledger.flush().unwrap();
        let barrier = ledger.take_write_stats();
        assert_eq!(barrier.syncs, 8);
        assert_eq!(barrier.directory_syncs, 1);
        assert_eq!(barrier.flushes, 0, "every segment already rotated");

        let scanned = scan(dir.path()).unwrap();
        assert_eq!(scanned.segments, 8);
        assert_eq!(scanned.torn_segments, 0);
        assert_eq!(
            scanned
                .entries
                .iter()
                .filter(|entry| matches!(entry, LedgerEntry::Raw { .. }))
                .count(),
            8
        );
    }

    #[test]
    fn first_barrier_syncs_new_directory_tree_child_first() {
        let temp = TempDir::new().unwrap();
        let dir = temp.path().join("ledger").join("runtime");
        let mut ledger = Ledger::open(&dir).unwrap();
        ledger
            .append_raw(&source("a.jsonl"), 1, [(0_u64, b"one".as_slice())])
            .unwrap();
        ledger.checkpoint().unwrap();

        let stats = ledger.take_write_stats();
        assert_eq!(stats.syncs, 1);
        assert_eq!(
            stats.directory_syncs, 3,
            "runtime, ledger root, and the pre-existing parent need barriers"
        );
    }

    #[test]
    fn generation_seals_carry_normalizer_outcome_and_source_identity() {
        let dir = TempDir::new().unwrap();
        let mut ledger = Ledger::open(dir.path()).unwrap();
        let source = source("a.jsonl");
        ledger
            .append_raw(&source, 7, [(0_u64, b"record".as_slice())])
            .unwrap();
        ledger.complete_generation(&source, 7, 4, false).unwrap();
        ledger.flush().unwrap();

        let scanned = scan(dir.path()).unwrap();
        assert!(matches!(
            &scanned.entries[1],
            LedgerEntry::GenerationComplete {
                source: sealed,
                generation: 7,
                normalizer_version: 4,
                normalized_clean: false,
            } if sealed == &source
        ));
    }

    #[test]
    fn same_path_with_new_source_metadata_gets_a_fresh_definition() {
        let dir = TempDir::new().unwrap();
        let mut ledger = Ledger::open(dir.path()).unwrap();
        let old = source("a.jsonl");
        let mut new = old.clone();
        new.agent_version = Some("2.0.0".to_string());
        ledger
            .append_raw(&old, 1, [(0_u64, b"old".as_slice())])
            .unwrap();
        ledger
            .append_raw(&new, 1, [(4_u64, b"new".as_slice())])
            .unwrap();
        ledger.flush().unwrap();

        let scanned = scan(dir.path()).unwrap();
        let sources: Vec<_> = scanned
            .entries
            .iter()
            .filter_map(|entry| match entry {
                LedgerEntry::Raw { source, .. } => Some(source.agent_version.as_deref()),
                _ => None,
            })
            .collect();
        assert_eq!(sources, [Some("1.2.3"), Some("2.0.0")]);
    }

    #[test]
    fn reopening_continues_in_a_new_segment_and_redefines_sources() {
        let dir = TempDir::new().unwrap();
        let mut ledger = Ledger::open(dir.path()).unwrap();
        ledger
            .append_raw(&source("a.jsonl"), 1, [(0_u64, b"first".as_slice())])
            .unwrap();
        ledger.flush().unwrap();
        drop(ledger);

        let mut ledger = Ledger::open(dir.path()).unwrap();
        ledger
            .append_raw(&source("a.jsonl"), 1, [(6_u64, b"second".as_slice())])
            .unwrap();
        ledger.flush().unwrap();

        let scanned = scan(dir.path()).unwrap();
        assert_eq!(scanned.segments, 2);
        assert_eq!(scanned.torn_segments, 0);
        let raw: Vec<&[u8]> = scanned
            .entries
            .iter()
            .filter_map(|entry| match entry {
                LedgerEntry::Raw { payload, .. } => Some(payload.as_slice()),
                LedgerEntry::Meta { .. } | LedgerEntry::GenerationComplete { .. } => None,
            })
            .collect();
        assert_eq!(raw, [b"first".as_slice(), b"second"]);
    }

    #[test]
    fn rotation_by_size_keeps_segments_self_contained() {
        let dir = TempDir::new().unwrap();
        let mut ledger = Ledger::open(dir.path()).unwrap().with_max_segment_bytes(64);
        for index in 0..8_u64 {
            ledger
                .append_raw(
                    &source("a.jsonl"),
                    1,
                    [(index * 8, format!("record-{index}").into_bytes())],
                )
                .unwrap();
            ledger.flush().unwrap();
        }

        let scanned = scan(dir.path()).unwrap();
        assert!(scanned.segments > 1, "64-byte cap must force rotation");
        assert_eq!(scanned.torn_segments, 0);
        let raw_count = scanned
            .entries
            .iter()
            .filter(|entry| matches!(entry, LedgerEntry::Raw { .. }))
            .count();
        assert_eq!(raw_count, 8, "rotation must not lose or split records");
    }

    #[test]
    fn a_torn_tail_is_harmless_and_prior_entries_survive() {
        let dir = TempDir::new().unwrap();
        let mut ledger = Ledger::open(dir.path())
            .unwrap()
            .with_codec(SegmentCodec::Plain);
        ledger
            .append_raw(&source("a.jsonl"), 1, [(0_u64, b"intact".as_slice())])
            .unwrap();
        ledger.flush().unwrap();
        drop(ledger);

        // Simulate a crash mid-append: garbage after the durable point.
        let segment = dir.path().join(segment_name(0));
        let mut file = OpenOptions::new().append(true).open(&segment).unwrap();
        file.write_all(&[TAG_RAW, 0, 0, 1]).unwrap(); // truncated header

        let scanned = scan(dir.path()).unwrap();
        assert_eq!(scanned.torn_segments, 1);
        assert_eq!(scanned.entries.len(), 1); // the intact record (defines resolve, they don't surface)

        // A checksum flip inside a frame also just stops the segment.
        let mut bytes = std::fs::read(&segment).unwrap();
        let flip_at = MAGIC_LEN + 7;
        bytes[flip_at] ^= 0xFF;
        std::fs::write(&segment, &bytes).unwrap();
        let scanned = scan(dir.path()).unwrap();
        assert_eq!(scanned.torn_segments, 1);
        assert!(scanned.entries.is_empty());
    }

    #[test]
    fn define_encoding_survives_hostile_strings() {
        let dir = TempDir::new().unwrap();
        let mut ledger = Ledger::open(dir.path()).unwrap();
        let hostile = SourceDef {
            ledger_only: false,
            relative_path: "we\"ird\\path\n.jsonl".to_string(),
            stream_id: "transcript".to_string(),
            session_id: "s".to_string(),
            native_session_id: "n".to_string(),
            agent_version: None,
        };
        ledger
            .append_raw(&hostile, 1, [(0_u64, b"x".as_slice())])
            .unwrap();
        ledger.flush().unwrap();

        let scanned = scan(dir.path()).unwrap();
        let LedgerEntry::Raw { source, .. } = &scanned.entries[0] else {
            panic!("expected raw");
        };
        assert_eq!(source, &hostile);
    }
}
