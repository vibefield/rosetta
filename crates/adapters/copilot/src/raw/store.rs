//! Raw types for the `session-store` snapshot stream
//! (`~/.copilot/session-store.db`, table `assistant_usage_events`).
//!
//! Evidence: THE CANONICAL `groundtruth.json` — the observed structure census
//! of a 1.0.80-written store (`sources/observed-2026-09-24.json`,
//! `schema_version` 6) merged with the runtime DDL in
//! `prebuilds/darwin-arm64/runtime.node` for 1.0.80 and 1.0.88. The 1.0.88
//! migration adds `copilot_usage_model` and `output_ttft_ms`. Rows arrive
//! from the shared SQLite driver as `{"query": name, "row": {...}}`; every
//! other table of the store is transcript-derived and is never queried.

use rosetta_adapter_support::{Drift, Unknown, note_unknown};
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

/// The one record query the snapshot plan declares.
pub const USAGE_EVENTS: &str = "assistant_usage_events";

/// The driver envelope around one query result row.
#[derive(Debug, Deserialize)]
pub struct SnapshotRecord {
    pub query: String,
    pub row: Value,
    #[serde(flatten)]
    pub unknown: Unknown,
}

/// One `assistant_usage_events` row: one model request as the runtime's
/// usage processor recorded it. Like the shutdown totals, `input_tokens`
/// includes cache reads and writes and `output_tokens` includes reasoning.
/// Every count is nullable in the DDL.
#[derive(Debug, Deserialize, Serialize)]
pub struct UsageEventRow {
    pub id: i64,
    pub session_id: String,
    pub model: String,
    /// 1.0.88: the billing model when CAPI reports one.
    #[serde(default)]
    pub copilot_usage_model: Option<String>,
    #[serde(default, deserialize_with = "lenient_count")]
    pub turn_index: Option<i64>,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub parent_tool_call_id: Option<String>,
    #[serde(default, deserialize_with = "lenient_count")]
    pub input_tokens: Option<i64>,
    #[serde(default, deserialize_with = "lenient_count")]
    pub output_tokens: Option<i64>,
    #[serde(default, deserialize_with = "lenient_count")]
    pub cache_read_tokens: Option<i64>,
    /// 1.0.88 folds 1-hour writes into this total (`token_details_json`
    /// still lists them separately).
    #[serde(default, deserialize_with = "lenient_count")]
    pub cache_write_tokens: Option<i64>,
    #[serde(default, deserialize_with = "lenient_count")]
    pub reasoning_tokens: Option<i64>,
    /// GitHub's metered charge: 10⁹ nano-AIU per AI credit ($0.01). Zero or
    /// null when GitHub did not meter the call (BYOK).
    #[serde(default, deserialize_with = "lenient_count")]
    pub total_nano_aiu: Option<i64>,
    #[serde(default)]
    pub request_multiplier: Option<f64>,
    #[serde(default, deserialize_with = "lenient_count")]
    pub duration_ms: Option<i64>,
    #[serde(default, deserialize_with = "lenient_count")]
    pub time_to_first_token_ms: Option<i64>,
    /// 1.0.88.
    #[serde(default)]
    pub output_ttft_ms: Option<f64>,
    #[serde(default, deserialize_with = "lenient_count")]
    pub inter_token_latency_ms: Option<i64>,
    #[serde(default)]
    pub initiator: Option<String>,
    #[serde(default)]
    pub api_endpoint: Option<String>,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    #[serde(default)]
    pub finish_reason: Option<String>,
    #[serde(default, deserialize_with = "lenient_count")]
    pub content_filter_triggered: Option<i64>,
    /// CAPI's `copilot_usage.tokenDetails` line items as JSON text.
    #[serde(default)]
    pub token_details_json: Option<String>,
    /// ISO 8601 UTC as the runtime writes it; the DDL default would be
    /// SQLite's `YYYY-MM-DD HH:MM:SS`.
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl Drift for UsageEventRow {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        note_unknown(&self.unknown, prefix, out);
    }
}

