//! Rebuild-not-migrate, as code: drop the projection, replay the ledger.
//!
//! Two streaming passes over one runtime's ledger. Pass one finds each source
//! file's highest SEALED generation; an orphan partial rewrite has no seal and
//! can never displace the last complete generation. Pass two deduplicates raw
//! source positions, normalizes them in ledger order, activates at the seal,
//! and applies later records as append operations.
//!
//! Deterministic ids make all of this idempotent — a rebuild interrupted
//! halfway and rerun converges on the same projection.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Instant, UNIX_EPOCH};

use rosetta_adapter_support::{AgentAdapter, LineStatus, NormalizeCtx};
use rosetta_capture::{CaptureError, LedgerEntry, LedgerVisit, SourceDef};
use rosetta_core::IngestOp;
use rosetta_core::rows::{RuntimeRow, SessionRole, SessionRow, StreamState};
use rosetta_store::{ApplyStats, Batch, CheckpointUpdate, SourceMode, SourceWrite, Store, queries};
use serde::{Deserialize, Serialize};

use crate::PipelineError;
use crate::checkpoint::{PIPE_CHECKPOINT_VERSION, PipeCheckpoint};
use crate::instrument::{Instrument, Metric};
use crate::meta::SessionFile;
use crate::stream::StreamReport;
use crate::stream::merge_stats;

/// Rebuild-only controls. The live path keeps its independently bounded
/// scheduler; these knobs let a private, unpublished rebuild spend more RAM
/// to amortize transaction and reconciliation overhead.
#[derive(Debug, Clone)]
pub struct RebuildOptions {
    pub chunk_ops: usize,
    /// Compatibility callers rebuild one runtime at a time and may inspect
    /// its tool rows immediately. Whole-projection rebuilds set this false
    /// and reconcile once after every runtime has contributed its facts.
    pub flush_reconciliation: bool,
    /// Benchmark the generation-control scan without replaying payloads.
    pub election_only: bool,
    /// Disposable writer-maintained generation control index. This
    /// experiment models the ledger v2 design: capture updates a tiny sidecar
    /// as it seals generations, so rebuild does not reread raw payloads merely
    /// to discover which generation it should replay.
    pub election_cache: Option<PathBuf>,
    /// Read/checksum ledger frames on a bounded producer thread while the
    /// caller normalizes and applies previously framed records.
    pub prefetch_ledger: bool,
}

impl Default for RebuildOptions {
    fn default() -> Self {
        Self {
            chunk_ops: 512,
            flush_reconciliation: true,
            election_only: false,
            election_cache: None,
            prefetch_ledger: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SegmentStamp {
    name: String,
    bytes: u64,
    modified_nanos: u128,
}

#[derive(Debug, Serialize, Deserialize)]
struct ElectionCache {
    schema: u32,
    normalizer_version: u32,
    segments: Vec<SegmentStamp>,
    visit_segments: usize,
    visit_torn_segments: usize,
    visit_entries: u64,
    surviving: BTreeMap<String, (u64, SourceDef)>,
    latest_seals: BTreeMap<String, (u64, SourceDef, bool)>,
    only_dirty: HashSet<String>,
}

struct ElectionState {
    visit: LedgerVisit,
    surviving: BTreeMap<String, (u64, SourceDef)>,
    latest_seals: BTreeMap<String, (u64, SourceDef, bool)>,
    only_dirty: HashSet<String>,
}

fn segment_stamps(ledger_dir: &Path) -> Result<Vec<SegmentStamp>, PipelineError> {
    let mut stamps = Vec::new();
    for entry in std::fs::read_dir(ledger_dir).map_err(|error| {
        PipelineError::Config(format!("listing ledger {}: {error}", ledger_dir.display()))
    })? {
        let entry = entry.map_err(|error| {
            PipelineError::Config(format!("listing ledger {}: {error}", ledger_dir.display()))
        })?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(stem) = name.strip_suffix(".rledger") else {
            continue;
        };
        if stem.parse::<u64>().is_err() {
            continue;
        }
        let metadata = entry.metadata().map_err(|error| {
            PipelineError::Config(format!(
                "stating ledger segment {}: {error}",
                entry.path().display()
            ))
        })?;
        let modified_nanos = metadata
            .modified()
            .ok()
            .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        stamps.push(SegmentStamp {
            name,
            bytes: metadata.len(),
            modified_nanos,
        });
    }
    stamps.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(stamps)
}

fn load_election_cache(
    path: &Path,
    normalizer_version: u32,
    segments: &[SegmentStamp],
) -> Option<ElectionState> {
    let payload = std::fs::read(path).ok()?;
    let cache: ElectionCache = serde_json::from_slice(&payload).ok()?;
    if cache.schema != 1
        || cache.normalizer_version != normalizer_version
        || cache.segments != segments
    {
        return None;
    }
    Some(ElectionState {
        visit: LedgerVisit {
            segments: cache.visit_segments,
            torn_segments: cache.visit_torn_segments,
            entries: cache.visit_entries,
        },
        surviving: cache.surviving,
        latest_seals: cache.latest_seals,
        only_dirty: cache.only_dirty,
    })
}

fn save_election_cache(
    path: &Path,
    normalizer_version: u32,
    segments: Vec<SegmentStamp>,
    state: &ElectionState,
) -> Result<(), PipelineError> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent).map_err(|error| {
            PipelineError::Config(format!(
                "creating election cache {}: {error}",
                parent.display()
            ))
        })?;
    }
    let cache = ElectionCache {
        schema: 1,
        normalizer_version,
        segments,
        visit_segments: state.visit.segments,
        visit_torn_segments: state.visit.torn_segments,
        visit_entries: state.visit.entries,
        surviving: state.surviving.clone(),
        latest_seals: state.latest_seals.clone(),
        only_dirty: state.only_dirty.clone(),
    };
    let payload = serde_json::to_vec(&cache).map_err(|error| {
        PipelineError::Config(format!(
            "encoding election cache {}: {error}",
            path.display()
        ))
    })?;
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    std::fs::write(&temporary, payload).map_err(|error| {
        PipelineError::Config(format!(
            "writing election cache {}: {error}",
            temporary.display()
        ))
    })?;
    std::fs::rename(&temporary, path).map_err(|error| {
        PipelineError::Config(format!(
            "installing election cache {} from {}: {error}",
            path.display(),
            temporary.display()
        ))
    })?;
    Ok(())
}

