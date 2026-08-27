//! Rebuild-not-migrate, as code: drop the projection, replay the ledger.
//!
//! Two bounded passes over one runtime's ledger. Pass one reads control frames
//! and raw prefixes while seeking over payloads to find each source file's
//! highest SEALED generation; an orphan partial rewrite has no seal and can
//! never displace the last complete generation. Pass two fully reads and
//! checksums every frame, deduplicates raw source positions, normalizes them in
//! ledger order, activates at the seal, and applies later records as appends.
//!
//! Deterministic ids make all of this idempotent — a rebuild interrupted
//! halfway and rerun converges on the same projection.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::time::Instant;

use rosetta_adapter_support::{AgentAdapter, LineStatus, NormalizeCtx};
use rosetta_capture::{CaptureError, LedgerElectionEntry, LedgerEntry, LedgerVisit, SourceDef};
use rosetta_core::IngestOp;
use rosetta_core::rows::{RuntimeRow, SessionRole, SessionRow, StreamState};
use rosetta_store::{ApplyStats, Batch, CheckpointUpdate, SourceMode, SourceWrite, Store, queries};

use crate::PipelineError;
use crate::checkpoint::{PIPE_CHECKPOINT_VERSION, PipeCheckpoint};
use crate::instrument::{Instrument, Metric};
use crate::stream::merge_stats;

/// Ops per staged chunk. Small enough to bound memory, large enough that the
/// per-transaction cost amortizes.
const CHUNK_OPS: usize = 512;

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

fn visit_election_profiled<E, F>(
    ledger_dir: &Path,
    instrument: &Instrument,
    mut visitor: F,
) -> Result<LedgerVisit, E>
where
    E: From<CaptureError>,
    F: for<'a> FnMut(LedgerElectionEntry<'a>) -> Result<(), E>,
{
    let started = Instant::now();
    let mut visitor_nanos = 0_u64;
    let result = rosetta_capture::visit_ledger_election::<E, _>(ledger_dir, |entry| {
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
    instrument.add(Metric::LedgerElectionReadNanos, ledger_nanos);
    result
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

#[derive(Debug, Default, PartialEq, Eq)]
struct ElectionState {
    surviving: BTreeMap<String, (u64, SourceDef)>,
    latest_seals: BTreeMap<String, (u64, SourceDef, bool)>,
    raw_paths: HashSet<String>,
    sealed_paths: HashSet<String>,
}

impl ElectionState {
    fn observe_raw(&mut self, source: &SourceDef) {
        if !self.raw_paths.contains(&source.relative_path) {
            self.raw_paths.insert(source.relative_path.clone());
        }
    }

    fn observe_complete(
        &mut self,
        adapter_normalizer_version: u32,
        source: &SourceDef,
        generation: u64,
        normalizer_version: u32,
        normalized_clean: bool,
    ) {
        self.sealed_paths.insert(source.relative_path.clone());
        let known_unclean = normalizer_version == adapter_normalizer_version && !normalized_clean;
        let latest = self
            .latest_seals
            .entry(source.relative_path.clone())
            .or_insert_with(|| (generation, source.clone(), known_unclean));
        if generation >= latest.0 {
            *latest = (generation, source.clone(), known_unclean);
        }
        if known_unclean {
            return;
        }
        let survivor = self
            .surviving
            .entry(source.relative_path.clone())
            .or_insert_with(|| (generation, source.clone()));
        if generation >= survivor.0 {
            *survivor = (generation, source.clone());
        }
    }
}

pub fn rebuild_runtime(
    adapter: &dyn AgentAdapter,
    runtime: &RuntimeRow,
    ledger_dir: &Path,
    store: &mut Store,
    instrument: &Instrument,
) -> Result<RebuildReport, PipelineError> {
    let mut report = RebuildReport::default();

    // Pass one: only a raw-complete marker can elect a replacement generation.
    // Raw-only paths are legacy/unsafe rather than guesses: fail closed instead
    // of silently promoting a crash-truncated generation.
    // The latest seal also owns readiness. A current normalizer may retain an
    // older clean projection after a failed rewrite, but the session must stay
    // Streaming until that latest source generation is clean.
    let mut election = ElectionState::default();
    let first = visit_election_profiled::<PipelineError, _>(ledger_dir, instrument, |entry| {
        match entry {
            LedgerElectionEntry::RawSource { source } => {
                election.observe_raw(source);
            }
            LedgerElectionEntry::GenerationComplete {
                source,
                generation,
                normalizer_version,
                normalized_clean,
            } => {
                election.observe_complete(
                    adapter.normalizer_version(),
                    source,
                    generation,
                    normalizer_version,
                    normalized_clean,
                );
            }
        }
        Ok(())
    })?;
    report.segments = first.segments;
    report.torn_segments = first.torn_segments;
    let mut unsealed: Vec<_> = election
        .raw_paths
        .difference(&election.sealed_paths)
        .cloned()
        .collect();
    unsealed.sort();
    if !unsealed.is_empty() {
        return Err(PipelineError::Config(format!(
            "ledger has raw records but no complete-generation marker for: {}",
            unsealed.join(", ")
        )));
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
    let mut replay_election = ElectionState::default();

    let second = visit_replay::<PipelineError, _>(ledger_dir, instrument, |entry| {
        match &entry {
            LedgerEntry::Raw { source, .. } => replay_election.observe_raw(source),
            LedgerEntry::GenerationComplete {
                source,
                generation,
                normalizer_version,
                normalized_clean,
            } => replay_election.observe_complete(
                adapter.normalizer_version(),
                source,
                *generation,
                *normalizer_version,
                *normalized_clean,
            ),
            LedgerEntry::Meta { .. } => {}
        }
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
                let Some((surviving_generation, _)) = election.surviving.get(&source.relative_path)
                else {
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
                if file.ops.len() >= CHUNK_OPS {
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
                let Some((surviving_generation, _)) = election.surviving.get(&source.relative_path)
                else {
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
                    let mode = if file.generation_clean {
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
    })?;
    if first != second || election != replay_election {
        return Err(PipelineError::Config(
            "ledger changed between generation election and replay; stop ingestion and retry"
                .to_string(),
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

    // Rebuild has the same readiness contract as source backfill: only
    // sessions whose every sealed source is currently clean are Indexed.
    // Meta-only sessions are absent from this map and retain Pending.
    let mut session_health: BTreeMap<String, bool> = BTreeMap::new();
    for (path, (_, source, known_unclean)) in election.latest_seals {
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
        let Some(mut session) = queries::session(store.conn(), &session_id)? else {
            continue;
        };
        let next = if clean {
            StreamState::Indexed
        } else {
            StreamState::Streaming
        };
        if session.stream_state == next {
            continue;
        }
        session.stream_state = next;
        let stats = instrument.apply(
            store,
            &Batch {
                ops: vec![IngestOp::SessionMeta(session)],
                checkpoints: vec![],
                source: None,
            },
        )?;
        merge_stats(&mut report.stats, &stats);
    }

    Ok(report)
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
    ctx.agent_version = source.agent_version.clone();
    ctx.state = adapter.restore_state(None).map_err(PipelineError::Config)?;
    ctx.src_cursor = Some(cursor.id);

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
