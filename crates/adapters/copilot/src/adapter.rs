//! The `AgentAdapter` boundary: all Copilot-specific stream admission,
//! metadata skimming, and normalization routing lives here.

use std::any::Any;

use rosetta_adapter_support::GroundTruthDoc;
use rosetta_adapter_support::adapter::{
    AgentAdapter, FileStat, LineOutcome, NormalizeCtx, SkimResult, StreamDriver,
};
use rosetta_core::rows::{SessionRole, TitleKind};
use serde_json::Value;

use crate::normalize::{self, CopilotState};
use crate::spec;
use crate::time::rfc3339_ms;

pub struct CopilotAdapter;

impl AgentAdapter for CopilotAdapter {
    fn kind(&self) -> &'static str {
        "copilot"
    }

    fn layout(&self) -> &GroundTruthDoc {
        spec::doc()
    }

    fn stream_driver(&self, stream_id: &str) -> Option<StreamDriver> {
        match stream_id {
            spec::EVENTS => Some(StreamDriver::JsonLines),
            spec::SESSION_WORKSPACE => Some(StreamDriver::DocumentSnapshot),
            _ => None,
        }
    }

    fn normalizer_version(&self) -> u32 {
        // 9: per-model shutdown intervals in exclusive buckets with billed
        // nano-AIU; no per-message output usage (§4.1.3).
        9
    }

    fn init_state(&self) -> Option<Box<dyn Any + Send>> {
        Some(Box::new(CopilotState::default()))
    }

    fn checkpoint_state(&self, state: Option<&(dyn Any + Send)>) -> Result<Option<Value>, String> {
        let state = state
            .and_then(|state| state.downcast_ref::<CopilotState>())
            .ok_or_else(|| "copilot checkpoint state has the wrong type".to_owned())?;
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
            Some(value) => serde_json::from_value::<CopilotState>(value.clone())
                .map(|state| Some(Box::new(state) as Box<dyn Any + Send>))
                .map_err(|error| error.to_string()),
        }
    }

    /// Read native facts from bounded windows only. The session id in the
    /// opening `session.start` is authoritative; the enclosing directory is
    /// the format-defined fallback when that first JSON line is clipped.
    fn skim(&self, stream_id: &str, stat: &FileStat, head: &[u8], tail: &[u8]) -> SkimResult {
        if stream_id == spec::SESSION_WORKSPACE {
            return workspace_skim(stat, head);
        }
        if stream_id != spec::EVENTS {
            return SkimResult::default();
        }

        let mut skim = Skim::default();
        for record in records(head_lines(head)) {
            skim.absorb(&record, true);
        }

        let whole_tail = tail.len() as u64 >= stat.size;
        for record in records(tail_lines(tail, whole_tail)) {
            skim.absorb(&record, whole_tail);
        }

        let session_id = skim
            .session_id
            .or_else(|| session_from_path(&stat.relative_path).map(str::to_owned));
        let role = skim
            .parent_session_id
            .as_ref()
            .map_or(SessionRole::Main, |_| SessionRole::Subagent);

        SkimResult {
            ledger_only: false,
            ops: Vec::new(),
            session_id,
            parent_session_id: skim.parent_session_id.clone(),
            delegation: None,
            // The grammar exposes an immediate spawning parent, but no
            // distinct root id. Until it does, that parent is the best native
            // root fact available for a detached child.
            root_session_id: skim.parent_session_id,
            role: Some(role),
            cwd: skim.cwd,
            project: None,
            agent_version: skim.agent_version,
            title: None,
            title_kind: None,
            summary: None,
            started_at: skim.started_at,
            updated_at: None,
        }
    }

    fn document_size_limit(&self, stream_id: &str, pipeline_limit: usize) -> usize {
        if stream_id == spec::SESSION_WORKSPACE {
            pipeline_limit.min(64 * 1024)
        } else {
            pipeline_limit
        }
    }

    fn document_records(
        &self,
        _stream_id: &str,
        _native_session_id: &str,
        _document: &[u8],
    ) -> Vec<Vec<u8>> {
        // Metadata is journaled through SessionMeta; events.jsonl remains the
        // only timeline source, and it uses the JSONL driver instead.
        Vec::new()
    }

    fn ingest_line(&self, ctx: &mut NormalizeCtx, stream_id: &str, line: &[u8]) -> LineOutcome {
        if stream_id != spec::EVENTS {
            return LineOutcome::parsed(Vec::new());
        }
        let mut state = take_state(ctx);
        let outcome = normalize::ingest_line(ctx, &mut state, stream_id, line);
        ctx.state = Some(state);
        outcome
    }
}