/// One `token_details_json` entry: CAPI's own line item for one token type,
/// with the exclusive count it charged (`costPerBatch` nano-AIU per
/// `batchSize` tokens). 1.0.88 entries may carry a `model`.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenDetail {
    pub token_type: String,
    #[serde(default, deserialize_with = "lenient_count")]
    pub token_count: Option<i64>,
    #[serde(default, deserialize_with = "lenient_count")]
    pub batch_size: Option<i64>,
    #[serde(default, deserialize_with = "lenient_count")]
    pub cost_per_batch: Option<i64>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

/// Parse one `assistant_usage_events` row.
pub fn parse_usage_row(value: Value) -> Result<UsageEventRow, serde_json::Error> {
    serde_json::from_value(value)
}

/// Parse a row's `token_details_json` text.
pub fn parse_token_details(text: &str) -> Result<Vec<TokenDetail>, serde_json::Error> {
    serde_json::from_str(text)
}

/// Whole-number reading of a JSON number. SQLite type affinity can hand an
/// INTEGER column a REAL, and the writer schema types the nano-AIU counters
/// as `number`, so an integral float converts exactly and a fractional one
/// rounds half away from zero instead of failing the record (the
/// `session.shutdown` failure mode of COPILOT-2). Anything else is unreported.
pub fn whole_number(value: &Value) -> Option<i64> {
    if let Some(int) = value.as_i64() {
        return Some(int);
    }
    let rounded = value.as_f64()?.round();
    (rounded.is_finite() && rounded.abs() < 9.2e18).then_some(rounded as i64)
}

fn lenient_count<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<i64>, D::Error> {
    match Option::<Value>::deserialize(deserializer)? {
        None | Some(Value::Null) => Ok(None),
        Some(value @ Value::Number(_)) => whole_number(&value)
            .map(Some)
            .ok_or_else(|| D::Error::custom("count out of range")),
        Some(other) => Err(D::Error::custom(format!(
            "expected a number, found {other}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_token_details, parse_usage_row, whole_number};
    use serde_json::json;

    #[test]
    fn counts_accept_integral_floats_and_round_fractions() {
        assert_eq!(whole_number(&json!(5_345_350_000_i64)), Some(5_345_350_000));
        assert_eq!(whole_number(&json!(5_345_350_000.0)), Some(5_345_350_000));
        assert_eq!(whole_number(&json!(1.5)), Some(2));
        assert_eq!(whole_number(&json!("5")), None);
        assert_eq!(whole_number(&json!(null)), None);
    }

    #[test]
    fn rows_tolerate_nulls_floats_and_future_columns() {
        let row = parse_usage_row(json!({
            "id": 7,
            "session_id": "session",
            "model": "gpt-5.6-terra",
            "input_tokens": 19918.0,
            "output_tokens": 305,
            "cache_read_tokens": null,
            "cache_write_tokens": 19915,
            "reasoning_tokens": 51,
            "total_nano_aiu": 5345350000.0,
            "request_multiplier": 1,
            "created_at": "2026-08-27T01:28:08.091Z",
            "future_column": "x",
        }))
        .expect("row parses");
        assert_eq!(row.input_tokens, Some(19_918));
        assert_eq!(row.cache_read_tokens, None);
        assert_eq!(row.total_nano_aiu, Some(5_345_350_000));
        assert_eq!(row.request_multiplier, Some(1.0));
        assert_eq!(row.unknown.keys().collect::<Vec<_>>(), ["future_column"]);
        assert!(
            parse_usage_row(json!({"id": 1, "session_id": "s", "model": "m", "input_tokens": "3"}))
                .is_err()
        );
    }

    #[test]
    fn line_items_keep_their_type_count_and_model() {
        let entries = parse_token_details(
            r#"[{"batchSize":1000000,"costPerBatch":250000000000,"tokenCount":19915,"tokenType":"cache_write"},{"tokenType":"cache_write_1h","tokenCount":12,"model":"claude-opus-5.5"}]"#,
        )
        .expect("details parse");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].cost_per_batch, Some(250_000_000_000));
        assert_eq!(entries[1].model.as_deref(), Some("claude-opus-5.5"));
        assert_eq!(entries[1].token_count, Some(12));
    }
}
