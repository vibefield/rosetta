//! The `AgentAdapter` implementation. Hermes contributes SQL declarations;
//! capture owns SQLite snapshotting and row serialization. The default
//! store and every named profile's store share one plan.

use std::any::Any;

use rosetta_adapter_support::GroundTruthDoc;
use rosetta_adapter_support::adapter::{
    AgentAdapter, FileStat, LineOutcome, NormalizeCtx, SkimResult, SnapshotPlan, SnapshotQuery,
    StreamDriver,
};

use crate::state::HermesState;
use crate::{normalize, spec};

pub struct HermesAdapter;

impl AgentAdapter for HermesAdapter {
    fn kind(&self) -> &'static str {
        "hermes"
    }

    fn layout(&self) -> &GroundTruthDoc {
        spec::doc()
    }

    fn stream_driver(&self, stream_id: &str) -> Option<StreamDriver> {
        spec::is_state_stream(stream_id).then_some(StreamDriver::SqliteSnapshot)
    }

    fn normalizer_version(&self) -> u32 {
        // 7: exclusive usage buckets, route, single-call scope (§4.1.3).
        // 8: per-model rows carry their API call count.
        // 9: Codex app-server rows withheld, thinking unreported on Anthropic
        //    and Gemini-native wires, billed only when a single call proves
        //    it, registered 0.21.x tool names, named-profile stores.
        9
    }

    fn snapshot_plan(&self, stream_id: &str) -> Option<SnapshotPlan> {
        spec::is_state_stream(stream_id).then(snapshot_plan)
    }

    fn init_state(&self) -> Option<Box<dyn Any + Send>> {
        Some(Box::new(HermesState::default()))
    }

    fn checkpoint_state(
        &self,
        state: Option<&(dyn Any + Send)>,
    ) -> Result<Option<serde_json::Value>, String> {
        let state = state
            .and_then(|state| state.downcast_ref::<HermesState>())
            .ok_or_else(|| "hermes checkpoint state has the wrong type".to_owned())?;
        serde_json::to_value(state)
            .map(Some)
            .map_err(|error| error.to_string())
    }

    fn restore_state(
        &self,
        checkpoint: Option<&serde_json::Value>,
    ) -> Result<Option<Box<dyn Any + Send>>, String> {
        match checkpoint {
            None => Ok(self.init_state()),
            Some(value) => serde_json::from_value::<HermesState>(value.clone())
                .map(|state| Some(Box::new(state) as Box<dyn Any + Send>))
                .map_err(|error| error.to_string()),
        }
    }

    /// SQLite-session metadata comes from `SnapshotPlan::sessions_sql`; the
    /// byte-window skim path is never used for this driver.
    fn skim(&self, _stream_id: &str, _stat: &FileStat, _head: &[u8], _tail: &[u8]) -> SkimResult {
        SkimResult::default()
    }

    fn ingest_line(&self, ctx: &mut NormalizeCtx, stream_id: &str, line: &[u8]) -> LineOutcome {
        if self.stream_driver(stream_id).is_none() {
            return LineOutcome::parsed(Vec::new());
        }
        normalize::ingest_line(ctx, stream_id, line)
    }
}

