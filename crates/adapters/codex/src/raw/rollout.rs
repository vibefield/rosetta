//! Raw types for the `rollout` stream
//! (`sessions/YYYY/MM/DD/rollout-<timestamp>-<uuid>.jsonl`).
//!
//! Evidence: fixtures/0.149.1 (scenarios basic-chat, tool-use, resume).
//! Envelope: {timestamp, ordinal, type, payload}. Top-level types observed:
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
    pub ordinal: u64,
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
    pub session_id: String,
    pub id: String,
    pub timestamp: String,
    pub cwd: String,
    /// e.g. "codex_exec" — which surface wrote the session.
    pub originator: String,
    /// Per-record writer version stamp.
    pub cli_version: String,
    pub source: String,
    pub thread_source: String,
    pub model_provider: String,
    pub history_mode: String,
    /// Large nested structures — kept raw until a consumer needs them typed.
    pub base_instructions: Value,
    pub context_window: Value,
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

/// Conversation item: message / function_call / function_call_output
/// (open vocabulary — same permissive policy as EventMsg).
#[derive(Debug, Deserialize, Serialize)]
pub struct ResponseItem {
    #[serde(rename = "type")]
    pub item_type: String,
    pub id: String,
    /// Present on every observed item; content unknown to the public API.
    pub internal_chat_message_metadata_passthrough: Value,
    /// message: user | assistant | developer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<Vec<ContentBlock>>,
    /// function_call fields; `arguments` is a JSON-encoded STRING.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    /// function_call_output result; also a STRING payload.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
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
    pub turn_id: String,
    pub cwd: String,
    pub model: String,
    pub approval_policy: String,
    pub approvals_reviewer: String,
    pub current_date: String,
    pub multi_agent_version: String,
    pub personality: String,
    pub realtime_active: bool,
    pub summary: String,
    pub timezone: String,
    /// Nested policy/config objects — kept raw until a consumer needs them.
    pub collaboration_mode: Value,
    pub permission_profile: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_permission_profile: Option<Value>,
    pub sandbox_policy: Value,
    pub workspace_roots: Value,
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
