//! Raw types for the `events` stream
//! (`session-state/<session-id>/events.jsonl`).
//!
//! Evidence: THE CANONICAL `groundtruth.json` — fixtures at 1.0.80 merged with
//! the writer JSON Schema (`sources/1.0.80.writer-schema.json`). Fixture-backed
//! types are typed below. The other persisted writer types (permission.*,
//! subagent.*, compaction, …) parse as `ParsedLine::UnknownType` until a
//! scenario writes them. Ephemeral streaming types are not persisted.
//!
//! Envelope: required `{id, timestamp, parentId, type, data}`; `parentId` is
//! explicit null on the first event; `agentId` / `ephemeral` are optional by
//! absence. Field requiredness follows the merged canonical document, not the
//! fixtures alone.

use rosetta_adapter_support::{Drift, Unknown, note_unknown};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

/// Outcome of parsing one events.jsonl line.
#[derive(Debug)]
pub enum ParsedLine {
    Known(Box<EventLine>),
    /// An envelope `type` this schema version does not know — soft drift.
    UnknownType {
        rtype: Option<String>,
        value: Value,
    },
}

const KNOWN_TYPES: &[&str] = &[
    "session.start",
    "session.permissions_changed",
    "session.model_change",
    "session.resume",
    "session.shutdown",
    "system.message",
    "user.message",
    "assistant.turn_start",
    "assistant.message",
    "assistant.turn_end",
    "tool.execution_start",
    "tool.execution_complete",
];

/// Parse one line of a Copilot `events.jsonl` file.
pub fn parse_line(line: &str) -> Result<ParsedLine, serde_json::Error> {
    let value: Value = serde_json::from_str(line)?;
    let rtype = value["type"].as_str().map(str::to_owned);
    if rtype
        .as_deref()
        .is_some_and(|name| KNOWN_TYPES.contains(&name))
    {
        Ok(ParsedLine::Known(Box::new(serde_json::from_value(value)?)))
    } else {
        Ok(ParsedLine::UnknownType { rtype, value })
    }
}

fn skip_none<T>(value: &Option<T>) -> bool {
    value.is_none()
}

/// Present JSON value, including explicit `null`. Absent fields stay `None`.
fn deserialize_present_value<'de, D>(deserializer: D) -> Result<Option<Value>, D::Error>
where
    D: Deserializer<'de>,
{
    Value::deserialize(deserializer).map(Some)
}

