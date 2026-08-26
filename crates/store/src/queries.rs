//! Typed read surface — consumers never hand-write SQL against the
//! projection. Heatmap and contribution are the two usage features from
//! docs/canonical-model.html; higher scopes are query-time sums through
//! `session` (session-grain-only cache).

use rusqlite::{Connection, OptionalExtension, params};

use rosetta_core::rows::{
    MessageKind, MessageRow, Ms, SessionRole, SessionRow, StreamState, TokenSnapshot, ToolCall,
};

use crate::StoreError;

#[derive(Debug, Clone, PartialEq)]
pub struct HeatmapDay {
    /// Date in the caller-selected timezone, `YYYY-MM-DD`.
    pub day: String,
    pub tokens: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ContributionRow {
    pub session_id: String,
    pub project_id: Option<String>,
    pub tokens: i64,
    pub commits: i64,
    pub edits: i64,
    pub loc: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SearchHit {
    pub timeline_id: String,
    pub session_id: String,
    pub snippet: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorState {
    pub id: i64,
    pub checkpoint: String,
    pub active_generation: u64,
    /// Highest generation with a persisted source op (or the active
    /// generation when an epoch emitted no ops). New recovery epochs allocate
    /// strictly above this value instead of reusing abandoned staged rows.
    pub highest_generation: u64,
    pub session_id: String,
}

fn parse_kind(text: &str) -> MessageKind {
    match text {
        "user" => MessageKind::User,
        "agent" => MessageKind::Agent,
        "thinking" => MessageKind::Thinking,
        "tool_use" => MessageKind::ToolUse,
        "summary" => MessageKind::Summary,
        _ => MessageKind::System,
    }
}

fn parse_role(text: &str) -> SessionRole {
    match text {
        "subagent" => SessionRole::Subagent,
        "team" => SessionRole::Team,
        _ => SessionRole::Main,
    }
}

fn parse_state(text: &str) -> StreamState {
    match text {
        "streaming" => StreamState::Streaming,
        "indexed" => StreamState::Indexed,
        _ => StreamState::Pending,
    }
}

fn message_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<MessageRow> {
    let kind: String = row.get("type")?;
    let tool_name: Option<String> = row.get("tool_name")?;
    let tool = match tool_name {
        Some(name) => {
            let input: Option<String> = row.get("tool_input")?;
            Some(ToolCall {
                tool_id: row.get("tool_id")?,
                name,
                input: input
                    .and_then(|text| serde_json::from_str(&text).ok())
                    .unwrap_or(serde_json::Value::Null),
                result: row.get("tool_result")?,
                payload: row
                    .get::<_, Option<String>>("tool_payload")?
                    .and_then(|text| serde_json::from_str(&text).ok()),
                is_error: row.get("tool_is_error")?,
            })
        }
        None => None,
    };
    let usage = TokenSnapshot {
        input: row.get("in_tokens")?,
        output: row.get("out_tokens")?,
        cache_read: row.get("cache_r_tokens")?,
        cache_write: row.get("cache_w_tokens")?,
        thinking: row.get("think_tokens")?,
    };
    Ok(MessageRow {
        timeline_id: row.get("timeline_id")?,
        session_id: row.get("session_id")?,
        ordinal: row.get("ordinal")?,
        kind: parse_kind(&kind),
        subtype: row.get("subtype")?,
        uuid: row.get("uuid")?,
        parent_uuid: row.get("parent_uuid")?,
        ts: row.get("ts")?,
        model: row.get("model")?,
        effort: row.get("effort")?,
        content: row.get("content")?,
        tool,
        branch_tool_id: row.get("branch_tool_id")?,
        usage: if usage.is_empty() { None } else { Some(usage) },
        agent_version: row.get("agent_version")?,
        src_cursor: row.get("src_cursor")?,
        src_offset: row.get("src_offset")?,
    })
}

fn session_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SessionRow> {
    let role: String = row.get("role")?;
    let state: String = row.get("stream_state")?;
    Ok(SessionRow {
        id: row.get("id")?,
        native_session_id: row.get("native_session_id")?,
        runtime_id: row.get("runtime_id")?,
        project_id: row.get("project_id")?,
        parent_session_id: row.get("parent_session_id")?,
        root_session_id: row.get("root_session_id")?,
        role: parse_role(&role),
        title: row.get("title")?,
        summary: row.get("summary")?,
        started_at: row.get("started_at")?,
        updated_at: row.get("updated_at")?,
        stream_state: parse_state(&state),
    })
}

pub fn session(conn: &Connection, session_id: &str) -> Result<Option<SessionRow>, StoreError> {
    let row = conn
        .prepare_cached("SELECT * FROM session WHERE id = ?1")?
        .query_row(params![session_id], session_from_row)
        .optional()?;
    Ok(row)
}

pub fn cursor_state(
    conn: &Connection,
    runtime_id: &str,
    path: &str,
) -> Result<Option<CursorState>, StoreError> {
    let raw = conn
        .prepare_cached(
            "SELECT c.id, c.checkpoint, c.active_generation,
                    MAX(c.active_generation, COALESCE((
                      SELECT MAX(so.generation) FROM source_op so WHERE so.cursor_id = c.id
                    ), 0)),
                    c.session_id
             FROM ingest_cursor c WHERE c.runtime_id = ?1 AND c.path = ?2",
        )?
        .query_row(params![runtime_id, path], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
            ))
        })
        .optional()?;
    raw.map(|(id, checkpoint, generation, highest, session_id)| {
        let active_generation = u64::try_from(generation).map_err(|_| StoreError::SourceState {
            cursor_id: id,
            message: "active generation is negative".to_string(),
        })?;
        let highest_generation = u64::try_from(highest).map_err(|_| StoreError::SourceState {
            cursor_id: id,
            message: "highest generation is negative".to_string(),
        })?;
        Ok(CursorState {
            id,
            checkpoint,
            active_generation,
            highest_generation,
            session_id,
        })
    })
    .transpose()
}

