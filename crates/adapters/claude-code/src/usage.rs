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
        tokens.requests,
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

/// The first writer observed to persist one record per streamed content
/// block whose `usage` is the `message_start` placeholder: 2.1.9
/// (2026-01-16). Writers through 2.1.7 wrote a progressive count whose last
/// record is the response's real output (2026-09-23 census: in every
/// multi-record response the last record carries the maximum, up to 5,198
/// tokens, and 2.1.2 alone has 218 non-final responses at or above 100).
/// From 2.1.9 every non-final record repeats one 1–92 token placeholder,
/// and until 2.1.50 no final record is written at all. No 2.1.8 transcript
/// exists locally, so the boundary is the first streaming writer seen.
const PLACEHOLDER_OUTPUT_WRITER: (u64, u64, u64) = (2, 1, 9);

/// Whether a writer's non-final records carry the `message_start` output
/// placeholder rather than a count (RESEARCH_PLAN §5.13). Such a snapshot
/// reports its output and thinking as unrecorded, never as the placeholder.
/// A version that does not parse is treated as current: every writer since
/// 2026-01-16 streams placeholders.
pub(crate) fn streams_placeholder_output(version: &str) -> bool {
    parse_version(version).is_none_or(|version| version >= PLACEHOLDER_OUTPUT_WRITER)
}

fn parse_version(version: &str) -> Option<(u64, u64, u64)> {
    let mut parts = version.trim().split('.');
    let mut component = || -> Option<u64> {
        let digits: String = parts
            .next()?
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        digits.parse().ok()
    };
    Some((component()?, component()?, component()?))
}

/// The transient native identity every copy of one response shares across
/// files (ARCHITECTURE §4.1.3, counting rule 2): `<message.id>:<requestId>`,
/// or the message id alone on the handful of pre-2.1.9 records without a
/// request id. It rides on every usage fact of the response and is never
/// persisted.
pub(crate) fn response_identity(message_id: &str, request_id: Option<&str>) -> Option<String> {
    if message_id.is_empty() {
        return None;
    }
    Some(match request_id.filter(|id| !id.is_empty()) {
        Some(request_id) => format!("{message_id}:{request_id}"),
        None => message_id.to_owned(),
    })
}

/// What one assistant record contributes to its response's usage: the fact
/// to emit, the sample that fact supersedes, and the state to remember.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct UsagePlan {
    pub(crate) target: ClaudeUsageTarget,
    pub(crate) tokens: TokenSnapshot,
    /// A `response:<id>` or `timeline:<id>` sample the fact moves out of,
    /// retracted with an all-`None` snapshot (inert in the rollup).
    pub(crate) retract: Option<String>,
    pub(crate) remember: ClaudeResponseUsage,
}

impl UsagePlan {
    pub(crate) fn targets_row(&self, timeline_id: &str) -> bool {
        matches!(&self.target, ClaudeUsageTarget::Timeline { timeline_id: id } if id == timeline_id)
    }

    /// Projection dropped the owning row: the fact becomes the
    /// `timeline:<id>` sample, and later corrections must replace that
    /// sample rather than aim at a row that does not exist.
    pub(crate) fn drop_owner(&mut self) {
        let ClaudeUsageTarget::Timeline { timeline_id } = &self.target else {
            return;
        };
        let sample_key = format!("timeline:{timeline_id}");
        if self.retract.as_deref() == Some(sample_key.as_str()) {
            self.retract = None;
        }
        self.target = ClaudeUsageTarget::Session { sample_key };
        self.remember.target = self.target.clone();
    }
}

