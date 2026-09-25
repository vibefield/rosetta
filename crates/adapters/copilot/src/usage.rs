//! Usage records (ARCHITECTURE §4.1.3, RESEARCH_PLAN §5.13): exact per-call
//! rows from the session store and per-model shutdown intervals from
//! events.jsonl, both mapped into the exclusive token buckets.
//!
//! One call is never counted twice. GitHub-metered usage (`total_nano_aiu`
//! above zero) is taken only from the store's `assistant_usage_events` rows,
//! which the runtime writes live from the ephemeral `assistant.usage` event
//! since CLI 1.0.69. Unmetered usage (BYOK, zero nano-AIU) is taken only from
//! the shutdown intervals, which are its only record. A writer older than
//! 1.0.69 has no store rows, so every one of its intervals counts. Each side
//! decides from its own record, so the rule holds whatever order the two
//! sources are ingested in. Its known gap: a metered interval whose store
//! rows were lost, and the unmetered calls of an interval that also metered
//! some, have no record until core's cross-file reconciliation can prefer
//! rows per session. The shutdown side lives here; the store rows are
//! `calls`.

use rosetta_adapter_support::adapter::NormalizeCtx;
use rosetta_core::rows::{Ms, ServiceTier, SessionUsageRow, TokenSnapshot, UsageScope};
use rosetta_core::{IngestOp, fold, ids};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::normalize::{CopilotState, drift};
use crate::raw::events::SessionShutdown;
use crate::raw::store::whole_number;
use crate::spec;

pub mod calls;

const SHUTDOWN: &str = "session.shutdown";
/// A native subset larger than the total that contains it (clamped, §4.1.3).
pub(crate) const USAGE_SUBSET_EXCEEDS_TOTAL: &str = "usage_subset_exceeds_total";
/// An optional cumulative counter went backwards without a reset.
const USAGE_COUNTER_REGRESSED: &str = "usage_counter_regressed";
/// Tokens grew in an interval whose request count did not, or was never
/// reported: the interval is an uncounted aggregate.
const USAGE_UNCOUNTED_INTERVAL: &str = "usage_uncounted_interval";

/// The first CLI whose runtime writes `assistant_usage_events` (changelog
/// 1.0.69, "Show exact local assistant usage in Chronicle and session SQL").
const STORE_USAGE_SINCE: [u64; 3] = [1, 0, 69];

/// Whether this writer records every metered call in the session store.
/// Rosetta has never met a Copilot writer below 1.0.80, so an unknown
/// version is treated as current rather than counting a metered call twice.
pub(crate) fn writer_persists_usage(version: Option<&str>) -> bool {
    version
        .and_then(parse_version)
        .is_none_or(|version| version >= STORE_USAGE_SINCE)
}

fn parse_version(text: &str) -> Option<[u64; 3]> {
    let core = text.trim().split(['-', '+']).next()?;
    let mut parts = core.split('.').map(|part| part.parse::<u64>().ok());
    Some([parts.next()??, parts.next()??, parts.next()??])
}

/// A usage record's model as the price resolver expects it. Copilot spells a
/// 1M-context serving `<model>-1m` (or `-1m-internal`) and the fast tier
/// `<model>-fast`; the resolver strips a `[1m]` context tag and prices the
/// tier from `service_tier`. Everything else, dotted ids included, is kept
/// verbatim and resolves through catalog aliases.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PricedModel {
    pub model: String,
    pub service_tier: Option<ServiceTier>,
}

pub fn priced_model(native: &str) -> PricedModel {
    let mut base = native.trim();
    let mut fast = false;
    let mut long_context = false;
    loop {
        if let Some(rest) = base
            .strip_suffix("-1m-internal")
            .or_else(|| base.strip_suffix("-1m"))
        {
            base = rest;
            long_context = true;
        } else if let Some(rest) = base.strip_suffix("-fast") {
            base = rest;
            fast = true;
        } else {
            break;
        }
    }
    if base.is_empty() {
        return PricedModel {
            model: native.to_owned(),
            service_tier: None,
        };
    }
    PricedModel {
        model: if long_context {
            format!("{base}[1m]")
        } else {
            base.to_owned()
        },
        service_tier: fast.then_some(ServiceTier::Fast),
    }
}

