//! The `AgentAdapter` implementation and SQLite snapshot admission plan.

use std::any::Any;

use rosetta_adapter_support::{
    AgentAdapter, FileStat, GroundTruthDoc, LineOutcome, NormalizeCtx, SkimResult, SnapshotPlan,
    SnapshotQuery, StreamDriver,
};
use serde_json::Value;

use crate::normalize::{self, OpenCodeState};
use crate::spec;

pub struct OpenCodeAdapter;

impl AgentAdapter for OpenCodeAdapter {
    fn kind(&self) -> &'static str {
        "opencode"
    }

    fn layout(&self) -> &GroundTruthDoc {
        spec::doc()
    }

    fn stream_driver(&self, stream_id: &str) -> Option<StreamDriver> {
        (stream_id == spec::DATABASE).then_some(StreamDriver::SqliteSnapshot)
    }

    fn normalizer_version(&self) -> u32 {
        6
    }

    fn snapshot_plan(&self, stream_id: &str) -> Option<SnapshotPlan> {
        (stream_id == spec::DATABASE).then(|| SnapshotPlan {
            sessions_sql: SESSIONS_SQL.to_owned(),
            record_queries: vec![SnapshotQuery {
                name: spec::TIMELINE_QUERY.to_owned(),
                sql: TIMELINE_SQL.to_owned(),
            }],
        })
    }

    fn init_state(&self) -> Option<Box<dyn Any + Send>> {
        Some(Box::new(OpenCodeState::default()))
    }

    fn checkpoint_state(&self, state: Option<&(dyn Any + Send)>) -> Result<Option<Value>, String> {
        let state = state
            .and_then(|state| state.downcast_ref::<OpenCodeState>())
            .ok_or_else(|| "opencode checkpoint state has the wrong type".to_owned())?;
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
            Some(value) => serde_json::from_value::<OpenCodeState>(value.clone())
                .map(|state| Some(Box::new(state) as Box<dyn Any + Send>))
                .map_err(|error| error.to_string()),
        }
    }

    // SQLite metadata comes from `SnapshotPlan::sessions_sql`; bounded byte
    // windows have no grammar and are deliberately ignored.
    fn skim(&self, _stream_id: &str, _stat: &FileStat, _head: &[u8], _tail: &[u8]) -> SkimResult {
        SkimResult::default()
    }

    fn ingest_line(&self, ctx: &mut NormalizeCtx, stream_id: &str, line: &[u8]) -> LineOutcome {
        if stream_id != spec::DATABASE {
            return LineOutcome::parsed(Vec::new());
        }
        let mut state = take_state(ctx);
        let outcome = normalize::ingest(ctx, &mut state, stream_id, line);
        ctx.state = Some(state);
        outcome
    }
}

fn take_state(ctx: &mut NormalizeCtx) -> Box<OpenCodeState> {
    match ctx.state.take() {
        Some(state) => state.downcast::<OpenCodeState>().unwrap_or_default(),
        None => Box::default(),
    }
}

/// Enumerate native sessions and resolve the root of each parent chain. A
/// missing ancestor keeps the named parent as root; the pipeline materializes
/// a placeholder for that retained relationship.
const SESSIONS_SQL: &str = r#"
WITH RECURSIVE lineage(native_session_id, ancestor_id, parent_id, depth) AS (
    SELECT id, id, parent_id, 0 FROM session
    UNION ALL
    SELECT lineage.native_session_id, ancestor.id, ancestor.parent_id, lineage.depth + 1
    FROM lineage
    JOIN session AS ancestor ON ancestor.id = lineage.parent_id
    WHERE lineage.depth < 100
)
SELECT
    session.id AS native_session_id,
    session.title AS title,
    session.directory AS cwd,
    session.version AS agent_version,
    session.time_created AS started_at,
    session.time_updated AS updated_at,
    session.parent_id AS parent_native_session_id,
    CASE
        WHEN session.parent_id IS NULL THEN session.id
        ELSE COALESCE(
            (SELECT ancestor_id
             FROM lineage
             WHERE native_session_id = session.id AND parent_id IS NULL
             ORDER BY depth DESC
             LIMIT 1),
            session.parent_id
        )
    END AS root_native_session_id,
    CASE WHEN session.parent_id IS NULL THEN 'main' ELSE 'subagent' END AS role
