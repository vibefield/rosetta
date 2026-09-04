//! The `AgentAdapter` implementation — where all Claude Code knowledge stops
//! (docs/ingest-engine.html, "The two contracts").

use std::any::Any;

use rosetta_adapter_support::{
    AgentAdapter, FileStat, GroundTruthDoc, LineOutcome, NormalizeCtx, ProjectHint, SkimResult,
    StreamDriver,
};
use rosetta_core::rows::{Ms, SessionDelegation, SessionRole, TitleKind};
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
        match stream_id {
            spec::TRANSCRIPT | spec::SUBAGENT_TRANSCRIPT | spec::WORKFLOW_JOURNAL => {
                Some(StreamDriver::JsonLines)
            }
            spec::SUBAGENT_METADATA | spec::WORKFLOW_RUN => Some(StreamDriver::DocumentSnapshot),
            _ => None,
        }
    }

    fn normalizer_version(&self) -> u32 {
        22
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
        if stream_id == spec::SUBAGENT_METADATA {
            return skim_subagent_metadata(stat, head);
        }
        if stream_id == spec::WORKFLOW_RUN {
            return crate::workflow::skim_run(stat, head);
        }
        if stream_id == spec::WORKFLOW_JOURNAL {
            return crate::workflow::skim_journal(stat);
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
        let (session_id, parent_session_id, root_session_id, role, delegation) = match stream_id {
            spec::TRANSCRIPT => (
                skim.session_id
                    .or_else(|| file_stem(&stat.relative_path).map(str::to_owned)),
                None,
                None,
                Some(SessionRole::Main),
                None,
            ),
            spec::SUBAGENT_TRANSCRIPT => {
                let parent = skim
                    .session_id
                    .or_else(|| parent_from_path(&stat.relative_path).map(str::to_owned));
                (
                    skim.agent_id
                        .or_else(|| subagent_file_stem(&stat.relative_path).map(str::to_owned)),
                    parent.clone(),
                    parent,
                    Some(SessionRole::Subagent),
                    crate::workflow::run_id_from_path(&stat.relative_path).map(|run_id| {
                        SessionDelegation {
                            parent_tool_native_id: None,
                            workflow_run_id: Some(run_id.to_owned()),
                            team: None,
                            color: None,
                            configured_model: None,
                        }
                    }),
                )
            }
            _ => unreachable!("unsupported streams returned above"),
        };
        let (title, title_kind) = match (skim.custom_title, skim.title) {
            (Some(title), _) => (Some(title), Some(TitleKind::Explicit)),
            (None, Some(title)) => (Some(title), Some(TitleKind::Generated)),
            (None, None) => (None, None),
        };
        SkimResult {
            ops: Vec::new(),
            session_id,
            parent_session_id,
            delegation,
            root_session_id,
            role,
            cwd: skim.cwd,
            project: None,
            agent_version: skim.agent_version,
            title,
            title_kind,
            summary: None,
            started_at: skim.started_at,
            updated_at: None,
        }
    }

    fn wants_skim_fallback(&self, stream_id: &str, skim: &SkimResult) -> bool {
        // Subagent transcripts do not own the parent session title. Main
        // transcripts occasionally place every title record just outside both
        // fixed windows, so only that missing optional fact earns extra IO.
        stream_id == spec::TRANSCRIPT && skim.title.is_none()
    }

    fn absorb_skim_fallback_record(
        &self,
        _stream_id: &str,
        record: &[u8],
        skim: &mut SkimResult,
    ) -> bool {
        let Ok(value) = serde_json::from_slice::<Value>(record) else {
            return false;
        };
        if let Some(title) = value["customTitle"]
            .as_str()
            .filter(|title| !title.is_empty())
        {
            // An explicit title is the highest-priority answer. Stop here;
            // later generated titles must never replace it.
            skim.title = Some(title.to_owned());
            skim.title_kind = Some(TitleKind::Explicit);
            return true;
        }
        if skim.title.is_none()
            && let Some(title) = value["aiTitle"].as_str().filter(|title| !title.is_empty())
        {
            // Keep scanning within the cap in case a custom title follows.
            skim.title = Some(title.to_owned());
            skim.title_kind = Some(TitleKind::Generated);
        }
        false
    }

    fn ingest_line(&self, ctx: &mut NormalizeCtx, stream_id: &str, line: &[u8]) -> LineOutcome {
        if stream_id == spec::SUBAGENT_METADATA {
            return LineOutcome::parsed(Vec::new());
        }
        match stream_id {
            spec::WORKFLOW_RUN => crate::workflow::ingest_run(ctx, line),
            spec::WORKFLOW_JOURNAL => crate::workflow::ingest_journal(ctx, line),
            _ => normalize::ingest(ctx, stream_id, line),
        }
    }

    fn document_size_limit(&self, stream_id: &str, pipeline_limit: usize) -> usize {
        if stream_id == spec::SUBAGENT_METADATA {
            pipeline_limit.min(64 * 1024)
        } else {
            pipeline_limit
        }
    }

    fn document_records(
        &self,
        stream_id: &str,
        _native_session_id: &str,
        document: &[u8],
    ) -> Vec<Vec<u8>> {
        match stream_id {
            spec::SUBAGENT_METADATA => Vec::new(),
            spec::WORKFLOW_RUN => vec![document.to_vec()],
            _ => Vec::new(),
        }
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
    custom_title: Option<String>,
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
        fill(&mut self.custom_title, record["customTitle"].as_str());
    }

    fn is_complete(&self) -> bool {
        self.session_id.is_some()
            && self.cwd.is_some()
            && self.agent_version.is_some()
            && (self.title.is_some() || self.custom_title.is_some())
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

fn subagent_metadata_stem(relative_path: &str) -> Option<&str> {
    relative_path
        .rsplit('/')
        .next()?
        .strip_prefix("agent-")?
        .strip_suffix(".meta.json")
        .filter(|id| !id.is_empty())
}

fn parent_from_path(relative_path: &str) -> Option<&str> {
    let mut parts = relative_path.split('/');
    (parts.next()? == "projects").then_some(())?;
    let _project = parts.next()?;
    let session = parts.next()?;
    (parts.next()? == "subagents").then_some(session)
}

/// Claude's worktree path is a lifecycle/isolation marker, not necessarily
/// the child's final shell cwd. Use it only to prove ownership by the parent;
/// the transcript remains authoritative for the child's observed cwd.
fn skim_subagent_metadata(stat: &FileStat, document: &[u8]) -> SkimResult {
    let Ok(text) = std::str::from_utf8(document) else {
        return SkimResult::default();
    };
    let Ok(meta) = crate::raw::parse_subagent_meta(text) else {
        return SkimResult::default();
    };
    let Some(child) = subagent_metadata_stem(&stat.relative_path).map(str::to_owned) else {
        return SkimResult::default();
    };
    let Some(root) = parent_from_path(&stat.relative_path).map(str::to_owned) else {
        return SkimResult::default();
    };
    let project = meta
        .worktree_path
        .as_deref()
        .filter(|path| absolute_native_path(path))
        .map(|_| ProjectHint::SameAsSession {
            native_session_id: root.clone(),
        });
    let role = if meta.task_kind.as_deref() == Some("in_process_teammate") {
        SessionRole::Team
    } else {
        SessionRole::Subagent
    };
    let title = meta
        .name
        .clone()
        .or_else(|| Some(meta.agent_type.clone()))
        .filter(|title| !title.trim().is_empty());
    let parent = meta
        .parent_agent_id
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| root.clone());
    let delegation = SessionDelegation {
        parent_tool_native_id: meta.tool_use_id.filter(|value| !value.trim().is_empty()),
        workflow_run_id: crate::workflow::run_id_from_path(&stat.relative_path).map(str::to_owned),
        team: meta.team_name.filter(|value| !value.trim().is_empty()),
        color: meta
            .color
            .as_deref()
            .and_then(crate::envelope::participant_color),
        configured_model: meta.model.filter(|value| !value.trim().is_empty()),
    };
    SkimResult {
        ops: Vec::new(),
        session_id: Some(child),
        parent_session_id: Some(parent),
        delegation: Some(delegation),
        root_session_id: Some(root),
        role: Some(role),
        cwd: None,
        project,
        agent_version: None,
        title,
        title_kind: Some(TitleKind::Generated),
        summary: meta.description,
        started_at: None,
        updated_at: None,
    }
}

fn absolute_native_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    path.starts_with('/')
        || path.starts_with("\\\\")
        || (bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && matches!(bytes[2], b'/' | b'\\'))
}

