//! The `AgentAdapter` implementation — all Pi-specific knowledge stops here.

use std::any::Any;

use rosetta_adapter_support::GroundTruthDoc;
use rosetta_adapter_support::adapter::{
    AgentAdapter, FileStat, LineOutcome, NormalizeCtx, SkimResult, StreamDriver,
};
use rosetta_core::rows::SessionRole;
use serde_json::Value;

use crate::normalize::{self, PiState};
use crate::raw::session::KnownRecord;
use crate::raw::{ParsedLine, parse_line};
use crate::spec;
use crate::time::rfc3339_ms;

pub struct PiAdapter;

impl AgentAdapter for PiAdapter {
    fn kind(&self) -> &'static str {
        "pi"
    }

    fn layout(&self) -> &GroundTruthDoc {
        spec::doc()
    }

    fn stream_driver(&self, stream_id: &str) -> Option<StreamDriver> {
        (stream_id == spec::SESSION).then_some(StreamDriver::JsonLines)
    }

    fn normalizer_version(&self) -> u32 {
        1
    }

    fn init_state(&self) -> Option<Box<dyn Any + Send>> {
        Some(Box::new(PiState::default()))
    }

    fn checkpoint_state(&self, state: Option<&(dyn Any + Send)>) -> Result<Option<Value>, String> {
        let state = state
            .and_then(|state| state.downcast_ref::<PiState>())
            .ok_or_else(|| "Pi checkpoint state has the wrong type".to_owned())?;
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
            Some(value) => serde_json::from_value::<PiState>(value.clone())
                .map(|state| Some(Box::new(state) as Box<dyn Any + Send>))
                .map_err(|error| error.to_string()),
        }
    }

    /// Read native session facts from bounded windows.
    ///
    /// The header id is authoritative. If the head cuts that first line, the
    /// `<timestamp>_<id>.jsonl` path supplies the same native id while all
    /// other header-only facts remain unknown. `parentSession` is a native
    /// session path, so only its final native id is retained.
    fn skim(&self, stream_id: &str, stat: &FileStat, head: &[u8], tail: &[u8]) -> SkimResult {
        if self.stream_driver(stream_id).is_none() {
            return SkimResult::default();
        }

        let whole_head = head.len() as u64 >= stat.size;
        let header = complete_head_lines(head, whole_head)
            .next()
            .and_then(parse_known)
            .and_then(|record| match record {
                KnownRecord::Session(header) => Some(header),
                _ => None,
            });

        let mut title = complete_head_lines(head, whole_head)
            .filter_map(session_title)
            .last();
        let whole_tail = tail.len() as u64 >= stat.size;
        if let Some(latest) = complete_tail_lines(tail, whole_tail).find_map(session_title) {
            title = Some(latest);
        }

        let path_id = native_id_from_session_path(&stat.relative_path).map(str::to_owned);
        let Some(header) = header else {
            return SkimResult {
                session_id: path_id,
                role: Some(SessionRole::Main),
                title,
                ..SkimResult::default()
            };
        };

        let parent = header
            .parent_session
            .as_deref()
            .and_then(native_id_from_session_path)
            .map(str::to_owned);
        SkimResult {
            ops: Vec::new(),
            session_id: Some(header.id),
            parent_session_id: parent.clone(),
            root_session_id: parent,
            role: Some(SessionRole::Main),
            cwd: (!header.cwd.is_empty()).then_some(header.cwd),
            // `version` is Pi's SESSION FORMAT version, not the agent release.
            agent_version: None,
            title,
            started_at: rfc3339_ms(&header.timestamp),
        }
    }

    fn ingest_line(&self, ctx: &mut NormalizeCtx, stream_id: &str, line: &[u8]) -> LineOutcome {
        let mut state = match ctx.state.take() {
            Some(state) => state.downcast::<PiState>().unwrap_or_default(),
            None => Box::default(),
        };
        let outcome = normalize::ingest_line(ctx, &mut state, stream_id, line);
        ctx.state = Some(state);
        outcome
    }
}

fn parse_known(line: &[u8]) -> Option<KnownRecord> {
    let text = std::str::from_utf8(line).ok()?;
    match parse_line(text).ok()? {
        ParsedLine::Known(record) => Some(*record),
        ParsedLine::UnknownType { .. } => None,
    }
}

fn session_title(line: &[u8]) -> Option<String> {
    match parse_known(line)? {
        KnownRecord::SessionInfo(info) => info.name.filter(|name| !name.is_empty()),
        _ => None,
    }
}

/// Head starts at byte zero; omit only its possibly cut final fragment.
fn complete_head_lines(head: &[u8], whole_file: bool) -> impl Iterator<Item = &[u8]> {
    let complete_len = if whole_file {
        head.len()
    } else {
        head.iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(0, |at| at + 1)
    };
    head[..complete_len]
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
}

