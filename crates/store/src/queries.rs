//! Typed read surface — consumers never hand-write SQL against the
//! projection. Heatmap and contribution are the two usage features from
//! docs/canonical-model.html; higher scopes are query-time sums through
//! `session` (session-grain-only cache).

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{Connection, OptionalExtension, params};

use rosetta_core::rows::{Ms, ProjectRow, RuntimeRow, SessionRole, SessionRow, StreamState};
use rosetta_core::timeline::{
    ResourceRef, SearchMember, WORKFLOW_ATTEMPT_LIMIT, WORKFLOW_JOURNAL_LIMIT,
    WORKFLOW_RUNTIME_PROFILE_LIMIT,
};
use rosetta_core::{
    TimelineBody, TimelineEntry, ToolAction, WorkflowAttempt, WorkflowJournalObservation,
    WorkflowRun,
};

use crate::{StoreError, message_codec};

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
    /// Canonical wire kind of the matched row (`user`, `agent`, `tool_use`,
    /// …): the search sheet labels who said it without a second read.
    pub kind: String,
    /// The matched row's timestamp in milliseconds, when the source reported
    /// one. Never coerced to epoch zero.
    pub at: Option<Ms>,
    pub snippet: String,
    /// Raw FTS5 bm25 rank (lower is better). Diagnostic only — scores have
    /// no cross-backend meaning and must never be compared across engines.
    pub score: f64,
}

/// Content-free response evidence used to describe a child session's actual
/// runtime without shipping its private transcript into the parent document.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct ResponseRuntime {
    pub response_id: String,
    pub model: Option<String>,
    pub effort: Option<String>,
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

/// Durable identity and publication/search watermarks for one disposable
/// canonical projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionState {
    pub projection_epoch: String,
    pub search_revision: u64,
    pub canonical_ready: bool,
}

/// One coalesced repair key consumed by the detached search worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchRepair {
    pub kind: String,
    pub key: String,
    pub revision: u64,
}

/// Complete search document materialized from canonical rows. Search owns its
/// physical index, while SQLite plus its verified projection members remain
/// the only source from which a document is built or repaired.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchDocument {
    pub id: i64,
    pub timeline_id: String,
    pub session_id: String,
    pub runtime_id: String,
    pub agent: String,
    pub project_id: Option<String>,
    pub ts: Option<Ms>,
    pub kind: String,
    pub is_error: Option<bool>,
    /// The detached worker resolves these committed UTF-8 members into their
    /// declared semantic lanes before FTS insertion. The strings below retain
    /// bounded previews for diagnostic in-process queries.
    pub members: Vec<SearchMember>,
    pub prose: String,
    pub target: String,
    pub intent: String,
    pub change: String,
    pub tool: String,
}

/// Minimal raw-ledger address for one shell/code timeline row. The exact
/// command is intentionally absent: callers hand this anchor to the ingest
/// engine, which reuses the owning adapter to recover it on demand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSource {
    pub cursor_id: i64,
    pub runtime_id: String,
    pub agent_kind: String,
    pub relative_path: String,
    pub session_id: String,
    pub generation: u64,
    pub offset: u64,
    pub timeline_id: String,
    pub correlation_key: String,
}

/// The live file named by one canonical file-bearing target.
///
/// The transport addresses this by session, timeline row and target index;
/// callers never submit a path of their own. `cwd` is retained only so a
/// relative target can be resolved exactly where the recorded action happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileReadTarget {
    pub path: String,
    pub cwd: Option<String>,
}

