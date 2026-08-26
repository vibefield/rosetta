//! The `AgentAdapter` implementation — where all Claude Code knowledge stops
//! (docs/ingest-engine.html, "The two contracts").

use std::any::Any;

use rosetta_adapter_support::{
    AgentAdapter, FileStat, GroundTruthDoc, LineOutcome, NormalizeCtx, SkimResult, StreamDriver,
};
use rosetta_core::rows::{Ms, SessionRole};
use serde_json::Value;

use crate::normalize::{self, ClaudeCodeState};
use crate::spec;
use crate::time::rfc3339_ms;

pub struct ClaudeCodeAdapter;

impl AgentAdapter for ClaudeCodeAdapter {
    fn kind(&self) -> &'static str {
        "claude-code"
    }

    fn layout(&self) -> &GroundTruthDoc {
        spec::doc()
    }

    fn stream_driver(&self, stream_id: &str) -> Option<StreamDriver> {
        matches!(stream_id, "transcript" | "subagent-transcript").then_some(StreamDriver::JsonLines)
    }

    fn normalizer_version(&self) -> u32 {
        1
    }

    fn init_state(&self) -> Option<Box<dyn Any + Send>> {
        Some(Box::new(ClaudeCodeState::default()))
    }

    fn checkpoint_state(&self, state: Option<&(dyn Any + Send)>) -> Result<Option<Value>, String> {
        let state = state
            .and_then(|state| state.downcast_ref::<ClaudeCodeState>())
            .ok_or_else(|| "claude-code checkpoint state has the wrong type".to_string())?;
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
            Some(value) => serde_json::from_value::<ClaudeCodeState>(value.clone())
                .map(|state| Some(Box::new(state) as Box<dyn Any + Send>))
                .map_err(|error| error.to_string()),
        }
    }

    /// Meta phase. Reads the native session facts out of the bounded head/tail
    /// and emits NO ops — deliberately.
    ///
    /// A `SessionRow` needs the rosetta session id, which is
    /// `ids::session_id(runtime_id, native_id)`; `skim` is handed no runtime id
    /// and cannot invent one. Deriving a session id from a placeholder would
    /// mint rows the stream phase can never find again, so the adapter reports
    /// the NATIVE id and the native scalars beside it and leaves id derivation
    /// to the pipeline, which owns the runtime. This is a gap in the trait, not
    /// a shortcut here — see the crate docs.
    fn skim(&self, stream_id: &str, stat: &FileStat, head: &[u8], tail: &[u8]) -> SkimResult {
        if self.stream_driver(stream_id).is_none() {
            return SkimResult::default();
        }
        let mut skim = Skim::default();
        // The head begins at byte 0, so its first stamped record is the one
        // that OPENED the session — the only honest source for `started_at`.
        // The scan walks on past it because a transcript often opens with
        // session-scoped markers (atis-latch, last-prompt) that carry the
        // session id and nothing else.
        for record in records(head_lines(head)) {
            if skim.started_at.is_none() {
                skim.started_at = stamp(&record);
            }
            skim.absorb(&record);
            if skim.is_complete() {
                break;
            }
        }
        // The tail answers for whatever the head could not: a window shorter
        // than one record, and the model-written title, which lands at the end
        // of the file. It cannot date the session — a late record's stamp says
        // when the transcript was last written, which the file's mtime already
        // tells the pipeline.
        for record in records(last_lines(tail)) {
            skim.absorb(&record);
            if skim.is_complete() {
                break;
            }
        }
        let (session_id, parent_session_id, root_session_id, role) = match stream_id {
            "transcript" => (
                skim.session_id
                    .or_else(|| file_stem(&stat.relative_path).map(str::to_owned)),
                None,
                None,
                Some(SessionRole::Main),
            ),
            "subagent-transcript" => {
                let parent = skim
                    .session_id
                    .or_else(|| parent_from_path(&stat.relative_path).map(str::to_owned));
                (
                    skim.agent_id
                        .or_else(|| subagent_file_stem(&stat.relative_path).map(str::to_owned)),
                    parent.clone(),
                    parent,
                    Some(SessionRole::Subagent),
                )
            }
            _ => unreachable!("unsupported streams returned above"),
        };
        SkimResult {
            ops: Vec::new(),
            session_id,
            parent_session_id,
            root_session_id,
            role,
            cwd: skim.cwd,
            agent_version: skim.agent_version,
            title: skim.title,
            started_at: skim.started_at,
        }
    }

    fn ingest_line(&self, ctx: &mut NormalizeCtx, stream_id: &str, line: &[u8]) -> LineOutcome {
        normalize::ingest(ctx, stream_id, line)
    }
}