/// `visit_ledger` interleaves IO/decoding with the caller's normalization and
/// SQLite work. Time the visitor separately and subtract it so "ledger read"
/// remains a useful non-overlapping IO/decode measurement.
fn visit_ledger_profiled<E, F>(
    ledger_dir: &Path,
    instrument: &Instrument,
    pass_metric: Metric,
    mut visitor: F,
) -> Result<LedgerVisit, E>
where
    E: From<CaptureError>,
    F: FnMut(LedgerEntry) -> Result<(), E>,
{
    let started = Instant::now();
    let mut visitor_nanos = 0_u64;
    let result = rosetta_capture::visit_ledger::<E, _>(ledger_dir, |entry| {
        let visitor_started = Instant::now();
        let result = visitor(entry);
        visitor_nanos = visitor_nanos.saturating_add(
            u64::try_from(visitor_started.elapsed().as_nanos()).unwrap_or(u64::MAX),
        );
        result
    });
    let total_nanos = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
    let ledger_nanos = total_nanos.saturating_sub(visitor_nanos);
    instrument.add(Metric::LedgerReadNanos, ledger_nanos);
    instrument.add(pass_metric, ledger_nanos);
    result
}

fn visit_election<E, F>(
    ledger_dir: &Path,
    instrument: &Instrument,
    visitor: F,
) -> Result<LedgerVisit, E>
where
    E: From<CaptureError>,
    F: FnMut(LedgerEntry) -> Result<(), E>,
{
    visit_ledger_profiled(
        ledger_dir,
        instrument,
        Metric::LedgerElectionReadNanos,
        visitor,
    )
}

fn visit_replay<E, F>(
    ledger_dir: &Path,
    instrument: &Instrument,
    visitor: F,
) -> Result<LedgerVisit, E>
where
    E: From<CaptureError>,
    F: FnMut(LedgerEntry) -> Result<(), E>,
{
    visit_ledger_profiled(
        ledger_dir,
        instrument,
        Metric::LedgerReplayReadNanos,
        visitor,
    )
}

#[derive(Debug)]
enum ReplayProducerError {
    Capture(CaptureError),
    ConsumerGone,
}

impl From<CaptureError> for ReplayProducerError {
    fn from(error: CaptureError) -> Self {
        Self::Capture(error)
    }
}

/// Keep at most a handful of owned frames ahead. This overlaps ledger I/O,
/// checksum, and payload framing with normalization/SQLite without turning a
/// 10 GiB ledger into a second in-memory corpus.
fn visit_replay_prefetched<F>(
    ledger_dir: &Path,
    instrument: &Instrument,
    mut visitor: F,
) -> Result<LedgerVisit, PipelineError>
where
    F: FnMut(LedgerEntry) -> Result<(), PipelineError>,
{
    const PREFETCH_FRAMES: usize = 16;
    let (sender, receiver) = mpsc::sync_channel(PREFETCH_FRAMES);
    std::thread::scope(|scope| {
        let producer = scope.spawn(move || {
            let started = Instant::now();
            let mut send_nanos = 0_u64;
            let result =
                rosetta_capture::visit_ledger::<ReplayProducerError, _>(ledger_dir, |entry| {
                    let send_started = Instant::now();
                    let sent = sender
                        .send(entry)
                        .map_err(|_| ReplayProducerError::ConsumerGone);
                    send_nanos = send_nanos.saturating_add(
                        u64::try_from(send_started.elapsed().as_nanos()).unwrap_or(u64::MAX),
                    );
                    sent
                });
            let elapsed = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
            (result, elapsed.saturating_sub(send_nanos))
        });

        let mut visitor_error = None;
        for entry in receiver {
            if let Err(error) = visitor(entry) {
                visitor_error = Some(error);
                break;
            }
        }
        let joined = producer.join().map_err(|_| {
            PipelineError::Config("ledger prefetch thread panicked during replay".to_string())
        })?;
        instrument.add(Metric::LedgerReadNanos, joined.1);
        instrument.add(Metric::LedgerReplayReadNanos, joined.1);
        if let Some(error) = visitor_error {
            return Err(error);
        }
        match joined.0 {
            Ok(visit) => Ok(visit),
            Err(ReplayProducerError::Capture(error)) => Err(PipelineError::Capture(error)),
            Err(ReplayProducerError::ConsumerGone) => Err(PipelineError::Config(
                "ledger replay consumer stopped unexpectedly".to_string(),
            )),
        }
    })
}

#[derive(Debug, Default)]
pub struct RebuildReport {
    pub segments: usize,
    pub torn_segments: usize,
    pub meta_batches: u64,
    pub files: u64,
    pub raw_records: u64,
    /// Records from superseded generations, skipped by design.
    pub stale_records_skipped: u64,
    /// At-least-once ledger writes with an already-seen source offset.
    pub duplicate_records_skipped: u64,
    /// Records whose generation is already active in the projection — a
    /// re-run of rebuild over the same store skips them wholesale.
    pub records_already_projected: u64,
    /// Sessions synthesized because no journaled meta batch named them.
    pub sessions_stubbed: u64,
    pub parsed: u64,
    pub unknown: u64,
    pub hard_errors: u64,
    pub stats: ApplyStats,
}

struct FileReplay {
    ctx: NormalizeCtx,
    source: SourceDef,
    cursor_id: i64,
    cursor_blob: String,
    generation: u64,
    next_sequence: u64,
    ops: Vec<IngestOp>,
    last_offset: Option<u64>,
    generation_clean: bool,
    activated: bool,
}

struct PreparedFile {
    source: SourceDef,
    generation: u64,
    ops: Vec<IngestOp>,
    checkpoint: String,
    generation_clean: bool,
    activate: bool,
}

#[derive(Debug)]
struct FinalMessageOrder {
    timeline_id: String,
    session_id: String,
    path: String,
    position: i64,
    source_sequence: u64,
}

