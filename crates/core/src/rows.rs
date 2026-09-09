//! Canonical row types, mirroring the projection DDL in
//! docs/canonical-model.html exactly. Token components are `Option` because
//! "never reported" is not zero — the projection preserves that distinction.

use serde::{Deserialize, Serialize};

use crate::timeline::{
    AttachmentRef, CitationRef, ParticipantColor, ResponseGroup, TimelineGroup, ToolInteraction,
};

/// Unix milliseconds.
pub type Ms = i64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub enum SessionRole {
    Main,
    Subagent,
    Team,
}

impl SessionRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Main => "main",
            Self::Subagent => "subagent",
            Self::Team => "team",
        }
    }
}

/// Confidence of one native session-title fact. This is deliberately about
/// provenance, not arrival order: an explicit native display name must not be
/// replaced by a prompt/checkpoint-derived fallback merely because that
/// source was scanned later.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub enum TitleKind {
    Fallback,
    Generated,
    Explicit,
}

impl TitleKind {
    pub fn rank(self) -> i64 {
        match self {
            Self::Fallback => 1,
            Self::Generated => 2,
            Self::Explicit => 3,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub enum StreamState {
    Pending,
    Streaming,
    Indexed,
}

impl StreamState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Streaming => "streaming",
            Self::Indexed => "indexed",
        }
    }
}

/// Normalizer-local construction vocabulary. This draft never crosses the
/// [`crate::IngestOp`] boundary; [`crate::IngestOp::project_message`] closes it
/// into a [`crate::TimelineEntry`] or deliberately leaves native context in
/// the raw ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageKind {
    User,
    Agent,
    Thinking,
    ToolUse,
    System,
    Summary,
}

impl MessageKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Agent => "agent",
            Self::Thinking => "thinking",
            Self::ToolUse => "tool_use",
            Self::System => "system",
            Self::Summary => "summary",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EditKind {
    Edit,
    Write,
    Checkpoint,
}

impl EditKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Edit => "edit",
            Self::Write => "write",
            Self::Checkpoint => "checkpoint",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub enum TaskStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
    Cancelled,
}

impl TaskStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::InProgress => "in_progress",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

/// Per-response token snapshot. Every component is `Option`: `None` means the
/// agent never reported it, which must never be coerced to zero.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct TokenSnapshot {
    pub input: Option<i64>,
    pub output: Option<i64>,
    pub cache_read: Option<i64>,
    pub cache_write: Option<i64>,
    pub thinking: Option<i64>,
}

