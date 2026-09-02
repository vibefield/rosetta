//! The `AgentAdapter` implementation — all codex knowledge stops here.

use std::any::Any;

use rosetta_adapter_support::GroundTruthDoc;
use rosetta_adapter_support::adapter::{
    AgentAdapter, FileStat, LineOutcome, NormalizeCtx, SkimResult, SnapshotPlan, StreamDriver,
};
use rosetta_core::rows::{SessionRole, TitleKind};
use serde_json::Value;

use crate::normalize::{self, CodexState};
use crate::raw::rollout::RolloutPayload;
use crate::raw::{ParsedLine, parse_line};
use crate::spec;
use crate::time::rfc3339_ms;

pub struct CodexAdapter;

impl AgentAdapter for CodexAdapter {
    fn kind(&self) -> &'static str {
        "codex"
    }

    fn layout(&self) -> &GroundTruthDoc {
        spec::doc()
    }

    fn stream_driver(&self, stream_id: &str) -> Option<StreamDriver> {
        match stream_id {
            spec::ROLLOUT => Some(StreamDriver::JsonLines),
            spec::STATE_DB => Some(StreamDriver::SqliteSnapshot),
            _ => None,
        }
    }

    fn normalizer_version(&self) -> u32 {
        8
    }

    fn snapshot_plan(&self, stream_id: &str) -> Option<SnapshotPlan> {
        (stream_id == spec::STATE_DB).then(|| SnapshotPlan {
            // `SELECT *` is intentional compatibility machinery: the base
            // `threads` columns predate optional `name` and millisecond stamp
            // columns. `snapshot_session` inspects whichever optional columns
            // this version exposes without emitting schema-dependent SQL.
            sessions_sql: "SELECT * FROM threads ORDER BY created_at, id".to_owned(),
            record_queries: Vec::new(),
        })
    }

    fn snapshot_session(&self, stream_id: &str, row: &Value) -> SkimResult {
        if stream_id != spec::STATE_DB {
            return SkimResult::default();
        }
        let (parent_session_id, subagent_source) = source_parent(row);
        let thread_source = row.get("thread_source").and_then(Value::as_str);
        let role = if parent_session_id.is_some()
            || subagent_source
            || matches!(thread_source, Some("subagent" | "guardian_review"))
        {
            SessionRole::Subagent
        } else {
            SessionRole::Main
        };
        let native_name = row.get("name").and_then(Value::as_str);
        let title = codex_display_title(native_name, row.get("title").and_then(Value::as_str));
        let title_kind = title.as_ref().map(|_| {
            if normalized_text(native_name).is_some() {
                TitleKind::Explicit
            } else {
                TitleKind::Fallback
            }
        });
        SkimResult {
            ops: Vec::new(),
            session_id: nonempty(row.get("id").and_then(Value::as_str)),
            parent_session_id: parent_session_id.clone(),
            root_session_id: parent_session_id,
            role: Some(role),
            cwd: nonempty(row.get("cwd").and_then(Value::as_str)),
            project: None,
            agent_version: nonempty(row.get("cli_version").and_then(Value::as_str)),
            title,
            title_kind,
            summary: None,
            started_at: timestamp_ms(row, "created_at_ms", "created_at"),
            updated_at: timestamp_ms(row, "updated_at_ms", "updated_at"),
        }
    }

    fn init_state(&self) -> Option<Box<dyn Any + Send>> {
        Some(Box::new(CodexState::default()))
    }

    fn checkpoint_state(
        &self,
        state: Option<&(dyn Any + Send)>,
    ) -> Result<Option<serde_json::Value>, String> {
        let state = state
            .and_then(|state| state.downcast_ref::<CodexState>())
            .ok_or_else(|| "codex checkpoint state has the wrong type".to_string())?;
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
            Some(value) => serde_json::from_value::<CodexState>(value.clone())
                .map(|state| Some(Box::new(state) as Box<dyn Any + Send>))
                .map_err(|error| error.to_string()),
        }
    }

    /// Meta phase: the native session facts, and no ops.
    ///
    /// A rollout file opens with its `session_meta` record, so the head window
    /// alone answers for every fact codex records about a session — but the ops
    /// those facts could build (`SessionMeta`) are keyed on a rosetta
    /// session id, which is `blake3(runtime_id, native_id)`, and the meta phase
    /// hands the adapter no runtime id. Deriving one here from the canonical
    /// `~/.codex` would silently disagree with the pipeline's whenever
    /// `CODEX_HOME` points elsewhere — two ids for one session is far worse than
    /// none — so the adapter reports the native facts and lets the stream phase,
    /// which does get a runtime id, emit the rows (`normalize::session_meta`).
    ///
    /// The tail window still has no consumer: every field below comes from the
    /// opening record, and the file's mtime already gives the pipeline the
    /// session's `updated_at`.
    fn skim(&self, stream_id: &str, stat: &FileStat, head: &[u8], _tail: &[u8]) -> SkimResult {
        if stream_id != spec::ROLLOUT {
            return SkimResult::default();
        }

        let whole_file = head.len() as u64 >= stat.size;
        let Some(first) = first_complete_line(head, whole_file) else {
            return SkimResult::default();
        };
        let Ok(ParsedLine::Known(record)) = parse_line(first) else {
            return SkimResult::default();
        };
        let RolloutPayload::SessionMeta(meta) = &record.payload else {
            return SkimResult::default();
        };
        let (parent_session_id, root_session_id, role) = match &meta.parent_thread_id {
            Some(parent) => (
                Some(parent.clone()),
                Some(parent.clone()),
                Some(rosetta_core::rows::SessionRole::Subagent),
            ),
            None => (None, None, Some(rosetta_core::rows::SessionRole::Main)),
        };
        SkimResult {
            ops: Vec::new(),
            session_id: Some(meta.id.clone()),
            parent_session_id,
            root_session_id,
            role,
            cwd: Some(meta.cwd.clone()),
            project: None,
            agent_version: Some(meta.cli_version.clone()),
            // Rollouts do not carry the display title. The state database is a
            // sibling metadata source and merges onto this same native id.
            title: None,
            title_kind: None,
            summary: None,
            // The payload's own stamp is when the session opened; the envelope's
            // is when the record was written. `normalize::session_meta` prefers
            // them in the same order.
            started_at: rfc3339_ms(&meta.timestamp).or_else(|| rfc3339_ms(&record.timestamp)),
            updated_at: None,
        }
    }

    fn ingest_line(&self, ctx: &mut NormalizeCtx, stream_id: &str, line: &[u8]) -> LineOutcome {
        // Owning the state for the call keeps it out of the borrow of `ctx`,
        // which every row needs mutably for its ordinal. One restore point, so
        // no projection path can lose it.
        let mut state = take_state(ctx);
        let outcome = normalize::ingest_line(ctx, &mut state, stream_id, line);
        ctx.state = Some(state);
        outcome
    }
}

