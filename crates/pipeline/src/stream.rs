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

use rosetta_adapter_support::AgentAdapter;
use rosetta_capture::Tailer;
use rosetta_core::rows::{RuntimeRow, StreamState};
use rosetta_store::{
    ApplyStats, Batch, CheckpointUpdate, SourceMode, SourceWrite, Store, StoreError, queries,
};

use crate::checkpoint::PipeCheckpoint;
use crate::instrument::{Instrument, Metric};
use crate::meta::SessionFile;
use crate::{IngestOptions, PipelineError};

mod pass;

pub(crate) use pass::{IngestPass, PassStep};

#[derive(Debug, Default)]
pub struct StreamReport {
    /// File PASSES, not distinct files: a budgeted source contributes one
    /// per slice, so compare against roster sizes, never against this.
    pub files: usize,
    pub files_missing: usize,
    pub batches: u64,
    pub records: u64,
    /// Framed payload bytes across those records — the progress-meter unit.
    /// Physical reads (anchor verification, snapshot copies, audit chain
    /// re-reads) amplify `BytesRead` past any census figure; payload never
    /// exceeds the source's raw size, so meters against census bytes hold.
    pub payload_bytes: u64,
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
        self.payload_bytes += part.payload_bytes;
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
    // Snapshot-driven virtual sources take their own path: no tailing, and
    // no rotating audit lane — any pass carrying an audit budget hashes
    // their full content (audit by construction), while audit-free passes
    // may trust a stamp their own checkpoint already ingested.
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
        opts.check_cancelled()?;
        report.files += 1;
        let restarts_before = report.generations_restarted;
        // An audited file VERIFIES its committed content chain by re-reading
        // it, and replays — a replacement generation, a projection sweep,
        // the file re-appended to the ledger — only when the chain actually
        // mismatches. The old audit forced that replay unconditionally,
        // paying all of it for every quiet file on every rotation, forever.
        let mode = if audit_set.contains(file.relative_path.as_str()) {
            PassMode::Audit
        } else {
            PassMode::Normal
        };
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
            mode,
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
        opts.check_cancelled()?;
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
            PassMode::ForceHeal,
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

    opts.check_cancelled()?;
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

    opts.check_cancelled()?;
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

    if let Some(progress) = &opts.progress {
        progress.reading_finished();
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

/// What kind of pass [`ingest_file`] is running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PassMode {
    /// Read whatever the checkpoint says is new.
    Normal,
    /// The rotating audit: verify the committed content chain by re-reading
    /// it, escalate to a replacement replay ONLY on a mismatch, and
    /// otherwise behave exactly like a normal pass (which, on a verified
    /// quiet file, reads nothing and writes nothing).
    Audit,
    /// Sibling healing after a generation restart: force a full replay so
    /// cross-file merges re-derive into the rebuilt rows.
    ForceHeal,
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
                session_id: file.projected_session_id(),
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

/// Reserve the highest raw generation observed without a matching seal during
/// restartable cold capture. This is process-local recovery control: it writes
/// no canonical payload and merely advances the disposable projection's cursor
/// floor so rereading the source allocates a strictly newer generation. The
/// caller must pass a roster entry settled by current metadata.
pub fn reserve_unsealed_generation(
    adapter: &dyn AgentAdapter,
    runtime: &RuntimeRow,
    store: &mut Store,
    file: &SessionFile,
    generation: u64,
    instrument: &Instrument,
) -> Result<bool, PipelineError> {
    if generation == 0 {
        return Err(PipelineError::Config(
            "cannot reserve source generation zero".to_string(),
        ));
    }
    let cursor = cursor_row(adapter, store, runtime, file, instrument)?;
    if cursor.highest_generation >= generation {
        return Ok(false);
    }
    instrument.apply(
        store,
        &Batch {
            ops: vec![],
            checkpoints: vec![],
            source: Some(SourceWrite {
                cursor_id: cursor.id,
                session_id: file.session_id.clone(),
                generation,
                first_sequence: 0,
                mode: SourceMode::Stage,
            }),
        },
    )?;
    Ok(true)
}

/// One pass over one JSONL source, driven serially: each batch
/// [`IngestPass::step`] produces is appended to the raw ledger, then applied
/// to the store under the cursor's CAS, then committed to the pass.
#[allow(clippy::too_many_arguments)]
pub(crate) fn ingest_file(
    adapter: &dyn AgentAdapter,
    runtime: &RuntimeRow,
    root: &Path,
    store: &mut Store,
    file: &SessionFile,
    tailer: &Tailer,
    opts: &IngestOptions,
    instrument: &Instrument,
    report: &mut StreamReport,
    mode: PassMode,
    mut ledger: Option<&mut rosetta_capture::Ledger>,
) -> Result<FilePass, PipelineError> {
    let cursor = cursor_row(adapter, store, runtime, file, instrument)?;
    let mut pass = IngestPass::begin(adapter, runtime, root, file, opts, cursor, mode)?;
    if mode == PassMode::Audit {
        pass.audit(runtime, file, tailer, store, instrument, report)?;
    }
    loop {
        let output = match pass.step(adapter, runtime, file, tailer, opts, instrument)? {
            PassStep::Batch(output) => output,
            PassStep::Finished(outcome, deltas) => {
                deltas.merge_into(report);
                return Ok(outcome);
            }
        };
        output.deltas.merge_into(report);

        // Ledger-first: the raw bytes are durable before the projection tx
        // may reference them.
        if let Some(ledger) = ledger.as_deref_mut() {
            let raw = output.raw;
            instrument.ledger_write(ledger, |ledger| -> Result<(), PipelineError> {
                let source = rosetta_capture::SourceDef {
                    ledger_only: file.ledger_only,
                    relative_path: file.relative_path.clone(),
                    stream_id: file.stream_id.clone(),
                    session_id: file.session_id.clone(),
                    native_session_id: file.native_session_id.clone(),
                    agent_version: file.agent_version.clone(),
                };
                let mut wrote = false;
                if !raw.records.is_empty() {
                    ledger.append_raw_owned(&source, raw.generation, raw.records)?;
                    wrote = true;
                }
                if let Some(clean) = raw.seal {
                    ledger.complete_generation(
                        &source,
                        raw.generation,
                        adapter.normalizer_version(),
                        clean,
                    )?;
                    wrote = true;
                }
                if wrote {
                    ledger.checkpoint()?;
                }
                Ok(())
            })?;
        }

        let ops_count = output.batch.ops.len() as u64;
        instrument.observe_write_batch(&output.batch);
        let applied = instrument.apply_owned(store, output.batch);
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
        if let Some(outcome) = pass.committed() {
            return Ok(outcome);
        }
    }
}

pub(crate) fn merge_stats(total: &mut ApplyStats, batch: &ApplyStats) {
    total.merge_from(batch);
}
