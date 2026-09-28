//! The write path: one batch, one transaction. Canonical rows, FTS entries,
//! bucket corrections, drift aggregates, and the checkpoint CAS
//! all commit together — aggregates can never drift from the rows they
//! summarize, and a torn projection write can never exist. Replacement input
//! and late-arrival joins wait only in the Store's process-local RAM buffer.
//! SQLite sees the final canonical rows plus the minimal durable cursor
//! watermark; the raw ledger is the restart/replay truth.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use rusqlite::{OptionalExtension, Transaction, params};

use rosetta_core::IngestOp;
use rosetta_core::rows::{
    DriftFact, FileEditRow, GitCommitRow, Ms, PlanRow, SessionUsageRow, TaskRow, TokenSnapshot,
};
use rosetta_core::{TimelineBody, ToolAction, WorkflowJournalObservation, WorkflowRun};

use crate::StoreError;
use crate::hot::{Effects, MetaEffects, MetaTouch, Mutation, meta_touch};

/// Compare-and-set update for one source file's driver checkpoint.
#[derive(Debug, Clone)]
pub struct CheckpointUpdate {
    pub runtime_id: String,
    pub stream: String,
    pub path: String,
    /// None for a ledger-only source: progress has no canonical session owner.
    pub session_id: Option<String>,
    /// `None` means "this file has no cursor yet" — the insert fails if one
    /// appeared concurrently.
    pub expected: Option<String>,
    pub next: String,
    pub updated_at: Option<Ms>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceMode {
    /// Hold an incomplete replacement generation in process RAM, away from the
    /// published projection. A private first-generation cold load may project
    /// it early because that database has no readers before validation.
    Stage,
    /// Extend the already-active generation and project only newly persisted ops.
    Append,
    /// Fold the RAM-staged ops, flip the active generation, and atomically rebuild.
    Activate,
    /// Publish only final diagnostic drift and discard the
    /// unclean generation without disturbing the last active projection.
    Abandon,
}

#[derive(Debug, Clone)]
pub struct SourceWrite {
    pub cursor_id: i64,
    pub session_id: String,
    pub generation: u64,
    pub first_sequence: u64,
    pub mode: SourceMode,
}

#[derive(Debug, Default, Clone)]
pub struct Batch {
    pub ops: Vec<IngestOp>,
    pub checkpoints: Vec<CheckpointUpdate>,
    pub source: Option<SourceWrite>,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ApplyStats {
    pub projection_assets: u64,
    pub asset_files_written: u64,
    pub asset_files_reused: u64,
    pub asset_bytes: u64,
    pub asset_write_nanos: u64,
    pub messages_inserted: u64,
    pub messages_updated: u64,
    pub tool_results_merged: u64,
    pub tool_results_unmatched: u64,
    pub usage_applied: u64,
    pub usage_unmatched: u64,
    /// Usage of responses copied from another session, dropped or retracted
    /// because an earlier-started session owns them.
    pub usage_copies_dropped: u64,
    pub drift_recorded: u64,
    /// Distinct session/hour/model keys rebuilt after the batch. This remains
    /// bounded by changed buckets, not changed messages.
    pub bucket_rebuilds: u64,
    /// Generation transitions that actually removed a prior source
    /// contribution. A source's first generation takes the empty fast path.
    pub source_sweeps: u64,
    /// Canonical row projection, including generation activation/sweeps and
    /// live bucket repair, but excluding the SQLite transaction commit.
    pub canonical_apply_nanos: u64,
    /// Time spent committing the SQLite transaction after all statements ran.
    pub sqlite_commit_nanos: u64,
    pub profile: ApplyProfile,
    /// Transaction-local cold timestamp maxima; drained before returning stats.
    session_touches: Option<BTreeMap<String, Ms>>,
}

/// Statement-level attribution within [`ApplyStats::canonical_apply_nanos`],
/// plus separately timed cold RAM reconciliation preparation and cleanup.
/// The RAM timers sit outside the canonical SQL timer, inside the pipeline's
/// enclosing apply timer.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ApplyProfile {
    pub message_attempts: u64,
    pub tool_result_attempts: u64,
    pub usage_attempts: u64,
    pub drift_attempts: u64,
    pub other_attempts: u64,
    pub session_touch_attempts: u64,
    pub session_touch_rows: u64,
    pub ordinal_lookups: u64,
    pub session_renumbers: u64,
    pub session_renumber_rows: u64,
    /// Tool correlations folded exactly once in RAM for a private cold build.
    pub bulk_tool_rows_reconciled: u64,
    pub message_lookup_nanos: u64,
    pub message_write_nanos: u64,
    pub tool_result_lookup_nanos: u64,
    pub tool_result_write_nanos: u64,
    pub usage_lookup_nanos: u64,
    pub usage_write_nanos: u64,
    pub session_touch_nanos: u64,
    pub drift_apply_nanos: u64,
    pub other_apply_nanos: u64,
    pub ordinal_lookup_nanos: u64,
    pub session_renumber_nanos: u64,
    pub tool_reconcile_nanos: u64,
    /// Release committed source trees and the temporary reconciliation plan.
    pub tool_reconcile_cleanup_nanos: u64,
}

impl ApplyStats {
    pub fn merge_from(&mut self, batch: &Self) {
        debug_assert!(batch.session_touches.is_none());
        self.projection_assets += batch.projection_assets;
        self.asset_files_written += batch.asset_files_written;
        self.asset_files_reused += batch.asset_files_reused;
        self.asset_bytes += batch.asset_bytes;
        self.asset_write_nanos += batch.asset_write_nanos;
        self.messages_inserted += batch.messages_inserted;
        self.messages_updated += batch.messages_updated;
        self.tool_results_merged += batch.tool_results_merged;
        self.tool_results_unmatched += batch.tool_results_unmatched;
        self.usage_applied += batch.usage_applied;
        self.usage_unmatched += batch.usage_unmatched;
        self.usage_copies_dropped += batch.usage_copies_dropped;
        self.drift_recorded += batch.drift_recorded;
        self.bucket_rebuilds += batch.bucket_rebuilds;
        self.source_sweeps += batch.source_sweeps;
        self.canonical_apply_nanos += batch.canonical_apply_nanos;
        self.sqlite_commit_nanos += batch.sqlite_commit_nanos;
        self.profile.merge_from(&batch.profile);
    }
}

impl ApplyProfile {
    fn merge_from(&mut self, batch: &Self) {
        self.message_attempts += batch.message_attempts;
        self.tool_result_attempts += batch.tool_result_attempts;
        self.usage_attempts += batch.usage_attempts;
        self.drift_attempts += batch.drift_attempts;
        self.other_attempts += batch.other_attempts;
        self.session_touch_attempts += batch.session_touch_attempts;
        self.session_touch_rows += batch.session_touch_rows;
        self.ordinal_lookups += batch.ordinal_lookups;
        self.session_renumbers += batch.session_renumbers;
        self.session_renumber_rows += batch.session_renumber_rows;
        self.bulk_tool_rows_reconciled += batch.bulk_tool_rows_reconciled;
        self.message_lookup_nanos += batch.message_lookup_nanos;
        self.message_write_nanos += batch.message_write_nanos;
        self.tool_result_lookup_nanos += batch.tool_result_lookup_nanos;
        self.tool_result_write_nanos += batch.tool_result_write_nanos;
        self.usage_lookup_nanos += batch.usage_lookup_nanos;
        self.usage_write_nanos += batch.usage_write_nanos;
        self.session_touch_nanos += batch.session_touch_nanos;
        self.drift_apply_nanos += batch.drift_apply_nanos;
        self.other_apply_nanos += batch.other_apply_nanos;
        self.ordinal_lookup_nanos += batch.ordinal_lookup_nanos;
        self.session_renumber_nanos += batch.session_renumber_nanos;
        self.tool_reconcile_nanos += batch.tool_reconcile_nanos;
        self.tool_reconcile_cleanup_nanos += batch.tool_reconcile_cleanup_nanos;
    }
}

fn elapsed_nanos(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

fn profile_other<T>(
    stats: &mut ApplyStats,
    work: impl FnOnce() -> Result<T, StoreError>,
) -> Result<T, StoreError> {
    stats.profile.other_attempts += 1;
    let started = Instant::now();
    let result = work();
    stats.profile.other_apply_nanos = stats
        .profile
        .other_apply_nanos
        .saturating_add(elapsed_nanos(started));
    result
}

fn profile_ordinal_lookup<T>(
    stats: &mut ApplyStats,
    work: impl FnOnce() -> Result<T, StoreError>,
) -> Result<T, StoreError> {
    stats.profile.ordinal_lookups += 1;
    let started = Instant::now();
    let result = work();
    stats.profile.ordinal_lookup_nanos = stats
        .profile
        .ordinal_lookup_nanos
        .saturating_add(elapsed_nanos(started));
    result
}

mod bulk_reconcile;
mod metadata;
mod ram_reconcile;
mod rows;
mod usage_owners;

pub(crate) use bulk_reconcile::{
    BulkReconciliationBuffer, BulkReconciliationDelta, BulkReconciliationPlan, ColdFold,
    defers as defers_relational,
};
pub use bulk_reconcile::{ResolvedSession, WithheldFacts};
use metadata::{apply_project, apply_runtime, apply_session, apply_session_title};
pub(crate) use ram_reconcile::RamReconciliationBuffer;
pub(crate) use rows::{BULK_MAINTENANCE, LIVE_MAINTENANCE, Maintenance};
use rows::{
    BucketKey, apply_message, apply_tool_updates, apply_usage, finalize_source_tools,
    flush_session_touches, mark_bucket, rebuild_bucket, touch_session,
};
pub(crate) use usage_owners::UsageOwners;
use usage_owners::{Claim, OwnerTarget};

pub(crate) fn apply_bulk_reconciliation(
    tx: &Transaction,
    plan: &mut BulkReconciliationPlan,
    preserve_message_ordinals: bool,
    owners: &mut UsageOwners,
) -> Result<ApplyStats, StoreError> {
    bulk_reconcile::apply_plan(tx, plan, preserve_message_ordinals, owners)
}

fn apply_session_usage(
    tx: &Transaction,
    row: &SessionUsageRow,
    src_cursor: Option<i64>,
    stats: &mut ApplyStats,
    maintenance: Maintenance,
    dirty_buckets: &mut BTreeSet<BucketKey>,
) -> Result<(), StoreError> {
    // A sample may name another session than its source (a Codex guardian
    // review attributes its usage to the parent thread). The parent row
    // exists whenever its rollout was captured; otherwise the sample is out
    // of coverage, never a foreign-key failure of the whole batch.
    let known: Option<i64> = tx
        .prepare_cached("SELECT 1 FROM session WHERE id = ?1")?
        .query_row(params![row.session_id], |record| record.get(0))
        .optional()?;
    if known.is_none() {
        stats.usage_unmatched += 1;
        return Ok(());
    }
    let previous = tx
        .prepare_cached(
            "SELECT ts, model FROM session_usage
             WHERE session_id = ?1 AND sample_key = ?2",
        )?
        .query_row(params![row.session_id, row.key], |record| {
            Ok((record.get::<_, Ms>(0)?, record.get::<_, Option<String>>(1)?))
        })
        .optional()?;
    if let Some((ts, model)) = previous {
        mark_bucket(
            dirty_buckets,
            maintenance,
            &row.session_id,
            Some(ts),
            model.as_deref(),
        );
    }
    mark_bucket(
        dirty_buckets,
        maintenance,
        &row.session_id,
        Some(row.ts),
        row.model.as_deref(),
    );
    tx.prepare_cached(
        "INSERT INTO session_usage
           (session_id, sample_key, ts, model, in_tokens, out_tokens,
            cache_r_tokens, cache_w_tokens, think_tokens, src_cursor, scope,
            cache_w1h_tokens, web_searches, service_tier, route, billed_pusd, requests)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)
         ON CONFLICT(session_id, sample_key) DO UPDATE SET
           ts = excluded.ts, model = excluded.model, scope = excluded.scope,
           in_tokens = excluded.in_tokens, out_tokens = excluded.out_tokens,
           cache_r_tokens = excluded.cache_r_tokens,
           cache_w_tokens = excluded.cache_w_tokens,
           think_tokens = excluded.think_tokens,
           cache_w1h_tokens = excluded.cache_w1h_tokens,
           web_searches = excluded.web_searches,
           service_tier = excluded.service_tier,
           route = excluded.route,
           billed_pusd = excluded.billed_pusd,
           requests = excluded.requests,
           src_cursor = excluded.src_cursor",
    )?
    .execute(params![
        row.session_id,
        row.key,
        row.ts,
        row.model,
        row.tokens.input,
        row.tokens.output,
        row.tokens.cache_read,
        row.tokens.cache_write,
        row.tokens.thinking,
        src_cursor,
        row.tokens.scope.as_str(),
        row.tokens.cache_write_1h,
        row.tokens.web_searches,
        row.tokens
            .service_tier
            .map(rosetta_core::ServiceTier::as_str),
        row.tokens.route,
        row.tokens.billed_pico_usd,
        row.tokens.requests,
    ])?;
    touch_session(tx, &row.session_id, Some(row.ts), stats)?;
    stats.usage_applied += 1;
    Ok(())
}

fn apply_file_edit(
    tx: &Transaction,
    row: &FileEditRow,
    src_cursor: Option<i64>,
) -> Result<(), StoreError> {
    let message_id: Option<i64> = match &row.message_timeline_id {
        Some(timeline_id) => tx
            .prepare_cached("SELECT id FROM message WHERE timeline_id = ?1")?
            .query_row(params![timeline_id], |r| r.get(0))
            .optional()?,
        None => None,
    };
    tx.prepare_cached(
        "INSERT INTO file_edit
           (session_id, message_id, path, kind, snapshot_ref, lines_added, lines_removed, at,
            src_cursor)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT DO NOTHING",
    )?
    .execute(params![
        row.session_id,
        message_id,
        row.path,
        row.kind.as_str(),
        row.snapshot_ref,
        row.lines_added,
        row.lines_removed,
        row.at,
        src_cursor,
    ])?;
    Ok(())
}

fn apply_plan(tx: &Transaction, row: &PlanRow, src_cursor: Option<i64>) -> Result<(), StoreError> {
    tx.prepare_cached(
        "INSERT INTO plan (session_id, revision, content, updated_at, src_cursor)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(session_id, revision) DO UPDATE SET
           content = excluded.content, updated_at = excluded.updated_at,
           src_cursor = excluded.src_cursor",
    )?
    .execute(params![
        row.session_id,
        row.revision,
        row.content,
        row.updated_at,
        src_cursor
    ])?;
    Ok(())
}

fn apply_task(tx: &Transaction, row: &TaskRow, src_cursor: Option<i64>) -> Result<(), StoreError> {
    let event_at = row.updated_at.or(row.created_at);
    let started_at = match row.status {
        rosetta_core::TaskStatus::Pending => None,
        rosetta_core::TaskStatus::InProgress => row.started_at.or(event_at),
        rosetta_core::TaskStatus::Completed
        | rosetta_core::TaskStatus::Failed
        | rosetta_core::TaskStatus::Cancelled => row.started_at,
    };
    let terminal_at = match row.status {
        rosetta_core::TaskStatus::Completed
        | rosetta_core::TaskStatus::Failed
        | rosetta_core::TaskStatus::Cancelled => row.terminal_at.or(event_at),
        rosetta_core::TaskStatus::Pending | rosetta_core::TaskStatus::InProgress => None,
    };
    tx.prepare_cached(
        "INSERT INTO task
           (id, session_id, status, content, created_at, updated_at,
            started_at, terminal_at, src_cursor)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(id) DO UPDATE SET
           status = excluded.status, content = excluded.content,
           updated_at = COALESCE(excluded.updated_at, task.updated_at),
           started_at = CASE
             WHEN excluded.status = 'pending' THEN NULL
             WHEN excluded.status = 'in_progress' AND task.status != 'in_progress'
               THEN COALESCE(excluded.started_at, excluded.updated_at, excluded.created_at)
             ELSE COALESCE(task.started_at, excluded.started_at)
           END,
           terminal_at = CASE
             WHEN excluded.status IN ('completed', 'failed', 'cancelled')
                  AND task.status IN ('completed', 'failed', 'cancelled')
               THEN COALESCE(task.terminal_at, excluded.terminal_at, excluded.updated_at)
             WHEN excluded.status IN ('completed', 'failed', 'cancelled')
               THEN COALESCE(excluded.terminal_at, excluded.updated_at)
             ELSE NULL
           END,
           src_cursor = excluded.src_cursor",
    )?
    .execute(params![
        row.id,
        row.session_id,
        row.status.as_str(),
        row.content,
        row.created_at,
        row.updated_at,
        started_at,
        terminal_at,
        src_cursor,
    ])?;
    Ok(())
}

fn apply_workflow_run(
    tx: &Transaction,
    session_id: &str,
    run: &WorkflowRun,
    src_cursor: Option<i64>,
) -> Result<(), StoreError> {
    // Attempts and journal windows are query-owned joins. Never copy them
    // back into the generation-owned sidecar document if a hydrated run is
    // reapplied by a caller.
    let mut stored = run.clone();
    stored.attempts.clear();
    stored.attempt_total_count = 0;
    stored.journal.clear();
    stored.journal_total_count = 0;
    stored.journal_started_count = 0;
    stored.journal_result_count = 0;
    stored
        .validate()
        .map_err(|error| StoreError::ProjectionInvalid {
            message: error.to_string(),
        })?;
    let detail = serde_json::to_string(&stored)?;
    tx.prepare_cached(
        "INSERT INTO workflow_run (run_id, session_id, detail, updated_at, src_cursor)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(session_id, run_id) DO UPDATE SET
           detail = excluded.detail,
           updated_at = excluded.updated_at,
           src_cursor = excluded.src_cursor",
    )?
    .execute(params![
        run.run_id,
        session_id,
        detail,
        run.updated_at,
        src_cursor
    ])?;
    Ok(())
}

fn apply_workflow_journal(
    tx: &Transaction,
    session_id: &str,
    observation: &WorkflowJournalObservation,
    src_cursor: Option<i64>,
) -> Result<(), StoreError> {
    let detail = serde_json::to_string(observation)?;
    tx.prepare_cached(
        "INSERT INTO workflow_journal
           (observation_id, run_id, session_id, detail, src_cursor)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(observation_id) DO UPDATE SET
           run_id = excluded.run_id,
           session_id = excluded.session_id,
           detail = excluded.detail,
           src_cursor = excluded.src_cursor",
    )?
    .execute(params![
        observation.observation_id,
        observation.run_id,
        session_id,
        detail,
        src_cursor,
    ])?;
    Ok(())
}

fn apply_git_commit(
    tx: &Transaction,
    row: &GitCommitRow,
    src_cursor: Option<i64>,
) -> Result<(), StoreError> {
    tx.prepare_cached(
        "INSERT OR IGNORE INTO git_commit (commit_id, session_id, branch, at, src_cursor)
         VALUES (?1, ?2, ?3, ?4, ?5)",
    )?
    .execute(params![
        row.commit_id,
        row.session_id,
        row.branch,
        row.at,
        src_cursor
    ])?;
    Ok(())
}

fn apply_drift(
    tx: &Transaction,
    fact: &DriftFact,
    stats: &mut ApplyStats,
) -> Result<(), StoreError> {
    stats.profile.drift_attempts += 1;
    let started = Instant::now();
    let result = (|| -> Result<(), StoreError> {
        // Cursor rowids depend on source admission order. The durable dedupe
        // identity must not: resolve provenance to the cursor's stable path
        // so private, progressive, and replay projections converge.
        let sample_path = match fact.sample_cursor {
            Some(cursor) => tx
                .prepare_cached("SELECT path FROM ingest_cursor WHERE id = ?1")?
                .query_row(params![cursor], |row| row.get::<_, String>(0))
                .optional()?,
            None => None,
        };
        let event_key = serde_json::to_string(&(
            &fact.runtime_id,
            &fact.agent_version,
            &fact.stream,
            &fact.record_kind,
            &fact.error_code,
            &fact.shape_hash,
            fact.at,
            sample_path,
            fact.sample_offset,
        ))?;
        let inserted = tx
            .prepare_cached("INSERT OR IGNORE INTO drift_seen (event_key) VALUES (?1)")?
            .execute(params![event_key])?;
        if inserted == 0 {
            return Ok(());
        }
        tx.prepare_cached(
            "INSERT INTO drift_agg
               (runtime_id, agent_version, stream, record_kind, error_code, shape_hash,
                count, first_seen, last_seen, sample_cursor, sample_offset)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, ?7, ?7, ?8, ?9)
             ON CONFLICT(runtime_id, agent_version, stream, record_kind, error_code, shape_hash)
             DO UPDATE SET
               count = count + 1,
               first_seen = CASE
                 WHEN first_seen IS NULL THEN excluded.first_seen
                 WHEN excluded.first_seen IS NULL THEN first_seen
                 ELSE MIN(first_seen, excluded.first_seen) END,
               last_seen = CASE
                 WHEN last_seen IS NULL THEN excluded.last_seen
                 WHEN excluded.last_seen IS NULL THEN last_seen
                 ELSE MAX(last_seen, excluded.last_seen) END,
               sample_cursor = COALESCE(sample_cursor, excluded.sample_cursor),
               sample_offset = COALESCE(sample_offset, excluded.sample_offset)",
        )?
        .execute(params![
            fact.runtime_id,
            fact.agent_version,
            fact.stream,
            fact.record_kind,
            fact.error_code,
            fact.shape_hash,
            fact.at,
            fact.sample_cursor,
            fact.sample_offset,
        ])?;
        stats.drift_recorded += 1;
        Ok(())
    })();
    stats.profile.drift_apply_nanos = stats
        .profile
        .drift_apply_nanos
        .saturating_add(elapsed_nanos(started));
    result
}

