//! The `AgentAdapter` implementation and stream admission boundary.

use std::any::Any;

use rosetta_adapter_support::GroundTruthDoc;
use rosetta_adapter_support::adapter::{
    AgentAdapter, FileStat, LineOutcome, NormalizeCtx, SkimResult, SnapshotPlan, SnapshotQuery,
    StreamDriver,
};
use rosetta_core::rows::{SessionRole, TitleKind};
use serde_json::Value;

use crate::normalize::{self, AntigravityState};
use crate::raw::conversation::{GEN_METADATA_QUERY, STEPS_QUERY};
use crate::spec;
use crate::time::rfc3339_ms;
use crate::usage;

pub struct AntigravityAdapter;

impl AgentAdapter for AntigravityAdapter {
    fn kind(&self) -> &'static str {
        "antigravity"
    }

    fn layout(&self) -> &GroundTruthDoc {
        spec::doc()
    }

    fn stream_driver(&self, stream_id: &str) -> Option<StreamDriver> {
        match stream_id {
            spec::TRANSCRIPT => Some(StreamDriver::JsonLines),
            spec::CONVERSATION_SUMMARIES | spec::CONVERSATION_DB => {
                Some(StreamDriver::SqliteSnapshot)
            }
            _ => None,
        }
    }

    fn census_session_streams(&self) -> Option<&'static [&'static str]> {
        Some(&[spec::TRANSCRIPT])
    }

    fn normalizer_version(&self) -> u32 {
        // 8: the conversation store's per-step usage becomes session usage
        // samples (§4.1.3); the checkpoint gains the generation index.
        8
    }

    fn snapshot_plan(&self, stream_id: &str) -> Option<SnapshotPlan> {
        match stream_id {
            spec::CONVERSATION_SUMMARIES => Some(SnapshotPlan {
                sessions_sql: SUMMARIES_SQL.to_owned(),
                // This database is metadata-only. The transcript remains the one
                // authoritative timeline source.
                record_queries: Vec::new(),
            }),
            spec::CONVERSATION_DB => Some(conversation_plan()),
            _ => None,
        }
    }

    fn init_state(&self) -> Option<Box<dyn Any + Send>> {
        Some(Box::new(AntigravityState::default()))
    }

    fn checkpoint_state(&self, state: Option<&(dyn Any + Send)>) -> Result<Option<Value>, String> {
        let state = state
            .and_then(|state| state.downcast_ref::<AntigravityState>())
            .ok_or_else(|| "antigravity checkpoint state has the wrong type".to_owned())?;
        serde_json::to_value(state)
            .map(Some)
            .map_err(|error| error.to_string())
    }

    fn restore_state(
        &self,
        checkpoint: Option<&Value>,
    ) -> Result<Option<Box<dyn Any + Send>>, String> {
        match checkpoint {
            None => Ok(self.init_state()),
            Some(value) => serde_json::from_value::<AntigravityState>(value.clone())
                .map(|state| Some(Box::new(state) as Box<dyn Any + Send>))
                .map_err(|error| error.to_string()),
        }
    }

    /// The conversation UUID is the native `brain/<conversation-id>/` path
    /// segment. No transcript record repeats it, and the stream carries no cwd,
    /// parent, or agent version. Complete checkpoint records in the bounded
    /// windows may supply a generated objective title; the oldest complete head
    /// record supplies `started_at` and a cut record is ignored.
    fn skim(&self, stream_id: &str, stat: &FileStat, head: &[u8], tail: &[u8]) -> SkimResult {
        if stream_id != spec::TRANSCRIPT {
            return SkimResult::default();
        }
        let whole_head = head.len() as u64 >= stat.size;
        let whole_tail = tail.len() as u64 >= stat.size;
        let title = complete_head_lines(head, whole_head)
            .chain(complete_tail_lines(tail, whole_tail))
            .filter_map(checkpoint_title)
            .max_by_key(|(step, _)| *step)
            .map(|(_, title)| title);
        SkimResult {
            ledger_only: false,
            ops: Vec::new(),
            session_id: conversation_id(&stat.relative_path).map(str::to_owned),
            parent_session_id: None,
            delegation: None,
            root_session_id: None,
            role: Some(SessionRole::Main),
            cwd: None,
            project: None,
            agent_version: None,
            title_kind: title.as_ref().map(|_| TitleKind::Fallback),
            title,
            summary: None,
            started_at: opening_stamp(head, whole_head),
            updated_at: None,
        }
    }

    fn wants_skim_fallback(&self, stream_id: &str, skim: &SkimResult) -> bool {
        stream_id == spec::TRANSCRIPT && skim.title.is_none()
    }

    fn absorb_skim_fallback_record(
        &self,
        stream_id: &str,
        record: &[u8],
        skim: &mut SkimResult,
    ) -> bool {
        if stream_id == spec::TRANSCRIPT
            && let Some((_, title)) = checkpoint_title(record)
        {
            // The scan is ordered. Keep walking so the latest valid objective
            // within the declared budget wins.
            skim.title = Some(title);
            skim.title_kind = Some(TitleKind::Fallback);
        }
        false
    }

    fn ingest_line(&self, ctx: &mut NormalizeCtx, stream_id: &str, line: &[u8]) -> LineOutcome {
        // Admission is repeated here defensively. Even a caller bypassing
        // `stream_driver` cannot normalize a byte-identical inventory sibling.
        let mut state = take_state(ctx);
        let outcome = match stream_id {
            spec::TRANSCRIPT => normalize::ingest(ctx, &mut state, stream_id, line),
            spec::CONVERSATION_DB => usage::ingest(ctx, &mut state, stream_id, line),
            _ => LineOutcome::parsed(Vec::new()),
        };
        ctx.state = Some(state);
        outcome
    }
}