/// Tail ends at EOF; omit its first fragment unless it is the whole file, then
/// walk newest-first so a renamed session reports its latest native title.
fn complete_tail_lines(tail: &[u8], whole_file: bool) -> impl Iterator<Item = &[u8]> {
    tail.split(|byte| *byte == b'\n')
        .skip(usize::from(!whole_file))
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
}

/// Extract the native id from Pi's `<timestamp>_<id>.jsonl` session path.
pub(crate) fn native_id_from_session_path(path: &str) -> Option<&str> {
    let file = path.rsplit(['/', '\\']).next()?;
    let stem = file.strip_suffix(".jsonl").unwrap_or(file);
    let (_, id) = stem.rsplit_once('_')?;
    (!id.is_empty()).then_some(id)
}

#[cfg(test)]
mod tests {
    use rosetta_adapter_support::adapter::{AgentAdapter, FileStat, SkimResult};
    use rosetta_core::rows::SessionRole;

    use super::PiAdapter;
    use crate::spec;

    const PATH: &str = "agent/sessions/--work--/2026-08-25T05-29-06-156Z_path-id.jsonl";

    fn stat(size: usize) -> FileStat {
        FileStat {
            relative_path: PATH.to_owned(),
            size: size as u64,
            modified_ms: 0,
        }
    }

    fn header(id: &str) -> String {
        format!(
            r#"{{"type":"session","version":3,"id":"{id}","timestamp":"2026-08-25T05:29:06.156Z","cwd":"<PROJECT>"}}"#
        )
    }

    fn assert_empty(result: &SkimResult) {
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
    fn header_id_is_authoritative_over_the_path() {
        let text = format!("{}\n", header("header-id"));
        let skim = PiAdapter.skim(spec::SESSION, &stat(text.len()), text.as_bytes(), b"");
        assert_eq!(skim.session_id.as_deref(), Some("header-id"));
        assert_eq!(skim.cwd.as_deref(), Some("<PROJECT>"));
        assert_eq!(skim.started_at, Some(1_787_635_746_156));
        assert_eq!(skim.role, Some(SessionRole::Main));
        assert_eq!(skim.agent_version, None);
    }

    #[test]
    fn cut_header_falls_back_only_to_the_path_identity() {
        let skim = PiAdapter.skim(spec::SESSION, &stat(4096), b"{\"type\":\"sess", b"");
        assert_eq!(skim.session_id.as_deref(), Some("path-id"));
        assert_eq!(skim.role, Some(SessionRole::Main));
        assert_eq!(skim.cwd, None);
        assert_eq!(skim.started_at, None);
    }

    #[test]
    fn whole_file_header_needs_no_trailing_newline() {
        let text = header("whole-id");
        let skim = PiAdapter.skim(spec::SESSION, &stat(text.len()), text.as_bytes(), b"");
        assert_eq!(skim.session_id.as_deref(), Some("whole-id"));
    }

    #[test]
    fn bounded_tail_can_supply_the_latest_title() {
        let head = format!("{}\n{{\"type\":\"message\"", header("session-id"));
        let tail = br#"cut-fragment"}
{"type":"session_info","id":"one","parentId":null,"timestamp":"2026-08-25T05:29:07Z","name":"older"}
{"type":"session_info","id":"two","parentId":"one","timestamp":"2026-08-25T05:29:08Z","name":"latest"}
"#;
        let skim = PiAdapter.skim(spec::SESSION, &stat(8192), head.as_bytes(), tail);
        assert_eq!(skim.session_id.as_deref(), Some("session-id"));
        assert_eq!(skim.title.as_deref(), Some("latest"));
    }

    #[test]
    fn minimal_empty_windows_still_use_the_native_path_rule() {
        let skim = PiAdapter.skim(spec::SESSION, &stat(1), b"", b"");
        assert_eq!(skim.session_id.as_deref(), Some("path-id"));
        assert_eq!(skim.cwd, None);
        assert_eq!(skim.title, None);
    }

    #[test]
    fn parent_session_path_becomes_native_parent_identity() {
        let text = r#"{"type":"session","version":3,"id":"child","timestamp":"2026-08-25T05:29:06.156Z","cwd":"<PROJECT>","parentSession":"<ROOT>/agent/sessions/--work--/2026-08-25T05-20-00Z_parent-id.jsonl"}"#;
        let skim = PiAdapter.skim(spec::SESSION, &stat(text.len()), text.as_bytes(), b"");
        assert_eq!(skim.parent_session_id.as_deref(), Some("parent-id"));
        assert_eq!(skim.root_session_id.as_deref(), Some("parent-id"));
    }

    #[test]
    fn inventory_streams_are_not_skimmed() {
        let text = header("header-id");
        let skim = PiAdapter.skim("settings", &stat(text.len()), text.as_bytes(), b"");
        assert_empty(&skim);
    }
}
