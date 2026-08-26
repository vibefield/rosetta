//! Rebuild-not-migrate, as code: drop the projection, replay the ledger.
//!
//! Two passes over one runtime's ledger. Pass one finds each source file's
//! HIGHEST generation — earlier generations were rewritten away and replaying
//! them would only stage rows the swap deletes again. Pass two applies
//! entries in ledger order: meta batches as-is; raw records through the
//! adapter into per-file contexts, staged in chunks and activated on the
//! file's final chunk, so the same generation machinery that guards live
//! ingest guards the rebuild.
//!
//! Deterministic ids make all of this idempotent — a rebuild interrupted
//! halfway and rerun converges on the same projection.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use rosetta_adapter_support::{AgentAdapter, LineStatus, NormalizeCtx};
use rosetta_capture::{LedgerEntry, SourceDef};
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

#[derive(Debug, Default)]
pub struct RebuildReport {
    pub segments: usize,
    pub torn_segments: usize,
    pub meta_batches: u64,
    pub files: u64,
    pub raw_records: u64,
    /// Records from superseded generations, skipped by design.
    pub stale_records_skipped: u64,
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
    remaining: u64,
    next_sequence: u64,
    ops: Vec<IngestOp>,
}

pub fn rebuild_runtime(
    adapter: &dyn AgentAdapter,
    runtime: &RuntimeRow,
    ledger_dir: &Path,
    store: &mut Store,
    instrument: &Instrument,
) -> Result<RebuildReport, PipelineError> {
    let scanned = rosetta_capture::scan_ledger(ledger_dir)?;
    let mut report = RebuildReport {
        segments: scanned.segments,
        torn_segments: scanned.torn_segments,
        ..Default::default()
    };

    // Pass one: the surviving generation per file, and its record count so
    // pass two knows which chunk is the last (the one that activates).
    let mut surviving: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    for entry in &scanned.entries {
        if let LedgerEntry::Raw {
            source, generation, ..
        } = entry
        {
            let slot = surviving
                .entry(source.relative_path.clone())
                .or_insert((*generation, 0));
            if *generation > slot.0 {
                *slot = (*generation, 0);
            }
            if *generation == slot.0 {
                slot.1 += 1;
            }
        }
    }

    // The runtime row first: every later FK hangs off it.
    let stats = instrument.time(Metric::ApplyNanos, || {
        store.apply(&Batch {
            ops: vec![IngestOp::Runtime(runtime.clone())],
            checkpoints: vec![],
            source: None,
        })
    })?;
    merge_stats(&mut report.stats, &stats);

    let mut sessions_seen: HashSet<String> = HashSet::new();
    let mut already_projected: HashSet<String> = HashSet::new();
    let mut files: BTreeMap<String, FileReplay> = BTreeMap::new();

    for entry in &scanned.entries {
        match entry {
            LedgerEntry::Meta(json) => {
                let ops: Vec<IngestOp> = serde_json::from_str(json).map_err(|error| {
                    PipelineError::Config(format!("undecodable meta batch in ledger: {error}"))
                })?;
                for op in &ops {
                    if let IngestOp::SessionMeta(row) = op {
                        sessions_seen.insert(row.id.clone());
                    }
                }
                report.meta_batches += 1;
                let stats = instrument.time(Metric::ApplyNanos, || {
                    store.apply(&Batch {
                        ops: ops.clone(),
                        checkpoints: vec![],
                        source: None,
                    })
                })?;
                merge_stats(&mut report.stats, &stats);
            }
            LedgerEntry::Raw {
                source,
                generation,
                start_offset,
                payload,
            } => {
                let Some((surviving_generation, record_count)) =
                    surviving.get(&source.relative_path).copied()
                else {
                    continue;
                };
                if *generation != surviving_generation {
                    report.stale_records_skipped += 1;
                    continue;
                }
                if already_projected.contains(&source.relative_path) {
                    report.records_already_projected += 1;
                    continue;
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
                        Some(cursor) if cursor.active_generation >= surviving_generation => {
                            already_projected.insert(source.relative_path.clone());
                            report.records_already_projected += 1;
                            continue;
                        }
                        Some(cursor) => surviving_generation.max(cursor.highest_generation + 1),
                        None => surviving_generation,
                    };
                    let replay = open_file(
                        adapter,
                        runtime,
                        source,
                        replay_generation,
                        record_count,
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

                report.raw_records += 1;
                instrument.add(Metric::RecordsFramed, 1);
                file.ctx.src_offset = Some(*start_offset as i64);
                let outcome = instrument.time(Metric::NormalizeNanos, || {
                    adapter.ingest_line(&mut file.ctx, &file.source.stream_id, payload)
                });
                match &outcome.status {
                    LineStatus::Parsed => report.parsed += 1,
                    LineStatus::UnknownType => report.unknown += 1,
                    LineStatus::HardError { .. } => report.hard_errors += 1,
                }
                file.ops.extend(outcome.ops);
                file.remaining -= 1;

                if file.remaining == 0 || file.ops.len() >= CHUNK_OPS {
                    flush_chunk(adapter, runtime, file, store, &mut report, instrument)?;
                }
            }
        }
    }

    Ok(report)
}

#[allow(clippy::too_many_arguments)]
fn open_file(
    adapter: &dyn AgentAdapter,
    runtime: &RuntimeRow,
    source: &SourceDef,
    generation: u64,
    record_count: u64,
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
        let stats = instrument.time(Metric::ApplyNanos, || {
            store.apply(&Batch {
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
            })
        })?;
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
            let stats = instrument.time(Metric::ApplyNanos, || {
                store.apply(&Batch {
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
                })
            })?;
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
        remaining: record_count,
        next_sequence: 0,
        ops: Vec::new(),
    })
}

fn flush_chunk(
    adapter: &dyn AgentAdapter,
    runtime: &RuntimeRow,
    file: &mut FileReplay,
    store: &mut Store,
    report: &mut RebuildReport,
    instrument: &Instrument,
) -> Result<(), PipelineError> {
    let last = file.remaining == 0;
    let ops = std::mem::take(&mut file.ops);
    let first_sequence = file.next_sequence;
    file.next_sequence += ops.len() as u64;

    // The final chunk activates and moves the cursor to a resumable state:
    // tail None means a still-present source will be re-framed from zero on
    // the next ingest — wasteful, idempotent, correct; a GC'd source simply
    // reports Missing.
    let checkpoints = if last {
        let reading = *file.ctx.cumulative.last_reading();
        let next = PipeCheckpoint {
            v: PIPE_CHECKPOINT_VERSION,
            session_id: file.source.session_id.clone(),
            stream_id: file.source.stream_id.clone(),
            normalizer_version: adapter.normalizer_version(),
            source_generation: file.generation,
            next_source_sequence: file.next_sequence,
            generation_clean: true,
            next_ordinal: file.ctx.next_ordinal,
            tail: None,
            cumulative: (!reading.is_empty()).then_some(reading),
            adapter_state: adapter
                .checkpoint_state(file.ctx.state.as_deref())
                .map_err(PipelineError::Config)?,
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

    let stats = instrument.time(Metric::ApplyNanos, || {
        store.apply(&Batch {
            ops,
            checkpoints,
            source: Some(SourceWrite {
                cursor_id: file.cursor_id,
                session_id: file.source.session_id.clone(),
                generation: file.generation,
                first_sequence,
                mode: if last {
                    SourceMode::Activate
                } else {
                    SourceMode::Stage
                },
            }),
        })
    })?;
    merge_stats(&mut report.stats, &stats);
    instrument.add(Metric::BatchesApplied, 1);
    Ok(())
}