fn snapshot_plan() -> SnapshotPlan {
    SnapshotPlan {
        // Root identity is derived from the native parent chain inside the
        // stable snapshot. Orphans retain their immediate parent and let the
        // pipeline's placeholder/root fallback preserve that relationship.
        sessions_sql: r#"
            WITH RECURSIVE roots(session_id, root_id) AS (
                SELECT id, id
                FROM sessions
                WHERE parent_session_id IS NULL
                UNION ALL
                SELECT child.id, roots.root_id
                FROM sessions AS child
                JOIN roots ON child.parent_session_id = roots.session_id
            )
            SELECT
                sessions.id AS native_session_id,
                sessions.title AS title,
                sessions.cwd AS cwd,
                CAST(sessions.started_at * 1000 AS INTEGER) AS started_at,
                CAST(MAX(
                    sessions.started_at,
                    COALESCE(sessions.last_activity_at, sessions.started_at),
                    COALESCE(sessions.ended_at, sessions.started_at)
                ) * 1000 AS INTEGER) AS updated_at,
                sessions.parent_session_id AS parent_native_session_id,
                roots.root_id AS root_native_session_id,
                CASE
                    WHEN sessions.parent_session_id IS NULL THEN 'main'
                    ELSE 'subagent'
                END AS role
            FROM sessions
            LEFT JOIN roots ON roots.session_id = sessions.id
            ORDER BY sessions.started_at, sessions.id
        "#
        .to_owned(),
        record_queries: vec![
            SnapshotQuery {
                name: "schema_version".to_owned(),
                sql: "SELECT * FROM schema_version WHERE ?1 IS NOT NULL ORDER BY rowid".to_owned(),
            },
            SnapshotQuery {
                name: "schema_inventory".to_owned(),
                sql: "SELECT name AS table_name FROM sqlite_master \
                      WHERE type = 'table' AND name NOT LIKE 'sqlite_%' \
                        AND ?1 IS NOT NULL ORDER BY name"
                    .to_owned(),
            },
            SnapshotQuery {
                name: "state_meta".to_owned(),
                sql: "SELECT * FROM state_meta WHERE ?1 IS NOT NULL ORDER BY key".to_owned(),
            },
            SnapshotQuery {
                name: "system_prompts".to_owned(),
                sql: r#"
                    SELECT
                        COALESCE(sessions.system_prompt_hash, 'session:' || sessions.id) AS hash,
                        COALESCE(system_prompts.prompt, sessions.system_prompt) AS prompt
                    FROM sessions
                    LEFT JOIN system_prompts
                      ON system_prompts.hash = sessions.system_prompt_hash
                    WHERE sessions.id = ?1
                      AND COALESCE(system_prompts.prompt, sessions.system_prompt) IS NOT NULL
                    ORDER BY hash
                "#
                .to_owned(),
            },
            SnapshotQuery {
                name: "sessions".to_owned(),
                sql: "SELECT * FROM sessions WHERE id = ?1".to_owned(),
            },
            SnapshotQuery {
                name: "messages".to_owned(),
                sql: "SELECT * FROM messages WHERE session_id = ?1 ORDER BY id".to_owned(),
            },
            SnapshotQuery {
                name: "session_model_usage".to_owned(),
                sql: r#"
                    SELECT *
                    FROM session_model_usage
                    WHERE session_id = ?1
                    ORDER BY model, billing_provider, billing_base_url, billing_mode, task
                "#
                .to_owned(),
            },
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::HermesAdapter;
    use crate::spec;
    use crate::state::HermesState;
    use rosetta_adapter_support::adapter::{AgentAdapter, StreamDriver};

    #[test]
    fn only_session_stores_are_admitted_as_snapshots() {
        for stream in [spec::STATE_DB, spec::PROFILE_STATE_DB] {
            assert_eq!(
                HermesAdapter.stream_driver(stream),
                Some(StreamDriver::SqliteSnapshot)
            );
            assert!(HermesAdapter.snapshot_plan(stream).is_some());
        }
        for inventory in [
            "config",
            "soul",
            "sessions-legacy",
            "profile-home",
            "farm:messages",
        ] {
            assert_eq!(HermesAdapter.stream_driver(inventory), None);
            assert!(HermesAdapter.snapshot_plan(inventory).is_none());
        }
        let plan = HermesAdapter
            .snapshot_plan(spec::STATE_DB)
            .expect("state plan");
        let messages = plan
            .record_queries
            .iter()
            .find(|query| query.name == "messages")
            .expect("messages query");
        assert!(messages.sql.contains("ORDER BY id"));
        assert!(
            plan.record_queries
                .iter()
                .any(|query| query.name == "state_meta")
        );
    }

    #[test]
    fn adapter_state_round_trips_through_the_checkpoint() {
        let adapter = HermesAdapter;
        let fresh = adapter.restore_state(None).unwrap().expect("initial state");
        assert_eq!(
            fresh.downcast_ref::<HermesState>(),
            Some(&HermesState::default())
        );
        let state = HermesState {
            codex_runtime: true,
        };
        let checkpoint = adapter
            .checkpoint_state(Some(&state))
            .unwrap()
            .expect("serialized state");
        let restored = adapter
            .restore_state(Some(&checkpoint))
            .unwrap()
            .expect("restored state");
        assert_eq!(restored.downcast_ref::<HermesState>(), Some(&state));
        assert!(adapter.checkpoint_state(Some(&7u8)).is_err());
        assert!(
            adapter
                .restore_state(Some(&serde_json::json!("not a state")))
                .is_err()
        );
    }

    #[test]
    fn session_query_preserves_parent_root_and_role() {
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE sessions (
                    id TEXT PRIMARY KEY,
                    title TEXT,
                    cwd TEXT,
                    started_at REAL NOT NULL,
                    last_activity_at REAL,
                    ended_at REAL,
                    parent_session_id TEXT
                 );
                 INSERT INTO sessions VALUES ('root', 'Root', '/w', 1, 4, NULL, NULL);
                 INSERT INTO sessions VALUES ('child', 'Child', '/w', 2, 3, NULL, 'root');",
            )
            .unwrap();

        let plan = HermesAdapter
            .snapshot_plan(spec::STATE_DB)
            .expect("state plan");
        let mut statement = connection.prepare(&plan.sessions_sql).unwrap();
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>("native_session_id")?,
                    row.get::<_, Option<String>>("parent_native_session_id")?,
                    row.get::<_, Option<String>>("root_native_session_id")?,
                    row.get::<_, String>("role")?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();

        assert_eq!(
            rows,
            vec![
                (
                    "root".to_owned(),
                    None,
                    Some("root".to_owned()),
                    "main".to_owned(),
                ),
                (
                    "child".to_owned(),
                    Some("root".to_owned()),
                    Some("root".to_owned()),
                    "subagent".to_owned(),
                ),
            ]
        );
    }
}
