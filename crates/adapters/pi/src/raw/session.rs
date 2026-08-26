//! Raw types for the `session` stream
//! (`agent/sessions/<cwd-slug>/<timestamp>_<uuid>.jsonl`).
//!
//! Evidence: THE CANONICAL `groundtruth.json` — fixtures at 0.84.3 merged with
//! writer-source `sources/0.84.3.writer.json` (session-manager.ts / messages.ts /
//! pi-ai types.ts at tag v0.84.3).
//!
//! Fixture-backed types: session (incl. parentSession on forks), session_info,
//! model_change, thinking_level_change, message (user / assistant / toolResult,
//! including stopReason=error). Writer-only types (compaction, branch_summary,
//! custom, custom_message, label) are typed from source so they parse when they
//! appear; they remain research-evidence in the canonical file until a scenario
//! writes them. Compaction.retainedTail is documented but NOT in the 0.84.3
//! writer interface — do not add it.
//!
//! Parsing policy: flatten-unknown maps everywhere; unknown top-level
//! discriminants are soft drift; a known type that fails to parse is hard.
//! Tool-call arguments stay Value.

use rosetta_adapter_support::{Drift, Unknown, note_unknown};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Outcome of parsing one session JSONL line.
#[derive(Debug)]
pub enum ParsedLine {
    Known(Box<KnownRecord>),
    UnknownType { rtype: Option<String>, value: Value },
}

/// Parse one line of a session JSONL file.
pub fn parse_line(line: &str) -> Result<ParsedLine, serde_json::Error> {
    let value: Value = serde_json::from_str(line)?;
    let rtype = value["type"].as_str().map(str::to_owned);
    let known = matches!(
        rtype.as_deref(),
        Some(
            "session"
                | "message"
                | "model_change"
                | "thinking_level_change"
                | "session_info"
                | "compaction"
                | "branch_summary"
                | "custom"
                | "custom_message"
                | "label"
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
    #[serde(rename = "session")]
    Session(SessionHeader),
    #[serde(rename = "message")]
    Message(MessageEntry),
    #[serde(rename = "model_change")]
    ModelChange(ModelChangeEntry),
    #[serde(rename = "thinking_level_change")]
    ThinkingLevelChange(ThinkingLevelChangeEntry),
    #[serde(rename = "session_info")]
    SessionInfo(SessionInfoEntry),
    #[serde(rename = "compaction")]
    Compaction(CompactionEntry),
    #[serde(rename = "branch_summary")]
    BranchSummary(BranchSummaryEntry),
    #[serde(rename = "custom")]
    Custom(CustomEntry),
    #[serde(rename = "custom_message")]
    CustomMessage(CustomMessageEntry),
    #[serde(rename = "label")]
    Label(LabelEntry),
}

impl Drift for KnownRecord {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        match self {
            KnownRecord::Session(entry) => note_unknown(&entry.unknown, prefix, out),
            KnownRecord::Message(entry) => entry.drift(prefix, out),
            KnownRecord::ModelChange(entry) => note_unknown(&entry.unknown, prefix, out),
            KnownRecord::ThinkingLevelChange(entry) => note_unknown(&entry.unknown, prefix, out),
            KnownRecord::SessionInfo(entry) => note_unknown(&entry.unknown, prefix, out),
            KnownRecord::Compaction(entry) => note_unknown(&entry.unknown, prefix, out),
            KnownRecord::BranchSummary(entry) => note_unknown(&entry.unknown, prefix, out),
            KnownRecord::Custom(entry) => note_unknown(&entry.unknown, prefix, out),
            KnownRecord::CustomMessage(entry) => entry.drift(prefix, out),
            KnownRecord::Label(entry) => note_unknown(&entry.unknown, prefix, out),
        }
    }
}

/// First line of the file. Not part of the id/parentId tree.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionHeader {
    /// Omitted on v1 files; present (3) on current writers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<u64>,
    pub id: String,
    pub timestamp: String,
    pub cwd: String,
    /// Absolute path of the session this one was forked from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session: Option<String>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

/// Conversation message on the session tree.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageEntry {
    pub id: String,
    /// `null` on a root entry; otherwise the parent id.
    pub parent_id: Option<String>,
    pub timestamp: String,
    pub message: AgentMessage,
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl Drift for MessageEntry {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        note_unknown(&self.unknown, prefix, out);
        self.message.drift(&format!("{prefix}.message"), out);
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(untagged)]
#[allow(clippy::large_enum_variant)]
pub enum AgentMessage {
    Known(KnownMessage),
    Other(Value),
}

impl Drift for AgentMessage {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        match self {
            AgentMessage::Known(message) => message.drift(prefix, out),
            AgentMessage::Other(value) => {
                out.push(format!(
                    "{prefix}<unknown role={}>",
                    value["role"].as_str().unwrap_or("?")
                ));
            }
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "role")]
#[allow(clippy::large_enum_variant)]
pub enum KnownMessage {
    #[serde(rename = "user")]
    User(UserMessage),
    #[serde(rename = "assistant")]
    Assistant(AssistantMessage),
    #[serde(rename = "toolResult")]
    ToolResult(ToolResultMessage),
    #[serde(rename = "bashExecution")]
    BashExecution(BashExecutionMessage),
    #[serde(rename = "custom")]
    Custom(CustomRoleMessage),
    #[serde(rename = "branchSummary")]
    BranchSummary(BranchSummaryMessage),
    #[serde(rename = "compactionSummary")]
    CompactionSummary(CompactionSummaryMessage),
}

impl Drift for KnownMessage {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        match self {
            KnownMessage::User(message) => message.drift(prefix, out),
            KnownMessage::Assistant(message) => message.drift(prefix, out),
            KnownMessage::ToolResult(message) => message.drift(prefix, out),
            KnownMessage::BashExecution(message) => note_unknown(&message.unknown, prefix, out),
            KnownMessage::Custom(message) => message.drift(prefix, out),
            KnownMessage::BranchSummary(message) => note_unknown(&message.unknown, prefix, out),
            KnownMessage::CompactionSummary(message) => note_unknown(&message.unknown, prefix, out),
        }
    }
}

