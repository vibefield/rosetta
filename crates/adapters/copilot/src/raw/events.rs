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
    "session.binary_asset",
    "abort",
    "assistant.reasoning",
    "external_tool.completed",
    "external_tool.requested",
    "session.start",
    "session.permissions_changed",
    "session.model_change",
    "session.resume",
    "session.shutdown",
    "session.autopilot_objective_changed",
    "session.compaction_complete",
    "session.context_changed",
    "session.context_cleared",
    "session.handoff",
    "session.info",
    "session.schedule_cancelled",
    "session.schedule_created",
    "session.schedule_rearmed",
    "session.task_complete",
    "session.usage_checkpoint",
    "session.warning",
    "session.error",
    "session.workspace_file_changed",
    "skill.invoked",
    "subagent.completed",
    "subagent.failed",
    "subagent.started",
    "system.message",
    "system.notification",
    "user.message",
    "assistant.turn_start",
    "assistant.message",
    "assistant.turn_end",
    "tool.execution_start",
    "tool.execution_complete",
    "tool.user_requested",
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
    #[serde(rename = "session.binary_asset")]
    SessionBinaryAsset(BinaryAsset),
    #[serde(rename = "abort")]
    Abort(Abort),
    #[serde(rename = "assistant.reasoning")]
    AssistantReasoning(AssistantReasoning),
    #[serde(rename = "external_tool.completed")]
    ExternalToolCompleted(ExternalToolCompleted),
    #[serde(rename = "external_tool.requested")]
    ExternalToolRequested(ExternalToolRequested),
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
    #[serde(rename = "session.autopilot_objective_changed")]
    SessionAutopilotObjectiveChanged(AutopilotObjectiveChanged),
    #[serde(rename = "session.compaction_complete")]
    SessionCompactionComplete(SessionCompactionComplete),
    #[serde(rename = "session.context_changed")]
    SessionContextChanged(SessionContextChanged),
    #[serde(rename = "session.context_cleared")]
    SessionContextCleared(SessionContextCleared),
    #[serde(rename = "session.handoff")]
    SessionHandoff(SessionHandoff),
    #[serde(rename = "session.info")]
    SessionInfo(SessionNotice),
    #[serde(rename = "session.schedule_cancelled")]
    SessionScheduleCancelled(SessionScheduleCancelled),
    #[serde(rename = "session.schedule_created")]
    SessionScheduleCreated(SessionScheduleCreated),
    #[serde(rename = "session.schedule_rearmed")]
    SessionScheduleRearmed(SessionScheduleRearmed),
    #[serde(rename = "session.task_complete")]
    SessionTaskComplete(SessionTaskComplete),
    #[serde(rename = "session.usage_checkpoint")]
    SessionUsageCheckpoint(SessionUsageCheckpoint),
    #[serde(rename = "session.warning")]
    SessionWarning(SessionNotice),
    #[serde(rename = "session.error")]
    SessionError(SessionNotice),
    #[serde(rename = "session.workspace_file_changed")]
    SessionWorkspaceFileChanged(SessionWorkspaceFileChanged),
    #[serde(rename = "skill.invoked")]
    SkillInvoked(SkillInvoked),
    #[serde(rename = "subagent.completed")]
    SubagentCompleted(SubagentCompleted),
    #[serde(rename = "subagent.failed")]
    SubagentFailed(SubagentFailed),
    #[serde(rename = "subagent.started")]
    SubagentStarted(SubagentStarted),
    #[serde(rename = "system.message")]
    SystemMessage(SystemMessage),
    #[serde(rename = "system.notification")]
    SystemNotification(SystemNotification),
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
    #[serde(rename = "tool.user_requested")]
    ToolUserRequested(ToolUserRequested),
}