/// One model's native cumulative counters as a shutdown reports them:
/// `input` includes cache reads and writes, `output` includes reasoning. The
/// four token counters are required by the writer schema; reasoning, the
/// request count and nano-AIU are optional and stay unknown when a shutdown
/// omits them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ShutdownTotals {
    #[serde(default)]
    pub(crate) input: Option<i64>,
    #[serde(default)]
    pub(crate) output: Option<i64>,
    #[serde(default)]
    pub(crate) cache_read: Option<i64>,
    #[serde(default)]
    pub(crate) cache_write: Option<i64>,
    #[serde(default)]
    pub(crate) reasoning: Option<i64>,
    #[serde(default)]
    pub(crate) requests: Option<i64>,
    #[serde(default)]
    pub(crate) nano_aiu: Option<i64>,
}

impl ShutdownTotals {
    /// Read one `modelMetrics` entry; `None` without its `usage` object.
    fn read(metric: &Value) -> Option<Self> {
        let usage = metric.get("usage")?.as_object()?;
        let count = |key: &str| usage.get(key).and_then(whole_number);
        Some(Self {
            input: count("inputTokens"),
            output: count("outputTokens"),
            cache_read: count("cacheReadTokens"),
            cache_write: count("cacheWriteTokens"),
            reasoning: count("reasoningTokens"),
            requests: metric.pointer("/requests/count").and_then(whole_number),
            nano_aiu: metric.get("totalNanoAiu").and_then(whole_number),
        })
    }

    /// The interval since `last`. One of the four required counters going
    /// backwards means a new epoch (a crash lost the old totals) and the
    /// whole reading is the interval. Optional counters never decide a reset:
    /// one that went backwards on its own is malformed, so its delta is
    /// unknown and the second value reports it.
    pub(crate) fn since(self, last: Self) -> (Self, bool) {
        let required = [
            (self.input, last.input),
            (self.output, last.output),
            (self.cache_read, last.cache_read),
            (self.cache_write, last.cache_write),
        ];
        let reset = required
            .into_iter()
            .any(|(now, then)| matches!((now, then), (Some(now), Some(then)) if now < then));
        if reset {
            return (self, false);
        }
        let mut regressed = false;
        let mut delta = |now: Option<i64>, then: Option<i64>| match (now, then) {
            (Some(now), Some(then)) if now >= then => Some(now - then),
            (Some(_), Some(_)) => {
                regressed = true;
                None
            }
            (now, None) => now,
            (None, Some(_)) => None,
        };
        let interval = Self {
            input: delta(self.input, last.input),
            output: delta(self.output, last.output),
            cache_read: delta(self.cache_read, last.cache_read),
            cache_write: delta(self.cache_write, last.cache_write),
            reasoning: delta(self.reasoning, last.reasoning),
            requests: delta(self.requests, last.requests),
            nano_aiu: delta(self.nano_aiu, last.nano_aiu),
        };
        (interval, regressed)
    }

    /// No token moved: a resume that made no request.
    fn is_idle(&self) -> bool {
        [
            self.input,
            self.output,
            self.cache_read,
            self.cache_write,
            self.reasoning,
        ]
        .iter()
        .all(|count| count.is_none_or(|count| count == 0))
    }

    /// GitHub charged for this interval, so its calls are store rows.
    fn metered(&self) -> bool {
        self.nano_aiu.is_some_and(|nano| nano > 0)
    }
}