#[cfg(test)]
mod tests {
    use super::{ClaudeCodeAdapter, FileStat};
    use crate::spec;
    use rosetta_adapter_support::{AgentAdapter, ProjectHint, StreamDriver};
    use rosetta_core::ParticipantColor;
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
    fn skim_prefers_an_explicit_custom_title() {
        let tail = br#"cut-fragment"}
{"type":"ai-title","aiTitle":"Generated title","sessionId":"from-tail"}
{"type":"custom-title","customTitle":"My explicit title","sessionId":"from-tail"}
"#;
        let result = ClaudeCodeAdapter.skim(
            "transcript",
            &stat("projects/p/from-name.jsonl"),
            HEAD,
            tail,
        );
        assert_eq!(result.title.as_deref(), Some("My explicit title"));
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
        assert_eq!(result.delegation, None);
    }

    #[test]
    fn workflow_subagent_skim_preserves_its_owning_run() {
        let path = "projects/p/parent-1/subagents/workflows/wf_review/agent-child-7.jsonl";
        let result = ClaudeCodeAdapter.skim(
            "subagent-transcript",
            &stat(path),
            br#"{"type":"assistant","agentId":"child-7","sessionId":"parent-1"}"#,
            b"",
        );
        assert_eq!(
            result
                .delegation
                .as_ref()
                .and_then(|delegation| delegation.workflow_run_id.as_deref()),
            Some("wf_review")
        );

        let sidecar = ClaudeCodeAdapter.skim(
            spec::SUBAGENT_METADATA,
            &stat("projects/p/parent-1/subagents/workflows/wf_review/agent-child-7.meta.json"),
            br#"{"agentType":"workflow-subagent"}"#,
            b"",
        );
        assert_eq!(
            sidecar
                .delegation
                .as_ref()
                .and_then(|delegation| delegation.workflow_run_id.as_deref()),
            Some("wf_review")
        );
    }

