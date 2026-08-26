//! Phase 3 — stream. Tail each session file through the capture layer, feed
//! complete records to the adapter, and commit ops + cursor in ONE store
//! transaction per batch (the crash protocol: a failed CAS rolls the whole
//! batch back, so the projection never runs ahead of its cursor).
//!
//! Backfill is synchronous and single-writer for now; the scheduler already
//! orders work the way the parallel-lane runtime will (live ▸ on-open ▸
//! newest-first backfill), so growing threads later changes topology, not
//! semantics.

use std::path::Path;

use rosetta_adapter_support::{AgentAdapter, LineStatus, NormalizeCtx};
use rosetta_capture::{TailItem, TailRead, Tailer, Transition};
use rosetta_core::IngestOp;
use rosetta_core::fold::CumulativeUsage;
use rosetta_core::rows::{DriftFact, RuntimeRow, StreamState};
use rosetta_store::{ApplyStats, Batch, CheckpointUpdate, Store, StoreError, queries};

use crate::checkpoint::PipeCheckpoint;
use crate::instrument::{Instrument, Metric};
use crate::meta::SessionFile;
use crate::sched::{Lane, WorkItem, WorkQueue};
use crate::{IngestOptions, PipelineError};

#[derive(Debug, Default)]
pub struct StreamReport {
    pub files: usize,
    pub files_missing: usize,
    pub batches: u64,
    pub records: u64,
    pub oversized: u64,
    pub parsed: u64,
    pub unknown: u64,
    pub hard_errors: u64,
    pub generations_restarted: u64,
    pub conflicts: u64,
    pub retries_exhausted: u64,
    pub sessions_indexed: u64,
    pub stats: ApplyStats,
}

pub fn stream_backfill(
    adapter: &dyn AgentAdapter,
    runtime: &RuntimeRow,
    root: &Path,
    store: &mut Store,
    files: &[SessionFile],
    opts: &IngestOptions,
    instrument: &Instrument,
) -> Result<StreamReport, PipelineError> {
    let tailer = Tailer::new(opts.tail.clone())?;
    let mut queue = WorkQueue::new();
    for file in files {
        queue.push(WorkItem {
            lane: Lane::Backfill,
            relative_path: file.relative_path.clone(),
            modified_ms: file.modified_ms,
        });
    }

    let mut report = StreamReport::default();
    while let Some(work) = queue.pop() {
        let Some(file) = files
            .iter()
            .find(|candidate| candidate.relative_path == work.relative_path)
        else {
            continue;
        };
        report.files += 1;
        ingest_file(
            adapter,
            runtime,
            root,
            store,
            file,
            &tailer,
            opts,
            instrument,
            &mut report,
        )?;
    }
    Ok(report)
}

/// Load-or-create the cursor row for a file, returning its rowid and the
/// EXACT stored blob (the CAS expectation, decodable or not).
fn cursor_row(
    store: &mut Store,
    runtime: &RuntimeRow,
    file: &SessionFile,
    instrument: &Instrument,
) -> Result<(i64, String), PipelineError> {
    if let Some(existing) = queries::checkpoint(store.conn(), &runtime.id, &file.relative_path)? {
        return Ok(existing);
    }
    let initial = PipeCheckpoint::fresh(&file.session_id, &file.stream_id).encode();
    instrument.time(Metric::ApplyNanos, || {
        store.apply(&Batch {
            ops: vec![],
            checkpoints: vec![CheckpointUpdate {
                runtime_id: runtime.id.clone(),
                stream: file.stream_id.clone(),
                path: file.relative_path.clone(),
                expected: None,
                next: initial,
                updated_at: Some(file.modified_ms),
            }],
        })
    })?;
    queries::checkpoint(store.conn(), &runtime.id, &file.relative_path)?
        .ok_or_else(|| PipelineError::Config("cursor row vanished after insert".to_string()))
}