/// Assign the complete runtime's presentation order while every canonical
/// message is still in RAM. Repeated timeline identities take their final
/// source position, matching the writer's last-upsert-wins payload semantics;
/// every occurrence receives that one ordinal so intermediate upserts cannot
/// collide. Tool openers retain their source position even though their
/// lifecycle is folded and written later.
fn assign_precomputed_ordinals(files: &mut [PreparedFile]) -> Result<(), PipelineError> {
    let mut final_positions = BTreeMap::<String, FinalMessageOrder>::new();
    for file in files.iter() {
        for (index, op) in file.ops.iter().enumerate() {
            let IngestOp::Message(row) = op else {
                continue;
            };
            let source_sequence = u64::try_from(index).map_err(|_| {
                PipelineError::Config("source operation sequence exceeds u64".to_string())
            })?;
            let position = row
                .provenance
                .src_offset
                .unwrap_or_else(|| i64::try_from(source_sequence).unwrap_or(i64::MAX));
            if let Some(previous) = final_positions.get(&row.timeline_id)
                && previous.session_id != row.session_id
            {
                return Err(PipelineError::Config(format!(
                    "timeline {} moved between sessions {} and {} during cold planning",
                    row.timeline_id, previous.session_id, row.session_id
                )));
            }
            final_positions.insert(
                row.timeline_id.clone(),
                FinalMessageOrder {
                    timeline_id: row.timeline_id.clone(),
                    session_id: row.session_id.clone(),
                    path: file.source.relative_path.clone(),
                    position,
                    source_sequence,
                },
            );
        }
    }

    let mut ordered = final_positions.values().collect::<Vec<_>>();
    ordered.sort_by(|left, right| {
        (
            &left.session_id,
            &left.path,
            left.position,
            left.source_sequence,
            &left.timeline_id,
        )
            .cmp(&(
                &right.session_id,
                &right.path,
                right.position,
                right.source_sequence,
                &right.timeline_id,
            ))
    });
    let mut ordinals = BTreeMap::<String, i64>::new();
    let mut current_session = None::<&str>;
    let mut next_ordinal = 0_i64;
    for message in ordered {
        if current_session != Some(message.session_id.as_str()) {
            current_session = Some(message.session_id.as_str());
            next_ordinal = 0;
        }
        ordinals.insert(message.timeline_id.clone(), next_ordinal);
        next_ordinal = next_ordinal.checked_add(1).ok_or_else(|| {
            PipelineError::Config(format!(
                "session {} has more messages than i64 ordinals",
                message.session_id
            ))
        })?;
    }

    for file in files {
        for op in &mut file.ops {
            let IngestOp::Message(row) = op else {
                continue;
            };
            row.ordinal = *ordinals.get(&row.timeline_id).ok_or_else(|| {
                PipelineError::Config(format!(
                    "cold ordinal plan lost timeline {}",
                    row.timeline_id
                ))
            })?;
        }
    }
    Ok(())
}

/// A complete runtime normalized without touching SQLite. It is intentionally
/// process-local and disposable: the raw ledger remains the recovery source.
/// Whole-projection cold builds can prepare independent runtimes in parallel,
/// then feed these closed source batches to the one canonical writer.
pub struct PreparedRuntime {
    report: RebuildReport,
    meta_ops: Vec<Vec<IngestOp>>,
    files: Vec<PreparedFile>,
    session_health: BTreeMap<String, bool>,
    /// Fresh serve already exposed the identical metadata batches through
    /// its canonical RAM view. Keep them for session identity/stub planning,
    /// but do not write them a second time with the closed source payloads.
    metadata_preapplied: bool,
}

impl PreparedRuntime {
    /// Read preparation health before admitting this runtime to the canonical
    /// writer. The payload itself stays opaque outside the pipeline crate.
    pub fn report(&self) -> &RebuildReport {
        &self.report
    }

    pub fn mark_metadata_preapplied(&mut self) {
        self.metadata_preapplied = true;
    }
}

pub fn rebuild_runtime(
    adapter: &dyn AgentAdapter,
    runtime: &RuntimeRow,
    ledger_dir: &Path,
    store: &mut Store,
    instrument: &Instrument,
) -> Result<RebuildReport, PipelineError> {
    rebuild_runtime_with_options(
        adapter,
        runtime,
        ledger_dir,
        store,
        instrument,
        RebuildOptions::default(),
    )
}