#[derive(Default)]
struct Workspace {
    id: Option<String>,
    cwd: Option<String>,
    name: Option<String>,
    created_at: Option<String>,
    updated_at: Option<String>,
}

fn workspace_skim(stat: &FileStat, document: &[u8]) -> SkimResult {
    let Some(workspace) = parse_workspace(document) else {
        return SkimResult::default();
    };
    let path_id = session_from_path(&stat.relative_path).map(str::to_owned);
    let session_id = match (workspace.id, path_id) {
        (Some(id), Some(path)) if id != path => {
            // An identity disagreement is a compatibility failure, not
            // permission to attach another session's title. Returning no
            // member increments the pipeline's explicit unidentified count.
            return SkimResult::default();
        }
        (Some(id), _) => Some(id),
        (None, path) => path,
    };
    let title = workspace.name.and_then(nonempty);
    SkimResult {
        ledger_only: false,
        ops: Vec::new(),
        session_id,
        parent_session_id: None,
        delegation: None,
        root_session_id: None,
        role: Some(SessionRole::Main),
        cwd: workspace.cwd.and_then(nonempty),
        project: None,
        agent_version: None,
        title_kind: title.as_ref().map(|_| TitleKind::Explicit),
        title,
        summary: None,
        started_at: workspace.created_at.as_deref().and_then(rfc3339_ms),
        updated_at: workspace.updated_at.as_deref().and_then(rfc3339_ms),
    }
}

fn nonempty(value: String) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn parse_workspace(document: &[u8]) -> Option<Workspace> {
    let text = std::str::from_utf8(document).ok()?;
    let mut workspace = Workspace::default();
    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') || matches!(line, "---" | "...") {
            continue;
        }
        let (key, raw_value) = line.split_once(':')?;
        let key = key.trim();
        if !matches!(key, "id" | "cwd" | "name" | "created_at" | "updated_at") {
            continue;
        }
        let value = yaml_scalar(raw_value)?;
        match key {
            "id" => workspace.id = value,
            "cwd" => workspace.cwd = value,
            "name" => workspace.name = value,
            "created_at" => workspace.created_at = value,
            "updated_at" => workspace.updated_at = value,
            _ => unreachable!(),
        }
    }
    Some(workspace)
}

/// The writer emits a flat YAML mapping. Decode its scalar spellings without
/// accepting nested/block YAML that could make identity or title ambiguous.
fn yaml_scalar(raw: &str) -> Option<Option<String>> {
    let raw = raw.trim();
    if raw.is_empty() || matches!(raw, "null" | "Null" | "NULL" | "~") {
        return Some(None);
    }
    if raw.starts_with('"') {
        return serde_json::from_str::<String>(raw).ok().map(Some);
    }
    if let Some(quoted) = raw.strip_prefix('\'') {
        let quoted = quoted.strip_suffix('\'')?;
        return Some(Some(quoted.replace("''", "'")));
    }
    if matches!(raw, "|" | ">" | "|-" | ">-" | "|+" | ">+") {
        return None;
    }
    let value = raw.split_once(" #").map_or(raw, |(value, _)| value).trim();
    Some((!value.is_empty()).then(|| value.to_owned()))
}

fn take_state(ctx: &mut NormalizeCtx) -> Box<CopilotState> {
    match ctx.state.take() {
        Some(state) => state.downcast::<CopilotState>().unwrap_or_default(),
        None => Box::default(),
    }
}