    #[test]
    fn explicit_worktree_sidecar_joins_only_that_child_to_the_parent_project() {
        let result = ClaudeCodeAdapter.skim(
            spec::SUBAGENT_METADATA,
            &stat("projects/p/parent-1/subagents/agent-child-7.meta.json"),
            br#"{"agentType":"Explore","worktreePath":"/w/.claude/worktrees/agent-child-7"}"#,
            b"",
        );
        assert_eq!(result.session_id.as_deref(), Some("child-7"));
        assert_eq!(result.parent_session_id.as_deref(), Some("parent-1"));
        assert_eq!(
            result.project,
            Some(ProjectHint::SameAsSession {
                native_session_id: "parent-1".to_owned(),
            })
        );
        assert_eq!(result.cwd, None);
    }

    #[test]
    fn ordinary_subagent_sidecar_does_not_manufacture_worktree_ownership() {
        for document in [
            br#"{"agentType":"Explore"}"#.as_slice(),
            br#"{"agentType":"Explore","worktreePath":"done"}"#.as_slice(),
        ] {
            let result = ClaudeCodeAdapter.skim(
                spec::SUBAGENT_METADATA,
                &stat("projects/p/parent-1/subagents/agent-child-7.meta.json"),
                document,
                b"",
            );
            assert_eq!(result.session_id.as_deref(), Some("child-7"));
            assert_eq!(result.project, None);
        }
    }

    #[test]
    fn teammate_sidecar_promotes_the_child_and_keeps_its_native_identity() {
        let result = ClaudeCodeAdapter.skim(
            spec::SUBAGENT_METADATA,
            &stat("projects/p/parent-1/subagents/agent-member-7.meta.json"),
            br#"{"agentType":"general-purpose","taskKind":"in_process_teammate","teamName":"refinement","name":"w2-docs","description":"Land the docs","model":"opus","color":"green","parentAgentId":"worker-parent","toolUseId":"toolu-team-2"}"#,
            b"",
        );
        assert_eq!(result.role, Some(SessionRole::Team));
        assert_eq!(result.title.as_deref(), Some("w2-docs"));
        assert_eq!(
            result.title_kind,
            Some(rosetta_core::rows::TitleKind::Generated)
        );
        assert_eq!(result.summary.as_deref(), Some("Land the docs"));
        assert_eq!(result.parent_session_id.as_deref(), Some("worker-parent"));
        assert_eq!(result.root_session_id.as_deref(), Some("parent-1"));
        let delegation = result.delegation.expect("sidecar delegation evidence");
        assert_eq!(
            delegation.parent_tool_native_id.as_deref(),
            Some("toolu-team-2")
        );
        assert_eq!(delegation.workflow_run_id, None);
        assert_eq!(delegation.team.as_deref(), Some("refinement"));
        assert_eq!(delegation.color, Some(ParticipantColor::Green));
        assert_eq!(delegation.configured_model.as_deref(), Some("opus"));
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

    #[test]
    fn metadata_sidecars_are_bounded_document_sources() {
        assert_eq!(
            ClaudeCodeAdapter.stream_driver(spec::SUBAGENT_METADATA),
            Some(StreamDriver::DocumentSnapshot)
        );
        assert_eq!(
            ClaudeCodeAdapter.document_size_limit(spec::SUBAGENT_METADATA, usize::MAX),
            64 * 1024
        );
    }
}