pub fn rebuild_runtime_with_options(
    adapter: &dyn AgentAdapter,
    runtime: &RuntimeRow,
    ledger_dir: &Path,
    store: &mut Store,
    instrument: &Instrument,
    options: RebuildOptions,
) -> Result<RebuildReport, PipelineError> {
    if options.chunk_ops == 0 {
        return Err(PipelineError::Config(
            "rebuild chunk size must be greater than zero".to_string(),
        ));
    }
    let mut report = RebuildReport::default();

    let cache_segments = options
        .election_cache
        .as_deref()
        .map(|_| segment_stamps(ledger_dir))
        .transpose()?;
    let cached = options.election_cache.as_deref().and_then(|path| {
        load_election_cache(
            path,
            adapter.normalizer_version(),
            cache_segments.as_deref().unwrap_or_default(),
        )
    });
    let election = if let Some(cached) = cached {
        cached
    } else {
        // Only a raw-complete marker can elect a replacement generation.
        // Raw-only paths are legacy/unsafe rather than guesses: fail closed
        // instead of silently promoting a crash-truncated generation.
        let mut surviving: BTreeMap<String, (u64, SourceDef)> = BTreeMap::new();
        let mut latest_seals: BTreeMap<String, (u64, SourceDef, bool)> = BTreeMap::new();
        let mut raw_paths: HashSet<String> = HashSet::new();
        let mut sealed_paths: HashSet<String> = HashSet::new();
        let visit = visit_election::<PipelineError, _>(ledger_dir, instrument, |entry| {
            match entry {
                LedgerEntry::Raw { source, .. } => {
                    raw_paths.insert(source.relative_path);
                }
                LedgerEntry::GenerationComplete {
                    source,
                    generation,
                    normalizer_version,
                    normalized_clean,
                } => {
                    sealed_paths.insert(source.relative_path.clone());
                    let known_unclean =
                        normalizer_version == adapter.normalizer_version() && !normalized_clean;
                    let latest = latest_seals
                        .entry(source.relative_path.clone())
                        .or_insert_with(|| (generation, source.clone(), known_unclean));
                    if generation >= latest.0 {
                        *latest = (generation, source.clone(), known_unclean);
                    }
                    if known_unclean {
                        return Ok(());
                    }
                    let slot = surviving
                        .entry(source.relative_path.clone())
                        .or_insert_with(|| (generation, source.clone()));
                    if generation >= slot.0 {
                        *slot = (generation, source);
                    }
                }
                LedgerEntry::Meta { .. } => {}
            }
            Ok(())
        })?;
        let mut only_dirty = HashSet::new();
        for (path, (generation, source, _)) in &latest_seals {
            if !surviving.contains_key(path) {
                only_dirty.insert(path.clone());
                surviving.insert(path.clone(), (*generation, source.clone()));
            }
        }
        let mut unsealed: Vec<_> = raw_paths.difference(&sealed_paths).cloned().collect();
        unsealed.sort();
        if !unsealed.is_empty() {
            return Err(PipelineError::Config(format!(
                "ledger has raw records but no complete-generation marker for: {}",
                unsealed.join(", ")
            )));
        }
        let state = ElectionState {
            visit,
            surviving,
            latest_seals,
            only_dirty,
        };
        if let (Some(path), Some(segments)) =
            (options.election_cache.as_deref(), cache_segments.clone())
        {
            save_election_cache(path, adapter.normalizer_version(), segments, &state)?;
        }
        state
    };
    let ElectionState {
        visit: first,
        surviving,
        latest_seals,
        only_dirty,
    } = election;
    report.segments = first.segments;
    report.torn_segments = first.torn_segments;
    if options.election_only {
        return Ok(report);
    }

    // The runtime row first: every later FK hangs off it.
    let stats = instrument.apply(
        store,
        &Batch {
            ops: vec![IngestOp::Runtime(runtime.clone())],
            checkpoints: vec![],
            source: None,
        },
    )?;
    merge_stats(&mut report.stats, &stats);

    let mut sessions_seen: HashSet<String> = HashSet::new();
    let mut already_projected: HashSet<String> = HashSet::new();
    let mut files: BTreeMap<String, FileReplay> = BTreeMap::new();

    let mut replay_entry = |entry| {
        match entry {
            LedgerEntry::Meta {
                schema_version,
                payload,
            } => {
                if !matches!(schema_version, 0 | crate::LEDGER_META_SCHEMA_VERSION) {
                    return Err(PipelineError::Config(format!(
                        "unsupported ledger meta schema version {schema_version}"
                    )));
                }
                let ops: Vec<IngestOp> = serde_json::from_str(&payload).map_err(|error| {
                    PipelineError::Config(format!("undecodable meta batch in ledger: {error}"))
                })?;
                for op in &ops {
                    if let IngestOp::SessionMeta(row) = op {
                        sessions_seen.insert(row.id.clone());
                    }
                }
                report.meta_batches += 1;
                let stats = instrument.apply(
                    store,
                    &Batch {
                        ops,
                        checkpoints: vec![],
                        source: None,
                    },
                )?;
                merge_stats(&mut report.stats, &stats);
            }
            LedgerEntry::Raw {
                source,
                generation,
                start_offset,
                payload,
            } => {
                let Some((surviving_generation, _)) = surviving.get(&source.relative_path) else {
                    return Ok(());
                };
                if generation != *surviving_generation {
                    report.stale_records_skipped += 1;
                    return Ok(());
                }
                if already_projected.contains(&source.relative_path) {
                    report.records_already_projected += 1;
                    return Ok(());
                }
                if !files.contains_key(&source.relative_path) {
                    // Idempotent re-run: a cursor whose active generation
                    // already covers the survivor means these rows are in the
                    // projection — skip the file wholesale. An ABANDONED
                    // staged generation (interrupted rebuild) instead remaps
                    // the replay above it, exactly like live recovery does.
                    let existing =
                        queries::cursor_state(store.conn(), &runtime.id, &source.relative_path)?;
                    let replay_generation = match &existing {
                        Some(cursor) if cursor.active_generation >= *surviving_generation => {
                            already_projected.insert(source.relative_path.clone());
                            report.records_already_projected += 1;
                            return Ok(());
                        }
                        Some(cursor) => (*surviving_generation).max(
                            cursor.highest_generation.checked_add(1).ok_or_else(|| {
                                PipelineError::Config(
                                    "source generation overflowed during rebuild".to_string(),
                                )
                            })?,
                        ),
                        None => *surviving_generation,
                    };
                    let replay = open_file(
                        adapter,
                        runtime,
                        &source,
                        replay_generation,
                        store,
                        &mut sessions_seen,
                        &mut report,
                        instrument,
                    )?;
                    files.insert(source.relative_path.clone(), replay);
                }
                let file = files
                    .get_mut(&source.relative_path)
                    .expect("inserted above");

                if file
                    .last_offset
                    .is_some_and(|previous| start_offset <= previous)
                {
                    report.duplicate_records_skipped += 1;
                    return Ok(());
                }
                file.last_offset = Some(start_offset);
                file.ctx.agent_version = source.agent_version.clone();

                report.raw_records += 1;
                instrument.add(Metric::RecordsFramed, 1);
                file.ctx.src_offset = Some(start_offset as i64);
                let outcome = instrument.time(Metric::NormalizeNanos, || {
                    adapter.ingest_line(&mut file.ctx, &file.source.stream_id, &payload)
                });
                match &outcome.status {
                    LineStatus::Parsed => report.parsed += 1,
                    LineStatus::UnknownType => report.unknown += 1,
                    LineStatus::HardError { .. } => {
                        file.generation_clean = false;
                        report.hard_errors += 1;
                    }
                }
                file.ops.extend(outcome.ops);
                if file.ops.len() >= options.chunk_ops {
                    let mode = if file.activated {
                        SourceMode::Append
                    } else {
                        SourceMode::Stage
                    };
                    flush_chunk(
                        adapter,
                        runtime,
                        file,
                        mode,
                        false,
                        store,
                        &mut report,
                        instrument,
                    )?;
                }
            }
            LedgerEntry::GenerationComplete {
                source, generation, ..
            } => {
                let Some((surviving_generation, _)) = surviving.get(&source.relative_path) else {
                    return Ok(());
                };
                if generation != *surviving_generation
                    || already_projected.contains(&source.relative_path)
                {
                    return Ok(());
                }
                if !files.contains_key(&source.relative_path) {
                    let existing =
                        queries::cursor_state(store.conn(), &runtime.id, &source.relative_path)?;
                    let replay_generation = match &existing {
                        Some(cursor) if cursor.active_generation >= *surviving_generation => {
                            already_projected.insert(source.relative_path.clone());
                            return Ok(());
                        }
                        Some(cursor) => (*surviving_generation).max(
                            cursor.highest_generation.checked_add(1).ok_or_else(|| {
                                PipelineError::Config(
                                    "source generation overflowed during rebuild".to_string(),
                                )
                            })?,
                        ),
                        None => *surviving_generation,
                    };
                    let replay = open_file(
                        adapter,
                        runtime,
                        &source,
                        replay_generation,
                        store,
                        &mut sessions_seen,
                        &mut report,
                        instrument,
                    )?;
                    files.insert(source.relative_path.clone(), replay);
                }
                let file = files
                    .get_mut(&source.relative_path)
                    .expect("opened for complete marker");
                if !file.activated {
                    // Clean at the seal activates — dirt found only in the
                    // post-seal appends is the live-growth shape, and those
                    // appends keep applying below. Dirt INSIDE the sealed
                    // range means the seal lied; trust the replay and abandon
                    // it, unless this path never had clean content to protect
                    // (the dirty-first-generation case, which the runtime
                    // activated too). Cleanliness still owns readiness.
                    let mode =
                        if file.generation_clean || only_dirty.contains(&source.relative_path) {
                            SourceMode::Activate
                        } else {
                            SourceMode::Abandon
                        };
                    flush_chunk(
                        adapter,
                        runtime,
                        file,
                        mode,
                        true,
                        store,
                        &mut report,
                        instrument,
                    )?;
                    file.activated = mode == SourceMode::Activate;
                }
            }
        }
        Ok(())
    };
    let second = if options.prefetch_ledger {
        visit_replay_prefetched(ledger_dir, instrument, &mut replay_entry)?
    } else {
        visit_replay::<PipelineError, _>(ledger_dir, instrument, &mut replay_entry)?
    };
    if first != second {
        return Err(PipelineError::Config(
            "ledger changed while rebuild was reading it; stop ingestion and retry".to_string(),
        ));
    }

    // Append records may follow the generation's first completion marker.
    // Publish them exactly as live ingest did, or abandon remaining staged
    // rows if the sealed base did not normalize cleanly.
    for file in files.values_mut() {
        let mode = if file.activated {
            SourceMode::Append
        } else {
            SourceMode::Abandon
        };
        flush_chunk(
            adapter,
            runtime,
            file,
            mode,
            true,
            store,
            &mut report,
            instrument,
        )?;
    }

    // All surviving sources for this runtime are now sealed. Reconcile their
    // cross-file tool lifecycle once in RAM before declaring sessions ready.
    if store.is_bulk_rebuild() && options.flush_reconciliation {
        let reconciled = instrument.flush_bulk_reconciliation(store)?;
        merge_stats(&mut report.stats, &reconciled);
    }

    // Rebuild has the same readiness contract as source backfill: only
    // sessions whose every sealed source is currently clean are Indexed.
    // Meta-only sessions are absent from this map and retain Pending.
    let mut session_health: BTreeMap<String, bool> = BTreeMap::new();
    for (path, (_, source, known_unclean)) in latest_seals {
        let clean = if known_unclean {
            false
        } else if already_projected.contains(&path) {
            true
        } else {
            files
                .get(&path)
                .is_some_and(|file| file.activated && file.generation_clean)
        };
        session_health
            .entry(source.session_id)
            .and_modify(|healthy| *healthy &= clean)
            .or_insert(clean);
    }
    for (session_id, clean) in session_health {
        let next = if clean {
            StreamState::Indexed
        } else {
            StreamState::Streaming
        };
        store.set_session_stream_state(&session_id, next)?;
    }

    Ok(report)
}

