//! Claude Code usage (docs/ARCHITECTURE.md §4.1.3, RESEARCH_PLAN §5.13):
//! recorded usage → the exclusive token buckets, how final a streamed snapshot
//! is, and the extra model calls a response's iterations report.

use rosetta_core::rows::{
    FileEditRow, MessageKind, MessageRow, Ms, ServiceTier, SessionUsageRow, TokenSnapshot,
    UsageScope,
};
use rosetta_core::{IngestOp, fold};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::raw::transcript::RecordedUsage;

/// A usage subset (thinking inside output, 1-hour writes inside the cache
/// creation total) larger than the total that contains it.
pub(crate) const USAGE_SUBSET_EXCEEDS_TOTAL: &str = "usage_subset_exceeds_total";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct ClaudeResponseUsage {
    /// [`fingerprint`] of the owning snapshot: enough to tell a correction
    /// from a repeat without keeping usage facts in the checkpoint.
    #[serde(default)]
    pub(crate) fingerprint: u64,
    pub(crate) target: ClaudeUsageTarget,
    #[serde(default)]
    pub(crate) rank: UsageRank,
}

/// A stable 64-bit FNV-1a fingerprint of every field of a usage snapshot, in
/// a fixed order. The checkpoint rewrites each response's state on every
/// live append, so it keeps this instead of the snapshot.
pub(crate) fn fingerprint(tokens: &TokenSnapshot) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET;
    let mut feed = |bytes: &[u8]| {
        for byte in bytes {
            hash = (hash ^ u64::from(*byte)).wrapping_mul(PRIME);
        }
    };
    for count in [
        tokens.input,
        tokens.output,
        tokens.cache_read,
        tokens.cache_write,
        tokens.cache_write_1h,
        tokens.thinking,
        tokens.web_searches,
        tokens.billed_pico_usd,
    ] {
        match count {
            Some(value) => {
                feed(&[1]);
                feed(&value.to_le_bytes());
            }
            None => feed(&[0]),
        }
    }
    for text in [
        tokens.service_tier.map(ServiceTier::as_str),
        tokens.route.as_deref(),
    ] {
        match text {
            Some(text) => {
                feed(&u64::try_from(text.len()).unwrap_or(u64::MAX).to_le_bytes());
                feed(text.as_bytes());
            }
            None => feed(&[0xff]),
        }
    }
    feed(&[u8::from(tokens.scope == UsageScope::Aggregate)]);
    hash
}

/// How final a streamed usage snapshot is. Claude Code writes one record per
/// content block, each repeating the response's usage; early records carry a
/// placeholder output (1–99 tokens) and only the final record — non-null
/// `stop_reason`, the `speed` key — the response's real usage. A later
/// placeholder never replaces a more final snapshot (RESEARCH_PLAN §5.13).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct UsageRank {
    pub(crate) final_record: bool,
    pub(crate) output: i64,
}

/// Claude Code's placeholder model for client-side API errors: zero usage
/// that is no request at all.
pub(crate) const SYNTHETIC_MODEL: &str = "<synthetic>";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum ClaudeUsageTarget {
    Timeline { timeline_id: String },
    Session { sample_key: String },
}

/// Extra model calls inside one response are requests of their own
/// (RESEARCH_PLAN §5.13): the top-level usage sums only `message` entries,
/// leaving advisor and compaction calls out. A fallback's serving attempt is
/// already the top level.
pub(crate) fn iteration_samples(
    session_id: String,
    response_id: &str,
    response_model: &str,
    usage: &RecordedUsage,
    ts: Ms,
) -> Vec<IngestOp> {
    let Some(iterations) = usage.iterations.as_ref().and_then(Value::as_array) else {
        return Vec::new();
    };
    iterations
        .iter()
        .enumerate()
        .filter(|(_, entry)| {
            matches!(
                entry.get("type").and_then(Value::as_str),
                Some("advisor_message" | "compaction")
            )
        })
        .filter_map(|(index, entry)| {
            let (tokens, _) = anthropic_snapshot(entry);
            (!tokens.is_empty()).then(|| {
                IngestOp::SessionUsage(SessionUsageRow {
                    session_id: session_id.clone(),
                    key: format!("response:{response_id}:iteration:{index}"),
                    ts,
                    model: Some(
                        entry
                            .get("model")
                            .and_then(Value::as_str)
                            .unwrap_or(response_model)
                            .to_owned(),
                    ),
                    tokens,
                })
            })
        })
        .collect()
}

/// Pick the best row already available when usage is first observed. A response
/// split across JSONL records cannot be buffered waiting for a later text block,
/// so the first projectable record becomes the stable replacement target.
pub(crate) fn response_usage_target(
    rows: &[(MessageRow, Option<FileEditRow>)],
    response_id: &str,
) -> ClaudeUsageTarget {
    let target = rows
        .iter()
        .position(|(row, _)| row.kind == MessageKind::Agent)
        .or_else(|| {
            rows.iter()
                .position(|(row, _)| row.kind == MessageKind::ToolUse)
        })
        .unwrap_or(0);
    match rows.get(target) {
        Some((row, _)) => ClaudeUsageTarget::Timeline {
            timeline_id: row.timeline_id.clone(),
        },
        None => ClaudeUsageTarget::Session {
            sample_key: format!("response:{response_id}"),
        },
    }
}

