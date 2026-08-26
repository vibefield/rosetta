//! The `AgentAdapter` implementation and stream admission boundary.

use std::any::Any;

use rosetta_adapter_support::GroundTruthDoc;
use rosetta_adapter_support::adapter::{
    AgentAdapter, FileStat, LineOutcome, NormalizeCtx, SkimResult, StreamDriver,
};
use rosetta_core::rows::SessionRole;
use serde_json::Value;

use crate::normalize::{self, AntigravityState};
use crate::spec;
use crate::time::rfc3339_ms;

pub struct AntigravityAdapter;

impl AgentAdapter for AntigravityAdapter {
    fn kind(&self) -> &'static str {
        "antigravity"
    }

    fn layout(&self) -> &GroundTruthDoc {
        spec::doc()
    }

    fn stream_driver(&self, stream_id: &str) -> Option<StreamDriver> {
        (stream_id == spec::TRANSCRIPT).then_some(StreamDriver::JsonLines)
    }

    fn normalizer_version(&self) -> u32 {
        1
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
    /// title, parent, or agent version. The oldest complete head record supplies
    /// only `started_at`; a cut record is ignored.
    fn skim(&self, stream_id: &str, stat: &FileStat, head: &[u8], _tail: &[u8]) -> SkimResult {
        if self.stream_driver(stream_id).is_none() {
            return SkimResult::default();
        }
        SkimResult {
            ops: Vec::new(),
            session_id: conversation_id(&stat.relative_path).map(str::to_owned),
            parent_session_id: None,
            root_session_id: None,
            role: Some(SessionRole::Main),
            cwd: None,
            agent_version: None,
            title: None,
            started_at: opening_stamp(head, head.len() as u64 >= stat.size),
        }
    }

    fn ingest_line(&self, ctx: &mut NormalizeCtx, stream_id: &str, line: &[u8]) -> LineOutcome {
        // Admission is repeated here defensively. Even a caller bypassing
        // `stream_driver` cannot normalize a byte-identical inventory sibling.
        if stream_id != spec::TRANSCRIPT {
            return LineOutcome::parsed(Vec::new());
        }

        let mut state = take_state(ctx);
        let outcome = normalize::ingest(ctx, &mut state, stream_id, line);
        ctx.state = Some(state);
        outcome
    }
}

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
    let complete = if whole_file || head.ends_with(b"\n") {
        head
    } else {
        let end = head.iter().rposition(|byte| *byte == b'\n')? + 1;
        &head[..end]
    };
    let line = complete
        .split(|byte| *byte == b'\n')
        .find(|line| !line.is_empty())?;
    let record = serde_json::from_slice::<Value>(line).ok()?;
    record["created_at"].as_str().and_then(rfc3339_ms)
}

#[cfg(test)]
mod tests {
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
        for inventory in [
            spec::TRANSCRIPT_FULL,
            spec::TRANSCRIPT_CHUNKS,
            spec::TRANSCRIPT_FULL_CHUNKS,
        ] {
            assert_eq!(adapter.stream_driver(inventory), None, "drove {inventory}");
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
