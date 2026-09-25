//! Raw types for 2.x `session_message.data` blobs and the durable
//! `session.usage.recorded.*` event payload.
//!
//! Evidence: THE CANONICAL `groundtruth.json` (iron rule #7) — the 2.0.14
//! writer schema (`packages/schema/src/session-message.ts` Info,
//! `token-usage.ts`, `prompt.ts`, `model.ts`, `session-event.ts`
//! UsageRecorded). The row's `type` column is the discriminant: `data` omits
//! `type` and `id` (`packages/core/src/session/sql.ts:19-20`). Assistant
//! `content` items stay `Value`s here and are typed one at a time by
//! [`parse_content`], so one unknown item never fails its message.
//!
//! Parsing policy (CLAUDE.md): permissive on fields — flatten-unknown maps
//! everywhere, non-empty = drift. Unknown discriminants are soft drift. A
//! known type that fails to parse is a hard error. Tool `state` stays
//! `Value`; do not narrow by tool.

use rosetta_adapter_support::{Drift, Unknown, note_unknown};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::payload::CacheTokens;

/// Outcome of parsing one `session_message.data` blob under its `type`.
#[derive(Debug)]
pub enum ParsedSessionMessage {
    Known(Box<SessionMessage>),
    UnknownType { rtype: String, value: Value },
}

/// Outcome of parsing one assistant `content[]` item.
#[derive(Debug)]
pub enum ParsedContent {
    Known(Box<AssistantContent>),
    UnknownType { rtype: Option<String>, value: Value },
}

pub fn parse_session_message(
    message_type: &str,
    json: &str,
) -> Result<ParsedSessionMessage, serde_json::Error> {
    let value: Value = serde_json::from_str(json)?;
    let known = match message_type {
        "user" => SessionMessage::User(serde_json::from_value(value)?),
        "assistant" => SessionMessage::Assistant(serde_json::from_value(value)?),
        "compaction" => SessionMessage::Compaction(serde_json::from_value(value)?),
        "synthetic" => SessionMessage::Synthetic(serde_json::from_value(value)?),
        "system" => SessionMessage::System(serde_json::from_value(value)?),
        "skill" => SessionMessage::Skill(serde_json::from_value(value)?),
        "shell" => SessionMessage::Shell(serde_json::from_value(value)?),
        "idle" => SessionMessage::Idle(serde_json::from_value(value)?),
        "agent-switched" => SessionMessage::AgentSwitched(serde_json::from_value(value)?),
        "model-switched" => SessionMessage::ModelSwitched(serde_json::from_value(value)?),
        "location-switched" => SessionMessage::LocationSwitched(serde_json::from_value(value)?),
        _ => {
            return Ok(ParsedSessionMessage::UnknownType {
                rtype: message_type.to_owned(),
                value,
            });
        }
    };
    Ok(ParsedSessionMessage::Known(Box::new(known)))
}

pub fn parse_content(value: &Value) -> Result<ParsedContent, serde_json::Error> {
    let rtype = value["type"].as_str().map(str::to_owned);
    let known = matches!(rtype.as_deref(), Some("text" | "reasoning" | "tool"));
    if known {
        Ok(ParsedContent::Known(Box::new(serde_json::from_value(
            value.clone(),
        )?)))
    } else {
        Ok(ParsedContent::UnknownType {
            rtype,
            value: value.clone(),
        })
    }
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum SessionMessage {
    User(User),
    Assistant(Assistant),
    Compaction(Compaction),
    Synthetic(TextMessage),
    System(TextMessage),
    Skill(SkillMessage),
    Shell(ShellMessage),
    Idle(IdleMessage),
    AgentSwitched(AgentSwitched),
    ModelSwitched(ModelSwitched),
    LocationSwitched(LocationSwitched),
}

impl SessionMessage {
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::User(_) => "user",
            Self::Assistant(_) => "assistant",
            Self::Compaction(_) => "compaction",
            Self::Synthetic(_) => "synthetic",
            Self::System(_) => "system",
            Self::Skill(_) => "skill",
            Self::Shell(_) => "shell",
            Self::Idle(_) => "idle",
            Self::AgentSwitched(_) => "agent-switched",
            Self::ModelSwitched(_) => "model-switched",
            Self::LocationSwitched(_) => "location-switched",
        }
    }

    pub fn time(&self) -> &MessageTime {
        match self {
            Self::User(message) => &message.time,
            Self::Assistant(message) => &message.time,
            Self::Compaction(message) => &message.time,
            Self::Synthetic(message) | Self::System(message) => &message.time,
            Self::Skill(message) => &message.time,
            Self::Shell(message) => &message.time,
            Self::Idle(message) => &message.time,
            Self::AgentSwitched(message) => &message.time,
            Self::ModelSwitched(message) => &message.time,
            Self::LocationSwitched(message) => &message.time,
        }
    }
}