/// Normalize one runtime into closed, source-scoped RAM batches. This path
/// requires a writer-maintained election cache: falling back to another full
/// ledger pass inside every worker would erase the architecture being tested.
pub fn prepare_runtime(
    adapter: &dyn AgentAdapter,
    runtime: &RuntimeRow,
    ledger_dir: &Path,
    instrument: &Instrument,
    options: RebuildOptions,
) -> Result<PreparedRuntime, PipelineError> {
    let cache_path = options.election_cache.as_deref().ok_or_else(|| {
        PipelineError::Config("parallel preparation requires an election cache".to_string())
    })?;
    let stamps = segment_stamps(ledger_dir)?;
    let election = load_election_cache(cache_path, adapter.normalizer_version(), &stamps)
        .ok_or_else(|| {
            PipelineError::Config(format!(
                "missing or stale election cache {}",
                cache_path.display()
            ))
        })?;
    if election.visit.torn_segments != 0 {
        return Err(PipelineError::Config(format!(
            "{} cached torn ledger segments",
            election.visit.torn_segments
        )));
    }

    struct PreparingFile {
        ctx: NormalizeCtx,
        source: SourceDef,
        generation: u64,
        ops: Vec<IngestOp>,
        last_offset: Option<u64>,
        generation_clean: bool,
        sealed: bool,
    }

    let mut report = RebuildReport {
        segments: election.visit.segments,
        torn_segments: election.visit.torn_segments,
        ..RebuildReport::default()
    };
    let mut meta_ops = Vec::new();
    let mut files: BTreeMap<String, PreparingFile> = BTreeMap::new();
    let surviving = &election.surviving;
    let replay = |entry: LedgerEntry,
                  files: &mut BTreeMap<String, PreparingFile>,
                  report: &mut RebuildReport,
                  meta_ops: &mut Vec<Vec<IngestOp>>|
     -> Result<(), PipelineError> {
        match entry {
            LedgerEntry::Meta {
                schema_version,
                payload,
            } => {
                if !matches!(schema_version, 0 | crate::LEDGER_META_SCHEMA_VERSION) {
                    return Err(PipelineError::Config(format!(
                        "unsupported ledger meta schema version {schema_version}"
                    )));
                }
                let ops = serde_json::from_str(&payload).map_err(|error| {
                    PipelineError::Config(format!("undecodable meta batch in ledger: {error}"))
                })?;
                meta_ops.push(ops);
                report.meta_batches += 1;
            }
            LedgerEntry::Raw {
                source,
                generation,
                start_offset,
                payload,
            } => {
                let Some((surviving_generation, _)) = surviving.get(&source.relative_path) else {
                    return Ok(());
                };
                if generation != *surviving_generation {
                    report.stale_records_skipped += 1;
                    return Ok(());
                }
                if !files.contains_key(&source.relative_path) {
                    let mut ctx = NormalizeCtx::new(&runtime.id, &source.session_id);
                    ctx.native_session_id = Some(source.native_session_id.clone());
                    ctx.agent_version = source.agent_version.clone();
                    ctx.state = adapter.restore_state(None).map_err(PipelineError::Config)?;
                    ctx.source_identity = Some(source.relative_path.clone());
                    ctx.source_generation = generation;
                    files.insert(
                        source.relative_path.clone(),
                        PreparingFile {
                            ctx,
                            source: source.clone(),
                            generation,
                            ops: Vec::new(),
                            last_offset: None,
                            generation_clean: true,
                            sealed: false,
                        },
                    );
                    report.files += 1;
                }
                let file = files
                    .get_mut(&source.relative_path)
                    .expect("prepared source inserted above");
                if file
                    .last_offset
                    .is_some_and(|previous| start_offset <= previous)
                {
                    report.duplicate_records_skipped += 1;
                    return Ok(());
                }
                file.last_offset = Some(start_offset);
                file.ctx.agent_version = source.agent_version;
                file.ctx.src_offset = Some(start_offset as i64);
                report.raw_records += 1;
                instrument.add(Metric::RecordsFramed, 1);
                let outcome = instrument.time(Metric::NormalizeNanos, || {
                    adapter.ingest_line(&mut file.ctx, &file.source.stream_id, &payload)
                });
                match outcome.status {
                    LineStatus::Parsed => report.parsed += 1,
                    LineStatus::UnknownType => report.unknown += 1,
                    LineStatus::HardError { .. } => {
                        file.generation_clean = false;
                        report.hard_errors += 1;
                    }
                }
                file.ops.extend(outcome.ops);
            }
            LedgerEntry::GenerationComplete {
                source, generation, ..
            } => {
                let Some((surviving_generation, _)) = surviving.get(&source.relative_path) else {
                    return Ok(());
                };
                if generation != *surviving_generation {
                    return Ok(());
                }
                if !files.contains_key(&source.relative_path) {
                    let mut ctx = NormalizeCtx::new(&runtime.id, &source.session_id);
                    ctx.native_session_id = Some(source.native_session_id.clone());
                    ctx.agent_version = source.agent_version.clone();
                    ctx.state = adapter.restore_state(None).map_err(PipelineError::Config)?;
                    ctx.source_identity = Some(source.relative_path.clone());
                    ctx.source_generation = generation;
                    files.insert(
                        source.relative_path.clone(),
                        PreparingFile {
                            ctx,
                            source: source.clone(),
                            generation,
                            ops: Vec::new(),
                            last_offset: None,
                            generation_clean: true,
                            sealed: true,
                        },
                    );
                    report.files += 1;
                } else {
                    files
                        .get_mut(&source.relative_path)
                        .expect("prepared source exists")
                        .sealed = true;
                }
            }
        }
        Ok(())
    };
    let second = if options.prefetch_ledger {
        visit_replay_prefetched(ledger_dir, instrument, |entry| {
            replay(entry, &mut files, &mut report, &mut meta_ops)
        })?
    } else {
        visit_replay::<PipelineError, _>(ledger_dir, instrument, |entry| {
            replay(entry, &mut files, &mut report, &mut meta_ops)
        })?
    };
    if election.visit != second {
        return Err(PipelineError::Config(
            "ledger changed after its generation index was written; retry capture".to_string(),
        ));
    }

    let mut prepared_files = Vec::with_capacity(files.len());
    for (_, file) in files {
        if !file.sealed {
            return Err(PipelineError::Config(format!(
                "elected source {} has no complete-generation marker",
                file.source.relative_path
            )));
        }
        let reading = *file.ctx.cumulative.last_reading();
        let checkpoint = PipeCheckpoint {
            v: PIPE_CHECKPOINT_VERSION,
            session_id: file.source.session_id.clone(),
            stream_id: file.source.stream_id.clone(),
            normalizer_version: adapter.normalizer_version(),
            source_generation: file.generation,
            next_source_sequence: file.ops.len() as u64,
            generation_clean: file.generation_clean,
            next_ordinal: file.ctx.next_ordinal,
            tail: None,
            cumulative: (!reading.is_empty()).then_some(reading),
            adapter_state: adapter
                .checkpoint_state(file.ctx.state.as_deref())
                .map_err(PipelineError::Config)?,
            snapshot_stamp: None,
            snapshot_revision: None,
        }
        .encode();
        let activate =
            file.generation_clean || election.only_dirty.contains(&file.source.relative_path);
        prepared_files.push(PreparedFile {
            source: file.source,
            generation: file.generation,
            ops: file.ops,
            checkpoint,
            generation_clean: file.generation_clean,
            activate,
        });
    }
    assign_precomputed_ordinals(&mut prepared_files)?;

    let mut session_health = BTreeMap::new();
    let prepared_by_path = prepared_files
        .iter()
        .map(|file| (file.source.relative_path.as_str(), file))
        .collect::<BTreeMap<_, _>>();
    for (path, (generation, source, known_unclean)) in election.latest_seals {
        let clean = !known_unclean
            && prepared_by_path.get(path.as_str()).is_some_and(|file| {
                file.generation == generation && file.activate && file.generation_clean
            });
        session_health
            .entry(source.session_id)
            .and_modify(|healthy| *healthy &= clean)
            .or_insert(clean);
    }

    Ok(PreparedRuntime {
        report,
        meta_ops,
        files: prepared_files,
        session_health,
        metadata_preapplied: false,
    })
}

