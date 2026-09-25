//! The session store's per-call rows: one metered `assistant_usage_events`
//! row → one request sample. The rule that keeps these and the shutdown
//! intervals from counting a call twice is on the parent module.

use std::collections::BTreeSet;

use rosetta_adapter_support::adapter::{LineOutcome, LineStatus, NormalizeCtx};
use rosetta_adapter_support::{Drift, note_unknown};
use rosetta_core::rows::{Ms, SessionUsageRow, TokenSnapshot, UsageScope};
use rosetta_core::{IngestOp, fold, ids};
use serde_json::Value;

use super::{USAGE_SUBSET_EXCEEDS_TOTAL, priced_model};
use crate::normalize::drift;
use crate::raw::store::{
    SnapshotRecord, TokenDetail, USAGE_EVENTS, UsageEventRow, parse_token_details, parse_usage_row,
};
use crate::time::rfc3339_ms;

const TOKEN_DETAILS: &str = "assistant_usage_events.token_details_json";
const UNKNOWN_QUERY: &str = "unknown_query";
const UNKNOWN_FIELDS: &str = "unknown_fields";
const PARSE_FAILED: &str = "parse_failed";
const INVALID_JSON: &str = "<invalid-json>";
const UNTYPED: &str = "<untyped>";
/// A call whose CAPI line items name more than one model (1.0.88 fusion);
/// the record keeps the billing model and the row's exact totals.
const USAGE_MIXED_MODELS: &str = "usage_mixed_models";
/// A store row without a parseable `created_at`.
const USAGE_UNDATED: &str = "usage_undated";

/// One `session-store` snapshot record: `{"query": name, "row": {...}}`.
/// A row of the declared usage query becomes one request sample; any other
/// query is soft drift, a malformed row is a hard error.
pub(crate) fn ingest_line(ctx: &mut NormalizeCtx, stream_id: &str, line: &[u8]) -> LineOutcome {
    let Ok(text) = std::str::from_utf8(line) else {
        return hard(ctx, stream_id, INVALID_JSON, &[], None);
    };
    if text.trim().is_empty() {
        return LineOutcome::parsed(Vec::new());
    }
    let value: Value = match serde_json::from_str(text) {
        Ok(value) => value,
        Err(_) => return hard(ctx, stream_id, INVALID_JSON, &[], None),
    };
    let record: SnapshotRecord = match serde_json::from_value(value.clone()) {
        Ok(record) => record,
        Err(_) => {
            let kind = value["query"].as_str().unwrap_or(UNTYPED).to_owned();
            return hard(ctx, stream_id, &kind, &keys(&value), None);
        }
    };

    let mut ops = Vec::new();
    if !record.unknown.is_empty() {
        let mut paths = Vec::new();
        note_unknown(&record.unknown, "envelope", &mut paths);
        ops.push(drift(
            ctx,
            stream_id,
            &record.query,
            UNKNOWN_FIELDS,
            &paths,
            None,
        ));
    }
    if record.query != USAGE_EVENTS {
        ops.push(drift(
            ctx,
            stream_id,
            &record.query,
            UNKNOWN_QUERY,
            &keys(&record.row),
            None,
        ));
        return LineOutcome {
            ops,
            status: LineStatus::UnknownType,
        };
    }
    let row = match parse_usage_row(record.row.clone()) {
        Ok(row) => row,
        Err(_) => {
            let mut outcome = hard(ctx, stream_id, USAGE_EVENTS, &keys(&record.row), None);
            ops.append(&mut outcome.ops);
            return LineOutcome {
                ops,
                status: outcome.status,
            };
        }
    };
    let stamp = row.created_at.as_deref().and_then(rfc3339_ms);
    let paths = row.drift_paths();
    if !paths.is_empty() {
        ops.push(drift(
            ctx,
            stream_id,
            USAGE_EVENTS,
            UNKNOWN_FIELDS,
            &paths,
            stamp,
        ));
    }
    call_usage(ctx, stream_id, &row, stamp, &mut ops);
    LineOutcome::parsed(ops)
}

/// CAPI's line items, reduced to what the buckets need.
#[derive(Default)]
struct LineItems {
    cache_write_1h: Option<i64>,
    models: BTreeSet<String>,
}

impl From<Vec<TokenDetail>> for LineItems {
    fn from(entries: Vec<TokenDetail>) -> Self {
        let mut items = Self::default();
        for entry in entries {
            if entry.token_type == "cache_write_1h"
                && let Some(count) = entry.token_count
            {
                items.cache_write_1h = Some(items.cache_write_1h.unwrap_or(0) + count);
            }
            if let Some(model) = entry.model.filter(|model| !model.trim().is_empty()) {
                items.models.insert(model);
            }
        }
        items
    }
}

