//! Raw types for the `chat-history` stream
//! (`sessions/<encoded-cwd>/<uuid>/chat_history.jsonl`).
//!
//! Evidence: fixtures/1.0.5 plus the 2026-08-25 structure census of organic
//! `~/.grok/sessions`. Fixture types: system, user, assistant, tool_result.
//! Census-only types: reasoning, backend_tool_call. Compaction may rewrite
//! the file.

use rosetta_adapter_support::{Drift, Unknown, note_unknown};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Outcome of parsing one chat_history line.
#[derive(Debug)]
pub enum ParsedLine {
    Known(Box<KnownRecord>),
    /// A record `type` this schema version does not know — soft drift.
    UnknownType {
        rtype: Option<String>,
        value: Value,
    },
}

/// Parse one line of a chat_history JSONL file.
pub fn parse_line(line: &str) -> Result<ParsedLine, serde_json::Error> {
    let value: Value = serde_json::from_str(line)?;
    let rtype = value["type"].as_str().map(str::to_owned);
    let known = matches!(
        rtype.as_deref(),
        Some("system" | "user" | "assistant" | "tool_result" | "reasoning" | "backend_tool_call")
    );
    if known {
        Ok(ParsedLine::Known(Box::new(serde_json::from_value(value)?)))
    } else {
        Ok(ParsedLine::UnknownType { rtype, value })
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type")]
pub enum KnownRecord {
    #[serde(rename = "system")]
    System(SystemLine),
    #[serde(rename = "user")]
    User(UserLine),
    #[serde(rename = "assistant")]
    Assistant(AssistantLine),
    #[serde(rename = "tool_result")]
    ToolResult(ToolResultLine),
    #[serde(rename = "reasoning")]
    Reasoning(ReasoningLine),
    #[serde(rename = "backend_tool_call")]
    BackendToolCall(BackendToolCallLine),
}

impl Drift for KnownRecord {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        match self {
            KnownRecord::System(line) => line.drift(prefix, out),
            KnownRecord::User(line) => line.drift(prefix, out),
            KnownRecord::Assistant(line) => line.drift(prefix, out),
            KnownRecord::ToolResult(line) => line.drift(prefix, out),
            KnownRecord::Reasoning(line) => note_unknown(&line.unknown, prefix, out),
            KnownRecord::BackendToolCall(line) => note_unknown(&line.unknown, prefix, out),
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct SystemLine {
    pub content: String,
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl Drift for SystemLine {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        note_unknown(&self.unknown, prefix, out);
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct UserLine {
    pub content: Vec<UserContentBlock>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_index: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub synthetic_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prior_turn_interrupt: Option<String>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl Drift for UserLine {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        note_unknown(&self.unknown, prefix, out);
        for (index, block) in self.content.iter().enumerate() {
            block.drift(&format!("{prefix}.content[{index}]"), out);
        }
    }
}

/// Untagged split: image (and other) blocks lack `text` and must not fail the line.
#[derive(Debug, Deserialize, Serialize)]
#[serde(untagged)]
pub enum UserContentBlock {
    Text(TextBlock),
    Other(Value),
}

impl Drift for UserContentBlock {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        match self {
            UserContentBlock::Text(block) => note_unknown(&block.unknown, prefix, out),
            UserContentBlock::Other(value) => out.push(format!(
                "{prefix}<unknown block type={}>",
                value["type"].as_str().unwrap_or("?")
            )),
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct TextBlock {
    #[serde(rename = "type")]
    pub block_type: String,
    pub text: String,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct AssistantLine {
    pub content: String,
    pub model_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl Drift for AssistantLine {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        note_unknown(&self.unknown, prefix, out);
        if let Some(calls) = &self.tool_calls {
            for (index, call) in calls.iter().enumerate() {
                note_unknown(&call.unknown, &format!("{prefix}.tool_calls[{index}]"), out);
            }
        }
    }
}

/// Tool call embedded on an assistant line. `arguments` is a JSON-encoded string.
#[derive(Debug, Deserialize, Serialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ToolResultLine {
    pub tool_call_id: String,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub images: Option<Value>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl Drift for ToolResultLine {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        note_unknown(&self.unknown, prefix, out);
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ReasoningLine {
    pub id: String,
    pub status: String,
    pub encrypted_content: String,
    pub summary: Value,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct BackendToolCallLine {
    pub kind: Value,
    #[serde(flatten)]
    pub unknown: Unknown,
}