fn source_state(
    tx: &Transaction,
    cursor_id: i64,
) -> Result<(Option<String>, u64, u64), StoreError> {
    let Some((session_id, generation, highest)) = tx
        .prepare_cached(
            "SELECT session_id, active_generation, highest_generation
             FROM ingest_cursor WHERE id = ?1",
        )?
        .query_row(params![cursor_id], |row| {
            Ok((
                row.get::<_, Option<String>>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })
        .optional()?
    else {
        return Err(StoreError::SourceState {
            cursor_id,
            message: "cursor does not exist".to_string(),
        });
    };
    let generation = u64::try_from(generation).map_err(|_| StoreError::SourceState {
        cursor_id,
        message: "active generation is negative".to_string(),
    })?;
    let highest = u64::try_from(highest).map_err(|_| StoreError::SourceState {
        cursor_id,
        message: "highest generation is negative".to_string(),
    })?;
    Ok((session_id, generation, highest))
}

fn sqlite_u64(cursor_id: i64, label: &str, value: u64) -> Result<i64, StoreError> {
    i64::try_from(value).map_err(|_| StoreError::SourceState {
        cursor_id,
        message: format!("{label} exceeds SQLite's signed integer range"),
    })
}

/// Project an already-active append, a private first generation, or a complete
/// RAM-staged activation. Message ordinals are collision-free while chunks
/// arrive and are canonicalized once the generation activates.
struct DirectProjection<'a> {
    preserve_message_ordinals: bool,
    trust_message_ordinals: bool,
    effects: Option<&'a mut Effects>,
}

#[allow(clippy::too_many_arguments)]
fn project_source_ops(
    tx: &Transaction,
    source: &SourceWrite,
    ops: &[IngestOp],
    stats: &mut ApplyStats,
    maintenance: Maintenance,
    dirty_buckets: &mut BTreeSet<BucketKey>,
    owners: &mut UsageOwners,
    defer_bulk_reconciliation: bool,
    mut projection: DirectProjection<'_>,
) -> Result<(), StoreError> {
    let mut next_ordinals: BTreeMap<String, i64> = BTreeMap::new();
    let mut tool_updates = Vec::new();
    let mut usage_merges = Vec::new();
    for op in ops {
        if defer_bulk_reconciliation && bulk_reconcile::defers(op) {
            continue;
        }
        if matches!(op, IngestOp::ToolUpdate { .. }) {
            tool_updates.push(op);
            continue;
        }
        if matches!(op, IngestOp::Usage { .. }) {
            usage_merges.push(op);
            continue;
        }
        if let IngestOp::Message(row) = op {
            let mut row = Cow::Borrowed(row.as_ref());
            let existing = if projection.preserve_message_ordinals {
                profile_ordinal_lookup(stats, || {
                    Ok(tx
                        .prepare_cached("SELECT ordinal FROM message WHERE timeline_id = ?1")?
                        .query_row(params![row.timeline_id], |record| record.get::<_, i64>(0))
                        .optional()?)
                })?
            } else {
                None
            };
            if !projection.trust_message_ordinals {
                let ordinal = match existing {
                    Some(ordinal) => ordinal,
                    None => {
                        if let Some(next) = next_ordinals.get_mut(&row.session_id) {
                            let ordinal = *next;
                            *next = next.checked_add(1).ok_or_else(|| StoreError::SourceState {
                                cursor_id: source.cursor_id,
                                message: "session ordinal overflowed".to_string(),
                            })?;
                            ordinal
                        } else {
                            let ordinal: i64 = profile_ordinal_lookup(stats, || {
                                Ok(tx
                                    .prepare_cached(
                                        "SELECT COALESCE(MAX(ordinal) + 1, 0)
                                     FROM message WHERE session_id = ?1",
                                    )?
                                    .query_row(params![row.session_id], |record| record.get(0))?)
                            })?;
                            next_ordinals.insert(
                                row.session_id.clone(),
                                ordinal
                                    .checked_add(1)
                                    .ok_or_else(|| StoreError::SourceState {
                                        cursor_id: source.cursor_id,
                                        message: "session ordinal overflowed".to_string(),
                                    })?,
                            );
                            ordinal
                        }
                    }
                };
                if row.ordinal != ordinal {
                    row.to_mut().ordinal = ordinal;
                }
            }
            if let Some(fx) = projection.effects.as_deref_mut() {
                fx.touched_sessions.insert(row.session_id.clone());
            }
            apply_message(
                tx,
                &row,
                stats,
                maintenance,
                dirty_buckets,
                projection.effects.as_deref_mut(),
            )?;
        } else {
            apply_op(
                tx,
                op,
                Some(source.cursor_id),
                stats,
                maintenance,
                dirty_buckets,
                owners,
                projection.effects.as_deref_mut(),
            )?;
        }
    }
    apply_tool_updates(tx, &tool_updates, stats, projection.effects.as_deref_mut())?;
    for op in usage_merges {
        apply_op(
            tx,
            op,
            Some(source.cursor_id),
            stats,
            maintenance,
            dirty_buckets,
            owners,
            projection.effects.as_deref_mut(),
        )?;
    }
    Ok(())
}

/// An initial generation has no old contribution to remove. Every branch is
/// backed by a provenance index so even this guard remains constant-time as
/// the projection grows.
fn cursor_has_projection(tx: &Transaction, cursor_id: i64) -> Result<bool, StoreError> {
    let found: i64 = tx
        .prepare_cached(
            "SELECT
               EXISTS(SELECT 1 FROM message       WHERE src_cursor = ?1 LIMIT 1) OR
               EXISTS(SELECT 1 FROM file_edit     WHERE src_cursor = ?1 LIMIT 1) OR
               EXISTS(SELECT 1 FROM git_commit    WHERE src_cursor = ?1 LIMIT 1) OR
               EXISTS(SELECT 1 FROM task          WHERE src_cursor = ?1 LIMIT 1) OR
               EXISTS(SELECT 1 FROM plan          WHERE src_cursor = ?1 LIMIT 1) OR
               EXISTS(SELECT 1 FROM session_usage WHERE src_cursor = ?1 LIMIT 1) OR
               EXISTS(SELECT 1 FROM workflow_run WHERE src_cursor = ?1 LIMIT 1) OR
               EXISTS(SELECT 1 FROM workflow_journal WHERE src_cursor = ?1 LIMIT 1) OR
               EXISTS(SELECT 1 FROM session       WHERE title_src_cursor = ?1 LIMIT 1)",
        )?
        .query_row(params![cursor_id], |row| row.get(0))?;
    Ok(found != 0)
}

/// Remove one source's projected contribution by provenance. Messages carry
/// `src_cursor` natively; the small tables carry the column added for exactly
/// this sweep. Buckets are untouched here — they are recomputed from the
/// surviving rows after re-projection, so a swap can never leave aggregate
/// residue behind.
fn remove_cursor_projection(
    tx: &Transaction,
    cursor_id: i64,
    maintenance: Maintenance,
    dirty_buckets: &mut BTreeSet<BucketKey>,
) -> Result<(), StoreError> {
    // A ledger-only cursor has no session of its own; its samples name
    // other sessions and its sweep owns no title.
    let session_id: Option<String> = tx
        .prepare_cached("SELECT session_id FROM ingest_cursor WHERE id = ?1")?
        .query_row(params![cursor_id], |row| row.get(0))?;
    if maintenance.buckets {
        let old_samples = {
            let mut statement = tx.prepare_cached(
                "SELECT session_id, ts, model FROM session_usage WHERE src_cursor = ?1",
            )?;
            statement
                .query_map(params![cursor_id], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Ms>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?
        };
        for (session_id, ts, model) in old_samples {
            mark_bucket(
                dirty_buckets,
                maintenance,
                &session_id,
                Some(ts),
                model.as_deref(),
            );
        }
    }
    for sql in [
        "DELETE FROM file_edit WHERE src_cursor = ?1",
        "DELETE FROM git_commit WHERE src_cursor = ?1",
        "DELETE FROM task WHERE src_cursor = ?1",
        "DELETE FROM plan WHERE src_cursor = ?1",
        "DELETE FROM session_usage WHERE src_cursor = ?1",
        "DELETE FROM workflow_journal WHERE src_cursor = ?1",
        "DELETE FROM workflow_run WHERE src_cursor = ?1",
    ] {
        tx.prepare_cached(sql)?.execute(params![cursor_id])?;
    }
    // Sibling-owned edits may link to rows this sweep removes; unlink rather
    // than cascade — the sibling heal pass re-links them on its next replay.
    tx.prepare_cached(
        "UPDATE file_edit SET message_id = NULL
         WHERE message_id IN (SELECT id FROM message WHERE src_cursor = ?1)",
    )?
    .execute(params![cursor_id])?;

    if maintenance.buckets {
        let mut stmt = tx.prepare_cached(
            "SELECT session_id, model, ts
             FROM message WHERE src_cursor = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map(params![cursor_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<Ms>>(2)?,
            ))
        })?;
        let old_rows = rows.collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        for (session_id, model, ts) in &old_rows {
            mark_bucket(
                dirty_buckets,
                maintenance,
                session_id,
                *ts,
                model.as_deref(),
            );
        }
    }
    tx.prepare_cached("DELETE FROM message WHERE src_cursor = ?1")?
        .execute(params![cursor_id])?;
    if let Some(session_id) = session_id {
        tx.prepare_cached(
            "UPDATE session
             SET title = NULL, title_rank = 0, title_at = NULL, title_src_cursor = NULL
             WHERE id = ?1 AND title_src_cursor = ?2",
        )?
        .execute(params![session_id, cursor_id])?;
    }
    Ok(())
}

/// Session-wide presentation ordinals, reassigned deterministically: source
/// path, then byte offset within the source, then insertion order — the same
/// total order the journal replay used to produce. Two phases through
/// negative space so UNIQUE(session_id, ordinal) never sees a transient
/// collision.
pub(crate) fn prepare_renumber_table(tx: &Transaction) -> Result<(), StoreError> {
    tx.execute_batch(
        "CREATE TEMP TABLE IF NOT EXISTS renumber_message (
           message_id INTEGER PRIMARY KEY,
           ordinal INTEGER NOT NULL
         ) WITHOUT ROWID;
         DELETE FROM renumber_message;",
    )?;
    Ok(())
}

