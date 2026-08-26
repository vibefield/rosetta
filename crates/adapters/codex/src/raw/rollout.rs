//! Raw types for the `rollout` stream
//! (`sessions/YYYY/MM/DD/rollout-<timestamp>-<uuid>.jsonl`).
//!
//! Evidence: fixtures/0.149.1 (scenarios basic-chat, tool-use, resume).
//! Envelope: {timestamp, ordinal?, type, payload}. Top-level types observed:
//! session_meta, event_msg, response_item, turn_context, world_state.
//! Types known from research but not yet fixture-backed (e.g. `compacted`)
//! parse as `ParsedLine::UnknownType` until a scenario exercises them.
//!
//! Parsing policy (CLAUDE.md): permissive on fields — flatten-unknown maps
//! everywhere, non-empty = drift. Strict on envelope structure. Payload
//! sub-vocabularies (`event_msg.payload.type`, `response_item.payload.type`)
//! churn fast upstream, so payloads are typed as permissive all-optional
//! structs rather than inner enums — a new subtype parses fine and its novel
//! fields surface as drift instead of dropped lines (spaghetti pitfall §8.4).

use rosetta_adapter_support::{Drift, Unknown, note_unknown};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Outcome of parsing one rollout line.
#[derive(Debug)]
pub enum ParsedLine {
    Known(Box<RolloutLine>),
    /// An envelope `type` this schema version does not know — soft drift.
    UnknownType {
        rtype: Option<String>,
        value: Value,
    },
}

/// Parse one line of a rollout JSONL file.
pub fn parse_line(line: &str) -> Result<ParsedLine, serde_json::Error> {
    let value: Value = serde_json::from_str(line)?;
    let rtype = value["type"].as_str().map(str::to_owned);
    let known = matches!(
        rtype.as_deref(),
        Some("session_meta" | "event_msg" | "response_item" | "turn_context" | "world_state")
    );
    if known {
        Ok(ParsedLine::Known(Box::new(serde_json::from_value(value)?)))
    } else {
        Ok(ParsedLine::UnknownType { rtype, value })
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct RolloutLine {
    pub timestamp: String,
    /// Added by newer writers. Legacy rollouts use the source byte offset as
    /// their deterministic record discriminator during normalization.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ordinal: Option<u64>,
    #[serde(flatten)]
    pub payload: RolloutPayload,
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl Drift for RolloutLine {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        note_unknown(&self.unknown, prefix, out);
        self.payload.drift(&format!("{prefix}.payload"), out);
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", content = "payload")]
#[allow(clippy::large_enum_variant)]
pub enum RolloutPayload {
    #[serde(rename = "session_meta")]
    SessionMeta(SessionMeta),
    #[serde(rename = "event_msg")]
    EventMsg(EventMsg),
    #[serde(rename = "response_item")]
    ResponseItem(ResponseItem),
    #[serde(rename = "turn_context")]
    TurnContext(TurnContext),
    #[serde(rename = "world_state")]
    WorldState(WorldState),
}

impl Drift for RolloutPayload {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        match self {
            RolloutPayload::SessionMeta(p) => note_unknown(&p.unknown, prefix, out),
            RolloutPayload::EventMsg(p) => note_unknown(&p.unknown, prefix, out),
            RolloutPayload::ResponseItem(p) => p.drift(prefix, out),
            RolloutPayload::TurnContext(p) => note_unknown(&p.unknown, prefix, out),
            RolloutPayload::WorldState(p) => note_unknown(&p.unknown, prefix, out),
        }
    }
}

/// Session header: identity, provenance, and the full base instruction text.
#[derive(Debug, Deserialize, Serialize)]
pub struct SessionMeta {
    /// Older writers only stamped `id`; newer ones repeat it here.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub id: String,
    pub timestamp: String,
    pub cwd: String,
    /// e.g. "codex_exec" — which surface wrote the session.
    pub originator: String,
    /// Per-record writer version stamp.
    pub cli_version: String,
    /// A string on older writers and a structured subagent descriptor on newer
    /// multi-agent writers.
    pub source: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_source: Option<String>,
    pub model_provider: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history_mode: Option<String>,
    /// Large nested structures — kept raw until a consumer needs them typed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_instructions: Option<Value>,
    /// Predecessor of `base_instructions`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub multi_agent_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_thread_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_nickname: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub forked_from_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history_base: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dynamic_tools: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subagent_history_start_ordinal: Option<u64>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

/// Lifecycle/telemetry event. `event_type` is an OPEN vocabulary (observed:
/// task_started, item_completed, token_count, task_complete) — all payload
/// fields are optional so new subtypes parse; novel fields surface as drift.
#[derive(Debug, Deserialize, Serialize)]
pub struct EventMsg {
    #[serde(rename = "type")]
    pub event_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_to_first_token_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_context_window: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub collaboration_mode_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_agent_message: Option<String>,
    /// token_count: carries CUMULATIVE total_token_usage (not deltas).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub info: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate_limits: Option<Value>,
    /// item_completed: the completed thread item, shape varies per item kind.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_settings: Option<Value>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

/// Conversation item: message / function_call / custom_tool_call and outputs
/// (open vocabulary — same permissive policy as EventMsg).
#[derive(Debug, Deserialize, Serialize)]
pub struct ResponseItem {
    #[serde(rename = "type")]
    pub item_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Absent on some historical tool results; content is not public API.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub internal_chat_message_metadata_passthrough: Option<Value>,
    /// message: user | assistant | developer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<Vec<ContentBlock>>,
    /// A JSON-encoded string for function calls and an object for tool search.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments: Option<Value>,
    /// Free-form body for `custom_tool_call` (for example `apply_patch`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// A string on older writers and content blocks on newer writers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<Value>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl Drift for ResponseItem {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        note_unknown(&self.unknown, prefix, out);
        if let Some(blocks) = &self.content {
            for (i, b) in blocks.iter().enumerate() {
                note_unknown(&b.unknown, &format!("{prefix}.content[{i}]"), out);
            }
        }
    }
}

/// Message content block (observed: input_text, output_text).
#[derive(Debug, Deserialize, Serialize)]
pub struct ContentBlock {
    #[serde(rename = "type")]
    pub block_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

/// Per-turn execution context (model, policies, sandbox, workspace).
#[derive(Debug, Deserialize, Serialize)]
pub struct TurnContext {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approval_policy: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approvals_reviewer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub multi_agent_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub personality: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub realtime_active: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    /// Nested policy/config objects — kept raw until a consumer needs them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub collaboration_mode: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission_profile: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_permission_profile: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sandbox_policy: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_roots: Option<Value>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

/// Workspace world-state snapshot (`full` marks complete vs incremental).
#[derive(Debug, Deserialize, Serialize)]
pub struct WorldState {
    pub full: bool,
    pub state: Value,
    #[serde(flatten)]
    pub unknown: Unknown,
}
