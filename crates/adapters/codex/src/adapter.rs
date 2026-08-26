//! The `AgentAdapter` implementation — all codex knowledge stops here.

use std::any::Any;

use rosetta_adapter_support::GroundTruthDoc;
use rosetta_adapter_support::adapter::{
    AgentAdapter, FileStat, LineOutcome, NormalizeCtx, SkimResult, StreamDriver,
};

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
        (stream_id == spec::ROLLOUT).then_some(StreamDriver::JsonLines)
    }

    fn normalizer_version(&self) -> u32 {
        3
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
    /// those facts could build (`SessionMeta`, `WorkspaceObs`) are keyed on a rosetta
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
        if self.stream_driver(stream_id).is_none() {
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
            agent_version: Some(meta.cli_version.clone()),
            // NOTHING codex writes names a session: `session_meta` carries only
            // identity and provenance, and the one human-readable string near
            // it — `turn_context.summary` — is the reasoning-summary policy
            // ("auto"), not a title. The field stays None for this agent.
            title: None,
            // The payload's own stamp is when the session opened; the envelope's
            // is when the record was written. `normalize::session_meta` prefers
            // them in the same order.
            started_at: rfc3339_ms(&meta.timestamp).or_else(|| rfc3339_ms(&record.timestamp)),
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
    use super::{CodexAdapter, first_complete_line};
    use crate::spec;
    use rosetta_adapter_support::adapter::{AgentAdapter, FileStat, SkimResult};

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
        // Codex names no session — see the note on `skim`.
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
}
