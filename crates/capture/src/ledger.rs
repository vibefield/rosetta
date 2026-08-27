//! The append-only raw ledger — the FIRST of the two data representations
//! (iron rule 4). The projection is disposable because this is not: raw
//! records land here, durably flushed, BEFORE the SQLite transaction that
//! projects them (docs/ingest-engine.html, crash protocol). Recovery may
//! find an orphan ledger tail (harmless — the projection never referenced
//! it); it must never find a projection row whose raw bytes were not
//! flushed first. That ordering is the caller's contract: `append*` then
//! `flush()` then the store tx.
//!
//! Layout: one directory per runtime, numbered segment files, rotated by
//! size. Segments are self-contained: every source a segment references is
//! (re)defined inside it, so a scan never needs another segment's state and
//! deleting old segments (future GC) cannot dangle references.
//!
//! Frames: `tag ∙ u32 body-len ∙ body ∙ 8-byte blake3 checksum`. A torn or
//! corrupt frame ends the ordinary/full segment scan — everything before it
//! is intact. The rebuild election scan may skip raw/meta payload checksums,
//! but publication requires its report to match a subsequent full scan.

use std::collections::{HashMap, HashSet};
use std::fs::{File, Metadata, OpenOptions};
use std::io::{BufReader, BufWriter, ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Instant, UNIX_EPOCH};

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

use crate::{CaptureError, io_error};

const SEGMENT_MAGIC: &[u8] = b"RLG1";
const CHECKSUM_BYTES: usize = 8;

const TAG_DEFINE: u8 = 1;
const TAG_RAW: u8 = 2;
/// Legacy unversioned canonical-op meta payload.
const TAG_META: u8 = 3;
const TAG_GENERATION_COMPLETE: u8 = 4;
const TAG_META_VERSIONED: u8 = 5;
const MAX_FRAME_BYTES: usize = 256 * 1024 * 1024;
const ELECTION_BUFFER_BYTES: usize = 1024 * 1024;

/// Everything replay needs to rebuild a normalization context for a source
/// file. Written into each segment the first time the source appears there.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SourceDef {
    pub relative_path: String,
    pub stream_id: String,
    pub session_id: String,
    pub native_session_id: String,
    pub agent_version: Option<String>,
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

/// The subset of a frame that generation election is allowed to observe.
/// Raw and meta payloads are deliberately absent: the first rebuild pass can
/// seek over them, while the ordinary second pass remains the authority that
/// reads, checksums, decodes, and replays every complete frame.
#[derive(Debug, Clone, Copy)]
pub enum LedgerElectionEntry<'a> {
    /// Emitted once for each relative path that owns at least one raw frame.
    RawSource { source: &'a SourceDef },
    GenerationComplete {
        source: &'a SourceDef,
        generation: u64,
        normalizer_version: u32,
        normalized_clean: bool,
    },
}

/// Non-overlapping append-side measurements. The pipeline drains these after
/// each durable ledger batch so profiling never changes the write ordering.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LedgerWriteStats {
    pub frames: u64,
    pub frame_bytes: u64,
    pub flushes: u64,
    pub syncs: u64,
    pub segments_opened: u64,
    /// Body construction/copy plus frame-header and checksum work.
    pub frame_nanos: u64,
    /// Bytes handed to the buffered writer, including any writes it triggers.
    pub buffer_write_nanos: u64,
    pub flush_nanos: u64,
    pub sync_nanos: u64,
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
    writer: Option<BufWriter<File>>,
    written: u64,
    /// Sources already defined in the CURRENT segment → their local id.
    defined: HashMap<SourceDef, u32>,
    write_stats: LedgerWriteStats,
}