/// The conversation store is one database per cascade, and `cascade_id` is
/// the brain UUID the transcript path carries, so the store enriches that
/// session instead of minting one. Generations are read before steps so the
/// normalizer can attribute every step's usage to the generation that owns
/// it. The payload columns are never selected: the transcript owns the
/// timeline, and this stream owns usage only. A store that ever listed
/// several cascades would attribute its steps to the lowest id alone rather
/// than count the same requests once per cascade.
fn conversation_plan() -> SnapshotPlan {
    SnapshotPlan {
        sessions_sql: r#"
            SELECT DISTINCT cascade_id AS native_session_id
            FROM trajectory_meta
            WHERE cascade_id IS NOT NULL AND cascade_id <> ''
            ORDER BY cascade_id
        "#
        .to_owned(),
        record_queries: vec![
            SnapshotQuery {
                name: GEN_METADATA_QUERY.to_owned(),
                sql: r#"
                    SELECT idx, data
                    FROM gen_metadata
                    WHERE ?1 = (SELECT MIN(cascade_id) FROM trajectory_meta
                                WHERE cascade_id IS NOT NULL AND cascade_id <> '')
                    ORDER BY idx
                "#
                .to_owned(),
            },
            SnapshotQuery {
                name: STEPS_QUERY.to_owned(),
                sql: r#"
                    SELECT idx, step_type, status, metadata
                    FROM steps
                    WHERE ?1 = (SELECT MIN(cascade_id) FROM trajectory_meta
                                WHERE cascade_id IS NOT NULL AND cascade_id <> '')
                    ORDER BY idx
                "#
                .to_owned(),
            },
        ],
    }
}

/// Enumerate the summary index and resolve native parent chains. Empty strings
/// are Antigravity's persisted null representation. `workspace_uris` has been
/// written both as a URI and as a JSON array across releases; accept either
/// without inventing a cwd when the field is empty.
const SUMMARIES_SQL: &str = r#"
WITH RECURSIVE lineage(native_session_id, ancestor_id, parent_id, depth) AS (
    SELECT
        conversation_id,
        conversation_id,
        NULLIF(parent_conversation_id, ''),
        0
    FROM conversation_summaries
    UNION ALL
    SELECT
        lineage.native_session_id,
        parent.conversation_id,
        NULLIF(parent.parent_conversation_id, ''),
        lineage.depth + 1
    FROM lineage
    JOIN conversation_summaries AS parent
      ON parent.conversation_id = lineage.parent_id
    WHERE lineage.depth < 100
)
SELECT
    summary.conversation_id AS native_session_id,
    NULLIF(summary.title, '') AS title,
    NULLIF(summary.preview, '') AS summary,
    CASE
        WHEN json_valid(summary.workspace_uris)
          THEN NULLIF(REPLACE(json_extract(summary.workspace_uris, '$[0]'), 'file://', ''), '')
        WHEN summary.workspace_uris LIKE 'file://%'
          THEN NULLIF(SUBSTR(summary.workspace_uris, 8), '')
        ELSE NULLIF(summary.workspace_uris, '')
    END AS cwd,
    CAST((julianday(summary.last_modified_time) - 2440587.5) * 86400000 AS INTEGER)
      AS started_at,
    CAST((julianday(summary.last_modified_time) - 2440587.5) * 86400000 AS INTEGER)
      AS updated_at,
    NULLIF(summary.parent_conversation_id, '') AS parent_native_session_id,
    CASE
        WHEN NULLIF(summary.parent_conversation_id, '') IS NULL
          THEN summary.conversation_id
        ELSE COALESCE(
            (SELECT ancestor_id
             FROM lineage
             WHERE native_session_id = summary.conversation_id AND parent_id IS NULL
             ORDER BY depth DESC
             LIMIT 1),
            summary.parent_conversation_id
        )
    END AS root_native_session_id,
    CASE
        WHEN NULLIF(summary.parent_conversation_id, '') IS NULL THEN 'main'
        ELSE 'subagent'
    END AS role
