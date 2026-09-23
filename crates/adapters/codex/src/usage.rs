//! Codex usage (docs/ARCHITECTURE.md §4.1.3, RESEARCH_PLAN §5.13): one usage
//! record per model request from the running `token_count` totals, with the
//! replayed prefixes of forks and subagents skipped.

use rosetta_adapter_support::adapter::NormalizeCtx;
use rosetta_core::rows::{MessageRow, Ms, ServiceTier, SessionUsageRow, TokenSnapshot};
use rosetta_core::{IngestOp, fold, ids};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::normalize::{CodexState, number};
use crate::raw::rollout::{EventMsg, SessionMeta};

/// One request's usage (RESEARCH_PLAN §5.13). Codex emits one `token_count`
/// per model request, after the request's items and tool outputs; `info` is
/// `{total_token_usage, last_token_usage}`, where the total is the running sum
/// of `last` for this thread lineage. The request's usage is:
///
/// - no earlier reading in this file → `last` (a fork or resume seeds the
///   total from its parent; the total is never counted whole);
/// - an unchanged total → nothing (rate-limit re-emits, recomputes, failed
///   streams);
/// - a total that advanced in every field → the difference (recovers a
///   missed emission);
/// - otherwise (a re-based or context-filled total) → `last`.
///
/// Readings inside a replayed prefix still advance the total but count
/// nothing. The usage lands on the request's latest surviving row, or becomes
/// a request sample when the request produced none (polls, empty reasoning):
/// usage aimed at a row that projection drops would be lost.
pub(crate) fn token_count(
    ctx: &mut NormalizeCtx,
    state: &mut CodexState,
    event: &EventMsg,
    ordinal: Option<u64>,
    ts: Option<Ms>,
    ops: &mut Vec<IngestOp>,
) {
    let Some(info) = event.info.as_ref() else {
        return;
    };
    let Some(total) = RawUsage::parse(&info["total_token_usage"]) else {
        return;
    };
    let last = RawUsage::parse(&info["last_token_usage"]);
    let request = match state.prev_total {
        None => last,
        Some(previous) if total == previous => None,
        Some(previous) if total.at_least(&previous) => Some(total.minus(&previous)),
        Some(_) => last,
    };
    state.prev_total = Some(total);
    let Some(request) = request.filter(RawUsage::has_tokens) else {
        return;
    };
    state.token_counts += 1;
    let row = state.request_row.take();
    let searches = std::mem::take(&mut state.request_searches);
    if state.replay_gate.skips(ordinal, ts) {
        return;
    }
    let tokens = TokenSnapshot {
        web_searches: (searches > 0).then_some(searches),
        service_tier: state.service_tier,
        route: state.route.clone(),
        ..request.exclusive()
    };
    let at = ts.unwrap_or_default();
    match row {
        Some(response) => ops.push(IngestOp::Usage {
            session_id: ctx.session_id.clone(),
            response,
            ts: at,
            model: state.model.clone(),
            tokens,
        }),
        None => ops.push(IngestOp::SessionUsage(SessionUsageRow {
            session_id: ctx.session_id.clone(),
            key: ids::usage_id(&ctx.session_id, &format!("tc:{}", state.token_counts)),
            ts: at,
            model: state.model.clone(),
            tokens,
        })),
    }
}

/// Emit an assistant-authored row; when it survives projection, the request
/// in flight produced it and its token count belongs to it.
pub(crate) fn push_response(state: &mut CodexState, row: MessageRow, ops: &mut Vec<IngestOp>) {
    let timeline_id = row.timeline_id.clone();
    if IngestOp::push_message(ops, row) {
        begin_response(state, &timeline_id);
    }
}

/// Point the next `token_count` at a newly emitted, surviving assistant row.
/// A codex request ends in a message OR a tool call, and its token_count
/// lands after whichever it was, so the latest such row owns the usage.
pub(crate) fn begin_response(state: &mut CodexState, timeline_id: &str) {
    state.request_row = Some(timeline_id.to_owned());
}

/// Codex's native token counters, all inclusive: `input_tokens` contains
/// cached reads and cache writes, `output_tokens` contains reasoning.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RawUsage {
    input: i64,
    cached: i64,
    cache_write: i64,
    output: i64,
    reasoning: i64,
    total: i64,
}