/// The native facts a bounded window can answer for, first value winning: the
/// head is walked oldest-first (the session's opening record), the tail
/// newest-first.
#[derive(Default)]
struct Skim {
    session_id: Option<String>,
    agent_id: Option<String>,
    cwd: Option<String>,
    agent_version: Option<String>,
    title: Option<String>,
    started_at: Option<Ms>,
}

impl Skim {
    /// Take the session-wide facts a record carries. Every timeline record
    /// repeats `sessionId`, `cwd` and the writing CLI's `version`, so any
    /// record answers for them; `aiTitle` rides its own session-level record,
    /// which the CLI rewrites in place as the model renames the session.
    fn absorb(&mut self, record: &Value) {
        fill(&mut self.session_id, record["sessionId"].as_str());
        fill(&mut self.agent_id, record["agentId"].as_str());
        fill(&mut self.cwd, record["cwd"].as_str());
        fill(&mut self.agent_version, record["version"].as_str());
        fill(&mut self.title, record["aiTitle"].as_str());
    }

    fn is_complete(&self) -> bool {
        self.session_id.is_some()
            && self.cwd.is_some()
            && self.agent_version.is_some()
            && self.title.is_some()
            && self.started_at.is_some()
    }
}

/// Keep the first answer, and treat an empty string as no answer — a blank cwd
/// would assign the session to a project that does not exist.
fn fill(slot: &mut Option<String>, value: Option<&str>) {
    if slot.is_none() {
        *slot = value.filter(|text| !text.is_empty()).map(str::to_owned);
    }
}

/// The record's own ISO-8601 stamp as `Ms`. The session-scoped marker records
/// (atis-latch, ai-title, mode) carry none.
fn stamp(record: &Value) -> Option<Ms> {
    record["timestamp"].as_str().and_then(rfc3339_ms)
}

/// The lines that parse as JSON. A fragment the window cut simply fails to
/// parse, which is what keeps a clipped window from yielding half a fact.
fn records<'a>(lines: impl Iterator<Item = &'a [u8]>) -> impl Iterator<Item = Value> {
    lines.filter_map(|line| serde_json::from_slice(line).ok())
}

/// The head window starts at byte 0, so its lines are whole — all but the last,
/// which the window may have cut mid-record.
fn head_lines(head: &[u8]) -> impl Iterator<Item = &[u8]> {
    head.split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
}

/// The tail window starts mid-file, so its FIRST fragment is a cut record and is
/// dropped. The rest are whole at the front, newest first; the newest may still
/// be a half-flushed live write, hence "lines" plural — the caller takes the
/// newest one that actually parses.
fn last_lines(tail: &[u8]) -> impl Iterator<Item = &[u8]> {
    tail.split(|byte| *byte == b'\n')
        .skip(1)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
}

/// `projects/<slug>/<session-uuid>.jsonl` → `<session-uuid>`. The session id a
/// record carries is authoritative; the name answers only when neither window
/// held one whole record.
fn file_stem(relative_path: &str) -> Option<&str> {
    let name = relative_path.rsplit('/').next()?;
    let stem = name.strip_suffix(".jsonl").unwrap_or(name);
    (!stem.is_empty()).then_some(stem)
}

fn subagent_file_stem(relative_path: &str) -> Option<&str> {
    file_stem(relative_path)?.strip_prefix("agent-")
}

fn parent_from_path(relative_path: &str) -> Option<&str> {
    let mut parts = relative_path.split('/');
    (parts.next()? == "projects").then_some(())?;
    let _project = parts.next()?;
    let session = parts.next()?;
    (parts.next()? == "subagents").then_some(session)
}

#[cfg(test)]
mod tests {
    use super::{ClaudeCodeAdapter, FileStat};
    use rosetta_adapter_support::AgentAdapter;
    use rosetta_core::rows::SessionRole;

    fn stat(relative_path: &str) -> FileStat {
        FileStat {
            relative_path: relative_path.to_owned(),
            size: 0,
            modified_ms: 0,
        }
    }

    // A transcript opens the way the fixtures do: a session-scoped marker that
    // carries only the session id, then the first stamped timeline record.
    const HEAD: &[u8] = br#"{"type":"atis-latch","atis":"x","sessionId":"from-record"}
{"type":"user","sessionId":"from-record","cwd":"/w/project","version":"2.1.241","gitBranch":"main","timestamp":"2026-08-25T02:35:19.072Z"}"#;

    #[test]
    fn skim_prefers_the_records_own_session_id() {
        let result =
            ClaudeCodeAdapter.skim("transcript", &stat("projects/p/from-name.jsonl"), HEAD, b"");
        assert_eq!(result.session_id.as_deref(), Some("from-record"));
        // Nothing else: a SessionRow would need a runtime id skim never sees.
        assert!(result.ops.is_empty());
    }

