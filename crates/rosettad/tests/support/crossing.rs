//! The cross-tree reference fixtures of R1 D21 (brief §10, v3).
//!
//! No registered adapter writes another tree's session: the tree harness
//! proves closure on every fixture and nested family, which is why Claude
//! Code and Codex may declare root-tree admission at all. A reference across
//! trees is the shape a future adapter could take, so it is injected here:
//! [`CROSSING`] is Claude Code's adapter, record for record, except that a
//! record mentioning `rosetta-cross:<native id>` also yields a plan row of
//! that other session — an op naming a session of another tree. A record
//! mentioning `rosetta-copy:<native id>` instead yields a message row of
//! that session, which the producer's preparer counts as crossing both
//! sessions: their tool facts are left to their tree's fold. The files
//! themselves are ordinary Claude Code transcripts.

use std::any::Any;
use std::path::Path;

use rosetta_adapter_claude_code::ClaudeCodeAdapter;
use rosetta_adapter_support::html_artifacts::CaptureHints;
use rosetta_adapter_support::{
    AgentAdapter, ColdAdmission, FileStat, GroundTruthDoc, LineOutcome, NormalizeCtx, SkimResult,
    SnapshotPlan, StreamDriver,
};
use rosetta_core::IngestOp;
use rosetta_core::html_artifacts::HtmlArtifactPublication;
use rosetta_core::ids;
use rosetta_core::rows::{MessageRow, PlanRow};

use super::fixtures::{ClaudeTranscript, T0, claude_root};

const MARK: &[u8] = b"rosetta-cross:";
const COPY: &[u8] = b"rosetta-copy:";
const HOUR: i64 = 3_600_000;

/// Claude Code's adapter with one injected cross-tree reference per marked
/// record.
pub struct CrossingAdapter;

pub static CROSSING: CrossingAdapter = CrossingAdapter;

/// The native session id a record marked with `mark` names.
fn marked(line: &[u8], mark: &[u8]) -> Option<String> {
    let start = line.windows(mark.len()).position(|window| window == mark)? + mark.len();
    let id: String = line[start..]
        .iter()
        .take_while(|byte| byte.is_ascii_alphanumeric() || **byte == b'-' || **byte == b'_')
        .map(|byte| char::from(*byte))
        .collect();
    (!id.is_empty()).then_some(id)
}

