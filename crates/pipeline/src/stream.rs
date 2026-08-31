//! Phase 3 — stream. Tail each session file through the capture layer, feed
//! complete records to the adapter, and commit ops + cursor in ONE store
//! transaction per batch (the crash protocol: a failed CAS rolls the whole
//! batch back, so the projection never runs ahead of its cursor).
//!
//! Backfill is synchronous and single-writer for now; the scheduler already
//! orders work the way the parallel-lane runtime will (live ▸ on-open ▸
//! newest-first backfill), so growing threads later changes topology, not
//! semantics.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use rosetta_adapter_support::{AgentAdapter, LineStatus, NormalizeCtx};
use rosetta_capture::{TailConfig, TailItem, TailRead, Tailer, Transition};
use rosetta_core::IngestOp;
use rosetta_core::fold::CumulativeUsage;
use rosetta_core::rows::{DriftFact, RuntimeRow, StreamState};
use rosetta_store::{
    ApplyStats, Batch, CheckpointUpdate, SourceMode, SourceWrite, Store, StoreError, queries,
};

use crate::checkpoint::PipeCheckpoint;
use crate::instrument::{Instrument, Metric};
use crate::meta::SessionFile;
use crate::{IngestOptions, PipelineError};

#[derive(Debug, Default)]
pub struct StreamReport {
    /// File PASSES, not distinct files: a budgeted source contributes one
    /// per slice, so compare against roster sizes, never against this.
    pub files: usize,
    pub files_missing: usize,
    pub batches: u64,
    pub records: u64,
    pub oversized: u64,
    pub parsed: u64,
    pub unknown: u64,
    pub hard_errors: u64,
    pub generations_restarted: u64,
    pub files_audited: u64,
    pub conflicts: u64,
    pub retries_exhausted: u64,
    pub files_incomplete: u64,
    /// Files a starter budget paused on purpose: a clean prefix is active and
    /// the remainder awaits a completion pass. Deliberate, never a failure.
    pub files_paused: u64,
    pub sessions_indexed: u64,
    /// Same-session sibling files force-replayed after another file's
    /// generation restart, so cross-file merges land in the rebuilt rows.
    pub siblings_healed: u64,
    pub stats: ApplyStats,
}

impl StreamReport {
    /// Fold another report into this one — the ONE merge every aggregator
    /// (serve's per-runtime totals, live ticks) must use, so a new field can
    /// never be silently dropped by a hand-rolled copy.
    pub fn merge_from(&mut self, part: &StreamReport) {
        self.files += part.files;
        self.files_missing += part.files_missing;
        self.batches += part.batches;
        self.records += part.records;
        self.oversized += part.oversized;
        self.parsed += part.parsed;
        self.unknown += part.unknown;
        self.hard_errors += part.hard_errors;
        self.generations_restarted += part.generations_restarted;
        self.files_audited += part.files_audited;
        self.conflicts += part.conflicts;
        self.retries_exhausted += part.retries_exhausted;
        self.files_incomplete += part.files_incomplete;
        self.files_paused += part.files_paused;
        self.sessions_indexed += part.sessions_indexed;
        self.siblings_healed += part.siblings_healed;
        self.stats.merge_from(&part.stats);
    }
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
    stream_backfill_into(adapter, runtime, root, store, files, opts, instrument, None)
}