#[derive(Debug, Deserialize, Serialize)]
pub struct EventLine {
    pub id: String,
    pub timestamp: String,
    /// Explicit null on the first event of a session — nullable, never absent.
    #[serde(rename = "parentId")]
    pub parent_id: Option<String>,
    #[serde(default, rename = "agentId", skip_serializing_if = "skip_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub ephemeral: Option<bool>,
    #[serde(flatten)]
    pub payload: EventPayload,
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl Drift for EventLine {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        note_unknown(&self.unknown, prefix, out);
        self.payload.drift(&format!("{prefix}.data"), out);
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", content = "data")]
#[allow(clippy::large_enum_variant)]
pub enum EventPayload {
    #[serde(rename = "session.start")]
    SessionStart(SessionStart),
    #[serde(rename = "session.permissions_changed")]
    SessionPermissionsChanged(SessionPermissionsChanged),
    #[serde(rename = "session.model_change")]
    SessionModelChange(SessionModelChange),
    #[serde(rename = "session.resume")]
    SessionResume(SessionResume),
    #[serde(rename = "session.shutdown")]
    SessionShutdown(SessionShutdown),
    #[serde(rename = "system.message")]
    SystemMessage(SystemMessage),
    #[serde(rename = "user.message")]
    UserMessage(UserMessage),
    #[serde(rename = "assistant.turn_start")]
    AssistantTurnStart(AssistantTurnStart),
    #[serde(rename = "assistant.message")]
    AssistantMessage(AssistantMessage),
    #[serde(rename = "assistant.turn_end")]
    AssistantTurnEnd(AssistantTurnEnd),
    #[serde(rename = "tool.execution_start")]
    ToolExecutionStart(ToolExecutionStart),
    #[serde(rename = "tool.execution_complete")]
    ToolExecutionComplete(ToolExecutionComplete),
}

impl Drift for EventPayload {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        match self {
            EventPayload::SessionStart(p) => note_unknown(&p.unknown, prefix, out),
            EventPayload::SessionPermissionsChanged(p) => note_unknown(&p.unknown, prefix, out),
            EventPayload::SessionModelChange(p) => note_unknown(&p.unknown, prefix, out),
            EventPayload::SessionResume(p) => note_unknown(&p.unknown, prefix, out),
            EventPayload::SessionShutdown(p) => note_unknown(&p.unknown, prefix, out),
            EventPayload::SystemMessage(p) => note_unknown(&p.unknown, prefix, out),
            EventPayload::UserMessage(p) => note_unknown(&p.unknown, prefix, out),
            EventPayload::AssistantTurnStart(p) => note_unknown(&p.unknown, prefix, out),
            EventPayload::AssistantMessage(p) => p.drift(prefix, out),
            EventPayload::AssistantTurnEnd(p) => note_unknown(&p.unknown, prefix, out),
            EventPayload::ToolExecutionStart(p) => note_unknown(&p.unknown, prefix, out),
            EventPayload::ToolExecutionComplete(p) => note_unknown(&p.unknown, prefix, out),
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionStart {
    pub session_id: String,
    pub version: u64,
    pub producer: String,
    pub copilot_version: String,
    pub start_time: String,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub already_in_use: Option<bool>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub context: Option<Value>,
    #[serde(
        default,
        deserialize_with = "deserialize_present_value",
        skip_serializing_if = "skip_none"
    )]
    pub context_tier: Option<Value>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub detached_from_spawning_parent_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub github_mcp_tool_config: Option<Value>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub reasoning_effort: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub reasoning_summary: Option<Value>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub remote_steerable: Option<bool>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub selected_model: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub session_limits: Option<Value>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub verbosity: Option<String>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionPermissionsChanged {
    pub previous_allow_all_permissions: bool,
    pub allow_all_permissions: bool,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub previous_allow_all_permission_mode: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub allow_all_permission_mode: Option<String>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionModelChange {
    pub new_model: String,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub previous_model: Option<String>,
    #[serde(
        default,
        deserialize_with = "deserialize_present_value",
        skip_serializing_if = "skip_none"
    )]
    pub context_tier: Option<Value>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub cause: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub previous_reasoning_effort: Option<String>,
    #[serde(
        default,
        deserialize_with = "deserialize_present_value",
        skip_serializing_if = "skip_none"
    )]
    pub reasoning_effort: Option<Value>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub previous_reasoning_summary: Option<Value>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub reasoning_summary: Option<Value>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub previous_verbosity: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub verbosity: Option<String>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionResume {
    pub resume_time: String,
    pub event_count: u64,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub already_in_use: Option<bool>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub context: Option<Value>,
    #[serde(
        default,
        deserialize_with = "deserialize_present_value",
        skip_serializing_if = "skip_none"
    )]
    pub context_tier: Option<Value>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub continue_pending_work: Option<bool>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub events_file_size_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub reasoning_effort: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub reasoning_summary: Option<Value>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub remote_steerable: Option<bool>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub selected_model: Option<String>,
    #[serde(
        default,
        deserialize_with = "deserialize_present_value",
        skip_serializing_if = "skip_none"
    )]
    pub session_limits: Option<Value>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub session_was_active: Option<bool>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub verbosity: Option<String>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionShutdown {
    pub shutdown_type: String,
    pub total_api_duration_ms: u64,
    pub session_start_time: u64,
    pub code_changes: Value,
    pub model_metrics: Value,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub conversation_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub current_model: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub current_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub error_reason: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub events_file_size_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub system_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub token_details: Option<Value>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub tool_definitions_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub total_nano_aiu: Option<u64>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub total_premium_requests: Option<u64>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemMessage {
    pub content: String,
    pub role: String,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub interaction_id: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub metadata: Option<Value>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub name: Option<String>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UserMessage {
    pub content: String,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub transformed_content: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub attachments: Option<Vec<Value>>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub supported_native_document_mime_types: Option<Vec<Value>>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub native_document_path_fallback_paths: Option<Value>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub delivery: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub agent_mode: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub is_autopilot_continuation: Option<bool>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub interaction_id: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub parent_agent_task_id: Option<String>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantTurnStart {
    pub turn_id: String,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub interaction_id: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub model: Option<String>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantMessage {
    pub message_id: String,
    pub content: String,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub tool_requests: Option<Vec<ToolRequest>>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub interaction_id: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub rte: Option<bool>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub api_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub chunk_count: Option<u64>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub chunk_index: Option<u64>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub citations: Option<Value>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub client_request_id: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub encrypted_content: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub parent_tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub phase: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub reasoning_opaque: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub reasoning_text: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub reasoning_wire_field: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub request_id: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub server_tools: Option<Value>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub service_request_id: Option<String>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl Drift for AssistantMessage {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        note_unknown(&self.unknown, prefix, out);
        if let Some(requests) = &self.tool_requests {
            for (i, request) in requests.iter().enumerate() {
                note_unknown(
                    &request.unknown,
                    &format!("{prefix}.toolRequests[{i}]"),
                    out,
                );
            }
        }
    }
}

/// `assistant.message.data.toolRequests[]`. `arguments` stays untyped.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolRequest {
    pub tool_call_id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub arguments: Option<Value>,
    #[serde(default, rename = "type", skip_serializing_if = "skip_none")]
    pub request_type: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub intention_summary: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub mcp_server_name: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub mcp_tool_name: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub tool_title: Option<String>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantTurnEnd {
    pub turn_id: String,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub model: Option<String>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolExecutionStart {
    pub tool_call_id: String,
    pub tool_name: String,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub arguments: Option<Value>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub display_verbatim: Option<bool>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub mcp_server_name: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub mcp_tool_name: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub parent_tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub rte: Option<bool>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub shell_tool_info: Option<Value>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub tool_description: Option<Value>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub turn_id: Option<String>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolExecutionComplete {
    pub tool_call_id: String,
    pub success: bool,
    /// Present on success; absent on a denied/failed call (tool-denied fixture).
    #[serde(default, skip_serializing_if = "skip_none")]
    pub result: Option<Value>,
    /// Present on failure (`code: denied` in the tool-denied fixture).
    #[serde(default, skip_serializing_if = "skip_none")]
    pub error: Option<Value>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub interaction_id: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub is_user_requested: Option<bool>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub mcp_meta: Option<Value>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub parent_tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub rte: Option<bool>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub sandboxed: Option<bool>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub tool_description: Option<Value>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub tool_telemetry: Option<Value>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub turn_id: Option<String>,
    #[serde(flatten)]
    pub unknown: Unknown,
}
