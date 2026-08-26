//! Canonical row types, mirroring the projection DDL in
//! docs/canonical-model.html exactly. Token components are `Option` because
//! "never reported" is not zero — the projection preserves that distinction.

use serde::{Deserialize, Serialize};

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

/// One flattened timeline row type. `Agent` is the assistant text row; tool
/// calls are their own rows with the result merged in (docs/canonical-model.html).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
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
pub enum TaskStatus {
    Pending,
    InProgress,
    Completed,
}

impl TaskStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::InProgress => "in_progress",
            Self::Completed => "completed",
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct ToolCall {
    /// Native correlation id — the merge key for the result.
    pub tool_id: Option<String>,
    pub name: String,
    pub input: serde_json::Value,
    /// Result text; `None` until the result lands (live sessions).
    pub result: Option<String>,
    /// Structured result payload — opaque by policy (spaghetti scar).
    pub payload: Option<serde_json::Value>,
    pub is_error: Option<bool>,
}

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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct SessionRow {
    pub id: String,
    pub native_session_id: String,
    pub runtime_id: String,
    pub project_id: Option<String>,
    pub parent_session_id: Option<String>,
    /// `== id` for top-level sessions; subagent trees roll up here.
    pub root_session_id: String,
    pub role: SessionRole,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub started_at: Ms,
    pub updated_at: Ms,
    pub stream_state: StreamState,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
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
    /// Anchors a spawned subagent thread to this tool_use row.
    pub branch_tool_id: Option<String>,
    pub usage: Option<TokenSnapshot>,
    pub agent_version: Option<String>,
    pub src_cursor: Option<i64>,
    pub src_offset: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceObsRow {
    pub session_id: String,
    pub at: Ms,
    pub cwd: String,
    pub git_branch: Option<String>,
    pub git_repo: Option<String>,
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