/// [`stream_backfill`] with a raw ledger attached: every framed record is
/// appended and durably flushed BEFORE the store transaction that projects
/// it (the crash protocol — an orphan ledger tail is harmless, a projection
/// row without flushed raw bytes is not).
#[allow(clippy::too_many_arguments)]
pub fn stream_backfill_into(
    adapter: &dyn AgentAdapter,
    runtime: &RuntimeRow,
    root: &Path,
    store: &mut Store,
    files: &[SessionFile],
    opts: &IngestOptions,
    instrument: &Instrument,
    mut ledger: Option<&mut rosetta_capture::Ledger>,
) -> Result<StreamReport, PipelineError> {
    let tailer = Tailer::new(opts.tail.clone())?;
    // Snapshot-driven virtual sources take their own path: no tailing, no
    // audit lane (every pass hashes full content — audit by construction).
    let mut document_files = Vec::new();
    let mut snapshot_files = Vec::new();
    let mut tail_files = Vec::new();
    for file in files {
        match file.driver {
            rosetta_adapter_support::StreamDriver::DocumentSnapshot => document_files.push(file),
            rosetta_adapter_support::StreamDriver::SqliteSnapshot => snapshot_files.push(file),
            rosetta_adapter_support::StreamDriver::JsonLines => tail_files.push(file),
        }
    }
    let audit_paths =
        rotating_audit_paths(store, &runtime.id, &tail_files, opts.audit_files_per_run)?;
    let audit_set: BTreeSet<&str> = audit_paths.iter().map(String::as_str).collect();

    let mut report = StreamReport::default();
    let mut restarted_sessions: BTreeSet<String> = BTreeSet::new();
    let mut restarted_files: BTreeSet<String> = BTreeSet::new();
    let mut session_health: BTreeMap<String, bool> = files
        .iter()
        .map(|file| (file.session_id.clone(), true))
        .collect();
    let mut file_health: BTreeMap<String, bool> = BTreeMap::new();
    // Caller order is the schedule: meta hands rosters newest-session-first,
    // and the daemon's cold scheduler passes exactly one file per call.
    for file in tail_files.iter().copied() {
        report.files += 1;
        let restarts_before = report.generations_restarted;
        let outcome = ingest_file(
            adapter,
            runtime,
            root,
            store,
            file,
            &tailer,
            opts,
            instrument,
            &mut report,
            audit_set.contains(file.relative_path.as_str()),
            ledger.as_deref_mut(),
        )?;
        if report.generations_restarted > restarts_before {
            restarted_sessions.insert(file.session_id.clone());
            restarted_files.insert(file.relative_path.clone());
        }
        let complete = outcome == FilePass::Finished(true);
        file_health.insert(file.relative_path.clone(), complete);
        session_health
            .entry(file.session_id.clone())
            .and_modify(|healthy| *healthy &= complete);
        match outcome {
            FilePass::Paused => report.files_paused += 1,
            FilePass::Finished(true) => {}
            FilePass::Finished(false) => report.files_incomplete += 1,
        }
    }

    // A generation restart swept and rebuilt that file's rows — but merge ops
    // a SIBLING file had contributed into them (grok events into chat rows)
    // died with the old rows. One forced replay of the affected sessions'
    // other files re-derives those merges; deterministic ids make it cheap
    // noise for everything already present.
    for file in &tail_files {
        if !restarted_sessions.contains(&file.session_id)
            || restarted_files.contains(&file.relative_path)
        {
            continue;
        }
        report.siblings_healed += 1;
        let outcome = ingest_file(
            adapter,
            runtime,
            root,
            store,
            file,
            &tailer,
            opts,
            instrument,
            &mut report,
            true, // forced replay
            ledger.as_deref_mut(),
        )?;
        let complete = outcome == FilePass::Finished(true);
        file_health.insert(file.relative_path.clone(), complete);
        session_health
            .entry(file.session_id.clone())
            .and_modify(|healthy| *healthy &= complete);
        match outcome {
            FilePass::Paused => report.files_paused += 1,
            FilePass::Finished(true) => {}
            FilePass::Finished(false) => report.files_incomplete += 1,
        }
    }

    crate::document::stream_document_sources(
        adapter,
        runtime,
        root,
        store,
        &document_files,
        opts,
        instrument,
        &mut ledger,
        &mut report,
        &mut file_health,
        &mut session_health,
    )?;

    crate::snapshot::stream_snapshot_sources(
        adapter,
        runtime,
        root,
        store,
        &snapshot_files,
        opts,
        instrument,
        &mut ledger,
        &mut report,
        &mut file_health,
        &mut session_health,
    )?;

    let mut last_completed_audit = None;
    for path in &audit_paths {
        if file_health.get(path).copied() != Some(true) {
            break;
        }
        last_completed_audit = Some(path.as_str());
        report.files_audited += 1;
    }
    if let Some(path) = last_completed_audit {
        store.set_audit_path(&runtime.id, path)?;
    }

    // The private projection is still unpublished, so relational lifecycle
    // fragments from every source in this runtime can be reconciled together
    // in RAM. Emit each closed tool row once before readiness is evaluated.
    if store.is_bulk_rebuild() {
        let reconciled = instrument.flush_bulk_reconciliation(store)?;
        merge_stats(&mut report.stats, &reconciled);
    }

    // A session is complete only when every admitted source file completed a
    // clean generation. This can both promote and downgrade: a torn append or
    // hard parse error must never leave a stale Indexed badge behind.
    for (session_id, complete) in session_health {
        let next = if complete {
            StreamState::Indexed
        } else {
            StreamState::Streaming
        };
        if store.set_session_stream_state(&session_id, next)? && complete {
            report.sessions_indexed += 1;
        }
    }
    Ok(report)
}