FROM session
ORDER BY session.id
"#;

/// Final message/part state is conversational truth. The append-only event
/// table contributes only first-seen presentation order: repeated updates to a
/// text or tool part never move the canonical row.
const TIMELINE_SQL: &str = r#"
WITH first_part_event AS (
    SELECT json_extract(data, '$.part.id') AS part_id, MIN(seq) AS first_event_seq
    FROM event
    WHERE aggregate_id = ?1 AND type = 'message.part.updated.1'
    GROUP BY json_extract(data, '$.part.id')
)
SELECT
    message.id AS message_id,
    message.data AS message_data,
    part.id AS part_id,
    part.time_created AS part_time_created,
    part.data AS part_data,
    first_part_event.first_event_seq AS first_event_seq
FROM message
JOIN part ON part.message_id = message.id AND part.session_id = message.session_id
LEFT JOIN first_part_event ON first_part_event.part_id = part.id
WHERE message.session_id = ?1
ORDER BY
    COALESCE(first_part_event.first_event_seq, 9223372036854775807),
    message.time_created,
    message.id,
    part.time_created,
    part.id
"#;

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::OpenCodeAdapter;
    use crate::spec;
    use rosetta_adapter_support::{AgentAdapter, FileStat, StreamDriver};

    #[test]
    fn only_the_database_stream_is_driven() {
        assert_eq!(
            OpenCodeAdapter.stream_driver(spec::DATABASE),
            Some(StreamDriver::SqliteSnapshot)
        );
        for inventory in ["log", "snapshot", "farm:headless-stdout"] {
            assert_eq!(OpenCodeAdapter.stream_driver(inventory), None);
        }
    }

    #[test]
    fn snapshot_plan_has_one_ordered_timeline_query() {
        let plan = OpenCodeAdapter
            .snapshot_plan(spec::DATABASE)
            .expect("snapshot plan");
        assert!(plan.sessions_sql.contains("native_session_id"));
        assert!(plan.sessions_sql.contains("root_native_session_id"));
        assert_eq!(plan.record_queries.len(), 1);
        assert_eq!(plan.record_queries[0].name, spec::TIMELINE_QUERY);
        assert!(plan.record_queries[0].sql.contains("first_event_seq"));
    }

    #[test]
    fn byte_skim_never_treats_sqlite_as_text() {
        let result = OpenCodeAdapter.skim(
            spec::DATABASE,
            &FileStat {
                relative_path: "opencode.db".to_owned(),
                size: 4096,
                modified_ms: 0,
            },
            b"SQLite format 3",
            b"not jsonl",
        );
        assert!(result.session_id.is_none());
        assert!(result.ops.is_empty());
    }

    #[test]
    fn sessions_query_resolves_parent_and_root_identity() {
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE session (
                    id TEXT PRIMARY KEY,
                    title TEXT NOT NULL,
                    directory TEXT NOT NULL,
                    version TEXT NOT NULL,
                    time_created INTEGER NOT NULL,
                    time_updated INTEGER NOT NULL,
                    parent_id TEXT
                 );
                 INSERT INTO session VALUES ('root', 'Root', '/w', '1', 1, 4, NULL);
                 INSERT INTO session VALUES ('child', 'Child', '/w', '1', 2, 4, 'root');
                 INSERT INTO session VALUES ('grand', 'Grand', '/w', '1', 3, 4, 'child');",
            )
            .unwrap();
        let plan = OpenCodeAdapter.snapshot_plan(spec::DATABASE).unwrap();
        let mut statement = connection.prepare(&plan.sessions_sql).unwrap();
        let rows: BTreeMap<String, (Option<String>, String, String)> = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>("native_session_id")?,
                    (
                        row.get::<_, Option<String>>("parent_native_session_id")?,
                        row.get::<_, String>("root_native_session_id")?,
                        row.get::<_, String>("role")?,
                    ),
                ))
            })
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(rows["root"], (None, "root".to_owned(), "main".to_owned()));
        assert_eq!(
            rows["child"],
            (
                Some("root".to_owned()),
                "root".to_owned(),
                "subagent".to_owned()
            )
        );
        assert_eq!(
            rows["grand"],
            (
                Some("child".to_owned()),
                "root".to_owned(),
                "subagent".to_owned()
            )
        );
    }
}
