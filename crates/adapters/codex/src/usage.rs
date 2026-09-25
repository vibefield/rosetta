//! Codex usage (docs/ARCHITECTURE.md §4.1.3, RESEARCH_PLAN §5.13): one usage
//! record per model request, from the per-response `token_usage_record`
//! (writers from 0.153) and the running `token_count` totals, with the
//! copied prefixes of forks and subagents skipped, the configured tier
//! back-applied to a file's first turn, and a guardian review's own requests
//! attributed to the session that ran it.

use rosetta_adapter_support::adapter::NormalizeCtx;
use rosetta_core::rows::{MessageRow, Ms, ServiceTier, SessionUsageRow, TokenSnapshot, UsageScope};
use rosetta_core::{IngestOp, fold, ids};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use rosetta_adapter_support::adapter::LineOutcome;

use crate::normalize::{CodexState, drift, number, turn_context};
use crate::raw::rollout::{EventMsg, ResponseItem, RolloutPayload, SessionMeta, TokenUsageRecord};
use crate::raw::{ParsedLine, parse_line};
use crate::time::rfc3339_ms;

/// One request's usage (RESEARCH_PLAN §5.13). Codex emits one `token_count`
/// per model request, after the request's items and tool outputs; `info` is
/// `{total_token_usage, last_token_usage}`, where the total is the running sum
/// of `last` for this thread lineage. The request's usage is:
///
/// - no earlier reading in this file → `last` (a fork or resume seeds the
///   total from its parent; the total is never counted whole);
/// - an unchanged total → nothing (rate-limit re-emits, recomputes, failed
///   streams);
/// - a total that advanced by exactly `last` → `last`; by more than `last`
///   in every field → `last` is the request and the rest is a missed
///   emission of unknown request count, an aggregate sample (a delta labelled
///   as one request crossed the long-context tier: CODEX-7);
/// - otherwise (a re-based or context-filled total) → `last`.
///
/// From 0.153 the response's `token_usage_record` precedes this event and
/// already counted it; the reading then only advances the running total.
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
    if let Some(record) = state.last_record_usage.take()
        && last == Some(record)
    {
        state.prev_total = Some(total);
        return;
    }
    let (request, remainder) = match state.prev_total {
        None => (last, None),
        Some(previous) if total == previous => (None, None),
        Some(previous) if total.at_least(&previous) => {
            let delta = total.minus(&previous);
            match last {
                Some(last) if delta != last && delta.at_least(&last) => {
                    (Some(last), Some(delta.minus(&last)))
                }
                _ => (Some(delta), None),
            }
        }
        Some(_) => (last, None),
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
    let native_key = format!("tc:{}", state.token_counts);
    let owner = target(ctx, state, row, &native_key);
    emit(ctx, state, owner, ts, request, searches, false, ops);
    if let Some(remainder) = remainder.filter(RawUsage::has_tokens) {
        let owner = target(ctx, state, None, &format!("{native_key}:rest"));
        emit(ctx, state, owner, ts, remainder, 0, true, ops);
    }
}

/// The per-response record (writers from 0.153): one request, keyed by the
/// provider's `response_id`. Its `token_count` twin follows and is skipped by
/// usage equality; a remote compaction has no twin and is counted only here.
/// A record of another thread is copied history.
pub(crate) fn token_usage_record(
    ctx: &mut NormalizeCtx,
    state: &mut CodexState,
    record: &TokenUsageRecord,
    ordinal: Option<u64>,
    ts: Option<Ms>,
    ops: &mut Vec<IngestOp>,
) {
    let own = ctx
        .native_session_id
        .as_deref()
        .or(state.thread_id.as_deref());
    if own.is_some_and(|own| own != record.thread_id) {
        return;
    }
    let Some(usage) = RawUsage::parse(&record.usage) else {
        return;
    };
    // Remembered before any gate so the twin is recognized whatever the
    // gate does with the record itself.
    state.last_record_usage = Some(usage);
    if !usage.has_tokens() {
        return;
    }
    let row = state.request_row.take();
    let searches = std::mem::take(&mut state.request_searches);
    if state.replay_gate.skips(ordinal, ts) {
        return;
    }
    let owner = target(ctx, state, row, &format!("tur:{}", record.response_id));
    emit(ctx, state, owner, ts, usage, searches, false, ops);
}

