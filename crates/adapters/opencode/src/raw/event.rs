//! Raw types for `event.data` JSON blobs.
//!
//! Evidence: fixtures/1.18.22. The `event.type` column is the discriminant;
//! the blob itself has no `type` field. Event payloads are streaming snapshots
//! — they do not match stored `message`/`part`/`session` rows field-for-field
//! (assistant `info` often lacks `finish`/`total`; tool parts may be `pending`).

use rosetta_adapter_support::{Drift, Unknown, note_unknown};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::payload::{AssistantPath, UserModel, UserSummary};

/// Outcome of parsing one `event.data` blob together with `event.type`.
#[derive(Debug)]
pub enum ParsedEvent {
    Known(Box<EventData>),
    UnknownType { rtype: Option<String>, value: Value },
}

/// Parse `event.data` using the SQL `type` column as the discriminant.
pub fn parse_event(event_type: &str, json: &str) -> Result<ParsedEvent, serde_json::Error> {
    let value: Value = serde_json::from_str(json)?;
    let known = matches!(
        event_type,
        "session.created.1" | "session.updated.1" | "message.updated.1" | "message.part.updated.1"
    );
    if !known {
        return Ok(ParsedEvent::UnknownType {
            rtype: Some(event_type.to_owned()),
            value,
        });
    }
    Ok(ParsedEvent::Known(Box::new(serde_json::from_value(value)?)))
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(untagged)]
#[allow(clippy::large_enum_variant)]
pub enum EventData {
    PartUpdated(PartUpdatedEvent),
    MessageUpdated(MessageUpdatedEvent),
    SessionLifecycle(SessionLifecycleEvent),
}

impl Drift for EventData {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        match self {
            EventData::PartUpdated(event) => event.drift(prefix, out),
            EventData::MessageUpdated(event) => event.drift(prefix, out),
            EventData::SessionLifecycle(event) => event.drift(prefix, out),
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct SessionLifecycleEvent {
    #[serde(rename = "sessionID")]
    pub session_id: String,
    pub info: SessionEventInfo,
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl Drift for SessionLifecycleEvent {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        note_unknown(&self.unknown, prefix, out);
        self.info.drift(&format!("{prefix}.info"), out);
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct SessionEventInfo {
    pub id: String,
    pub slug: String,
    #[serde(rename = "projectID")]
    pub project_id: String,
    pub directory: String,
    pub path: String,
    pub cost: serde_json::Number,
    pub tokens: EventTokens,
    pub title: String,
    pub version: String,
    pub time: SessionEventTime,
    pub permission: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<SessionModel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<SessionSummary>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl Drift for SessionEventInfo {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        note_unknown(&self.unknown, prefix, out);
        note_unknown(&self.tokens.unknown, &format!("{prefix}.tokens"), out);
        note_unknown(
            &self.tokens.cache.unknown,
            &format!("{prefix}.tokens.cache"),
            out,
        );
        note_unknown(&self.time.unknown, &format!("{prefix}.time"), out);
        if let Some(model) = &self.model {
            note_unknown(&model.unknown, &format!("{prefix}.model"), out);
        }
        if let Some(summary) = &self.summary {
            note_unknown(&summary.unknown, &format!("{prefix}.summary"), out);
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct SessionEventTime {
    pub created: u64,
    pub updated: u64,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct SessionModel {
    pub id: String,
    #[serde(rename = "providerID")]
    pub provider_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct SessionSummary {
    pub additions: u64,
    pub deletions: u64,
    pub files: u64,
    #[serde(flatten)]
    pub unknown: Unknown,
}

/// Token snapshot on session/message events — no `total` field (unlike stored parts).
#[derive(Debug, Deserialize, Serialize)]
pub struct EventTokens {
    pub input: u64,
    pub output: u64,
    pub reasoning: u64,
    pub cache: super::payload::CacheTokens,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct MessageUpdatedEvent {
    #[serde(rename = "sessionID")]
    pub session_id: String,
    pub info: MessageEventInfo,
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl Drift for MessageUpdatedEvent {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        note_unknown(&self.unknown, prefix, out);
        self.info.drift(&format!("{prefix}.info"), out);
    }
}

/// Streaming message snapshot. Requiredness is weaker than stored `message.data`.
#[derive(Debug, Deserialize, Serialize)]
pub struct MessageEventInfo {
    pub id: String,
    #[serde(rename = "sessionID")]
    pub session_id: String,
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<MessageEventTime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<UserModel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<UserSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "parentID")]
    pub parent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "modelID")]
    pub model_id: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "providerID"
    )]
    pub provider_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<AssistantPath>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<serde_json::Number>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<EventTokens>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish: Option<String>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct MessageEventTime {
    pub created: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed: Option<u64>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl Drift for MessageEventInfo {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        note_unknown(&self.unknown, prefix, out);
        if let Some(time) = &self.time {
            note_unknown(&time.unknown, &format!("{prefix}.time"), out);
        }
        if let Some(summary) = &self.summary {
            note_unknown(&summary.unknown, &format!("{prefix}.summary"), out);
        }
        if let Some(model) = &self.model {
            note_unknown(&model.unknown, &format!("{prefix}.model"), out);
        }
        if let Some(path) = &self.path {
            note_unknown(&path.unknown, &format!("{prefix}.path"), out);
        }
        if let Some(tokens) = &self.tokens {
            note_unknown(&tokens.unknown, &format!("{prefix}.tokens"), out);
            note_unknown(
                &tokens.cache.unknown,
                &format!("{prefix}.tokens.cache"),
                out,
            );
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct PartUpdatedEvent {
    #[serde(rename = "sessionID")]
    pub session_id: String,
    pub part: EventPart,
    pub time: u64,
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl Drift for PartUpdatedEvent {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        note_unknown(&self.unknown, prefix, out);
        note_unknown(&self.part.unknown, &format!("{prefix}.part"), out);
    }
}

/// Streaming part snapshot, including `pending` tool state not stored on the row.
#[derive(Debug, Deserialize, Serialize)]
pub struct EventPart {
    pub id: String,
    #[serde(rename = "sessionID")]
    pub session_id: String,
    #[serde(rename = "messageID")]
    pub message_id: String,
    #[serde(rename = "type")]
    pub part_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "callID")]
    pub call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<serde_json::Number>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<String>,
    #[serde(flatten)]
    pub unknown: Unknown,
}