impl Drift for EventPayload {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        match self {
            EventPayload::SessionBinaryAsset(p) => note_unknown(&p.unknown, prefix, out),
            EventPayload::Abort(p) => note_unknown(&p.unknown, prefix, out),
            EventPayload::AssistantReasoning(p) => note_unknown(&p.unknown, prefix, out),
            EventPayload::ExternalToolCompleted(p) => note_unknown(&p.unknown, prefix, out),
            EventPayload::ExternalToolRequested(p) => note_unknown(&p.unknown, prefix, out),
            EventPayload::SessionStart(p) => note_unknown(&p.unknown, prefix, out),
            EventPayload::SessionPermissionsChanged(p) => note_unknown(&p.unknown, prefix, out),
            EventPayload::SessionModelChange(p) => note_unknown(&p.unknown, prefix, out),
            EventPayload::SessionResume(p) => note_unknown(&p.unknown, prefix, out),
            EventPayload::SessionShutdown(p) => note_unknown(&p.unknown, prefix, out),
            EventPayload::SessionAutopilotObjectiveChanged(p) => {
                note_unknown(&p.unknown, prefix, out)
            }
            EventPayload::SessionCompactionComplete(p) => note_unknown(&p.unknown, prefix, out),
            EventPayload::SessionContextChanged(p) => note_unknown(&p.unknown, prefix, out),
            EventPayload::SessionContextCleared(p) => note_unknown(&p.unknown, prefix, out),
            EventPayload::SessionHandoff(p) => note_unknown(&p.unknown, prefix, out),
            EventPayload::SessionInfo(p)
            | EventPayload::SessionWarning(p)
            | EventPayload::SessionError(p) => note_unknown(&p.unknown, prefix, out),
            EventPayload::SessionScheduleCancelled(p) => note_unknown(&p.unknown, prefix, out),
            EventPayload::SessionScheduleCreated(p) => note_unknown(&p.unknown, prefix, out),
            EventPayload::SessionScheduleRearmed(p) => note_unknown(&p.unknown, prefix, out),
            EventPayload::SessionTaskComplete(p) => note_unknown(&p.unknown, prefix, out),
            EventPayload::SessionUsageCheckpoint(p) => note_unknown(&p.unknown, prefix, out),
            EventPayload::SessionWorkspaceFileChanged(p) => note_unknown(&p.unknown, prefix, out),
            EventPayload::SkillInvoked(p) => note_unknown(&p.unknown, prefix, out),
            EventPayload::SubagentCompleted(p) => note_unknown(&p.unknown, prefix, out),
            EventPayload::SubagentFailed(p) => note_unknown(&p.unknown, prefix, out),
            EventPayload::SubagentStarted(p) => note_unknown(&p.unknown, prefix, out),
            EventPayload::SystemMessage(p) => note_unknown(&p.unknown, prefix, out),
            EventPayload::SystemNotification(p) => note_unknown(&p.unknown, prefix, out),
            EventPayload::UserMessage(p) => note_unknown(&p.unknown, prefix, out),
            EventPayload::AssistantTurnStart(p) => note_unknown(&p.unknown, prefix, out),
            EventPayload::AssistantMessage(p) => p.drift(prefix, out),
            EventPayload::AssistantTurnEnd(p) => note_unknown(&p.unknown, prefix, out),
            EventPayload::ToolExecutionStart(p) => note_unknown(&p.unknown, prefix, out),
            EventPayload::ToolExecutionComplete(p) => note_unknown(&p.unknown, prefix, out),
            EventPayload::ToolUserRequested(p) => note_unknown(&p.unknown, prefix, out),
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Abort {
    pub reason: String,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantReasoning {
    pub content: String,
    pub reasoning_id: String,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub rte: Option<bool>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalToolRequested {
    pub request_id: String,
    pub session_id: String,
    pub tool_call_id: String,
    pub tool_name: String,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub arguments: Option<Value>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub traceparent: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub tracestate: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub working_directory: Option<String>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalToolCompleted {
    pub request_id: String,
    #[serde(flatten)]
    pub unknown: Unknown,
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
pub struct AutopilotObjectiveChanged {
    #[serde(default, skip_serializing_if = "skip_none")]
    pub id: Option<u64>,
    pub operation: String,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub status: Option<String>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionCompactionComplete {
    pub success: bool,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub summary_content: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub checkpoint_number: Option<u64>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub checkpoint_path: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub compaction_tokens_used: Option<Value>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub conversation_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub custom_instructions: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub messages_removed: Option<u64>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub post_compaction_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub pre_compaction_messages_length: Option<u64>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub pre_compaction_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub request_id: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub service_request_id: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub status_code: Option<u64>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub system_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub token_limit: Option<u64>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub tokens_removed: Option<u64>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub tool_definitions_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub trigger: Option<String>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionContextChanged {
    pub cwd: String,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub repository: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub git_root: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub base_commit: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub head_commit: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub host_type: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub pending_git_context: Option<bool>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub repository_host: Option<String>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionContextCleared {
    pub messages_cleared: u64,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub initial_message: Option<String>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionHandoff {
    pub handoff_time: String,
    pub source_type: String,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub context: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub host: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub remote_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub repository: Option<Value>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionNotice {
    pub message: String,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub info_type: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub warning_type: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub error_type: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub error_code: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub eligible_for_auto_switch: Option<bool>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub provider_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub service_request_id: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub stack: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub status_code: Option<u64>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub tip: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub url: Option<String>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionScheduleCreated {
    pub id: u64,
    pub prompt: String,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub at: Option<u64>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub cron: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub display_prompt: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub interval_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub origin: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub recurring: Option<bool>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub self_paced: Option<bool>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub tz: Option<String>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct SessionScheduleCancelled {
    pub id: u64,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionScheduleRearmed {
    pub id: u64,
    pub next_run_at: u64,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionTaskComplete {
    #[serde(default, skip_serializing_if = "skip_none")]
    pub objective_id: Option<u64>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub outcome: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub success: Option<bool>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub summary: Option<String>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionUsageCheckpoint {
    pub total_nano_aiu: u64,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub total_premium_requests: Option<u64>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub model_cache_state: Option<Vec<Value>>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct SessionWorkspaceFileChanged {
    pub operation: String,
    pub path: String,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillInvoked {
    pub content: String,
    pub name: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub allowed_tools: Option<Vec<Value>>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub plugin_name: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub plugin_version: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub trigger: Option<String>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentStarted {
    pub agent_description: String,
    pub agent_display_name: String,
    pub agent_name: String,
    pub tool_call_id: String,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub model: Option<String>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentCompleted {
    pub agent_display_name: String,
    pub agent_name: String,
    pub tool_call_id: String,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub cancelled: Option<bool>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub total_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub total_tool_calls: Option<u64>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentFailed {
    pub agent_display_name: String,
    pub agent_name: String,
    pub error: String,
    pub tool_call_id: String,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub total_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub total_tool_calls: Option<u64>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct SystemNotification {
    pub content: String,
    pub kind: Value,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolUserRequested {
    pub tool_call_id: String,
    pub tool_name: String,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub arguments: Option<Value>,
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

/// Persisted writer-schema 1.0.80: bytes may precede or follow user.message.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BinaryAsset {
    pub asset_id: String,
    pub byte_length: u64,
    pub data: String,
    pub mime_type: String,
    #[serde(rename = "type")]
    pub asset_type: String,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "skip_none")]
    pub metadata: Option<Value>,
    #[serde(flatten)]
    pub unknown: Unknown,
}