impl RawUsage {
    fn parse(value: &Value) -> Option<Self> {
        let count = |key: &str| number(value, key).unwrap_or(0);
        value.is_object().then(|| Self {
            input: count("input_tokens"),
            cached: count("cached_input_tokens"),
            cache_write: count("cache_write_input_tokens"),
            output: count("output_tokens"),
            reasoning: count("reasoning_output_tokens"),
            total: count("total_tokens"),
        })
    }

    fn fields(&self) -> [i64; 6] {
        [
            self.input,
            self.cached,
            self.cache_write,
            self.output,
            self.reasoning,
            self.total,
        ]
    }

    fn at_least(&self, other: &Self) -> bool {
        self.fields()
            .iter()
            .zip(other.fields())
            .all(|(now, then)| *now >= then)
    }

    fn minus(&self, other: &Self) -> Self {
        Self {
            input: self.input - other.input,
            cached: self.cached - other.cached,
            cache_write: self.cache_write - other.cache_write,
            output: self.output - other.output,
            reasoning: self.reasoning - other.reasoning,
            total: self.total - other.total,
        }
    }

    /// A context-window fill reports only `total_tokens`; it is not a request.
    fn has_tokens(&self) -> bool {
        self.input > 0 || self.output > 0
    }

    /// The exclusive buckets, each subset clamped to what contains it.
    fn exclusive(&self) -> TokenSnapshot {
        let (rest, cache_read, _) = fold::split_subset(Some(self.input), Some(self.cached));
        let (input, cache_write, _) = fold::split_subset(rest, Some(self.cache_write));
        let (output, thinking, _) = fold::split_subset(Some(self.output), Some(self.reasoning));
        TokenSnapshot {
            input,
            output,
            cache_read,
            cache_write,
            thinking,
            ..TokenSnapshot::default()
        }
    }
}

/// Where a rollout's own usage begins. A paginated subagent replays its
/// parent's items below `subagent_history_start_ordinal`; a user fork, a
/// legacy subagent and a migrated subagent instead copy the parent's history
/// (token counts included) in one append whose timestamps fall in the
/// file's first second (RESEARCH_PLAN §5.13).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum ReplayGate {
    #[default]
    Open,
    BelowOrdinal {
        start: u64,
    },
    HeadBurst {
        started_ms: Option<i64>,
    },
}

impl ReplayGate {
    /// The ordinal boundary appeared in 0.146.0. A file that carries one but
    /// was written by an older CLI was migrated, and its boundary sits at the
    /// end of the file, below the child's own turns.
    pub(crate) fn for_session(meta: &SessionMeta, ts: Option<Ms>) -> Self {
        let migrated = semver(&meta.cli_version).is_some_and(|version| version < (0, 146, 0));
        match meta.subagent_history_start_ordinal {
            Some(start) if !migrated => Self::BelowOrdinal { start },
            _ if migrated
                || meta.forked_from_id.is_some()
                || meta.source.pointer("/subagent/thread_spawn").is_some() =>
            {
                Self::HeadBurst { started_ms: ts }
            }
            _ => Self::Open,
        }
    }

    fn skips(&mut self, ordinal: Option<u64>, ts: Option<Ms>) -> bool {
        match *self {
            Self::Open => false,
            Self::BelowOrdinal { start } => ordinal.is_some_and(|ordinal| ordinal < start),
            Self::HeadBurst { started_ms } => {
                let (Some(started), Some(ts)) = (started_ms, ts) else {
                    return false;
                };
                if ts - started <= 1_000 {
                    true
                } else {
                    *self = Self::Open;
                    false
                }
            }
        }
    }
}

fn semver(version: &str) -> Option<(u64, u64, u64)> {
    let mut parts = version.trim_start_matches('v').split('.');
    let mut next = || {
        parts
            .next()?
            .split(|character: char| !character.is_ascii_digit())
            .next()?
            .parse()
            .ok()
    };
    Some((next()?, next()?, next()?))
}

/// Codex records the CONFIGURED tier (the served one is never persisted):
/// `fast`/`priority` requests OpenAI priority processing.
pub(crate) fn codex_service_tier(value: Option<&str>) -> Option<ServiceTier> {
    match value? {
        "priority" | "fast" => Some(ServiceTier::Priority),
        "flex" => Some(ServiceTier::Flex),
        "default" | "standard" | "auto" => Some(ServiceTier::Standard),
        _ => None,
    }
}
