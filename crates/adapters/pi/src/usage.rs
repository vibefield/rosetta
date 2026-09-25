//! Pi usage records → canonical token snapshots (ARCHITECTURE §4.1.3, the Pi
//! row of RESEARCH_PLAN §5.13).

use rosetta_adapter_support::adapter::NormalizeCtx;
use rosetta_core::rows::{Ms, SessionUsageRow, TokenSnapshot, UsageScope, canonical_route};
use rosetta_core::{IngestOp, fold, ids};

use crate::normalize::drift;
use crate::raw::session::{AssistantMessage, Usage, UsageEntry};
use crate::state::PiState;
use crate::time::rfc3339_ms;

/// The one `usage` entry kind Pi itself writes: a prompt-cache warming request
/// (`cache-warmer.ts:331-349` at pi-mono a8ed497).
const CACHE_WARM: &str = "cache_warm";

/// Map Pi's usage record into the exclusive buckets. `input` already excludes
/// cache on every Pi API, `cacheWrite` includes `cacheWrite1h`, and `output`
/// includes `reasoning`. Pi's `cost` is computed from its own price table, so
/// it is never billing. `None` when the request never reported usage. The
/// flag says whether a subset exceeded its total (clamped, reported as drift).
pub(crate) fn usage_snapshot(
    usage: &Usage,
    provider: Option<&str>,
    scope: UsageScope,
) -> Option<(TokenSnapshot, bool)> {
    if never_reported(usage) {
        return None;
    }
    let count = |value: u64| i64::try_from(value).ok();
    let (cache_write, cache_write_1h, write_clamped) = fold::split_subset(
        usage.cache_write.and_then(count),
        usage.cache_write_1h.and_then(count),
    );
    let (output, thinking, output_clamped) = fold::split_subset(
        usage.output.and_then(count),
        usage.reasoning.and_then(count),
    );
    let tokens = TokenSnapshot {
        input: usage.input.and_then(count),
        output,
        cache_read: usage.cache_read.and_then(count),
        cache_write,
        cache_write_1h,
        thinking,
        route: provider.and_then(provider_route),
        scope,
        ..TokenSnapshot::default()
    };
    Some((tokens, write_clamped || output_clamped))
}

/// Every Pi API starts a response with all-zero usage and no `reasoning` key
/// (`openai-completions.ts:314-321`, `openai-responses.ts:129-136`,
/// `google-generative-ai.ts:74-81`, `anthropic-messages.ts:529-536` at
/// pi-mono a8ed497) and only overwrites it from a usage chunk, which is also
/// what sets `reasoning` where the API exposes one (`openai-completions.ts:1546`,
/// `google-generative-ai.ts:239`). An endpoint that never sends usage leaves
/// that placeholder on disk: unreported, never a $0 request.
fn never_reported(usage: &Usage) -> bool {
    [
        usage.input,
        usage.output,
        usage.cache_read,
        usage.cache_write,
        usage.total_tokens,
    ]
    .iter()
    .all(|count| count.unwrap_or(0) == 0)
        && usage.reasoning.is_none()
        && usage.cache_write_1h.is_none()
}

/// Custom provider ids are free-form (`model-config.ts:243` keys `providers`
/// by any string). Only the leading segment is the route: a `/` inside the id
/// would otherwise read as a qualified route without a cited premium.
pub(crate) fn provider_route(provider: &str) -> Option<String> {
    canonical_route(provider.split('/').next().unwrap_or_default())
}

/// The served model: `responseModel` when the provider echoed one, else the
/// requested id — the precedence Pi's own accounting uses
/// (`cache-warmer.ts:343`).
pub(crate) fn assistant_model(message: &AssistantMessage) -> Option<&str> {
    non_empty(message.response_model.as_deref()).or_else(|| non_empty(Some(&message.model)))
}

pub(crate) fn non_empty(text: Option<&str>) -> Option<&str> {
    text.filter(|text| !text.is_empty())
}

/// A `usage` entry is one recorded request outside the conversation: a
/// session sample keyed by the entry id, timed and modelled by the entry
/// (`session-manager.ts:1242-1256`). `cache_warm` is exactly one request; any
/// other kind is still usage (docs/session-format.md "UsageEntry") whose
/// request count Pi does not state, so it is an uncounted aggregate. A fork's
/// copied history keeps none, as with every other copied usage.
pub(crate) fn usage_entry(
    ctx: &NormalizeCtx,
    state: &PiState,
    stream_id: &str,
    entry: &UsageEntry,
    ops: &mut Vec<IngestOp>,
) {
    let ts = rfc3339_ms(&entry.timestamp);
    if state.inherited_copy(ts) {
        return;
    }
    let scope = if entry.kind == CACHE_WARM {
        UsageScope::Request
    } else {
        UsageScope::Aggregate
    };
    let Some((tokens, clamped)) = usage_snapshot(&entry.usage, Some(&entry.provider), scope) else {
        return;
    };
    if clamped {
        ops.push(usage_drift(ctx, stream_id, "usage", ts));
    }
    ops.push(IngestOp::SessionUsage(SessionUsageRow {
        identity: None,
        session_id: ctx.session_id.clone(),
        key: ids::usage_id(&ctx.session_id, &entry.id),
        ts: ts.unwrap_or(0),
        model: non_empty(Some(&entry.model)).map(str::to_owned),
        tokens,
    }));
}

/// Usage of a compaction or branch summary: `requests` summarization requests
/// recorded as one total, none on a fork's copied history.
pub(crate) fn summary_usage(
    ctx: &NormalizeCtx,
    state: &PiState,
    usage: Option<&Usage>,
    requests: i64,
    at: Option<Ms>,
    ops: &mut Vec<IngestOp>,
) -> Option<TokenSnapshot> {
    if state.inherited_copy(at) {
        return None;
    }
    let scope = if requests > 1 {
        UsageScope::Aggregate
    } else {
        UsageScope::Request
    };
    let (mut tokens, clamped) = usage_snapshot(usage?, None, scope)?;
    if requests > 1 {
        tokens.requests = Some(requests);
    }
    if clamped {
        ops.push(usage_drift(ctx, "session", "summary", at));
    }
    Some(tokens)
}

/// A usage subset (reasoning, 1-hour writes) larger than its total.
pub(crate) fn usage_drift(
    ctx: &NormalizeCtx,
    stream_id: &str,
    record_kind: &str,
    at: Option<Ms>,
) -> IngestOp {
    drift(
        ctx,
        stream_id,
        record_kind,
        "usage_subset_exceeds_total",
        &["usage".to_owned()],
        at,
    )
}