/// A shutdown's `modelMetrics` are cumulative per session × model and are
/// re-reported on every resume. Each model's interval since the previous
/// shutdown becomes one aggregate sample (RESEARCH_PLAN §5.13): `inputTokens`
/// includes cache reads and writes, `outputTokens` includes reasoning, and
/// `totalNanoAiu` is GitHub's metered charge (10⁹ nano-AIU per AI credit, one
/// credit = $0.01, so one nano-AIU is 10 pico-USD). A metered interval of a
/// writer that persists usage is skipped: its calls are the store's rows.
pub(crate) fn shutdown_usage(
    ctx: &mut NormalizeCtx,
    state: &mut CopilotState,
    event_id: &str,
    shutdown: &SessionShutdown,
    ts: Option<Ms>,
    ops: &mut Vec<IngestOp>,
) {
    let Some(metrics) = shutdown.model_metrics.as_object() else {
        return;
    };
    let store_records_metered_calls = writer_persists_usage(ctx.agent_version.as_deref());
    for (native_model, metric) in metrics {
        let Some(reading) = ShutdownTotals::read(metric) else {
            continue;
        };
        let last = state
            .model_totals
            .get(native_model)
            .copied()
            .unwrap_or_default();
        state.model_totals.insert(native_model.clone(), reading);
        let (interval, regressed) = reading.since(last);
        let path = [format!("modelMetrics.{native_model}")];
        if regressed {
            ops.push(drift(
                ctx,
                spec::EVENTS,
                SHUTDOWN,
                USAGE_COUNTER_REGRESSED,
                &path,
                ts,
            ));
        }
        if interval.is_idle() {
            continue;
        }
        if store_records_metered_calls && interval.metered() {
            continue;
        }
        let (tokens, clamped) = interval_snapshot(interval);
        if clamped {
            ops.push(drift(
                ctx,
                spec::EVENTS,
                SHUTDOWN,
                USAGE_SUBSET_EXCEEDS_TOTAL,
                &path,
                ts,
            ));
        }
        if interval.requests.is_none_or(|count| count <= 0) {
            ops.push(drift(
                ctx,
                spec::EVENTS,
                SHUTDOWN,
                USAGE_UNCOUNTED_INTERVAL,
                &path,
                ts,
            ));
        }
        let priced = priced_model(native_model);
        ops.push(IngestOp::SessionUsage(SessionUsageRow {
            identity: None,
            session_id: ctx.session_id.clone(),
            key: ids::usage_id(
                &ctx.session_id,
                &format!("shutdown:{event_id}:{native_model}"),
            ),
            ts: ts.unwrap_or_default(),
            model: Some(priced.model),
            tokens: TokenSnapshot {
                service_tier: priced.service_tier,
                ..tokens
            },
        }));
    }
}

/// The exclusive buckets of one interval and whether a subset was clamped.
pub(crate) fn interval_snapshot(interval: ShutdownTotals) -> (TokenSnapshot, bool) {
    let (input, input_clamped) =
        fold::exclusive_remainder(interval.input, &[interval.cache_read, interval.cache_write]);
    let (output, thinking, output_clamped) =
        fold::split_subset(interval.output, interval.reasoning);
    let tokens = TokenSnapshot {
        input,
        output,
        cache_read: interval.cache_read,
        cache_write: interval.cache_write,
        thinking,
        billed_pico_usd: interval
            .nano_aiu
            .filter(|nano| *nano > 0)
            .and_then(|nano| nano.checked_mul(10)),
        scope: if interval.requests == Some(1) {
            UsageScope::Request
        } else {
            UsageScope::Aggregate
        },
        requests: interval.requests.filter(|count| *count > 1),
        ..TokenSnapshot::default()
    };
    (tokens, input_clamped || output_clamped)
}

#[cfg(test)]
mod tests {
    use super::{
        PricedModel, ShutdownTotals, interval_snapshot, priced_model, writer_persists_usage,
    };
    use crate::normalize::{CopilotState, ingest_line};
    use rosetta_adapter_support::adapter::{LineStatus, NormalizeCtx};
    use rosetta_core::IngestOp;
    use rosetta_core::rows::{DriftFact, ServiceTier, SessionUsageRow, UsageScope};
    use serde_json::json;

    fn context(version: Option<&str>) -> NormalizeCtx {
        let mut ctx = NormalizeCtx::new("runtime", "session");
        ctx.agent_version = version.map(str::to_owned);
        ctx
    }

    fn shutdown_line(id: &str, ts: &str, data: serde_json::Value) -> String {
        let mut data = data;
        for (key, value) in [
            ("shutdownType", json!("routine")),
            ("totalApiDurationMs", json!(1)),
            ("sessionStartTime", json!(1_787_794_001_822_i64)),
            (
                "codeChanges",
                json!({"linesAdded": 0, "linesRemoved": 0, "filesModified": []}),
            ),
        ] {
            data.as_object_mut().unwrap().entry(key).or_insert(value);
        }
        json!({"id": id, "timestamp": ts, "parentId": null, "type": "session.shutdown", "data": data})
            .to_string()
    }