pub fn file_read_target(
    conn: &Connection,
    session_id: &str,
    timeline_id: &str,
    target_index: usize,
) -> Result<Option<FileReadTarget>, StoreError> {
    let raw = conn
        .prepare_cached(
            "SELECT m.*, s.cwd AS file_read_cwd
             FROM message m
             JOIN session s ON s.id = m.session_id
             WHERE m.session_id = ?1 AND m.timeline_id = ?2
               AND m.type = 'tool_use'",
        )?
        .query_row(params![session_id, timeline_id], |row| {
            Ok((
                message_codec::decode(row)?,
                row.get::<_, Option<String>>("file_read_cwd")?,
            ))
        })
        .optional()?;

    let Some((entry, cwd)) = raw else {
        return Ok(None);
    };
    let mut entries = vec![entry];
    hydrate_workflows(conn, session_id, &mut entries)?;
    let Some(entry) = entries.pop() else {
        return Ok(None);
    };
    let TimelineBody::ToolUse { tool } = entry.body else {
        return Ok(None);
    };
    let path =
        match tool.action {
            rosetta_core::ToolAction::FileRead { targets, .. } => targets
                .get(target_index)
                .and_then(|target| match &target.resource {
                    ResourceRef::File { path, .. } => Some(path.clone()),
                    _ => None,
                }),
            rosetta_core::ToolAction::Artifact { resources, .. } => resources
                .get(target_index)
                .and_then(|resource| match resource {
                    ResourceRef::File { path, .. } => Some(path.clone()),
                    _ => None,
                }),
            rosetta_core::ToolAction::Workflow { path, run, .. } => {
                let mut resources = Vec::new();
                if let Some(script) = run
                    .as_ref()
                    .and_then(|run| run.script_path.as_ref())
                    .or(path.as_ref())
                {
                    resources.push(script.clone());
                }
                if let Some(run) = run {
                    for resource in run.resources {
                        if !resources.contains(&resource) {
                            resources.push(resource);
                        }
                    }
                }
                resources
                    .get(target_index)
                    .and_then(|resource| match resource {
                        ResourceRef::File { path, .. } => Some(path.clone()),
                        _ => None,
                    })
            }
            _ => None,
        };
    let Some(path) = path else {
        return Ok(None);
    };
    Ok(Some(FileReadTarget { path, cwd }))
}

pub fn command_source(
    conn: &Connection,
    session_id: &str,
    timeline_id: &str,
) -> Result<Option<CommandSource>, StoreError> {
    let raw = conn
        .prepare_cached(
            "SELECT c.id, c.runtime_id, r.kind, c.path, c.session_id,
                    c.active_generation, m.src_offset, m.tool_correlation_key
             FROM message m
             JOIN ingest_cursor c ON c.id = m.src_cursor
             JOIN agent_runtime r ON r.id = c.runtime_id
             WHERE m.session_id = ?1 AND m.timeline_id = ?2
               AND m.type = 'tool_use'
               AND m.tool_kind IN ('shell', 'code_execute')",
        )?
        .query_row(params![session_id, timeline_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, String>(7)?,
            ))
        })
        .optional()?;
    raw.map(
        |(
            cursor_id,
            runtime_id,
            agent_kind,
            relative_path,
            source_session_id,
            generation,
            offset,
            correlation_key,
        )| {
            let generation = u64::try_from(generation).map_err(|_| StoreError::SourceState {
                cursor_id,
                message: "active generation is negative".to_string(),
            })?;
            let offset = u64::try_from(offset).map_err(|_| StoreError::SourceState {
                cursor_id,
                message: "command source offset is negative".to_string(),
            })?;
            Ok(CommandSource {
                cursor_id,
                runtime_id,
                agent_kind,
                relative_path,
                session_id: source_session_id,
                generation,
                offset,
                timeline_id: timeline_id.to_owned(),
                correlation_key,
            })
        },
    )
    .transpose()
}

