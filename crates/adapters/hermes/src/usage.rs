//! `session_model_usage` rows → `session_usage` samples (ARCHITECTURE
//! §4.1.3, RESEARCH_PLAN §5.13).
//!
//! One Hermes row is a running total per session × model × billing provider
//! × base URL × billing mode × task, upserted per API call (0.21.4
//! `hermes_state_usage.py:52-71`; the same statement in 0.20.5
//! `hermes_state.py:8758-8777`). The row is the unit of evidence: nothing
//! per call survives except `api_call_count`, `first_seen`, and `last_seen`.

use rosetta_adapter_support::adapter::NormalizeCtx;
use rosetta_core::rows::{Ms, SessionUsageRow, TokenSnapshot, UsageScope};
use rosetta_core::{IngestOp, ids};

use crate::normalize::{drift, seconds_ms};
use crate::raw::state::SessionModelUsageRow;
use crate::state::HermesState;

/// Reasoning larger than the output that should contain it.
pub const USAGE_SUBSET_EXCEEDS_TOTAL: &str = "usage_subset_exceeds_total";
/// The main-loop requests of this session were made by the Codex app-server,
/// whose rollout the Codex adapter ingests from `~/.codex/sessions`; Hermes
/// keeps only each turn's last request, so its total is neither complete nor
/// exclusive and is withheld.
pub const USAGE_RECORDED_BY_CODEX: &str = "usage_recorded_by_codex_runtime";
/// A provider-billed amount the row cannot prove covers every call.
pub const USAGE_BILLED_UNPROVEN: &str = "usage_billed_unproven";

/// `sessions.billing_mode` after a `/model` switch persists the agent's
/// `api_mode` (0.21.4 `agent/agent_runtime_helpers.py:2264-2275`), which
/// `_record_model_usage` copies onto later main-loop rows
/// (`hermes_state_usage.py:358-367`).
const CODEX_APP_SERVER_MODE: &str = "codex_app_server";
/// `api_mode` values whose wire never carries a reasoning count
/// (`agent/agent_init.py:392-402` selects them).
const ANTHROPIC_WIRE_MODES: [&str; 2] = ["anthropic_messages", "bedrock_converse"];
/// Providers that always ride the Anthropic Messages / Bedrock Converse wire
/// (`agent/agent_init.py:394-402`).
const ANTHROPIC_WIRE_PROVIDERS: [&str; 2] = ["anthropic", "bedrock"];
/// Providers served by the Gemini native adapter
/// (`agent/auxiliary_client.py:6403`, `agent/agent_runtime_helpers.py:1858-1861`).
const GEMINI_NATIVE_PROVIDERS: [&str; 4] =
    ["gemini", "google", "google-gemini", "google-ai-studio"];

pub fn project_session_usage(
    ctx: &mut NormalizeCtx,
    stream_id: &str,
    row: &SessionModelUsageRow,
    ops: &mut Vec<IngestOp>,
) {
    let ts = sample_ts(row);
    if recorded_by_codex(HermesState::of(ctx), row) {
        ops.push(drift(
            ctx,
            stream_id,
            "session_model_usage",
            USAGE_RECORDED_BY_CODEX,
            &[],
            Some(ts),
        ));
        return;
    }
    // The native composite key is the table's replacement identity. Hashing
    // it keeps provider URLs (which can contain credentials) out of the
    // projection while preserving exact replacement semantics.
    let native_key = serde_json::to_string(&(
        &row.model,
        &row.billing_provider,
        &row.billing_base_url,
        &row.billing_mode,
        &row.task,
    ))
    .expect("usage key serializes");
    let (tokens, anomalies) = usage_snapshot(row);
    for (code, path) in anomalies {
        ops.push(drift(
            ctx,
            stream_id,
            "session_model_usage",
            code,
            &[path.to_owned()],
            Some(ts),
        ));
    }
    ops.push(IngestOp::SessionUsage(SessionUsageRow {
        identity: None,
        session_id: ctx.session_id.clone(),
        key: ids::usage_id(&ctx.session_id, &native_key),
        ts,
        model: Some(row.model.clone()),
        tokens,
    }));
}

/// When the whole-row sample is dated. The row spans `first_seen..last_seen`
/// with no per-call stamps, so any single instant is wrong for a long
/// session and a price change or time-of-day window inside the span reprices
/// every call. `last_seen` is kept: it is the instant the total became true,
/// it moves with the row on every live replacement, and `first_seen` is no
/// less wrong (the row cannot say how its calls are distributed).
fn sample_ts(row: &SessionModelUsageRow) -> Ms {
    row.last_seen
        .or(row.first_seen)
        .and_then(seconds_ms)
        .unwrap_or(0)
}