    fn samples(ops: &[IngestOp]) -> Vec<&SessionUsageRow> {
        ops.iter()
            .filter_map(|op| match op {
                IngestOp::SessionUsage(row) => Some(row),
                _ => None,
            })
            .collect()
    }

    fn drift_codes(ops: &[IngestOp]) -> Vec<&str> {
        ops.iter()
            .filter_map(|op| match op {
                IngestOp::Drift(DriftFact { error_code, .. }) => Some(error_code.as_str()),
                _ => None,
            })
            .collect()
    }

    /// James's real 1.0.80 session as its shutdown reported it (numbers).
    fn real_reading() -> serde_json::Value {
        json!({
            "gpt-5.6-terra": {
                "requests": {"count": 5, "cost": 0},
                "usage": {"inputTokens": 136_067, "outputTokens": 1_794, "cacheReadTokens": 76_154, "cacheWriteTokens": 59_898, "reasoningTokens": 362},
                "totalNanoAiu": 18_653_380_000_i64,
            }
        })
    }

    #[test]
    fn copilot_model_spellings_map_to_resolver_spellings() {
        let plain = |model: &str| PricedModel {
            model: model.to_owned(),
            service_tier: None,
        };
        assert_eq!(priced_model("gpt-5.6-terra"), plain("gpt-5.6-terra"));
        assert_eq!(priced_model("claude-fable-5.1"), plain("claude-fable-5.1"));
        assert_eq!(
            priced_model("claude-opus-5.5-1m"),
            plain("claude-opus-5.5[1m]")
        );
        assert_eq!(
            priced_model("claude-opus-5.5-1m-internal"),
            plain("claude-opus-5.5[1m]")
        );
        assert_eq!(
            priced_model("claude-opus-4.8-fast"),
            PricedModel {
                model: "claude-opus-4.8".to_owned(),
                service_tier: Some(ServiceTier::Fast),
            }
        );
        assert_eq!(
            priced_model("gpt-5.6-sol-fast"),
            PricedModel {
                model: "gpt-5.6-sol".to_owned(),
                service_tier: Some(ServiceTier::Fast),
            }
        );
        assert_eq!(
            priced_model("claude-opus-5.5-fast-1m"),
            PricedModel {
                model: "claude-opus-5.5[1m]".to_owned(),
                service_tier: Some(ServiceTier::Fast),
            }
        );
        assert_eq!(priced_model("-fast"), plain("-fast"));
    }

    #[test]
    fn writers_from_1_0_69_persist_usage() {
        assert!(!writer_persists_usage(Some("1.0.68")));
        assert!(writer_persists_usage(Some("1.0.69")));
        assert!(writer_persists_usage(Some("1.0.80")));
        assert!(writer_persists_usage(Some("1.0.88-1")));
        assert!(writer_persists_usage(Some("2.0.0")));
        assert!(!writer_persists_usage(Some("0.0.422")));
        assert!(writer_persists_usage(None));
        assert!(writer_persists_usage(Some("not-a-version")));
    }

    #[test]
    fn shutdown_intervals_preserve_reported_zeroes() {
        let (tokens, _) = interval_snapshot(ShutdownTotals {
            input: Some(128),
            output: Some(32),
            cache_read: Some(0),
            cache_write: Some(0),
            reasoning: Some(0),
            requests: Some(1),
            nano_aiu: Some(0),
        });
        assert_eq!(tokens.input, Some(128));
        assert_eq!(tokens.cache_read, Some(0));
        assert_eq!(tokens.thinking, Some(0));
        assert_eq!(tokens.billed_pico_usd, None);
        assert_eq!(tokens.scope, UsageScope::Request);
    }

