//! Raw types for the `wire` stream
//! (`sessions/<workDirKey>/<sessionId>/agents/<agentId>/wire.jsonl`).
//!
//! Evidence: THE CANONICAL `groundtruth.json` (iron rule #6) — fixtures at
//! 0.38.0 (basic-chat, tool-use, resume, subagent, error) merged with an
//! organic census. Census-only Event2 types (prompt.accepted, tools.update_store,
//! interaction.*, full_compaction.*, context.apply_compaction, config.update,
//! permission.record_approval_result) stay UnknownType until a fixture writes
//! them.
//!
//! Record types typed below: metadata, runtime.set_binding, profile.bind,
//! permission.set_mode, turn.prompt, context.append_message,
//! plugin.session_start, context.append_loop_event, llm.tools_snapshot,
//! llm.request, usage.record, token_counting.measured, turn.ended,
//! token_counting.turn_recorded. Nested `context.append_loop_event.event.type`
//! is an OPEN vocabulary (observed: step.begin/end, content.part, tool.call,
//! tool.result) — the event payload is a permissive all-optional struct.
//!
//! `agentId` is required in the writer zod schema but optional in bytes:
//! hermetic 0.38.0 always writes it; organic protocol-1.5 sessions usually
//! omit it. `subagents` is omitted on explore sub-agent profile.bind.
//!
//! Parsing policy (CLAUDE.md): permissive on fields — flatten-unknown maps
//! everywhere, non-empty = drift. Strict on envelope structure. Tool-result
//! payloads stay `serde_json::Value`.

use rosetta_adapter_support::{Drift, Unknown, note_unknown};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Outcome of parsing one wire line.
#[derive(Debug)]
pub enum ParsedLine {
    Known(Box<KnownRecord>),
    /// A record `type` this schema version does not know — soft drift.
    UnknownType {
        rtype: Option<String>,
        value: Value,
    },
}

