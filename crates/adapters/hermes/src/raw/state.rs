//! Raw types for rows extracted from the `state-db` snapshot stream.
//!
//! Evidence: fixtures/0.20.5 (scenarios basic-chat, tool-use, resume),
//! schema_version = 26. Primary tables typed below: sessions, messages,
//! session_model_usage, schema_version, state_meta, system_prompts.
//! FTS shadow tables and empty bookkeeping tables parse as
//! `ParsedRow::UnknownTable` (soft) until a scenario exercises them.

use rosetta_adapter_support::{Drift, Unknown, note_unknown};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Outcome of parsing one snapshot row (or a farm JSONL dump of that row).
#[derive(Debug)]
pub enum ParsedRow {
    Known(Box<KnownRecord>),
    UnknownTable { table: String, value: Value },
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(untagged)]
#[allow(clippy::large_enum_variant)]
pub enum KnownRecord {
    Session(SessionRow),
    Message(MessageRow),
    SessionModelUsage(SessionModelUsageRow),
    SchemaVersion(SchemaVersionRow),
    StateMeta(StateMetaRow),
    SystemPrompt(SystemPromptRow),
}

impl Drift for KnownRecord {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        match self {
            KnownRecord::Session(row) => note_unknown(&row.unknown, prefix, out),
            KnownRecord::Message(row) => note_unknown(&row.unknown, prefix, out),
            KnownRecord::SessionModelUsage(row) => note_unknown(&row.unknown, prefix, out),
            KnownRecord::SchemaVersion(row) => note_unknown(&row.unknown, prefix, out),
            KnownRecord::StateMeta(row) => note_unknown(&row.unknown, prefix, out),
            KnownRecord::SystemPrompt(row) => note_unknown(&row.unknown, prefix, out),
        }
    }
}

const PRIMARY_TABLES: &[&str] = &[
    "sessions",
    "messages",
    "session_model_usage",
    "schema_version",
    "state_meta",
    "system_prompts",
];

/// Parse one row of a Hermes `state.db` table.
pub fn parse_row(table: &str, value: Value) -> Result<ParsedRow, serde_json::Error> {
    if !PRIMARY_TABLES.contains(&table) {
        return Ok(ParsedRow::UnknownTable {
            table: table.to_owned(),
            value,
        });
    }
    let known = match table {
        "sessions" => KnownRecord::Session(serde_json::from_value(value)?),
        "messages" => KnownRecord::Message(serde_json::from_value(value)?),
        "session_model_usage" => KnownRecord::SessionModelUsage(serde_json::from_value(value)?),
        "schema_version" => KnownRecord::SchemaVersion(serde_json::from_value(value)?),
        "state_meta" => KnownRecord::StateMeta(serde_json::from_value(value)?),
        "system_prompts" => KnownRecord::SystemPrompt(serde_json::from_value(value)?),
        _ => unreachable!(),
    };
    Ok(ParsedRow::Known(Box::new(known)))
}

