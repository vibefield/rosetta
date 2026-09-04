//! The one trait every agent implements — all agent knowledge stops at this
//! boundary (docs/ingest-engine.html, "The two contracts").
//!
//! `skim` is deliberately typed bytes-in/ops-out: the meta-phase promise ("no
//! transcript body is ever fully read before first paint") is enforced by the
//! signature — the adapter never sees a file handle, only the bounded head
//! and tail the pipeline chose to read.

use std::any::Any;

use rosetta_core::fold::CumulativeUsage;
use rosetta_core::rows::{Ms, SessionDelegation, SessionRole, TitleKind};
use rosetta_core::{EvidenceOrder, IngestOp, SourceIdentity};

use crate::GroundTruthDoc;

/// A stream parser the generic pipeline knows how to frame. Layout inventory
/// is intentionally broader than this list: an adapter must opt a stream into
/// normalization instead of every matching file being treated as a transcript.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamDriver {
    JsonLines,
    /// A rewrite-in-place JSON, Markdown, TOML, or text document. The
    /// pipeline reads one stable whole-file snapshot, asks the adapter which
    /// native sessions it contains, and gives each session a virtual source
    /// with ordinary generation/replay semantics. This is the metadata and
    /// artifact counterpart to `SqliteSnapshot`; it must never be used for an
    /// append-only transcript.
    DocumentSnapshot,
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
    /// `root_native_session_id`, `role` (`main` | `subagent` | `team`),
    /// `project_root`, `project_git_repo`, and
    /// `project_owner_native_session_id`. The owner alias takes precedence
    /// over a root when both are present.
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
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum ProjectHint {
    /// This session belongs to the local checkout group rooted at `root`.
    /// Ordinary clones report their own root; linked worktrees report the
    /// original/source checkout root. A remote is display metadata only.
    Root {
        root: String,
        git_repo: Option<String>,
    },
    /// This session belongs to the same project as another native session.
    /// The pipeline resolves the native id after every bounded metadata source
    /// has contributed, so adapters never mint canonical ids.
    SameAsSession { native_session_id: String },
}

#[derive(Debug, Default)]
pub struct SkimResult {
    pub ops: Vec<IngestOp>,
    /// The session this file belongs to, when the adapter can tell — lets the
    /// pipeline key its cursors and ctx before the stream phase.
    pub session_id: Option<String>,
    /// Native parent session id for a child stream (for example a Claude Code
    /// subagent transcript). The pipeline derives both canonical ids together.
    pub parent_session_id: Option<String>,
    /// Exact invocation/team facts from a native child sidecar.
    pub delegation: Option<SessionDelegation>,
    /// Native root session id when it differs from the immediate parent.
    pub root_session_id: Option<String>,
    pub role: Option<rosetta_core::rows::SessionRole>,
    /// One primary working directory proven by bounded native metadata. The
    /// pipeline normalizes it and settles project ownership before streaming.
    pub cwd: Option<String>,
    /// Optional native project ownership stronger than cwd alone. This is
    /// transient meta-ingest evidence; only the settled ProjectRow and
    /// SessionMeta enter the raw ledger and canonical projection.
    pub project: Option<ProjectHint>,
    pub agent_version: Option<String>,
    /// Session title/summary when the head or tail happens to carry one.
    pub title: Option<String>,
    /// Native confidence for `title`; absent whenever `title` is absent.
    pub title_kind: Option<TitleKind>,
    pub summary: Option<String>,
    pub started_at: Option<Ms>,
    pub updated_at: Option<Ms>,
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

    /// Convert the lossless normalizer draft into the intentionally bounded
    /// disposable projection. Raw-ledger inspection deliberately calls
    /// `ingest_line` without this step so it can recover the exact command.
    pub fn into_projection(mut self) -> Self {
        IngestOp::compact_command_previews(&mut self.ops);
        self
    }
}

/// Per-file normalization context, created by the pipeline for each source
/// file (or live tail) and threaded through every `ingest_line` call.
pub struct NormalizeCtx {
    pub runtime_id: String,
    pub session_id: String,
    /// The adapter's native identity for this session. Kept in process so a
    /// proven child id in a tool result can be converted to the canonical
    /// session id without persisting a second reconciliation vocabulary.
    pub native_session_id: Option<String>,
    pub agent_version: Option<String>,
    /// Next presentation ordinal to assign; adapters take and increment.
    pub next_ordinal: i64,
    /// Cumulative→delta tracker for agents with running counters (codex).
    pub cumulative: CumulativeUsage,
    /// Provenance: the ingest_cursor rowid + byte offset of the record being
    /// ingested, stamped by the pipeline before each call.
    pub src_cursor: Option<i64>,
    pub src_offset: Option<i64>,
    /// Stable raw-ledger identity used by deterministic typed merges.
    pub source_identity: Option<String>,
    pub source_generation: u64,
    /// Adapter-private state (e.g. pending tool names by id), created by
    /// [`AgentAdapter::init_state`] and downcast by the adapter.
    pub state: Option<Box<dyn Any + Send>>,
}

