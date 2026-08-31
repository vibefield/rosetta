//! The current `AgentAdapter` implementation for Grok Build.

use std::any::Any;

use rosetta_adapter_support::GroundTruthDoc;
use rosetta_adapter_support::adapter::{
    AgentAdapter, FileStat, LineOutcome, NormalizeCtx, SkimResult, StreamDriver,
};
use rosetta_core::rows::TitleKind;
use serde_json::Value;

use crate::normalize::{self, GrokState};
use crate::spec;

pub struct GrokAdapter;

impl AgentAdapter for GrokAdapter {
    fn kind(&self) -> &'static str {
        "grok"
    }

    fn layout(&self) -> &GroundTruthDoc {
        spec::doc()
    }

    /// `updates` alone owns canonical conversation rows. `summary` is a
    /// metadata-only document snapshot that admits summary-only sessions and
    /// enriches transcript sessions without duplicating messages. `chat-history`
    /// repeats the raw model messages, and `events` repeats turn/tool
    /// telemetry; admitting either would make duplicate logical events
    /// possible. JSON sidecars and text files remain inventory as well.
    fn stream_driver(&self, stream_id: &str) -> Option<StreamDriver> {
        match stream_id {
            spec::UPDATES => Some(StreamDriver::JsonLines),
            spec::SUMMARY => Some(StreamDriver::DocumentSnapshot),
            _ => None,
        }
    }

    fn normalizer_version(&self) -> u32 {
        4
    }

    fn init_state(&self) -> Option<Box<dyn Any + Send>> {
        Some(Box::new(GrokState::default()))
    }

    fn checkpoint_state(&self, state: Option<&(dyn Any + Send)>) -> Result<Option<Value>, String> {
        let state = state
            .and_then(|state| state.downcast_ref::<GrokState>())
            .ok_or_else(|| "grok checkpoint state has the wrong type".to_owned())?;
        state
            .checkpoint_value()
            .map(Some)
            .map_err(|error| error.to_string())
    }

    fn restore_state(
        &self,
        checkpoint: Option<&Value>,
    ) -> Result<Option<Box<dyn Any + Send>>, String> {
        match checkpoint {
            None => Ok(self.init_state()),
            Some(value) => serde_json::from_value::<GrokState>(value.clone())
                .map(|state| Some(Box::new(state) as Box<dyn Any + Send>))
                .map_err(|error| error.to_string()),
        }
    }

    /// `updates` stamps the native session id on every complete record. A
    /// clipped bounded window falls back to the session directory, whose
    /// second wildcard is the same UUID in every committed fixture and whose
    /// meaning is stated by the canonical session path.
    ///
    fn skim(&self, stream_id: &str, stat: &FileStat, head: &[u8], tail: &[u8]) -> SkimResult {
        if stream_id == spec::SUMMARY {
            let Ok(text) = std::str::from_utf8(head) else {
                return SkimResult::default();
            };
            let Ok(summary) = crate::raw::parse_summary(text) else {
                return SkimResult::default();
            };
            let parent_session_id = summary.parent_session_id.filter(|value| !value.is_empty());
            let title = summary.generated_title.filter(|value| !value.is_empty());
            return SkimResult {
                ops: Vec::new(),
                session_id: Some(summary.info.id),
                root_session_id: None,
                role: parent_session_id
                    .as_ref()
                    .map(|_| rosetta_core::rows::SessionRole::Subagent),
                parent_session_id,
                cwd: (!summary.info.cwd.is_empty()).then_some(summary.info.cwd),
                agent_version: None,
                title_kind: title.as_ref().map(|_| TitleKind::Generated),
                title,
                summary: (!summary.session_summary.is_empty()).then_some(summary.session_summary),
                started_at: rosetta_adapter_support::rfc3339_ms(&summary.created_at),
                updated_at: None,
            };
        }
        if stream_id != spec::UPDATES {
            return SkimResult::default();
        }

        let from_record = records(head, false)
            .chain(records(tail, (tail.len() as u64) < stat.size))
            .find_map(|record| {
                record["params"]["sessionId"]
                    .as_str()
                    .filter(|value| !value.is_empty())
                    .map(str::to_owned)
            });

        SkimResult {
            ops: Vec::new(),
            session_id: from_record.or_else(|| session_from_path(&stat.relative_path)),
            parent_session_id: None,
            root_session_id: None,
            role: None,
            cwd: None,
            agent_version: None,
            title: None,
            title_kind: None,
            summary: None,
            started_at: None,
            updated_at: None,
        }
    }

    fn document_records(
        &self,
        stream_id: &str,
        _native_session_id: &str,
        _document: &[u8],
    ) -> Vec<Vec<u8>> {
        debug_assert_eq!(stream_id, spec::SUMMARY);
        Vec::new()
    }

    fn ingest_line(&self, ctx: &mut NormalizeCtx, stream_id: &str, line: &[u8]) -> LineOutcome {
        // Admission control is authoritative even for direct callers: an
        // inventory rail can never accidentally emit overlapping messages.
        if stream_id == spec::SUMMARY || self.stream_driver(stream_id).is_none() {
            return LineOutcome::parsed(Vec::new());
        }

        let mut state = take_state(ctx);
        let outcome = normalize::ingest_line(ctx, &mut state, stream_id, line);
        ctx.state = Some(state);
        outcome
    }
}