impl Ledger {
    /// Open (or create) the runtime's ledger directory and continue after
    /// the highest existing segment — existing segments are never reopened
    /// for writing, so a previous crash's torn tail stays untouched.
    pub fn open(dir: &Path) -> Result<Self, CaptureError> {
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
            written: 0,
            defined: HashMap::new(),
            write_stats: LedgerWriteStats::default(),
        })
    }

    pub fn with_max_segment_bytes(mut self, max: u64) -> Self {
        self.max_segment_bytes = max.max(1);
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
        let file_id = self.define(source)?;
        for (start_offset, payload) in records {
            let payload = payload.as_ref();
            let started = Instant::now();
            let mut body = Vec::with_capacity(20 + payload.len());
            body.extend_from_slice(&file_id.to_be_bytes());
            body.extend_from_slice(&generation.to_be_bytes());
            body.extend_from_slice(&start_offset.to_be_bytes());
            body.extend_from_slice(payload);
            self.write_stats.frame_nanos = self
                .write_stats
                .frame_nanos
                .saturating_add(elapsed_nanos(started));
            self.frame(TAG_RAW, &body)?;
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
        let file_id = self.define(source)?;
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

    /// Durably flush everything appended so far. MUST complete before the
    /// store transaction that projects those records commits.
    pub fn flush(&mut self) -> Result<(), CaptureError> {
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

        self.write_stats.syncs = self.write_stats.syncs.saturating_add(1);
        let started = Instant::now();
        let synced = writer.get_ref().sync_data();
        self.write_stats.sync_nanos = self
            .write_stats
            .sync_nanos
            .saturating_add(elapsed_nanos(started));
        synced.map_err(|error| io_error("syncing", &path, error))?;
        // Rotate AFTER the durable point so a segment never splits a batch.
        if self.written >= self.max_segment_bytes {
            self.writer = None;
            self.defined.clear();
            self.segment_seq += 1;
            self.written = 0;
        }
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

    fn ensure_writer(&mut self) -> Result<(), CaptureError> {
        if self.writer.is_some() {
            return Ok(());
        }
        let path = self.dir.join(segment_name(self.segment_seq));
        let started = Instant::now();
        let opened = (|| {
            let mut file = OpenOptions::new()
                .create_new(true)
                .append(true)
                .open(&path)
                .map_err(|error| io_error("creating segment", &path, error))?;
            file.write_all(SEGMENT_MAGIC)
                .map_err(|error| io_error("writing segment header to", &path, error))?;
            Ok::<_, CaptureError>(file)
        })();
        self.write_stats.segment_open_nanos = self
            .write_stats
            .segment_open_nanos
            .saturating_add(elapsed_nanos(started));
        let file = opened?;
        self.write_stats.segments_opened = self.write_stats.segments_opened.saturating_add(1);
        self.written = SEGMENT_MAGIC.len() as u64;
        self.writer = Some(BufWriter::new(file));
        Ok(())
    }
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
    /// Hash of the exact segment names and file identities/lengths observed
    /// by this pass. Private so callers compare complete reports rather than
    /// treating it as a durable or portable content fingerprint.
    segment_snapshot: [u8; 32],
}

/// Visit every ledger entry in segment order without retaining payloads.
/// Torn/corrupt tails stop only their segment; callback errors are propagated.
pub fn visit<E, F>(dir: &Path, mut visitor: F) -> Result<LedgerVisit, E>
where
    E: From<CaptureError>,
    F: FnMut(LedgerEntry) -> Result<(), E>,
{
    let mut report = LedgerVisit::default();
    let mut snapshot = blake3::Hasher::new();
    for (sequence, path) in segment_files(dir).map_err(E::from)? {
        report.segments += 1;
        let file = File::open(&path)
            .map_err(|error| E::from(io_error("opening segment", &path, error)))?;
        let before = file
            .metadata()
            .map_err(|error| E::from(io_error("stating segment", &path, error)))?;
        let identity = segment_identity(sequence, &path, &before);
        snapshot.update(&identity);
        let mut counted = |entry| {
            report.entries += 1;
            visitor(entry)
        };
        let intact = scan_segment(&path, &file, before.len(), &mut counted)?;
        let after = file
            .metadata()
            .map_err(|error| E::from(io_error("restating segment", &path, error)))?;
        if !intact || identity != segment_identity(sequence, &path, &after) {
            report.torn_segments += 1;
        }
    }
    report.segment_snapshot = *snapshot.finalize().as_bytes();
    Ok(report)
}

/// Visit only the control information needed to elect source generations.
/// Define and completion frames are fully checksummed. Raw record prefixes
/// are decoded, but raw payloads/checksums and meta payloads/checksums are
/// skipped by seek; a subsequent ordinary [`visit`] must match this report
/// and independently reproduce the election before any rebuild is published.
pub fn visit_election<E, F>(dir: &Path, mut visitor: F) -> Result<LedgerVisit, E>
where
    E: From<CaptureError>,
    F: for<'a> FnMut(LedgerElectionEntry<'a>) -> Result<(), E>,
{
    let mut report = LedgerVisit::default();
    let mut snapshot = blake3::Hasher::new();
    let mut raw_paths = HashSet::new();
    for (sequence, path) in segment_files(dir).map_err(E::from)? {
        report.segments += 1;
        let file = File::open(&path)
            .map_err(|error| E::from(io_error("opening segment", &path, error)))?;
        let before = file
            .metadata()
            .map_err(|error| E::from(io_error("stating segment", &path, error)))?;
        let identity = segment_identity(sequence, &path, &before);
        snapshot.update(&identity);
        let intact = scan_election_segment(
            &path,
            &file,
            before.len(),
            &mut report.entries,
            &mut raw_paths,
            &mut visitor,
        )?;
        let after = file
            .metadata()
            .map_err(|error| E::from(io_error("restating segment", &path, error)))?;
        if !intact || identity != segment_identity(sequence, &path, &after) {
            report.torn_segments += 1;
        }
    }
    report.segment_snapshot = *snapshot.finalize().as_bytes();
    Ok(report)
}

/// Returns false if the segment ended in a torn/corrupt frame.
fn scan_segment<E, F>(
    path: &Path,
    file: &File,
    segment_len: u64,
    visitor: &mut F,
) -> Result<bool, E>
where
    E: From<CaptureError>,
    F: FnMut(LedgerEntry) -> Result<(), E>,
{
    let mut reader = BufReader::new(file.take(segment_len));
    let mut magic = vec![0_u8; SEGMENT_MAGIC.len()];
    if !read_frame_part(&mut reader, &mut magic, path)? || magic != SEGMENT_MAGIC {
        return Ok(false);
    }
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
        if !read_frame_part(&mut reader, &mut head[1..], path)? {
            return Ok(false);
        }
        let tag = head[0];
        let body_len = u32::from_be_bytes(head[1..5].try_into().expect("4 bytes")) as usize;
        if body_len > MAX_FRAME_BYTES {
            return Ok(false);
        }
        let Some(body) = read_verified_body(&mut reader, &head, body_len, path)? else {
            return Ok(false);
        };
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

/// Returns false if the segment ended in a torn/corrupt control frame. Raw
/// and meta payload checksums are intentionally deferred to the full pass.
fn scan_election_segment<E, F>(
    path: &Path,
    file: &File,
    segment_len: u64,
    entries: &mut u64,
    raw_paths: &mut HashSet<String>,
    visitor: &mut F,
) -> Result<bool, E>
where
    E: From<CaptureError>,
    F: for<'a> FnMut(LedgerElectionEntry<'a>) -> Result<(), E>,
{
    let mut reader = BufReader::with_capacity(ELECTION_BUFFER_BYTES, file);
    let mut magic = vec![0_u8; SEGMENT_MAGIC.len()];
    if !read_frame_part(&mut reader, &mut magic, path)? || magic != SEGMENT_MAGIC {
        return Ok(false);
    }
    let mut position = SEGMENT_MAGIC.len() as u64;
    let mut defined: HashMap<u32, SourceDef> = HashMap::new();
    loop {
        if position == segment_len {
            return Ok(true);
        }
        if position > segment_len {
            return Ok(false);
        }

        let mut head = [0_u8; 5];
        if !read_frame_part(&mut reader, &mut head, path)? {
            return Ok(false);
        }
        position += head.len() as u64;
        let tag = head[0];
        let body_len = u32::from_be_bytes(head[1..5].try_into().expect("4 bytes")) as usize;
        if body_len > MAX_FRAME_BYTES {
            return Ok(false);
        }
        let frame_end = position
            .checked_add(body_len as u64)
            .and_then(|end| end.checked_add(CHECKSUM_BYTES as u64));
        let Some(frame_end) = frame_end.filter(|end| *end <= segment_len) else {
            return Ok(false);
        };

        match tag {
            TAG_DEFINE => {
                let Some(body) = read_verified_body(&mut reader, &head, body_len, path)? else {
                    return Ok(false);
                };
                let Some((id, source)) = decode_define(&body) else {
                    return Ok(false);
                };
                defined.insert(id, source);
            }
            TAG_RAW => {
                if body_len < 20 {
                    return Ok(false);
                }
                let mut prefix = [0_u8; 20];
                if !read_frame_part(&mut reader, &mut prefix, path)? {
                    return Ok(false);
                }
                let file_id = u32::from_be_bytes(prefix[0..4].try_into().expect("4 bytes"));
                let Some(source) = defined.get(&file_id) else {
                    return Ok(false);
                };
                skip_relative(&mut reader, body_len - prefix.len() + CHECKSUM_BYTES, path)?;
                *entries = entries.saturating_add(1);
                if raw_paths.insert(source.relative_path.clone()) {
                    visitor(LedgerElectionEntry::RawSource { source })?;
                }
            }
            TAG_GENERATION_COMPLETE => {
                if body_len != 17 {
                    return Ok(false);
                }
                let Some(body) = read_verified_body(&mut reader, &head, body_len, path)? else {
                    return Ok(false);
                };
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
                *entries = entries.saturating_add(1);
                visitor(LedgerElectionEntry::GenerationComplete {
                    source,
                    generation,
                    normalizer_version,
                    normalized_clean,
                })?;
            }
            TAG_META => {
                skip_relative(&mut reader, body_len + CHECKSUM_BYTES, path)?;
                *entries = entries.saturating_add(1);
            }
            TAG_META_VERSIONED => {
                if body_len < 4 {
                    return Ok(false);
                }
                skip_relative(&mut reader, body_len + CHECKSUM_BYTES, path)?;
                *entries = entries.saturating_add(1);
            }
            _ => return Ok(false),
        }
        position = frame_end;
    }
}

fn read_verified_body<E: From<CaptureError>>(
    reader: &mut impl Read,
    head: &[u8; 5],
    body_len: usize,
    path: &Path,
) -> Result<Option<Vec<u8>>, E> {
    let mut body = vec![0_u8; body_len];
    if !read_frame_part(reader, &mut body, path)? {
        return Ok(None);
    }
    let mut checksum = [0_u8; CHECKSUM_BYTES];
    if !read_frame_part(reader, &mut checksum, path)? {
        return Ok(None);
    }
    let mut hasher = blake3::Hasher::new();
    hasher.update(head);
    hasher.update(&body);
    if hasher.finalize().as_bytes()[..CHECKSUM_BYTES] != checksum {
        return Ok(None);
    }
    Ok(Some(body))
}

fn skip_relative<E: From<CaptureError>>(
    reader: &mut BufReader<&File>,
    bytes: usize,
    path: &Path,
) -> Result<(), E> {
    reader
        .seek_relative(bytes as i64)
        .map_err(|error| E::from(io_error("seeking through segment", path, error)))
}

fn segment_identity(sequence: u64, path: &Path, metadata: &Metadata) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(&sequence.to_be_bytes());
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_default();
    hasher.update(&(name.len() as u64).to_be_bytes());
    hasher.update(name.as_bytes());
    hasher.update(&metadata.len().to_be_bytes());
    match metadata.modified() {
        Ok(modified) => match modified.duration_since(UNIX_EPOCH) {
            Ok(stamp) => {
                hasher.update(&[0]);
                hasher.update(&stamp.as_secs().to_be_bytes());
                hasher.update(&stamp.subsec_nanos().to_be_bytes());
            }
            Err(error) => {
                hasher.update(&[1]);
                hasher.update(&error.duration().as_secs().to_be_bytes());
                hasher.update(&error.duration().subsec_nanos().to_be_bytes());
            }
        },
        Err(_) => {
            hasher.update(&[2]);
        }
    }
    #[cfg(unix)]
    {
        hasher.update(&metadata.dev().to_be_bytes());
        hasher.update(&metadata.ino().to_be_bytes());
    }
    *hasher.finalize().as_bytes()
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

fn segment_files(dir: &Path) -> Result<Vec<(u64, PathBuf)>, CaptureError> {
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
            relative_path: path.to_string(),
            stream_id: "transcript".to_string(),
            session_id: "s-canonical".to_string(),
            native_session_id: "s-native".to_string(),
            agent_version: Some("1.2.3".to_string()),
        }
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
        ledger.flush().unwrap();

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
        ledger.flush().unwrap();

        let stats = ledger.take_write_stats();
        assert_eq!(stats.frames, 3, "one define plus two raw frames");
        assert!(stats.frame_bytes > 6);
        assert_eq!(stats.flushes, 1);
        assert_eq!(stats.syncs, 1);
        assert_eq!(stats.segments_opened, 1);
        assert_eq!(ledger.take_write_stats(), LedgerWriteStats::default());

        let scanned = scan(dir.path()).unwrap();
        assert_eq!(scanned.entries.len(), 2);
        assert_eq!(scanned.torn_segments, 0);
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
    fn payload_light_election_matches_the_full_structural_scan() {
        let dir = TempDir::new().unwrap();
        let mut ledger = Ledger::open(dir.path()).unwrap();
        let source = source("a.jsonl");
        ledger.append_meta(1, &"m".repeat(64 * 1024)).unwrap();
        ledger
            .append_raw(
                &source,
                7,
                [
                    (0_u64, vec![b'a'; 128 * 1024]),
                    (128 * 1024, vec![b'b'; 128 * 1024]),
                ],
            )
            .unwrap();
        ledger.complete_generation(&source, 7, 4, true).unwrap();
        ledger.flush().unwrap();

        let mut facts = Vec::new();
        let election = visit_election::<CaptureError, _>(dir.path(), |entry| {
            match entry {
                LedgerElectionEntry::RawSource { source } => {
                    facts.push(("raw", source.relative_path.clone(), 0, 0))
                }
                LedgerElectionEntry::GenerationComplete {
                    source, generation, ..
                } => facts.push(("seal", source.relative_path.clone(), generation, 0)),
            }
            Ok(())
        })
        .unwrap();
        let full = visit::<CaptureError, _>(dir.path(), |_| Ok(())).unwrap();

        assert_eq!(election, full);
        assert_eq!(election.entries, 4);
        assert_eq!(
            facts,
            [
                ("raw", "a.jsonl".to_string(), 0, 0),
                ("seal", "a.jsonl".to_string(), 7, 0),
            ]
        );
    }

    #[test]
    fn full_replay_exposes_a_raw_checksum_the_election_pass_skips() {
        let dir = TempDir::new().unwrap();
        let mut ledger = Ledger::open(dir.path()).unwrap();
        let source = source("a.jsonl");
        ledger
            .append_raw(&source, 1, [(0_u64, b"checksum-marker".as_slice())])
            .unwrap();
        ledger.complete_generation(&source, 1, 1, true).unwrap();
        ledger.flush().unwrap();
        drop(ledger);

        let segment = dir.path().join(segment_name(0));
        let mut bytes = std::fs::read(&segment).unwrap();
        let marker = b"checksum-marker";
        let payload_at = bytes
            .windows(marker.len())
            .position(|window| window == marker)
            .unwrap();
        bytes[payload_at] ^= 0x01;
        std::fs::write(&segment, bytes).unwrap();

        let mut saw_seal = false;
        let election = visit_election::<CaptureError, _>(dir.path(), |entry| {
            saw_seal |= matches!(entry, LedgerElectionEntry::GenerationComplete { .. });
            Ok(())
        })
        .unwrap();
        let full = visit::<CaptureError, _>(dir.path(), |_| Ok(())).unwrap();

        assert!(
            saw_seal,
            "the control pass intentionally skipped raw checksum work"
        );
        assert_eq!(election.torn_segments, 0);
        assert_eq!(full.torn_segments, 1);
        assert_ne!(election, full, "rebuild must reject the two-pass mismatch");
    }

    #[cfg(unix)]
    #[test]
    fn same_size_valid_segment_replacement_changes_the_pass_snapshot() {
        let dir = TempDir::new().unwrap();
        let mut ledger = Ledger::open(dir.path()).unwrap();
        let source = source("a.jsonl");
        ledger
            .append_raw(&source, 1, [(0_u64, b"mutable-marker".as_slice())])
            .unwrap();
        ledger.complete_generation(&source, 1, 1, true).unwrap();
        ledger.flush().unwrap();
        drop(ledger);

        let before = visit_election::<CaptureError, _>(dir.path(), |_| Ok(())).unwrap();
        let segment = dir.path().join(segment_name(0));
        let mut bytes = std::fs::read(&segment).unwrap();
        let marker = b"mutable-marker";
        let payload_at = bytes
            .windows(marker.len())
            .position(|window| window == marker)
            .unwrap();
        let head_at = payload_at - 20 - 5;
        assert_eq!(bytes[head_at], TAG_RAW);
        let body_len = u32::from_be_bytes(
            bytes[head_at + 1..head_at + 5]
                .try_into()
                .expect("frame body length"),
        ) as usize;
        bytes[payload_at] ^= 0x01;
        let checksum_at = head_at + 5 + body_len;
        let mut hasher = blake3::Hasher::new();
        hasher.update(&bytes[head_at..checksum_at]);
        bytes[checksum_at..checksum_at + CHECKSUM_BYTES]
            .copy_from_slice(&hasher.finalize().as_bytes()[..CHECKSUM_BYTES]);
        let replacement = dir.path().join("replacement");
        std::fs::write(&replacement, &bytes).unwrap();
        std::fs::rename(&replacement, &segment).unwrap();

        let after = visit::<CaptureError, _>(dir.path(), |_| Ok(())).unwrap();
        assert_eq!(before.segments, after.segments);
        assert_eq!(before.entries, after.entries);
        assert_eq!(before.torn_segments, after.torn_segments);
        assert_ne!(before, after, "file identity must bind the two passes");
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
        let mut ledger = Ledger::open(dir.path()).unwrap();
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
        let flip_at = SEGMENT_MAGIC.len() + 7;
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
