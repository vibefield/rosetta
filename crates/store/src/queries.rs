//! Typed read surface — consumers never hand-write SQL against the
//! projection. Heatmap and contribution are the two usage features from
//! docs/canonical-model.html; higher scopes are query-time sums through
//! `session` (session-grain-only cache).

use rusqlite::{Connection, OptionalExtension, params};

use rosetta_core::rows::{
    MessageKind, MessageRow, Ms, ProjectRow, RuntimeRow, SessionRole, SessionRow, StreamState,
    TokenSnapshot, ToolCall,
};

use crate::StoreError;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct HeatmapDay {
    /// Date in the caller-selected timezone, `YYYY-MM-DD`.
    pub day: String,
    pub tokens: i64,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct ModelDay {
    /// Date in the caller-selected timezone, `YYYY-MM-DD`.
    pub day: String,
    /// Model id as the agent reported it; `unknown` when it never did.
    pub model: String,
    pub tokens: i64,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct HourTotal {
    /// Hour of day 0–23 in the caller-selected timezone.
    pub hour: i64,
    pub tokens: i64,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct ContributionRow {
    pub session_id: String,
    pub project_id: Option<String>,
    pub tokens: i64,
    pub commits: i64,
    pub edits: i64,
    pub loc: i64,
    /// Message rows across the session's whole subagent tree, folded into the
    /// root the same way the view folds tokens.
    pub messages: i64,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct SearchHit {
    pub timeline_id: String,
    pub session_id: String,
    pub snippet: String,
    /// Raw FTS5 bm25 rank (lower is better). Diagnostic only — scores have
    /// no cross-backend meaning and must never be compared across engines.
    pub score: f64,
}

/// Structured search filters. Filtering happens inside the ranked query —
/// retrieving top K and filtering afterwards would be ranking-incorrect.
#[derive(Debug, Clone, Default)]
pub struct SearchFilter {
    pub session: Option<String>,
    pub runtime: Option<String>,
    /// Runtime family (`claude-code`, `codex`, …), distinct from one concrete
    /// runtime id.
    pub agent: Option<String>,
    pub project: Option<String>,
    /// Message kind, in wire form (`user`, `agent`, `tool_use`, …).
    pub kind: Option<String>,
    /// Tool terminal state. `Some` excludes rows that never reported a tool
    /// error state; `false` is distinct from unknown.
    pub is_error: Option<bool>,
    pub after_ts: Option<Ms>,
    pub before_ts: Option<Ms>,
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

fn project_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProjectRow> {
    let roots: String = row.get("root_paths")?;
    Ok(ProjectRow {
        id: row.get("id")?,
        name: row.get("name")?,
        root_paths: serde_json::from_str(&roots).unwrap_or_default(),
        git_repo: row.get("git_repo")?,
    })
}

fn json_column(row: &rusqlite::Row<'_>, name: &str) -> rusqlite::Result<Option<serde_json::Value>> {
    Ok(row
        .get::<_, Option<String>>(name)?
        .and_then(|text| serde_json::from_str(&text).ok()))
}

fn runtime_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RuntimeRow> {
    Ok(RuntimeRow {
        id: row.get("id")?,
        kind: row.get("kind")?,
        data_root: row.get("data_root")?,
        enabled: row.get("enabled")?,
        account: json_column(row, "account")?,
        config: json_column(row, "config")?,
        skills: json_column(row, "skills")?,
        versions_seen: json_column(row, "versions_seen")?,
        discovered_at: row.get("discovered_at")?,
    })
}

pub fn project(conn: &Connection, project_id: &str) -> Result<Option<ProjectRow>, StoreError> {
    Ok(conn
        .prepare_cached("SELECT * FROM project WHERE id = ?1")?
        .query_row(params![project_id], project_from_row)
        .optional()?)
}

pub fn runtime(conn: &Connection, runtime_id: &str) -> Result<Option<RuntimeRow>, StoreError> {
    Ok(conn
        .prepare_cached("SELECT * FROM agent_runtime WHERE id = ?1")?
        .query_row(params![runtime_id], runtime_from_row)
        .optional()?)
}

/// The whole metadata surface, for warming the hot tier's mirror. These are
/// full-table scans by design — the mirror is fully resident.
pub fn all_sessions(conn: &Connection) -> Result<Vec<SessionRow>, StoreError> {
    let mut stmt = conn.prepare_cached("SELECT * FROM session ORDER BY id")?;
    let rows = stmt.query_map([], session_from_row)?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::from)
}

pub fn all_projects(conn: &Connection) -> Result<Vec<ProjectRow>, StoreError> {
    let mut stmt = conn.prepare_cached("SELECT * FROM project ORDER BY id")?;
    let rows = stmt.query_map([], project_from_row)?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::from)
}

pub fn all_runtimes(conn: &Connection) -> Result<Vec<RuntimeRow>, StoreError> {
    let mut stmt = conn.prepare_cached("SELECT * FROM agent_runtime ORDER BY id")?;
    let rows = stmt.query_map([], runtime_from_row)?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::from)
}

pub fn cursor_state(
    conn: &Connection,
    runtime_id: &str,
    path: &str,
) -> Result<Option<CursorState>, StoreError> {
    let raw = conn
        .prepare_cached(
            "SELECT c.id, c.checkpoint, c.active_generation,
                    c.highest_generation, c.session_id
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

/// Existing virtual sources beneath one physical database path. Snapshot
/// admission uses these to retain tombstones long enough for an empty
/// generation to sweep sessions that disappeared from the authoritative DB.
pub fn cursor_sources_with_prefix(
    conn: &Connection,
    runtime_id: &str,
    path_prefix: &str,
) -> Result<Vec<(String, String, String)>, StoreError> {
    let escaped = path_prefix
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    let pattern = format!("{escaped}%");
    let mut statement = conn.prepare_cached(
        "SELECT path, stream, session_id
         FROM ingest_cursor
         WHERE runtime_id = ?1 AND path LIKE ?2 ESCAPE '\\'
         ORDER BY path",
    )?;
    let rows = statement.query_map(params![runtime_id, pattern], |row| {
        Ok((row.get(0)?, row.get(1)?, row.get(2)?))
    })?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::from)
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

/// The heatmap's daily totals split by model — the same `usage_bucket` sum
/// with `model` kept in the grouping, so per-model days always add back to
/// [`heatmap_days_in_timezone`]'s figure for the same window and offset.
pub fn model_days_in_timezone(
    conn: &Connection,
    project_id: &str,
    since_ms: Ms,
    offset_minutes: i32,
) -> Result<Vec<ModelDay>, StoreError> {
    let mut stmt = conn.prepare_cached(
        "SELECT date((b.hour_ts + ?3 * 60000) / 1000, 'unixepoch') AS day, b.model,
                SUM(COALESCE(b.in_tokens,0) + COALESCE(b.out_tokens,0)
                    + COALESCE(b.cache_r_tokens,0) + COALESCE(b.cache_w_tokens,0)
                    + COALESCE(b.think_tokens,0)) AS tokens
         FROM usage_bucket b
         JOIN session s ON s.id = b.session_id
         WHERE s.project_id = ?1 AND b.hour_ts >= ?2
         GROUP BY day, b.model ORDER BY day, b.model",
    )?;
    let rows = stmt.query_map(params![project_id, since_ms, offset_minutes], |row| {
        Ok(ModelDay {
            day: row.get(0)?,
            model: row.get(1)?,
            tokens: row.get(2)?,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Tokens by hour of day (0–23) over the window — which hours a project's
/// work actually happens in. Hours with no usage are absent, not zero.
pub fn hour_profile_in_timezone(
    conn: &Connection,
    project_id: &str,
    since_ms: Ms,
    offset_minutes: i32,
) -> Result<Vec<HourTotal>, StoreError> {
    let mut stmt = conn.prepare_cached(
        "SELECT CAST(strftime('%H', (b.hour_ts + ?3 * 60000) / 1000, 'unixepoch') AS INTEGER) AS hour,
                SUM(COALESCE(b.in_tokens,0) + COALESCE(b.out_tokens,0)
                    + COALESCE(b.cache_r_tokens,0) + COALESCE(b.cache_w_tokens,0)
                    + COALESCE(b.think_tokens,0)) AS tokens
         FROM usage_bucket b
         JOIN session s ON s.id = b.session_id
         WHERE s.project_id = ?1 AND b.hour_ts >= ?2
         GROUP BY hour ORDER BY hour",
    )?;
    let rows = stmt.query_map(params![project_id, since_ms, offset_minutes], |row| {
        Ok(HourTotal {
            hour: row.get(0)?,
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
        "SELECT c.session_id, c.project_id, c.tokens, c.commits, c.edits, c.loc,
                COALESCE(m.messages, 0) AS messages
         FROM session_contribution c
         LEFT JOIN (SELECT s.root_session_id rid, COUNT(*) messages
                    FROM message msg JOIN session s ON s.id = msg.session_id
                    GROUP BY rid) m ON m.rid = c.session_id
         WHERE c.project_id = ?1
         ORDER BY c.tokens DESC",
    )?;
    let rows = stmt.query_map(params![project_id], |row| {
        Ok(ContributionRow {
            session_id: row.get(0)?,
            project_id: row.get(1)?,
            tokens: row.get(2)?,
            commits: row.get(3)?,
            edits: row.get(4)?,
            loc: row.get(5)?,
            messages: row.get(6)?,
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
    search_filtered(conn, query, limit, &SearchFilter::default())
}

/// Ranked search with structured filters applied inside the query, so LIMIT
/// runs after filtering and ranking stays correct. The filter combinations
/// are finite, so the prepared-statement cache still applies.
pub fn search_filtered(
    conn: &Connection,
    query: &str,
    limit: i64,
    filter: &SearchFilter,
) -> Result<Vec<SearchHit>, StoreError> {
    use rusqlite::types::Value;

    let mut sql = String::from(
        "SELECT m.timeline_id, m.session_id,
                snippet(message_fts, -1, '[', ']', '…', 12), rank
         FROM message_fts
         JOIN message m ON m.id = message_fts.rowid
         JOIN session s ON s.id = m.session_id",
    );
    if filter.agent.is_some() {
        sql.push_str(" JOIN agent_runtime r ON r.id = s.runtime_id");
    }
    sql.push_str(" WHERE message_fts MATCH ?1");
    let mut binds: Vec<Value> = vec![Value::from(query.to_string())];
    let clause = |sql: &mut String, predicate: &str, value: Value, binds: &mut Vec<Value>| {
        binds.push(value);
        sql.push_str(&format!(" AND {predicate} ?{}", binds.len()));
    };
    if let Some(session) = &filter.session {
        clause(
            &mut sql,
            "m.session_id =",
            Value::from(session.clone()),
            &mut binds,
        );
    }
    if let Some(runtime) = &filter.runtime {
        clause(
            &mut sql,
            "s.runtime_id =",
            Value::from(runtime.clone()),
            &mut binds,
        );
    }
    if let Some(agent) = &filter.agent {
        clause(&mut sql, "r.kind =", Value::from(agent.clone()), &mut binds);
    }
    if let Some(project) = &filter.project {
        clause(
            &mut sql,
            "s.project_id =",
            Value::from(project.clone()),
            &mut binds,
        );
    }
    if let Some(kind) = &filter.kind {
        clause(&mut sql, "m.type =", Value::from(kind.clone()), &mut binds);
    }
    if let Some(is_error) = filter.is_error {
        clause(
            &mut sql,
            "m.tool_is_error =",
            Value::from(i64::from(is_error)),
            &mut binds,
        );
    }
    if let Some(after) = filter.after_ts {
        clause(&mut sql, "m.ts >=", Value::from(after), &mut binds);
    }
    if let Some(before) = filter.before_ts {
        clause(&mut sql, "m.ts <=", Value::from(before), &mut binds);
    }
    binds.push(Value::from(limit));
    // FTS5 may assign the same BM25 score to many documents. Keep the wire
    // contract deterministic at the LIMIT boundary instead of inheriting
    // incidental rowid/insertion order from the current projection build.
    sql.push_str(&format!(
        " ORDER BY rank, m.timeline_id COLLATE BINARY LIMIT ?{}",
        binds.len()
    ));

    let mut stmt = conn.prepare_cached(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(binds), |row| {
        Ok(SearchHit {
            timeline_id: row.get(0)?,
            session_id: row.get(1)?,
            snippet: row.get(2)?,
            score: row.get(3)?,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Exact commit watermark for one session's presentation order, per the L3
/// pagination rule: cursors bind this value and EXPIRE on mismatch — never a
/// silent rebase. It moves when any of the session's sources activates a
/// generation (sweep + renumber) or the cursor set itself changes; plain
/// appends leave it stable, so backward paging survives live appends.
pub fn session_watermark(conn: &Connection, session_id: &str) -> Result<String, StoreError> {
    Ok(conn
        .prepare_cached(
            "SELECT COUNT(*), COALESCE(SUM(active_generation), 0), COALESCE(MAX(id), 0)
             FROM ingest_cursor WHERE session_id = ?1",
        )?
        .query_row(params![session_id], |row| {
            Ok(format!(
                "{}.{}.{}",
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?
            ))
        })?)
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