fn take_state(ctx: &mut NormalizeCtx) -> Box<GrokState> {
    match ctx.state.take() {
        Some(state) => state.downcast::<GrokState>().unwrap_or_default(),
        None => Box::default(),
    }
}

/// Parse only whole JSON records from a bounded window. Head starts at byte
/// zero and keeps its first record; a partial tail starts mid-record and drops
/// its first fragment.
fn records(bytes: &[u8], skip_first: bool) -> impl Iterator<Item = Value> + '_ {
    bytes
        .split(|byte| *byte == b'\n')
        .skip(usize::from(skip_first && !bytes.is_empty()))
        .filter(|line| !line.is_empty())
        .filter_map(|line| serde_json::from_slice(line).ok())
}

fn session_from_path(relative_path: &str) -> Option<String> {
    let parts: Vec<_> = relative_path.split('/').collect();
    match parts.as_slice() {
        ["sessions", _workspace, session, "updates.jsonl"] if !session.is_empty() => {
            Some((*session).to_owned())
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use rosetta_adapter_support::adapter::{AgentAdapter, FileStat};

    use super::GrokAdapter;
    use crate::spec;

    const PATH: &str = "sessions/%2Fw%2Fproject/session-from-path/updates.jsonl";
    const LINE: &str = r#"{"timestamp":1787634571,"method":"session/update","params":{"sessionId":"session-from-record","update":{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"hello"}}}}"#;

    fn stat(size: u64) -> FileStat {
        FileStat {
            relative_path: PATH.to_owned(),
            size,
            modified_ms: 0,
        }
    }

    #[test]
    fn skim_reads_a_whole_file_without_a_trailing_newline() {
        let result = GrokAdapter.skim(
            spec::UPDATES,
            &stat(LINE.len() as u64),
            LINE.as_bytes(),
            b"",
        );
        assert_eq!(result.session_id.as_deref(), Some("session-from-record"));
        assert!(result.ops.is_empty());
    }

    #[test]
    fn skim_recovers_from_a_cut_head_using_the_tail() {
        let tail = format!("cut fragment}}\n{LINE}\n");
        let result = GrokAdapter.skim(
            spec::UPDATES,
            &stat(4_096),
            b"{\"timestamp\":",
            tail.as_bytes(),
        );
        assert_eq!(result.session_id.as_deref(), Some("session-from-record"));
    }

    #[test]
    fn skim_minimal_windows_fall_back_to_the_native_session_directory() {
        let result = GrokAdapter.skim(spec::UPDATES, &stat(4_096), b"", b"");
        assert_eq!(result.session_id.as_deref(), Some("session-from-path"));
        assert!(result.cwd.is_none());
        assert!(result.title.is_none());
        assert!(result.started_at.is_none());
    }

    #[test]
    fn skim_ignores_non_driven_streams() {
        let result = GrokAdapter.skim(
            spec::CHAT_HISTORY,
            &stat(LINE.len() as u64),
            LINE.as_bytes(),
            b"",
        );
        assert!(result.session_id.is_none());
        assert!(result.ops.is_empty());
    }

    #[test]
    fn only_updates_has_a_driver() {
        assert_eq!(
            GrokAdapter.stream_driver(spec::UPDATES),
            Some(rosetta_adapter_support::StreamDriver::JsonLines)
        );
        assert_eq!(
            GrokAdapter.stream_driver(spec::SUMMARY),
            Some(rosetta_adapter_support::StreamDriver::DocumentSnapshot)
        );
        for inventory in [spec::CHAT_HISTORY, spec::EVENTS] {
            assert!(GrokAdapter.stream_driver(inventory).is_none());
        }
    }

    #[test]
    fn summary_document_supplies_session_metadata() {
        let path = "sessions/%2Fw%2Fproject/session-from-path/summary.json";
        let document = br#"{
            "info":{"id":"child","cwd":"/work/project"},
            "session_summary":"Investigate the parser",
            "created_at":"2026-08-25T05:09:30.810488Z",
            "updated_at":"2026-08-25T05:09:31Z",
            "num_messages":3,"num_chat_messages":4,
            "current_model_id":"grok","next_trace_turn":1,
            "chat_format_version":1,"grok_home":"/tmp/grok",
            "agent_name":"grok-build","generated_title":"Parser audit",
            "parent_session_id":"parent"
        }"#;
        let result = GrokAdapter.skim(
            spec::SUMMARY,
            &FileStat {
                relative_path: path.to_owned(),
                size: document.len() as u64,
                modified_ms: 0,
            },
            document,
            &[],
        );
        assert_eq!(result.session_id.as_deref(), Some("child"));
        assert_eq!(result.parent_session_id.as_deref(), Some("parent"));
        assert_eq!(result.cwd.as_deref(), Some("/work/project"));
        assert_eq!(result.title.as_deref(), Some("Parser audit"));
        assert_eq!(result.summary.as_deref(), Some("Investigate the parser"));
        assert_eq!(result.started_at, Some(1_787_634_570_810));
        assert_eq!(result.role, Some(rosetta_core::rows::SessionRole::Subagent));
    }
}
