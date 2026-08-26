//! The one trait every agent implements — all agent knowledge stops at this
//! boundary (docs/ingest-engine.html, "The two contracts").
//!
//! `skim` is deliberately typed bytes-in/ops-out: the meta-phase promise ("no
//! transcript body is ever fully read before first paint") is enforced by the
//! signature — the adapter never sees a file handle, only the bounded head
//! and tail the pipeline chose to read.

use std::any::Any;

use rosetta_core::IngestOp;
use rosetta_core::fold::CumulativeUsage;
use rosetta_core::rows::Ms;

use crate::GroundTruthDoc;

/// A stream parser the generic pipeline knows how to frame. Layout inventory
/// is intentionally broader than this list: an adapter must opt a stream into
/// normalization instead of every matching file being treated as a transcript.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamDriver {
    JsonLines,
    /// Rewrite-in-place SQLite databases (opencode, hermes). The pipeline
    /// snapshots the db, runs the adapter's DECLARED queries, and feeds each
    /// result row through `ingest_line` as a `{"query": name, "row": {...}}`
    /// JSON record — adapters never see a database handle.
    SqliteSnapshot,
}

/// Declarative extraction plan for a [`StreamDriver::SqliteSnapshot`] stream.
/// All SQL is executed by the capture layer against a stable private copy.
#[derive(Debug, Clone)]
pub struct SnapshotPlan {
    /// One result row per native session. Recognized columns:
    /// `native_session_id` (required), `title`, `cwd`, `agent_version`,
    /// `started_at`, `updated_at` (ms epochs), `parent_native_session_id`,
    /// `root_native_session_id`, `role` (`main` | `subagent` | `team`).
    /// Order stably.
    pub sessions_sql: String,
    /// Per-session record queries; `?1` binds the native session id, and
    /// every result row becomes one record. Order stably — record order is
    /// presentation order.
    pub record_queries: Vec<SnapshotQuery>,
}

#[derive(Debug, Clone)]
pub struct SnapshotQuery {
    /// Lands as the record's `query` field so `ingest_line` can dispatch.
    pub name: String,
    pub sql: String,
}

/// What the pipeline knows about a source file without reading its body.
#[derive(Debug, Clone)]
pub struct FileStat {
    /// Path relative to the runtime's data root — matches layout globs.
    pub relative_path: String,
    pub size: u64,
    pub modified_ms: Ms,
}

/// Meta-phase output: ops derived from stat + bounded head/tail reads only.
///
/// Everything here is NATIVE fact — ids the agent minted, paths the agent
/// recorded. The pipeline owns rosetta id derivation and row construction;
/// an adapter never sees a runtime id at skim time, so it could not mint a
/// rosetta id if it tried.
#[derive(Debug, Default)]
pub struct SkimResult {
    pub ops: Vec<IngestOp>,
    /// The session this file belongs to, when the adapter can tell — lets the
    /// pipeline key its cursors and ctx before the stream phase.
    pub session_id: Option<String>,
    /// Native parent session id for a child stream (for example a Claude Code
    /// subagent transcript). The pipeline derives both canonical ids together.
    pub parent_session_id: Option<String>,
    /// Native root session id when it differs from the immediate parent.
    pub root_session_id: Option<String>,
    pub role: Option<rosetta_core::rows::SessionRole>,
    /// Workspace directory the session ran in — drives project assignment.
    pub cwd: Option<String>,
    pub agent_version: Option<String>,
    /// Session title/summary when the head or tail happens to carry one.
    pub title: Option<String>,
    pub started_at: Option<Ms>,
}

