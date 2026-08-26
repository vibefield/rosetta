//! The write path: one batch, one transaction. Canonical rows, FTS entries,
//! bucket corrections (new − old), drift aggregates, and the checkpoint CAS
//! all commit together — aggregates can never drift from the rows they
//! summarize, and a torn projection write can never exist. `source_op` is a
//! STAGING BUFFER, not a representation: it holds only not-yet-active staged
//! generations and is pruned at activation. The raw ledger (rosetta-capture)
//! is the durable replay truth; the projection undoes a source's old
//! contribution via provenance columns, never via a permanent op journal.

use rusqlite::{OptionalExtension, Transaction, params};

use rosetta_core::fold::{HOUR_MS, hour_bucket};
use rosetta_core::rows::{
    DriftFact, FileEditRow, GitCommitRow, MessageRow, Ms, PlanRow, ProjectRow, RuntimeRow,
    SessionRow, TaskRow, TokenSnapshot, WorkspaceObsRow,
};
use rosetta_core::{IngestOp, ids};

use crate::StoreError;

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
    /// Persist ops for an incomplete replacement generation; project nothing.
    Stage,
    /// Extend the already-active generation and project only newly persisted ops.
    Append,
    /// Persist the final ops, flip the active generation, and atomically rebuild.
    Activate,
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
}

/// The columns of an existing message row that FTS resync and usage
/// replacement need.
struct OldRow {
    id: i64,
    session_id: String,
    content: Option<String>,
    tool_input: Option<String>,
    tool_result: Option<String>,
    model: Option<String>,
    ts: Option<Ms>,
}

fn load_old_by_timeline(tx: &Transaction, timeline_id: &str) -> Result<Option<OldRow>, StoreError> {
    load_old(
        tx,
        "SELECT id, session_id, content, tool_input, tool_result, model, ts
         FROM message WHERE timeline_id = ?1",
        params![timeline_id],
    )
}

fn load_old_by_tool(
    tx: &Transaction,
    session_id: &str,
    tool_id: &str,
) -> Result<Option<OldRow>, StoreError> {
    load_old(
        tx,
        "SELECT id, session_id, content, tool_input, tool_result, model, ts
         FROM message WHERE session_id = ?1 AND tool_id = ?2",
        params![session_id, tool_id],
    )
}

fn load_old(
    tx: &Transaction,
    sql: &str,
    params: impl rusqlite::Params,
) -> Result<Option<OldRow>, StoreError> {
    let row = tx
        .prepare_cached(sql)?
        .query_row(params, |row| {
            Ok(OldRow {
                id: row.get(0)?,
                session_id: row.get(1)?,
                content: row.get(2)?,
                tool_input: row.get(3)?,
                tool_result: row.get(4)?,
                model: row.get(5)?,
                ts: row.get(6)?,
            })
        })
        .optional()?;
    Ok(row)
}

fn fts_delete(tx: &Transaction, old: &OldRow) -> Result<(), StoreError> {
    tx.prepare_cached(
        "INSERT INTO message_fts (message_fts, rowid, content, tool_input, tool_result)
         VALUES ('delete', ?1, ?2, ?3, ?4)",
    )?
    .execute(params![
        old.id,
        old.content,
        old.tool_input,
        old.tool_result
    ])?;
    Ok(())
}

fn fts_insert(
    tx: &Transaction,
    rowid: i64,
    content: Option<&str>,
    tool_input: Option<&str>,
    tool_result: Option<&str>,
) -> Result<(), StoreError> {
    tx.prepare_cached(
        "INSERT INTO message_fts (rowid, content, tool_input, tool_result)
         VALUES (?1, ?2, ?3, ?4)",
    )?
    .execute(params![rowid, content, tool_input, tool_result])?;
    Ok(())
}