/// Turn a just-captured first ingest into the same closed RAM representation
/// used by ledger replay. The capture drivers still own framing, snapshot
/// consistency, generation changes, and adapter checkpoints; their planning
/// store persists only that control plane in memory and retains the canonical
/// payload batches here instead of writing them to a throwaway projection.
///
/// This is deliberately a cold-only bridge. Every source must have reached a
/// terminal generation in this pass, and the caller must refuse publication
/// when the accompanying [`StreamReport`] reports an incomplete source.
pub fn prepare_captured_runtime(
    adapter: &dyn AgentAdapter,
    runtime: &RuntimeRow,
    files: &[SessionFile],
    planner: &mut Store,
    stream: &StreamReport,
) -> Result<PreparedRuntime, PipelineError> {
    struct CapturedSource {
        source: SourceDef,
        generation: u64,
        active_generation: u64,
        checkpoint: String,
        ops: Vec<IngestOp>,
        last_mode: Option<SourceMode>,
    }

    let mut captured = BTreeMap::<i64, CapturedSource>::new();
    for file in files {
        let cursor = queries::cursor_state(planner.conn(), &runtime.id, &file.relative_path)?
            .ok_or_else(|| {
                PipelineError::Config(format!(
                    "cold planner lost cursor for {}",
                    file.relative_path
                ))
            })?;
        if cursor.highest_generation == 0 {
            return Err(PipelineError::Config(format!(
                "cold source {} never started a generation",
                file.relative_path
            )));
        }
        let previous = captured.insert(
            cursor.id,
            CapturedSource {
                source: SourceDef {
                    relative_path: file.relative_path.clone(),
                    stream_id: file.stream_id.clone(),
                    session_id: file.session_id.clone(),
                    native_session_id: file.native_session_id.clone(),
                    agent_version: file.agent_version.clone(),
                },
                generation: cursor.highest_generation,
                active_generation: cursor.active_generation,
                checkpoint: cursor.checkpoint,
                ops: Vec::new(),
                last_mode: None,
            },
        );
        if previous.is_some() {
            return Err(PipelineError::Config(format!(
                "cold planner assigned cursor {} to more than one source",
                cursor.id
            )));
        }
    }

    let mut meta_ops = Vec::new();
    let planned = planner.take_planned_batches()?;
    for batch in planned {
        let Some(source_write) = batch.source else {
            // Cursor-install and audit-watermark batches contain no canonical
            // operations. Metadata batches are the only source-free payload
            // that belongs in the final projection.
            if !batch.ops.is_empty() {
                meta_ops.push(batch.ops);
            }
            continue;
        };
        let file = captured.get_mut(&source_write.cursor_id).ok_or_else(|| {
            PipelineError::Config(format!(
                "cold planner retained unknown cursor {}",
                source_write.cursor_id
            ))
        })?;
        if source_write.session_id != file.source.session_id {
            return Err(PipelineError::Config(format!(
                "cold source {} moved from session {} to {}",
                file.source.relative_path, file.source.session_id, source_write.session_id
            )));
        }
        if source_write.generation < file.generation {
            // A rewrite observed during capture superseded this incomplete
            // generation. The planning control store swept it; mirror that
            // decision before the one real canonical write.
            continue;
        }
        if source_write.generation > file.generation {
            return Err(PipelineError::Config(format!(
                "cold source {} retained generation {} above planner watermark {}",
                file.source.relative_path, source_write.generation, file.generation
            )));
        }
        let expected = u64::try_from(file.ops.len()).map_err(|_| {
            PipelineError::Config(format!(
                "cold source {} operation count exceeds u64",
                file.source.relative_path
            ))
        })?;
        if source_write.first_sequence != expected {
            return Err(PipelineError::Config(format!(
                "cold source {} has non-contiguous operation sequence {} after {}",
                file.source.relative_path, source_write.first_sequence, expected
            )));
        }
        file.ops.extend(batch.ops);
        file.last_mode = Some(source_write.mode);
    }

    let mut prepared_files = Vec::with_capacity(captured.len());
    let mut session_health = BTreeMap::new();
    for (_, file) in captured {
        let terminal = matches!(
            file.last_mode,
            Some(SourceMode::Activate | SourceMode::Abandon)
        ) || (file.active_generation == file.generation
            && matches!(file.last_mode, Some(SourceMode::Append)));
        if !terminal {
            return Err(PipelineError::Config(format!(
                "cold source {} stopped without a terminal generation",
                file.source.relative_path
            )));
        }
        let checkpoint = PipeCheckpoint::decode(&file.checkpoint).ok_or_else(|| {
            PipelineError::Config(format!(
                "cold source {} ended with an undecodable checkpoint",
                file.source.relative_path
            ))
        })?;
        if checkpoint.source_generation != file.generation {
            return Err(PipelineError::Config(format!(
                "cold source {} checkpoint generation {} differs from control watermark {}",
                file.source.relative_path, checkpoint.source_generation, file.generation
            )));
        }
        if checkpoint.normalizer_version != adapter.normalizer_version() {
            return Err(PipelineError::Config(format!(
                "cold source {} checkpoint normalizer {} differs from adapter {}",
                file.source.relative_path,
                checkpoint.normalizer_version,
                adapter.normalizer_version()
            )));
        }
        let activate = file.active_generation == file.generation;
        let clean = activate && checkpoint.generation_clean;
        session_health
            .entry(file.source.session_id.clone())
            .and_modify(|healthy| *healthy &= clean)
            .or_insert(clean);
        prepared_files.push(PreparedFile {
            source: file.source,
            generation: file.generation,
            ops: file.ops,
            checkpoint: file.checkpoint,
            generation_clean: checkpoint.generation_clean,
            activate,
        });
    }
    // Match ledger preparation's deterministic path order. Besides keeping
    // surrogate row ids stable, this makes the precomputed (session, ordinal)
    // key arrive close to monotonic instead of turning a newest-first capture
    // roster into random B-tree writes.
    prepared_files
        .sort_by(|left, right| left.source.relative_path.cmp(&right.source.relative_path));
    assign_precomputed_ordinals(&mut prepared_files)?;

    Ok(PreparedRuntime {
        report: RebuildReport {
            meta_batches: u64::try_from(meta_ops.len()).unwrap_or(u64::MAX),
            files: u64::try_from(prepared_files.len()).unwrap_or(u64::MAX),
            raw_records: stream.records,
            parsed: stream.parsed,
            unknown: stream.unknown,
            hard_errors: stream.hard_errors,
            ..RebuildReport::default()
        },
        meta_ops,
        files: prepared_files,
        session_health,
        metadata_preapplied: false,
    })
}

