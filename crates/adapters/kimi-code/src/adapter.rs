//! The `AgentAdapter` implementation for Kimi Code.

use std::any::Any;

use rosetta_adapter_support::{
    AgentAdapter, FileStat, GroundTruthDoc, LineOutcome, NormalizeCtx, SkimResult, StreamDriver,
};
use rosetta_core::rows::{SessionRole, TitleKind};
use serde_json::Value;

use crate::normalize::{self, KimiCodeState};
use crate::spec;
use crate::time::epoch_ms;

pub struct KimiCodeAdapter;

impl AgentAdapter for KimiCodeAdapter {
    fn kind(&self) -> &'static str {
        "kimi-code"
    }

    fn layout(&self) -> &GroundTruthDoc {
        spec::doc()
    }

    fn stream_driver(&self, stream_id: &str) -> Option<StreamDriver> {
        match stream_id {
            spec::WIRE => Some(StreamDriver::JsonLines),
            spec::SESSION_STATE => Some(StreamDriver::DocumentSnapshot),
            _ => None,
        }
    }

    fn normalizer_version(&self) -> u32 {
        11
    }

    fn init_state(&self) -> Option<Box<dyn Any + Send>> {
        Some(Box::new(KimiCodeState::default()))
    }

    fn checkpoint_state(&self, state: Option<&(dyn Any + Send)>) -> Result<Option<Value>, String> {
        let state = state
            .and_then(|state| state.downcast_ref::<KimiCodeState>())
            .ok_or_else(|| "kimi-code checkpoint state has the wrong type".to_owned())?;
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
            Some(value) => serde_json::from_value::<KimiCodeState>(value.clone())
                .map(|state| Some(Box::new(state) as Box<dyn Any + Send>))
                .map_err(|error| error.to_string()),
        }
    }

    /// Kimi puts no enclosing session id inside a wire record. Its native path
    /// supplies that identity: the state and session-index fixtures repeat the
    /// same session id as the third path segment. Each non-main agent is a
    /// distinct session under that root, so its native id is the stable native
    /// subpath `<root>/agents/<agent-id>` rather than the globally repeating
    /// presentation name `agent-0`.
    fn skim(&self, stream_id: &str, stat: &FileStat, head: &[u8], tail: &[u8]) -> SkimResult {
        if stream_id != spec::WIRE {
            return SkimResult::default();
        }
        let Some(identity) = wire_identity(&stat.relative_path) else {
            return SkimResult::default();
        };

        let head_is_whole = head.len() as u64 >= stat.size;
        let tail_is_whole = tail.len() as u64 >= stat.size;
        let mut facts = WireFacts::default();
        for record in records(head_lines(head, head_is_whole)) {
            facts.absorb(&record, true);
        }
        for record in records(tail_lines(tail, tail_is_whole)).rev() {
            facts.absorb(&record, tail_is_whole);
        }

        SkimResult {
            ledger_only: false,
            ops: Vec::new(),
            session_id: Some(identity.native_session_id),
            parent_session_id: identity.parent_session_id,
            delegation: None,
            root_session_id: identity.root_session_id,
            role: identity.role,
            cwd: facts.cwd,
            project: None,
            // `metadata.protocol_version` is a wire protocol version, not the
            // Kimi Code release that wrote the file.
            agent_version: None,
            // The mutable title lives only in non-driven state.json.
            title: None,
            title_kind: None,
            summary: None,
            started_at: facts.started_at,
            updated_at: None,
        }
    }

    fn document_sessions(
        &self,
        stream_id: &str,
        _stat: &FileStat,
        document: &[u8],
    ) -> Vec<SkimResult> {
        if stream_id != spec::SESSION_STATE {
            return Vec::new();
        }
        let Ok(text) = std::str::from_utf8(document) else {
            return Vec::new();
        };
        let Ok(state) = crate::raw::parse_session_state(text) else {
            return Vec::new();
        };
        let root = state.id;
        let cwd = (!state.cwd.is_empty()).then_some(state.cwd);
        let started_at = epoch_ms(state.created_at);
        let title = state.title.filter(|title| !title.is_empty());
        let mut sessions = vec![SkimResult {
            ledger_only: false,
            ops: Vec::new(),
            session_id: Some(root.clone()),
            parent_session_id: None,
            delegation: None,
            root_session_id: None,
            role: Some(SessionRole::Main),
            cwd: cwd.clone(),
            project: None,
            agent_version: None,
            title_kind: title.as_ref().map(|_| TitleKind::Explicit),
            title,
            summary: None,
            started_at,
            updated_at: None,
        }];
        for (agent_id, agent) in state.agents {
            if agent_id == "main" || agent.agent_type == "main" {
                continue;
            }
            let parent_session_id = match agent.parent_agent_id.as_deref() {
                Some("main") | None => root.clone(),
                Some(parent) => format!("{root}/agents/{parent}"),
            };
            sessions.push(SkimResult {
                ledger_only: false,
                ops: Vec::new(),
                session_id: Some(format!("{root}/agents/{agent_id}")),
                parent_session_id: Some(parent_session_id),
                delegation: None,
                root_session_id: Some(root.clone()),
                role: Some(SessionRole::Subagent),
                cwd: cwd.clone(),
                project: None,
                agent_version: None,
                title: None,
                title_kind: None,
                summary: None,
                started_at,
                updated_at: None,
            });
        }
        sessions
    }

    fn document_records(
        &self,
        stream_id: &str,
        _native_session_id: &str,
        _document: &[u8],
    ) -> Vec<Vec<u8>> {
        debug_assert_eq!(stream_id, spec::SESSION_STATE);
        Vec::new()
    }

    fn ingest_line(&self, ctx: &mut NormalizeCtx, stream_id: &str, line: &[u8]) -> LineOutcome {
        if stream_id == spec::SESSION_STATE || self.stream_driver(stream_id).is_none() {
            return LineOutcome::parsed(Vec::new());
        }
        let mut state = take_state(ctx);
        let outcome = normalize::ingest_line(ctx, &mut state, stream_id, line);
        ctx.state = Some(state);
        outcome
    }
}

