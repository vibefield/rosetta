//! Raw types for JSON blobs stored in `opencode.db`.
//!
//! Evidence: THE CANONICAL `groundtruth.json` (iron rule #6) — fixtures at
//! 1.18.22 (basic-chat, tool-use, resume). Conversational truth lives in
//! `message.data` (role-tagged) and `part.data` (type-tagged). Table columns
//! themselves are covered by the SQLite schema dump; credential/account tables
//! are inventoried and MUST-NOT-PARSE.
//!
//! Parsing policy (CLAUDE.md): permissive on fields — flatten-unknown maps
//! everywhere, non-empty = drift. Unknown top-level discriminants are soft
//! drift. A known role/type that fails to parse is a hard error. Tool `state`
//! stays `Value`; do not narrow by tool.

use rosetta_adapter_support::{Drift, Unknown, note_unknown};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Outcome of parsing one `message.data` blob.
#[derive(Debug)]
pub enum ParsedMessage {
    Known(Box<MessageData>),
    UnknownType { role: Option<String>, value: Value },
}

/// Outcome of parsing one `part.data` blob.
#[derive(Debug)]
pub enum ParsedPart {
    Known(Box<PartData>),
    UnknownType { rtype: Option<String>, value: Value },
}

/// Parse the JSON stored in `message.data`.
pub fn parse_message(json: &str) -> Result<ParsedMessage, serde_json::Error> {
    let value: Value = serde_json::from_str(json)?;
    let role = value["role"].as_str().map(str::to_owned);
    let known = matches!(role.as_deref(), Some("user" | "assistant"));
    if known {
        Ok(ParsedMessage::Known(Box::new(serde_json::from_value(
            value,
        )?)))
    } else {
        Ok(ParsedMessage::UnknownType { role, value })
    }
}

/// Parse the JSON stored in `part.data`.
pub fn parse_part(json: &str) -> Result<ParsedPart, serde_json::Error> {
    let value: Value = serde_json::from_str(json)?;
    let rtype = value["type"].as_str().map(str::to_owned);
    let known = matches!(
        rtype.as_deref(),
        Some("text" | "tool" | "step-start" | "step-finish")
    );
    if known {
        Ok(ParsedPart::Known(Box::new(serde_json::from_value(value)?)))
    } else {
        Ok(ParsedPart::UnknownType { rtype, value })
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "role")]
#[allow(clippy::large_enum_variant)]
pub enum MessageData {
    #[serde(rename = "user")]
    User(UserMessage),
    #[serde(rename = "assistant")]
    Assistant(AssistantMessage),
}

impl Drift for MessageData {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        match self {
            MessageData::User(message) => message.drift(prefix, out),
            MessageData::Assistant(message) => message.drift(prefix, out),
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct UserMessage {
    pub agent: String,
    pub model: UserModel,
    pub summary: UserSummary,
    pub time: CreatedTime,
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl Drift for UserMessage {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        note_unknown(&self.unknown, prefix, out);
        note_unknown(&self.model.unknown, &format!("{prefix}.model"), out);
        note_unknown(&self.summary.unknown, &format!("{prefix}.summary"), out);
        note_unknown(&self.time.unknown, &format!("{prefix}.time"), out);
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct UserModel {
    #[serde(rename = "providerID")]
    pub provider_id: String,
    #[serde(rename = "modelID")]
    pub model_id: String,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct UserSummary {
    pub diffs: Vec<Value>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct CreatedTime {
    pub created: u64,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct AssistantMessage {
    pub agent: String,
    pub cost: serde_json::Number,
    pub finish: String,
    pub mode: String,
    #[serde(rename = "modelID")]
    pub model_id: String,
    #[serde(rename = "parentID")]
    pub parent_id: String,
    pub path: AssistantPath,
    #[serde(rename = "providerID")]
    pub provider_id: String,
    pub time: CompletedTime,
    pub tokens: Tokens,
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl Drift for AssistantMessage {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        note_unknown(&self.unknown, prefix, out);
        note_unknown(&self.path.unknown, &format!("{prefix}.path"), out);
        note_unknown(&self.time.unknown, &format!("{prefix}.time"), out);
        self.tokens.drift(&format!("{prefix}.tokens"), out);
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct AssistantPath {
    pub cwd: String,
    pub root: String,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct CompletedTime {
    pub created: u64,
    pub completed: u64,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Tokens {
    pub total: u64,
    pub input: u64,
    pub output: u64,
    pub reasoning: u64,
    pub cache: CacheTokens,
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl Drift for Tokens {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        note_unknown(&self.unknown, prefix, out);
        note_unknown(&self.cache.unknown, &format!("{prefix}.cache"), out);
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct CacheTokens {
    pub read: u64,
    pub write: u64,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type")]
pub enum PartData {
    #[serde(rename = "text")]
    Text(TextPart),
    #[serde(rename = "tool")]
    Tool(ToolPart),
    #[serde(rename = "step-start")]
    StepStart(StepStartPart),
    #[serde(rename = "step-finish")]
    StepFinish(StepFinishPart),
}

impl Drift for PartData {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        match self {
            PartData::Text(part) => part.drift(prefix, out),
            PartData::Tool(part) => note_unknown(&part.unknown, prefix, out),
            PartData::StepStart(part) => note_unknown(&part.unknown, prefix, out),
            PartData::StepFinish(part) => part.drift(prefix, out),
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct TextPart {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<SpanTime>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl Drift for TextPart {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        note_unknown(&self.unknown, prefix, out);
        if let Some(time) = &self.time {
            note_unknown(&time.unknown, &format!("{prefix}.time"), out);
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct SpanTime {
    pub start: u64,
    pub end: u64,
    #[serde(flatten)]
    pub unknown: Unknown,
}

/// Tool call/result envelope. `state` holds input, output, and metadata —
/// kept untyped so a new tool never needs a parser change.
#[derive(Debug, Deserialize, Serialize)]
pub struct ToolPart {
    pub tool: String,
    #[serde(rename = "callID")]
    pub call_id: String,
    pub state: Value,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct StepStartPart {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<String>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct StepFinishPart {
    pub cost: serde_json::Number,
    pub reason: String,
    pub tokens: Tokens,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<String>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl Drift for StepFinishPart {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        note_unknown(&self.unknown, prefix, out);
        self.tokens.drift(&format!("{prefix}.tokens"), out);
    }
}