fn stamp_source_cursor(ops: &mut [IngestOp], cursor_id: i64) {
    for op in ops {
        match op {
            IngestOp::Message(row) => row.provenance.src_cursor = Some(cursor_id),
            IngestOp::Drift(fact) if fact.sample_offset.is_some() => {
                fact.sample_cursor = Some(cursor_id);
            }
            _ => {}
        }
    }
}

/// Feed one prepared runtime to the sole canonical writer. Every source is a
/// closed batch, so SQLite never sees parser scheduling or partial lifecycle
/// state.
pub fn apply_prepared_runtime(
    adapter: &dyn AgentAdapter,
    runtime: &RuntimeRow,
    mut prepared: PreparedRuntime,
    store: &mut Store,
    instrument: &Instrument,
) -> Result<RebuildReport, PipelineError> {
    let mut setup_ops = vec![IngestOp::Runtime(runtime.clone())];
    let mut sessions_seen = HashSet::new();
    for ops in prepared.meta_ops {
        for op in &ops {
            if let IngestOp::SessionMeta(row) = op {
                sessions_seen.insert(row.id.clone());
            }
        }
        if !prepared.metadata_preapplied {
            setup_ops.extend(ops);
        }
    }

    for file in &prepared.files {
        if !sessions_seen.contains(&file.source.session_id) {
            sessions_seen.insert(file.source.session_id.clone());
            prepared.report.sessions_stubbed += 1;
            setup_ops.push(IngestOp::SessionMeta(SessionRow {
                id: file.source.session_id.clone(),
                native_session_id: file.source.native_session_id.clone(),
                runtime_id: runtime.id.clone(),
                project_id: None,
                cwd: None,
                parent_session_id: None,
                root_session_id: file.source.session_id.clone(),
                role: SessionRole::Main,
                title: None,
                summary: None,
                started_at: 0,
                updated_at: 0,
                stream_state: StreamState::Pending,
            }));
        }
    }

    let mut initial_by_path = BTreeMap::new();
    let mut setup_checkpoints = Vec::with_capacity(prepared.files.len());
    for file in &prepared.files {
        let initial = PipeCheckpoint::fresh(
            &file.source.session_id,
            &file.source.stream_id,
            adapter.normalizer_version(),
        )
        .encode();
        initial_by_path.insert(file.source.relative_path.clone(), initial.clone());
        setup_checkpoints.push(CheckpointUpdate {
            runtime_id: runtime.id.clone(),
            stream: file.source.stream_id.clone(),
            path: file.source.relative_path.clone(),
            session_id: file.source.session_id.clone(),
            expected: None,
            next: initial,
            updated_at: None,
        });
    }
    let stats = instrument.apply(
        store,
        &Batch {
            ops: setup_ops,
            checkpoints: setup_checkpoints,
            source: None,
        },
    )?;
    merge_stats(&mut prepared.report.stats, &stats);

    let mut source_batches = Vec::with_capacity(prepared.files.len());
    for mut file in prepared.files {
        let initial = initial_by_path
            .remove(&file.source.relative_path)
            .ok_or_else(|| {
                PipelineError::Config(format!(
                    "prepared cursor setup lost {}",
                    file.source.relative_path
                ))
            })?;
        let cursor = queries::cursor_state(store.conn(), &runtime.id, &file.source.relative_path)?
            .ok_or_else(|| PipelineError::Config("prepared cursor vanished".to_string()))?;
        stamp_source_cursor(&mut file.ops, cursor.id);
        source_batches.push(Batch {
            ops: file.ops,
            checkpoints: vec![CheckpointUpdate {
                runtime_id: runtime.id.clone(),
                stream: file.source.stream_id,
                path: file.source.relative_path,
                session_id: file.source.session_id.clone(),
                expected: Some(initial),
                next: file.checkpoint,
                updated_at: None,
            }],
            source: Some(SourceWrite {
                cursor_id: cursor.id,
                session_id: file.source.session_id,
                generation: file.generation,
                first_sequence: 0,
                mode: if file.activate {
                    SourceMode::Activate
                } else {
                    SourceMode::Abandon
                },
            }),
        });
    }
    let source_count = u64::try_from(source_batches.len()).unwrap_or(u64::MAX);
    let stats = instrument.apply_closed_bulk_sources(store, &source_batches)?;
    merge_stats(&mut prepared.report.stats, &stats);
    instrument.add(Metric::BatchesApplied, source_count);

    let stream_states = prepared
        .session_health
        .into_iter()
        .map(|(session_id, clean)| {
            (
                session_id,
                if clean {
                    StreamState::Indexed
                } else {
                    StreamState::Streaming
                },
            )
        })
        .collect::<Vec<_>>();

    // Fresh serve has already exposed these sessions through its canonical
    // RAM metadata view. Source application above may commit in more than
    // one physical transaction, so keep every session Pending until the
    // runtime's cross-file relations are fully folded. The server treats
    // Pending as an admission barrier and the state flip below is the first
    // point at which its timeline can become public.
    if prepared.metadata_preapplied {
        let reconciled = instrument.flush_bulk_reconciliation(store)?;
        merge_stats(&mut prepared.report.stats, &reconciled);
    }
    store.set_session_stream_states(&stream_states)?;
    Ok(prepared.report)
}

