//! The write path: one batch, one transaction. Canonical rows, FTS entries,
//! bucket corrections (new − old), drift aggregates, and the checkpoint CAS
//! all commit together — aggregates can never drift from the rows they
//! summarize, and a torn write can never exist (docs/ingest-engine.html,
//! "Decided — ledger-first, with an explicit crash protocol").

use rusqlite::{OptionalExtension, Transaction, params};

use rosetta_core::IngestOp;
use rosetta_core::fold::{bucket_apply, hour_bucket, snapshot_delta};
use rosetta_core::rows::{
    DriftFact, FileEditRow, GitCommitRow, MessageRow, Ms, PlanRow, ProjectRow, RuntimeRow,
    SessionRow, TaskRow, TokenSnapshot, WorkspaceObsRow,
};

use crate::StoreError;

/// Compare-and-set update for one source file's driver checkpoint.
#[derive(Debug, Clone)]
pub struct CheckpointUpdate {
    pub runtime_id: String,
    pub stream: String,
    pub path: String,
    /// `None` means "this file has no cursor yet" — the insert fails if one
    /// appeared concurrently.
    pub expected: Option<String>,
    pub next: String,
    pub updated_at: Option<Ms>,
}

#[derive(Debug, Default)]
pub struct Batch {
    pub ops: Vec<IngestOp>,
    pub checkpoints: Vec<CheckpointUpdate>,
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
    content: Option<String>,
    tool_input: Option<String>,
    tool_result: Option<String>,
    model: Option<String>,
    ts: Option<Ms>,
    usage: TokenSnapshot,
}