fn take_state(ctx: &mut NormalizeCtx) -> Box<KimiCodeState> {
    match ctx.state.take() {
        Some(state) => state.downcast::<KimiCodeState>().unwrap_or_default(),
        None => Box::default(),
    }
}

struct WireIdentity {
    native_session_id: String,
    parent_session_id: Option<String>,
    root_session_id: Option<String>,
    role: Option<SessionRole>,
}

fn wire_identity(relative_path: &str) -> Option<WireIdentity> {
    let parts: Vec<&str> = relative_path.split('/').collect();
    if parts.len() != 6
        || parts[0] != "sessions"
        || parts[1].is_empty()
        || parts[2].is_empty()
        || parts[3] != "agents"
        || parts[4].is_empty()
        || parts[5] != "wire.jsonl"
    {
        return None;
    }
    let root = parts[2];
    let agent = parts[4];
    if agent == "main" {
        return Some(WireIdentity {
            native_session_id: root.to_owned(),
            parent_session_id: None,
            root_session_id: None,
            role: Some(SessionRole::Main),
        });
    }
    let is_subagent = agent
        .strip_prefix("agent-")
        .is_some_and(|suffix| !suffix.is_empty());
    // The current canonical corpus has one child and its state entry says
    // `parentAgentId = main`. No child wire carries that field, and the Agent
    // tool reports its child id only inside free-form result text. Therefore
    // the current native grammar justifies the enclosing root as parent, but
    // cannot represent a future nested subagent's immediate parent without
    // new structural evidence or a wider skim contract.
    Some(WireIdentity {
        native_session_id: format!("{root}/agents/{agent}"),
        parent_session_id: is_subagent.then(|| root.to_owned()),
        root_session_id: is_subagent.then(|| root.to_owned()),
        role: is_subagent.then_some(SessionRole::Subagent),
    })
}

#[derive(Default)]
struct WireFacts {
    cwd: Option<String>,
    started_at: Option<i64>,
}