/// Main-loop rows of a Codex app-server session. The app-server records one
/// `api_call_count = 1` delta per turn from `thread/tokenUsage/updated`'s
/// `last` request (0.21.4 `agent/codex_runtime.py:148-215`,
/// `agent/transports/codex_app_server_session.py:786-799`) while its own
/// rollout in `~/.codex/sessions` carries every request. Auxiliary rows
/// (`task` set: title generation, vision, compression, background review
/// forks, which run on `codex_responses`, `agent/background_review.py:224-232`)
/// are Hermes-side calls the rollout never sees and stay.
fn recorded_by_codex(state: &HermesState, row: &SessionModelUsageRow) -> bool {
    row.task.is_empty() && (state.codex_runtime || row.billing_mode == CODEX_APP_SERVER_MODE)
}

/// Map one running total into the exclusive buckets. Hermes' `input_tokens`
/// is already uncached and its `output_tokens` includes `reasoning_tokens`
/// (0.21.4 `agent/usage_pricing.py:517-546`). Reasoning larger than output
/// means the provider reported it exclusively: both are kept and flagged.
/// Only a provider-reported `actual` cost is billing; estimates and
/// subscription-included rows are not. Returns the drift codes the row
/// earned, each with the field it concerns.
fn usage_snapshot(
    row: &SessionModelUsageRow,
) -> (TokenSnapshot, Vec<(&'static str, &'static str)>) {
    let mut anomalies = Vec::new();
    let exclusive_reasoning = row.reasoning_tokens > row.output_tokens;
    if exclusive_reasoning {
        anomalies.push((USAGE_SUBSET_EXCEEDS_TOTAL, "reasoning_tokens"));
    }
    let output = if exclusive_reasoning {
        row.output_tokens
    } else {
        row.output_tokens - row.reasoning_tokens
    };
    let (billed_pico_usd, billed_unproven) = billed(row);
    if billed_unproven {
        anomalies.push((USAGE_BILLED_UNPROVEN, "actual_cost_usd"));
    }
    let tokens = TokenSnapshot {
        input: Some(row.input_tokens),
        output: Some(output),
        cache_read: Some(row.cache_read_tokens),
        cache_write: Some(row.cache_write_tokens),
        thinking: reasoning_reported(row).then_some(row.reasoning_tokens),
        route: rosetta_core::rows::canonical_route(&row.billing_provider),
        billed_pico_usd,
        // One row per session × model × billing route × task accumulates every
        // call; only a single-call row is one request.
        scope: if row.api_call_count == 1 {
            UsageScope::Request
        } else {
            UsageScope::Aggregate
        },
        requests: (row.api_call_count > 1).then_some(row.api_call_count),
        ..TokenSnapshot::default()
    };
    (tokens, anomalies)
}

/// The provider-billed amount, and whether one was withheld. The writer sums
/// `actual_cost_usd` over the calls that reported one while `cost_status`
/// is the LAST call's status (`COALESCE(excluded.cost_status, cost_status)`,
/// `hermes_state_usage.py:67-69`), so a multi-call row can pair a partial sum
/// with `actual` or a full sum with `estimated`. Only a single-call row
/// proves its amount covers every call.
fn billed(row: &SessionModelUsageRow) -> (Option<i64>, bool) {
    let reported = row.actual_cost_usd.is_finite() && row.actual_cost_usd > 0.0;
    if !reported {
        return (None, false);
    }
    let proven = row.cost_status.as_deref() == Some("actual") && row.api_call_count == 1;
    if proven {
        (Some((row.actual_cost_usd * 1e12).round() as i64), false)
    } else {
        (None, true)
    }
}

/// Whether the row's route can report a reasoning count at all. Hermes reads
/// reasoning only from OpenAI-style `output_tokens_details` /
/// `completion_tokens_details` (0.21.4 `agent/usage_pricing.py:541-546`).
/// The Anthropic Messages and Bedrock Converse wires never split thinking out
/// of `output_tokens` (`_ANTHROPIC_USAGE_SHAPE`, `:496-498`), and the Gemini
/// native adapter translates only `candidatesTokenCount`
/// (`agent/gemini_native_adapter.py:552-557`, writer bug HERMES-2). On those
/// routes a zero is "never reported", not "none". A positive count is proof
/// the route reported it, whatever the rule says.
fn reasoning_reported(row: &SessionModelUsageRow) -> bool {
    row.reasoning_tokens > 0
        || !wire_hides_reasoning(
            &row.billing_provider,
            &row.billing_base_url,
            &row.billing_mode,
        )
}

