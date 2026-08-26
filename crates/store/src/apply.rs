//! The write path: one batch, one transaction. Canonical rows, FTS entries,
//! bucket corrections, drift aggregates, and the checkpoint CAS
//! all commit together — aggregates can never drift from the rows they
//! summarize, and a torn projection write can never exist. `source_op` is a
//! STAGING BUFFER, not a representation: live replacements and replacements
//! of an already-active source use it, then prune it at activation. A private
//! cold build projects a source's first generation directly because there is
//! no prior contribution to protect and the whole database remains
//! unpublished until validation. The raw ledger (rosetta-capture) is the
//! durable replay truth; the projection undoes a source's old contribution via
//! provenance columns, never via a permanent op journal.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use rusqlite::{OptionalExtension, Transaction, params};

use rosetta_core::rows::{
    DriftFact, FileEditRow, GitCommitRow, Ms, PlanRow, ProjectRow, TaskRow, WorkspaceObsRow,
};
use rosetta_core::{IngestOp, ids};

use crate::StoreError;
use crate::hot::{Effects, Mutation};

/// Compare-and-set update for one source file's driver checkpoint.
#[derive(Debug, Clone)]
pub struct CheckpointUpdate {
    pub runtime_id: String,
    pub stream: String,
    pub path: String,
    pub session_id: String,
    /// `None` means "this file has no cursor yet" — the insert fails if one
    /// appeared concurrently.
    pub expected: Option<String>,
    pub next: String,
    pub updated_at: Option<Ms>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceMode {
    /// Hold an incomplete replacement generation away from the published
    /// projection. A private first-generation cold load may project it early;
    /// that database has no readers and cannot be published before activation.
    Stage,
    /// Extend the already-active generation and project only newly persisted ops.
    Append,
    /// Persist the final ops, flip the active generation, and atomically rebuild.
    Activate,
    /// Persist the final diagnostic ops, publish only drift, and discard the
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

#[derive(Debug, Default)]
pub struct Batch {
    pub ops: Vec<IngestOp>,
    pub checkpoints: Vec<CheckpointUpdate>,
    pub source: Option<SourceWrite>,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ApplyStats {
    pub messages_inserted: u64,
    pub messages_updated: u64,
    pub tool_results_merged: u64,
    pub tool_results_unmatched: u64,
    pub usage_applied: u64,
    pub usage_unmatched: u64,
    pub drift_recorded: u64,
    /// Distinct session/hour/model keys rebuilt after the batch. This remains
    /// bounded by changed buckets, not changed messages.
    pub bucket_rebuilds: u64,
    /// Generation transitions that actually removed a prior source
    /// contribution. A source's first generation takes the empty fast path.
    pub source_sweeps: u64,
    /// JSON serialization plus writes to the temporary `source_op` staging
    /// table. Zero on ordinary active-generation appends.
    pub source_stage_nanos: u64,
    /// Canonical row projection, including generation activation/sweeps and
    /// live bucket repair, but excluding the SQLite transaction commit.
    pub canonical_apply_nanos: u64,
    /// Time spent committing the SQLite transaction after all statements ran.
    pub sqlite_commit_nanos: u64,
}

fn elapsed_nanos(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

mod rows;

pub(crate) use rows::{BULK_MAINTENANCE, LIVE_MAINTENANCE, Maintenance};
use rows::{
    BucketKey, OldRow, apply_message, apply_project, apply_runtime, apply_session,
    apply_tool_result, apply_usage, fts_delete, mark_bucket, queue_pending, rebuild_bucket,
};

fn apply_workspace_obs(
    tx: &Transaction,
    row: &WorkspaceObsRow,
    src_cursor: Option<i64>,
) -> Result<(), StoreError> {
    let cwd = ids::normalize_root(&row.cwd);
    let project_id = ids::project_id(&cwd, row.git_repo.as_deref());
    apply_project(
        tx,
        &ProjectRow {
            id: project_id.clone(),
            name: ids::project_name(&cwd),
            root_paths: vec![cwd.clone()],
            git_repo: row.git_repo.clone(),
        },
    )?;
    tx.prepare_cached(
        "INSERT INTO workspace_obs (session_id, at, cwd, git_branch, git_repo, src_cursor)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT DO NOTHING",
    )?
    .execute(params![
        row.session_id,
        row.at,
        cwd,
        row.git_branch,
        row.git_repo,
        src_cursor
    ])?;
    tx.prepare_cached(
        "UPDATE session
         SET project_id = ?2, project_assigned_at = ?3
         WHERE id = ?1
           AND (project_assigned_at IS NULL OR ?3 < project_assigned_at)",
    )?
    .execute(params![row.session_id, project_id, row.at])?;
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
    tx.prepare_cached(
        "INSERT INTO task (id, session_id, status, content, created_at, updated_at, src_cursor)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(id) DO UPDATE SET
           status = excluded.status, content = excluded.content,
           updated_at = COALESCE(excluded.updated_at, task.updated_at),
           src_cursor = excluded.src_cursor",
    )?
    .execute(params![
        row.id,
        row.session_id,
        row.status.as_str(),
        row.content,
        row.created_at,
        row.updated_at,
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
    let event_key = serde_json::to_string(fact)?;
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
}

fn source_state(tx: &Transaction, cursor_id: i64) -> Result<(String, u64, u64), StoreError> {
    let Some((session_id, generation, highest)) = tx
        .prepare_cached(
            "SELECT session_id, active_generation, highest_generation
             FROM ingest_cursor WHERE id = ?1",
        )?
        .query_row(params![cursor_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
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

/// Persist each normalized op before it can affect the projection. A repeated
/// key with byte-identical JSON is an at-least-once retry; a different payload
/// at the same source position is a deterministic-normalizer violation.
fn persist_source_ops(
    tx: &Transaction,
    source: &SourceWrite,
    ops: &[IngestOp],
) -> Result<(), StoreError> {
    let generation = sqlite_u64(source.cursor_id, "generation", source.generation)?;
    for (index, op) in ops.iter().enumerate() {
        let sequence = source
            .first_sequence
            .checked_add(index as u64)
            .ok_or_else(|| StoreError::SourceState {
                cursor_id: source.cursor_id,
                message: "source sequence overflowed".to_string(),
            })?;
        let sequence_sql = sqlite_u64(source.cursor_id, "sequence", sequence)?;
        let json = serde_json::to_string(op)?;
        let changed = tx
            .prepare_cached(
                "INSERT INTO source_op (cursor_id, generation, sequence, op_json)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(cursor_id, generation, sequence) DO NOTHING",
            )?
            .execute(params![source.cursor_id, generation, sequence_sql, json])?;
        if changed == 1 {
            continue;
        }
        let existing: String = tx
            .prepare_cached(
                "SELECT op_json FROM source_op
                 WHERE cursor_id = ?1 AND generation = ?2 AND sequence = ?3",
            )?
            .query_row(params![source.cursor_id, generation, sequence_sql], |row| {
                row.get(0)
            })?;
        if existing != json {
            return Err(StoreError::SourceConflict {
                cursor_id: source.cursor_id,
                generation: source.generation,
                sequence,
            });
        }
    }
    Ok(())
}

/// Project a source chunk without serializing it through `source_op`. This is
/// used for active-generation appends and for a source's first generation in
/// a private cold build. Message ordinals are collision-free while chunks
/// arrive and are canonicalized once the generation activates.
struct DirectProjection<'a> {
    preserve_message_ordinals: bool,
    effects: Option<&'a mut Effects>,
}

fn project_source_ops(
    tx: &Transaction,
    source: &SourceWrite,
    ops: &[IngestOp],
    stats: &mut ApplyStats,
    maintenance: Maintenance,
    dirty_buckets: &mut BTreeSet<BucketKey>,
    mut projection: DirectProjection<'_>,
) -> Result<(), StoreError> {
    let mut next_ordinals: BTreeMap<String, i64> = BTreeMap::new();
    for op in ops {
        if let IngestOp::Message(row) = op {
            let mut row = (**row).clone();
            let existing = if projection.preserve_message_ordinals {
                tx.prepare_cached("SELECT ordinal FROM message WHERE timeline_id = ?1")?
                    .query_row(params![row.timeline_id], |record| record.get::<_, i64>(0))
                    .optional()?
            } else {
                None
            };
            row.ordinal = match existing {
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
                        let ordinal: i64 = tx
                            .prepare_cached(
                                "SELECT COALESCE(MAX(ordinal) + 1, 0)
                                 FROM message WHERE session_id = ?1",
                            )?
                            .query_row(params![row.session_id], |record| record.get(0))?;
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
                projection.effects.as_deref_mut(),
            )?;
        }
    }
    Ok(())
}

fn staged_ops(
    tx: &Transaction,
    cursor_id: i64,
    generation: i64,
) -> Result<Vec<IngestOp>, StoreError> {
    let mut stmt = tx.prepare_cached(
        "SELECT op_json FROM source_op
         WHERE cursor_id = ?1 AND generation = ?2 ORDER BY sequence",
    )?;
    let rows = stmt.query_map(params![cursor_id, generation], |row| {
        row.get::<_, String>(0)
    })?;
    let mut ops = Vec::new();
    for row in rows {
        ops.push(serde_json::from_str(&row?)?);
    }
    Ok(ops)
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
               EXISTS(SELECT 1 FROM workspace_obs WHERE src_cursor = ?1 LIMIT 1) OR
               EXISTS(SELECT 1 FROM pending_merge WHERE src_cursor = ?1 LIMIT 1)",
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
    for sql in [
        "DELETE FROM file_edit WHERE src_cursor = ?1",
        "DELETE FROM git_commit WHERE src_cursor = ?1",
        "DELETE FROM task WHERE src_cursor = ?1",
        "DELETE FROM plan WHERE src_cursor = ?1",
        "DELETE FROM workspace_obs WHERE src_cursor = ?1",
        "DELETE FROM pending_merge WHERE src_cursor = ?1",
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

    if maintenance.fts || maintenance.buckets {
        let mut stmt = tx.prepare_cached(
            "SELECT id, timeline_id, session_id, content, tool_input, tool_result, model, ts
             FROM message WHERE src_cursor = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map(params![cursor_id], |row| {
            Ok(OldRow {
                id: row.get(0)?,
                timeline_id: row.get(1)?,
                session_id: row.get(2)?,
                content: row.get(3)?,
                tool_input: row.get(4)?,
                tool_result: row.get(5)?,
                model: row.get(6)?,
                ts: row.get(7)?,
            })
        })?;
        let old_rows = rows.collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        for old in &old_rows {
            if maintenance.fts {
                fts_delete(tx, old)?;
            }
            mark_bucket(
                dirty_buckets,
                maintenance,
                &old.session_id,
                old.ts,
                old.model.as_deref(),
            );
        }
    }
    tx.prepare_cached("DELETE FROM message WHERE src_cursor = ?1")?
        .execute(params![cursor_id])?;
    Ok(())
}

/// Session-wide presentation ordinals, reassigned deterministically: source
/// path, then byte offset within the source, then insertion order — the same
/// total order the journal replay used to produce. Two phases through
/// negative space so UNIQUE(session_id, ordinal) never sees a transient
/// collision.
fn renumber_session(tx: &Transaction, session_id: &str) -> Result<(), StoreError> {
    // Materialize the window once. Putting this ranking subquery directly in
    // UPDATE's scalar expression lets SQLite execute the full window for each
    // outer message, which is quadratic on long real-world sessions.
    tx.execute_batch(
        "CREATE TEMP TABLE IF NOT EXISTS renumber_message (
           message_id INTEGER PRIMARY KEY,
           ordinal INTEGER NOT NULL
         ) WITHOUT ROWID;
         DELETE FROM renumber_message;",
    )?;
    tx.prepare_cached(
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
    .execute(params![session_id])?;
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

/// Activation with a staged-only journal: sweep this cursor's old
/// contribution by provenance, project the staged generation, renumber the
/// session, and PRUNE the journal — `source_op` holds
/// nothing for active generations. Drift is additive-only: what was observed
/// stays observed; a rewrite does not un-observe it.
fn activate_cursor(
    tx: &Transaction,
    source: &SourceWrite,
    stats: &mut ApplyStats,
    maintenance: Maintenance,
    dirty_buckets: &mut BTreeSet<BucketKey>,
    effects: Option<&mut Effects>,
) -> Result<(), StoreError> {
    let generation = sqlite_u64(source.cursor_id, "generation", source.generation)?;
    let staged = staged_ops(tx, source.cursor_id, generation)?;
    if cursor_has_projection(tx, source.cursor_id)? {
        remove_cursor_projection(tx, source.cursor_id, maintenance, dirty_buckets)?;
        stats.source_sweeps += 1;
    }

    // Messages land with collision-free ordinals above the session max; the
    // renumber below assigns the real presentation order. Edits then merge
    // ops trail all rows, exactly as the journal replay ordered them.
    let mut ordinal: i64 = tx
        .prepare_cached("SELECT COALESCE(MAX(ordinal) + 1, 0) FROM message WHERE session_id = ?1")?
        .query_row(params![source.session_id], |row| row.get(0))?;
    let mut edits = Vec::new();
    let mut merges = Vec::new();
    for op in &staged {
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
            IngestOp::ToolResult { .. } | IngestOp::Usage { .. } => merges.push(op),
            _ => apply_op(
                tx,
                op,
                Some(source.cursor_id),
                stats,
                maintenance,
                dirty_buckets,
                None,
            )?,
        }
    }
    for op in edits.into_iter().chain(merges) {
        apply_op(
            tx,
            op,
            Some(source.cursor_id),
            stats,
            maintenance,
            dirty_buckets,
            None,
        )?;
    }

    renumber_session(tx, &source.session_id)?;
    tx.prepare_cached("DELETE FROM source_op WHERE cursor_id = ?1")?
        .execute(params![source.cursor_id])?;

    // A swap is one mutation-log entry: the sweep marker. Subscribers take a
    // replacement snapshot, which sees the renumbered rows — the mirror still
    // learns precisely which rows to reload from the touch notes.
    if let Some(fx) = effects {
        for op in &staged {
            note_touch(fx, op);
        }
        fx.touched_sessions.insert(source.session_id.clone());
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
    source: &SourceWrite,
    stats: &mut ApplyStats,
) -> Result<(), StoreError> {
    let generation = sqlite_u64(source.cursor_id, "generation", source.generation)?;
    for op in staged_ops(tx, source.cursor_id, generation)? {
        if let IngestOp::Drift(fact) = op {
            apply_drift(tx, &fact, stats)?;
        }
    }
    tx.prepare_cached("DELETE FROM source_op WHERE cursor_id = ?1")?
        .execute(params![source.cursor_id])?;
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
/// runs for direct applies AND for staged ops at activation.
fn note_touch(fx: &mut Effects, op: &IngestOp) {
    match op {
        IngestOp::Runtime(row) => {
            fx.touched_runtimes.insert(row.id.clone());
        }
        IngestOp::Project(row) => {
            fx.touched_projects.insert(row.id.clone());
        }
        IngestOp::SessionMeta(row) => {
            fx.touched_sessions.insert(row.id.clone());
        }
        // These touch the session row (`updated_at`, and for workspace obs
        // the project link plus a possibly-new project row).
        IngestOp::Message(row) => {
            fx.touched_sessions.insert(row.session_id.clone());
        }
        IngestOp::ToolResult { session_id, .. } | IngestOp::Usage { session_id, .. } => {
            fx.touched_sessions.insert(session_id.clone());
        }
        IngestOp::WorkspaceObs(row) => {
            let cwd = ids::normalize_root(&row.cwd);
            fx.touched_projects
                .insert(ids::project_id(&cwd, row.git_repo.as_deref()));
            fx.touched_sessions.insert(row.session_id.clone());
        }
        // No session/project/runtime row changes.
        IngestOp::FileEdit(_)
        | IngestOp::Plan(_)
        | IngestOp::Task(_)
        | IngestOp::GitCommit(_)
        | IngestOp::Drift(_) => {}
    }
}

fn apply_op(
    tx: &Transaction,
    op: &IngestOp,
    src_cursor: Option<i64>,
    stats: &mut ApplyStats,
    maintenance: Maintenance,
    dirty_buckets: &mut BTreeSet<BucketKey>,
    mut effects: Option<&mut Effects>,
) -> Result<(), StoreError> {
    if let Some(fx) = effects.as_deref_mut() {
        note_touch(fx, op);
    }
    match op {
        IngestOp::Runtime(row) => apply_runtime(tx, row)?,
        IngestOp::Project(row) => apply_project(tx, row)?,
        IngestOp::SessionMeta(row) => apply_session(tx, row)?,
        IngestOp::Message(row) => {
            apply_message(tx, row, stats, maintenance, dirty_buckets, effects)?
        }
        IngestOp::ToolResult {
            session_id,
            tool_id,
            result,
            payload,
            is_error,
            at,
        } => {
            if !apply_tool_result(
                tx,
                session_id,
                tool_id,
                result.as_deref(),
                payload.as_ref(),
                *is_error,
                *at,
                stats,
                maintenance,
                effects,
            )? {
                queue_pending(tx, op, src_cursor)?;
            }
        }
        IngestOp::Usage {
            session_id,
            response,
            ts,
            model,
            tokens,
        } => {
            if !apply_usage(
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
            )? {
                queue_pending(tx, op, src_cursor)?;
            }
        }
        IngestOp::WorkspaceObs(row) => apply_workspace_obs(tx, row, src_cursor)?,
        IngestOp::FileEdit(row) => apply_file_edit(tx, row, src_cursor)?,
        IngestOp::Plan(row) => apply_plan(tx, row, src_cursor)?,
        IngestOp::Task(row) => apply_task(tx, row, src_cursor)?,
        IngestOp::GitCommit(row) => apply_git_commit(tx, row, src_cursor)?,
        IngestOp::Drift(fact) => apply_drift(tx, fact, stats)?,
    }
    Ok(())
}

pub(crate) fn apply_batch(
    tx: &Transaction,
    batch: &Batch,
    maintenance: Maintenance,
    bulk_load: bool,
    mut effects: Option<&mut Effects>,
) -> Result<ApplyStats, StoreError> {
    let mut stats = ApplyStats::default();
    let mut dirty_buckets = BTreeSet::new();
    match &batch.source {
        None => {
            let started = Instant::now();
            for op in &batch.ops {
                apply_op(
                    tx,
                    op,
                    None,
                    &mut stats,
                    maintenance,
                    &mut dirty_buckets,
                    effects.as_deref_mut(),
                )?;
            }
            stats.canonical_apply_nanos += elapsed_nanos(started);
        }
        Some(source) => {
            let (cursor_session, active_generation, highest_generation) =
                source_state(tx, source.cursor_id)?;
            let starts_new_generation = source.generation > highest_generation;
            if cursor_session != source.session_id {
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

            // No installed reader can observe a private cold database, and a
            // source with active_generation=0 has no clean contribution that
            // needs an atomic swap. Project that first generation directly;
            // later rewrites retain the exact live staging semantics below.
            let direct_cold_generation =
                bulk_load && active_generation == 0 && source.mode != SourceMode::Append;
            if direct_cold_generation {
                let started = Instant::now();
                match source.mode {
                    SourceMode::Stage | SourceMode::Activate => {
                        // A retry may supersede an incomplete generation before
                        // it activated. Its direct private contribution is safe
                        // to sweep because no clean generation exists yet.
                        if starts_new_generation && cursor_has_projection(tx, source.cursor_id)? {
                            remove_cursor_projection(
                                tx,
                                source.cursor_id,
                                maintenance,
                                &mut dirty_buckets,
                            )?;
                            stats.source_sweeps += 1;
                        }
                        // Effects are intentionally suppressed: this database
                        // is not observable until it has been finalized.
                        project_source_ops(
                            tx,
                            source,
                            &batch.ops,
                            &mut stats,
                            maintenance,
                            &mut dirty_buckets,
                            DirectProjection {
                                preserve_message_ordinals: false,
                                effects: None,
                            },
                        )?;
                        if source.mode == SourceMode::Activate {
                            set_active_generation(tx, source, active_generation)?;
                            renumber_session(tx, &source.session_id)?;
                            tx.prepare_cached("DELETE FROM source_op WHERE cursor_id = ?1")?
                                .execute(params![source.cursor_id])?;
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
                        }
                        tx.prepare_cached("DELETE FROM source_op WHERE cursor_id = ?1")?
                            .execute(params![source.cursor_id])?;
                    }
                    SourceMode::Append => unreachable!("append excluded from cold generation"),
                }
                stats.canonical_apply_nanos += elapsed_nanos(started);
            } else {
                match source.mode {
                    // Staged generations are the ONLY thing the journal holds.
                    SourceMode::Stage => {
                        let started = Instant::now();
                        persist_source_ops(tx, source, &batch.ops)?;
                        stats.source_stage_nanos += elapsed_nanos(started);
                    }
                    // The hot path projects directly — replay idempotence comes
                    // from deterministic ids and identity indexes, not a journal.
                    SourceMode::Append => {
                        let started = Instant::now();
                        project_source_ops(
                            tx,
                            source,
                            &batch.ops,
                            &mut stats,
                            maintenance,
                            &mut dirty_buckets,
                            DirectProjection {
                                preserve_message_ordinals: true,
                                effects: effects.as_deref_mut(),
                            },
                        )?;
                        stats.canonical_apply_nanos += elapsed_nanos(started);
                    }
                    SourceMode::Activate => {
                        let started = Instant::now();
                        persist_source_ops(tx, source, &batch.ops)?;
                        stats.source_stage_nanos += elapsed_nanos(started);
                        set_active_generation(tx, source, active_generation)?;
                        let started = Instant::now();
                        activate_cursor(
                            tx,
                            source,
                            &mut stats,
                            maintenance,
                            &mut dirty_buckets,
                            effects,
                        )?;
                        stats.canonical_apply_nanos += elapsed_nanos(started);
                    }
                    SourceMode::Abandon => {
                        let started = Instant::now();
                        persist_source_ops(tx, source, &batch.ops)?;
                        stats.source_stage_nanos += elapsed_nanos(started);
                        let started = Instant::now();
                        abandon_cursor(tx, source, &mut stats)?;
                        stats.canonical_apply_nanos += elapsed_nanos(started);
                    }
                }
            }
        }
    }
    for checkpoint in &batch.checkpoints {
        apply_checkpoint(tx, checkpoint)?;
    }
    let started = Instant::now();
    for key in dirty_buckets {
        rebuild_bucket(tx, &key.session_id, key.hour_ts, &key.model)?;
        stats.bucket_rebuilds += 1;
    }
    stats.canonical_apply_nanos += elapsed_nanos(started);
    Ok(stats)
}