/// Decide what a record's usage snapshot does for its response. Pure: the
/// same inputs plan the same ops on a whole-file pass, a live append, or a
/// checkpoint resume, because `known` is exactly the checkpointed state.
///
/// * The first snapshot, or a more final one with a different fingerprint,
///   is the response's fact (an improvement).
/// * A fact held in a session sample moves to the response's first visible
///   row as soon as one exists and the record carries the same or a better
///   snapshot (CC-6): reply heads read row usage only.
/// * A replayed record (every block already projected) carrying the known
///   snapshot re-states the fact (CC-7): a re-emitted row carries no usage,
///   so the store's row upsert would otherwise wipe what it holds.
/// * Anything else — a placeholder after the final record, a strictly worse
///   snapshot, a repeated identical snapshot on a new record — changes
///   nothing.
pub(crate) fn plan_response_usage(
    known: Option<&ClaudeResponseUsage>,
    rows: &[(MessageRow, Option<FileEditRow>)],
    response_id: &str,
    tokens: TokenSnapshot,
    rank: UsageRank,
    replayed: bool,
) -> Option<UsagePlan> {
    let print = fingerprint(&tokens);
    let improves = known.is_none_or(|known| rank > known.rank && known.fingerprint != print);
    let restates = known.is_some_and(|known| known.fingerprint == print);
    if !improves && !restates {
        return None;
    }
    let visible = response_usage_target(rows, response_id);
    let mut target = known.map_or_else(|| visible.clone(), |known| known.target.clone());
    let mut retract = None;
    if let ClaudeUsageTarget::Session { sample_key } = &target
        && matches!(visible, ClaudeUsageTarget::Timeline { .. })
    {
        retract = Some(sample_key.clone());
        target = visible;
    }
    if !(improves || retract.is_some() || replayed) {
        return None;
    }
    let rank = known.map_or(rank, |known| rank.max(known.rank));
    Some(UsagePlan {
        target: target.clone(),
        tokens,
        retract,
        remember: ClaudeResponseUsage {
            fingerprint: print,
            target,
            rank,
        },
    })
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
    identity: Option<&str>,
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
                    identity: identity.map(str::to_owned),
                })
            })
        })
        .collect()
}