fn wire_hides_reasoning(provider: &str, base_url: &str, mode: &str) -> bool {
    let provider = provider.trim().to_ascii_lowercase();
    let mode = mode.trim().to_ascii_lowercase();
    let url = base_url.trim().trim_end_matches('/').to_ascii_lowercase();
    if ANTHROPIC_WIRE_MODES.contains(&mode.as_str())
        || ANTHROPIC_WIRE_PROVIDERS.contains(&provider.as_str())
    {
        return true;
    }
    // Host and path rules of `_resolve_api_mode` (`agent/agent_init.py:394-402`):
    // api.anthropic.com, third-party `/anthropic` endpoints, Bedrock runtime hosts.
    if host_of(&url).is_some_and(|host| host == "api.anthropic.com")
        || url.ends_with("/anthropic")
        || host_of(&url).is_some_and(|host| {
            host.starts_with("bedrock-runtime.") && host.ends_with(".amazonaws.com")
        })
    {
        return true;
    }
    // Gemini native REST (`is_native_gemini_base_url`,
    // `agent/gemini_native_adapter.py:160-165`): the provider's default endpoint,
    // unless the row names the OpenAI-compatible `/openai` surface.
    let openai_surface = url.ends_with("/openai");
    let native_host = url.contains("generativelanguage.googleapis.com");
    (GEMINI_NATIVE_PROVIDERS.contains(&provider.as_str()) && (url.is_empty() || !openai_surface))
        || (native_host && !openai_surface)
}

