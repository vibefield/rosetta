//! Raw type for `sessions/<cwd>/<uuid>/summary.json`.
//!
//! Evidence: fixtures/1.0.5 merged with the 2026-08-25 organic census.
//! Required fields are those present on every fixture *and* census record.

use rosetta_adapter_support::{Drift, Unknown, note_unknown};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Deserialize, Serialize)]
pub struct Summary {
    pub info: SessionInfo,
    pub session_summary: String,
    pub created_at: String,
    pub updated_at: String,
    pub num_messages: u64,
    pub num_chat_messages: u64,
    pub current_model_id: String,
    pub next_trace_turn: u64,
    pub chat_format_version: u64,
    pub grok_home: String,
    pub agent_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generated_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_active_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox_profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fork_context_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fork_parent_prompt_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forked_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_remotes: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_root_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_commit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inherited_prefix_len: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_recap: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_turn_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_turn_summary_prompt_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_kind: Option<String>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl Drift for Summary {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        note_unknown(&self.unknown, prefix, out);
        note_unknown(&self.info.unknown, &format!("{prefix}.info"), out);
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct SessionInfo {
    pub id: String,
    pub cwd: String,
    #[serde(flatten)]
    pub unknown: Unknown,
}

pub fn parse(text: &str) -> Result<Summary, serde_json::Error> {
    serde_json::from_str(text)
}