impl NormalizeCtx {
    pub fn new(runtime_id: impl Into<String>, session_id: impl Into<String>) -> Self {
        Self {
            runtime_id: runtime_id.into(),
            session_id: session_id.into(),
            native_session_id: None,
            agent_version: None,
            next_ordinal: 0,
            cumulative: CumulativeUsage::new(),
            src_cursor: None,
            src_offset: None,
            source_identity: None,
            source_generation: 0,
            state: None,
        }
    }

    pub fn take_ordinal(&mut self) -> i64 {
        let ordinal = self.next_ordinal;
        self.next_ordinal += 1;
        ordinal
    }

    pub fn evidence(&self, mutation_index: u32) -> EvidenceOrder {
        EvidenceOrder {
            source_identity: SourceIdentity(
                self.source_identity
                    .clone()
                    .unwrap_or_else(|| format!("direct:{}/{}", self.runtime_id, self.session_id)),
            ),
            generation: self.source_generation,
            record_offset: self
                .src_offset
                .and_then(|offset| u64::try_from(offset).ok())
                .unwrap_or(0),
            mutation_index,
        }
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

    /// Convert one `sessions_sql` result object into native session facts.
    /// The default consumes the standardized aliases documented on
    /// [`SnapshotPlan`]. Adapters whose stable compatibility query is
    /// deliberately `SELECT *` can override this to handle optional columns
    /// without interpolating schema-dependent SQL.
    fn snapshot_session(&self, _stream_id: &str, row: &serde_json::Value) -> SkimResult {
        let text = |key: &str| {
            row.get(key)
                .and_then(|value| value.as_str())
                .map(str::to_owned)
        };
        let title = text("title");
        let project = text("project_owner_native_session_id")
            .map(|native_session_id| ProjectHint::SameAsSession { native_session_id })
            .or_else(|| {
                text("project_root").map(|root| ProjectHint::Root {
                    root,
                    git_repo: text("project_git_repo"),
                })
            });
        SkimResult {
            ops: Vec::new(),
            session_id: text("native_session_id"),
            parent_session_id: text("parent_native_session_id"),
            delegation: None,
            root_session_id: text("root_native_session_id"),
            role: match text("role").as_deref() {
                Some("subagent") => Some(SessionRole::Subagent),
                Some("team") => Some(SessionRole::Team),
                Some("main") => Some(SessionRole::Main),
                _ => None,
            },
            cwd: text("cwd"),
            project,
            agent_version: text("agent_version"),
            title_kind: title.as_ref().map(|_| TitleKind::Explicit),
            title,
            summary: text("summary"),
            started_at: row.get("started_at").and_then(serde_json::Value::as_i64),
            updated_at: row.get("updated_at").and_then(serde_json::Value::as_i64),
        }
    }

    /// Enumerate the native sessions represented by a stable whole-document
    /// snapshot. Most sidecars describe exactly one session, so the default
    /// delegates to `skim` with the complete document as its head. Index files
    /// containing several sessions override this method.
    fn document_sessions(
        &self,
        stream_id: &str,
        stat: &FileStat,
        document: &[u8],
    ) -> Vec<SkimResult> {
        vec![self.skim(stream_id, stat, document, &[])]
    }

    /// Maximum stable document size this adapter is willing to inspect.
    /// The pipeline-wide record limit remains the default; small metadata
    /// formats can lower it without gaining file-system access themselves.
    fn document_size_limit(&self, _stream_id: &str, pipeline_limit: usize) -> usize {
        pipeline_limit
    }

    /// Frame a document snapshot for one enumerated native session. A
    /// one-session sidecar is one record by default. Multi-session indexes can
    /// return only the matching entry, and metadata-only documents may return
    /// no records after their meta facts have been journaled.
    fn document_records(
        &self,
        _stream_id: &str,
        _native_session_id: &str,
        document: &[u8],
    ) -> Vec<Vec<u8>> {
        vec![document.to_vec()]
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

    /// Whether the ordinary head/tail skim is missing an optional fact worth
    /// looking for in a bounded run of complete records immediately after the
    /// head window. The pipeline owns all extra IO and its limits; adapters
    /// only opt in after inspecting their already-bounded [`SkimResult`].
    fn wants_skim_fallback(&self, _stream_id: &str, _skim: &SkimResult) -> bool {
        false
    }

    /// Fold one complete record from the bounded fallback scan into `skim`.
    /// Return `true` when the best possible answer has been found and the
    /// pipeline may stop before exhausting its byte/record budget.
    fn absorb_skim_fallback_record(
        &self,
        _stream_id: &str,
        _record: &[u8],
        _skim: &mut SkimResult,
    ) -> bool {
        true
    }

    /// Stream phase: one raw record in, canonical ops out. Combines the pure
    /// parse and normalize stages; each adapter keeps its `parse_line` public
    /// so the stages stay independently testable and benchable.
    fn ingest_line(&self, ctx: &mut NormalizeCtx, stream_id: &str, line: &[u8]) -> LineOutcome;
}