/// The configured tier from a `thread_settings_applied` snapshot. Codex omits
/// the key when no tier is configured (`protocol.rs:2200-2201`), so a missing
/// key is unset, never the previous value; a tier this adapter does not know
/// is drift and stays absent (CODEX-9).
///
/// The first snapshot settles the file's tier: Codex records none for a
/// session's first turn (`TurnContextItem` has no tier field), so the
/// requests already emitted untiered are re-emitted with this tier. That is
/// an estimate — the tier those requests were actually served at is not
/// recorded; a configured tier applies from session start unless the user
/// changed it in between (CODEX-1).
pub(crate) fn thread_settings_applied(
    ctx: &mut NormalizeCtx,
    state: &mut CodexState,
    stream_id: &str,
    event: &EventMsg,
    ts: Option<Ms>,
    ops: &mut Vec<IngestOp>,
) {
    let raw = event
        .thread_settings
        .as_ref()
        .and_then(|settings| settings.get("service_tier"));
    let tier = match raw {
        None | Some(Value::Null) => None,
        Some(value) => {
            let tier = value
                .as_str()
                .and_then(|value| codex_service_tier(Some(value)));
            if tier.is_none() && !state.ledger_only {
                ops.push(drift(
                    ctx,
                    stream_id,
                    "event_msg/thread_settings_applied",
                    "unknown_service_tier",
                    &[],
                    ts,
                ));
            }
            tier
        }
    };
    state.service_tier = tier;
    if state.tier_settled {
        return;
    }
    state.tier_settled = true;
    let held = std::mem::take(&mut state.untiered);
    let Some(tier) = tier else {
        return;
    };
    for request in held {
        push_usage(
            ctx,
            sample_session(ctx, state),
            &request.target,
            request.ts,
            request.model,
            &request.usage,
            request.searches,
            request.aggregate,
            Some(tier),
            state.route.clone(),
            ops,
        );
    }
}

/// A guardian review is ledger-only for the timeline, but its requests cost
/// money (§4.1.3 rule 5): with the parent proven by the header, only the
/// facts a usage record needs are read, and the usage is emitted as samples
/// on the parent session. Nothing else — no rows, assets or drift — leaves
/// the raw ledger.
pub(crate) fn usage_only(
    ctx: &mut NormalizeCtx,
    state: &mut CodexState,
    stream_id: &str,
    text: &str,
) -> LineOutcome {
    if state.parent_session.is_none() {
        return LineOutcome::parsed(Vec::new());
    }
    let Ok(ParsedLine::Known(record)) = parse_line(text) else {
        return LineOutcome::parsed(Vec::new());
    };
    let ts = rfc3339_ms(&record.timestamp);
    let mut ops = Vec::new();
    match &record.payload {
        RolloutPayload::TurnContext(turn) => turn_context(state, turn),
        RolloutPayload::TokenUsageRecord(item) => {
            token_usage_record(ctx, state, item, record.ordinal, ts, &mut ops);
        }
        RolloutPayload::EventMsg(event) => match event.event_type.as_str() {
            "token_count" => token_count(ctx, state, event, record.ordinal, ts, &mut ops),
            "thread_settings_applied" => {
                thread_settings_applied(ctx, state, stream_id, event, ts, &mut ops);
            }
            _ => {}
        },
        RolloutPayload::ResponseItem(item) if billable_search(item) => {
            state.request_searches += 1;
        }
        _ => {}
    }
    LineOutcome::parsed(ops)
}