pub fn audit_path(conn: &Connection, runtime_id: &str) -> Result<Option<String>, StoreError> {
    Ok(conn
        .prepare_cached("SELECT last_path FROM source_audit WHERE runtime_id = ?1")?
        .query_row(params![runtime_id], |row| row.get(0))
        .optional()?)
}

/// Recent sessions, newest first; scoped to a project when given.
pub fn session_list(
    conn: &Connection,
    project_id: Option<&str>,
    limit: i64,
) -> Result<Vec<SessionRow>, StoreError> {
    let mut out = Vec::new();
    match project_id {
        Some(project) => {
            let mut stmt = conn.prepare_cached(
                "SELECT * FROM session WHERE project_id = ?1
                 ORDER BY updated_at DESC LIMIT ?2",
            )?;
            let rows = stmt.query_map(params![project, limit], session_from_row)?;
            for row in rows {
                out.push(row?);
            }
        }
        None => {
            let mut stmt =
                conn.prepare_cached("SELECT * FROM session ORDER BY updated_at DESC LIMIT ?1")?;
            let rows = stmt.query_map(params![limit], session_from_row)?;
            for row in rows {
                out.push(row?);
            }
        }
    }
    Ok(out)
}

/// One page of a session timeline, oldest-first within the page. Pass the
/// lowest ordinal already loaded as `before` to page backwards.
pub fn timeline_page(
    conn: &Connection,
    session_id: &str,
    before: Option<i64>,
    limit: i64,
) -> Result<Vec<MessageRow>, StoreError> {
    let mut stmt = conn.prepare_cached(
        "SELECT * FROM (
           SELECT * FROM message
           WHERE session_id = ?1 AND ordinal < ?2
           ORDER BY ordinal DESC LIMIT ?3
         ) ORDER BY ordinal ASC",
    )?;
    let rows = stmt.query_map(
        params![session_id, before.unwrap_or(i64::MAX), limit],
        message_from_row,
    )?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Deterministic UTC compatibility wrapper for a GitHub-style activity
/// heatmap. Viewer-local callers should pass their explicit offset to
/// [`heatmap_days_in_timezone`].
pub fn heatmap_days(
    conn: &Connection,
    project_id: &str,
    since_ms: Ms,
) -> Result<Vec<HeatmapDay>, StoreError> {
    heatmap_days_in_timezone(conn, project_id, since_ms, 0)
}

/// Daily totals in an explicit fixed viewer offset. The daemon's host timezone
/// is never consulted, so the same query is deterministic across machines.
pub fn heatmap_days_in_timezone(
    conn: &Connection,
    project_id: &str,
    since_ms: Ms,
    offset_minutes: i32,
) -> Result<Vec<HeatmapDay>, StoreError> {
    let mut stmt = conn.prepare_cached(
        "SELECT date((b.hour_ts + ?3 * 60000) / 1000, 'unixepoch') AS day,
                SUM(COALESCE(b.in_tokens,0) + COALESCE(b.out_tokens,0)
                    + COALESCE(b.cache_r_tokens,0) + COALESCE(b.cache_w_tokens,0)
                    + COALESCE(b.think_tokens,0)) AS tokens
         FROM usage_bucket b
         JOIN session s ON s.id = b.session_id
         WHERE s.project_id = ?1 AND b.hour_ts >= ?2
         GROUP BY day ORDER BY day",
    )?;
    let rows = stmt.query_map(params![project_id, since_ms, offset_minutes], |row| {
        Ok(HeatmapDay {
            day: row.get(0)?,
            tokens: row.get(1)?,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Sessions ranked by what they added to a project. Subagent trees fold into
/// their root session (the `session_contribution` view).
pub fn contribution(
    conn: &Connection,
    project_id: &str,
) -> Result<Vec<ContributionRow>, StoreError> {
    let mut stmt = conn.prepare_cached(
        "SELECT session_id, project_id, tokens, commits, edits, loc
         FROM session_contribution WHERE project_id = ?1
         ORDER BY tokens DESC",
    )?;
    let rows = stmt.query_map(params![project_id], |row| {
        Ok(ContributionRow {
            session_id: row.get(0)?,
            project_id: row.get(1)?,
            tokens: row.get(2)?,
            commits: row.get(3)?,
            edits: row.get(4)?,
            loc: row.get(5)?,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Full-text search over message content, tool inputs, and tool results.
pub fn search(conn: &Connection, query: &str, limit: i64) -> Result<Vec<SearchHit>, StoreError> {
    let mut stmt = conn.prepare_cached(
        "SELECT m.timeline_id, m.session_id,
                snippet(message_fts, -1, '[', ']', '…', 12)
         FROM message_fts
         JOIN message m ON m.id = message_fts.rowid
         WHERE message_fts MATCH ?1
         ORDER BY rank LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![query, limit], |row| {
        Ok(SearchHit {
            timeline_id: row.get(0)?,
            session_id: row.get(1)?,
            snippet: row.get(2)?,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// The stored driver checkpoint for one source file, with its cursor rowid
/// (the provenance anchor messages reference).
pub fn checkpoint(
    conn: &Connection,
    runtime_id: &str,
    path: &str,
) -> Result<Option<(i64, String)>, StoreError> {
    let row = conn
        .prepare_cached(
            "SELECT id, checkpoint FROM ingest_cursor WHERE runtime_id = ?1 AND path = ?2",
        )?
        .query_row(params![runtime_id, path], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .optional()?;
    Ok(row)
}