/// Pick the best row a record offers for its response's usage: the reply
/// (agent) row, else a tool row, else any row, else the `response:<id>`
/// session sample. A response split across JSONL records cannot be buffered
/// waiting for a later text block, so the first record with a visible row
/// becomes the stable replacement target ([`plan_response_usage`]).
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
    let passes = message_iterations(usage);
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
        // The top level sums every `message` iteration: several of them are
        // several sampling passes, priced as that many equal requests.
        scope: if passes > 1 {
            UsageScope::Aggregate
        } else {
            UsageScope::Request
        },
        requests: (passes > 1).then(|| i64::try_from(passes).ok()).flatten(),
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
            TokenSnapshot {
                scope: UsageScope::Aggregate,
                requests: Some(2),
                ..base.clone()
            },
        ];
        for variant in &variants {
            assert_ne!(fingerprint(variant), print, "{variant:?}");
        }
    }

    #[test]
    fn the_placeholder_writer_boundary_is_the_first_streaming_release() {
        for old in ["2.1.2", "2.1.7", "2.1.8", "2.0.99", "1.9.9"] {
            assert!(!streams_placeholder_output(old), "{old}");
        }
        for new in [
            "2.1.9",
            "2.1.50",
            "2.1.241",
            "2.2.0",
            "3.0.0",
            "2.1.9-beta",
            "",
            "later",
        ] {
            assert!(streams_placeholder_output(new), "{new}");
        }
    }

    #[test]
    fn the_planner_decides_from_the_checkpointed_state_alone() {
        use rosetta_core::rows::{MessageKind, MessageRow};
        let row = |kind: MessageKind, timeline_id: &str| {
            (
                MessageRow {
                    timeline_id: timeline_id.to_owned(),
                    session_id: "s".to_owned(),
                    ordinal: 0,
                    kind,
                    subtype: None,
                    uuid: None,
                    parent_uuid: None,
                    ts: None,
                    model: None,
                    effort: None,
                    content: None,
                    tool: None,
                    attachments: Vec::new(),
                    citations: Vec::new(),
                    group: None,
                    response: None,
                    block_index: None,
                    branch_tool_id: None,
                    usage: None,
                    agent_version: None,
                    src_cursor: None,
                    src_offset: None,
                },
                None,
            )
        };
        let placeholder = TokenSnapshot {
            input: Some(100),
            cache_read: Some(9),
            ..TokenSnapshot::default()
        };
        let real = TokenSnapshot {
            output: Some(161),
            ..placeholder.clone()
        };
        let non_final = UsageRank {
            final_record: false,
            output: 1,
        };
        let final_rank = UsageRank {
            final_record: true,
            output: 161,
        };
        let tool_rows = [row(MessageKind::ToolUse, "tool")];
        let sample = ClaudeUsageTarget::Session {
            sample_key: "response:msg".to_owned(),
        };
        let owner = ClaudeUsageTarget::Timeline {
            timeline_id: "tool".to_owned(),
        };
        let held = |target: &ClaudeUsageTarget, tokens: &TokenSnapshot, rank: UsageRank| {
            ClaudeResponseUsage {
                fingerprint: fingerprint(tokens),
                target: target.clone(),
                rank,
            }
        };

        // First sight: the fact opens where the record allows.
        let opened = plan_response_usage(None, &[], "msg", placeholder.clone(), non_final, false)
            .expect("first snapshot");
        assert_eq!(
            (opened.target.clone(), opened.retract.clone()),
            (sample.clone(), None)
        );
        assert_eq!(opened.remember, held(&sample, &placeholder, non_final));

        // The same placeholder on a new record with a visible row: the fact
        // moves to the row and the sample is retracted.
        let known = held(&sample, &placeholder, non_final);
        let moved = plan_response_usage(
            Some(&known),
            &tool_rows,
            "msg",
            placeholder.clone(),
            non_final,
            false,
        )
        .expect("retarget");
        assert_eq!(moved.target, owner);
        assert_eq!(moved.retract.as_deref(), Some("response:msg"));
        assert_eq!(moved.remember.target, owner);

        // The same snapshot on a new record without a visible row, or on a
        // known row target, changes nothing.
        assert!(
            plan_response_usage(
                Some(&known),
                &[],
                "msg",
                placeholder.clone(),
                non_final,
                false
            )
            .is_none()
        );
        let owned = held(&owner, &real, final_rank);
        assert!(
            plan_response_usage(Some(&owned), &[], "msg", real.clone(), final_rank, false)
                .is_none()
        );

        // A replayed record with the known snapshot re-states the fact on
        // the known target; a replayed placeholder after the final does not.
        let restated = plan_response_usage(
            Some(&owned),
            &tool_rows,
            "msg",
            real.clone(),
            final_rank,
            true,
        )
        .expect("restatement");
        assert_eq!(
            (restated.target.clone(), restated.retract.clone()),
            (owner.clone(), None)
        );
        assert_eq!(restated.remember, owned);
        assert!(
            plan_response_usage(
                Some(&owned),
                &tool_rows,
                "msg",
                placeholder.clone(),
                non_final,
                true
            )
            .is_none()
        );

        // The final record corrects the placeholder on the known target, and
        // a more final rank never regresses to a less final one.
        let corrected =
            plan_response_usage(Some(&known), &[], "msg", real.clone(), final_rank, false)
                .expect("correction");
        assert_eq!(corrected.target, sample);
        assert_eq!(corrected.remember.rank, final_rank);
        assert!(
            plan_response_usage(Some(&owned), &[], "msg", placeholder, non_final, false).is_none()
        );
    }

    #[test]
    fn the_identity_is_the_message_and_request_id() {
        assert_eq!(
            response_identity("msg_01", Some("req_01")).as_deref(),
            Some("msg_01:req_01")
        );
        assert_eq!(response_identity("msg_01", None).as_deref(), Some("msg_01"));
        assert_eq!(
            response_identity("msg_01", Some("")).as_deref(),
            Some("msg_01")
        );
        assert_eq!(response_identity("", Some("req_01")), None);
    }
}