pub(crate) fn apply_renumber_table(tx: &Transaction) -> Result<(), StoreError> {
    // Move through negative space so the live UNIQUE(session_id, ordinal)
    // constraint never observes a transient collision.
    tx.prepare_cached(
        "UPDATE message AS m
         SET ordinal = -1 - ranked.ordinal
         FROM renumber_message AS ranked
         WHERE m.id = ranked.message_id",
    )?
    .execute([])?;
    tx.prepare_cached(
        "UPDATE message AS m
         SET ordinal = -1 - m.ordinal
         FROM renumber_message AS ranked
         WHERE m.id = ranked.message_id",
    )?
    .execute([])?;
    tx.prepare_cached("DELETE FROM renumber_message")?
        .execute([])?;
    Ok(())
}

fn renumber_session(tx: &Transaction, session_id: &str) -> Result<u64, StoreError> {
    // Materialize the window once. Putting this ranking subquery directly in
    // UPDATE's scalar expression lets SQLite execute the full window for each
    // outer message, which is quadratic on long real-world sessions.
    prepare_renumber_table(tx)?;
    let rows = tx
        .prepare_cached(
            "INSERT INTO renumber_message (message_id, ordinal)
         SELECT message_id, ranked_ordinal
         FROM (
           SELECT m.id AS message_id, m.ordinal AS current_ordinal,
                  ROW_NUMBER() OVER (
                    ORDER BY COALESCE(c.path, ''), COALESCE(m.src_offset, m.id), m.id
                  ) - 1 AS ranked_ordinal
           FROM message m LEFT JOIN ingest_cursor c ON c.id = m.src_cursor
           WHERE m.session_id = ?1
         )
         WHERE current_ordinal != ranked_ordinal",
        )?
        .execute(params![session_id])? as u64;
    apply_renumber_table(tx)?;
    Ok(rows)
}