impl WireFacts {
    fn absorb(&mut self, record: &Value, allow_started_at: bool) {
        match record["type"].as_str() {
            Some("metadata") if allow_started_at && self.started_at.is_none() => {
                self.started_at = record["created_at"].as_u64().and_then(epoch_ms);
            }
            Some("profile.bind" | "config.update") if self.cwd.is_none() => {
                self.cwd = record["environmentDisclosure"]["cwd"]
                    .as_str()
                    .filter(|cwd| !cwd.is_empty())
                    .map(str::to_owned);
            }
            _ => {}
        }
    }
}

fn records<'a>(lines: impl Iterator<Item = &'a [u8]>) -> std::vec::IntoIter<Value> {
    lines
        .filter_map(|line| serde_json::from_slice(line).ok())
        .collect::<Vec<_>>()
        .into_iter()
}

fn head_lines(head: &[u8], whole_file: bool) -> impl Iterator<Item = &[u8]> {
    let mut lines: Vec<&[u8]> = head.split(|byte| *byte == b'\n').collect();
    if !whole_file && !head.ends_with(b"\n") {
        lines.pop();
    }
    lines.into_iter().filter(|line| !line.is_empty())
}

fn tail_lines(tail: &[u8], whole_file: bool) -> impl Iterator<Item = &[u8]> {
    let mut lines: Vec<&[u8]> = tail.split(|byte| *byte == b'\n').collect();
    if !whole_file && !lines.is_empty() {
        lines.remove(0);
    }
    lines.into_iter().filter(|line| !line.is_empty())
}

#[cfg(test)]
mod tests {
    use super::KimiCodeAdapter;
    use crate::spec;
    use rosetta_adapter_support::{AgentAdapter, FileStat};
    use rosetta_core::rows::SessionRole;

    const ROOT: &str = "session_873ba5dd-4e9d-4733-89a5-a7b411338f94";

    fn path(agent: &str) -> String {
        format!("sessions/wd_project_87dbbc8da657/{ROOT}/agents/{agent}/wire.jsonl")
    }

    fn stat(agent: &str, size: usize) -> FileStat {
        FileStat {
            relative_path: path(agent),
            size: size as u64,
            modified_ms: 0,
        }
    }

    fn wire() -> Vec<u8> {
        br#"{"type":"metadata","protocol_version":"1.5","created_at":1787636369430}
{"type":"runtime.set_binding","workspaceId":"wd_project_87dbbc8da657","runtimeId":"local","agentId":"main","time":1787636369446}
{"type":"profile.bind","agentId":"main","modelAlias":"mock","profileName":"agent","thinkingEffort":"medium","systemPrompt":"x","environmentDisclosure":{"cwd":"<PROJECT>"},"agentsMdPaths":[],"activeToolNames":[],"disallowedTools":[],"time":1787636369454}"#
            .to_vec()
    }

    #[test]
    fn root_agent_identity_uses_the_enclosing_session() {
        let result = KimiCodeAdapter.skim(spec::WIRE, &stat("main", 0), b"", b"");
        assert_eq!(result.session_id.as_deref(), Some(ROOT));
        assert_eq!(result.parent_session_id, None);
        assert_eq!(result.root_session_id, None);
        assert_eq!(result.role, Some(SessionRole::Main));
    }

    #[test]
    fn child_agent_identity_is_composite_and_linked_to_root() {
        let result = KimiCodeAdapter.skim(spec::WIRE, &stat("agent-0", 0), b"", b"");
        assert_eq!(
            result.session_id.as_deref(),
            Some("session_873ba5dd-4e9d-4733-89a5-a7b411338f94/agents/agent-0")
        );
        assert_eq!(result.parent_session_id.as_deref(), Some(ROOT));
        assert_eq!(result.root_session_id.as_deref(), Some(ROOT));
        assert_eq!(result.role, Some(SessionRole::Subagent));
    }

