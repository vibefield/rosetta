//! Raw types for the `events` stream
//! (`sessions/<encoded-cwd>/<uuid>/events.jsonl`).
//!
//! Evidence: fixtures/1.0.5. `type` is an open vocabulary (observed:
//! turn_started, loop_started, phase_changed, first_token, turn_ended,
//! tool_started, tool_completed, permission_requested, permission_resolved).
//! All payload fields are optional so a new subtype parses; novel fields
//! surface as drift.

use rosetta_adapter_support::{Drift, Unknown, note_unknown};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Outcome of parsing one events.jsonl line.
#[derive(Debug)]
pub enum ParsedLine {
    Known(Box<EventLine>),
    /// A record `type` this schema version does not know — soft drift.
    UnknownType {
        rtype: Option<String>,
        value: Value,
    },
}

/// Parse one line of an events JSONL file.
pub fn parse_line(line: &str) -> Result<ParsedLine, serde_json::Error> {
    let value: Value = serde_json::from_str(line)?;
    let rtype = value["type"].as_str().map(str::to_owned);
    let known = matches!(
        rtype.as_deref(),
        Some(
            "turn_started"
                | "loop_started"
                | "phase_changed"
                | "first_token"
                | "turn_ended"
                | "tool_started"
                | "tool_completed"
                | "permission_requested"
                | "permission_resolved"
                | "mcp_server_starting"
                | "mcp_server_failed"
                | "mcp_server_connected"
                | "mcp_config_resolved"
                | "mcp_init_completed"
                | "mcp_managed_config_result"
                | "mcp_tool_call_started"
                | "mcp_tool_call_completed"
                | "mcp_oauth_discovery_timeout"
                | "goal_classifier_fired"
                | "goal_classifier_verdict"
                | "goal_planner_fired"
                | "goal_planner_completed"
                | "goal_summarizer_fired"
                | "goal_summarizer_completed"
                | "goal_verifier_skeptic_verdict"
                | "goal_verifier_aggregate_verdict"
        )
    );
    if known {
        Ok(ParsedLine::Known(Box::new(serde_json::from_value(value)?)))
    } else {
        Ok(ParsedLine::UnknownType { rtype, value })
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct EventLine {
    pub ts: String,
    #[serde(rename = "type")]
    pub event_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loop_index: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_number: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub yolo_mode: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation_message_count: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_relationship: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redirect_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancellation_category: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancellation_context: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_sec: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_count: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub servers: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_required: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed_servers: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_reinit: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub succeeded: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_servers: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tools: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_count: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub success: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_timeout: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_retry_attempted: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reconnect_attempted: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_runs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refuted: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skeptic_idx: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub achieved: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refuted_count: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl Drift for EventLine {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        note_unknown(&self.unknown, prefix, out);
    }
}
