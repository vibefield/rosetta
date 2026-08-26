//! The `AgentAdapter` boundary: all Copilot-specific stream admission,
//! metadata skimming, and normalization routing lives here.

use std::any::Any;

use rosetta_adapter_support::GroundTruthDoc;
use rosetta_adapter_support::adapter::{
    AgentAdapter, FileStat, LineOutcome, NormalizeCtx, SkimResult, StreamDriver,
};
use rosetta_core::rows::SessionRole;
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
        (stream_id == spec::EVENTS).then_some(StreamDriver::JsonLines)
    }

    fn normalizer_version(&self) -> u32 {
        1
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
        if self.stream_driver(stream_id).is_none() {
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
            ops: Vec::new(),
            session_id,
            parent_session_id: skim.parent_session_id.clone(),
            // The grammar exposes an immediate spawning parent, but no
            // distinct root id. Until it does, that parent is the best native
            // root fact available for a detached child.
            root_session_id: skim.parent_session_id,
            role: Some(role),
            cwd: skim.cwd,
            agent_version: skim.agent_version,
            title: None,
            started_at: skim.started_at,
        }
    }

    fn ingest_line(&self, ctx: &mut NormalizeCtx, stream_id: &str, line: &[u8]) -> LineOutcome {
        let mut state = take_state(ctx);
        let outcome = normalize::ingest_line(ctx, &mut state, stream_id, line);
        ctx.state = Some(state);
        outcome
    }
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
    (parts.next()? == "events.jsonl").then_some(())?;
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
    fn only_events_has_a_json_lines_driver() {
        assert_eq!(
            CopilotAdapter.stream_driver(spec::EVENTS),
            Some(StreamDriver::JsonLines)
        );
        for inventory in [
            "session-db",
            "session-plans",
            "session-checkpoints",
            "session-workspace",
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
}
