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
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::segment::{MAGIC_LEN, SegmentCodec, SegmentWriter};
use crate::segment_index::{self, IndexedSource, SegmentIndexLine};
use crate::{CaptureError, io_error};

mod frames;
mod raw;
mod read;
mod verify;
mod worker;

pub use frames::PrebuiltFrames;
use frames::{frame_checksum, frame_head, seal_body};
#[cfg(test)]
use read::decode_define;
pub(crate) use read::{FramePrefix, frame_prefix, scan_segment};
pub use read::{LedgerScan, LedgerVisit, scan, visit, visit_segments};
pub use verify::{EntryDigest, EntryTally, LedgerVerify, SegmentVerify, verify};
use worker::{AsyncBackend, AsyncCommand};

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
    /// Roster keys ([`Ledger::set_source_keys`]): a keyed source's file id
    /// in every segment is its key.
    keys: Option<Arc<HashMap<SourceDef, u32>>>,
    /// Sources outside the keys already defined in the CURRENT segment; the
    /// next one gets the id after them, above every key.
    unkeyed: u32,
    /// Private cold builds defer physical sync until their pre-publication
    /// barrier. Their async worker also coalesces intermediate buffer flushes.
    /// Live writers leave this false and retain per-batch durability.
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
            keys: None,
            unkeyed: 0,
            defer_checkpoint_sync: false,
            unsynced_segments: Vec::new(),
            unsynced_directories,
            write_stats: LedgerWriteStats::default(),
            async_backend: None,
        })
    }

    /// Snapshot segments that this writer can never append to again. The
    /// projection lock must outlive maintenance of these paths. Reopened
    /// writers always start above the existing maximum; the active segment
    /// and any segments created by later appends are excluded.
    pub fn closed_segment_paths(&self) -> Result<Vec<PathBuf>, CaptureError> {
        if self.async_backend.is_some() {
            return Err(CaptureError::InvalidConfig(
                "join the asynchronous ledger before selecting closed segments".into(),
            ));
        }
        Ok(segment_files(&self.dir)?
            .into_iter()
            .filter(|(sequence, _)| *sequence < self.segment_seq)
            .map(|(_, path)| path)
            .collect())
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
    /// their tail batch. Small records share a bounded frame so checksumming
    /// and compression writes are amortized. Large records retain the direct
    /// single-record path; order, payloads and checkpoint durability are unchanged.
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
        self.raw_owned_batch(file_id, generation, records)
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
        let body = seal_body(file_id, generation, normalizer_version, normalized_clean);
        self.frame(TAG_GENERATION_COMPLETE, &body)
    }

    /// Install roster keys: from now on a keyed source's file id, in every
    /// segment, is its key, and any other source gets an id above every key.
    /// A keyed source's frames can therefore be finished before the segment
    /// they land in is known ([`PrebuiltFrames`], [`Ledger::append_prebuilt`]).
    /// The keys must number their sources `0..n`, and the open segment must
    /// not have defined a source yet: one segment never mixes dense and keyed
    /// ids. On an asynchronous ledger the keys take effect in queue order.
    pub fn set_source_keys(
        &mut self,
        keys: Arc<HashMap<SourceDef, u32>>,
    ) -> Result<(), CaptureError> {
        validate_keys(&keys)?;
        if self.async_backend.is_some() {
            // The owner keeps them too, to refuse mis-keyed frames early.
            self.keys = Some(Arc::clone(&keys));
            return self.send_async(AsyncCommand::SourceKeys(keys));
        }
        if !self.defined.is_empty() {
            return Err(CaptureError::InvalidConfig(
                "roster keys must be installed before the open segment defines a source"
                    .to_string(),
            ));
        }
        self.keys = Some(keys);
        self.unkeyed = 0;
        Ok(())
    }

    /// Append one source generation's frames, finished ahead of the ledger
    /// under the source's roster key. The source is defined first if the
    /// open segment has not defined it, so the bytes land exactly where the
    /// ledger's own framing of the same records would have put them.
    pub fn append_prebuilt(
        &mut self,
        source: &SourceDef,
        frames: PrebuiltFrames,
    ) -> Result<(), CaptureError> {
        let key = self
            .keys
            .as_ref()
            .and_then(|keys| keys.get(source))
            .copied();
        if key != Some(frames.key()) {
            return Err(CaptureError::InvalidConfig(format!(
                "prebuilt frames for {} carry file id {}, but its roster key is {key:?}",
                source.relative_path,
                frames.key()
            )));
        }
        if self.async_backend.is_some() {
            return self.send_async(AsyncCommand::Prebuilt {
                source: source.clone(),
                frames,
            });
        }
        let file_id = self.define(source)?;
        self.write_frames(file_id, &frames)
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

    /// Cross one projection checkpoint. Live/default synchronous ledgers make
    /// it durable immediately. A deferred private ledger drains its buffer and
    /// rotates without syncing; its async worker may coalesce intermediate
    /// checkpoints until metadata, a source seal or rotation. The private
    /// caller must invoke [`Ledger::flush`] before publication.
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
        self.unkeyed = 0;
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
        let id = match &self.keys {
            None => self.defined.len() as u32,
            Some(keys) => match keys.get(source) {
                Some(key) => *key,
                None => {
                    let id = (keys.len() as u32)
                        .checked_add(self.unkeyed)
                        .ok_or_else(|| {
                            CaptureError::LimitExceeded("ledger file ids exhausted".to_string())
                        })?;
                    self.unkeyed += 1;
                    id
                }
            },
        };
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
        let head = frame_head(tag, body.len());
        let checksum = frame_checksum(&head, &[body]);
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
                .and_then(|_| writer.write_all(&checksum))
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

/// Roster keys must number their sources `0..n`, so keyed ids are distinct
/// and every unkeyed id (`n` and up) is disjoint from them.
fn validate_keys(keys: &HashMap<SourceDef, u32>) -> Result<(), CaptureError> {
    let count = u32::try_from(keys.len())
        .map_err(|_| CaptureError::LimitExceeded("more roster keys than file ids".to_string()))?;
    let mut seen = vec![false; keys.len()];
    for (source, key) in keys {
        let Some(slot) = seen.get_mut(*key as usize) else {
            return Err(CaptureError::InvalidConfig(format!(
                "roster key {key} for {} is outside 0..{count}",
                source.relative_path
            )));
        };
        if std::mem::replace(slot, true) {
            return Err(CaptureError::InvalidConfig(format!(
                "roster key {key} names two sources"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod prebuilt_tests;
#[cfg(test)]
mod tests;