impl TokenSnapshot {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// Compatibility name retained while callers move to the explicit canonical
/// term.  The value itself is the closed typed interaction—there is no legacy
/// JSON-bearing tool representation behind this alias.
pub type ToolCall = ToolInteraction;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct RuntimeRow {
    pub id: String,
    pub kind: String,
    pub data_root: String,
    pub enabled: bool,
    pub account: Option<serde_json::Value>,
    pub config: Option<serde_json::Value>,
    pub skills: Option<serde_json::Value>,
    pub versions_seen: Option<serde_json::Value>,
    pub discovered_at: Ms,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct ProjectRow {
    pub id: String,
    pub name: String,
    pub root_paths: Vec<String>,
    pub git_repo: Option<String>,
}

/// Native evidence connecting a child session to the invocation or team that
/// created it. This belongs on the session relation because Claude can write
/// the sidecar before or after either transcript.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct SessionDelegation {
    /// Exact native tool-use id from the child sidecar, when recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_tool_native_id: Option<String>,
    /// Exact native workflow run which owns this child. A workflow child is
    /// rendered only inside that run's phase/worker hierarchy; it is never an
    /// unanchored child in the parent transcript.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_run_id: Option<String>,
    /// Persistent Agent Team identity. Absence means an ordinary subagent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
    /// Native participant address colour; never inferred from ordering.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<ParticipantColor>,
    /// Launch configuration. Observed response runtime still wins in the UI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub configured_model: Option<String>,
}

/// Bounds of recorded conversation activity, including descendant sessions.
/// Derived from canonical messages and native tool lifecycle timestamps;
/// metadata edits, file dates, and ingestion progress never contribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct SessionActivity {
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub first_at: Ms,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub last_at: Ms,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct SessionRow {
    pub id: String,
    pub native_session_id: String,
    pub runtime_id: String,
    pub project_id: Option<String>,
    /// One normalized primary working directory, selected by the adapter's
    /// bounded metadata admission. Timeline records never mutate it.
    pub cwd: Option<String>,
    pub parent_session_id: Option<String>,
    /// Exact child-launch and team metadata, when a native sidecar proves it.
    #[serde(default)]
    pub delegation: Option<SessionDelegation>,
    /// `== id` for top-level sessions; subagent trees roll up here.
    pub root_session_id: String,
    pub role: SessionRole,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub started_at: Ms,
    /// Source/metadata freshness, not conversation activity.
    pub updated_at: Ms,
    /// Unknown until timestamped conversation evidence is available. This is
    /// a read-time projection; SessionMeta and old ledger frames cannot set it.
    #[serde(default)]
    pub activity: Option<SessionActivity>,
    pub stream_state: StreamState,
}

impl SessionRow {
    /// Most recent recorded conversation first; unknown activity sorts last.
    pub fn compare_recent(&self, other: &Self) -> std::cmp::Ordering {
        other.activity.map(|activity| activity.last_at)
            .cmp(&self.activity.map(|activity| activity.last_at))
            .then_with(|| self.id.cmp(&other.id))
    }
}

/// One bounded title candidate for the transaction-local fold. The store
/// compares it by [`TitleKind`] and native timestamp, persists only the
/// selected public `session.title`, and leaves losing candidates in the raw
/// ledger/process RAM.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct SessionTitleRow {
    pub session_id: String,
    pub title: String,
    pub kind: TitleKind,
    pub at: Ms,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessageRow {
    /// Stable, unique within the session — native uuids aren't.
    pub timeline_id: String,
    pub session_id: String,
    /// Presentation order ONLY. Live delivery sequences on the hub's
    /// (epoch, mutation_seq) — merges mutate earlier rows.
    pub ordinal: i64,
    pub kind: MessageKind,
    pub subtype: Option<String>,
    pub uuid: Option<String>,
    pub parent_uuid: Option<String>,
    pub ts: Option<Ms>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub content: Option<String>,
    pub tool: Option<ToolCall>,
    pub attachments: Vec<AttachmentRef>,
    pub citations: Vec<CitationRef>,
    pub group: Option<TimelineGroup>,
    pub response: Option<ResponseGroup>,
    pub block_index: Option<u32>,
    /// Anchors a spawned subagent thread to this tool_use row.
    pub branch_tool_id: Option<String>,
    pub usage: Option<TokenSnapshot>,
    pub agent_version: Option<String>,
    pub src_cursor: Option<i64>,
    pub src_offset: Option<i64>,
}

/// Replacement-keyed usage reported for a whole native session/task rather
/// than for one response row. These samples contribute tokens to usage
/// buckets but deliberately do not manufacture timeline messages or counts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionUsageRow {
    pub session_id: String,
    /// Stable native replacement key within the session (for example a model
    /// or task id). Re-emitting this key replaces, rather than double-counts,
    /// the prior snapshot.
    pub key: String,
    pub ts: Ms,
    pub model: Option<String>,
    pub tokens: TokenSnapshot,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileEditRow {
    pub session_id: String,
    /// The tool_use row that made the change, when derivable.
    pub message_timeline_id: Option<String>,
    pub path: String,
    pub kind: EditKind,
    pub snapshot_ref: Option<String>,
    pub lines_added: Option<i64>,
    pub lines_removed: Option<i64>,
    pub at: Option<Ms>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanRow {
    pub session_id: String,
    pub revision: i64,
    pub content: String,
    pub updated_at: Ms,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskRow {
    pub id: String,
    pub session_id: String,
    pub status: TaskStatus,
    pub content: String,
    pub created_at: Option<Ms>,
    pub updated_at: Option<Ms>,
    /// First source-observed transition into `in_progress`, when the native
    /// format makes it explicit. Older ledger records deserialize without it.
    #[serde(default)]
    pub started_at: Option<Ms>,
    /// First source-observed terminal transition for the current run.
    #[serde(default)]
    pub terminal_at: Option<Ms>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GitCommitRow {
    pub commit_id: String,
    pub session_id: String,
    pub branch: Option<String>,
    pub at: Option<Ms>,
}

/// One aggregated drift observation. The projection groups these by the full
/// key and keeps counts — never one row per bad record (spaghetti scar:
/// 457,311 persisted error rows).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DriftFact {
    pub runtime_id: String,
    pub agent_version: String,
    pub stream: String,
    pub record_kind: String,
    pub error_code: String,
    pub shape_hash: String,
    pub at: Option<Ms>,
    pub sample_cursor: Option<i64>,
    pub sample_offset: Option<i64>,
}