    #[test]
    fn shutdown_intervals_are_per_model_deltas_in_exclusive_buckets() {
        let first = ShutdownTotals {
            input: Some(136_067),
            output: Some(1_794),
            cache_read: Some(76_154),
            cache_write: Some(59_898),
            reasoning: Some(362),
            requests: Some(5),
            nano_aiu: Some(18_653_380_000),
        };
        let (interval, regressed) = first.since(Default::default());
        assert!(!regressed);
        let (tokens, clamped) = interval_snapshot(interval);
        assert!(!clamped);
        // Copilot's inclusive input: 15 + 76,154 + 59,898 = 136,067.
        assert_eq!(tokens.input, Some(15));
        assert_eq!((tokens.output, tokens.thinking), (Some(1_432), Some(362)));
        assert_eq!(tokens.total_tokens(), Some(136_067 + 1_794));
        // $0.1865338 billed: nano-AIU / 10¹¹ USD = × 10 pico-USD.
        assert_eq!(tokens.billed_pico_usd, Some(186_533_800_000));
        assert_eq!(tokens.scope, UsageScope::Aggregate);
        assert_eq!(tokens.requests, Some(5));

        // A resume re-reports the running total: only the growth counts.
        let resumed = ShutdownTotals {
            input: Some(136_267),
            requests: Some(6),
            ..first
        };
        let (interval, _) = resumed.since(first);
        assert_eq!(interval.input, Some(200));
        assert_eq!(interval.requests, Some(1));
        let (one, _) = interval_snapshot(interval);
        assert_eq!((one.scope, one.requests), (UsageScope::Request, None));
        // A required total that went backwards is a new epoch, counted whole.
        assert_eq!(first.since(resumed), (first, false));
    }

    #[test]
    fn optional_counters_never_start_an_epoch() {
        let first = ShutdownTotals {
            input: Some(100),
            output: Some(50),
            cache_read: Some(10),
            cache_write: Some(20),
            reasoning: Some(30),
            requests: Some(2),
            nano_aiu: Some(1_000),
        };
        // Reasoning, count and nano-AIU omitted on resume (probe E): the
        // token growth is the interval and the omitted counters are unknown.
        let omitted = ShutdownTotals {
            input: Some(150),
            output: Some(80),
            cache_read: Some(10),
            cache_write: Some(20),
            reasoning: None,
            requests: None,
            nano_aiu: None,
        };
        let (interval, regressed) = omitted.since(first);
        assert!(!regressed);
        assert_eq!(interval.input, Some(50));
        assert_eq!(interval.output, Some(30));
        assert_eq!(
            (interval.reasoning, interval.requests, interval.nano_aiu),
            (None, None, None)
        );
        let (tokens, _) = interval_snapshot(interval);
        assert_eq!((tokens.output, tokens.thinking), (Some(30), None));
        assert_eq!(tokens.billed_pico_usd, None);
        assert_eq!(
            (tokens.scope, tokens.requests),
            (UsageScope::Aggregate, None)
        );

        // An optional counter going backwards on its own is malformed, not a reset.
        let dipped = ShutdownTotals {
            reasoning: Some(5),
            ..first
        };
        let (interval, regressed) = dipped.since(first);
        assert!(regressed);
        assert_eq!(interval.reasoning, None);
        assert_eq!(interval.input, Some(0));
    }