/// Writer: `string | (TextContent | ImageContent)[]`.
#[derive(Debug, Deserialize, Serialize)]
#[serde(untagged)]
pub enum UserContent {
    Text(String),
    Blocks(Vec<ContentBlock>),
}

impl Drift for UserContent {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        if let UserContent::Blocks(blocks) = self {
            for (index, block) in blocks.iter().enumerate() {
                block.drift(&format!("{prefix}[{index}]"), out);
            }
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct UserMessage {
    pub content: UserContent,
    pub timestamp: u64,
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl Drift for UserMessage {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        note_unknown(&self.unknown, prefix, out);
        self.content.drift(&format!("{prefix}.content"), out);
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantMessage {
    pub content: Vec<ContentBlock>,
    pub api: String,
    pub provider: String,
    pub model: String,
    pub usage: Usage,
    pub stop_reason: String,
    pub timestamp: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_stop_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_turn: Option<bool>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl Drift for AssistantMessage {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        note_unknown(&self.unknown, prefix, out);
        note_unknown(&self.usage.unknown, &format!("{prefix}.usage"), out);
        note_unknown(
            &self.usage.cost.unknown,
            &format!("{prefix}.usage.cost"),
            out,
        );
        for (index, block) in self.content.iter().enumerate() {
            block.drift(&format!("{prefix}.content[{index}]"), out);
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolResultMessage {
    pub tool_call_id: String,
    pub tool_name: String,
    pub content: Vec<ContentBlock>,
    pub is_error: bool,
    pub timestamp: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl Drift for ToolResultMessage {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        note_unknown(&self.unknown, prefix, out);
        for (index, block) in self.content.iter().enumerate() {
            block.drift(&format!("{prefix}.content[{index}]"), out);
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BashExecutionMessage {
    pub command: String,
    pub output: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<u64>,
    pub cancelled: bool,
    pub truncated: bool,
    pub timestamp: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_output_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclude_from_context: Option<bool>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomRoleMessage {
    pub custom_type: String,
    pub content: UserContent,
    pub display: bool,
    pub timestamp: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl Drift for CustomRoleMessage {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        note_unknown(&self.unknown, prefix, out);
        self.content.drift(&format!("{prefix}.content"), out);
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchSummaryMessage {
    pub summary: String,
    pub from_id: String,
    pub timestamp: u64,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionSummaryMessage {
    pub summary: String,
    pub tokens_before: u64,
    pub timestamp: u64,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(untagged)]
pub enum ContentBlock {
    Known(KnownContentBlock),
    Other(Value),
}

impl Drift for ContentBlock {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        match self {
            ContentBlock::Known(
                KnownContentBlock::Text { unknown, .. }
                | KnownContentBlock::Thinking { unknown, .. }
                | KnownContentBlock::Image { unknown, .. }
                | KnownContentBlock::ToolCall { unknown, .. },
            ) => note_unknown(unknown, prefix, out),
            ContentBlock::Other(value) => {
                out.push(format!(
                    "{prefix}<unknown block type={}>",
                    value["type"].as_str().unwrap_or("?")
                ));
            }
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum KnownContentBlock {
    #[serde(rename = "text")]
    Text {
        text: String,
        #[serde(flatten)]
        unknown: Unknown,
    },
    #[serde(rename = "thinking")]
    Thinking {
        thinking: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        thinking_signature: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        redacted: Option<bool>,
        #[serde(flatten)]
        unknown: Unknown,
    },
    #[serde(rename = "image")]
    Image {
        data: String,
        mime_type: String,
        #[serde(flatten)]
        unknown: Unknown,
    },
    #[serde(rename = "toolCall")]
    ToolCall {
        id: String,
        name: String,
        arguments: Value,
        #[serde(flatten)]
        unknown: Unknown,
    },
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_1h: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<u64>,
    pub total_tokens: u64,
    pub cost: Cost,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Cost {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub total: u64,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelChangeEntry {
    pub id: String,
    pub parent_id: Option<String>,
    pub timestamp: String,
    pub provider: String,
    pub model_id: String,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThinkingLevelChangeEntry {
    pub id: String,
    pub parent_id: Option<String>,
    pub timestamp: String,
    pub thinking_level: String,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfoEntry {
    pub id: String,
    pub parent_id: Option<String>,
    pub timestamp: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionEntry {
    pub id: String,
    pub parent_id: Option<String>,
    pub timestamp: String,
    pub summary: String,
    pub first_kept_entry_id: String,
    pub tokens_before: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_hook: Option<bool>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchSummaryEntry {
    pub id: String,
    pub parent_id: Option<String>,
    pub timestamp: String,
    pub from_id: String,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_hook: Option<bool>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomEntry {
    pub id: String,
    pub parent_id: Option<String>,
    pub timestamp: String,
    pub custom_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomMessageEntry {
    pub id: String,
    pub parent_id: Option<String>,
    pub timestamp: String,
    pub custom_type: String,
    pub content: UserContent,
    pub display: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl Drift for CustomMessageEntry {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        note_unknown(&self.unknown, prefix, out);
        self.content.drift(&format!("{prefix}.content"), out);
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LabelEntry {
    pub id: String,
    pub parent_id: Option<String>,
    pub timestamp: String,
    pub target_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(flatten)]
    pub unknown: Unknown,
}