/// Recompute one bucket from its source rows. Replacement can change hour,
/// model, kind, or reported-component presence; recomputation makes all of
/// those moves exact and lets SQL SUM restore NULL when nobody reports a
/// component.
fn rebuild_bucket(
    tx: &Transaction,
    session_id: &str,
    hour_ts: Ms,
    model: &str,
) -> Result<(), StoreError> {
    let aggregate = tx
        .prepare_cached(
            "SELECT SUM(in_tokens), SUM(out_tokens), SUM(cache_r_tokens),
                    SUM(cache_w_tokens), SUM(think_tokens), COUNT(*),
                    SUM(CASE WHEN type = 'tool_use' THEN 1 ELSE 0 END)
             FROM message
             WHERE session_id = ?1
               AND COALESCE(ts, 0) >= ?2 AND COALESCE(ts, 0) < ?2 + ?4
               AND COALESCE(model, 'unknown') = ?3",
        )?
        .query_row(params![session_id, hour_ts, model, HOUR_MS], |row| {
            Ok((
                TokenSnapshot {
                    input: row.get(0)?,
                    output: row.get(1)?,
                    cache_read: row.get(2)?,
                    cache_write: row.get(3)?,
                    thinking: row.get(4)?,
                },
                row.get::<_, i64>(5)?,
                row.get::<_, Option<i64>>(6)?.unwrap_or(0),
            ))
        })?;
    let (tokens, messages, tools) = aggregate;
    if messages == 0 {
        tx.prepare_cached(
            "DELETE FROM usage_bucket WHERE session_id = ?1 AND hour_ts = ?2 AND model = ?3",
        )?
        .execute(params![session_id, hour_ts, model])?;
        return Ok(());
    }
    tx.prepare_cached(
        "INSERT INTO usage_bucket
           (session_id, hour_ts, model, in_tokens, out_tokens, cache_r_tokens,
            cache_w_tokens, think_tokens, message_count, tool_call_count)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(session_id, hour_ts, model) DO UPDATE SET
           in_tokens = excluded.in_tokens, out_tokens = excluded.out_tokens,
           cache_r_tokens = excluded.cache_r_tokens,
           cache_w_tokens = excluded.cache_w_tokens,
           think_tokens = excluded.think_tokens,
           message_count = excluded.message_count,
           tool_call_count = excluded.tool_call_count",
    )?
    .execute(params![
        session_id,
        hour_ts,
        model,
        tokens.input,
        tokens.output,
        tokens.cache_read,
        tokens.cache_write,
        tokens.thinking,
        messages,
        tools,
    ])?;
    Ok(())
}

fn rebuild_old_bucket(tx: &Transaction, old: &OldRow) -> Result<(), StoreError> {
    rebuild_bucket(
        tx,
        &old.session_id,
        hour_bucket(old.ts.unwrap_or(0)),
        bucket_model(old.model.as_deref()),
    )
}

fn touch_session(tx: &Transaction, session_id: &str, ts: Option<Ms>) -> Result<(), StoreError> {
    if let Some(ts) = ts {
        tx.prepare_cached("UPDATE session SET updated_at = MAX(updated_at, ?2) WHERE id = ?1")?
            .execute(params![session_id, ts])?;
    }
    Ok(())
}

fn bucket_model(model: Option<&str>) -> &str {
    model.unwrap_or("unknown")
}