#[derive(Debug, Deserialize, Serialize)]
pub struct SessionRow {
    pub id: String,
    pub source: String,
    pub user_id: Option<String>,
    pub session_key: Option<String>,
    pub chat_id: Option<String>,
    pub chat_type: Option<String>,
    pub thread_id: Option<String>,
    pub display_name: Option<String>,
    pub origin_json: Option<String>,
    pub expiry_finalized: Option<i64>,
    pub model: Option<String>,
    /// JSON object stored as TEXT.
    pub model_config: Option<String>,
    pub system_prompt: Option<String>,
    pub system_prompt_hash: Option<String>,
    pub parent_session_id: Option<String>,
    pub started_at: f64,
    pub ended_at: Option<f64>,
    pub end_reason: Option<String>,
    pub message_count: Option<i64>,
    pub tool_call_count: Option<i64>,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub cache_read_tokens: Option<i64>,
    pub cache_write_tokens: Option<i64>,
    pub reasoning_tokens: Option<i64>,
    pub cwd: Option<String>,
    pub git_branch: Option<String>,
    pub git_repo_root: Option<String>,
    pub git_metadata_generation: i64,
    pub billing_provider: Option<String>,
    pub billing_base_url: Option<String>,
    pub billing_mode: Option<String>,
    pub estimated_cost_usd: Option<f64>,
    pub actual_cost_usd: Option<f64>,
    pub cost_status: Option<String>,
    pub cost_source: Option<String>,
    pub pricing_version: Option<String>,
    pub title: Option<String>,
    pub title_source: Option<String>,
    pub last_activity_at: Option<f64>,
    pub last_activity_description: Option<String>,
    pub last_activity_provenance: Option<String>,
    pub last_read_at: Option<f64>,
    pub api_call_count: Option<i64>,
    pub handoff_state: Option<String>,
    pub handoff_platform: Option<String>,
    pub handoff_error: Option<String>,
    pub compression_failure_cooldown_until: Option<f64>,
    pub compression_failure_error: Option<String>,
    pub compression_fallback_streak: i64,
    pub compression_ineffective_count: i64,
    pub profile_name: Option<String>,
    pub rewind_count: i64,
    pub archived: i64,
    pub pinned: i64,
    pub hidden: i64,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct MessageRow {
    pub id: i64,
    pub session_id: String,
    pub role: String,
    pub content: Option<String>,
    pub tool_call_id: Option<String>,
    /// JSON-encoded list of tool-call objects, stored as TEXT.
    pub tool_calls: Option<String>,
    pub tool_name: Option<String>,
    pub effect_disposition: Option<String>,
    pub timestamp: f64,
    pub token_count: Option<i64>,
    pub finish_reason: Option<String>,
    pub reasoning: Option<String>,
    pub reasoning_content: Option<String>,
    pub reasoning_details: Option<String>,
    pub codex_reasoning_items: Option<String>,
    pub codex_message_items: Option<String>,
    pub platform_message_id: Option<String>,
    pub observed: Option<i64>,
    pub active: i64,
    pub compacted: i64,
    pub api_content: Option<String>,
    pub display_kind: Option<String>,
    pub display_metadata: Option<String>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct SessionModelUsageRow {
    pub session_id: String,
    pub model: String,
    pub billing_provider: String,
    pub billing_base_url: String,
    pub billing_mode: String,
    pub task: String,
    pub api_call_count: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_write_tokens: i64,
    pub reasoning_tokens: i64,
    pub estimated_cost_usd: f64,
    pub actual_cost_usd: f64,
    pub cost_status: Option<String>,
    pub cost_source: Option<String>,
    pub first_seen: Option<f64>,
    pub last_seen: Option<f64>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct SchemaVersionRow {
    pub version: i64,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct StateMetaRow {
    pub key: String,
    pub value: Option<String>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct SystemPromptRow {
    pub hash: String,
    pub prompt: String,
    #[serde(flatten)]
    pub unknown: Unknown,
}

/// One OpenAI-style function call inside `messages.tool_calls`, which Hermes
/// stores as a JSON-encoded TEXT array.
#[derive(Debug, Deserialize, Serialize)]
pub struct ToolCallRow {
    pub id: String,
    pub call_id: String,
    pub response_item_id: String,
    #[serde(rename = "type")]
    pub call_type: String,
    pub function: ToolFunctionRow,
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl Drift for ToolCallRow {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        note_unknown(&self.unknown, prefix, out);
        note_unknown(&self.function.unknown, &format!("{prefix}.function"), out);
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ToolFunctionRow {
    pub name: String,
    /// JSON encoded a second time by the OpenAI-compatible wire shape.
    pub arguments: String,
    #[serde(flatten)]
    pub unknown: Unknown,
}

/// Fixture-backed terminal result payload stored in a tool-role message's
/// `content` TEXT column.
#[derive(Debug, Deserialize, Serialize)]
pub struct ToolResultRow {
    pub output: String,
    pub exit_code: i64,
    /// Required but nullable in the committed successful result.
    pub error: Option<Value>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl Drift for ToolResultRow {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        note_unknown(&self.unknown, prefix, out);
    }
}