    #[test]
    fn fractional_premium_requests_and_float_nano_aiu_still_parse() {
        // Writer schema: `totalPremiumRequests` and `totalNanoAiu` are
        // `number`; the SDK says the premium count may be fractional.
        let mut metrics = real_reading();
        metrics["gpt-5.6-terra"]["totalNanoAiu"] = json!(18_653_380_000.0);
        let line = shutdown_line(
            "s1",
            "2026-08-27T04:34:11.280Z",
            json!({"modelMetrics": metrics, "totalNanoAiu": 18_653_380_000.0, "totalPremiumRequests": 1.5}),
        );
        // A writer without a store: the interval is the record.
        let mut ctx = context(Some("1.0.60"));
        let outcome = ingest_line(
            &mut ctx,
            &mut CopilotState::default(),
            "events",
            line.as_bytes(),
        );
        assert_eq!(outcome.status, LineStatus::Parsed);
        let rows = samples(&outcome.ops);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].model.as_deref(), Some("gpt-5.6-terra"));
        assert_eq!(rows[0].tokens.billed_pico_usd, Some(186_533_800_000));
        assert_eq!(rows[0].tokens.requests, Some(5));
        assert_eq!(rows[0].ts, 1_787_805_251_280);
    }

    #[test]
    fn metered_intervals_of_persisting_writers_belong_to_the_store() {
        let line = shutdown_line(
            "s1",
            "2026-08-27T04:34:11.280Z",
            json!({"modelMetrics": real_reading(), "totalNanoAiu": 18_653_380_000_i64, "totalPremiumRequests": 0}),
        );
        for version in [Some("1.0.69"), Some("1.0.80"), None] {
            let mut ctx = context(version);
            let outcome = ingest_line(
                &mut ctx,
                &mut CopilotState::default(),
                "events",
                line.as_bytes(),
            );
            assert_eq!(outcome.status, LineStatus::Parsed);
            assert!(
                samples(&outcome.ops).is_empty(),
                "{version:?} should defer to the store"
            );
        }
        let mut ctx = context(Some("1.0.68"));
        let outcome = ingest_line(
            &mut ctx,
            &mut CopilotState::default(),
            "events",
            line.as_bytes(),
        );
        assert_eq!(samples(&outcome.ops).len(), 1);
    }

    #[test]
    fn unmetered_intervals_are_always_accounted_from_the_shutdown() {
        // The BYOK fixtures: zero nano-AIU, one request, then a resume.
        let mut ctx = context(Some("1.0.80"));
        let mut state = CopilotState::default();
        let first = shutdown_line(
            "s1",
            "2026-08-25T05:28:07.351Z",
            json!({"modelMetrics": {"mock-model": {"requests": {"count": 1, "cost": 0}, "usage": {"inputTokens": 128, "outputTokens": 32, "cacheReadTokens": 0, "cacheWriteTokens": 0, "reasoningTokens": 0}, "totalNanoAiu": 0}}, "totalNanoAiu": 0, "totalPremiumRequests": 0}),
        );
        let outcome = ingest_line(&mut ctx, &mut state, "events", first.as_bytes());
        let rows = samples(&outcome.ops);
        assert_eq!(rows.len(), 1);
        assert_eq!(
            (rows[0].tokens.input, rows[0].tokens.output),
            (Some(128), Some(32))
        );
        assert_eq!(rows[0].tokens.scope, UsageScope::Request);
        assert!(drift_codes(&outcome.ops).is_empty());

        let resumed = shutdown_line(
            "s2",
            "2026-08-25T05:28:08.474Z",
            json!({"modelMetrics": {"mock-model": {"requests": {"count": 2, "cost": 0}, "usage": {"inputTokens": 256, "outputTokens": 64, "cacheReadTokens": 0, "cacheWriteTokens": 0, "reasoningTokens": 0}, "totalNanoAiu": 0}}, "totalNanoAiu": 0, "totalPremiumRequests": 0}),
        );
        let outcome = ingest_line(&mut ctx, &mut state, "events", resumed.as_bytes());
        let rows = samples(&outcome.ops);
        assert_eq!(rows.len(), 1);
        assert_eq!(
            (rows[0].tokens.input, rows[0].tokens.output),
            (Some(128), Some(32))
        );

        // A resume that made no request adds nothing.
        let idle = shutdown_line(
            "s3",
            "2026-08-25T05:28:09.000Z",
            json!({"modelMetrics": {"mock-model": {"requests": {"count": 2, "cost": 0}, "usage": {"inputTokens": 256, "outputTokens": 64, "cacheReadTokens": 0, "cacheWriteTokens": 0, "reasoningTokens": 0}, "totalNanoAiu": 0}}}),
        );
        let outcome = ingest_line(&mut ctx, &mut state, "events", idle.as_bytes());
        assert!(samples(&outcome.ops).is_empty());
    }

    #[test]
    fn a_real_reset_counts_the_whole_reading() {
        let mut ctx = context(Some("1.0.60"));
        let mut state = CopilotState::default();
        let first = shutdown_line(
            "s1",
            "2026-08-27T04:34:11.280Z",
            json!({"modelMetrics": real_reading()}),
        );
        ingest_line(&mut ctx, &mut state, "events", first.as_bytes());
        let mut restarted = real_reading();
        restarted["gpt-5.6-terra"]["usage"] = json!({"inputTokens": 5_000, "outputTokens": 100, "cacheReadTokens": 4_000, "cacheWriteTokens": 900, "reasoningTokens": 10});
        restarted["gpt-5.6-terra"]["requests"]["count"] = json!(1);
        restarted["gpt-5.6-terra"]["totalNanoAiu"] = json!(2_000_000);
        let second = shutdown_line(
            "s2",
            "2026-08-28T00:00:00.000Z",
            json!({"modelMetrics": restarted}),
        );
        let outcome = ingest_line(&mut ctx, &mut state, "events", second.as_bytes());
        let rows = samples(&outcome.ops);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].tokens.input, Some(100));
        assert_eq!(rows[0].tokens.cache_read, Some(4_000));
        assert_eq!(rows[0].tokens.billed_pico_usd, Some(20_000_000));
        assert_eq!(rows[0].tokens.scope, UsageScope::Request);
        assert!(drift_codes(&outcome.ops).is_empty());
    }

    #[test]
    fn token_growth_without_requests_is_an_uncounted_aggregate() {
        // Probe F: tokens grew while `requests.count` stayed put.
        let mut ctx = context(Some("1.0.60"));
        let mut state = CopilotState::default();
        let first = shutdown_line(
            "s1",
            "2026-08-27T04:34:11.280Z",
            json!({"modelMetrics": real_reading()}),
        );
        ingest_line(&mut ctx, &mut state, "events", first.as_bytes());
        let mut grown = real_reading();
        grown["gpt-5.6-terra"]["usage"]["inputTokens"] = json!(173_193);
        grown["gpt-5.6-terra"]["usage"]["cacheReadTokens"] = json!(111_468);
        grown["gpt-5.6-terra"]["usage"]["cacheWriteTokens"] = json!(61_707);
        grown["gpt-5.6-terra"]["usage"]["outputTokens"] = json!(2_425);
        grown["gpt-5.6-terra"]["usage"]["reasoningTokens"] = json!(484);
        grown["gpt-5.6-terra"]["totalNanoAiu"] = json!(20_569_710_000_i64);
        let second = shutdown_line(
            "s2",
            "2026-08-28T00:00:00.000Z",
            json!({"modelMetrics": grown}),
        );
        let outcome = ingest_line(&mut ctx, &mut state, "events", second.as_bytes());
        let rows = samples(&outcome.ops);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].tokens.input, Some(3));
        assert_eq!(
            (rows[0].tokens.scope, rows[0].tokens.requests),
            (UsageScope::Aggregate, None)
        );
        assert_eq!(rows[0].tokens.billed_pico_usd, Some(19_163_300_000));
        assert_eq!(drift_codes(&outcome.ops), ["usage_uncounted_interval"]);
    }

    #[test]
    fn mixed_model_intervals_are_one_sample_per_model() {
        let mut metrics = real_reading();
        metrics["claude-opus-4.8-fast"] = json!({
            "requests": {"count": 2, "cost": 0},
            "usage": {"inputTokens": 1_000, "outputTokens": 100, "cacheReadTokens": 600, "cacheWriteTokens": 300, "reasoningTokens": 40},
            "totalNanoAiu": 0,
        });
        let line = shutdown_line(
            "s1",
            "2026-08-27T04:34:11.280Z",
            json!({"modelMetrics": metrics}),
        );
        let mut ctx = context(Some("1.0.60"));
        let outcome = ingest_line(
            &mut ctx,
            &mut CopilotState::default(),
            "events",
            line.as_bytes(),
        );
        let rows = samples(&outcome.ops);
        assert_eq!(rows.len(), 2);
        let fast = rows
            .iter()
            .find(|row| row.model.as_deref() == Some("claude-opus-4.8"))
            .expect("fast model priced at its base id");
        assert_eq!(fast.tokens.service_tier, Some(ServiceTier::Fast));
        assert_eq!(fast.tokens.input, Some(100));
        assert_eq!(fast.tokens.requests, Some(2));
        assert!(
            rows.iter()
                .any(|row| row.model.as_deref() == Some("gpt-5.6-terra"))
        );
    }
}