#[derive(Default)]
struct Skim {
    session_id: Option<String>,
    parent_session_id: Option<String>,
    cwd: Option<String>,
    agent_version: Option<String>,
    started_at: Option<i64>,
}

impl Skim {
    fn absorb(&mut self, record: &Value, may_date_session: bool) {
        match record["type"].as_str() {
            Some("session.start") => {
                fill(&mut self.session_id, record["data"]["sessionId"].as_str());
                fill(
                    &mut self.parent_session_id,
                    record["data"]["detachedFromSpawningParentSessionId"].as_str(),
                );
                fill(&mut self.cwd, record["data"]["context"]["cwd"].as_str());
                fill(
                    &mut self.agent_version,
                    record["data"]["copilotVersion"].as_str(),
                );
                if may_date_session && self.started_at.is_none() {
                    self.started_at = record["data"]["startTime"].as_str().and_then(rfc3339_ms);
                }
            }
            // A clipped head may hide the start record. Resume repeats the
            // current workspace but not the native session id or CLI version.
            Some("session.resume") => {
                fill(&mut self.cwd, record["data"]["context"]["cwd"].as_str());
            }
            _ => {}
        }
    }
}

fn fill(slot: &mut Option<String>, value: Option<&str>) {
    if slot.is_none() {
        *slot = value.filter(|text| !text.is_empty()).map(str::to_owned);
    }
}

fn records<'a>(lines: impl Iterator<Item = &'a [u8]>) -> impl Iterator<Item = Value> {
    lines.filter_map(|line| serde_json::from_slice(line).ok())
}

fn head_lines(head: &[u8]) -> impl Iterator<Item = &[u8]> {
    head.split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
}

fn tail_lines(tail: &[u8], whole_file: bool) -> impl Iterator<Item = &[u8]> {
    tail.split(|byte| *byte == b'\n')
        .skip(usize::from(!whole_file))
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
}

fn session_from_path(relative_path: &str) -> Option<&str> {
    let mut parts = relative_path.split('/');
    (parts.next()? == "session-state").then_some(())?;
    let session = parts.next()?;
    matches!(parts.next()?, "events.jsonl" | "workspace.yaml").then_some(())?;
    parts.next().is_none().then_some(())?;
    (!session.is_empty()).then_some(session)
}

#[cfg(test)]
mod tests {
    use super::CopilotAdapter;
    use crate::spec;
    use rosetta_adapter_support::adapter::{AgentAdapter, FileStat, StreamDriver};
    use rosetta_core::rows::SessionRole;

    const PATH: &str = "session-state/from-directory/events.jsonl";

    fn stat(path: &str, size: u64) -> FileStat {
        FileStat {
            relative_path: path.to_owned(),
            size,
            modified_ms: 0,
        }
    }

    fn start(session: &str) -> String {
        format!(
            r#"{{"id":"event-1","timestamp":"2026-08-25T05:27:58.278Z","parentId":null,"type":"session.start","data":{{"sessionId":"{session}","version":1,"producer":"copilot-agent","copilotVersion":"1.0.80","startTime":"2026-08-25T05:27:58.248Z","context":{{"cwd":"<PROJECT>"}}}}}}"#
        )
    }

    #[test]
    fn events_and_workspace_have_separate_drivers() {
        assert_eq!(
            CopilotAdapter.stream_driver(spec::EVENTS),
            Some(StreamDriver::JsonLines)
        );
        assert_eq!(
            CopilotAdapter.stream_driver(spec::SESSION_WORKSPACE),
            Some(StreamDriver::DocumentSnapshot)
        );
        assert_eq!(
            CopilotAdapter.document_size_limit(spec::SESSION_WORKSPACE, usize::MAX),
            64 * 1024
        );
        for inventory in [
            "session-db",
            "session-plans",
            "session-checkpoints",
            "session-store",
        ] {
            assert_eq!(
                CopilotAdapter.stream_driver(inventory),
                None,
                "{inventory} must remain inventory"
            );
        }
    }

