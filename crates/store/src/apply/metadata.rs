//! Order-independent upserts for bounded runtime, project, and session facts.

use rosetta_core::rows::{ProjectRow, RuntimeRow, SessionRow, SessionTitleRow, TitleKind};
use rusqlite::{OptionalExtension, Transaction, params};

use crate::StoreError;

pub(super) fn apply_runtime(tx: &Transaction, row: &RuntimeRow) -> Result<(), StoreError> {
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

pub(super) fn apply_project(tx: &Transaction, row: &ProjectRow) -> Result<(), StoreError> {
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
           -- Several worktree roots can name one repository. Their basenames
           -- are equally strong, so use a stable tie instead of arrival order.
           name = MIN(project.name, excluded.name),
           root_paths = excluded.root_paths,
           git_repo = CASE
             WHEN project.git_repo IS NULL THEN excluded.git_repo
             WHEN excluded.git_repo IS NULL THEN project.git_repo
             ELSE MIN(project.git_repo, excluded.git_repo) END",
    )?
    .execute(params![
        row.id,
        row.name,
        serde_json::to_string(&roots)?,
        row.git_repo,
    ])?;
    Ok(())
}

pub(super) fn apply_session(tx: &Transaction, row: &SessionRow) -> Result<(), StoreError> {
    let title_rank = row.title.as_ref().map_or(0, |_| TitleKind::Explicit.rank());
    let title_at = row.title.as_ref().map(|_| row.updated_at);
    tx.prepare_cached(
        "INSERT INTO session
           (id, native_session_id, runtime_id, project_id, parent_session_id,
            root_session_id, role, title, meta_title, meta_title_rank, meta_title_at,
            summary, started_at, updated_at, stream_state, project_assigned_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                 CASE WHEN ?4 IS NULL THEN NULL ELSE ?12 END)
         ON CONFLICT(id) DO UPDATE SET
           project_id = CASE
             WHEN excluded.project_id IS NULL THEN session.project_id
             WHEN session.project_id IS NULL THEN excluded.project_id
             WHEN session.project_assigned_at IS NULL
               OR excluded.project_assigned_at < session.project_assigned_at
               OR (excluded.project_assigned_at = session.project_assigned_at
                   AND excluded.project_id < session.project_id)
             THEN excluded.project_id ELSE session.project_id END,
           project_assigned_at = CASE
             WHEN excluded.project_id IS NULL THEN session.project_assigned_at
             WHEN session.project_id IS NULL THEN excluded.project_assigned_at
             WHEN session.project_assigned_at IS NULL
               OR excluded.project_assigned_at < session.project_assigned_at
               OR (excluded.project_assigned_at = session.project_assigned_at
                   AND excluded.project_id < session.project_id)
             THEN excluded.project_assigned_at ELSE session.project_assigned_at END,
           parent_session_id = CASE
             WHEN session.parent_session_id IS NULL THEN excluded.parent_session_id
             WHEN excluded.parent_session_id IS NULL THEN session.parent_session_id
             ELSE MIN(session.parent_session_id, excluded.parent_session_id) END,
           -- A main/self row is the placeholder used before stronger native
           -- topology arrives; it must never erase a subagent relationship.
           root_session_id = CASE
             WHEN session.root_session_id = session.id
                  AND excluded.root_session_id != excluded.id
               THEN excluded.root_session_id
             WHEN excluded.root_session_id = excluded.id
                  AND session.root_session_id != session.id
               THEN session.root_session_id
             ELSE MIN(session.root_session_id, excluded.root_session_id) END,
           role = CASE
             WHEN (CASE session.role
                     WHEN 'team' THEN 2 WHEN 'subagent' THEN 1 ELSE 0 END)
                  >=
                  (CASE excluded.role
                     WHEN 'team' THEN 2 WHEN 'subagent' THEN 1 ELSE 0 END)
               THEN session.role ELSE excluded.role END,
           title = COALESCE(excluded.title, session.title),
           meta_title = CASE
             WHEN excluded.meta_title IS NOT NULL AND (
               session.meta_title IS NULL
               OR excluded.meta_title_rank > session.meta_title_rank
               OR (excluded.meta_title_rank = session.meta_title_rank AND (
                 (excluded.meta_title_at IS NOT NULL AND session.meta_title_at IS NULL)
                 OR excluded.meta_title_at > session.meta_title_at
                 OR (excluded.meta_title_at IS session.meta_title_at
                     AND excluded.meta_title < session.meta_title))))
             THEN excluded.meta_title ELSE session.meta_title END,
           meta_title_rank = CASE
             WHEN excluded.meta_title IS NOT NULL AND (
               session.meta_title IS NULL
               OR excluded.meta_title_rank > session.meta_title_rank
               OR (excluded.meta_title_rank = session.meta_title_rank AND (
                 (excluded.meta_title_at IS NOT NULL AND session.meta_title_at IS NULL)
                 OR excluded.meta_title_at > session.meta_title_at
                 OR (excluded.meta_title_at IS session.meta_title_at
                     AND excluded.meta_title < session.meta_title))))
             THEN excluded.meta_title_rank ELSE session.meta_title_rank END,
           meta_title_at = CASE
             WHEN excluded.meta_title IS NOT NULL AND (
               session.meta_title IS NULL
               OR excluded.meta_title_rank > session.meta_title_rank
               OR (excluded.meta_title_rank = session.meta_title_rank AND (
                 (excluded.meta_title_at IS NOT NULL AND session.meta_title_at IS NULL)
                 OR excluded.meta_title_at > session.meta_title_at
                 OR (excluded.meta_title_at IS session.meta_title_at
                     AND excluded.meta_title < session.meta_title))))
             THEN excluded.meta_title_at ELSE session.meta_title_at END,
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
        title_rank,
        title_at,
        row.summary,
        row.started_at,
        row.updated_at,
        row.stream_state.as_str(),
    ])?;
    refresh_session_title(tx, &row.id)?;
    Ok(())
}