fn host_of(url: &str) -> Option<&str> {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority = rest.split(['/', '?', '#']).next()?;
    let host = authority.rsplit('@').next()?;
    let host = host.split(':').next()?;
    (!host.is_empty()).then_some(host)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rosetta_core::rows::UsageScope;

    fn row(provider: &str, base_url: &str, mode: &str, task: &str) -> SessionModelUsageRow {
        serde_json::from_value(serde_json::json!({
            "session_id": "s",
            "model": "synthetic-model",
            "billing_provider": provider,
            "billing_base_url": base_url,
            "billing_mode": mode,
            "task": task,
            "api_call_count": 3,
            "input_tokens": 300,
            "output_tokens": 90,
            "cache_read_tokens": 30,
            "cache_write_tokens": 0,
            "reasoning_tokens": 0,
            "estimated_cost_usd": 0.0,
            "actual_cost_usd": 0.0,
            "cost_status": "estimated",
            "cost_source": "official_docs_snapshot",
            "first_seen": 1.0,
            "last_seen": 2.0
        }))
        .unwrap()
    }

    #[test]
    fn reasoning_is_unreported_on_wires_that_never_split_it() {
        for (provider, url, mode) in [
            ("anthropic", "https://api.anthropic.com", ""),
            (
                "nous",
                "https://inference-api.nousresearch.com/v1",
                "anthropic_messages",
            ),
            ("bedrock", "", ""),
            (
                "custom",
                "https://bedrock-runtime.us-east-1.amazonaws.com",
                "",
            ),
            ("minimax", "https://api.minimax.io/anthropic", ""),
            ("custom", "https://api.anthropic.com/v1/", ""),
            ("gemini", "", ""),
            (
                "google",
                "https://generativelanguage.googleapis.com/v1beta",
                "",
            ),
            (
                "custom",
                "https://generativelanguage.googleapis.com/v1beta/",
                "",
            ),
        ] {
            let (tokens, _) = usage_snapshot(&row(provider, url, mode, ""));
            assert_eq!(tokens.thinking, None, "{provider} {url} {mode}");
            assert_eq!(tokens.output, Some(90));
        }
    }

    #[test]
    fn reasoning_zero_is_a_value_on_openai_style_wires() {
        for (provider, url, mode) in [
            ("copilot", "https://api.githubcopilot.com", ""),
            (
                "openai-codex",
                "https://chatgpt.com/backend-api/codex",
                "subscription_included",
            ),
            ("custom", "http://127.0.0.1:61299/v1", ""),
            (
                "openrouter",
                "https://openrouter.ai/api/v1",
                "official_models_api",
            ),
            (
                "gemini",
                "https://generativelanguage.googleapis.com/v1beta/openai",
                "",
            ),
            ("nous", "https://inference-api.nousresearch.com/v1", ""),
        ] {
            let (tokens, _) = usage_snapshot(&row(provider, url, mode, ""));
            assert_eq!(tokens.thinking, Some(0), "{provider} {url} {mode}");
        }
    }

    #[test]
    fn a_positive_reasoning_count_is_proof_on_any_wire() {
        let mut anthropic = row("anthropic", "", "", "");
        anthropic.reasoning_tokens = 40;
        let (tokens, anomalies) = usage_snapshot(&anthropic);
        assert_eq!(tokens.thinking, Some(40));
        assert_eq!(tokens.output, Some(50));
        assert!(anomalies.is_empty());
    }

    #[test]
    fn exclusive_reasoning_keeps_both_and_flags() {
        let mut exclusive = row("custom", "http://127.0.0.1:1/v1", "", "");
        exclusive.reasoning_tokens = 120;
        let (tokens, anomalies) = usage_snapshot(&exclusive);
        assert_eq!(tokens.output, Some(90));
        assert_eq!(tokens.thinking, Some(120));
        assert_eq!(
            anomalies,
            vec![(USAGE_SUBSET_EXCEEDS_TOTAL, "reasoning_tokens")]
        );
    }

    #[test]
    fn billed_only_when_a_single_call_reported_actual_cost() {
        let mut single = row(
            "openrouter",
            "https://openrouter.ai/api/v1",
            "official_models_api",
            "",
        );
        single.api_call_count = 1;
        single.cost_status = Some("actual".to_owned());
        single.actual_cost_usd = 0.0125;
        let (tokens, anomalies) = usage_snapshot(&single);
        assert_eq!(tokens.billed_pico_usd, Some(12_500_000_000));
        assert_eq!(tokens.scope, UsageScope::Request);
        assert!(anomalies.is_empty());

        // Three calls, status from the last one: the sum may cover one call or all.
        let mut mixed = single;
        mixed.api_call_count = 3;
        let (tokens, anomalies) = usage_snapshot(&mixed);
        assert_eq!(tokens.billed_pico_usd, None);
        assert_eq!(tokens.requests, Some(3));
        assert_eq!(anomalies, vec![(USAGE_BILLED_UNPROVEN, "actual_cost_usd")]);

        // An amount left over from earlier calls under a later estimate.
        let mut stale = mixed;
        stale.api_call_count = 1;
        stale.cost_status = Some("estimated".to_owned());
        let (tokens, anomalies) = usage_snapshot(&stale);
        assert_eq!(tokens.billed_pico_usd, None);
        assert_eq!(anomalies, vec![(USAGE_BILLED_UNPROVEN, "actual_cost_usd")]);

        // `actual` with nothing reported is not billing either.
        let mut empty = stale;
        empty.cost_status = Some("actual".to_owned());
        empty.actual_cost_usd = 0.0;
        let (tokens, anomalies) = usage_snapshot(&empty);
        assert_eq!(tokens.billed_pico_usd, None);
        assert!(anomalies.is_empty());
    }

    #[test]
    fn codex_runtime_covers_main_loop_rows_only() {
        let state = HermesState {
            codex_runtime: true,
        };
        assert!(recorded_by_codex(
            &state,
            &row("openai-codex", "", "subscription_included", "")
        ));
        assert!(!recorded_by_codex(
            &state,
            &row("openai-codex", "", "", "title_generation")
        ));
        assert!(!recorded_by_codex(
            &state,
            &row("openai", "", "codex_responses", "background_review")
        ));
        let plain = HermesState::default();
        assert!(!recorded_by_codex(
            &plain,
            &row("openai-codex", "", "subscription_included", "")
        ));
        assert!(recorded_by_codex(
            &plain,
            &row(
                "openai",
                "https://api.openai.com/v1",
                "codex_app_server",
                ""
            )
        ));
    }

    #[test]
    fn samples_are_dated_at_last_seen() {
        let mut dated = row("custom", "", "", "");
        assert_eq!(sample_ts(&dated), 2_000);
        dated.last_seen = None;
        assert_eq!(sample_ts(&dated), 1_000);
        dated.first_seen = None;
        assert_eq!(sample_ts(&dated), 0);
    }

    #[test]
    fn hosts_are_read_without_credentials_or_ports() {
        assert_eq!(
            host_of("https://user:pw@api.anthropic.com:443/v1"),
            Some("api.anthropic.com")
        );
        assert_eq!(host_of("http://127.0.0.1:61299/v1"), Some("127.0.0.1"));
        assert_eq!(host_of(""), None);
    }
}