const CODEX_FALLBACK_TITLE_CHARS: usize = 36;

fn nonempty(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn normalized_text(value: Option<&str>) -> Option<String> {
    let normalized = value?.split_whitespace().collect::<Vec<_>>().join(" ");
    (!normalized.is_empty()).then_some(normalized)
}

fn codex_display_title(name: Option<&str>, native_title: Option<&str>) -> Option<String> {
    if let Some(name) = normalized_text(name) {
        return Some(name);
    }
    let fallback = normalized_text(native_title)?;
    if fallback.chars().count() <= CODEX_FALLBACK_TITLE_CHARS {
        return Some(fallback);
    }
    let mut display: String = fallback
        .chars()
        .take(CODEX_FALLBACK_TITLE_CHARS - 1)
        .collect();
    display.push('…');
    Some(display)
}

fn timestamp_ms(row: &Value, millisecond_key: &str, second_key: &str) -> Option<i64> {
    row.get(millisecond_key)
        .and_then(Value::as_i64)
        .or_else(|| {
            row.get(second_key)
                .and_then(Value::as_i64)
                .and_then(|seconds| seconds.checked_mul(1_000))
        })
}

fn source_parent(row: &Value) -> (Option<String>, bool) {
    let Some(source) = row.get("source").and_then(Value::as_str) else {
        return (None, false);
    };
    let Ok(source) = serde_json::from_str::<Value>(source) else {
        return (None, false);
    };
    let subagent = source.get("subagent").is_some();
    let parent = source
        .pointer("/subagent/thread_spawn/parent_thread_id")
        .and_then(Value::as_str)
        .and_then(|value| nonempty(Some(value)));
    (parent, subagent)
}

fn take_state(ctx: &mut NormalizeCtx) -> Box<CodexState> {
    match ctx.state.take() {
        Some(state) => state.downcast::<CodexState>().unwrap_or_default(),
        // The pipeline is expected to call `init_state`; recovering here keeps a
        // missed call from silently reordering a session's rows.
        None => Box::default(),
    }
}

/// The first line of `head`, or `None` when the window cut it short. `head` may
/// end mid-record — the meta phase reads a fixed number of bytes, not lines.
fn first_complete_line(head: &[u8], whole_file: bool) -> Option<&str> {
    let end = match head.iter().position(|byte| *byte == b'\n') {
        Some(at) => at,
        None if whole_file => head.len(),
        None => return None,
    };
    std::str::from_utf8(&head[..end]).ok()
}

#[cfg(test)]
mod tests {
    use super::{CodexAdapter, codex_display_title, first_complete_line};
    use crate::spec;
    use rosetta_adapter_support::adapter::{AgentAdapter, FileStat, SkimResult};
    use rosetta_core::rows::SessionRole;

    const ROLLOUT: &str = "sessions/2026/08/24/rollout-2026-08-24T19-35-03-01a036c5.jsonl";

    fn stat(relative_path: &str, size: u64) -> FileStat {
        FileStat {
            relative_path: relative_path.to_owned(),
            size,
            modified_ms: 1_787_625_303_577,
        }
    }

    fn session_meta_line(id: &str) -> String {
        format!(
            r#"{{"timestamp":"2026-08-25T02:35:03.577Z","ordinal":0,"type":"session_meta","payload":{{"session_id":"{id}","id":"{id}","timestamp":"2026-08-25T02:35:03.577Z","cwd":"/w","originator":"codex_exec","cli_version":"0.149.1","source":"exec","thread_source":"user","model_provider":"mock","history_mode":"paginated","base_instructions":{{}},"context_window":{{}}}}}}"#
        )
    }

    /// Every field a `SkimResult` can carry is unanswered.
    fn assert_all_none(result: &SkimResult) {
        assert!(result.ops.is_empty());
        assert!(result.session_id.is_none());
        assert!(result.parent_session_id.is_none());
        assert!(result.root_session_id.is_none());
        assert!(result.role.is_none());
        assert!(result.cwd.is_none());
        assert!(result.agent_version.is_none());
        assert!(result.title.is_none());
        assert!(result.started_at.is_none());
    }

    #[test]
    fn skim_reports_the_native_session_id() {
        let line = session_meta_line("01a036c5-6e18-73f3-8c07-d4430763840b");
        let head = format!("{line}\n{{\"type\":\"event_msg\"");
        let result = CodexAdapter.skim(spec::ROLLOUT, &stat(ROLLOUT, 4096), head.as_bytes(), b"");
        assert_eq!(
            result.session_id.as_deref(),
            Some("01a036c5-6e18-73f3-8c07-d4430763840b")
        );
        // Nothing can be keyed to a session without a runtime id.
        assert!(result.ops.is_empty());
    }

    #[test]
    fn skim_reports_the_rest_of_the_opening_record() {
        let line = session_meta_line("01a036c5-6e18-73f3-8c07-d4430763840b");
        let head = format!("{line}\n{{\"type\":\"event_msg\"");
        let result = CodexAdapter.skim(spec::ROLLOUT, &stat(ROLLOUT, 4096), head.as_bytes(), b"");
        assert_eq!(result.cwd.as_deref(), Some("/w"));
        assert_eq!(result.agent_version.as_deref(), Some("0.149.1"));
        assert_eq!(result.started_at, Some(1_787_625_303_577));
        // The rollout itself has no title; state metadata is a sibling source.
        assert!(result.title.is_none());
    }

    #[test]
    fn skim_accepts_legacy_headers_without_envelope_ordinals() {
        let line = r#"{"timestamp":"2024-05-01T00:00:00Z","type":"session_meta","payload":{"id":"legacy","timestamp":"2024-05-01T00:00:00Z","cwd":"/legacy","originator":"codex_cli_rs","cli_version":"0.85.0","source":"cli","model_provider":"openai","instructions":"legacy"}}"#;
        let result = CodexAdapter.skim(
            spec::ROLLOUT,
            &stat(ROLLOUT, line.len() as u64),
            line.as_bytes(),
            b"",
        );
        assert_eq!(result.session_id.as_deref(), Some("legacy"));
        assert_eq!(result.cwd.as_deref(), Some("/legacy"));
        assert_eq!(result.role, Some(rosetta_core::rows::SessionRole::Main));
    }

    #[test]
    fn skim_preserves_current_subagent_topology() {
        let line = r#"{"timestamp":"2026-08-25T02:35:03.577Z","type":"session_meta","payload":{"id":"child","timestamp":"2026-08-25T02:35:03.577Z","cwd":"/w","originator":"codex_exec","cli_version":"0.149.1","source":{"subagent":{"other":"review"}},"model_provider":"openai","parent_thread_id":"parent"}}"#;
        let result = CodexAdapter.skim(
            spec::ROLLOUT,
            &stat(ROLLOUT, line.len() as u64),
            line.as_bytes(),
            b"",
        );
        assert_eq!(result.session_id.as_deref(), Some("child"));
        assert_eq!(result.parent_session_id.as_deref(), Some("parent"));
        assert_eq!(result.root_session_id.as_deref(), Some("parent"));
        assert_eq!(result.role, Some(rosetta_core::rows::SessionRole::Subagent));
    }

    #[test]
    fn skim_ignores_files_outside_the_rollout_stream() {
        let line = session_meta_line("01a036c5");
        let head = format!("{line}\n");
        let result = CodexAdapter.skim("config", &stat("config.toml", 64), head.as_bytes(), b"");
        assert_all_none(&result);
    }

    #[test]
    fn skim_needs_a_complete_first_record() {
        // A head window that stops mid-record yields nothing rather than a
        // half-parsed session id — or half a workspace path.
        let line = session_meta_line("01a036c5");
        let result = CodexAdapter.skim(
            spec::ROLLOUT,
            &stat(ROLLOUT, 4096),
            &line.as_bytes()[..40],
            b"",
        );
        assert_all_none(&result);
    }

    #[test]
    fn skim_ignores_a_rollout_that_opens_on_another_record() {
        // A rollout whose first record is not the session header answers for
        // nothing rather than guessing from a record that repeats no identity.
        let head = "{\"timestamp\":\"2026-08-25T02:35:03.577Z\",\"ordinal\":0,\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\"}}\n";
        let result = CodexAdapter.skim(spec::ROLLOUT, &stat(ROLLOUT, 4096), head.as_bytes(), b"");
        assert_all_none(&result);
    }

    #[test]
    fn a_whole_file_needs_no_trailing_newline() {
        assert_eq!(first_complete_line(b"one", true), Some("one"));
        assert_eq!(first_complete_line(b"one", false), None);
        assert_eq!(first_complete_line(b"one\ntwo", false), Some("one"));
    }

    #[test]
    fn state_metadata_prefers_name_and_bounds_unicode_fallback() {
        let named = serde_json::json!({
            "id": "named",
            "name": "  Review   Rosetta codebase  ",
            "title": "ignored first prompt",
            "cwd": "/w",
            "cli_version": "0.150.1",
            "created_at": 10,
            "updated_at_ms": 20_123,
            "source": "cli"
        });
        let result = CodexAdapter.snapshot_session(spec::STATE_DB, &named);
        assert_eq!(result.title.as_deref(), Some("Review Rosetta codebase"));
        assert_eq!(result.started_at, Some(10_000));
        assert_eq!(result.updated_at, Some(20_123));

        let long = format!("{}   tail", "界".repeat(40));
        let fallback = codex_display_title(None, Some(&long)).unwrap();
        assert_eq!(fallback.chars().count(), 36);
        assert!(fallback.ends_with('…'));
    }

    #[test]
    fn state_metadata_tolerates_legacy_optional_columns_and_recovers_parent() {
        let legacy = serde_json::json!({
            "id": "child",
            "title": "  legacy\n title  ",
            "cwd": "/w",
            "created_at": 10,
            "updated_at": 11,
            "source": "{\"subagent\":{\"thread_spawn\":{\"parent_thread_id\":\"parent\"}}}"
        });
        let result = CodexAdapter.snapshot_session(spec::STATE_DB, &legacy);
        assert_eq!(result.title.as_deref(), Some("legacy title"));
        assert_eq!(result.parent_session_id.as_deref(), Some("parent"));
        assert_eq!(result.root_session_id.as_deref(), Some("parent"));
        assert_eq!(result.role, Some(SessionRole::Subagent));
    }
}
