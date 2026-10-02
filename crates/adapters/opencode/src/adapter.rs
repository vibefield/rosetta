//! The `AgentAdapter` implementation and SQLite snapshot admission plan.

use std::any::Any;
use std::collections::BTreeSet;

use rosetta_adapter_support::{
    AgentAdapter, FileStat, GroundTruthDoc, LineOutcome, NormalizeCtx, SkimResult, SnapshotPlan,
    StreamDriver,
};
use serde_json::Value;

use crate::normalize::{self, OpenCodeState};
use crate::{plan, spec};

pub struct OpenCodeAdapter;

impl OpenCodeAdapter {
    /// The plan for a database whose table inventory is known: OpenCode 2.x
    /// writes `session_v2`/`session_message` into the same file the 1.x
    /// tables live in, and a plan must only name tables that exist
    /// (`plan::plan_for_tables`). The `AgentAdapter::snapshot_plan` contract
    /// has no inventory and returns the 1.x plan; the pipeline selects the
    /// 2.x plans through this method once it passes the snapshot's tables.
    pub fn plan_for_tables(
        &self,
        stream_id: &str,
        tables: &BTreeSet<String>,
    ) -> Option<SnapshotPlan> {
        plan::plan_for_tables(stream_id, tables)
    }
}

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
        // 9: fork copies by session creation time, imported step-finish
        // usage kept and dated at completion, bucket rule per record from
        // `tokens.total`, preview-build bands, provider-segment routes,
        // legacy message shapes, and the 2.x `session_message` reader
        // (2026-09-23 audit, OPENCODE-1/2/3/6/10).
        // 10: image members are WebP with bounded display renditions.
        // 11: recorded tool images use durable optimized attachment members.
        11
    }

    fn snapshot_plan(&self, stream_id: &str) -> Option<SnapshotPlan> {
        (stream_id == spec::DATABASE).then(plan::v1_plan)
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::OpenCodeAdapter;
    use crate::spec;
    use rosetta_adapter_support::{AgentAdapter, FileStat, StreamDriver};

    #[test]
    fn only_the_database_stream_is_driven() {
        assert_eq!(
            OpenCodeAdapter.stream_driver(spec::DATABASE),
            Some(StreamDriver::SqliteSnapshot)
        );
        for inventory in [
            "log",
            "snapshot",
            "farm:headless-stdout",
            "opencode-next-db",
        ] {
            assert_eq!(OpenCodeAdapter.stream_driver(inventory), None);
        }
    }

    #[test]
    fn the_contract_plan_is_the_1x_plan() {
        let plan = OpenCodeAdapter
            .snapshot_plan(spec::DATABASE)
            .expect("snapshot plan");
        assert!(plan.sessions_sql.contains("native_session_id"));
        assert!(plan.sessions_sql.contains("root_native_session_id"));
        assert!(!plan.sessions_sql.contains("session_v2"));
        assert_eq!(plan.record_queries.len(), 1);
        assert_eq!(plan.record_queries[0].name, spec::TIMELINE_QUERY);
        assert!(plan.record_queries[0].sql.contains("first_event_seq"));
        assert!(plan.record_queries[0].sql.contains("session_time_created"));
        assert!(OpenCodeAdapter.snapshot_plan("log").is_none());

        let v1_tables: BTreeSet<String> = ["session", "message", "part", "event"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        let inventory_plan = OpenCodeAdapter
            .plan_for_tables(spec::DATABASE, &v1_tables)
            .expect("inventory plan");
        assert_eq!(inventory_plan.sessions_sql, plan.sessions_sql);
        assert_eq!(inventory_plan.record_queries.len(), 1);
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
}