impl AgentAdapter for CrossingAdapter {
    fn kind(&self) -> &'static str {
        ClaudeCodeAdapter.kind()
    }

    fn layout(&self) -> &GroundTruthDoc {
        ClaudeCodeAdapter.layout()
    }

    fn stream_driver(&self, stream_id: &str) -> Option<StreamDriver> {
        ClaudeCodeAdapter.stream_driver(stream_id)
    }

    fn census_session_streams(&self) -> Option<&'static [&'static str]> {
        ClaudeCodeAdapter.census_session_streams()
    }

    fn cold_admission(&self) -> ColdAdmission {
        ClaudeCodeAdapter.cold_admission()
    }

    fn normalizer_version(&self) -> u32 {
        ClaudeCodeAdapter.normalizer_version()
    }

    fn html_artifact_hints(
        &self,
        ctx: &NormalizeCtx,
        publication: &HtmlArtifactPublication,
        data_root: &str,
    ) -> CaptureHints {
        ClaudeCodeAdapter.html_artifact_hints(ctx, publication, data_root)
    }

    fn html_artifact_record(&self, record: &[u8], path: &str) -> Option<Vec<u8>> {
        ClaudeCodeAdapter.html_artifact_record(record, path)
    }

    fn snapshot_plan(&self, stream_id: &str) -> Option<SnapshotPlan> {
        ClaudeCodeAdapter.snapshot_plan(stream_id)
    }

    fn snapshot_session(&self, stream_id: &str, row: &serde_json::Value) -> SkimResult {
        ClaudeCodeAdapter.snapshot_session(stream_id, row)
    }

    fn document_sessions(
        &self,
        stream_id: &str,
        stat: &FileStat,
        document: &[u8],
    ) -> Vec<SkimResult> {
        ClaudeCodeAdapter.document_sessions(stream_id, stat, document)
    }

    fn document_size_limit(&self, stream_id: &str, pipeline_limit: usize) -> usize {
        ClaudeCodeAdapter.document_size_limit(stream_id, pipeline_limit)
    }

    fn document_records(
        &self,
        stream_id: &str,
        native_session_id: &str,
        document: &[u8],
    ) -> Vec<Vec<u8>> {
        ClaudeCodeAdapter.document_records(stream_id, native_session_id, document)
    }

    fn init_state(&self) -> Option<Box<dyn Any + Send>> {
        ClaudeCodeAdapter.init_state()
    }

    fn checkpoint_state(
        &self,
        state: Option<&(dyn Any + Send)>,
    ) -> Result<Option<serde_json::Value>, String> {
        ClaudeCodeAdapter.checkpoint_state(state)
    }

    fn restore_state(
        &self,
        checkpoint: Option<&serde_json::Value>,
    ) -> Result<Option<Box<dyn Any + Send>>, String> {
        ClaudeCodeAdapter.restore_state(checkpoint)
    }

    fn skim(&self, stream_id: &str, stat: &FileStat, head: &[u8], tail: &[u8]) -> SkimResult {
        ClaudeCodeAdapter.skim(stream_id, stat, head, tail)
    }

    fn wants_skim_fallback(&self, stream_id: &str, skim: &SkimResult) -> bool {
        ClaudeCodeAdapter.wants_skim_fallback(stream_id, skim)
    }

    fn absorb_skim_fallback_record(
        &self,
        stream_id: &str,
        record: &[u8],
        skim: &mut SkimResult,
    ) -> bool {
        ClaudeCodeAdapter.absorb_skim_fallback_record(stream_id, record, skim)
    }

    fn ingest_line(&self, ctx: &mut NormalizeCtx, stream_id: &str, line: &[u8]) -> LineOutcome {
        let mut outcome = ClaudeCodeAdapter.ingest_line(ctx, stream_id, line);
        if let Some(native) = marked(line, COPY) {
            let session_id = ids::session_id(&ctx.runtime_id, &native);
            let row: MessageRow = serde_json::from_value(serde_json::json!({
                "timeline_id": ids::timeline_id(&session_id, &format!("copy-from-{}", ctx.session_id)),
                "session_id": session_id, "ordinal": 0, "kind": "agent", "ts": T0,
                "content": "copied across sessions", "attachments": [], "citations": [],
            }))
            .unwrap();
            outcome.ops.extend(IngestOp::project_message(row));
        }
        if let Some(native) = marked(line, MARK) {
            outcome.ops.push(IngestOp::Plan(PlanRow {
                session_id: ids::session_id(&ctx.runtime_id, &native),
                revision: 0,
                content: format!("named across trees by {}", ctx.session_id),
                updated_at: T0,
            }));
        }
        outcome
    }
}

/// Write main session `id` in its own project, `turns` turns of about
/// `bytes` each, its first prompt naming `names` when set.
fn session(home: &Path, id: &str, names: Option<&str>, turns: usize, bytes: usize, hours: i64) {
    let mut transcript = ClaudeTranscript::new(id, None, T0);
    match names {
        Some(target) => transcript.prompt(&format!("see rosetta-cross:{target} for context")),
        None => transcript.prompt("start"),
    };
    transcript.reply("ok").bulk(turns, bytes);
    transcript.write(
        &claude_root(home).join(format!("projects/-w-{id}/{id}.jsonl")),
        T0 + hours * HOUR,
    );
}

/// The newest tree `big` walks the big lane (split the walk at 64 KiB);
/// `small`, in lane S, names `big`'s session: a reference across lanes,
/// which fails closed whatever the schedule. `other` keeps lane S busy.
pub fn cross_lane(home: &Path) {
    session(home, "big", None, 96, 4096, 10);
    session(home, "small", Some("big"), 1, 256, 5);
    session(home, "other", None, 2, 256, 1);
}

/// In lane S, `newer` names `older`, whose tree the lane reaches later: a
/// forward reference in one lane binds both trees, which the runtime's close
/// admits. `big` (64 KiB split) keeps a second lane walking beside them.
pub fn same_lane(home: &Path) {
    session(home, "big", None, 96, 4096, 10);
    session(home, "newer", Some("older"), 1, 256, 5);
    session(home, "middle", None, 2, 256, 3);
    session(home, "older", None, 2, 256, 1);
}

