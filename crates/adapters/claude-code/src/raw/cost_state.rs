//! `transcript` → `cost-state`, as declared by the canonical groundtruth.

use std::collections::BTreeMap;

use rosetta_adapter_support::{Drift, Unknown, note_unknown};
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CostStateLine {
    pub session_id: String,
    #[serde(rename = "totalCostUSD")]
    pub total_cost_usd: f64,
    #[serde(rename = "totalAPIDuration")]
    pub total_api_duration: u64,
    #[serde(rename = "totalAPIDurationWithoutRetries")]
    pub total_api_duration_without_retries: u64,
    pub total_tool_duration: u64,
    pub total_lines_added: u64,
    pub total_lines_removed: u64,
    pub total_duration: u64,
    pub start_time: i64,
    pub has_unknown_model_cost: bool,
    pub model_usage: BTreeMap<String, ModelUsage>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_tokens: Option<u64>,
    pub cache_read_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub web_search_requests: u64,
    #[serde(rename = "costUSD")]
    pub cost_usd: f64,
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl Drift for CostStateLine {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        note_unknown(&self.unknown, prefix, out);
        for usage in self.model_usage.values() {
            note_unknown(
                &usage.unknown,
                &format!("{prefix}.modelUsage.<dynamic>"),
                out,
            );
        }
    }
}
