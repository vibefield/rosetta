//! Raw types for the `session-state` stream (`sessions/*/*/state.json`).
//!
//! Evidence: THE CANONICAL `groundtruth.json` — fixtures at 0.38.0
//! (basic-chat, tool-use, resume, subagent, error) merged with an organic
//! census. `title` / `lastPrompt` / `titleKind` are organic-only.
//! Sub-agent entries add `parentAgentId` and `labels`.

use std::collections::BTreeMap;

use rosetta_adapter_support::{Drift, Unknown, note_unknown};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Parse a `state.json` document.
pub fn parse_document(text: &str) -> Result<SessionState, serde_json::Error> {
    serde_json::from_str(text)
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionState {
    pub id: String,
    pub version: u64,
    pub cwd: String,
    pub created_at: u64,
    pub updated_at: u64,
    pub archived: bool,
    pub agents: BTreeMap<String, SessionAgent>,
    pub custom: Value,
    pub last_turn_reason: String,
    pub is_custom_title: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_kind: Option<String>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl Drift for SessionState {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        note_unknown(&self.unknown, prefix, out);
        for (id, agent) in &self.agents {
            agent.drift(&format!("{prefix}.agents.{id}"), out);
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionAgent {
    pub homedir: String,
    #[serde(rename = "type")]
    pub agent_type: String,
    /// Present on sub-agents (`type = "sub"`); absent on `main`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub labels: Option<Value>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl Drift for SessionAgent {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        note_unknown(&self.unknown, prefix, out);
    }
}