fn load_old_by_timeline(tx: &Transaction, timeline_id: &str) -> Result<Option<OldRow>, StoreError> {
    load_old(
        tx,
        "SELECT id, content, tool_input, tool_result, model, ts,
                in_tokens, out_tokens, cache_r_tokens, cache_w_tokens, think_tokens
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
        "SELECT id, content, tool_input, tool_result, model, ts,
                in_tokens, out_tokens, cache_r_tokens, cache_w_tokens, think_tokens
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
                content: row.get(1)?,
                tool_input: row.get(2)?,
                tool_result: row.get(3)?,
                model: row.get(4)?,
                ts: row.get(5)?,
                usage: TokenSnapshot {
                    input: row.get(6)?,
                    output: row.get(7)?,
                    cache_read: row.get(8)?,
                    cache_write: row.get(9)?,
                    thinking: row.get(10)?,
                },
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

/// Read-modify-write one usage bucket with None-aware arithmetic. `None`
/// components stay `None` — "never reported" is not zero.
fn bucket_touch(
    tx: &Transaction,
    session_id: &str,
    hour_ts: Ms,
    model: &str,
    delta: &TokenSnapshot,
    message_inc: i64,
    tool_inc: i64,
) -> Result<(), StoreError> {
    if delta.is_empty() && message_inc == 0 && tool_inc == 0 {
        return Ok(());
    }
    let existing = tx
        .prepare_cached(
            "SELECT in_tokens, out_tokens, cache_r_tokens, cache_w_tokens, think_tokens,
                    message_count, tool_call_count
             FROM usage_bucket WHERE session_id = ?1 AND hour_ts = ?2 AND model = ?3",
        )?
        .query_row(params![session_id, hour_ts, model], |row| {
            Ok((
                TokenSnapshot {
                    input: row.get(0)?,
                    output: row.get(1)?,
                    cache_read: row.get(2)?,
                    cache_write: row.get(3)?,
                    thinking: row.get(4)?,
                },
                row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)?,
            ))
        })
        .optional()?;

    match existing {
        Some((bucket, messages, tools)) => {
            let next = bucket_apply(&bucket, delta);
            tx.prepare_cached(
                "UPDATE usage_bucket
                 SET in_tokens = ?4, out_tokens = ?5, cache_r_tokens = ?6,
                     cache_w_tokens = ?7, think_tokens = ?8,
                     message_count = ?9, tool_call_count = ?10
                 WHERE session_id = ?1 AND hour_ts = ?2 AND model = ?3",
            )?
            .execute(params![
                session_id,
                hour_ts,
                model,
                next.input,
                next.output,
                next.cache_read,
                next.cache_write,
                next.thinking,
                messages + message_inc,
                tools + tool_inc,
            ])?;
        }
        None => {
            let next = bucket_apply(&TokenSnapshot::default(), delta);
            tx.prepare_cached(
                "INSERT INTO usage_bucket
                   (session_id, hour_ts, model, in_tokens, out_tokens, cache_r_tokens,
                    cache_w_tokens, think_tokens, message_count, tool_call_count)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            )?
            .execute(params![
                session_id,
                hour_ts,
                model,
                next.input,
                next.output,
                next.cache_read,
                next.cache_write,
                next.thinking,
                message_inc,
                tool_inc,
            ])?;
        }
    }
    Ok(())
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
        serde_json::to_string(&row.root_paths)?,
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
            // Replacement: bucket takes (new − old); FTS resyncs.
            let delta = snapshot_delta(&new_usage, Some(&old.usage));
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
            bucket_touch(
                tx,
                &row.session_id,
                hour_bucket(row.ts.unwrap_or(0)),
                bucket_model(row.model.as_deref()),
                &delta,
                0,
                0,
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
            let delta = snapshot_delta(&new_usage, None);
            let is_tool = matches!(row.kind, rosetta_core::rows::MessageKind::ToolUse);
            bucket_touch(
                tx,
                &row.session_id,
                hour_bucket(row.ts.unwrap_or(0)),
                bucket_model(row.model.as_deref()),
                &delta,
                1,
                if is_tool { 1 } else { 0 },
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
        // The call row hasn't landed yet (out-of-order stream). The ledger
        // has the record; a later pass or the row's own arrival re-merges.
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
    let delta = snapshot_delta(tokens, Some(&old.usage));
    tx.prepare_cached(
        "UPDATE message SET in_tokens = ?2, out_tokens = ?3, cache_r_tokens = ?4,
                cache_w_tokens = ?5, think_tokens = ?6
         WHERE id = ?1",
    )?
    .execute(params![
        old.id,
        tokens.input,
        tokens.output,
        tokens.cache_read,
        tokens.cache_write,
        tokens.thinking,
    ])?;
    let model = model.or(old.model.as_deref());
    let hour = hour_bucket(old.ts.unwrap_or(ts));
    bucket_touch(tx, session_id, hour, bucket_model(model), &delta, 0, 0)?;
    touch_session(tx, session_id, Some(ts))?;
    stats.usage_applied += 1;
    Ok(())
}

fn apply_workspace_obs(tx: &Transaction, row: &WorkspaceObsRow) -> Result<(), StoreError> {
    tx.prepare_cached(
        "INSERT INTO workspace_obs (session_id, at, cwd, git_branch, git_repo)
         VALUES (?1, ?2, ?3, ?4, ?5)",
    )?
    .execute(params![
        row.session_id,
        row.at,
        row.cwd,
        row.git_branch,
        row.git_repo
    ])?;
    Ok(())
}

fn apply_file_edit(tx: &Transaction, row: &FileEditRow) -> Result<(), StoreError> {
    let message_id: Option<i64> = match &row.message_timeline_id {
        Some(timeline_id) => tx
            .prepare_cached("SELECT id FROM message WHERE timeline_id = ?1")?
            .query_row(params![timeline_id], |r| r.get(0))
            .optional()?,
        None => None,
    };
    tx.prepare_cached(
        "INSERT INTO file_edit
           (session_id, message_id, path, kind, snapshot_ref, lines_added, lines_removed, at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
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
    ])?;
    Ok(())
}

fn apply_plan(tx: &Transaction, row: &PlanRow) -> Result<(), StoreError> {
    tx.prepare_cached(
        "INSERT INTO plan (session_id, revision, content, updated_at)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(session_id, revision) DO UPDATE SET
           content = excluded.content, updated_at = excluded.updated_at",
    )?
    .execute(params![
        row.session_id,
        row.revision,
        row.content,
        row.updated_at
    ])?;
    Ok(())
}

fn apply_task(tx: &Transaction, row: &TaskRow) -> Result<(), StoreError> {
    tx.prepare_cached(
        "INSERT INTO task (id, session_id, status, content, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(id) DO UPDATE SET
           status = excluded.status, content = excluded.content,
           updated_at = COALESCE(excluded.updated_at, task.updated_at)",
    )?
    .execute(params![
        row.id,
        row.session_id,
        row.status.as_str(),
        row.content,
        row.created_at,
        row.updated_at,
    ])?;
    Ok(())
}

fn apply_git_commit(tx: &Transaction, row: &GitCommitRow) -> Result<(), StoreError> {
    tx.prepare_cached(
        "INSERT OR IGNORE INTO git_commit (commit_id, session_id, branch, at)
         VALUES (?1, ?2, ?3, ?4)",
    )?
    .execute(params![row.commit_id, row.session_id, row.branch, row.at])?;
    Ok(())
}

fn apply_drift(
    tx: &Transaction,
    fact: &DriftFact,
    stats: &mut ApplyStats,
) -> Result<(), StoreError> {
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

fn apply_checkpoint(tx: &Transaction, update: &CheckpointUpdate) -> Result<(), StoreError> {
    let changed = match &update.expected {
        None => tx
            .prepare_cached(
                "INSERT INTO ingest_cursor (runtime_id, stream, path, checkpoint, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(runtime_id, path) DO NOTHING",
            )?
            .execute(params![
                update.runtime_id,
                update.stream,
                update.path,
                update.next,
                update.updated_at,
            ])?,
        Some(expected) => tx
            .prepare_cached(
                "UPDATE ingest_cursor SET checkpoint = ?4, stream = ?2, updated_at = ?5
                 WHERE runtime_id = ?1 AND path = ?3 AND checkpoint = ?6",
            )?
            .execute(params![
                update.runtime_id,
                update.stream,
                update.path,
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

pub(crate) fn apply_batch(tx: &Transaction, batch: &Batch) -> Result<ApplyStats, StoreError> {
    let mut stats = ApplyStats::default();
    for op in &batch.ops {
        match op {
            IngestOp::Runtime(row) => apply_runtime(tx, row)?,
            IngestOp::Project(row) => apply_project(tx, row)?,
            IngestOp::SessionMeta(row) => apply_session(tx, row)?,
            IngestOp::Message(row) => apply_message(tx, row, &mut stats)?,
            IngestOp::ToolResult {
                session_id,
                tool_id,
                result,
                payload,
                is_error,
                at,
            } => {
                apply_tool_result(
                    tx,
                    session_id,
                    tool_id,
                    result.as_deref(),
                    payload.as_ref(),
                    *is_error,
                    *at,
                    &mut stats,
                )?;
            }
            IngestOp::Usage {
                session_id,
                response,
                ts,
                model,
                tokens,
            } => {
                apply_usage(
                    tx,
                    session_id,
                    response,
                    *ts,
                    model.as_deref(),
                    tokens,
                    &mut stats,
                )?;
            }
            IngestOp::WorkspaceObs(row) => apply_workspace_obs(tx, row)?,
            IngestOp::FileEdit(row) => apply_file_edit(tx, row)?,
            IngestOp::Plan(row) => apply_plan(tx, row)?,
            IngestOp::Task(row) => apply_task(tx, row)?,
            IngestOp::GitCommit(row) => apply_git_commit(tx, row)?,
            IngestOp::Drift(fact) => apply_drift(tx, fact, &mut stats)?,
        }
    }
    for checkpoint in &batch.checkpoints {
        apply_checkpoint(tx, checkpoint)?;
    }
    Ok(stats)
}