pub(crate) fn rotating_audit_paths(
    store: &Store,
    runtime_id: &str,
    files: &[&SessionFile],
    limit: usize,
) -> Result<Vec<String>, PipelineError> {
    if limit == 0 || files.is_empty() {
        return Ok(Vec::new());
    }
    let mut paths: Vec<&str> = files
        .iter()
        .map(|file| file.relative_path.as_str())
        .collect();
    paths.sort_unstable();
    paths.dedup();
    let last = queries::audit_path(store.conn(), runtime_id)?;
    let start = last
        .as_deref()
        .and_then(|last| paths.iter().position(|path| *path > last))
        .unwrap_or(0);
    Ok((0..paths.len().min(limit))
        .map(|offset| paths[(start + offset) % paths.len()].to_owned())
        .collect())
}

/// How one pass over one source file ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FilePass {
    /// The whole source was read; `true` means a clean generation is fully
    /// active with no partial suffix (today's completion condition).
    Finished(bool),
    /// A starter budget stopped the pass at a record boundary: the clean
    /// prefix is active (and its generation sealed in the ledger), so the
    /// unread remainder is ordinary append growth for a later pass.
    Paused,
}

/// Load-or-create the cursor row for a file, returning its rowid and the
/// EXACT stored blob (the CAS expectation, decodable or not).
pub(crate) fn cursor_row(
    adapter: &dyn AgentAdapter,
    store: &mut Store,
    runtime: &RuntimeRow,
    file: &SessionFile,
    instrument: &Instrument,
) -> Result<queries::CursorState, PipelineError> {
    if let Some(existing) = queries::cursor_state(store.conn(), &runtime.id, &file.relative_path)? {
        return Ok(existing);
    }
    let initial = PipeCheckpoint::fresh(
        &file.session_id,
        &file.stream_id,
        adapter.normalizer_version(),
    )
    .encode();
    instrument.apply(
        store,
        &Batch {
            ops: vec![],
            checkpoints: vec![CheckpointUpdate {
                runtime_id: runtime.id.clone(),
                stream: file.stream_id.clone(),
                path: file.relative_path.clone(),
                session_id: file.session_id.clone(),
                expected: None,
                next: initial,
                updated_at: Some(file.modified_ms),
            }],
            source: None,
        },
    )?;
    queries::cursor_state(store.conn(), &runtime.id, &file.relative_path)?
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
    audit_requested: bool,
    mut ledger: Option<&mut rosetta_capture::Ledger>,
) -> Result<FilePass, PipelineError> {
    let path = root.join(&file.relative_path);
    let cursor = cursor_row(adapter, store, runtime, file, instrument)?;
    if cursor.session_id != file.session_id {
        return Err(PipelineError::Config(format!(
            "source {} moved from session {} to {}",
            file.relative_path, cursor.session_id, file.session_id
        )));
    }
    let cursor_id = cursor.id;
    let mut prev_blob = cursor.checkpoint;
    let mut active_generation = cursor.active_generation;
    let mut highest_generation = cursor.highest_generation;

    // An expired blob (undecodable, foreign version, or a cursor row that
    // belongs to a different session id) means full replay from byte zero —
    // deterministic ids make the replay idempotent.
    let resumed = PipeCheckpoint::decode(&prev_blob)
        .filter(|checkpoint| {
            checkpoint.session_id == file.session_id
                && checkpoint.stream_id == file.stream_id
                && checkpoint.normalizer_version == adapter.normalizer_version()
                // An unactivated generation may have process-local tool join
                // state. After restart, replay it from byte zero instead of
                // pretending SQLite was its reconciliation journal.
                && checkpoint.source_generation == active_generation
        })
        .and_then(|checkpoint| {
            adapter
                .restore_state(checkpoint.adapter_state.as_ref())
                .ok()
                .map(|state| (checkpoint, state))
        });
    let mut tail = resumed
        .as_ref()
        .and_then(|(checkpoint, _)| checkpoint.tail_checkpoint());

    let mut ctx = NormalizeCtx::new(runtime.id.as_str(), file.session_id.as_str());
    ctx.native_session_id = Some(file.native_session_id.clone());
    ctx.agent_version = file.agent_version.clone();
    ctx.state = match &resumed {
        Some((_, _)) => None,
        None => adapter.restore_state(None).map_err(PipelineError::Config)?,
    };
    ctx.src_cursor = Some(cursor_id);
    ctx.source_identity = Some(file.relative_path.clone());
    let mut source_generation = highest_generation
        .checked_add(1)
        .ok_or_else(|| PipelineError::Config("source generation overflowed".to_string()))?;
    let mut next_source_sequence = 0_u64;
    let mut generation_clean = true;
    let mut generation_raw_complete = true;
    if let Some((checkpoint, state)) = resumed {
        ctx.next_ordinal = checkpoint.next_ordinal;
        if let Some(last) = checkpoint.cumulative {
            ctx.cumulative = CumulativeUsage::resume(last);
        }
        ctx.state = state;
        if checkpoint.source_generation > 0 {
            source_generation = checkpoint.source_generation;
        }
        next_source_sequence = checkpoint.next_source_sequence;
        generation_clean = checkpoint.generation_clean;
    }

    // A starter budget applies only where pausing cannot regress what a
    // reader already sees: a fresh source's first generation (early Activate
    // turns the unread remainder into ordinary growth) or append growth of
    // the already-active generation. A replacement generation ignores it so
    // the previous complete content stays visible until the full swap.
    let mut budget = opts
        .starter_budget
        .filter(|_| active_generation == 0 || source_generation == active_generation);
    let budget_tailer = match budget {
        Some(slice) => Some(Tailer::new(TailConfig {
            // A soft per-batch byte cap sizes budgeted batches to the slice
            // itself — one transaction per slice in the common case — while
            // the ordinary record and hard-byte bounds stay untouched, so a
            // large valid record still frames instead of going oversized.
            soft_batch_bytes: Some(
                usize::try_from(slice.max_bytes)
                    .unwrap_or(usize::MAX)
                    .max(1),
            ),
            ..opts.tail.clone()
        })?),
        None => None,
    };
    let mut budget_bytes = 0_u64;
    let mut budget_records = 0_u64;

    let mut retries = 0_u32;
    let mut force_replay =
        audit_requested && tail.is_some() && source_generation == active_generation;
    loop {
        // Chosen per iteration: a mid-pass rewrite that disables the budget
        // must also stop paying its batch shape for the replacement replay.
        let read_tailer = match (&budget, &budget_tailer) {
            (Some(_), Some(budgeted)) => budgeted,
            _ => tailer,
        };
        let read = instrument.time(Metric::ReadNanos, || {
            read_tailer.read(&path, tail.as_ref(), force_replay)
        })?;
        let batch = match read {
            TailRead::Missing => {
                report.files_missing += 1;
                return Ok(FilePass::Finished(false));
            }
            TailRead::Retry => {
                retries += 1;
                if retries > opts.max_retries {
                    report.retries_exhausted += 1;
                    return Ok(FilePass::Finished(false));
                }
                continue;
            }
            TailRead::Batch(batch) => batch,
        };
        force_replay = false;
        instrument.add(Metric::BytesRead, batch.bytes_read);

        if batch.transition.starts_new_generation()
            && !matches!(batch.transition, Transition::Initial)
        {
            if active_generation > 0 {
                // A rewrite mid-pass is a replacement generation: pausing it
                // early would hide previously complete content, so the
                // budget stops applying.
                budget = None;
            }
            // The replacement generation starts its own budget arithmetic;
            // carrying the abandoned generation's counts could pause it
            // below the documented record floor.
            budget_bytes = 0;
            budget_records = 0;
            report.generations_restarted += 1;
            source_generation = source_generation
                .max(active_generation)
                .max(highest_generation)
                .checked_add(1)
                .ok_or_else(|| PipelineError::Config("source generation overflowed".to_string()))?;
            highest_generation = source_generation;
            next_source_sequence = 0;
            generation_clean = true;
            generation_raw_complete = true;
            ctx.next_ordinal = 0;
            ctx.cumulative = CumulativeUsage::new();
            ctx.state = adapter.restore_state(None).map_err(PipelineError::Config)?;
        }

        let mut ops: Vec<IngestOp> = Vec::with_capacity(batch.items.len());
        for item in &batch.items {
            match item {
                TailItem::Record(record) => {
                    report.records += 1;
                    budget_records += 1;
                    // The budget counts FRAMED PAYLOAD, mirroring the soft
                    // batch cap — physical amplification (anchor and
                    // continuity re-reads) must not spend the slice.
                    budget_bytes = budget_bytes.saturating_add(record.payload.len() as u64);
                    instrument.add(Metric::RecordsFramed, 1);
                    ctx.src_offset = Some(record.start_offset as i64);
                    ctx.source_generation = source_generation;
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
                            generation_clean = false;
                            report.hard_errors += 1;
                            instrument.add(Metric::LinesHardError, 1);
                        }
                    }
                    instrument.observe_normalization(
                        &file.stream_id,
                        ctx.src_offset,
                        &record.payload,
                        &outcome,
                    );
                    ops.extend(outcome.ops);
                }
                TailItem::Oversized {
                    start_offset,
                    payload_len,
                    ..
                } => {
                    generation_clean = false;
                    generation_raw_complete = false;
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
        let adapter_state = adapter
            .checkpoint_state(ctx.state.as_deref())
            .map_err(PipelineError::Config)?;
        let first_sequence = next_source_sequence;
        next_source_sequence = next_source_sequence
            .checked_add(ops.len() as u64)
            .ok_or_else(|| PipelineError::Config("source sequence overflowed".to_string()))?;
        let complete = !batch.more_available && batch.checkpoint.incomplete_suffix_len == 0;
        // The slice boundary: enough clean bytes and complete records are
        // framed that this pass may stop. A fresh generation stops by
        // ACTIVATING and sealing its prefix, so the unread remainder becomes
        // ordinary append growth — indistinguishable from the file having
        // ended here and then grown.
        let budget_stop = !complete
            && generation_clean
            && generation_raw_complete
            && budget.is_some_and(|slice| {
                budget_bytes >= slice.max_bytes && budget_records >= u64::from(slice.min_records)
            });
        let source_mode = if source_generation == active_generation {
            SourceMode::Append
        } else if (complete || budget_stop) && generation_clean {
            SourceMode::Activate
        } else if complete && active_generation == 0 && generation_raw_complete {
            // The live-growth rule, converged (product decision 2026-08-31):
            // a FIRST generation that reaches EOF unclean still activates its
            // parseable rows and the session holds Streaming — exactly what
            // the sliced path and live growth already produce. A dirty
            // REPLACEMENT still abandons so previously clean content
            // survives, and a raw-incomplete (oversized) generation still
            // abandons because an unsealable generation must never activate.
            SourceMode::Activate
        } else if complete {
            SourceMode::Abandon
        } else {
            SourceMode::Stage
        };
        let next_blob = PipeCheckpoint {
            v: crate::checkpoint::PIPE_CHECKPOINT_VERSION,
            session_id: file.session_id.clone(),
            stream_id: file.stream_id.clone(),
            normalizer_version: adapter.normalizer_version(),
            source_generation,
            next_source_sequence,
            generation_clean,
            next_ordinal: ctx.next_ordinal,
            tail: Some(batch.checkpoint.encode_hex()),
            cumulative: (!last_reading.is_empty()).then_some(last_reading),
            adapter_state,
            snapshot_stamp: None,
            snapshot_revision: None,
        }
        .encode();

        // Nothing new and nothing to move the cursor to: don't write an
        // empty transaction on every re-scan of a quiet file.
        if ops.is_empty() && next_blob == prev_blob && !batch.more_available {
            break;
        }

        // Ledger-first: the raw bytes are durable before the projection tx
        // may reference them. Oversized records are quarantined by the
        // tailer and carry no payload, so only ordinary records land here.
        if let Some(ledger) = ledger.as_deref_mut() {
            instrument.ledger_write(ledger, |ledger| -> Result<(), PipelineError> {
                let source = rosetta_capture::SourceDef {
                    relative_path: file.relative_path.clone(),
                    stream_id: file.stream_id.clone(),
                    session_id: file.session_id.clone(),
                    native_session_id: file.native_session_id.clone(),
                    agent_version: file.agent_version.clone(),
                };
                let raw: Vec<(u64, &[u8])> = batch
                    .items
                    .iter()
                    .filter_map(|item| match item {
                        TailItem::Record(record) => {
                            Some((record.start_offset, record.payload.as_slice()))
                        }
                        TailItem::Oversized { .. } => None,
                    })
                    .collect();
                let mut wrote = false;
                if !raw.is_empty() {
                    ledger.append_raw(&source, source_generation, raw)?;
                    wrote = true;
                }
                if source_generation != active_generation
                    && (complete || budget_stop)
                    && generation_raw_complete
                {
                    ledger.complete_generation(
                        &source,
                        source_generation,
                        adapter.normalizer_version(),
                        generation_clean,
                    )?;
                    wrote = true;
                }
                if wrote {
                    ledger.checkpoint()?;
                }
                Ok(())
            })?;
        }

        let ops_count = ops.len() as u64;
        let write_batch = Batch {
            ops,
            checkpoints: vec![CheckpointUpdate {
                runtime_id: runtime.id.clone(),
                stream: file.stream_id.clone(),
                path: file.relative_path.clone(),
                session_id: file.session_id.clone(),
                expected: Some(prev_blob.clone()),
                next: next_blob.clone(),
                updated_at: Some(file.modified_ms),
            }],
            source: Some(SourceWrite {
                cursor_id,
                session_id: file.session_id.clone(),
                generation: source_generation,
                first_sequence,
                mode: source_mode,
            }),
        };
        instrument.observe_write_batch(&write_batch);
        let applied = instrument.apply(store, &write_batch);
        let stats = match applied {
            Ok(stats) => stats,
            Err(StoreError::CheckpointConflict { .. }) => {
                // Another writer owns this file now; abandon it untouched.
                report.conflicts += 1;
                instrument.add(Metric::CheckpointConflicts, 1);
                return Ok(FilePass::Finished(false));
            }
            Err(error) => return Err(error.into()),
        };
        merge_stats(&mut report.stats, &stats);
        report.batches += 1;
        instrument.add(Metric::OpsEmitted, ops_count);
        instrument.add(Metric::BatchesApplied, 1);

        prev_blob = next_blob;
        if source_mode == SourceMode::Activate {
            active_generation = source_generation;
        }
        highest_generation = highest_generation.max(source_generation);
        tail = Some(batch.checkpoint);
        if budget_stop {
            // The prefix is active and its checkpoint durable; the rest of
            // this file is a later completion pass's work.
            return Ok(FilePass::Paused);
        }
        if !batch.more_available {
            // An incomplete trailing record stays unconsumed until the file
            // grows; looping now would re-frame the same bytes forever.
            break;
        }
    }
    Ok(FilePass::Finished(
        generation_clean
            && active_generation == source_generation
            && tail
                .as_ref()
                .is_some_and(|checkpoint| checkpoint.incomplete_suffix_len == 0),
    ))
}

pub(crate) fn merge_stats(total: &mut ApplyStats, batch: &ApplyStats) {
    total.merge_from(batch);
}
