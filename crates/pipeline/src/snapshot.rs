//! Stream phase for `SqliteSnapshot` sources — rewrite-in-place databases
//! projected through the SAME machinery as everything else.
//!
//! Each database session is a VIRTUAL source (`<db>#<native-id>`) with its
//! own cursor. A pass stamps the database (equal stamps skip even the copy),
//! snapshots it once per database, serializes each session's declared
//! queries to records, and hashes them: an unchanged revision is at most a
//! stamp refresh; a changed one is an ordinary staged-generation swap. The
//! full-content revision makes a separate audit lane unnecessary here — every
//! pass verifies content, not metadata.

use std::collections::BTreeMap;
use std::path::Path;

use rosetta_adapter_support::{AgentAdapter, LineStatus, NormalizeCtx};
use rosetta_core::IngestOp;
use rosetta_store::{Batch, CheckpointUpdate, SourceMode, SourceWrite, Store, StoreError, queries};

use crate::checkpoint::{PIPE_CHECKPOINT_VERSION, PipeCheckpoint};
use crate::instrument::{Instrument, Metric};
use crate::meta::SessionFile;
use crate::stream::{StreamReport, cursor_row, merge_stats};
use crate::{IngestOptions, PipelineError};

const CHUNK_OPS: usize = 512;

/// The database path of a virtual source, if it is one.
pub(crate) fn virtual_db_path(relative_path: &str) -> Option<&str> {
    relative_path.split_once('#').map(|(db, _)| db)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn stream_snapshot_sources(
    adapter: &dyn AgentAdapter,
    runtime: &rosetta_core::rows::RuntimeRow,
    root: &Path,
    store: &mut Store,
    files: &[&SessionFile],
    opts: &IngestOptions,
    instrument: &Instrument,
    ledger: &mut Option<&mut rosetta_capture::Ledger>,
    report: &mut StreamReport,
    file_health: &mut BTreeMap<String, bool>,
    session_health: &mut BTreeMap<String, bool>,
) -> Result<(), PipelineError> {
    let mut by_db: BTreeMap<&str, Vec<&SessionFile>> = BTreeMap::new();
    for file in files {
        let Some(db) = virtual_db_path(&file.relative_path) else {
            return Err(PipelineError::Config(format!(
                "snapshot source {} has no #session suffix",
                file.relative_path
            )));
        };
        by_db.entry(db).or_default().push(file);
    }

    for (db_relative, group) in by_db {
        opts.check_cancelled()?;
        report.files += group.len();
        let mark = |file: &SessionFile,
                    complete: bool,
                    file_health: &mut BTreeMap<String, bool>,
                    session_health: &mut BTreeMap<String, bool>| {
            file_health.insert(file.relative_path.clone(), complete);
            session_health
                .entry(file.session_id.clone())
                .and_modify(|healthy| *healthy &= complete);
        };

        let db_path = root.join(db_relative);
        let Some(_) = rosetta_capture::db_stamp(&db_path)? else {
            report.files_missing += group.len() as u64 as usize;
            if group.iter().all(|file| file.source_missing) {
                for file in &group {
                    opts.check_cancelled()?;
                    let cursor = cursor_row(adapter, store, runtime, file, instrument)?;
                    let complete = apply_snapshot_records(
                        adapter,
                        runtime,
                        store,
                        file,
                        &cursor,
                        &[],
                        "missing",
                        opts,
                        ledger,
                        instrument,
                        report,
                    )?;
                    mark(file, complete, file_health, session_health);
                }
                continue;
            }
            for file in &group {
                mark(file, false, file_health, session_health);
            }
            continue;
        };

        // Load (or create) every virtual source cursor. Metadata never decides
        // content identity: every pass takes one SQLite-consistent backup and
        // hashes the adapter's declared rows.
        let mut cursors = Vec::with_capacity(group.len());
        for file in &group {
            opts.check_cancelled()?;
            cursors.push(cursor_row(adapter, store, runtime, file, instrument)?);
        }

        let Some(plan) = adapter.snapshot_plan(&group[0].stream_id) else {
            return Err(PipelineError::Config(format!(
                "{} drives stream {} as SqliteSnapshot but declares no snapshot plan",
                adapter.kind(),
                group[0].stream_id
            )));
        };

        // The stat gate first: opening the source — even read-only — writes
        // its `-shm`, and the watcher sees that write as a database hint, so
        // an unconditional open per pass feeds the daemon its own footsteps.
        // When every cursor in the group already ingested exactly this
        // stamp's content (and that generation is active), there is nothing
        // to read and, critically, nothing to open. The gate yields to any
        // pass carrying an audit budget: those hash full content by
        // construction — the lane that still catches a database whose
        // content moved under a preserved stamp, once per audit sweep —
        // while the audit-free hint, catch-up, and cold lanes (the ones a
        // self-hint loop can feed) trust a stamp their own checkpoint
        // already ingested.
        let current_stamp = (opts.audit_files_per_run == 0)
            .then(|| rosetta_capture::db_stamp(&db_path))
            .transpose()?
            .flatten();
        if let Some(stamp) = &current_stamp {
            // The skip repeats the outcome the stamp's own pass reported:
            // each file's generation_clean, never an unconditional true — a
            // dirty-but-active generation stays Streaming through the gate.
            let settled: Vec<bool> = group
                .iter()
                .zip(&cursors)
                .filter_map(|(file, cursor)| {
                    PipeCheckpoint::decode(&cursor.checkpoint)
                        .filter(|checkpoint| {
                            checkpoint.session_id == file.session_id
                                && checkpoint.stream_id == file.stream_id
                                && checkpoint.normalizer_version == adapter.normalizer_version()
                                && checkpoint.source_generation == cursor.active_generation
                                && checkpoint.snapshot_stamp.as_deref() == Some(stamp.as_str())
                        })
                        .map(|checkpoint| checkpoint.generation_clean)
                })
                .collect();
            if settled.len() == group.len() {
                for (file, clean) in group.iter().zip(settled) {
                    mark(file, clean, file_health, session_health);
                }
                continue;
            }
        }

        // One stable copy per database per pass, shared by all its sessions.
        let mut retries = 0_u32;
        let snapshot = loop {
            opts.check_cancelled()?;
            match instrument.time(Metric::ReadNanos, || rosetta_capture::snapshot_db(&db_path))? {
                rosetta_capture::SnapshotOutcome::Ready(snapshot) => break Some(snapshot),
                rosetta_capture::SnapshotOutcome::Missing => break None,
                rosetta_capture::SnapshotOutcome::Unstable => {
                    retries += 1;
                    if retries > opts.max_retries {
                        break None;
                    }
                }
            }
        };
        let Some(snapshot) = snapshot else {
            report.retries_exhausted += group.len() as u64;
            for file in &group {
                mark(file, false, file_health, session_health);
            }
            continue;
        };
        let stamp = snapshot.stamp().clone();

        for (file, cursor) in group.iter().zip(cursors) {
            opts.check_cancelled()?;
            let mut records = Vec::new();
            for query in &plan.record_queries {
                opts.check_cancelled()?;
                let name = serde_json::to_string(&query.name).expect("string encodes");
                for row in instrument.time(Metric::ReadNanos, || {
                    snapshot.query_rows(&query.sql, Some(&file.native_session_id))
                })? {
                    records.push(format!("{{\"query\":{name},\"row\":{row}}}").into_bytes());
                }
            }
            let complete = apply_snapshot_records(
                adapter,
                runtime,
                store,
                file,
                &cursor,
                &records,
                stamp.as_str(),
                opts,
                ledger,
                instrument,
                report,
            )?;
            mark(file, complete, file_health, session_health);
        }
    }
    Ok(())
}

/// Project one stable record snapshot through the common staged-generation
/// protocol. SQLite and whole-document sources deliberately meet here so
/// revision checks, raw-ledger ordering, health, and sweeps cannot diverge.
#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_snapshot_records(
    adapter: &dyn AgentAdapter,
    runtime: &rosetta_core::rows::RuntimeRow,
    store: &mut Store,
    file: &SessionFile,
    cursor: &queries::CursorState,
    records: &[Vec<u8>],
    stamp: &str,
    opts: &IngestOptions,
    ledger: &mut Option<&mut rosetta_capture::Ledger>,
    instrument: &Instrument,
    report: &mut StreamReport,
) -> Result<bool, PipelineError> {
    let mut hasher = blake3::Hasher::new();
    for record in records {
        opts.check_cancelled()?;
        hasher.update(record);
        hasher.update(b"\n");
        instrument.add(Metric::BytesRead, record.len() as u64 + 1);
    }
    let revision = hasher.finalize().to_hex().to_string();

    let checkpoint = PipeCheckpoint::decode(&cursor.checkpoint).filter(|checkpoint| {
        checkpoint.session_id == file.session_id
            && checkpoint.stream_id == file.stream_id
            && checkpoint.normalizer_version == adapter.normalizer_version()
    });
    if let Some(previous) = &checkpoint
        && previous.snapshot_revision.as_deref() == Some(revision.as_str())
        && cursor.active_generation == previous.source_generation
    {
        if previous.snapshot_stamp.as_deref() != Some(stamp) {
            let mut next = previous.clone();
            next.snapshot_stamp = Some(stamp.to_string());
            let applied = instrument.apply(
                store,
                &Batch {
                    ops: vec![],
                    checkpoints: vec![CheckpointUpdate {
                        runtime_id: runtime.id.clone(),
                        stream: file.stream_id.clone(),
                        path: file.relative_path.clone(),
                        session_id: file.projected_session_id(),
                        expected: Some(cursor.checkpoint.clone()),
                        next: next.encode(),
                        updated_at: Some(file.modified_ms),
                    }],
                    source: None,
                },
            );
            match applied {
                Ok(stats) => {
                    merge_stats(&mut report.stats, &stats);
                    report.batches += 1;
                }
                Err(StoreError::CheckpointConflict { .. }) => {
                    report.conflicts += 1;
                    return Ok(false);
                }
                Err(error) => return Err(error.into()),
            }
        }
        return Ok(previous.generation_clean);
    }

    if cursor.active_generation > 0 {
        report.generations_restarted += 1;
    }
    let generation = cursor
        .highest_generation
        .max(cursor.active_generation)
        .checked_add(1)
        .ok_or_else(|| PipelineError::Config("snapshot generation overflowed".to_string()))?;

    if let Some(raw_ledger) = ledger.as_deref_mut() {
        instrument.ledger_write(raw_ledger, |raw_ledger| -> Result<(), PipelineError> {
            let source = rosetta_capture::SourceDef {
                ledger_only: file.ledger_only,
                relative_path: file.relative_path.clone(),
                stream_id: file.stream_id.clone(),
                session_id: file.session_id.clone(),
                native_session_id: file.native_session_id.clone(),
                agent_version: file.agent_version.clone(),
            };
            if !records.is_empty() {
                raw_ledger.append_raw(
                    &source,
                    generation,
                    records
                        .iter()
                        .enumerate()
                        .map(|(index, record)| (index as u64, record.as_slice())),
                )?;
            }
            raw_ledger.checkpoint()?;
            Ok(())
        })?;
    }

    let mut ctx = NormalizeCtx::new(runtime.id.as_str(), file.session_id.as_str());
    ctx.native_session_id = Some(file.native_session_id.clone());
    ctx.agent_version = file.agent_version.clone();
    ctx.state = adapter.restore_state(None).map_err(PipelineError::Config)?;
    ctx.src_cursor = Some(cursor.id);
    ctx.source_identity = Some(file.relative_path.clone());
    ctx.source_generation = generation;

    let mut generation_clean = true;
    let mut next_sequence = 0_u64;
    let mut prev_blob = cursor.checkpoint.clone();
    let mut ops: Vec<IngestOp> = Vec::new();
    let mut conflicted = false;
    let mut progress_bytes = 0;
    for (index, record) in records.iter().enumerate() {
        opts.check_cancelled()?;
        report.records += 1;
        report.payload_bytes += record.len() as u64;
        progress_bytes += record.len() as u64;
        if (index + 1).is_multiple_of(64) || index + 1 == records.len() {
            instrument.add(Metric::PayloadBytes, progress_bytes);
            if let Some(progress) = &opts.progress {
                progress.source_bytes(file, progress_bytes);
            }
            progress_bytes = 0;
        }
        instrument.add(Metric::RecordsFramed, 1);
        ctx.src_offset = Some(index as i64);
        let outcome = instrument.time(Metric::NormalizeNanos, || {
            adapter
                .ingest_line(&mut ctx, &file.stream_id, record)
                .into_projection()
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
                generation_clean = false;
                report.hard_errors += 1;
                instrument.add(Metric::LinesHardError, 1);
            }
        }
        ops.extend(outcome.ops);

        let last = index + 1 == records.len();
        if !last && ops.len() < CHUNK_OPS {
            continue;
        }
        if last {
            seal_generation(
                ledger,
                adapter,
                file,
                generation,
                generation_clean,
                instrument,
            )?;
        }
        if flush_chunk(
            adapter,
            runtime,
            store,
            file,
            cursor,
            generation,
            &mut ops,
            &mut next_sequence,
            last,
            generation_clean,
            stamp,
            &revision,
            &mut ctx,
            &mut prev_blob,
            instrument,
            report,
        )? {
            conflicted = true;
            break;
        }
    }
    if records.is_empty() && !conflicted {
        seal_generation(
            ledger,
            adapter,
            file,
            generation,
            generation_clean,
            instrument,
        )?;
        conflicted = flush_chunk(
            adapter,
            runtime,
            store,
            file,
            cursor,
            generation,
            &mut ops,
            &mut next_sequence,
            true,
            generation_clean,
            stamp,
            &revision,
            &mut ctx,
            &mut prev_blob,
            instrument,
            report,
        )?;
    }
    if conflicted {
        report.conflicts += 1;
        return Ok(false);
    }
    Ok(generation_clean)
}