/// One metered store row → one request sample: `input` is the inclusive
/// total minus cache reads and writes, `output` the total minus reasoning,
/// 1-hour writes are split out of the write total when CAPI's line items
/// carry them, and the charge is the row's own nano-AIU.
fn call_usage(
    ctx: &NormalizeCtx,
    stream_id: &str,
    row: &UsageEventRow,
    stamp: Option<Ms>,
    ops: &mut Vec<IngestOp>,
) {
    let Some(nano_aiu) = row.total_nano_aiu.filter(|nano| *nano > 0) else {
        return;
    };
    let items = match row.token_details_json.as_deref().map(parse_token_details) {
        None => LineItems::default(),
        Some(Ok(entries)) => LineItems::from(entries),
        Some(Err(_)) => {
            ops.push(drift(
                ctx,
                stream_id,
                TOKEN_DETAILS,
                PARSE_FAILED,
                &[],
                stamp,
            ));
            LineItems::default()
        }
    };
    if items.models.len() > 1 {
        ops.push(drift(
            ctx,
            stream_id,
            USAGE_EVENTS,
            USAGE_MIXED_MODELS,
            &["token_details_json[].model".to_owned()],
            stamp,
        ));
    }

    let (cache_write, cache_write_1h, write_clamped) =
        fold::split_subset(row.cache_write_tokens, items.cache_write_1h);
    let (input, input_clamped) = fold::exclusive_remainder(
        row.input_tokens,
        &[row.cache_read_tokens, row.cache_write_tokens],
    );
    let (output, thinking, output_clamped) =
        fold::split_subset(row.output_tokens, row.reasoning_tokens);
    let clamped: Vec<String> = [
        (input_clamped, "input_tokens"),
        (output_clamped, "output_tokens"),
        (write_clamped, "cache_write_tokens"),
    ]
    .into_iter()
    .filter(|(clamped, _)| *clamped)
    .map(|(_, column)| column.to_owned())
    .collect();
    if !clamped.is_empty() {
        ops.push(drift(
            ctx,
            stream_id,
            USAGE_EVENTS,
            USAGE_SUBSET_EXCEEDS_TOTAL,
            &clamped,
            stamp,
        ));
    }
    if stamp.is_none() {
        ops.push(drift(
            ctx,
            stream_id,
            USAGE_EVENTS,
            USAGE_UNDATED,
            &["created_at".to_owned()],
            None,
        ));
    }

    let native_model = row
        .copilot_usage_model
        .as_deref()
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .unwrap_or(&row.model);
    let priced = priced_model(native_model);
    ops.push(IngestOp::SessionUsage(SessionUsageRow {
        identity: None,
        session_id: ctx.session_id.clone(),
        key: ids::usage_id(&ctx.session_id, &format!("call:{}", row.id)),
        ts: stamp.unwrap_or_default(),
        model: Some(priced.model),
        tokens: TokenSnapshot {
            input,
            output,
            cache_read: row.cache_read_tokens,
            cache_write,
            thinking,
            cache_write_1h,
            service_tier: priced.service_tier,
            billed_pico_usd: nano_aiu.checked_mul(10),
            scope: UsageScope::Request,
            requests: None,
            ..TokenSnapshot::default()
        },
    }));
}

fn hard(
    ctx: &NormalizeCtx,
    stream_id: &str,
    record_kind: &str,
    paths: &[String],
    at: Option<Ms>,
) -> LineOutcome {
    LineOutcome {
        ops: vec![drift(ctx, stream_id, record_kind, PARSE_FAILED, paths, at)],
        status: LineStatus::HardError {
            code: PARSE_FAILED.to_owned(),
        },
    }
}

fn keys(value: &Value) -> Vec<String> {
    value
        .as_object()
        .map(|object| object.keys().cloned().collect())
        .unwrap_or_else(|| vec!["<non-object>".to_owned()])
}

#[cfg(test)]
mod tests {
    use super::ingest_line;
    use rosetta_adapter_support::adapter::{LineStatus, NormalizeCtx};
    use rosetta_core::IngestOp;
    use rosetta_core::rows::DriftFact;

    fn drift_codes(ops: &[IngestOp]) -> Vec<&str> {
        ops.iter()
            .filter_map(|op| match op {
                IngestOp::Drift(DriftFact { error_code, .. }) => Some(error_code.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn store_rows_outside_the_usage_query_are_soft_drift() {
        let mut ctx = NormalizeCtx::new("runtime", "session");
        let outcome = ingest_line(
            &mut ctx,
            "session-store",
            br#"{"query":"turns","row":{"id":1}}"#,
        );
        assert_eq!(outcome.status, LineStatus::UnknownType);
        assert_eq!(drift_codes(&outcome.ops), ["unknown_query"]);

        let outcome = ingest_line(
            &mut ctx,
            "session-store",
            br#"{"query":"assistant_usage_events","row":{"id":"seven"}}"#,
        );
        assert!(matches!(outcome.status, LineStatus::HardError { .. }));
    }
}