    #[test]
    fn skim_walks_past_markers_to_the_first_stamped_record() {
        let result =
            ClaudeCodeAdapter.skim("transcript", &stat("projects/p/from-name.jsonl"), HEAD, b"");
        assert_eq!(result.cwd.as_deref(), Some("/w/project"));
        assert_eq!(result.agent_version.as_deref(), Some("2.1.241"));
        assert_eq!(result.started_at, Some(1_787_625_319_072));
        // No ai-title record in either window.
        assert_eq!(result.title, None);
    }

    #[test]
    fn skim_reads_the_tail_when_the_head_line_is_cut() {
        // Head window ends mid-record; tail window starts mid-record.
        let head = br#"{"type":"user","sessi"#;
        let tail = br#"ionId":"cut"}
{"type":"user","sessionId":"from-tail","cwd":"/w/project","version":"2.1.241","timestamp":"2026-08-25T02:35:19.072Z"}
{"type":"mode","mode":"auto","sessionId":"from-tail"}
"#;
        let result = ClaudeCodeAdapter.skim(
            "transcript",
            &stat("projects/p/from-name.jsonl"),
            head,
            tail,
        );
        assert_eq!(result.session_id.as_deref(), Some("from-tail"));
        assert_eq!(result.cwd.as_deref(), Some("/w/project"));
        assert_eq!(result.agent_version.as_deref(), Some("2.1.241"));
        // The tail dates the last write, not the session's start, so it never
        // fills started_at.
        assert_eq!(result.started_at, None);
    }

    #[test]
    fn skim_takes_the_title_from_the_ai_title_record() {
        let tail = br#"cut-fragment"}
{"type":"ai-title","aiTitle":"Skim the head window","sessionId":"from-tail"}
{"type":"mode","mode":"auto","sessionId":"from-tail"}
"#;
        let result = ClaudeCodeAdapter.skim(
            "transcript",
            &stat("projects/p/from-name.jsonl"),
            HEAD,
            tail,
        );
        assert_eq!(result.title.as_deref(), Some("Skim the head window"));
        // The head still owns the facts it answered for.
        assert_eq!(result.session_id.as_deref(), Some("from-record"));
        assert_eq!(result.started_at, Some(1_787_625_319_072));
    }

    #[test]
    fn skim_falls_back_to_the_file_name() {
        let result =
            ClaudeCodeAdapter.skim("transcript", &stat("projects/p/from-name.jsonl"), b"", b"");
        assert_eq!(result.session_id.as_deref(), Some("from-name"));
        // Empty windows answer for nothing else.
        assert_eq!(result.cwd, None);
        assert_eq!(result.agent_version, None);
        assert_eq!(result.title, None);
        assert_eq!(result.started_at, None);
    }

    #[test]
    fn skim_ignores_blank_and_unstamped_values() {
        // An empty cwd would assign the session to a project that is not there;
        // an unparseable stamp leaves the start time unknown.
        let head =
            br#"{"type":"user","sessionId":"s","cwd":"","version":"","timestamp":"not-a-date"}"#;
        let result =
            ClaudeCodeAdapter.skim("transcript", &stat("projects/p/from-name.jsonl"), head, b"");
        assert_eq!(result.session_id.as_deref(), Some("s"));
        assert_eq!(result.cwd, None);
        assert_eq!(result.agent_version, None);
        assert_eq!(result.started_at, None);
    }

    #[test]
    fn subagent_skim_uses_agent_identity_and_links_the_parent() {
        let head = br#"{"type":"assistant","agentId":"child-7","sessionId":"parent-1","cwd":"/w/project","version":"2.1.241","timestamp":"2026-08-25T02:35:19.072Z"}"#;
        let result = ClaudeCodeAdapter.skim(
            "subagent-transcript",
            &stat("projects/p/parent-1/subagents/task/agent-child-7.jsonl"),
            head,
            b"",
        );
        assert_eq!(result.session_id.as_deref(), Some("child-7"));
        assert_eq!(result.parent_session_id.as_deref(), Some("parent-1"));
        assert_eq!(result.root_session_id.as_deref(), Some("parent-1"));
        assert_eq!(result.role, Some(SessionRole::Subagent));
    }

    #[test]
    fn inventory_json_never_falls_back_to_a_fake_session() {
        let result =
            ClaudeCodeAdapter.skim("state", &stat(".claude.json"), br#"{"projects":{}}"#, b"");
        assert!(result.session_id.is_none());
    }

    #[test]
    fn layout_matches_transcript_paths() {
        let layout = ClaudeCodeAdapter.layout();
        let stream = layout.matching_stream("projects/-tmp-x/abc.jsonl");
        assert_eq!(stream.map(|s| s.id.as_str()), Some("transcript"));
    }
}
