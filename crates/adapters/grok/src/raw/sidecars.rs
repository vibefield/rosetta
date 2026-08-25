//! Sidecar streams next to the conversation logs: signals, rewind points,
//! and per-cwd prompt history. Evidence: fixtures/1.0.5 plus the 2026-08-25
//! organic census.

use rosetta_adapter_support::{Drift, Unknown, note_unknown};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Signals {
    pub turn_count: u64,
    pub user_message_count: u64,
    pub assistant_message_count: u64,
    pub error_count: u64,
    pub tool_failure_count: u64,
    pub cancellation_count: u64,
    pub consecutive_cancellations: u64,
    pub regeneration_count: u64,
    pub compaction_count: u64,
    pub total_tokens_before_compaction: u64,
    pub context_window_usage: u64,
    pub context_tokens_used: u64,
    pub context_window_tokens: u64,
    pub tool_call_count: u64,
    pub edit_and_retry_count: u64,
    pub bash_bare_echo_count: u64,
    pub git_commit_count: u64,
    pub pr_created_count: u64,
    pub pr_merged_count: u64,
    pub inference_idle_timeouts: u64,
    pub doom_loop_recovery_attempts: u64,
    pub doom_loop_recovery_accepted_after_budget: u64,
    pub doom_loop_recovery_aborted_chunks: u64,
    pub inference_idle_timeout_configured_secs: u64,
    pub gcs_queue_enqueued: u64,
    pub gcs_queue_uploaded: u64,
    pub gcs_queue_failed: u64,
    pub gcs_queue_fallbacks: u64,
    pub gcs_queue_circuit_breaker_trips: u64,
    pub gcs_queue_pending: u64,
    pub gcs_queue_pending_bytes: u64,
    pub gcs_queue_orphans_cleaned: u64,
    pub positive_ratings: u64,
    pub negative_ratings: u64,
    pub long_pauses_count: u64,
    pub session_duration_seconds: u64,
    pub avg_time_to_first_token_ms: u64,
    pub avg_response_time_ms: u64,
    pub min_time_to_first_token_ms: u64,
    pub max_time_to_first_token_ms: u64,
    pub latency_sample_count: u64,
    pub itl_p50_ms: u64,
    pub itl_p99_ms: u64,
    pub itl_max_ms: u64,
    pub itl_mean_ms: u64,
    pub total_chunk_count: u64,
    pub itl_sample_count: u64,
    pub agent_lines_added: u64,
    pub agent_lines_removed: u64,
    pub agent_lines_added_reverted: u64,
    pub agent_lines_removed_reverted: u64,
    pub human_lines_added: u64,
    pub human_lines_removed: u64,
    pub human_lines_added_reverted: u64,
    pub human_lines_removed_reverted: u64,
    pub agent_files_touched: u64,
    pub human_files_touched: u64,
    pub total_files_touched: u64,
    pub peak_rss_bytes: u64,
    pub has_reverted: bool,
    pub tools_used: Value,
    pub models_used: Value,
    pub primary_model_id: String,
    /// Explicit null on the 1.0.5 farm corpus.
    pub doom_loop_recovery_top_trigger: Value,
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl Drift for Signals {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        note_unknown(&self.unknown, prefix, out);
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct RewindPoint {
    pub prompt_index: u64,
    pub created_at: String,
    pub file_snapshots: Value,
    pub after_snapshots: Value,
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl Drift for RewindPoint {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        note_unknown(&self.unknown, prefix, out);
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct PromptHistoryEntry {
    pub timestamp: String,
    pub session_id: String,
    pub prompt: String,
    pub is_bash: bool,
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl Drift for PromptHistoryEntry {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        note_unknown(&self.unknown, prefix, out);
    }
}

pub fn parse_signals(text: &str) -> Result<Signals, serde_json::Error> {
    serde_json::from_str(text)
}

pub fn parse_rewind_point(line: &str) -> Result<RewindPoint, serde_json::Error> {
    serde_json::from_str(line)
}

pub fn parse_prompt_history(line: &str) -> Result<PromptHistoryEntry, serde_json::Error> {
    serde_json::from_str(line)
}