pub(super) fn apply_session_title(
    tx: &Transaction,
    row: &SessionTitleRow,
) -> Result<(), StoreError> {
    tx.prepare_cached(
        "UPDATE session
         SET meta_title = ?2, meta_title_rank = ?3, meta_title_at = ?4
         WHERE id = ?1 AND (
           meta_title IS NULL
           OR ?3 > meta_title_rank
           OR (?3 = meta_title_rank AND (
             (?4 IS NOT NULL AND meta_title_at IS NULL)
             OR ?4 > meta_title_at
             OR (?4 IS meta_title_at AND ?2 < meta_title))))",
    )?
    .execute(params![row.session_id, row.title, row.kind.rank(), row.at])?;
    refresh_session_title(tx, &row.session_id)?;
    Ok(())
}

/// Reconcile the public session title from source-owned singleton rows.
/// Explicit custom titles outrank generated titles, which outrank derived
/// fallbacks, independent of record order, batching, replay, or generation
/// replacement. Stream and bounded-meta candidates use the same ranks.
pub(super) fn refresh_session_title(tx: &Transaction, session_id: &str) -> Result<(), StoreError> {
    let streamed = tx
        .prepare_cached(
            "SELECT content,
                    CASE subtype
                      WHEN 'custom_title' THEN 3
                      WHEN 'ai_title' THEN 2
                      WHEN 'title_fallback' THEN 1
                    END AS title_rank
             FROM message
             WHERE session_id = ?1 AND type = 'summary'
               AND subtype IN ('custom_title', 'ai_title', 'title_fallback')
               AND content IS NOT NULL AND content != ''
             ORDER BY title_rank DESC, ts DESC, ordinal DESC, id DESC
             LIMIT 1",
        )?
        .query_row(params![session_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .optional()?;
    let meta = tx
        .prepare_cached("SELECT meta_title, meta_title_rank FROM session WHERE id = ?1")?
        .query_row(params![session_id], |row| {
            Ok((row.get::<_, Option<String>>(0)?, row.get::<_, i64>(1)?))
        })
        .optional()?;
    let title = match (streamed, meta) {
        (Some((streamed, streamed_rank)), Some((_meta, meta_rank)))
            if streamed_rank >= meta_rank =>
        {
            Some(streamed)
        }
        (_, Some((meta, _))) => meta,
        (Some((streamed, _)), None) => Some(streamed),
        (None, None) => None,
    };
    tx.prepare_cached("UPDATE session SET title = ?2 WHERE id = ?1")?
        .execute(params![session_id, title])?;
    Ok(())
}