/// Canonicalize every private cold-build session in one set pass. Per-source
/// activation only needs collision-free provisional ordinals because no
/// reader can observe a bulk projection before finalization.
pub(crate) fn renumber_all_sessions(tx: &Transaction) -> Result<u64, StoreError> {
    prepare_renumber_table(tx)?;
    let rows = tx.execute(
        "INSERT INTO renumber_message (message_id, ordinal)
         SELECT message_id, ranked_ordinal
         FROM (
           SELECT m.id AS message_id, m.ordinal AS current_ordinal,
                  ROW_NUMBER() OVER (
                    PARTITION BY m.session_id
                    ORDER BY COALESCE(c.path, ''), COALESCE(m.src_offset, m.id), m.id
                  ) - 1 AS ranked_ordinal
           FROM message m LEFT JOIN ingest_cursor c ON c.id = m.src_cursor
         )
         WHERE current_ordinal != ranked_ordinal",
        [],
    )? as u64;
    apply_renumber_table(tx)?;
    Ok(rows)
}

fn profile_renumber_session(
    tx: &Transaction,
    session_id: &str,
    stats: &mut ApplyStats,
) -> Result<(), StoreError> {
    stats.profile.session_renumbers += 1;
    let started = Instant::now();
    let result = renumber_session(tx, session_id);
    stats.profile.session_renumber_nanos = stats
        .profile
        .session_renumber_nanos
        .saturating_add(elapsed_nanos(started));
    stats.profile.session_renumber_rows =
        stats.profile.session_renumber_rows.saturating_add(result?);
    Ok(())
}