    #[test]
    fn whole_file_skim_uses_native_start_facts() {
        let body = start("from-event");
        let result = CopilotAdapter.skim(
            spec::EVENTS,
            &stat(PATH, body.len() as u64),
            body.as_bytes(),
            body.as_bytes(),
        );
        assert_eq!(result.session_id.as_deref(), Some("from-event"));
        assert_eq!(result.cwd.as_deref(), Some("<PROJECT>"));
        assert_eq!(result.agent_version.as_deref(), Some("1.0.80"));
        assert_eq!(result.started_at, Some(1_787_635_678_248));
        assert_eq!(result.role, Some(SessionRole::Main));
        assert!(result.ops.is_empty());
    }

    #[test]
    fn cut_head_falls_back_to_the_session_directory() {
        let body = start("from-event");
        let result = CopilotAdapter.skim(
            spec::EVENTS,
            &stat(PATH, body.len() as u64),
            &body.as_bytes()[..24],
            b"clipped-tail",
        );
        assert_eq!(result.session_id.as_deref(), Some("from-directory"));
        assert!(result.cwd.is_none());
        assert!(result.agent_version.is_none());
        assert!(result.started_at.is_none());
    }

    #[test]
    fn minimal_empty_windows_still_preserve_path_identity() {
        let result = CopilotAdapter.skim(spec::EVENTS, &stat(PATH, 4096), b"", b"");
        assert_eq!(result.session_id.as_deref(), Some("from-directory"));
        assert_eq!(result.role, Some(SessionRole::Main));
        assert!(result.cwd.is_none());
    }

    #[test]
    fn non_driven_stream_has_no_skim_facts() {
        let body = start("not-a-session");
        let result = CopilotAdapter.skim(
            "session-db",
            &stat("session-state/not-a-session/session.db", body.len() as u64),
            body.as_bytes(),
            body.as_bytes(),
        );
        assert!(result.session_id.is_none());
        assert!(result.role.is_none());
        assert!(result.ops.is_empty());
    }

    #[test]
    fn detached_session_reports_native_parent_facts() {
        let body = start("child").replace(
            "\"startTime\"",
            "\"detachedFromSpawningParentSessionId\":\"parent\",\"startTime\"",
        );
        let result = CopilotAdapter.skim(
            spec::EVENTS,
            &stat(PATH, body.len() as u64),
            body.as_bytes(),
            b"",
        );
        assert_eq!(result.parent_session_id.as_deref(), Some("parent"));
        assert_eq!(result.root_session_id.as_deref(), Some("parent"));
        assert_eq!(result.role, Some(SessionRole::Subagent));
    }

    #[test]
    fn workspace_document_projects_title_and_native_times() {
        let body = br#"id: from-directory
cwd: /w/project
name: 'Codebase Overview: Session'
user_named: false
created_at: 2026-08-25T05:27:58.262Z
updated_at: 2026-08-25T05:27:58.417Z
"#;
        let result = CopilotAdapter.skim(
            spec::SESSION_WORKSPACE,
            &stat(
                "session-state/from-directory/workspace.yaml",
                body.len() as u64,
            ),
            body,
            b"",
        );
        assert_eq!(result.session_id.as_deref(), Some("from-directory"));
        assert_eq!(result.cwd.as_deref(), Some("/w/project"));
        assert_eq!(result.title.as_deref(), Some("Codebase Overview: Session"));
        assert_eq!(result.started_at, Some(1_787_635_678_262));
        assert_eq!(result.updated_at, Some(1_787_635_678_417));
    }

    #[test]
    fn workspace_identity_mismatch_is_not_attached() {
        let body = b"id: another-session\nname: Wrong title\n";
        let result = CopilotAdapter.skim(
            spec::SESSION_WORKSPACE,
            &stat(
                "session-state/from-directory/workspace.yaml",
                body.len() as u64,
            ),
            body,
            b"",
        );
        assert!(result.session_id.is_none());
        assert!(result.title.is_none());
    }
}