#[allow(clippy::too_many_arguments)]
fn open_file(
    adapter: &dyn AgentAdapter,
    runtime: &RuntimeRow,
    source: &SourceDef,
    generation: u64,
    store: &mut Store,
    sessions_seen: &mut HashSet<String>,
    report: &mut RebuildReport,
    instrument: &Instrument,
) -> Result<FileReplay, PipelineError> {
    // A session no journaled meta batch named (ledger enabled mid-history)
    // still needs its row for FKs — stub it honestly; a later meta batch or
    // re-skim refines the same deterministic id.
    if !sessions_seen.contains(&source.session_id) {
        report.sessions_stubbed += 1;
        sessions_seen.insert(source.session_id.clone());
        let stats = instrument.apply(
            store,
            &Batch {
                ops: vec![IngestOp::SessionMeta(SessionRow {
                    id: source.session_id.clone(),
                    native_session_id: source.native_session_id.clone(),
                    runtime_id: runtime.id.clone(),
                    project_id: None,
                    cwd: None,
                    parent_session_id: None,
                    root_session_id: source.session_id.clone(),
                    role: SessionRole::Main,
                    title: None,
                    summary: None,
                    started_at: 0,
                    updated_at: 0,
                    stream_state: StreamState::Pending,
                })],
                checkpoints: vec![],
                source: None,
            },
        )?;
        merge_stats(&mut report.stats, &stats);
    }

    let blob = PipeCheckpoint::fresh(
        &source.session_id,
        &source.stream_id,
        adapter.normalizer_version(),
    )
    .encode();
    let cursor = match queries::cursor_state(store.conn(), &runtime.id, &source.relative_path)? {
        Some(existing) => existing,
        None => {
            let stats = instrument.apply(
                store,
                &Batch {
                    ops: vec![],
                    checkpoints: vec![CheckpointUpdate {
                        runtime_id: runtime.id.clone(),
                        stream: source.stream_id.clone(),
                        path: source.relative_path.clone(),
                        session_id: source.session_id.clone(),
                        expected: None,
                        next: blob.clone(),
                        updated_at: None,
                    }],
                    source: None,
                },
            )?;
            merge_stats(&mut report.stats, &stats);
            queries::cursor_state(store.conn(), &runtime.id, &source.relative_path)?.ok_or_else(
                || PipelineError::Config("cursor row vanished during rebuild".to_string()),
            )?
        }
    };

    let mut ctx = NormalizeCtx::new(runtime.id.as_str(), source.session_id.as_str());
    ctx.native_session_id = Some(source.native_session_id.clone());
    ctx.agent_version = source.agent_version.clone();
    ctx.state = adapter.restore_state(None).map_err(PipelineError::Config)?;
    ctx.src_cursor = Some(cursor.id);
    ctx.source_identity = Some(source.relative_path.clone());
    ctx.source_generation = generation;

    report.files += 1;
    Ok(FileReplay {
        ctx,
        source: source.clone(),
        cursor_id: cursor.id,
        cursor_blob: cursor.checkpoint,
        generation,
        next_sequence: 0,
        ops: Vec::new(),
        last_offset: None,
        generation_clean: true,
        activated: false,
    })
}

#[allow(clippy::too_many_arguments)]
fn flush_chunk(
    adapter: &dyn AgentAdapter,
    runtime: &RuntimeRow,
    file: &mut FileReplay,
    mode: SourceMode,
    checkpoint: bool,
    store: &mut Store,
    report: &mut RebuildReport,
    instrument: &Instrument,
) -> Result<(), PipelineError> {
    let ops = std::mem::take(&mut file.ops);
    let first_sequence = file.next_sequence;
    file.next_sequence += ops.len() as u64;

    // The final chunk activates and moves the cursor to a resumable state:
    // tail None means a still-present source will be re-framed from zero on
    // the next ingest — wasteful, idempotent, correct; a GC'd source simply
    // reports Missing.
    let checkpoints = if checkpoint {
        let reading = *file.ctx.cumulative.last_reading();
        let next = PipeCheckpoint {
            v: PIPE_CHECKPOINT_VERSION,
            session_id: file.source.session_id.clone(),
            stream_id: file.source.stream_id.clone(),
            normalizer_version: adapter.normalizer_version(),
            source_generation: file.generation,
            next_source_sequence: file.next_sequence,
            generation_clean: file.generation_clean,
            next_ordinal: file.ctx.next_ordinal,
            tail: None,
            cumulative: (!reading.is_empty()).then_some(reading),
            adapter_state: adapter
                .checkpoint_state(file.ctx.state.as_deref())
                .map_err(PipelineError::Config)?,
            snapshot_stamp: None,
            snapshot_revision: None,
        }
        .encode();
        let update = CheckpointUpdate {
            runtime_id: runtime.id.clone(),
            stream: file.source.stream_id.clone(),
            path: file.source.relative_path.clone(),
            session_id: file.source.session_id.clone(),
            expected: Some(file.cursor_blob.clone()),
            next: next.clone(),
            updated_at: None,
        };
        file.cursor_blob = next;
        vec![update]
    } else {
        vec![]
    };

    let stats = instrument.apply(
        store,
        &Batch {
            ops,
            checkpoints,
            source: Some(SourceWrite {
                cursor_id: file.cursor_id,
                session_id: file.source.session_id.clone(),
                generation: file.generation,
                first_sequence,
                mode,
            }),
        },
    )?;
    merge_stats(&mut report.stats, &stats);
    instrument.add(Metric::BatchesApplied, 1);
    Ok(())
}