/// A first-generation source is projected above the session's current
/// ordinal maximum. That is already canonical when every previously visible
/// source path sorts before this source path; only an out-of-order activation
/// needs the window/sort repair. The serve scheduler normally arranges that
/// fast path, while this guard keeps Store correct for boosts, resumes, and
/// direct callers that activate sources in an arbitrary order.
/// An append writes rows at its own file-local checkpoint ordinals. Once any
/// SIBLING source's messages share the session, a session renumber (the
/// sibling's activation, or the finalize gate) owns the final order and those
/// checkpoint ordinals are stale in either path direction — so the append
/// must re-rank the session before this commit can be read. Single-source
/// sessions, the overwhelmingly common case, skip this entirely.
fn append_needs_renumber(tx: &Transaction, source: &SourceWrite) -> Result<bool, StoreError> {
    Ok(tx
        .prepare_cached(
            "SELECT EXISTS(
               SELECT 1 FROM message
               WHERE session_id = ?2 AND src_cursor != ?1
               LIMIT 1
             )",
        )?
        .query_row(params![source.cursor_id, source.session_id], |row| {
            row.get::<_, bool>(0)
        })?)
}

fn progressive_source_needs_renumber(
    tx: &Transaction,
    source: &SourceWrite,
) -> Result<bool, StoreError> {
    Ok(tx
        .prepare_cached(
            "SELECT EXISTS(
               SELECT 1
               FROM message m
               JOIN ingest_cursor prior ON prior.id = m.src_cursor
               JOIN ingest_cursor current ON current.id = ?1
               WHERE m.session_id = ?2
                 AND m.src_cursor != ?1
                 AND prior.path > current.path
               LIMIT 1
             )",
        )?
        .query_row(params![source.cursor_id, source.session_id], |row| {
            row.get::<_, bool>(0)
        })?)
}

fn set_active_generation(
    tx: &Transaction,
    source: &SourceWrite,
    expected_generation: u64,
) -> Result<(), StoreError> {
    let changed = tx
        .prepare_cached(
            "UPDATE ingest_cursor SET active_generation = ?2
             WHERE id = ?1 AND active_generation = ?3",
        )?
        .execute(params![
            source.cursor_id,
            sqlite_u64(source.cursor_id, "generation", source.generation)?,
            sqlite_u64(source.cursor_id, "active generation", expected_generation,)?,
        ])?;
    if changed != 1 {
        return Err(StoreError::SourceState {
            cursor_id: source.cursor_id,
            message: "active generation changed concurrently".to_string(),
        });
    }
    Ok(())
}

/// Atomically activate the complete generation assembled in RAM: sweep this
/// cursor's old contribution by provenance, project the resolved input, and
/// renumber the session. Drift is additive-only: what was observed stays
/// observed; a rewrite does not un-observe it.
#[allow(clippy::too_many_arguments)]
fn activate_cursor(
    tx: &Transaction,
    source: &SourceWrite,
    staged: &[IngestOp],
    stats: &mut ApplyStats,
    maintenance: Maintenance,
    dirty_buckets: &mut BTreeSet<BucketKey>,
    owners: &mut UsageOwners,
    defer_bulk_reconciliation: bool,
    effects: Option<&mut Effects>,
) -> Result<(), StoreError> {
    if cursor_has_projection(tx, source.cursor_id)? {
        remove_cursor_projection(tx, source.cursor_id, maintenance, dirty_buckets)?;
        stats.source_sweeps += 1;
    }

    // Messages land with collision-free ordinals above the session max; the
    // renumber below assigns the real presentation order. Edits then merge
    // ops trail all rows, exactly as the journal replay ordered them.
    let mut ordinal: i64 = profile_ordinal_lookup(stats, || {
        Ok(tx
            .prepare_cached(
                "SELECT COALESCE(MAX(ordinal) + 1, 0) FROM message WHERE session_id = ?1",
            )?
            .query_row(params![source.session_id], |row| row.get(0))?)
    })?;
    let mut edits = Vec::new();
    let mut tool_updates = Vec::new();
    let mut usage_merges = Vec::new();
    for op in staged {
        if defer_bulk_reconciliation && bulk_reconcile::defers(op) {
            continue;
        }
        match op {
            IngestOp::Message(row) => {
                let mut row = (**row).clone();
                row.ordinal = ordinal;
                ordinal = ordinal
                    .checked_add(1)
                    .ok_or_else(|| StoreError::SourceState {
                        cursor_id: source.cursor_id,
                        message: "session ordinal overflowed".to_string(),
                    })?;
                // Effects are suppressed for every per-row apply in this
                // block: rows carry temporary ordinals until the renumber
                // below, and none of that may leak into the mutation log.
                apply_message(tx, &row, stats, maintenance, dirty_buckets, None)?;
            }
            IngestOp::FileEdit(_) => edits.push(op),
            IngestOp::ToolUpdate { .. } => tool_updates.push(op),
            IngestOp::Usage { .. } => usage_merges.push(op),
            _ => apply_op(
                tx,
                op,
                Some(source.cursor_id),
                stats,
                maintenance,
                dirty_buckets,
                owners,
                None,
            )?,
        }
    }
    for op in edits {
        apply_op(
            tx,
            op,
            Some(source.cursor_id),
            stats,
            maintenance,
            dirty_buckets,
            owners,
            None,
        )?;
    }
    apply_tool_updates(tx, &tool_updates, stats, None)?;
    for op in usage_merges {
        apply_op(
            tx,
            op,
            Some(source.cursor_id),
            stats,
            maintenance,
            dirty_buckets,
            owners,
            None,
        )?;
    }

    if !defer_bulk_reconciliation {
        finalize_source_tools(tx, source.cursor_id, source.generation)?;
    }

    profile_renumber_session(tx, &source.session_id, stats)?;

    // A swap is one mutation-log entry: the sweep marker. Subscribers take a
    // replacement snapshot, which sees the renumbered rows — the mirror still
    // learns precisely which rows to reload from the touch notes.
    if let Some(fx) = effects
        && fx.scope.admits(&source.session_id)
    {
        for op in staged {
            note_touch(fx, op);
        }
        fx.touch_session(&source.session_id);
        fx.push(Mutation::SessionSwept {
            session_id: source.session_id.clone(),
        });
    }
    Ok(())
}

/// Finish an unclean generation without exposing any of its canonical rows.
/// Drift is observation metadata rather than source contribution, so publish
/// those facts before pruning every abandoned staged row for this cursor.
fn abandon_cursor(
    tx: &Transaction,
    ops: &[IngestOp],
    stats: &mut ApplyStats,
) -> Result<(), StoreError> {
    for op in ops {
        if let IngestOp::Drift(fact) = op {
            apply_drift(tx, fact, stats)?;
        }
    }
    Ok(())
}