FROM conversation_summaries AS summary
ORDER BY summary.conversation_id
"#;

fn take_state(ctx: &mut NormalizeCtx) -> Box<AntigravityState> {
    match ctx.state.take() {
        Some(state) => state.downcast::<AntigravityState>().unwrap_or_default(),
        None => Box::default(),
    }
}

/// `brain/<conversation-id>/.system_generated/logs/transcript.jsonl`.
fn conversation_id(relative_path: &str) -> Option<&str> {
    let mut parts = relative_path.split('/');
    (parts.next()? == "brain").then_some(())?;
    let id = parts.next()?;
    (!id.is_empty()).then_some(())?;
    (parts.next()? == ".system_generated").then_some(())?;
    (parts.next()? == "logs").then_some(())?;
    (parts.next()? == "transcript.jsonl").then_some(())?;
    parts.next().is_none().then_some(id)
}

/// First complete head record. The head begins at byte zero but may end halfway
/// through its last line; a whole file need not end in a newline. A malformed
/// opening record answers nothing rather than borrowing a later timestamp as
/// the session start.
fn opening_stamp(head: &[u8], whole_file: bool) -> Option<i64> {
    let line = complete_head_lines(head, whole_file).next()?;
    let record = serde_json::from_slice::<Value>(line).ok()?;
    record["created_at"].as_str().and_then(rfc3339_ms)
}

fn checkpoint_title(line: &[u8]) -> Option<(u64, String)> {
    let text = std::str::from_utf8(line).ok()?;
    let crate::raw::ParsedLine::Known(record) = crate::raw::parse_line(text).ok()? else {
        return None;
    };
    let crate::raw::transcript::KnownRecord::Checkpoint(checkpoint) = record.as_ref() else {
        return None;
    };
    crate::raw::transcript::checkpoint_objective(&checkpoint.content)
        .map(|title| (checkpoint.step_index, title))
}

/// Head starts at byte zero; omit only its possibly cut final fragment.
fn complete_head_lines(head: &[u8], whole_file: bool) -> impl Iterator<Item = &[u8]> {
    let complete_len = if whole_file {
        head.len()
    } else {
        head.iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(0, |at| at + 1)
    };
    head[..complete_len]
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
}