/// The split both crossing fixtures walk with.
pub const CROSSING_SPLIT: u64 = 64 * 1024;

/// Claude nested family (see `fixtures::claude_nested`) whose main
/// transcript `m` also writes a row of its subagent `a`: both sessions are
/// crossed, so their tool rows are written by the fold of their tree's flip.
pub fn claude_crossed(home: &Path) {
    super::fixtures::claude_nested(home, 16 * 1024);
    let main = claude_root(home).join("projects/-w-project/m.jsonl");
    let mut transcript = ClaudeTranscript::new("m", None, T0);
    transcript
        .prompt("start the work, rosetta-copy:a")
        .reply("delegating")
        .tool("ls", "a b c");
    transcript.write(&main, T0 + 10 * HOUR);
}

/// `fixtures::claude_fork` whose origin also writes a row of its own
/// subagent `o` (`rosetta-copy:o`): origin and `o` are crossed, so the
/// origin's row usage — the copied response's owner — waits for its tree's
/// flip fold, after the fork's tree has flipped. The origin's copy carries
/// no iterations, so no immediate usage sample claims the response first;
/// the fork stays uncrossed, its copy (row and iteration samples) written by
/// its own source's transactions.
pub fn fork_crossed(home: &Path) {
    use super::fixtures::{claude_meta, claude_usage};
    super::fixtures::claude_fork(home);
    let project = claude_root(home).join("projects/-w-fork");
    let mut origin = ClaudeTranscript::new("origin", None, T0);
    origin
        .prompt("plan the change")
        .reply_as(
            "msg_fork_1",
            "req_fork_1",
            "here is the plan",
            claude_usage(300, 40, serde_json::json!([])),
        )
        .tool("git status", "clean")
        .prompt("go on, see rosetta-copy:o")
        .reply("going on");
    origin.write(&project.join("origin.jsonl"), T0 + 2 * HOUR);
    let subagents = project.join("origin/subagents");
    let mut delegated = ClaudeTranscript::new("origin", Some("o"), T0 + 60_000);
    delegated
        .prompt("delegated work")
        .reply("done")
        .tool("ls", "a b");
    delegated.write(&subagents.join("agent-o.jsonl"), T0 + 2 * HOUR + 1);
    let meta = subagents.join("agent-o.meta.json");
    claude_meta(&meta, None);
    // The tree's recency is its newest source's: the fork's stays newer.
    super::fixtures::set_mtime(&meta, T0 + 2 * HOUR + 2);
}

/// A crossed tree walking the big lane while lane S has work. The newest
/// tree's main transcript `m` writes a row of its subagent `a`
/// (`rosetta-copy:a`), so both are crossed and `a`'s own later source —
/// about `giant` bytes, over the lane split and the chunk bound — is not
/// withheld: its Stage slices stage fold deltas. `small` older single-session
/// trees keep lane S emitting while the writer steps it.
pub fn crossed_big_lane(home: &Path, giant: usize, small: usize) {
    use super::fixtures::claude_meta;
    let project = claude_root(home).join("projects/-w-crossed");
    let mut main = ClaudeTranscript::new("m", None, T0);
    main.prompt("start the work, rosetta-copy:a")
        .reply("delegating")
        .tool("ls", "a b c");
    main.write(&project.join("m.jsonl"), T0 + 100 * HOUR);
    let subagents = project.join("m/subagents");
    let mut a = ClaudeTranscript::new("m", Some("a"), T0 + 60_000);
    a.prompt("delegated work")
        .bulk(giant / 4096, 2048)
        .tools(giant / 4096, 1024);
    a.write(&subagents.join("agent-a.jsonl"), T0 + 100 * HOUR + 1);
    let meta = subagents.join("agent-a.meta.json");
    claude_meta(&meta, None);
    super::fixtures::set_mtime(&meta, T0 + 100 * HOUR + 2);
    for index in 0..small {
        let id = format!("s{index:02}");
        let mut transcript = ClaudeTranscript::new(&id, None, T0);
        transcript
            .prompt("a small question")
            .bulk(2, 512)
            .tool("pwd", "/w/project");
        transcript.write(
            &claude_root(home).join(format!("projects/-w-{id}/{id}.jsonl")),
            T0 + (50 - index as i64) * HOUR,
        );
    }
}