/// Map Claude Code's recorded usage into the exclusive buckets (ARCHITECTURE
/// §4.1.3, RESEARCH_PLAN §5.13). Anthropic's `input_tokens` already excludes
/// cache reads and writes. `cache_creation_input_tokens` is the 5-minute plus
/// 1-hour total, split by `cache_creation.ephemeral_1h_input_tokens`; a writer
/// without the breakdown leaves every write at the default TTL.
/// `output_tokens` includes thinking, which Claude Code breaks out as
/// `output_tokens_details.thinking_tokens`. Returns whether a subset exceeded
/// its total (clamped, reported as drift).
pub(crate) fn snapshot(usage: &RecordedUsage) -> (TokenSnapshot, bool) {
    let count = |value: u64| i64::try_from(value).ok();
    // On a fallback turn the top-level scalars are the serving attempt's but
    // its TTL breakdown can be stale; the serving entry has its own.
    let serving = usage
        .iterations
        .as_ref()
        .and_then(Value::as_array)
        .and_then(|entries| {
            entries
                .iter()
                .find(|entry| entry.get("type").and_then(Value::as_str) == Some("fallback_message"))
        });
    let one_hour = serving
        .and_then(|entry| entry.get("cache_creation"))
        .unwrap_or(&usage.cache_creation)
        .get("ephemeral_1h_input_tokens")
        .and_then(Value::as_i64);
    let (cache_write, one_hour, write_clamped) =
        fold::split_subset(count(usage.cache_creation_input_tokens), one_hour);
    let thinking = usage
        .output_tokens_details
        .as_ref()
        .and_then(|details| details.get("thinking_tokens"))
        .and_then(Value::as_i64);
    let (output, thinking, output_clamped) =
        fold::split_subset(count(usage.output_tokens), thinking);
    let tokens = TokenSnapshot {
        input: count(usage.input_tokens),
        output,
        cache_read: count(usage.cache_read_input_tokens),
        cache_write,
        cache_write_1h: one_hour,
        thinking,
        // Web fetches carry no per-call charge; only searches do.
        web_searches: usage
            .server_tool_use
            .as_ref()
            .and_then(|tools| tools.get("web_search_requests"))
            .and_then(Value::as_i64),
        service_tier: claude_service_tier(usage),
        route: usage.inference_geo.as_deref().and_then(claude_route),
        billed_pico_usd: None,
        // The top level sums every `message` iteration; several of them are
        // several sampling passes, whose long-context tier cannot be read
        // from the sum.
        scope: if message_iterations(usage) > 1 {
            UsageScope::Aggregate
        } else {
            UsageScope::Request
        },
    };
    (tokens, write_clamped || output_clamped)
}

fn message_iterations(usage: &RecordedUsage) -> usize {
    usage
        .iterations
        .as_ref()
        .and_then(Value::as_array)
        .map_or(0, |entries| {
            entries
                .iter()
                .filter(|entry| entry.get("type").and_then(Value::as_str) == Some("message"))
                .count()
        })
}

/// The same mapping for an Anthropic usage object held as JSON (an entry of
/// `usage.iterations`).
pub(crate) fn anthropic_snapshot(usage: &Value) -> (TokenSnapshot, bool) {
    let count = |key: &str| usage.get(key).and_then(Value::as_i64);
    let (cache_write, cache_write_1h, write_clamped) = fold::split_subset(
        count("cache_creation_input_tokens"),
        usage
            .pointer("/cache_creation/ephemeral_1h_input_tokens")
            .and_then(Value::as_i64),
    );
    let (output, thinking, output_clamped) = fold::split_subset(
        count("output_tokens"),
        usage
            .pointer("/output_tokens_details/thinking_tokens")
            .and_then(Value::as_i64),
    );
    let tokens = TokenSnapshot {
        input: count("input_tokens"),
        output,
        cache_read: count("cache_read_input_tokens"),
        cache_write,
        cache_write_1h,
        thinking,
        ..TokenSnapshot::default()
    };
    (tokens, write_clamped || output_clamped)
}

/// Claude Code stamps `speed: "fast"` for fast mode, which Anthropic prices
/// apart from the `service_tier` it also records.
fn claude_service_tier(usage: &RecordedUsage) -> Option<ServiceTier> {
    if usage.speed.as_deref() == Some("fast") {
        return Some(ServiceTier::Fast);
    }
    match usage.service_tier.as_deref()? {
        "standard" => Some(ServiceTier::Standard),
        "priority" => Some(ServiceTier::Priority),
        "batch" => Some(ServiceTier::Batch),
        _ => None,
    }
}

/// `inference_geo` names where Anthropic ran the request; the default
/// (`not_available`, `global`) is no route at all, and a residency such as
/// `us` becomes the qualified route `anthropic/us`.
fn claude_route(geo: &str) -> Option<String> {
    match geo {
        "" | "not_available" | "global" => None,
        residency => rosetta_core::rows::canonical_route(&format!("anthropic/{residency}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprints_separate_every_field_and_repeat_exactly() {
        let base = TokenSnapshot {
            input: Some(229),
            output: Some(387),
            cache_read: Some(15_409),
            thinking: Some(342),
            ..TokenSnapshot::default()
        };
        let print = fingerprint(&base);
        assert_eq!(print, fingerprint(&base.clone()));
        let variants = [
            TokenSnapshot {
                output: Some(388),
                ..base.clone()
            },
            TokenSnapshot {
                cache_write: Some(0),
                ..base.clone()
            },
            TokenSnapshot {
                service_tier: Some(ServiceTier::Fast),
                ..base.clone()
            },
            TokenSnapshot {
                route: Some(String::new()),
                ..base.clone()
            },
            TokenSnapshot {
                scope: UsageScope::Aggregate,
                ..base.clone()
            },
        ];
        for variant in &variants {
            assert_ne!(fingerprint(variant), print, "{variant:?}");
        }
    }
}