/// Outcome of ingesting one raw record in the stream phase.
#[derive(Debug)]
pub struct LineOutcome {
    pub ops: Vec<IngestOp>,
    pub status: LineStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineStatus {
    Parsed,
    /// Unknown top-level discriminant — soft, expected across releases.
    UnknownType,
    /// A KNOWN record kind failed to parse — the drift alarm. The adapter
    /// still emits an aggregated `IngestOp::Drift` in `ops`; this status
    /// tells the pipeline to count it against the stream's health.
    HardError {
        code: String,
    },
}

impl LineOutcome {
    pub fn parsed(ops: Vec<IngestOp>) -> Self {
        Self {
            ops,
            status: LineStatus::Parsed,
        }
    }
}

/// Per-file normalization context, created by the pipeline for each source
/// file (or live tail) and threaded through every `ingest_line` call.
pub struct NormalizeCtx {
    pub runtime_id: String,
    pub session_id: String,
    pub agent_version: Option<String>,
    /// Next presentation ordinal to assign; adapters take and increment.
    pub next_ordinal: i64,
    /// Cumulative→delta tracker for agents with running counters (codex).
    pub cumulative: CumulativeUsage,
    /// Provenance: the ingest_cursor rowid + byte offset of the record being
    /// ingested, stamped by the pipeline before each call.
    pub src_cursor: Option<i64>,
    pub src_offset: Option<i64>,
    /// Adapter-private state (e.g. pending tool names by id), created by
    /// [`AgentAdapter::init_state`] and downcast by the adapter.
    pub state: Option<Box<dyn Any + Send>>,
}

impl NormalizeCtx {
    pub fn new(runtime_id: impl Into<String>, session_id: impl Into<String>) -> Self {
        Self {
            runtime_id: runtime_id.into(),
            session_id: session_id.into(),
            agent_version: None,
            next_ordinal: 0,
            cumulative: CumulativeUsage::new(),
            src_cursor: None,
            src_offset: None,
            state: None,
        }
    }

    pub fn take_ordinal(&mut self) -> i64 {
        let ordinal = self.next_ordinal;
        self.next_ordinal += 1;
        ordinal
    }
}

/// One implementation per agent. Object-safe: the pipeline holds a registry
/// of `&'static dyn AgentAdapter`.
pub trait AgentAdapter: Send + Sync {
    /// Agent kind, e.g. "claude-code".
    fn kind(&self) -> &'static str;

    /// The embedded canonical layout (from the agent's L1 groundtruth.json);
    /// discovery probes data roots with it.
    fn layout(&self) -> &GroundTruthDoc;

    /// Explicit admission control for stream normalization. A layout describes
    /// everything an agent persists; only streams with a driver are transcript
    /// inputs. This is deliberately stream-id based because two JSONL files can
    /// have completely different grammars.
    fn stream_driver(&self, stream_id: &str) -> Option<StreamDriver>;

    /// Bump whenever normalized output or adapter checkpoint semantics change.
    /// A mismatch expires the cursor and starts a source-generation replay.
    fn normalizer_version(&self) -> u32;

    /// The extraction plan for a [`StreamDriver::SqliteSnapshot`] stream.
    /// Required (and only consulted) for streams whose driver is
    /// `SqliteSnapshot`.
    fn snapshot_plan(&self, _stream_id: &str) -> Option<SnapshotPlan> {
        None
    }

    /// Adapter-private per-file state for [`NormalizeCtx::state`].
    fn init_state(&self) -> Option<Box<dyn Any + Send>> {
        None
    }

    /// Serialize adapter-private state into the pipeline checkpoint. Stateful
    /// adapters must override both this and [`AgentAdapter::restore_state`].
    fn checkpoint_state(
        &self,
        state: Option<&(dyn Any + Send)>,
    ) -> Result<Option<serde_json::Value>, String> {
        match state {
            None => Ok(None),
            Some(_) => Err(format!(
                "{} has adapter state but no checkpoint serializer",
                self.kind()
            )),
        }
    }

    /// Restore adapter-private state. An invalid payload is an expired cursor,
    /// never permission to continue with a freshly initialized state halfway
    /// through a file.
    fn restore_state(
        &self,
        checkpoint: Option<&serde_json::Value>,
    ) -> Result<Option<Box<dyn Any + Send>>, String> {
        match checkpoint {
            None => Ok(self.init_state()),
            Some(_) => Err(format!(
                "{} has a checkpoint payload but no state deserializer",
                self.kind()
            )),
        }
    }

    /// Meta phase: bounded bytes in, session/project meta ops out.
    fn skim(&self, stream_id: &str, stat: &FileStat, head: &[u8], tail: &[u8]) -> SkimResult;

    /// Stream phase: one raw record in, canonical ops out. Combines the pure
    /// parse and normalize stages; each adapter keeps its `parse_line` public
    /// so the stages stay independently testable and benchable.
    fn ingest_line(&self, ctx: &mut NormalizeCtx, stream_id: &str, line: &[u8]) -> LineOutcome;
}