/// Only a search action is a billable web search call; opening or finding
/// in a page is not counted (RESEARCH_PLAN §5.13).
pub(crate) fn billable_search(item: &ResponseItem) -> bool {
    item.item_type == "web_search_call"
        && item
            .action
            .as_ref()
            .and_then(|action| action.get("type"))
            .and_then(Value::as_str)
            == Some("search")
}

/// Emit an assistant-authored row; when it survives projection, the request
/// in flight produced it and its token count belongs to it.
pub(crate) fn push_response(state: &mut CodexState, row: MessageRow, ops: &mut Vec<IngestOp>) {
    let timeline_id = row.timeline_id.clone();
    if IngestOp::push_message(ops, row) {
        begin_response(state, &timeline_id);
    }
}

/// Point the next usage record at a newly emitted, surviving assistant row.
/// A codex request ends in a message OR a tool call, and its usage record
/// lands after whichever it was, so the latest such row owns the usage.
pub(crate) fn begin_response(state: &mut CodexState, timeline_id: &str) {
    state.request_row = Some(timeline_id.to_owned());
}

/// Requests emitted before the file's first settings snapshot, held until it
/// arrives (CODEX-1). Bounded so a file that never records its settings
/// keeps a small checkpoint; locally no first turn exceeded 512 requests.
const UNTIERED_CAP: usize = 512;