fn apply_runtime(tx: &Transaction, row: &RuntimeRow) -> Result<(), StoreError> {
    tx.prepare_cached(
        "INSERT INTO agent_runtime
           (id, kind, data_root, enabled, account, config, skills, versions_seen, discovered_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(id) DO UPDATE SET
           kind = excluded.kind,
           data_root = excluded.data_root,
           -- enabled is the user's consent flag: re-discovery never resets it
           account = COALESCE(excluded.account, agent_runtime.account),
           config = COALESCE(excluded.config, agent_runtime.config),
           skills = COALESCE(excluded.skills, agent_runtime.skills),
           versions_seen = COALESCE(excluded.versions_seen, agent_runtime.versions_seen)",
    )?
    .execute(params![
        row.id,
        row.kind,
        row.data_root,
        row.enabled,
        row.account.as_ref().map(|v| v.to_string()),
        row.config.as_ref().map(|v| v.to_string()),
        row.skills.as_ref().map(|v| v.to_string()),
        row.versions_seen.as_ref().map(|v| v.to_string()),
        row.discovered_at,
    ])?;
    Ok(())
}

fn apply_project(tx: &Transaction, row: &ProjectRow) -> Result<(), StoreError> {
    let mut roots = row.root_paths.clone();
    if let Some(existing) = tx
        .prepare_cached("SELECT root_paths FROM project WHERE id = ?1")?
        .query_row(params![row.id], |record| record.get::<_, String>(0))
        .optional()?
    {
        roots.extend(serde_json::from_str::<Vec<String>>(&existing)?);
    }
    roots.sort();
    roots.dedup();
    tx.prepare_cached(
        "INSERT INTO project (id, name, root_paths, git_repo) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(id) DO UPDATE SET
           name = excluded.name,
           root_paths = excluded.root_paths,
           git_repo = COALESCE(excluded.git_repo, project.git_repo)",
    )?
    .execute(params![
        row.id,
        row.name,
        serde_json::to_string(&roots)?,
        row.git_repo,
    ])?;
    Ok(())
}

fn apply_session(tx: &Transaction, row: &SessionRow) -> Result<(), StoreError> {
    tx.prepare_cached(
        "INSERT INTO session
           (id, native_session_id, runtime_id, project_id, parent_session_id,
            root_session_id, role, title, summary, started_at, updated_at, stream_state)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
         ON CONFLICT(id) DO UPDATE SET
           project_id = COALESCE(excluded.project_id, session.project_id),
           parent_session_id = COALESCE(excluded.parent_session_id, session.parent_session_id),
           root_session_id = excluded.root_session_id,
           role = excluded.role,
           title = COALESCE(excluded.title, session.title),
           summary = COALESCE(excluded.summary, session.summary),
           started_at = MIN(session.started_at, excluded.started_at),
           updated_at = MAX(session.updated_at, excluded.updated_at),
           stream_state = excluded.stream_state",
    )?
    .execute(params![
        row.id,
        row.native_session_id,
        row.runtime_id,
        row.project_id,
        row.parent_session_id,
        row.root_session_id,
        row.role.as_str(),
        row.title,
        row.summary,
        row.started_at,
        row.updated_at,
        row.stream_state.as_str(),
    ])?;
    Ok(())
}

fn apply_message(
    tx: &Transaction,
    row: &MessageRow,
    stats: &mut ApplyStats,
) -> Result<(), StoreError> {
    let old = load_old_by_timeline(tx, &row.timeline_id)?;
    let new_usage = row.usage.unwrap_or_default();
    let tool_input = row.tool.as_ref().map(|tool| tool.input.to_string());
    let tool_payload = row
        .tool
        .as_ref()
        .and_then(|tool| tool.payload.as_ref().map(|p| p.to_string()));

    match old {
        Some(old) => {
            // Replacement: FTS resyncs, then both old and new aggregate keys
            // are rebuilt so an hour/model/kind move cannot strand counts.
            fts_delete(tx, &old)?;
            tx.prepare_cached(
                "UPDATE message SET
                   session_id = ?2, ordinal = ?3, type = ?4, subtype = ?5, uuid = ?6,
                   parent_uuid = ?7, ts = ?8, model = ?9, effort = ?10, content = ?11,
                   tool_id = ?12, tool_name = ?13, tool_input = ?14, tool_result = ?15,
                   tool_payload = ?16, tool_is_error = ?17, branch_tool_id = ?18,
                   in_tokens = ?19, out_tokens = ?20, cache_r_tokens = ?21,
                   cache_w_tokens = ?22, think_tokens = ?23, agent_version = ?24,
                   src_cursor = ?25, src_offset = ?26
                 WHERE timeline_id = ?1",
            )?
            .execute(params![
                row.timeline_id,
                row.session_id,
                row.ordinal,
                row.kind.as_str(),
                row.subtype,
                row.uuid,
                row.parent_uuid,
                row.ts,
                row.model,
                row.effort,
                row.content,
                row.tool.as_ref().and_then(|t| t.tool_id.as_deref()),
                row.tool.as_ref().map(|t| t.name.as_str()),
                tool_input,
                row.tool.as_ref().and_then(|t| t.result.as_deref()),
                tool_payload,
                row.tool.as_ref().and_then(|t| t.is_error),
                row.branch_tool_id,
                new_usage.input,
                new_usage.output,
                new_usage.cache_read,
                new_usage.cache_write,
                new_usage.thinking,
                row.agent_version,
                row.src_cursor,
                row.src_offset,
            ])?;
            fts_insert(
                tx,
                old.id,
                row.content.as_deref(),
                tool_input.as_deref(),
                row.tool.as_ref().and_then(|t| t.result.as_deref()),
            )?;
            rebuild_old_bucket(tx, &old)?;
            rebuild_bucket(
                tx,
                &row.session_id,
                hour_bucket(row.ts.unwrap_or(0)),
                bucket_model(row.model.as_deref()),
            )?;
            stats.messages_updated += 1;
        }
        None => {
            tx.prepare_cached(
                "INSERT INTO message
                   (timeline_id, session_id, ordinal, type, subtype, uuid, parent_uuid, ts,
                    model, effort, content, tool_id, tool_name, tool_input, tool_result,
                    tool_payload, tool_is_error, branch_tool_id, in_tokens, out_tokens,
                    cache_r_tokens, cache_w_tokens, think_tokens, agent_version,
                    src_cursor, src_offset)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
                         ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26)",
            )?
            .execute(params![
                row.timeline_id,
                row.session_id,
                row.ordinal,
                row.kind.as_str(),
                row.subtype,
                row.uuid,
                row.parent_uuid,
                row.ts,
                row.model,
                row.effort,
                row.content,
                row.tool.as_ref().and_then(|t| t.tool_id.as_deref()),
                row.tool.as_ref().map(|t| t.name.as_str()),
                tool_input,
                row.tool.as_ref().and_then(|t| t.result.as_deref()),
                tool_payload,
                row.tool.as_ref().and_then(|t| t.is_error),
                row.branch_tool_id,
                new_usage.input,
                new_usage.output,
                new_usage.cache_read,
                new_usage.cache_write,
                new_usage.thinking,
                row.agent_version,
                row.src_cursor,
                row.src_offset,
            ])?;
            let rowid = tx.last_insert_rowid();
            fts_insert(
                tx,
                rowid,
                row.content.as_deref(),
                tool_input.as_deref(),
                row.tool.as_ref().and_then(|t| t.result.as_deref()),
            )?;
            rebuild_bucket(
                tx,
                &row.session_id,
                hour_bucket(row.ts.unwrap_or(0)),
                bucket_model(row.model.as_deref()),
            )?;
            stats.messages_inserted += 1;
        }
    }
    touch_session(tx, &row.session_id, row.ts)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn apply_tool_result(
    tx: &Transaction,
    session_id: &str,
    tool_id: &str,
    result: Option<&str>,
    payload: Option<&serde_json::Value>,
    is_error: Option<bool>,
    at: Option<Ms>,
    stats: &mut ApplyStats,
) -> Result<(), StoreError> {
    let Some(old) = load_old_by_tool(tx, session_id, tool_id)? else {
        // The call row hasn't landed yet (out-of-order stream). The source-op
        // journal has the record; generation activation or an audit replay
        // re-applies it after the call is available.
        stats.tool_results_unmatched += 1;
        return Ok(());
    };
    fts_delete(tx, &old)?;
    tx.prepare_cached(
        "UPDATE message SET tool_result = ?2, tool_payload = ?3, tool_is_error = ?4
         WHERE id = ?1",
    )?
    .execute(params![
        old.id,
        result,
        payload.map(|p| p.to_string()),
        is_error
    ])?;
    fts_insert(
        tx,
        old.id,
        old.content.as_deref(),
        old.tool_input.as_deref(),
        result,
    )?;
    touch_session(tx, session_id, at)?;
    stats.tool_results_merged += 1;
    Ok(())
}

fn apply_usage(
    tx: &Transaction,
    session_id: &str,
    response: &str,
    ts: Ms,
    model: Option<&str>,
    tokens: &TokenSnapshot,
    stats: &mut ApplyStats,
) -> Result<(), StoreError> {
    let Some(old) = load_old_by_timeline(tx, response)? else {
        stats.usage_unmatched += 1;
        return Ok(());
    };
    let next_model = model.map(str::to_owned).or_else(|| old.model.clone());
    let next_ts = old.ts.or(Some(ts));
    tx.prepare_cached(
        "UPDATE message SET in_tokens = ?2, out_tokens = ?3, cache_r_tokens = ?4,
                cache_w_tokens = ?5, think_tokens = ?6, model = ?7, ts = ?8
         WHERE id = ?1",
    )?
    .execute(params![
        old.id,
        tokens.input,
        tokens.output,
        tokens.cache_read,
        tokens.cache_write,
        tokens.thinking,
        next_model,
        next_ts,
    ])?;
    rebuild_old_bucket(tx, &old)?;
    rebuild_bucket(
        tx,
        session_id,
        hour_bucket(next_ts.unwrap_or(0)),
        bucket_model(next_model.as_deref()),
    )?;
    touch_session(tx, session_id, Some(ts))?;
    stats.usage_applied += 1;
    Ok(())
}

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
    tx.prepare_cached("UPDATE session SET project_id = ?2 WHERE id = ?1")?
        .execute(params![row.session_id, project_id])?;
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

fn source_state(tx: &Transaction, cursor_id: i64) -> Result<(String, u64), StoreError> {
    let Some((session_id, generation)) = tx
        .prepare_cached("SELECT session_id, active_generation FROM ingest_cursor WHERE id = ?1")?
        .query_row(params![cursor_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
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
    Ok((session_id, generation))
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

fn append_source_op(
    tx: &Transaction,
    op: &IngestOp,
    src_cursor: Option<i64>,
    stats: &mut ApplyStats,
) -> Result<(), StoreError> {
    if let IngestOp::Message(row) = op {
        let mut row = (**row).clone();
        let existing = tx
            .prepare_cached("SELECT ordinal FROM message WHERE timeline_id = ?1")?
            .query_row(params![row.timeline_id], |record| record.get::<_, i64>(0))
            .optional()?;
        row.ordinal = match existing {
            Some(ordinal) => ordinal,
            None => tx
                .prepare_cached(
                    "SELECT COALESCE(MAX(ordinal) + 1, 0) FROM message WHERE session_id = ?1",
                )?
                .query_row(params![row.session_id], |record| record.get(0))?,
        };
        return apply_message(tx, &row, stats);
    }
    apply_op(tx, op, src_cursor, stats)
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

/// Remove one source's projected contribution by provenance. Messages carry
/// `src_cursor` natively; the small tables carry the column added for exactly
/// this sweep. Buckets are untouched here — they are recomputed from the
/// surviving rows after re-projection, so a swap can never leave aggregate
/// residue behind.
fn remove_cursor_projection(tx: &Transaction, cursor_id: i64) -> Result<(), StoreError> {
    for sql in [
        "DELETE FROM file_edit WHERE src_cursor = ?1",
        "DELETE FROM git_commit WHERE src_cursor = ?1",
        "DELETE FROM task WHERE src_cursor = ?1",
        "DELETE FROM plan WHERE src_cursor = ?1",
        "DELETE FROM workspace_obs WHERE src_cursor = ?1",
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

    let mut stmt = tx.prepare_cached(
        "SELECT id, session_id, content, tool_input, tool_result, model, ts
         FROM message WHERE src_cursor = ?1 ORDER BY id",
    )?;
    let rows = stmt.query_map(params![cursor_id], |row| {
        Ok(OldRow {
            id: row.get(0)?,
            session_id: row.get(1)?,
            content: row.get(2)?,
            tool_input: row.get(3)?,
            tool_result: row.get(4)?,
            model: row.get(5)?,
            ts: row.get(6)?,
        })
    })?;
    let old_rows = rows.collect::<Result<Vec<_>, _>>()?;
    drop(stmt);
    for old in &old_rows {
        fts_delete(tx, old)?;
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
    tx.prepare_cached(
        "UPDATE message SET ordinal = -1 - (
           SELECT rn FROM (
             SELECT m.id AS mid,
                    ROW_NUMBER() OVER (
                      ORDER BY COALESCE(c.path, ''), COALESCE(m.src_offset, m.id), m.id
                    ) - 1 AS rn
             FROM message m LEFT JOIN ingest_cursor c ON c.id = m.src_cursor
             WHERE m.session_id = ?1
           ) WHERE mid = message.id
         )
         WHERE session_id = ?1",
    )?
    .execute(params![session_id])?;
    tx.prepare_cached(
        "UPDATE message SET ordinal = -1 - ordinal WHERE session_id = ?1 AND ordinal < 0",
    )?
    .execute(params![session_id])?;
    Ok(())
}

fn add_component(acc: &mut Option<i64>, value: Option<i64>) {
    if let Some(value) = value {
        *acc = Some(acc.unwrap_or(0) + value);
    }
}

/// Buckets are a pure function of the message rows (final usage, hour,
/// model): recompute this session's buckets from scratch.
fn recompute_session_buckets(tx: &Transaction, session_id: &str) -> Result<(), StoreError> {
    tx.prepare_cached("DELETE FROM usage_bucket WHERE session_id = ?1")?
        .execute(params![session_id])?;

    #[derive(Default)]
    struct Acc {
        tokens: TokenSnapshot,
        messages: i64,
        tools: i64,
    }
    let mut buckets: std::collections::BTreeMap<(i64, String), Acc> =
        std::collections::BTreeMap::new();
    let mut stmt = tx.prepare_cached(
        "SELECT ts, model, in_tokens, out_tokens, cache_r_tokens, cache_w_tokens, think_tokens,
                tool_id IS NOT NULL
         FROM message WHERE session_id = ?1",
    )?;
    let rows = stmt.query_map(params![session_id], |row| {
        Ok((
            row.get::<_, Option<i64>>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, Option<i64>>(2)?,
            row.get::<_, Option<i64>>(3)?,
            row.get::<_, Option<i64>>(4)?,
            row.get::<_, Option<i64>>(5)?,
            row.get::<_, Option<i64>>(6)?,
            row.get::<_, bool>(7)?,
        ))
    })?;
    for row in rows {
        let (ts, model, input, output, cache_read, cache_write, thinking, has_tool) = row?;
        let key = (
            hour_bucket(ts.unwrap_or(0)),
            bucket_model(model.as_deref()).to_string(),
        );
        let acc = buckets.entry(key).or_default();
        add_component(&mut acc.tokens.input, input);
        add_component(&mut acc.tokens.output, output);
        add_component(&mut acc.tokens.cache_read, cache_read);
        add_component(&mut acc.tokens.cache_write, cache_write);
        add_component(&mut acc.tokens.thinking, thinking);
        acc.messages += 1;
        if has_tool {
            acc.tools += 1;
        }
    }
    drop(stmt);
    for ((hour, model), acc) in buckets {
        tx.prepare_cached(
            "INSERT INTO usage_bucket
               (session_id, hour_ts, model, in_tokens, out_tokens, cache_r_tokens,
                cache_w_tokens, think_tokens, message_count, tool_call_count)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        )?
        .execute(params![
            session_id,
            hour,
            model,
            acc.tokens.input,
            acc.tokens.output,
            acc.tokens.cache_read,
            acc.tokens.cache_write,
            acc.tokens.thinking,
            acc.messages,
            acc.tools,
        ])?;
    }
    Ok(())
}

/// Activation with a staged-only journal: sweep this cursor's old
/// contribution by provenance, project the staged generation, renumber the
/// session, recompute its buckets, and PRUNE the journal — `source_op` holds
/// nothing for active generations. Drift is additive-only: what was observed
/// stays observed; a rewrite does not un-observe it.
fn activate_cursor(
    tx: &Transaction,
    source: &SourceWrite,
    stats: &mut ApplyStats,
) -> Result<(), StoreError> {
    let generation = sqlite_u64(source.cursor_id, "generation", source.generation)?;
    let staged = staged_ops(tx, source.cursor_id, generation)?;
    remove_cursor_projection(tx, source.cursor_id)?;

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
                apply_message(tx, &row, stats)?;
            }
            IngestOp::FileEdit(_) => edits.push(op),
            IngestOp::ToolResult { .. } | IngestOp::Usage { .. } => merges.push(op),
            _ => apply_op(tx, op, Some(source.cursor_id), stats)?,
        }
    }
    for op in edits.into_iter().chain(merges) {
        apply_op(tx, op, Some(source.cursor_id), stats)?;
    }

    renumber_session(tx, &source.session_id)?;
    recompute_session_buckets(tx, &source.session_id)?;
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

fn apply_op(
    tx: &Transaction,
    op: &IngestOp,
    src_cursor: Option<i64>,
    stats: &mut ApplyStats,
) -> Result<(), StoreError> {
    match op {
        IngestOp::Runtime(row) => apply_runtime(tx, row)?,
        IngestOp::Project(row) => apply_project(tx, row)?,
        IngestOp::SessionMeta(row) => apply_session(tx, row)?,
        IngestOp::Message(row) => apply_message(tx, row, stats)?,
        IngestOp::ToolResult {
            session_id,
            tool_id,
            result,
            payload,
            is_error,
            at,
        } => apply_tool_result(
            tx,
            session_id,
            tool_id,
            result.as_deref(),
            payload.as_ref(),
            *is_error,
            *at,
            stats,
        )?,
        IngestOp::Usage {
            session_id,
            response,
            ts,
            model,
            tokens,
        } => apply_usage(
            tx,
            session_id,
            response,
            *ts,
            model.as_deref(),
            tokens,
            stats,
        )?,
        IngestOp::WorkspaceObs(row) => apply_workspace_obs(tx, row, src_cursor)?,
        IngestOp::FileEdit(row) => apply_file_edit(tx, row, src_cursor)?,
        IngestOp::Plan(row) => apply_plan(tx, row, src_cursor)?,
        IngestOp::Task(row) => apply_task(tx, row, src_cursor)?,
        IngestOp::GitCommit(row) => apply_git_commit(tx, row, src_cursor)?,
        IngestOp::Drift(fact) => apply_drift(tx, fact, stats)?,
    }
    Ok(())
}

pub(crate) fn apply_batch(tx: &Transaction, batch: &Batch) -> Result<ApplyStats, StoreError> {
    let mut stats = ApplyStats::default();
    match &batch.source {
        None => {
            for op in &batch.ops {
                apply_op(tx, op, None, &mut stats)?;
            }
        }
        Some(source) => {
            let (cursor_session, active_generation) = source_state(tx, source.cursor_id)?;
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
                SourceMode::Stage if source.generation == active_generation => {
                    return Err(StoreError::SourceState {
                        cursor_id: source.cursor_id,
                        message: "cannot stage into the active generation".to_string(),
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
                SourceMode::Activate if source.generation <= active_generation => {
                    return Err(StoreError::SourceState {
                        cursor_id: source.cursor_id,
                        message: format!(
                            "activation generation {} must exceed active generation {active_generation}",
                            source.generation
                        ),
                    });
                }
                _ => {}
            }

            match source.mode {
                // Staged generations are the ONLY thing the journal holds.
                SourceMode::Stage => {
                    persist_source_ops(tx, source, &batch.ops)?;
                }
                // The hot path projects directly — replay idempotence comes
                // from deterministic ids and identity indexes, not a journal.
                SourceMode::Append => {
                    for op in &batch.ops {
                        append_source_op(tx, op, Some(source.cursor_id), &mut stats)?;
                    }
                }
                SourceMode::Activate => {
                    persist_source_ops(tx, source, &batch.ops)?;
                    let generation = sqlite_u64(source.cursor_id, "generation", source.generation)?;
                    let changed = tx
                        .prepare_cached(
                            "UPDATE ingest_cursor SET active_generation = ?2
                             WHERE id = ?1 AND active_generation = ?3",
                        )?
                        .execute(params![
                            source.cursor_id,
                            generation,
                            sqlite_u64(source.cursor_id, "active generation", active_generation,)?,
                        ])?;
                    if changed != 1 {
                        return Err(StoreError::SourceState {
                            cursor_id: source.cursor_id,
                            message: "active generation changed concurrently".to_string(),
                        });
                    }
                    activate_cursor(tx, source, &mut stats)?;
                }
            }
        }
    }
    for checkpoint in &batch.checkpoints {
        apply_checkpoint(tx, checkpoint)?;
    }
    Ok(stats)
}