/// Tail ends at EOF; omit its first fragment unless it is the whole file.
fn complete_tail_lines(tail: &[u8], whole_file: bool) -> impl Iterator<Item = &[u8]> {
    tail.split(|byte| *byte == b'\n')
        .skip(usize::from(!whole_file))
        .filter(|line| !line.is_empty())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{AntigravityAdapter, conversation_id};
    use crate::spec;
    use rosetta_adapter_support::adapter::{AgentAdapter, FileStat, StreamDriver};
    use rosetta_core::rows::SessionRole;

    const PATH: &str =
        "brain/11111111-2222-3333-4444-555555555555/.system_generated/logs/transcript.jsonl";
    const LINE: &str = r#"{"step_index":0,"source":"USER_EXPLICIT","type":"USER_INPUT","status":"DONE","created_at":"2026-08-25T05:10:43Z","content":"hello"}"#;

    fn stat(size: usize) -> FileStat {
        FileStat {
            relative_path: PATH.to_owned(),
            size: size as u64,
            modified_ms: 0,
        }
    }

    #[test]
    fn exactly_one_overlapping_transcript_stream_is_driven() {
        let adapter = AntigravityAdapter;
        assert_eq!(
            adapter.stream_driver(spec::TRANSCRIPT),
            Some(StreamDriver::JsonLines)
        );
        assert_eq!(
            adapter.stream_driver(spec::CONVERSATION_SUMMARIES),
            Some(StreamDriver::SqliteSnapshot)
        );
        assert_eq!(
            adapter.stream_driver(spec::CONVERSATION_DB),
            Some(StreamDriver::SqliteSnapshot)
        );
        for inventory in [
            spec::TRANSCRIPT_FULL,
            spec::TRANSCRIPT_CHUNKS,
            spec::TRANSCRIPT_FULL_CHUNKS,
        ] {
            assert_eq!(adapter.stream_driver(inventory), None, "drove {inventory}");
        }
    }

    #[test]
    fn summaries_plan_preserves_metadata_and_parentage() {
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE conversation_summaries (
                    conversation_id TEXT PRIMARY KEY,
                    title TEXT NOT NULL,
                    preview TEXT NOT NULL,
                    last_modified_time TEXT NOT NULL,
                    workspace_uris TEXT NOT NULL,
                    parent_conversation_id TEXT NOT NULL
                 );
                 INSERT INTO conversation_summaries VALUES
                    ('root', 'Root title', 'Root preview',
                     '2026-08-25 05:10:45.422+00:00',
                     '[\"file:///workspace/root\"]', '');
                 INSERT INTO conversation_summaries VALUES
                    ('child', '', 'Child preview',
                     '2026-08-25 05:10:46.422+00:00',
                     'file:///workspace/root', 'root');",
            )
            .unwrap();
        let plan = AntigravityAdapter
            .snapshot_plan(spec::CONVERSATION_SUMMARIES)
            .expect("summary plan");
        assert!(plan.record_queries.is_empty());
        let mut statement = connection.prepare(&plan.sessions_sql).unwrap();
        type SummaryFacts = (Option<String>, Option<String>, String, String, String);
        let rows: BTreeMap<String, SummaryFacts> = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>("native_session_id")?,
                    (
                        row.get::<_, Option<String>>("title")?,
                        row.get::<_, Option<String>>("parent_native_session_id")?,
                        row.get::<_, String>("root_native_session_id")?,
                        row.get::<_, String>("role")?,
                        row.get::<_, String>("cwd")?,
                    ),
                ))
            })
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            rows["root"],
            (
                Some("Root title".to_owned()),
                None,
                "root".to_owned(),
                "main".to_owned(),
                "/workspace/root".to_owned(),
            )
        );
        assert_eq!(rows["child"].1.as_deref(), Some("root"));
        assert_eq!(rows["child"].2, "root");
        assert_eq!(rows["child"].3, "subagent");
    }

    /// The 1.1.20 store schema (sources/1.1.20.sqlite-schema.sql), reduced to
    /// the three tables the plan touches.
    fn conversation_store() -> rusqlite::Connection {
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE trajectory_meta (trajectory_id TEXT PRIMARY KEY, cascade_id TEXT,
                    trajectory_type INTEGER, source INTEGER);
                 CREATE TABLE steps (idx INTEGER PRIMARY KEY, step_type INTEGER NOT NULL DEFAULT 0,
                    status INTEGER NOT NULL DEFAULT 0, has_subtrajectory NUMERIC NOT NULL DEFAULT 0,
                    metadata BLOB, error_details BLOB, permissions BLOB, task_details BLOB,
                    render_info BLOB, step_payload BLOB, step_format INTEGER NOT NULL DEFAULT 0);
                 CREATE TABLE gen_metadata (idx INTEGER PRIMARY KEY, data BLOB,
                    size INTEGER NOT NULL DEFAULT 0);
                 INSERT INTO trajectory_meta VALUES ('traj-1', 'cascade-a', 4, 17);
                 INSERT INTO trajectory_meta VALUES ('traj-2', 'cascade-a', 4, 17);
                 INSERT INTO trajectory_meta VALUES ('traj-3', '', 4, 17);
                 INSERT INTO steps (idx, step_type, status, metadata, step_payload)
                    VALUES (0, 14, 3, x'0800', x'DEADBEEF');
                 INSERT INTO steps (idx, step_type, status, metadata, step_payload)
                    VALUES (1, 15, 3, x'0801', x'DEADBEEF');
                 INSERT INTO gen_metadata VALUES (0, x'1001', 2);",
            )
            .unwrap();
        connection
    }

    fn column_names(connection: &rusqlite::Connection, sql: &str) -> Vec<String> {
        connection
            .prepare(sql)
            .unwrap()
            .column_names()
            .iter()
            .map(|name| (*name).to_owned())
            .collect()
    }

    fn count(connection: &rusqlite::Connection, sql: &str, bind: &str) -> usize {
        let mut statement = connection.prepare(sql).unwrap();
        statement
            .query_map(rusqlite::params![bind], |_| Ok(()))
            .unwrap()
            .count()
    }

    #[test]
    fn conversation_plan_names_one_cascade_and_reads_only_usage_columns() {
        let connection = conversation_store();
        let plan = AntigravityAdapter
            .snapshot_plan(spec::CONVERSATION_DB)
            .expect("conversation plan");

        let mut statement = connection.prepare(&plan.sessions_sql).unwrap();
        let sessions: Vec<String> = statement
            .query_map([], |row| row.get::<_, String>("native_session_id"))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            sessions,
            ["cascade-a"],
            "one session per cascade, blanks dropped"
        );

        let names: Vec<&str> = plan
            .record_queries
            .iter()
            .map(|query| query.name.as_str())
            .collect();
        assert_eq!(
            names,
            ["gen_metadata", "steps"],
            "generations precede steps"
        );
        assert_eq!(
            column_names(&connection, &plan.record_queries[0].sql),
            ["idx", "data"]
        );
        assert_eq!(
            column_names(&connection, &plan.record_queries[1].sql),
            ["idx", "step_type", "status", "metadata"],
            "payload columns are never selected"
        );
        for query in &plan.record_queries {
            assert!(
                count(&connection, &query.sql, "cascade-a") > 0,
                "{} rows belong to the cascade",
                query.name
            );
        }

        // A store that ever listed two cascades attributes its rows once.
        connection
            .execute(
                "INSERT INTO trajectory_meta VALUES ('traj-4', 'cascade-b', 4, 17)",
                [],
            )
            .unwrap();
        let mut statement = connection.prepare(&plan.sessions_sql).unwrap();
        let sessions: Vec<String> = statement
            .query_map([], |row| row.get::<_, String>("native_session_id"))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(sessions, ["cascade-a", "cascade-b"]);
        for query in &plan.record_queries {
            assert!(count(&connection, &query.sql, "cascade-a") > 0);
            assert_eq!(count(&connection, &query.sql, "cascade-b"), 0);
        }
    }

    #[test]
    fn skim_extracts_native_conversation_identity_from_the_path() {
        let result =
            AntigravityAdapter.skim(spec::TRANSCRIPT, &stat(LINE.len()), LINE.as_bytes(), b"");
        assert_eq!(
            result.session_id.as_deref(),
            Some("11111111-2222-3333-4444-555555555555")
        );
        assert_eq!(result.role, Some(SessionRole::Main));
        assert_eq!(result.started_at, Some(1_787_634_643_000));
        assert!(result.ops.is_empty());
    }

    #[test]
    fn skim_tolerates_a_cut_head() {
        let result = AntigravityAdapter.skim(
            spec::TRANSCRIPT,
            &stat(LINE.len()),
            &LINE.as_bytes()[..32],
            b"ignored tail",
        );
        assert!(
            result.session_id.is_some(),
            "path identity survives a cut line"
        );
        assert_eq!(result.started_at, None);
    }

    #[test]
    fn skim_accepts_a_whole_file_without_a_trailing_newline() {
        let result =
            AntigravityAdapter.skim(spec::TRANSCRIPT, &stat(LINE.len()), LINE.as_bytes(), b"");
        assert_eq!(result.started_at, Some(1_787_634_643_000));
    }

    #[test]
    fn skim_minimal_empty_windows_still_report_path_identity() {
        let result = AntigravityAdapter.skim(spec::TRANSCRIPT, &stat(10), b"", b"");
        assert_eq!(
            result.session_id.as_deref(),
            Some("11111111-2222-3333-4444-555555555555")
        );
        assert_eq!(result.started_at, None);
    }

    #[test]
    fn skim_ignores_non_driven_streams() {
        let result = AntigravityAdapter.skim(
            spec::TRANSCRIPT_FULL,
            &stat(LINE.len()),
            LINE.as_bytes(),
            b"",
        );
        assert!(result.session_id.is_none());
        assert!(result.role.is_none());
        assert!(result.started_at.is_none());
    }

    #[test]
    fn path_parser_requires_the_native_layout() {
        assert_eq!(
            conversation_id(PATH),
            Some("11111111-2222-3333-4444-555555555555")
        );
        assert_eq!(conversation_id("logs/transcript.jsonl"), None);
        assert_eq!(
            conversation_id("brain/id/.system_generated/logs/transcript_full.jsonl"),
            None
        );
    }
}