#[allow(clippy::too_many_arguments)]
fn ingest_file(
    adapter: &dyn AgentAdapter,
    runtime: &RuntimeRow,
    root: &Path,
    store: &mut Store,
    file: &SessionFile,
    tailer: &Tailer,
    opts: &IngestOptions,
    instrument: &Instrument,
    report: &mut StreamReport,
) -> Result<(), PipelineError> {
    let path = root.join(&file.relative_path);
    let (cursor_id, mut prev_blob) = cursor_row(store, runtime, file, instrument)?;

    // An expired blob (undecodable, foreign version, or a cursor row that
    // belongs to a different session id) means full replay from byte zero —
    // deterministic ids make the replay idempotent.
    let resumed = PipeCheckpoint::decode(&prev_blob)
        .filter(|checkpoint| checkpoint.session_id == file.session_id);
    let mut tail = resumed.as_ref().and_then(PipeCheckpoint::tail_checkpoint);

    let mut ctx = NormalizeCtx::new(runtime.id.as_str(), file.session_id.as_str());
    ctx.agent_version = file.agent_version.clone();
    ctx.state = adapter.init_state();
    ctx.src_cursor = Some(cursor_id);
    if let Some(checkpoint) = &resumed {
        ctx.next_ordinal = checkpoint.next_ordinal;
        if let Some(last) = checkpoint.cumulative {
            ctx.cumulative = CumulativeUsage::resume(last);
        }
    }

    let mut retries = 0_u32;
    loop {
        let read = instrument.time(Metric::ReadNanos, || {
            tailer.read(&path, tail.as_ref(), false)
        })?;
        let batch = match read {
            TailRead::Missing => {
                report.files_missing += 1;
                return Ok(());
            }
            TailRead::Retry => {
                retries += 1;
                if retries > opts.max_retries {
                    report.retries_exhausted += 1;
                    return Ok(());
                }
                continue;
            }
            TailRead::Batch(batch) => batch,
        };
        instrument.add(Metric::BytesRead, batch.bytes_read);

        if batch.transition.starts_new_generation() && tail.is_some() {
            debug_assert!(!matches!(batch.transition, Transition::Initial));
            report.generations_restarted += 1;
            ctx.next_ordinal = 0;
            ctx.cumulative = CumulativeUsage::new();
            ctx.state = adapter.init_state();
        }

        let mut ops: Vec<IngestOp> = Vec::with_capacity(batch.items.len());
        for item in &batch.items {
            match item {
                TailItem::Record(record) => {
                    report.records += 1;
                    instrument.add(Metric::RecordsFramed, 1);
                    ctx.src_offset = Some(record.start_offset as i64);
                    let outcome = instrument.time(Metric::NormalizeNanos, || {
                        adapter.ingest_line(&mut ctx, &file.stream_id, &record.payload)
                    });
                    match &outcome.status {
                        LineStatus::Parsed => {
                            report.parsed += 1;
                            instrument.add(Metric::LinesParsed, 1);
                        }
                        LineStatus::UnknownType => {
                            report.unknown += 1;
                            instrument.add(Metric::LinesUnknown, 1);
                        }
                        LineStatus::HardError { .. } => {
                            report.hard_errors += 1;
                            instrument.add(Metric::LinesHardError, 1);
                        }
                    }
                    ops.extend(outcome.ops);
                }
                TailItem::Oversized {
                    start_offset,
                    payload_len,
                    ..
                } => {
                    report.oversized += 1;
                    instrument.add(Metric::RecordsOversized, 1);
                    let _ = payload_len;
                    ops.push(IngestOp::Drift(DriftFact {
                        runtime_id: runtime.id.clone(),
                        agent_version: file.agent_version.clone().unwrap_or_default(),
                        stream: file.stream_id.clone(),
                        record_kind: "raw_record".to_string(),
                        error_code: "oversized".to_string(),
                        shape_hash: String::new(),
                        at: None,
                        sample_cursor: Some(cursor_id),
                        sample_offset: Some(*start_offset as i64),
                    }));
                }
            }
        }

        let last_reading = *ctx.cumulative.last_reading();
        let next_blob = PipeCheckpoint {
            v: crate::checkpoint::PIPE_CHECKPOINT_VERSION,
            session_id: file.session_id.clone(),
            stream_id: file.stream_id.clone(),
            next_ordinal: ctx.next_ordinal,
            tail: Some(batch.checkpoint.encode_hex()),
            cumulative: (!last_reading.is_empty()).then_some(last_reading),
        }
        .encode();

        // Nothing new and nothing to move the cursor to: don't write an
        // empty transaction on every re-scan of a quiet file.
        if ops.is_empty() && next_blob == prev_blob && !batch.more_available {
            break;
        }

        let ops_count = ops.len() as u64;
        let applied = instrument.time(Metric::ApplyNanos, || {
            store.apply(&Batch {
                ops,
                checkpoints: vec![CheckpointUpdate {
                    runtime_id: runtime.id.clone(),
                    stream: file.stream_id.clone(),
                    path: file.relative_path.clone(),
                    expected: Some(prev_blob.clone()),
                    next: next_blob.clone(),
                    updated_at: Some(file.modified_ms),
                }],
            })
        });
        let stats = match applied {
            Ok(stats) => stats,
            Err(StoreError::CheckpointConflict { .. }) => {
                // Another writer owns this file now; abandon it untouched.
                report.conflicts += 1;
                instrument.add(Metric::CheckpointConflicts, 1);
                return Ok(());
            }
            Err(error) => return Err(error.into()),
        };
        merge_stats(&mut report.stats, &stats);
        report.batches += 1;
        instrument.add(Metric::OpsEmitted, ops_count);
        instrument.add(Metric::BatchesApplied, 1);

        prev_blob = next_blob;
        tail = Some(batch.checkpoint);
        if !batch.more_available {
            // An incomplete trailing record stays unconsumed until the file
            // grows; looping now would re-frame the same bytes forever.
            break;
        }
    }

    // The whole backlog of this file is in the projection — flip the session
    // out of Pending so consumers know the timeline is complete.
    if let Some(mut session) = queries::session(store.conn(), &file.session_id)?
        && session.stream_state != StreamState::Indexed
    {
        session.stream_state = StreamState::Indexed;
        let stats = instrument.time(Metric::ApplyNanos, || {
            store.apply(&Batch {
                ops: vec![IngestOp::SessionMeta(session)],
                checkpoints: vec![],
            })
        })?;
        merge_stats(&mut report.stats, &stats);
        report.sessions_indexed += 1;
    }
    Ok(())
}

fn merge_stats(total: &mut ApplyStats, batch: &ApplyStats) {
    total.messages_inserted += batch.messages_inserted;
    total.messages_updated += batch.messages_updated;
    total.tool_results_merged += batch.tool_results_merged;
    total.tool_results_unmatched += batch.tool_results_unmatched;
    total.usage_applied += batch.usage_applied;
    total.usage_unmatched += batch.usage_unmatched;
    total.drift_recorded += batch.drift_recorded;
}