fn apply_checkpoint(tx: &Transaction, update: &CheckpointUpdate) -> Result<(), StoreError> {
    let changed = match &update.expected {
        None => tx
            .prepare_cached(
                "INSERT INTO ingest_cursor
                   (runtime_id, stream, path, session_id, checkpoint, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(runtime_id, path) DO NOTHING",
            )?
            .execute(params![
                update.runtime_id,
                update.stream,
                update.path,
                update.session_id,
                update.next,
                update.updated_at,
            ])?,
        Some(expected) => tx
            .prepare_cached(
                "UPDATE ingest_cursor
                 SET checkpoint = ?5, stream = ?2, session_id = ?4, updated_at = ?6
                 WHERE runtime_id = ?1 AND path = ?3 AND checkpoint = ?7",
            )?
            .execute(params![
                update.runtime_id,
                update.stream,
                update.path,
                update.session_id,
                update.next,
                update.updated_at,
                expected,
            ])?,
    };
    if changed != 1 {
        return Err(StoreError::CheckpointConflict {
            path: update.path.clone(),
        });
    }
    Ok(())
}

/// Record which mirror-backed rows an op may change. Idempotent set inserts;
/// runs for direct applies AND for staged ops at activation. The
/// classification itself is shared with [`MetaEffects`] so the two
/// granularities can never disagree about what a batch touched.
fn note_touch(fx: &mut Effects, op: &IngestOp) {
    match meta_touch(op) {
        MetaTouch::Runtime(id) => {
            fx.touched_runtimes.insert(id.to_owned());
        }
        MetaTouch::Project(id) => {
            fx.touched_projects.insert(id.to_owned());
        }
        MetaTouch::Session(id) => fx.touch_session(id),
        MetaTouch::None => {}
    }
    if let Some(session_id) = timeline_meta_session(op) {
        fx.touch_timeline_meta(session_id);
    }
}

fn action_affects_timeline_meta(action: &ToolAction) -> bool {
    matches!(
        action,
        ToolAction::Task { .. }
            | ToolAction::Plan { .. }
            | ToolAction::Artifact { .. }
            | ToolAction::Workflow { .. }
    )
}

fn timeline_meta_session(op: &IngestOp) -> Option<&str> {
    match op {
        IngestOp::Task(row) => Some(&row.session_id),
        IngestOp::Plan(row) => Some(&row.session_id),
        IngestOp::WorkflowRun { session_id, .. } | IngestOp::WorkflowJournal { session_id, .. } => {
            Some(session_id)
        }
        IngestOp::Message(row)
            if matches!(
                &row.body,
                TimelineBody::ToolUse { tool } if action_affects_timeline_meta(&tool.action)
            ) =>
        {
            Some(&row.session_id)
        }
        IngestOp::ToolUpdate {
            session_id, patch, ..
        } if [
            patch.request.as_ref().map(|request| &request.action),
            patch.outcome.as_ref().map(|outcome| &outcome.action),
        ]
        .into_iter()
        .flatten()
        .any(action_affects_timeline_meta) =>
        {
            Some(session_id)
        }
        _ => None,
    }
}

#[allow(clippy::too_many_arguments)]
fn apply_op(
    tx: &Transaction,
    op: &IngestOp,
    src_cursor: Option<i64>,
    stats: &mut ApplyStats,
    maintenance: Maintenance,
    dirty_buckets: &mut BTreeSet<BucketKey>,
    owners: &mut UsageOwners,
    mut effects: Option<&mut Effects>,
) -> Result<(), StoreError> {
    if let Some(fx) = effects.as_deref_mut() {
        note_touch(fx, op);
    }
    match op {
        IngestOp::ProjectionAsset { .. } => {}
        IngestOp::Runtime(row) => profile_other(stats, || apply_runtime(tx, row))?,
        IngestOp::Project(row) => profile_other(stats, || apply_project(tx, row))?,
        IngestOp::SessionMeta(row) => profile_other(stats, || apply_session(tx, row))?,
        IngestOp::SessionTitle(row) => {
            profile_other(stats, || apply_session_title(tx, row, src_cursor))?
        }
        IngestOp::Message(row) => {
            apply_message(tx, row, stats, maintenance, dirty_buckets, effects)?
        }
        IngestOp::ToolUpdate { .. } => {
            apply_tool_updates(tx, std::slice::from_ref(&op), stats, effects)?;
            // Unmatched lifecycle noise remains in the raw ledger. Source
            // batches order tool updates after openers, and a generation
            // replay re-derives the resolved row when a source is replaced.
        }
        IngestOp::Usage {
            session_id,
            response,
            ts,
            model,
            tokens,
            identity,
        } => {
            if let Some(identity) = identity
                && !claim_usage(
                    tx,
                    owners,
                    identity,
                    session_id,
                    OwnerTarget::Row(response.clone()),
                    !tokens.is_empty(),
                    stats,
                    maintenance,
                    dirty_buckets,
                    effects.as_deref_mut(),
                )?
            {
                return Ok(());
            }
            let _ = apply_usage(
                tx,
                session_id,
                response,
                *ts,
                model.as_deref(),
                tokens,
                stats,
                maintenance,
                dirty_buckets,
                effects,
            )?;
        }
        IngestOp::SessionUsage(row) => {
            if let Some(identity) = &row.identity
                && !claim_usage(
                    tx,
                    owners,
                    identity,
                    &row.session_id,
                    OwnerTarget::Sample(row.key.clone()),
                    !row.tokens.is_empty(),
                    stats,
                    maintenance,
                    dirty_buckets,
                    effects,
                )?
            {
                return Ok(());
            }
            apply_session_usage(tx, row, src_cursor, stats, maintenance, dirty_buckets)?
        }
        IngestOp::FileEdit(row) => profile_other(stats, || apply_file_edit(tx, row, src_cursor))?,
        IngestOp::Plan(row) => profile_other(stats, || apply_plan(tx, row, src_cursor))?,
        IngestOp::Task(row) => profile_other(stats, || apply_task(tx, row, src_cursor))?,
        IngestOp::WorkflowRun { session_id, run } => profile_other(stats, || {
            apply_workflow_run(tx, session_id, run, src_cursor)
        })?,
        IngestOp::WorkflowJournal {
            session_id,
            observation,
        } => profile_other(stats, || {
            apply_workflow_journal(tx, session_id, observation, src_cursor)
        })?,
        IngestOp::GitCommit(row) => profile_other(stats, || apply_git_commit(tx, row, src_cursor))?,
        IngestOp::Drift(fact) => apply_drift(tx, fact, stats)?,
    }
    Ok(())
}

/// Settle who owns a usage record that carries a response identity
/// (ARCHITECTURE §4.1.3, counting rule 2). Returns whether the record should
/// be applied; a copy of usage another session owns is dropped, and a record
/// from an earlier-started session displaces the current owner, whose usage
/// is retracted here.
#[allow(clippy::too_many_arguments)]
fn claim_usage(
    tx: &Transaction,
    owners: &mut UsageOwners,
    identity: &str,
    session_id: &str,
    target: OwnerTarget,
    carries_usage: bool,
    stats: &mut ApplyStats,
    maintenance: Maintenance,
    dirty_buckets: &mut BTreeSet<BucketKey>,
    effects: Option<&mut Effects>,
) -> Result<bool, StoreError> {
    match owners.claim(tx, identity, session_id, target, carries_usage)? {
        Claim::Own => Ok(true),
        Claim::DropCopy => {
            stats.usage_copies_dropped += 1;
            Ok(false)
        }
        Claim::Displace {
            session_id: displaced,
            target,
        } => {
            stats.usage_copies_dropped += 1;
            match target {
                OwnerTarget::Row(timeline_id) => {
                    apply_usage(
                        tx,
                        &displaced,
                        &timeline_id,
                        0,
                        None,
                        &TokenSnapshot::default(),
                        stats,
                        maintenance,
                        dirty_buckets,
                        effects,
                    )?;
                }
                OwnerTarget::Sample(key) => {
                    retract_sample(tx, &displaced, &key, maintenance, dirty_buckets)?;
                }
            }
            Ok(true)
        }
    }
}

/// Blank a session sample's usage in place: the row keeps its key, time and
/// model, contributes nothing to any rollup, and its hour bucket is redone.
fn retract_sample(
    tx: &Transaction,
    session_id: &str,
    key: &str,
    maintenance: Maintenance,
    dirty_buckets: &mut BTreeSet<BucketKey>,
) -> Result<(), StoreError> {
    let previous = tx
        .prepare_cached(
            "SELECT ts, model FROM session_usage WHERE session_id = ?1 AND sample_key = ?2",
        )?
        .query_row(params![session_id, key], |row| {
            Ok((row.get::<_, Ms>(0)?, row.get::<_, Option<String>>(1)?))
        })
        .optional()?;
    let Some((ts, model)) = previous else {
        return Ok(());
    };
    tx.prepare_cached(
        "UPDATE session_usage
         SET in_tokens = NULL, out_tokens = NULL, cache_r_tokens = NULL,
             cache_w_tokens = NULL, think_tokens = NULL, cache_w1h_tokens = NULL,
             web_searches = NULL, billed_pusd = NULL, requests = NULL
         WHERE session_id = ?1 AND sample_key = ?2",
    )?
    .execute(params![session_id, key])?;
    mark_bucket(
        dirty_buckets,
        maintenance,
        session_id,
        Some(ts),
        model.as_deref(),
    );
    Ok(())
}

/// What this apply is observed at, per granularity. Row-level effects are
/// worth their mutation log only for sessions a reader is watching — the
/// armed [`Effects::scope`] enforces exactly that, so the cold paths below
/// collect for watched sessions and stay silent for the rest. Where a cold
/// path projects provisional ordinals, per-row mutations stay suppressed and
/// the seal emits one sweep marker instead. Metadata effects are ids only,
/// so they stay on for every path that projects an identity row.
pub(crate) struct Observation<'a> {
    pub rows: Option<&'a mut Effects>,
    pub meta: Option<&'a mut MetaEffects>,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_batch(
    tx: &Transaction,
    batch: &Batch,
    maintenance: Maintenance,
    bulk_load: bool,
    progressive_public: bool,
    precomputed_bulk_ordinals: bool,
    owners: &mut UsageOwners,
    observed: Observation<'_>,
) -> Result<ApplyStats, StoreError> {
    let Observation {
        rows: mut effects,
        meta: mut meta_effects,
    } = observed;
    // A private projection has no readers. Keep relational lifecycle fragments
    // out of SQLite until the runtime closes, then fold each correlation once
    // in RAM and write only its final canonical row. Progressive/public and
    // live paths use the Store's process-local reconciliation buffer.
    let defer_bulk_reconciliation = bulk_load && !progressive_public;
    let mut stats = ApplyStats::default();
    if defer_bulk_reconciliation && batch.source.is_some() {
        // Source batches cannot contain SessionMeta, so no intervening
        // metadata write can change the meaning of these timestamp maxima.
        stats.session_touches = Some(BTreeMap::new());
    }
    let mut dirty_buckets = BTreeSet::new();
    match &batch.source {
        None => {
            // The metadata batch: every op here projects directly, so the
            // whole op list is what this transaction may have moved.
            if let Some(fx) = meta_effects.as_deref_mut() {
                for op in &batch.ops {
                    fx.note(op);
                }
            }
            let started = Instant::now();
            let mut tool_updates = Vec::new();
            let mut usage_merges = Vec::new();
            for op in &batch.ops {
                if matches!(op, IngestOp::ToolUpdate { .. }) {
                    tool_updates.push(op);
                    continue;
                }
                if matches!(op, IngestOp::Usage { .. }) {
                    usage_merges.push(op);
                    continue;
                }
                apply_op(
                    tx,
                    op,
                    None,
                    &mut stats,
                    maintenance,
                    &mut dirty_buckets,
                    owners,
                    effects.as_deref_mut(),
                )?;
            }
            apply_tool_updates(tx, &tool_updates, &mut stats, effects.as_deref_mut())?;
            for op in usage_merges {
                apply_op(
                    tx,
                    op,
                    None,
                    &mut stats,
                    maintenance,
                    &mut dirty_buckets,
                    owners,
                    effects.as_deref_mut(),
                )?;
            }
            stats.canonical_apply_nanos += elapsed_nanos(started);
        }
        Some(source) => {
            let (cursor_session, active_generation, highest_generation) =
                source_state(tx, source.cursor_id)?;
            let starts_new_generation = source.generation > highest_generation;
            if let Some(cursor_session) = &cursor_session
                && cursor_session != &source.session_id
            {
                return Err(StoreError::SourceState {
                    cursor_id: source.cursor_id,
                    message: format!(
                        "batch session {} does not match cursor session {cursor_session}",
                        source.session_id
                    ),
                });
            }
            match source.mode {
                SourceMode::Stage | SourceMode::Activate | SourceMode::Abandon
                    if source.generation == active_generation =>
                {
                    return Err(StoreError::SourceState {
                        cursor_id: source.cursor_id,
                        message: "replacement generation is already active".to_string(),
                    });
                }
                SourceMode::Append if source.generation != active_generation => {
                    return Err(StoreError::SourceState {
                        cursor_id: source.cursor_id,
                        message: format!(
                            "append generation {} is not active generation {active_generation}",
                            source.generation
                        ),
                    });
                }
                SourceMode::Stage | SourceMode::Activate | SourceMode::Abandon
                    if source.generation < highest_generation =>
                {
                    return Err(StoreError::SourceState {
                        cursor_id: source.cursor_id,
                        message: format!(
                            "replacement generation {} is below highest generation {highest_generation}",
                            source.generation,
                        ),
                    });
                }
                _ => {}
            }

            if source.mode != SourceMode::Append {
                tx.prepare_cached(
                    "UPDATE ingest_cursor
                     SET highest_generation = MAX(highest_generation, ?2)
                     WHERE id = ?1",
                )?
                .execute(params![
                    source.cursor_id,
                    sqlite_u64(source.cursor_id, "generation", source.generation)?,
                ])?;
            }

            // An internal source has a durable capture cursor but no canonical
            // session. Advance that cursor and admit only what §4.1.3 rule 5
            // lets a hidden session contribute: usage samples for a proven
            // session and diagnostic drift. Every other payload is rejected
            // before it can create rows, titles, or search repair work.
            if cursor_session.is_none() {
                project_ledger_only_source(
                    tx,
                    source,
                    &batch.ops,
                    &mut stats,
                    maintenance,
                    &mut dirty_buckets,
                    owners,
                    bulk_load && active_generation == 0 && source.mode != SourceMode::Append,
                    starts_new_generation,
                    active_generation,
                )?;
                for checkpoint in &batch.checkpoints {
                    apply_checkpoint(tx, checkpoint)?;
                }
                flush_session_touches(tx, &mut stats)?;
                fold_dirty_buckets(tx, dirty_buckets, &mut stats)?;
                return Ok(stats);
            }

            // No installed reader can observe a private cold database, and a
            // source with active_generation=0 has no clean contribution that
            // needs an atomic swap. Project that first generation directly;
            // later rewrites retain the exact live staging semantics below.
            let direct_cold_generation =
                bulk_load && active_generation == 0 && source.mode != SourceMode::Append;

            // Metadata effects follow what this transaction projects, not
            // what the batch carries: a live Stage parks its ops in process
            // RAM and a live Abandon commits only diagnostic drift, so
            // neither can have moved an identity row. Every other path
            // projects its ops, and any sweep along the way rewrote this
            // session's title and activity time. Unlike the row mutations
            // below, these are collected on the cold paths too — that is the
            // whole point of the cheaper granularity.
            if let Some(fx) = meta_effects
                && (direct_cold_generation
                    || matches!(source.mode, SourceMode::Append | SourceMode::Activate))
            {
                for op in &batch.ops {
                    fx.note(op);
                }
                fx.note_session(&source.session_id);
            }

            if direct_cold_generation {
                let started = Instant::now();
                match source.mode {
                    SourceMode::Stage | SourceMode::Activate => {
                        // A retry may supersede an incomplete generation before
                        // it activated. Its direct private contribution is safe
                        // to sweep because no clean generation exists yet. A
                        // watched subscriber re-snapshots past the sweep rather
                        // than holding rows the retry just removed.
                        if starts_new_generation && cursor_has_projection(tx, source.cursor_id)? {
                            remove_cursor_projection(
                                tx,
                                source.cursor_id,
                                maintenance,
                                &mut dirty_buckets,
                            )?;
                            stats.source_sweeps += 1;
                            if progressive_public && let Some(fx) = effects.as_deref_mut() {
                                fx.push(Mutation::SessionSwept {
                                    session_id: source.session_id.clone(),
                                });
                            }
                        }
                        // Per-row effects stay suppressed: rows land with
                        // provisional ordinals until the renumber at the seal,
                        // and none of that may leak into the mutation log. The
                        // seal below emits the one sweep marker a watched
                        // subscriber needs to take a replacement snapshot.
                        project_source_ops(
                            tx,
                            source,
                            &batch.ops,
                            &mut stats,
                            maintenance,
                            &mut dirty_buckets,
                            owners,
                            defer_bulk_reconciliation,
                            DirectProjection {
                                preserve_message_ordinals: false,
                                trust_message_ordinals: precomputed_bulk_ordinals,
                                effects: None,
                            },
                        )?;
                        if source.mode == SourceMode::Activate {
                            if !defer_bulk_reconciliation {
                                finalize_source_tools(tx, source.cursor_id, source.generation)?;
                            }
                            set_active_generation(tx, source, active_generation)?;
                            if progressive_public && progressive_source_needs_renumber(tx, source)?
                            {
                                // This generation is now reader-visible. Put
                                // an out-of-order source in canonical
                                // path/offset order before its Indexed state
                                // commit can be observed. In-order sources
                                // already appended with their final ordinals.
                                profile_renumber_session(tx, &source.session_id, &mut stats)?;
                            }
                            // A private build keeps collision-free provisional
                            // ordinals until one set-based bulk-finalize pass —
                            // and, readerless, emits no markers either.
                            if progressive_public
                                && let Some(fx) = effects.as_deref_mut()
                                && fx.scope.admits(&source.session_id)
                            {
                                for op in &batch.ops {
                                    note_touch(fx, op);
                                }
                                fx.touch_session(&source.session_id);
                                fx.push(Mutation::SessionSwept {
                                    session_id: source.session_id.clone(),
                                });
                            }
                        }
                    }
                    SourceMode::Abandon => {
                        // Earlier chunks may already be in the private
                        // projection. Remove their provenance-owned rows and
                        // retain only the diagnostic facts from this last
                        // chunk. Store prevents this scratch database from
                        // being finalized after an abandon.
                        for op in &batch.ops {
                            if let IngestOp::Drift(fact) = op {
                                apply_drift(tx, fact, &mut stats)?;
                            }
                        }
                        if cursor_has_projection(tx, source.cursor_id)? {
                            remove_cursor_projection(
                                tx,
                                source.cursor_id,
                                maintenance,
                                &mut dirty_buckets,
                            )?;
                            stats.source_sweeps += 1;
                            if progressive_public && let Some(fx) = effects.as_deref_mut() {
                                fx.push(Mutation::SessionSwept {
                                    session_id: source.session_id.clone(),
                                });
                            }
                        }
                    }
                    SourceMode::Append => unreachable!("append excluded from cold generation"),
                }
                stats.canonical_apply_nanos += elapsed_nanos(started);
            } else {
                match source.mode {
                    // Store::apply already retained this generation's ops in
                    // process RAM. Only the cursor watermark commits here.
                    SourceMode::Stage => {}
                    // The hot path projects directly — replay idempotence comes
                    // from deterministic ids and identity indexes, not a journal.
                    SourceMode::Append => {
                        let started = Instant::now();
                        let messages_before = stats.messages_inserted + stats.messages_updated;
                        project_source_ops(
                            tx,
                            source,
                            &batch.ops,
                            &mut stats,
                            maintenance,
                            &mut dirty_buckets,
                            owners,
                            defer_bulk_reconciliation,
                            DirectProjection {
                                preserve_message_ordinals: true,
                                trust_message_ordinals: false,
                                effects: effects.as_deref_mut(),
                            },
                        )?;
                        // A budget-paused sibling's remainder — or live growth
                        // of a multi-source session — appends file-local
                        // ordinals into a session a sibling's activation may
                        // have renumbered. Re-rank before a reader can see the
                        // collision; a private bulk rebuild defers to its one
                        // finalize pass instead. The renumber reorders rows
                        // whose mutations already left this batch, so it also
                        // emits the sweep marker: deltas holding pre-renumber
                        // ordinals are void, subscribers take a replacement
                        // snapshot.
                        if !defer_bulk_reconciliation
                            && stats.messages_inserted + stats.messages_updated > messages_before
                            && append_needs_renumber(tx, source)?
                        {
                            profile_renumber_session(tx, &source.session_id, &mut stats)?;
                            if let Some(fx) = effects.as_deref_mut() {
                                fx.push(Mutation::SessionSwept {
                                    session_id: source.session_id.clone(),
                                });
                            }
                        }
                        stats.canonical_apply_nanos += elapsed_nanos(started);
                    }
                    SourceMode::Activate => {
                        set_active_generation(tx, source, active_generation)?;
                        let started = Instant::now();
                        activate_cursor(
                            tx,
                            source,
                            &batch.ops,
                            &mut stats,
                            maintenance,
                            &mut dirty_buckets,
                            owners,
                            defer_bulk_reconciliation,
                            effects,
                        )?;
                        stats.canonical_apply_nanos += elapsed_nanos(started);
                    }
                    SourceMode::Abandon => {
                        let started = Instant::now();
                        abandon_cursor(tx, &batch.ops, &mut stats)?;
                        stats.canonical_apply_nanos += elapsed_nanos(started);
                    }
                }
            }
        }
    }
    for checkpoint in &batch.checkpoints {
        apply_checkpoint(tx, checkpoint)?;
    }
    flush_session_touches(tx, &mut stats)?;
    fold_dirty_buckets(tx, dirty_buckets, &mut stats)?;
    Ok(stats)
}