impl Drift for SessionMessage {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        match self {
            Self::User(message) => message.drift(prefix, out),
            Self::Assistant(message) => message.drift(prefix, out),
            Self::Compaction(message) => message.drift(prefix, out),
            Self::Synthetic(message) | Self::System(message) => message.drift(prefix, out),
            Self::Skill(message) => message.drift(prefix, out),
            Self::Shell(message) => message.drift(prefix, out),
            Self::Idle(message) => message.drift(prefix, out),
            Self::AgentSwitched(message) => message.drift(prefix, out),
            Self::ModelSwitched(message) => message.drift(prefix, out),
            Self::LocationSwitched(message) => message.drift(prefix, out),
        }
    }
}

/// `Base.time` plus the assistant's `streamed`/`completed` and the shell's
/// `completed`; all UTC milliseconds.
#[derive(Debug, Deserialize, Serialize)]
pub struct MessageTime {
    pub created: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub streamed: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed: Option<u64>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct User {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files: Option<Vec<FileAttachment>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agents: Option<Vec<Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills: Option<Vec<Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
    pub time: MessageTime,
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl Drift for User {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        note_unknown(&self.unknown, prefix, out);
        note_unknown(&self.time.unknown, &format!("{prefix}.time"), out);
        for file in self.files.iter().flatten() {
            note_unknown(&file.unknown, &format!("{prefix}.files[]"), out);
        }
    }
}

/// `Prompt.FileAttachment`: inline base64 bytes with their mime type.
#[derive(Debug, Deserialize, Serialize)]
pub struct FileAttachment {
    pub data: String,
    pub mime: String,
    pub source: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mention: Option<Value>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

/// `synthetic` and `system` messages: model-facing text with a display
/// description.
#[derive(Debug, Deserialize, Serialize)]
pub struct TextMessage {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
    pub time: MessageTime,
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl Drift for TextMessage {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        note_unknown(&self.unknown, prefix, out);
        note_unknown(&self.time.unknown, &format!("{prefix}.time"), out);
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct SkillMessage {
    pub skill: String,
    pub name: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
    pub time: MessageTime,
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl Drift for SkillMessage {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        note_unknown(&self.unknown, prefix, out);
        note_unknown(&self.time.unknown, &format!("{prefix}.time"), out);
    }
}

/// A user-run `!command` shell message.
#[derive(Debug, Deserialize, Serialize)]
pub struct ShellMessage {
    #[serde(rename = "shellID")]
    pub shell_id: String,
    pub command: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit: Option<serde_json::Number>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
    pub time: MessageTime,
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl Drift for ShellMessage {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        note_unknown(&self.unknown, prefix, out);
        note_unknown(&self.time.unknown, &format!("{prefix}.time"), out);
    }
}

/// `Model.Ref`: the served model id under its provider.
#[derive(Debug, Deserialize, Serialize)]
pub struct ModelRef {
    pub id: String,
    #[serde(rename = "providerID")]
    pub provider_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

/// `TokenUsage.Info`, already exclusive: `input` is the non-cached prompt
/// and `output` the visible output (`session/usage.ts:11-19`).
#[derive(Debug, Deserialize, Serialize)]
pub struct TokenUsage {
    pub input: serde_json::Number,
    pub output: serde_json::Number,
    pub reasoning: serde_json::Number,
    pub cache: CacheTokens,
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl Drift for TokenUsage {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        note_unknown(&self.unknown, prefix, out);
        note_unknown(&self.cache.unknown, &format!("{prefix}.cache"), out);
    }
}

/// One step's assistant message: the whole message is one model request
/// (`session/message-updater.ts:213-255,261-285`).
#[derive(Debug, Deserialize, Serialize)]
pub struct Assistant {
    pub agent: String,
    pub model: ModelRef,
    pub content: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish: Option<String>,
    #[serde(rename = "rawFinish", default, skip_serializing_if = "Option::is_none")]
    pub raw_finish: Option<String>,
    #[serde(
        rename = "providerState",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub provider_state: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<serde_json::Number>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<TokenUsage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry: Option<Retry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
    pub time: MessageTime,
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl Drift for Assistant {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        note_unknown(&self.unknown, prefix, out);
        note_unknown(&self.model.unknown, &format!("{prefix}.model"), out);
        note_unknown(&self.time.unknown, &format!("{prefix}.time"), out);
        if let Some(tokens) = &self.tokens {
            tokens.drift(&format!("{prefix}.tokens"), out);
        }
        if let Some(retry) = &self.retry {
            note_unknown(&retry.unknown, &format!("{prefix}.retry"), out);
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Retry {
    pub attempt: u64,
    pub at: u64,
    pub error: Value,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type")]
pub enum AssistantContent {
    #[serde(rename = "text")]
    Text(ContentText),
    #[serde(rename = "reasoning")]
    Reasoning(ContentReasoning),
    #[serde(rename = "tool")]
    Tool(ContentTool),
}

impl Drift for AssistantContent {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        match self {
            Self::Text(item) => note_unknown(&item.unknown, prefix, out),
            Self::Reasoning(item) => {
                note_unknown(&item.unknown, prefix, out);
                if let Some(time) = &item.time {
                    note_unknown(&time.unknown, &format!("{prefix}.time"), out);
                }
            }
            Self::Tool(item) => {
                note_unknown(&item.unknown, prefix, out);
                note_unknown(&item.time.unknown, &format!("{prefix}.time"), out);
            }
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ContentText {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<Value>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ContentReasoning {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<MessageTime>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

/// Tool call/result envelope. `state` holds status, input, content, and
/// error — kept untyped so a new tool never needs a parser change.
#[derive(Debug, Deserialize, Serialize)]
pub struct ContentTool {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executed: Option<bool>,
    #[serde(
        rename = "providerState",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub provider_state: Option<Value>,
    #[serde(
        rename = "providerResultState",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub provider_result_state: Option<Value>,
    pub state: Value,
    pub time: ToolTime,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ToolTime {
    pub created: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ran: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed: Option<u64>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

/// The `compaction` union (`running` | `completed` | `failed`) flattened over
/// `status`; `tokens`/`cost` describe the compaction request itself.
#[derive(Debug, Deserialize, Serialize)]
pub struct Compaction {
    pub status: String,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelRef>,
    #[serde(
        rename = "providerState",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub provider_state: Option<Value>,
    #[serde(
        rename = "providerContext",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub provider_context: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<serde_json::Number>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<TokenUsage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
    pub time: MessageTime,
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl Drift for Compaction {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        note_unknown(&self.unknown, prefix, out);
        note_unknown(&self.time.unknown, &format!("{prefix}.time"), out);
        if let Some(model) = &self.model {
            note_unknown(&model.unknown, &format!("{prefix}.model"), out);
        }
        if let Some(tokens) = &self.tokens {
            tokens.drift(&format!("{prefix}.tokens"), out);
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct IdleMessage {
    pub outcome: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
    pub time: MessageTime,
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl Drift for IdleMessage {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        note_unknown(&self.unknown, prefix, out);
        note_unknown(&self.time.unknown, &format!("{prefix}.time"), out);
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct AgentSwitched {
    pub agent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
    pub time: MessageTime,
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl Drift for AgentSwitched {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        note_unknown(&self.unknown, prefix, out);
        note_unknown(&self.time.unknown, &format!("{prefix}.time"), out);
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ModelSwitched {
    pub model: ModelRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous: Option<ModelRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
    pub time: MessageTime,
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl Drift for ModelSwitched {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        note_unknown(&self.unknown, prefix, out);
        note_unknown(&self.model.unknown, &format!("{prefix}.model"), out);
        note_unknown(&self.time.unknown, &format!("{prefix}.time"), out);
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct LocationSwitched {
    pub location: Value,
    #[serde(rename = "projectID", default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subpath: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
    pub time: MessageTime,
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl Drift for LocationSwitched {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        note_unknown(&self.unknown, prefix, out);
        note_unknown(&self.time.unknown, &format!("{prefix}.time"), out);
    }
}

/// `session.usage.recorded.*` event data: a request recorded outside any
/// message, from title generation or compaction (`session-event.ts`
/// UsageRecorded; `session/title.ts:57-63`, `session/compaction.ts:438-443`).
#[derive(Debug, Deserialize, Serialize)]
pub struct UsageRecordedEvent {
    #[serde(rename = "sessionID")]
    pub session_id: String,
    pub source: String,
    pub cost: serde_json::Number,
    pub tokens: TokenUsage,
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl Drift for UsageRecordedEvent {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        note_unknown(&self.unknown, prefix, out);
        self.tokens.drift(&format!("{prefix}.tokens"), out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Writer-shaped rows (mock numbers, no content): every declared type
    /// parses with zero drift and round-trips losslessly.
    #[test]
    fn every_declared_message_type_parses_and_round_trips() {
        let rows = [
            ("user", json!({"text": "hi", "time": {"created": 1}})),
            (
                "user",
                json!({"text": "look", "files": [{"data": "AA==", "mime": "image/png",
                        "source": {"type": "inline"}, "name": "a.png"}],
                       "agents": [{"name": "build"}], "skills": [], "time": {"created": 2}}),
            ),
            (
                "assistant",
                json!({"agent": "build", "model": {"id": "m", "providerID": "p", "variant": "default"},
                       "content": [
                           {"type": "reasoning", "text": "t", "time": {"created": 3, "completed": 4}},
                           {"type": "tool", "id": "call_1", "name": "read",
                            "state": {"status": "completed", "input": {"path": "x"}, "content": [{"type": "text", "text": "y"}]},
                            "time": {"created": 5, "ran": 6, "completed": 7}},
                           {"type": "text", "text": "done", "state": {"k": 1}}
                       ],
                       "snapshot": {"start": "s1"}, "finish": "stop", "rawFinish": "end_turn",
                       "providerState": {"x": 1}, "cost": 0.01,
                       "tokens": {"input": 10, "output": 20, "reasoning": 5, "cache": {"read": 100, "write": 7}},
                       "retry": {"attempt": 1, "at": 8, "error": {"type": "e", "message": "m"}},
                       "metadata": {"a": 1}, "time": {"created": 3, "streamed": 8, "completed": 9}}),
            ),
            (
                "compaction",
                json!({"status": "completed", "reason": "auto", "summary": "s", "recent": "r",
                       "model": {"id": "m", "providerID": "p"}, "cost": 0.02,
                       "tokens": {"input": 1, "output": 2, "reasoning": 0, "cache": {"read": 0, "write": 0}},
                       "time": {"created": 10}}),
            ),
            (
                "compaction",
                json!({"status": "failed", "reason": "manual", "error": {"type": "compaction.failed", "message": "x"},
                       "time": {"created": 11}}),
            ),
            (
                "synthetic",
                json!({"text": "t", "description": "d", "time": {"created": 12}}),
            ),
            ("system", json!({"text": "t", "time": {"created": 13}})),
            (
                "skill",
                json!({"skill": "sk_1", "name": "n", "text": "t", "time": {"created": 14}}),
            ),
            (
                "shell",
                json!({"shellID": "sh_1", "command": "ls", "status": "completed", "exit": 0,
                       "output": {"text": "o"}, "time": {"created": 15, "completed": 16}}),
            ),
            (
                "idle",
                json!({"outcome": "succeeded", "time": {"created": 17}}),
            ),
            (
                "agent-switched",
                json!({"agent": "plan", "previous": "build", "time": {"created": 18}}),
            ),
            (
                "model-switched",
                json!({"model": {"id": "m", "providerID": "p"}, "previous": {"id": "n", "providerID": "p"},
                       "time": {"created": 19}}),
            ),
            (
                "location-switched",
                json!({"location": {"type": "local"}, "projectID": "pr", "subpath": "a/b", "time": {"created": 20}}),
            ),
        ];
        for (message_type, value) in rows {
            let ParsedSessionMessage::Known(message) =
                parse_session_message(message_type, &value.to_string()).unwrap()
            else {
                panic!("{message_type} is declared");
            };
            assert_eq!(message.type_name(), message_type);
            assert!(message.drift_paths().is_empty(), "{message_type}");
            assert_eq!(
                serde_json::to_value(&*message).unwrap(),
                value,
                "{message_type}"
            );
            if let SessionMessage::Assistant(assistant) = &*message {
                for item in &assistant.content {
                    let ParsedContent::Known(content) = parse_content(item).unwrap() else {
                        panic!("declared content");
                    };
                    assert!(content.drift_paths().is_empty());
                    assert_eq!(&serde_json::to_value(&*content).unwrap(), item);
                }
            }
        }
    }

    #[test]
    fn unknown_types_and_fields_are_soft() {
        assert!(matches!(
            parse_session_message("future", r#"{"time":{"created":1}}"#).unwrap(),
            ParsedSessionMessage::UnknownType { .. }
        ));
        assert!(parse_session_message("user", "{}").is_err());
        let ParsedSessionMessage::Known(message) = parse_session_message(
            "idle",
            r#"{"outcome":"failed","time":{"created":1},"why":1}"#,
        )
        .unwrap() else {
            panic!()
        };
        assert_eq!(message.drift_paths(), vec![".why"]);
        assert!(matches!(
            parse_content(&json!({"type": "image", "url": "x"})).unwrap(),
            ParsedContent::UnknownType { rtype: Some(rtype), .. } if rtype == "image"
        ));
    }

    #[test]
    fn usage_events_parse() {
        let event: UsageRecordedEvent = serde_json::from_value(json!({
            "sessionID": "ses_1", "source": "title", "cost": 0.001,
            "tokens": {"input": 50, "output": 8, "reasoning": 0, "cache": {"read": 0, "write": 0}}
        }))
        .unwrap();
        assert_eq!(event.source, "title");
        assert!(event.drift_paths().is_empty());
    }
}