/// Parse one line of a wire JSONL file.
pub fn parse_line(line: &str) -> Result<ParsedLine, serde_json::Error> {
    let value: Value = serde_json::from_str(line)?;
    let rtype = value["type"].as_str().map(str::to_owned);
    let known = matches!(
        rtype.as_deref(),
        Some(
            "metadata"
                | "runtime.set_binding"
                | "profile.bind"
                | "permission.set_mode"
                | "turn.prompt"
                | "context.append_message"
                | "plugin.session_start"
                | "context.append_loop_event"
                | "llm.tools_snapshot"
                | "llm.request"
                | "usage.record"
                | "token_counting.measured"
                | "turn.ended"
                | "token_counting.turn_recorded"
        )
    );
    if known {
        Ok(ParsedLine::Known(Box::new(serde_json::from_value(value)?)))
    } else {
        Ok(ParsedLine::UnknownType { rtype, value })
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type")]
#[allow(clippy::large_enum_variant)]
pub enum KnownRecord {
    #[serde(rename = "metadata")]
    Metadata(MetadataLine),
    #[serde(rename = "runtime.set_binding")]
    RuntimeSetBinding(RuntimeSetBindingLine),
    #[serde(rename = "profile.bind")]
    ProfileBind(ProfileBindLine),
    #[serde(rename = "permission.set_mode")]
    PermissionSetMode(PermissionSetModeLine),
    #[serde(rename = "turn.prompt")]
    TurnPrompt(TurnPromptLine),
    #[serde(rename = "context.append_message")]
    ContextAppendMessage(ContextAppendMessageLine),
    #[serde(rename = "plugin.session_start")]
    PluginSessionStart(PluginSessionStartLine),
    #[serde(rename = "context.append_loop_event")]
    ContextAppendLoopEvent(ContextAppendLoopEventLine),
    #[serde(rename = "llm.tools_snapshot")]
    LlmToolsSnapshot(LlmToolsSnapshotLine),
    #[serde(rename = "llm.request")]
    LlmRequest(LlmRequestLine),
    #[serde(rename = "usage.record")]
    UsageRecord(UsageRecordLine),
    #[serde(rename = "token_counting.measured")]
    TokenCountingMeasured(TokenCountingMeasuredLine),
    #[serde(rename = "turn.ended")]
    TurnEnded(TurnEndedLine),
    #[serde(rename = "token_counting.turn_recorded")]
    TokenCountingTurnRecorded(TokenCountingTurnRecordedLine),
}

impl Drift for KnownRecord {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        match self {
            KnownRecord::Metadata(l) => note_unknown(&l.unknown, prefix, out),
            KnownRecord::RuntimeSetBinding(l) => note_unknown(&l.unknown, prefix, out),
            KnownRecord::ProfileBind(l) => note_unknown(&l.unknown, prefix, out),
            KnownRecord::PermissionSetMode(l) => note_unknown(&l.unknown, prefix, out),
            KnownRecord::TurnPrompt(l) => l.drift(prefix, out),
            KnownRecord::ContextAppendMessage(l) => l.drift(prefix, out),
            KnownRecord::PluginSessionStart(l) => note_unknown(&l.unknown, prefix, out),
            KnownRecord::ContextAppendLoopEvent(l) => l.drift(prefix, out),
            KnownRecord::LlmToolsSnapshot(l) => note_unknown(&l.unknown, prefix, out),
            KnownRecord::LlmRequest(l) => note_unknown(&l.unknown, prefix, out),
            KnownRecord::UsageRecord(l) => l.drift(prefix, out),
            KnownRecord::TokenCountingMeasured(l) => note_unknown(&l.unknown, prefix, out),
            KnownRecord::TurnEnded(l) => note_unknown(&l.unknown, prefix, out),
            KnownRecord::TokenCountingTurnRecorded(l) => note_unknown(&l.unknown, prefix, out),
        }
    }
}

/// First line of a wire file: protocol stamp.
#[derive(Debug, Deserialize, Serialize)]
pub struct MetadataLine {
    pub protocol_version: String,
    pub created_at: u64,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeSetBindingLine {
    pub workspace_id: String,
    pub runtime_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    pub time: u64,
    #[serde(flatten)]
    pub unknown: Unknown,
}

/// Agent profile bind, including the full system prompt text.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileBindLine {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    pub model_alias: String,
    pub profile_name: String,
    pub thinking_effort: String,
    pub system_prompt: String,
    pub environment_disclosure: Value,
    pub agents_md_paths: Value,
    pub active_tool_names: Vec<String>,
    pub disallowed_tools: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagents: Option<Vec<String>>,
    pub time: u64,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionSetModeLine {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    pub mode: String,
    pub time: u64,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnPromptLine {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    pub input: Vec<ContentBlock>,
    pub origin: Origin,
    pub time: u64,
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl Drift for TurnPromptLine {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        note_unknown(&self.unknown, prefix, out);
        for (i, block) in self.input.iter().enumerate() {
            note_unknown(&block.unknown, &format!("{prefix}.input[{i}]"), out);
        }
        note_unknown(&self.origin.unknown, &format!("{prefix}.origin"), out);
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextAppendMessageLine {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    pub message: WireMessage,
    pub time: u64,
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl Drift for ContextAppendMessageLine {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        note_unknown(&self.unknown, prefix, out);
        self.message.drift(&format!("{prefix}.message"), out);
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WireMessage {
    pub role: String,
    pub content: Vec<ContentBlock>,
    pub tool_calls: Value,
    pub origin: Origin,
    /// Present on ordinary user prompts; absent on injected system-reminder lines.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl Drift for WireMessage {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        note_unknown(&self.unknown, prefix, out);
        for (i, block) in self.content.iter().enumerate() {
            note_unknown(&block.unknown, &format!("{prefix}.content[{i}]"), out);
        }
        note_unknown(&self.origin.unknown, &format!("{prefix}.origin"), out);
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ContentBlock {
    #[serde(rename = "type")]
    pub block_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Origin {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
    /// Present on sub-agent system triggers (`kind = "system_trigger"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

/// Plugin hook fire. `content` is explicit null in the 0.38.0 fixtures.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginSessionStartLine {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    pub content: Option<Value>,
    pub time: u64,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextAppendLoopEventLine {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    pub event: LoopEvent,
    pub time: u64,
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl Drift for ContextAppendLoopEventLine {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        note_unknown(&self.unknown, prefix, out);
        self.event.drift(&format!("{prefix}.event"), out);
    }
}

/// Nested loop-event payload. `event_type` is an OPEN vocabulary — all other
/// fields are optional so a new subtype parses and novel fields surface as
/// drift instead of dropped lines.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoopEvent {
    #[serde(rename = "type")]
    pub event_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uuid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_uuid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub part: Option<LoopPart>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Tool-dependent arguments; kept raw.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_uuid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<ToolResultBody>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<RecordedUsage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llm_first_token_latency_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llm_stream_duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llm_request_build_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llm_server_first_token_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llm_server_decode_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llm_client_consume_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_finish_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_finish_reason: Option<String>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl Drift for LoopEvent {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        note_unknown(&self.unknown, prefix, out);
        if let Some(part) = &self.part {
            note_unknown(&part.unknown, &format!("{prefix}.part"), out);
        }
        if let Some(result) = &self.result {
            note_unknown(&result.unknown, &format!("{prefix}.result"), out);
        }
        if let Some(usage) = &self.usage {
            note_unknown(&usage.unknown, &format!("{prefix}.usage"), out);
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct LoopPart {
    #[serde(rename = "type")]
    pub part_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub think: Option<String>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolResultBody {
    pub output: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmToolsSnapshotLine {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    pub hash: String,
    /// Full tool schema array — kept raw (large, tool-set-dependent).
    pub tools: Value,
    pub time: u64,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmRequestLine {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    pub kind: String,
    pub provider: String,
    pub model: String,
    pub model_alias: String,
    pub thinking_effort: String,
    pub thinking_keep: String,
    pub max_tokens: u64,
    pub tool_select: bool,
    pub system_prompt_hash: String,
    pub tools_hash: String,
    pub message_count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_step: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dropped_count: Option<u64>,
    pub time: u64,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageRecordLine {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    pub model: String,
    pub usage: RecordedUsage,
    pub usage_scope: String,
    pub time: u64,
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl Drift for UsageRecordLine {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        note_unknown(&self.unknown, prefix, out);
        note_unknown(&self.usage.unknown, &format!("{prefix}.usage"), out);
    }
}

/// Usage as Kimi records it on the wire (not the raw OpenAI usage block).
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordedUsage {
    pub input_other: u64,
    pub output: u64,
    pub input_cache_read: u64,
    pub input_cache_creation: u64,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenCountingMeasuredLine {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    pub length: u64,
    pub tokens: u64,
    pub time: u64,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnEndedLine {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    pub turn_id: u64,
    pub reason: String,
    /// Present when `reason` is `failed` (error scenario: provider.filtered).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<Value>,
    pub duration_ms: u64,
    pub time: u64,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenCountingTurnRecordedLine {
    pub agent_id: String,
    pub turn_id: u64,
    pub length: u64,
    pub tokens: u64,
    pub time: u64,
    #[serde(flatten)]
    pub unknown: Unknown,
}