pub fn projection_state(conn: &Connection) -> Result<ProjectionState, StoreError> {
    let (projection_epoch, revision, canonical_ready): (String, i64, bool) = conn.query_row(
        "SELECT projection_epoch, search_revision, canonical_ready
         FROM projection_state WHERE singleton = 1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    let search_revision = u64::try_from(revision).map_err(|_| StoreError::ProjectionInvalid {
        message: format!("negative search revision {revision}"),
    })?;
    Ok(ProjectionState {
        projection_epoch,
        search_revision,
        canonical_ready,
    })
}

/// Repairs newer than `after_revision`, bounded by one canonical snapshot.
/// A key coalesced to a still-newer revision is intentionally excluded and
/// survives acknowledgement of the older watermark.
pub fn search_repairs(
    conn: &Connection,
    after_revision: u64,
    through_revision: u64,
) -> Result<Vec<SearchRepair>, StoreError> {
    let after = i64::try_from(after_revision).map_err(|_| StoreError::ProjectionInvalid {
        message: format!("search revision {after_revision} exceeds SQLite's integer range"),
    })?;
    let through = i64::try_from(through_revision).map_err(|_| StoreError::ProjectionInvalid {
        message: format!("search revision {through_revision} exceeds SQLite's integer range"),
    })?;
    let mut statement = conn.prepare(
        "SELECT kind, key, revision
         FROM search_dirty
         WHERE revision > ?1 AND revision <= ?2
         ORDER BY revision, kind, key",
    )?;
    let rows = statement.query_map(params![after, through], |row| {
        let revision: i64 = row.get(2)?;
        Ok((row.get(0)?, row.get(1)?, revision))
    })?;
    let mut repairs = Vec::new();
    for row in rows {
        let (kind, key, revision) = row?;
        repairs.push(SearchRepair {
            kind,
            key,
            revision: u64::try_from(revision).map_err(|_| StoreError::ProjectionInvalid {
                message: format!("negative dirty search revision {revision}"),
            })?,
        });
    }
    Ok(repairs)
}

/// Search coverage policy (product decision, 2026-09-09): only prompts and
/// replies become search documents. Tool activity, thinking, summaries, team
/// traffic, and notices stay in the canonical projection and the transcript
/// but are never indexed — the extractor still knows how to lane them, so the
/// decision is one predicate, and reversing it is one edit here.
pub const SEARCH_KINDS: [&str; 2] = ["user", "agent"];

/// `column IN ('user', 'agent')`, for every query that must agree with
/// [`SEARCH_KINDS`]: the document visitor, the build denominator, and the
/// legacy in-projection FTS path.
pub fn search_kind_predicate(column: &str) -> String {
    let list = SEARCH_KINDS
        .iter()
        .map(|kind| format!("'{kind}'"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("{column} IN ({list})")
}

/// How many rows the detached search worker will index for this projection.
pub fn search_document_count(conn: &Connection) -> Result<u64, StoreError> {
    let count: i64 = conn.query_row(
        &format!(
            "SELECT count(*) FROM message WHERE {}",
            search_kind_predicate("type")
        ),
        [],
        |row| row.get(0),
    )?;
    Ok(u64::try_from(count).unwrap_or(0))
}

fn visit_search_documents_where(
    conn: &Connection,
    predicate: &str,
    session_id: Option<&str>,
    mut visit: impl FnMut(SearchDocument) -> Result<(), StoreError>,
) -> Result<(), StoreError> {
    let sql = format!(
        "SELECT m.*, s.runtime_id AS search_runtime_id, r.kind AS search_agent,
                s.project_id AS search_project_id
         FROM message m
         JOIN session s ON s.id = m.session_id
         JOIN agent_runtime r ON r.id = s.runtime_id
         WHERE {kinds} {predicate}
         ORDER BY m.id",
        kinds = search_kind_predicate("m.type"),
    );
    let mut statement = conn.prepare(&sql)?;
    let decoder = message_codec::Decoder::new(&statement)?;
    let mut rows = match session_id {
        Some(session_id) => statement.query(params![session_id])?,
        None => statement.query([])?,
    };
    while let Some(row) = rows.next()? {
        let entry = decoder.decode(row)?;
        let lanes = entry.search_lanes();
        let status = match &entry.body {
            TimelineBody::ToolUse { tool } => Some(tool.status.value),
            _ => None,
        };
        visit(SearchDocument {
            id: row.get("id")?,
            timeline_id: entry.timeline_id,
            session_id: entry.session_id,
            runtime_id: row.get("search_runtime_id")?,
            agent: row.get("search_agent")?,
            project_id: row.get("search_project_id")?,
            ts: row.get("ts")?,
            kind: row.get("type")?,
            is_error: status.and_then(|status| match status {
                rosetta_core::ToolStatus::Succeeded => Some(false),
                rosetta_core::ToolStatus::Failed
                | rosetta_core::ToolStatus::Denied
                | rosetta_core::ToolStatus::Cancelled
                | rosetta_core::ToolStatus::TimedOut
                | rosetta_core::ToolStatus::Interrupted => Some(true),
                rosetta_core::ToolStatus::Pending
                | rosetta_core::ToolStatus::Running
                | rosetta_core::ToolStatus::Unknown => None,
            }),
            members: lanes.members,
            prose: lanes.prose,
            target: lanes.target,
            intent: lanes.intent,
            change: lanes.change,
            tool: lanes.tool,
        })?;
    }
    Ok(())
}

pub fn visit_search_documents(
    conn: &Connection,
    visit: impl FnMut(SearchDocument) -> Result<(), StoreError>,
) -> Result<(), StoreError> {
    visit_search_documents_where(conn, "", None, visit)
}

pub fn visit_session_search_documents(
    conn: &Connection,
    session_id: &str,
    visit: impl FnMut(SearchDocument) -> Result<(), StoreError>,
) -> Result<(), StoreError> {
    visit_search_documents_where(conn, "AND m.session_id = ?1", Some(session_id), visit)
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

fn session_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SessionRow> {
    let role: String = row.get("role")?;
    let state: String = row.get("stream_state")?;
    let delegation = row
        .get::<_, Option<String>>("delegation")?
        .map(|value| serde_json::from_str(&value))
        .transpose()
        .map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?;
    Ok(SessionRow {
        id: row.get("id")?,
        native_session_id: row.get("native_session_id")?,
        runtime_id: row.get("runtime_id")?,
        project_id: row.get("project_id")?,
        cwd: row.get("cwd")?,
        parent_session_id: row.get("parent_session_id")?,
        delegation,
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
                 ORDER BY updated_at DESC, id LIMIT ?2",
            )?;
            let rows = stmt.query_map(params![project, limit], session_from_row)?;
            for row in rows {
                out.push(row?);
            }
        }
        None => {
            let mut stmt =
                conn.prepare_cached("SELECT * FROM session ORDER BY updated_at DESC, id LIMIT ?1")?;
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
) -> Result<Vec<TimelineEntry>, StoreError> {
    let mut stmt = conn.prepare_cached(
        "SELECT * FROM (
           SELECT * FROM message
           WHERE session_id = ?1 AND ordinal < ?2
           ORDER BY ordinal DESC LIMIT ?3
         ) ORDER BY ordinal ASC",
    )?;
    let rows = stmt.query_map(
        params![session_id, before.unwrap_or(i64::MAX), limit],
        message_codec::decode,
    )?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    hydrate_workflows(conn, session_id, &mut out)?;
    Ok(out)
}

/// Complete shared-boundary history for one team member plus content-free
/// runtime observations. Unlike the live tail this is deliberately unpaged:
/// it selects only collaboration rows, never private worker prose/tools.
pub fn collaboration_timeline(
    conn: &Connection,
    session_id: &str,
) -> Result<(Vec<TimelineEntry>, Vec<ResponseRuntime>), StoreError> {
    let mut entries_stmt = conn.prepare_cached(
        "SELECT * FROM message
         WHERE session_id = ?1 AND type = 'collaboration'
         ORDER BY ordinal ASC, timeline_id ASC",
    )?;
    let rows = entries_stmt.query_map(params![session_id], message_codec::decode)?;
    let mut entries = Vec::new();
    for row in rows {
        entries.push(row?);
    }

    let mut runtime_stmt = conn.prepare_cached(
        "SELECT response_id, model, effort
         FROM message
         WHERE session_id = ?1
           AND response_id IS NOT NULL
           AND (model IS NOT NULL OR effort IS NOT NULL)
         GROUP BY response_id, model, effort
         ORDER BY MIN(ordinal) ASC, response_id ASC, model ASC, effort ASC",
    )?;
    let rows = runtime_stmt.query_map(params![session_id], |row| {
        Ok(ResponseRuntime {
            response_id: row.get(0)?,
            model: row.get(1)?,
            effort: row.get(2)?,
        })
    })?;
    let mut runtimes = Vec::new();
    for row in rows {
        runtimes.push(row?);
    }
    Ok((entries, runtimes))
}

/// Read only the runtime fields affected by a usage update. Collaboration
/// subscribers never need to decode a private tool or hydrate its workflow.
pub fn response_runtime(
    conn: &Connection,
    session_id: &str,
    timeline_id: &str,
) -> Result<Option<ResponseRuntime>, StoreError> {
    Ok(conn
        .query_row(
            "SELECT response_id, model, effort FROM message
         WHERE session_id = ?1 AND timeline_id = ?2 AND response_id IS NOT NULL
           AND (model IS NOT NULL OR effort IS NOT NULL)",
            params![session_id, timeline_id],
            |row| {
                Ok(ResponseRuntime {
                    response_id: row.get(0)?,
                    model: row.get(1)?,
                    effort: row.get(2)?,
                })
            },
        )
        .optional()?)
}

/// One exact canonical timeline row, scoped to its owning session. This is
/// the lazy landing path for indexes such as `timeline_meta`; callers do not
/// need to walk every intervening history page to reach an old anchor.
pub fn timeline_entry(
    conn: &Connection,
    session_id: &str,
    timeline_id: &str,
) -> Result<Option<TimelineEntry>, StoreError> {
    let entry = conn
        .prepare_cached("SELECT * FROM message WHERE session_id = ?1 AND timeline_id = ?2")?
        .query_row(params![session_id, timeline_id], message_codec::decode)
        .optional()
        .map_err(StoreError::from)?;
    let Some(entry) = entry else {
        return Ok(None);
    };
    let mut entries = vec![entry];
    hydrate_workflows(conn, session_id, &mut entries)?;
    Ok(entries.pop())
}

/// Resolve a report's recorded bounds without accepting caller-supplied paths
/// or ordinal ranges. All reads must share the caller's read transaction.
pub fn report_work(
    conn: &Connection,
    session_id: &str,
    timeline_id: &str,
) -> Result<Option<Vec<TimelineEntry>>, StoreError> {
    if session(conn, session_id)?
        .is_none_or(|row| row.stream_state == rosetta_core::rows::StreamState::Pending)
    {
        return Ok(None);
    }
    let Some(report) = timeline_entry(conn, session_id, timeline_id)? else {
        return Ok(None);
    };
    let TimelineBody::Collaboration { event } = report.body else {
        return Ok(None);
    };
    if event.kind != rosetta_core::timeline::CollaborationEventKind::Report {
        return Ok(None);
    }
    let (Some(source), Some(response), Some(start), Some(end)) = (
        event.source_session_id,
        event.source_response_id,
        event.work_start_timeline_id,
        event.work_end_timeline_id,
    ) else {
        return Ok(None);
    };
    let Some(source_session) = session(conn, &source)? else {
        return Ok(None);
    };
    if source_session.stream_state == rosetta_core::rows::StreamState::Pending {
        return Ok(None);
    }
    let ordinal = |id: &str| -> Result<Option<i64>, StoreError> {
        Ok(conn
            .query_row(
                "SELECT ordinal FROM message WHERE session_id = ?1 AND timeline_id = ?2",
                params![source, id],
                |row| row.get(0),
            )
            .optional()?)
    };
    let (Some(start), Some(end)) = (ordinal(&start)?, ordinal(&end)?) else {
        return Ok(None);
    };
    if end < start {
        return Ok(None);
    }
    let mut statement = conn.prepare_cached(
        "SELECT * FROM message WHERE session_id = ?1 AND ordinal BETWEEN ?2 AND ?3 ORDER BY ordinal, timeline_id"
    )?;
    let mut entries = statement
        .query_map(params![source, start, end], message_codec::decode)?
        .collect::<Result<Vec<_>, _>>()?;
    if !entries
        .iter()
        .any(|row| row.response.as_ref().is_some_and(|r| r.id == response))
    {
        return Ok(None);
    }
    hydrate_workflows(conn, &source, &mut entries)?;
    Ok(Some(entries))
}

/// Join the generation-owned Workflow document onto each chronological
/// invocation that proved the same native run id. Runtime profiles come from
/// child transcripts and therefore override neither configured values nor one
/// another; the UI can honestly say "varies" when multiple were observed.
fn hydrate_workflows(
    conn: &Connection,
    session_id: &str,
    entries: &mut [TimelineEntry],
) -> Result<(), StoreError> {
    let wanted = entries
        .iter()
        .filter_map(|entry| match &entry.body {
            TimelineBody::ToolUse { tool } => match &tool.action {
                ToolAction::Workflow {
                    run_id: Some(run_id),
                    ..
                } => Some(run_id.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    if wanted.is_empty() {
        return Ok(());
    }

    let mut runs = BTreeMap::<String, WorkflowRun>::new();
    for run_id in &wanted {
        let detail = conn
            .prepare_cached(
                "SELECT detail FROM workflow_run WHERE session_id = ?1 AND run_id = ?2",
            )?
            .query_row(params![session_id, run_id], |row| row.get::<_, String>(0))
            .optional()?;
        let Some(detail) = detail else {
            continue;
        };
        let mut run = serde_json::from_str::<WorkflowRun>(&detail)?;
        hydrate_workflow_attempts(conn, session_id, &mut run)?;
        hydrate_workflow_journal(conn, session_id, &mut run)?;
        runs.insert(run_id.clone(), run);
    }
    if runs.is_empty() {
        return Ok(());
    }

    let mut profiles = BTreeMap::<String, (Vec<String>, Vec<String>)>::new();
    let mut statement = conn.prepare_cached(
        "SELECT m.session_id, m.model, m.effort
         FROM message m
         JOIN session child ON child.id = m.session_id
         WHERE child.parent_session_id = ?1
           AND (m.model IS NOT NULL OR m.effort IS NOT NULL)
         ORDER BY m.id",
    )?;
    let rows = statement.query_map(params![session_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, Option<String>>(2)?,
        ))
    })?;
    for row in rows {
        let (child_session_id, model, effort) = row?;
        let profile = profiles.entry(child_session_id).or_default();
        if let Some(model) = model {
            push_unique(&mut profile.0, model);
        }
        if let Some(effort) = effort {
            push_unique(&mut profile.1, effort);
        }
    }
    drop(statement);

    for run in runs.values_mut() {
        for worker in &mut run.workers {
            let Some(child_session_id) = worker.child_session_id.as_ref() else {
                continue;
            };
            if let Some((models, efforts)) = profiles.get(child_session_id) {
                worker.observed_models.clone_from(models);
                worker.observed_efforts.clone_from(efforts);
            }
        }
        run.validate()
            .map_err(|error| StoreError::ProjectionInvalid {
                message: error.to_string(),
            })?;
    }

    for entry in entries {
        let TimelineBody::ToolUse { tool } = &mut entry.body else {
            continue;
        };
        let ToolAction::Workflow { run_id, run, .. } = &mut tool.action else {
            continue;
        };
        *run = run_id
            .as_ref()
            .and_then(|run_id| runs.get(run_id))
            .cloned()
            .map(Box::new);
    }
    Ok(())
}

/// Hydrate every ordinary attempt independent of the caller's timeline page.
/// A pathological native history is capped for wire safety while the exact
/// total remains visible; real Workflow runs are far below this boundary.
fn hydrate_workflow_attempts(
    conn: &Connection,
    session_id: &str,
    run: &mut WorkflowRun,
) -> Result<(), StoreError> {
    let total = conn
        .prepare_cached(
            "SELECT COUNT(*) FROM message
         WHERE session_id = ?1 AND type = 'tool_use' AND tool_kind = 'workflow'
           AND json_extract(tool_detail, '$.run_id') = ?2",
        )?
        .query_row(params![session_id, run.run_id], |row| row.get::<_, i64>(0))?;
    run.attempt_total_count = u64::try_from(total).map_err(|_| StoreError::ProjectionInvalid {
        message: "workflow attempt count is negative".into(),
    })?;

    let mut statement = conn.prepare_cached(
        "SELECT * FROM (
           SELECT * FROM message
           WHERE session_id = ?1 AND type = 'tool_use' AND tool_kind = 'workflow'
             AND json_extract(tool_detail, '$.run_id') = ?2
           ORDER BY ordinal DESC LIMIT ?3
         ) ORDER BY ordinal ASC",
    )?;
    let rows = statement.query_map(
        params![
            session_id,
            run.run_id,
            i64::try_from(WORKFLOW_ATTEMPT_LIMIT).unwrap_or(i64::MAX),
        ],
        message_codec::decode,
    )?;
    run.attempts.clear();
    for row in rows {
        let entry = row?;
        let TimelineBody::ToolUse { tool } = &entry.body else {
            continue;
        };
        let ToolAction::Workflow {
            task_id,
            resume_id,
            attempt,
            callback_summary,
            ..
        } = &tool.action
        else {
            continue;
        };
        let started_at = tool
            .timing
            .as_ref()
            .and_then(|timing| timing.started_at)
            .or(entry.at);
        let completed_at = tool.timing.as_ref().and_then(|timing| timing.completed_at);
        let duration_ms = tool
            .timing
            .as_ref()
            .and_then(|timing| timing.duration_ms)
            .or_else(|| {
                completed_at
                    .zip(started_at)
                    .and_then(|(completed, started)| completed.checked_sub(started))
                    .and_then(|value| u64::try_from(value).ok())
            });
        let attempt = attempt.unwrap_or(1);
        run.attempts.push(WorkflowAttempt {
            timeline_id: entry.timeline_id,
            attempt,
            task_id: task_id.clone(),
            resumed: attempt > 1 || resume_id.is_some(),
            status: tool.status.value,
            at: started_at,
            completed_at,
            duration_ms,
            summary: callback_summary.clone(),
        });
    }
    Ok(())
}

/// Keep only the latest bounded observation window on the wire. Aggregate
/// counts come from all durable observations, so a large journal never makes
/// the whole timeline unreadable and never under-reports progress.
fn hydrate_workflow_journal(
    conn: &Connection,
    session_id: &str,
    run: &mut WorkflowRun,
) -> Result<(), StoreError> {
    let (total, started, result) = conn.prepare_cached(
        "SELECT COUNT(*),
                COALESCE(SUM(CASE WHEN json_extract(detail, '$.kind') = 'started' THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN json_extract(detail, '$.kind') = 'result' THEN 1 ELSE 0 END), 0)
         FROM workflow_journal WHERE session_id = ?1 AND run_id = ?2",
    )?
    .query_row(params![session_id, run.run_id], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
        ))
    })?;
    let count = |value: i64, label: &str| {
        u64::try_from(value).map_err(|_| StoreError::ProjectionInvalid {
            message: format!("workflow journal {label} count is negative"),
        })
    };
    run.journal_total_count = count(total, "total")?;
    run.journal_started_count = count(started, "started")?;
    run.journal_result_count = count(result, "result")?;

    let mut statement = conn.prepare_cached(
        "SELECT detail FROM workflow_journal
         WHERE session_id = ?1 AND run_id = ?2
         ORDER BY COALESCE(CAST(json_extract(detail, '$.source_offset') AS INTEGER), -1) DESC,
                  observation_id DESC
         LIMIT ?3",
    )?;
    let rows = statement.query_map(
        params![
            session_id,
            run.run_id,
            i64::try_from(WORKFLOW_JOURNAL_LIMIT).unwrap_or(i64::MAX),
        ],
        |row| row.get::<_, String>(0),
    )?;
    run.journal.clear();
    for row in rows {
        run.journal
            .push(serde_json::from_str::<WorkflowJournalObservation>(&row?)?);
    }
    run.journal.sort_by(|left, right| {
        left.source_offset
            .cmp(&right.source_offset)
            .then_with(|| left.observation_id.cmp(&right.observation_id))
    });
    Ok(())
}

fn push_unique(values: &mut Vec<String>, value: String) {
    if values.len() < WORKFLOW_RUNTIME_PROFILE_LIMIT && !values.contains(&value) {
        values.push(value);
    }
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
    heatmap_days_in_timezone_for_scope(conn, Some(project_id), since_ms, offset_minutes)
}

/// Daily totals for either one persisted project (`Some(id)`) or the
/// intentionally project-less session bucket (`None`). `IS` preserves the
/// ordinary indexed equality lookup while also giving SQL NULL an explicit,
/// queryable meaning; no fake project row has to enter the canonical model.
pub fn heatmap_days_in_timezone_for_scope(
    conn: &Connection,
    project_id: Option<&str>,
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
         WHERE s.project_id IS ?1 AND s.stream_state != 'pending' AND b.hour_ts >= ?2
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
    model_days_in_timezone_for_scope(conn, Some(project_id), since_ms, offset_minutes)
}

/// Per-model daily totals for a real project or the project-less bucket.
pub fn model_days_in_timezone_for_scope(
    conn: &Connection,
    project_id: Option<&str>,
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
         WHERE s.project_id IS ?1 AND s.stream_state != 'pending' AND b.hour_ts >= ?2
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
    hour_profile_in_timezone_for_scope(conn, Some(project_id), since_ms, offset_minutes)
}

/// Hour-of-day totals for a real project or the project-less bucket.
pub fn hour_profile_in_timezone_for_scope(
    conn: &Connection,
    project_id: Option<&str>,
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
         WHERE s.project_id IS ?1 AND s.stream_state != 'pending' AND b.hour_ts >= ?2
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
/// their root session. Every aggregate is constrained through the project's
/// roots first: the presentation layer asks this once per project, so an
/// uncorrelated whole-corpus GROUP BY here becomes an accidental O(projects ×
/// corpus) cold-start tax.
pub fn contribution(
    conn: &Connection,
    project_id: &str,
) -> Result<Vec<ContributionRow>, StoreError> {
    contribution_for_scope(conn, Some(project_id))
}

/// Root-session contribution for a real project or the project-less bucket.
pub fn contribution_for_scope(
    conn: &Connection,
    project_id: Option<&str>,
) -> Result<Vec<ContributionRow>, StoreError> {
    let mut stmt = conn.prepare_cached(
        "WITH roots AS (
             SELECT id, project_id
             FROM session
             WHERE project_id IS ?1 AND id = root_session_id
               AND stream_state != 'pending'
         ),
         tree_sessions AS (
             SELECT s.root_session_id AS rid, s.id AS session_id
             FROM session s JOIN roots r ON r.id = s.root_session_id
         ),
         usage AS (
             SELECT t.rid,
                    SUM(COALESCE(b.in_tokens,0) + COALESCE(b.out_tokens,0)
                        + COALESCE(b.cache_r_tokens,0) + COALESCE(b.cache_w_tokens,0)
                        + COALESCE(b.think_tokens,0)) AS tokens
             FROM tree_sessions t JOIN usage_bucket b ON b.session_id = t.session_id
             GROUP BY t.rid
         ),
         commits AS (
             SELECT t.rid, COUNT(*) AS commits
             FROM tree_sessions t JOIN git_commit g ON g.session_id = t.session_id
             GROUP BY t.rid
         ),
         edits AS (
             SELECT t.rid, COUNT(*) AS edits,
                    SUM(COALESCE(f.lines_added, 0) + COALESCE(f.lines_removed, 0)) AS loc
             FROM tree_sessions t JOIN file_edit f ON f.session_id = t.session_id
             GROUP BY t.rid
         ),
         messages AS (
             SELECT t.rid, COUNT(*) AS messages
             FROM tree_sessions t JOIN message msg ON msg.session_id = t.session_id
             GROUP BY t.rid
         )
         SELECT r.id, r.project_id, COALESCE(u.tokens, 0), COALESCE(c.commits, 0),
                COALESCE(e.edits, 0), COALESCE(e.loc, 0), COALESCE(m.messages, 0)
         FROM roots r
         LEFT JOIN usage u ON u.rid = r.id
         LEFT JOIN commits c ON c.rid = r.id
         LEFT JOIN edits e ON e.rid = r.id
         LEFT JOIN messages m ON m.rid = r.id
         ORDER BY COALESCE(u.tokens, 0) DESC",
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

    let has_legacy_fts: bool = conn.query_row(
        "SELECT EXISTS(
           SELECT 1 FROM sqlite_schema
           WHERE type = 'table' AND name = 'message_fts'
         )",
        [],
        |row| row.get(0),
    )?;
    if !has_legacy_fts {
        return search_canonical_compat(conn, query, limit, filter);
    }

    let mut sql = String::from(
        "SELECT m.timeline_id, m.session_id, m.type, m.ts,
                snippet(message_fts, -1, '[', ']', '…', 12), rank
         FROM message_fts
         JOIN message m ON m.id = message_fts.rowid
         JOIN session s ON s.id = m.session_id",
    );
    if filter.agent.is_some() {
        sql.push_str(" JOIN agent_runtime r ON r.id = s.runtime_id");
    }
    sql.push_str(" WHERE message_fts MATCH ?1");
    // The legacy in-projection index holds every row; the coverage policy
    // still applies to what it may answer with.
    sql.push_str(&format!(" AND {}", search_kind_predicate("m.type")));
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
            kind: row.get(2)?,
            at: row.get(3)?,
            snippet: row.get(4)?,
            score: row.get(5)?,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Compatibility scan for L2 tests and old diagnostic callers after search
/// moved out of the canonical database. Production L3 never uses this path:
/// it goes through `SearchBackend`, whose non-ready state is explicit. Frozen
/// schema-6 benchmark projections still take the exact FTS path above.
fn search_canonical_compat(
    conn: &Connection,
    query: &str,
    limit: i64,
    filter: &SearchFilter,
) -> Result<Vec<SearchHit>, StoreError> {
    fn clean_term(term: &str) -> String {
        let term = term.trim().trim_matches('"');
        term.replace("\"\"", "\"").to_lowercase()
    }

    let (separator, disjunction) = if query.contains(" OR ") {
        (" OR ", true)
    } else if query.contains(" AND ") {
        (" AND ", false)
    } else {
        (" ", false)
    };
    let terms: Vec<String> = query
        .split(separator)
        .map(clean_term)
        .filter(|term| !term.is_empty())
        .collect();
    if terms.is_empty() {
        return Ok(Vec::new());
    }

    let mut hits = Vec::new();
    visit_search_documents(conn, |document| {
        if filter
            .session
            .as_ref()
            .is_some_and(|value| value != &document.session_id)
            || filter
                .runtime
                .as_ref()
                .is_some_and(|value| value != &document.runtime_id)
            || filter
                .agent
                .as_ref()
                .is_some_and(|value| value != &document.agent)
            || filter
                .project
                .as_ref()
                .is_some_and(|value| document.project_id.as_ref() != Some(value))
            || filter
                .kind
                .as_ref()
                .is_some_and(|value| value != &document.kind)
            || filter
                .is_error
                .is_some_and(|value| document.is_error != Some(value))
            || filter
                .after_ts
                .is_some_and(|value| document.ts.is_none_or(|ts| ts < value))
            || filter
                .before_ts
                .is_some_and(|value| document.ts.is_none_or(|ts| ts > value))
        {
            return Ok(());
        }
        let lanes = [
            document.prose.as_str(),
            document.target.as_str(),
            document.intent.as_str(),
            document.change.as_str(),
            document.tool.as_str(),
        ];
        let haystack = lanes.join("\n").to_lowercase();
        let matches = if disjunction {
            terms.iter().any(|term| haystack.contains(term))
        } else {
            terms.iter().all(|term| haystack.contains(term))
        };
        if matches {
            let snippet = lanes
                .into_iter()
                .find(|lane| !lane.is_empty())
                .unwrap_or_default()
                .chars()
                .take(512)
                .collect();
            hits.push(SearchHit {
                timeline_id: document.timeline_id,
                session_id: document.session_id,
                kind: document.kind,
                at: document.ts,
                snippet,
                score: 0.0,
            });
        }
        Ok(())
    })?;
    hits.sort_by(|left, right| left.timeline_id.cmp(&right.timeline_id));
    hits.truncate(usize::try_from(limit.max(0)).unwrap_or(usize::MAX));
    Ok(hits)
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
