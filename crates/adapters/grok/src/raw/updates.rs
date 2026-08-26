//! Raw types for the `updates` stream
//! (`sessions/<encoded-cwd>/<uuid>/updates.jsonl`).
//!
//! Evidence: fixtures/1.0.5. Envelope {timestamp, method, params}. `method` is
//! `session/update` or `_x.ai/session/update`. The inner discriminator is
//! `params.update.sessionUpdate`. Payloads churn (tool_call vs tool_call_update
//! field sets differ), so the update body is a permissive all-optional struct.

use rosetta_adapter_support::{Drift, Unknown, note_unknown};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A field that is absent on most update variants but may be explicitly null
/// on the variant that declares it. `Option<Value>` cannot distinguish those
/// two cases during serde round-trip (`null` also becomes `None`).
#[derive(Debug, Default)]
pub enum MaybeValue {
    #[default]
    Absent,
    Present(Value),
}

impl MaybeValue {
    fn is_absent(&self) -> bool {
        matches!(self, Self::Absent)
    }
}

impl<'de> Deserialize<'de> for MaybeValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Value::deserialize(deserializer).map(Self::Present)
    }
}

impl Serialize for MaybeValue {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::Present(value) => value.serialize(serializer),
            Self::Absent => Err(serde::ser::Error::custom(
                "absent MaybeValue must be skipped",
            )),
        }
    }
}

/// Outcome of parsing one updates.jsonl line.
#[derive(Debug)]
pub enum ParsedLine {
    Known(Box<UpdateLine>),
    /// An envelope `method` this schema version does not know — soft drift.
    UnknownType {
        rtype: Option<String>,
        value: Value,
    },
}

/// Parse one line of an updates JSONL file.
pub fn parse_line(line: &str) -> Result<ParsedLine, serde_json::Error> {
    let value: Value = serde_json::from_str(line)?;
    let rtype = value["method"].as_str().map(str::to_owned);
    let known = matches!(
        rtype.as_deref(),
        Some("session/update" | "_x.ai/session/update")
    );
    if known {
        Ok(ParsedLine::Known(Box::new(serde_json::from_value(value)?)))
    } else {
        Ok(ParsedLine::UnknownType { rtype, value })
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct UpdateLine {
    pub timestamp: i64,
    pub method: String,
    pub params: UpdateParams,
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl Drift for UpdateLine {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        note_unknown(&self.unknown, prefix, out);
        self.params.drift(&format!("{prefix}.params"), out);
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct UpdateParams {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    pub update: SessionUpdate,
    #[serde(rename = "_meta", default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<Value>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl Drift for UpdateParams {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        note_unknown(&self.unknown, prefix, out);
        self.update.drift(&format!("{prefix}.update"), out);
    }
}

/// Open-vocabulary ACP/xAI session update. Tool I/O stays [`Value`].
#[derive(Debug, Deserialize, Serialize)]
pub struct SessionUpdate {
    #[serde(rename = "sessionUpdate")]
    pub session_update: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<Value>,
    #[serde(
        rename = "toolCallId",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locations: Option<Value>,
    #[serde(rename = "rawInput", default, skip_serializing_if = "Option::is_none")]
    pub raw_input: Option<Value>,
    #[serde(rename = "rawOutput", default, skip_serializing_if = "Option::is_none")]
    pub raw_output: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Value>,
    #[serde(rename = "_meta", default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runs: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entries: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_prompt_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_context_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resumed_from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub percentage: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens_used: Option<u64>,
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub update_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_retries: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub will_wake: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(
        rename = "tool_call_id",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub tool_call_id_snake: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_snapshot: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "MaybeValue::is_absent")]
    pub summary_preview: MaybeValue,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens_after: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens_before: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint_file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_index_at_compaction: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema_version: Option<u64>,
    #[serde(
        rename = "currentModeId",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub current_mode_id: Option<String>,
    #[serde(rename = "auto", default, skip_serializing_if = "Option::is_none")]
    pub auto_flag: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub images: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monitor_description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_result: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub objective: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turns: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub classifier_max_runs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub classifier_runs_attempted: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_deliverables: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_subagent_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_classifier_details_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_classifier_verdict: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_event: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_event_detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_event_timestamp: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live_context_pct: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live_subagent_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live_tool_call_count: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live_turn_count: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub planning: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_baseline: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_deliverables: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_verify_rounds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_worker_rounds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verifying_completion: Option<bool>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl Drift for SessionUpdate {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        note_unknown(&self.unknown, prefix, out);
    }
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::{ParsedLine, parse_line};

    #[test]
    fn explicit_null_variant_field_round_trips() {
        let text = r#"{"timestamp":1,"method":"session/update","params":{"sessionId":"s","_meta":{"eventId":"e","agentTimestampMs":1000},"update":{"sessionUpdate":"auto_compact_completed","elapsed_ms":1,"summary_preview":null,"tokens_after":2,"tokens_before":3}}}"#;
        let original: Value = serde_json::from_str(text).unwrap();
        let ParsedLine::Known(record) = parse_line(text).unwrap() else {
            panic!("declared update should parse");
        };
        assert_eq!(serde_json::to_value(record).unwrap(), original);
    }
}