/// Recompute every hourly bucket a batch touched.
fn fold_dirty_buckets(
    tx: &Transaction,
    dirty_buckets: BTreeSet<BucketKey>,
    stats: &mut ApplyStats,
) -> Result<(), StoreError> {
    let started = Instant::now();
    for key in dirty_buckets {
        rebuild_bucket(tx, &key.session_id, key.hour_ts, &key.model)?;
        stats.bucket_rebuilds += 1;
    }
    stats.canonical_apply_nanos += elapsed_nanos(started);
    Ok(())
}

/// Project what a session-less source may contribute (ARCHITECTURE §4.1.3,
/// counting rule 5): `session_usage` samples that name a proven session — a
/// Codex guardian review's requests attributed to the thread that ran it —
/// and drift. The samples carry this cursor as provenance, so a replacement
/// generation sweeps them exactly as it sweeps a canonical session's rows.
/// A live Stage parks its ops in RAM upstream and delivers them whole at
/// activation, like any other source; a private cold generation projects its
/// chunks directly and sweeps only when a retry starts the generation over.
#[allow(clippy::too_many_arguments)]
fn project_ledger_only_source(
    tx: &Transaction,
    source: &SourceWrite,
    ops: &[IngestOp],
    stats: &mut ApplyStats,
    maintenance: Maintenance,
    dirty_buckets: &mut BTreeSet<BucketKey>,
    owners: &mut UsageOwners,
    direct_cold_generation: bool,
    starts_new_generation: bool,
    active_generation: u64,
) -> Result<(), StoreError> {
    if ops
        .iter()
        .any(|op| !matches!(op, IngestOp::SessionUsage(_) | IngestOp::Drift(_)))
    {
        return Err(StoreError::SourceState {
            cursor_id: source.cursor_id,
            message: "ledger-only source cannot project operations".to_string(),
        });
    }
    let started = Instant::now();
    let sweep = match source.mode {
        SourceMode::Stage | SourceMode::Append => direct_cold_generation && starts_new_generation,
        SourceMode::Activate => !direct_cold_generation || starts_new_generation,
        SourceMode::Abandon => direct_cold_generation,
    };
    if sweep && cursor_has_projection(tx, source.cursor_id)? {
        remove_cursor_projection(tx, source.cursor_id, maintenance, dirty_buckets)?;
        stats.source_sweeps += 1;
    }
    match source.mode {
        SourceMode::Abandon => abandon_cursor(tx, ops, stats)?,
        SourceMode::Stage | SourceMode::Append | SourceMode::Activate => {
            for op in ops {
                apply_op(
                    tx,
                    op,
                    Some(source.cursor_id),
                    stats,
                    maintenance,
                    dirty_buckets,
                    owners,
                    None,
                )?;
            }
            if source.mode == SourceMode::Activate {
                set_active_generation(tx, source, active_generation)?;
            }
        }
    }
    stats.canonical_apply_nanos += elapsed_nanos(started);
    Ok(())
}