    #[test]
    fn skim_handles_a_cut_head_and_cut_tail() {
        let whole = wire();
        let first_newline = whole
            .iter()
            .position(|byte| *byte == b'\n')
            .expect("metadata newline");
        let head = &whole[..first_newline + 20];
        let profile = whole
            .windows(b"{\"type\":\"profile.bind\"".len())
            .position(|window| window == b"{\"type\":\"profile.bind\"")
            .expect("profile line");
        let tail = &whole[profile - 7..];
        let result = KimiCodeAdapter.skim(spec::WIRE, &stat("main", whole.len()), head, tail);
        assert_eq!(result.session_id.as_deref(), Some(ROOT));
        assert_eq!(result.started_at, Some(1_787_636_369_430));
        assert_eq!(result.cwd.as_deref(), Some("<PROJECT>"));
    }

    #[test]
    fn whole_file_without_trailing_newline_is_read() {
        let whole = wire();
        let result = KimiCodeAdapter.skim(spec::WIRE, &stat("main", whole.len()), &whole, b"");
        assert_eq!(result.started_at, Some(1_787_636_369_430));
        assert_eq!(result.cwd.as_deref(), Some("<PROJECT>"));
        assert_eq!(result.agent_version, None, "protocol 1.5 is not CLI 0.38.0");
    }

    #[test]
    fn minimal_windows_still_use_native_path_identity() {
        let result = KimiCodeAdapter.skim(spec::WIRE, &stat("main", 4096), b"{", b"}");
        assert_eq!(result.session_id.as_deref(), Some(ROOT));
        assert_eq!(result.cwd, None);
        assert_eq!(result.started_at, None);
    }

    #[test]
    fn state_json_is_a_metadata_document() {
        let state = FileStat {
            relative_path: format!("sessions/wd_project/{ROOT}/state.json"),
            size: 2,
            modified_ms: 0,
        };
        let result = KimiCodeAdapter.skim(spec::SESSION_STATE, &state, b"{}", b"{}");
        assert!(result.session_id.is_none());
        assert!(result.ops.is_empty());
        assert_eq!(
            KimiCodeAdapter.stream_driver(spec::SESSION_STATE),
            Some(rosetta_adapter_support::StreamDriver::DocumentSnapshot)
        );
        assert_eq!(KimiCodeAdapter.stream_driver("session-index"), None);
    }

    #[test]
    fn state_document_enumerates_root_and_nested_agents() {
        let document = format!(
            r#"{{
                "id":"{ROOT}","version":2,"cwd":"/work/project",
                "createdAt":1787636369409,"updatedAt":1787636369614,
                "archived":false,"custom":{{}},"lastTurnReason":"completed",
                "isCustomTitle":true,"title":"State title",
                "agents":{{
                    "main":{{"homedir":"/tmp/main","type":"main"}},
                    "agent-0":{{"homedir":"/tmp/a0","type":"sub","parentAgentId":"main"}},
                    "agent-1":{{"homedir":"/tmp/a1","type":"sub","parentAgentId":"agent-0"}}
                }}
            }}"#
        );
        let sessions = KimiCodeAdapter.document_sessions(
            spec::SESSION_STATE,
            &FileStat {
                relative_path: format!("sessions/wd/{ROOT}/state.json"),
                size: document.len() as u64,
                modified_ms: 0,
            },
            document.as_bytes(),
        );
        assert_eq!(sessions.len(), 3);
        assert_eq!(sessions[0].session_id.as_deref(), Some(ROOT));
        assert_eq!(sessions[0].title.as_deref(), Some("State title"));
        assert_eq!(sessions[1].parent_session_id.as_deref(), Some(ROOT));
        let nested_parent = format!("{ROOT}/agents/agent-0");
        assert_eq!(
            sessions[2].parent_session_id.as_deref(),
            Some(nested_parent.as_str())
        );
        assert!(
            sessions
                .iter()
                .all(|session| session.cwd.as_deref() == Some("/work/project"))
        );
    }

    #[test]
    fn layout_matches_wire_paths() {
        let stream = KimiCodeAdapter.layout().matching_stream(&path("main"));
        assert_eq!(stream.map(|stream| stream.id.as_str()), Some(spec::WIRE));
    }
}