/// Where one request's usage landed, enough to re-emit it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum UsageTarget {
    /// The request's surviving assistant row in this session.
    Row { timeline_id: String },
    /// A `session_usage` sample of this session, or of the parent session
    /// for a guardian review.
    Sample { key: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct UntieredRequest {
    target: UsageTarget,
    ts: Ms,
    model: Option<String>,
    usage: RawUsage,
    searches: i64,
    aggregate: bool,
}

/// The usage of a guardian review lands on the session that ran it, as
/// samples (§4.1.3 rule 5): the review's own file is ledger-only and never
/// has rows. Otherwise the request's latest surviving row owns it, and a
/// request without one becomes a sample of this session.
fn target(
    ctx: &NormalizeCtx,
    state: &CodexState,
    row: Option<String>,
    native_key: &str,
) -> UsageTarget {
    match (&state.parent_session, row) {
        (Some(parent), _) => {
            let child = state.thread_id.as_deref().unwrap_or(&ctx.session_id);
            UsageTarget::Sample {
                key: ids::usage_id(parent, &format!("guardian:{child}:{native_key}")),
            }
        }
        (None, Some(timeline_id)) => UsageTarget::Row { timeline_id },
        (None, None) => UsageTarget::Sample {
            key: ids::usage_id(&ctx.session_id, native_key),
        },
    }
}

/// The session a sample of this file belongs to: the parent's for a
/// guardian review, otherwise this one.
fn sample_session<'a>(ctx: &'a NormalizeCtx, state: &'a CodexState) -> &'a str {
    state.parent_session.as_deref().unwrap_or(&ctx.session_id)
}

#[allow(clippy::too_many_arguments)]
fn emit(
    ctx: &NormalizeCtx,
    state: &mut CodexState,
    target: UsageTarget,
    ts: Option<Ms>,
    usage: RawUsage,
    searches: i64,
    aggregate: bool,
    ops: &mut Vec<IngestOp>,
) {
    let at = ts.unwrap_or_default();
    let model = state.model.clone();
    push_usage(
        ctx,
        sample_session(ctx, state),
        &target,
        at,
        model.clone(),
        &usage,
        searches,
        aggregate,
        state.service_tier,
        state.route.clone(),
        ops,
    );
    if !state.tier_settled && state.service_tier.is_none() && state.untiered.len() < UNTIERED_CAP {
        state.untiered.push(UntieredRequest {
            target,
            ts: at,
            model,
            usage,
            searches,
            aggregate,
        });
    }
}

#[allow(clippy::too_many_arguments)]
fn push_usage(
    ctx: &NormalizeCtx,
    sample_session: &str,
    target: &UsageTarget,
    ts: Ms,
    model: Option<String>,
    usage: &RawUsage,
    searches: i64,
    aggregate: bool,
    service_tier: Option<ServiceTier>,
    route: Option<String>,
    ops: &mut Vec<IngestOp>,
) {
    let tokens = TokenSnapshot {
        web_searches: (searches > 0).then_some(searches),
        service_tier,
        route,
        scope: if aggregate {
            UsageScope::Aggregate
        } else {
            UsageScope::Request
        },
        ..usage.exclusive()
    };
    match target {
        UsageTarget::Row { timeline_id } => ops.push(IngestOp::Usage {
            identity: None,
            session_id: ctx.session_id.clone(),
            response: timeline_id.clone(),
            ts,
            model,
            tokens,
        }),
        UsageTarget::Sample { key } => ops.push(IngestOp::SessionUsage(SessionUsageRow {
            identity: None,
            session_id: sample_session.to_owned(),
            key: key.clone(),
            ts,
            model,
            tokens,
        })),
    }
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

/// Where a rollout's own usage begins. A paginated child's inherited prefix
/// sits below `subagent_history_start_ordinal`; a user fork and a legacy
/// subagent instead copy the parent's history (token counts included) in one
/// append whose timestamps fall in the file's first second
/// (RESEARCH_PLAN §5.13). Both copies are one burst right after the header,
/// and only a burst is skipped: the boundary alone proves nothing, because a
/// guardian rollout and a file migrated by `legacy_to_paginated_v1` carry a
/// boundary one past their last record, below which everything is the
/// child's own work (CODEX-4).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum ReplayGate {
    #[default]
    Open,
    /// Records below `start` count only outside the head burst; a record at
    /// or above it is the child's own whatever its timestamp.
    Prefix {
        start: u64,
        started_ms: Option<Ms>,
    },
    HeadBurst {
        started_ms: Option<Ms>,
    },
}

impl ReplayGate {
    /// Only a file whose header says its history was copied gets a gate: a
    /// boundary, a fork source, a parent, or a spawn descriptor. A plain
    /// session written by any CLI is open from its first record (CODEX-8).
    pub(crate) fn for_session(meta: &SessionMeta, ts: Option<Ms>) -> Self {
        let copied_history = meta.forked_from_id.is_some()
            || meta.parent_thread_id.is_some()
            || thread_spawn(&meta.source);
        match meta.subagent_history_start_ordinal {
            Some(start) => Self::Prefix {
                start,
                started_ms: ts,
            },
            None if copied_history => Self::HeadBurst { started_ms: ts },
            None => Self::Open,
        }
    }

    fn skips(&mut self, ordinal: Option<u64>, ts: Option<Ms>) -> bool {
        let started_ms = match *self {
            Self::Open => return false,
            Self::Prefix { start, started_ms } => {
                if ordinal.is_some_and(|ordinal| ordinal >= start) {
                    *self = Self::Open;
                    return false;
                }
                started_ms
            }
            Self::HeadBurst { started_ms } => started_ms,
        };
        match burst(started_ms, ts) {
            Some(true) => true,
            Some(false) => {
                *self = Self::Open;
                false
            }
            None => false,
        }
    }
}

/// A copied history lands in one append within the file's first second.
fn burst(started_ms: Option<Ms>, ts: Option<Ms>) -> Option<bool> {
    Some(ts? - started_ms? <= 1_000)
}

/// A structured spawn descriptor, on newer writers an object and on the
/// state database's rows a JSON string.
fn thread_spawn(source: &Value) -> bool {
    let decoded = source
        .as_str()
        .and_then(|encoded| serde_json::from_str::<Value>(encoded).ok());
    decoded
        .as_ref()
        .unwrap_or(source)
        .pointer("/subagent/thread_spawn")
        .is_some()
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