pub(crate) fn seal_generation(
    ledger: &mut Option<&mut rosetta_capture::Ledger>,
    adapter: &dyn AgentAdapter,
    file: &SessionFile,
    generation: u64,
    generation_clean: bool,
    instrument: &Instrument,
) -> Result<(), PipelineError> {
    let Some(ledger) = ledger.as_deref_mut() else {
        return Ok(());
    };
    instrument.ledger_write(ledger, |ledger| -> Result<(), PipelineError> {
        ledger.complete_generation(
            &rosetta_capture::SourceDef {
                ledger_only: file.ledger_only,
                relative_path: file.relative_path.clone(),
                stream_id: file.stream_id.clone(),
                session_id: file.session_id.clone(),
                native_session_id: file.native_session_id.clone(),
                agent_version: file.agent_version.clone(),
            },
            generation,
            adapter.normalizer_version(),
            generation_clean,
        )?;
        ledger.checkpoint()?;
        Ok(())
    })
}

/// One staged chunk (or the activating final one). Returns true on a
/// checkpoint conflict — another writer owns the cursor now.
#[allow(clippy::too_many_arguments)]
pub(crate) fn flush_chunk(
    adapter: &dyn AgentAdapter,
    runtime: &rosetta_core::rows::RuntimeRow,
    store: &mut Store,
    file: &SessionFile,
    cursor: &queries::CursorState,
    generation: u64,
    ops: &mut Vec<IngestOp>,
    next_sequence: &mut u64,
    last: bool,
    generation_clean: bool,
    stamp: &str,
    revision: &str,
    ctx: &mut NormalizeCtx,
    prev_blob: &mut String,
    instrument: &Instrument,
    report: &mut StreamReport,
) -> Result<bool, PipelineError> {
    let chunk = std::mem::take(ops);
    let first_sequence = *next_sequence;
    *next_sequence += chunk.len() as u64;
    let mode = if !last {
        SourceMode::Stage
    } else if generation_clean {
        SourceMode::Activate
    } else {
        SourceMode::Abandon
    };
    let checkpoints = if last {
        let reading = *ctx.cumulative.last_reading();
        let next = PipeCheckpoint {
            v: PIPE_CHECKPOINT_VERSION,
            session_id: file.session_id.clone(),
            stream_id: file.stream_id.clone(),
            normalizer_version: adapter.normalizer_version(),
            source_generation: generation,
            next_source_sequence: *next_sequence,
            generation_clean,
            next_ordinal: ctx.next_ordinal,
            tail: None,
            cumulative: (!reading.is_empty()).then_some(reading),
            adapter_state: adapter
                .checkpoint_state(ctx.state.as_deref())
                .map_err(PipelineError::Config)?,
            snapshot_stamp: Some(stamp.to_string()),
            snapshot_revision: Some(revision.to_string()),
        }
        .encode();
        let update = CheckpointUpdate {
            runtime_id: runtime.id.clone(),
            stream: file.stream_id.clone(),
            path: file.relative_path.clone(),
            session_id: file.projected_session_id(),
            expected: Some(prev_blob.clone()),
            next: next.clone(),
            updated_at: Some(file.modified_ms),
        };
        *prev_blob = next;
        vec![update]
    } else {
        vec![]
    };

    let ops_count = chunk.len() as u64;
    let applied = instrument.apply(
        store,
        &Batch {
            ops: chunk,
            checkpoints,
            source: Some(SourceWrite {
                cursor_id: cursor.id,
                session_id: file.session_id.clone(),
                generation,
                first_sequence,
                mode,
            }),
        },
    );
    match applied {
        Ok(stats) => {
            merge_stats(&mut report.stats, &stats);
            report.batches += 1;
            instrument.add(Metric::OpsEmitted, ops_count);
            instrument.add(Metric::BatchesApplied, 1);
            Ok(false)
        }
        Err(StoreError::CheckpointConflict { .. }) => Ok(true),
        Err(error) => Err(error.into()),
    }
}
