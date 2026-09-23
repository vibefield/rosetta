//! Pure fold helpers: hour bucketing, cumulative→delta normalization, and the
//! None-aware token arithmetic that keeps "never reported" distinct from zero.
//! No IO, no clock, no randomness — these bench and test in isolation.

use crate::rows::{Ms, TokenSnapshot, UsageScope};

pub const HOUR_MS: Ms = 3_600_000;

/// Floor a timestamp to its UTC hour bucket start.
pub fn hour_bucket(ts_ms: Ms) -> Ms {
    ts_ms.div_euclid(HOUR_MS) * HOUR_MS
}

fn component_delta(new: Option<i64>, old: Option<i64>) -> Option<i64> {
    match (new, old) {
        // Never reported now: contributes nothing, stays unknown. This also
        // covers a component that vanishes on re-report — we never subtract
        // what we can no longer see.
        (None, _) => None,
        (Some(n), None) => Some(n),
        (Some(n), Some(o)) => Some(n - o),
    }
}

/// Replacement diff: what a re-reported snapshot adds to (or removes from)
/// the bucket, component-wise. `old = None` means this response never
/// reported usage before. Tier and route are facts of the newer report.
pub fn snapshot_delta(new: &TokenSnapshot, old: Option<&TokenSnapshot>) -> TokenSnapshot {
    let old = old.cloned().unwrap_or_default();
    TokenSnapshot {
        input: component_delta(new.input, old.input),
        output: component_delta(new.output, old.output),
        cache_read: component_delta(new.cache_read, old.cache_read),
        cache_write: component_delta(new.cache_write, old.cache_write),
        thinking: component_delta(new.thinking, old.thinking),
        cache_write_1h: component_delta(new.cache_write_1h, old.cache_write_1h),
        web_searches: component_delta(new.web_searches, old.web_searches),
        service_tier: new.service_tier,
        route: new.route.clone(),
        billed_pico_usd: component_delta(new.billed_pico_usd, old.billed_pico_usd),
        scope: new.scope,
    }
}

fn component_add(bucket: Option<i64>, delta: Option<i64>) -> Option<i64> {
    match delta {
        None => bucket,
        Some(d) => Some(bucket.unwrap_or(0) + d),
    }
}

/// Apply a delta to a bucket, component-wise. A `None` delta component leaves
/// the bucket component untouched (including leaving it `None`). A reported
/// tier or route replaces the bucket's; an unreported one keeps it. Adding one
/// record's tokens to another's makes the result an aggregate.
pub fn bucket_apply(bucket: &TokenSnapshot, delta: &TokenSnapshot) -> TokenSnapshot {
    let scope = match (bucket.total_tokens(), delta.total_tokens()) {
        (Some(_), Some(_)) => UsageScope::Aggregate,
        (Some(_), None) => bucket.scope,
        _ => delta.scope,
    };
    TokenSnapshot {
        input: component_add(bucket.input, delta.input),
        output: component_add(bucket.output, delta.output),
        cache_read: component_add(bucket.cache_read, delta.cache_read),
        cache_write: component_add(bucket.cache_write, delta.cache_write),
        thinking: component_add(bucket.thinking, delta.thinking),
        cache_write_1h: component_add(bucket.cache_write_1h, delta.cache_write_1h),
        web_searches: component_add(bucket.web_searches, delta.web_searches),
        service_tier: delta.service_tier.or(bucket.service_tier),
        route: delta.route.clone().or_else(|| bucket.route.clone()),
        billed_pico_usd: component_add(bucket.billed_pico_usd, delta.billed_pico_usd),
        scope,
    }
}

/// The exclusive remainder of an inclusive native total after removing the
/// subsets the same record reports inside it (cache inside input, reasoning
/// inside output). Returns the remainder and whether the subsets exceeded the
/// total — a malformed record, clamped to zero rather than going negative.
/// An unreported total stays unreported; unreported subsets remove nothing.
pub fn exclusive_remainder(total: Option<i64>, subsets: &[Option<i64>]) -> (Option<i64>, bool) {
    let Some(total) = total else {
        return (None, false);
    };
    let inside = subsets.iter().flatten().sum::<i64>();
    if inside > total {
        (Some(0), true)
    } else {
        (Some(total - inside), false)
    }
}

/// Split an inclusive native total into (the rest, the subset it contains) —
/// reasoning inside output, 1-hour writes inside all cache writes — keeping
/// their sum equal to the reported total. A subset larger than its total is
/// malformed: it is clamped to the total and flagged, so neither bucket can
/// overcount. An unreported total leaves the subset as reported.
pub fn split_subset(total: Option<i64>, subset: Option<i64>) -> (Option<i64>, Option<i64>, bool) {
    match (total, subset) {
        (Some(total), Some(subset)) if subset > total => (Some(0), Some(total.max(0)), true),
        (Some(total), Some(subset)) => (Some(total - subset), Some(subset), false),
        (total, subset) => (total, subset, false),
    }
}

/// Turns cumulative token counters into per-report deltas. A reading in
/// which any reported counter went backwards starts a new epoch: the whole
/// reading is the delta. Counters never reset one at a time — resetting only
/// the component that dipped re-adds its entire running total (the
/// pre-2026-09-22 overcount, ARCHITECTURE §4.1.3).
#[derive(Debug, Clone, Default)]
pub struct CumulativeUsage {
    last: TokenSnapshot,
}

impl CumulativeUsage {
    pub fn new() -> Self {
        Self::default()
    }

    /// The last cumulative reading — what a resume checkpoint must persist so
    /// a restarted stream doesn't re-count the whole session as one delta.
    pub fn last_reading(&self) -> &TokenSnapshot {
        &self.last
    }

    /// Rebuild the tracker from a persisted reading.
    pub fn resume(last: TokenSnapshot) -> Self {
        Self { last }
    }

    pub fn absorb(&mut self, reading: &TokenSnapshot) -> TokenSnapshot {
        let counters = |snapshot: &TokenSnapshot| {
            [
                snapshot.input,
                snapshot.output,
                snapshot.cache_read,
                snapshot.cache_write,
                snapshot.thinking,
                snapshot.cache_write_1h,
                snapshot.web_searches,
                snapshot.billed_pico_usd,
            ]
        };
        let went_backwards = counters(reading)
            .into_iter()
            .zip(counters(&self.last))
            .any(|(new, last)| matches!((new, last), (Some(new), Some(last)) if new < last));
        let step = |new: Option<i64>, last: Option<i64>| match (new, last) {
            (None, _) => None,
            (Some(new), Some(last)) if !went_backwards => Some(new - last),
            (Some(new), _) => Some(new),
        };
        let delta = TokenSnapshot {
            input: step(reading.input, self.last.input),
            output: step(reading.output, self.last.output),
            cache_read: step(reading.cache_read, self.last.cache_read),
            cache_write: step(reading.cache_write, self.last.cache_write),
            thinking: step(reading.thinking, self.last.thinking),
            cache_write_1h: step(reading.cache_write_1h, self.last.cache_write_1h),
            web_searches: step(reading.web_searches, self.last.web_searches),
            service_tier: reading.service_tier,
            route: reading.route.clone(),
            billed_pico_usd: step(reading.billed_pico_usd, self.last.billed_pico_usd),
            scope: reading.scope,
        };
        // A counter that is not reported this time keeps its last reading, so
        // a later report subtracts from the right base.
        let keep =
            |new: Option<i64>, last: Option<i64>| new.or(if went_backwards { None } else { last });
        self.last = TokenSnapshot {
            input: keep(reading.input, self.last.input),
            output: keep(reading.output, self.last.output),
            cache_read: keep(reading.cache_read, self.last.cache_read),
            cache_write: keep(reading.cache_write, self.last.cache_write),
            thinking: keep(reading.thinking, self.last.thinking),
            cache_write_1h: keep(reading.cache_write_1h, self.last.cache_write_1h),
            web_searches: keep(reading.web_searches, self.last.web_searches),
            service_tier: reading.service_tier,
            route: reading.route.clone(),
            billed_pico_usd: keep(reading.billed_pico_usd, self.last.billed_pico_usd),
            scope: reading.scope,
        };
        delta
    }
}

/// Count added/removed lines from an old→new text replacement (Edit-style
/// tools). Whole-file writes pass `old = None`: added = line count,
/// removed unknown.
pub fn line_delta(old: Option<&str>, new: &str) -> (Option<i64>, Option<i64>) {
    let count = |text: &str| -> i64 {
        if text.is_empty() {
            0
        } else {
            text.lines().count() as i64
        }
    };
    match old {
        Some(old) => (Some(count(new)), Some(count(old))),
        None => (Some(count(new)), None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(
        input: Option<i64>,
        output: Option<i64>,
        cache_read: Option<i64>,
        cache_write: Option<i64>,
        thinking: Option<i64>,
    ) -> TokenSnapshot {
        TokenSnapshot {
            input,
            output,
            cache_read,
            cache_write,
            thinking,
            ..TokenSnapshot::default()
        }
    }

    #[test]
    fn hour_bucket_floors_including_pre_epoch() {
        assert_eq!(hour_bucket(0), 0);
        assert_eq!(hour_bucket(HOUR_MS - 1), 0);
        assert_eq!(hour_bucket(HOUR_MS), HOUR_MS);
        assert_eq!(hour_bucket(-1), -HOUR_MS);
    }

    #[test]
    fn snapshot_delta_preserves_unknownness() {
        // First report: delta is the report itself; unreported stays None.
        let first = snap(Some(100), Some(20), None, None, None);
        let delta = snapshot_delta(&first, None);
        assert_eq!(delta, first);

        // Re-report replaces: only the difference lands.
        let second = snap(Some(150), Some(20), Some(30), None, None);
        let delta = snapshot_delta(&second, Some(&first));
        assert_eq!(delta, snap(Some(50), Some(0), Some(30), None, None));

        // A component that vanishes contributes nothing and is not subtracted.
        let third = snap(Some(150), None, Some(30), None, None);
        let delta = snapshot_delta(&third, Some(&second));
        assert_eq!(delta.output, None);
    }

    #[test]
    fn bucket_apply_never_coerces_none_to_zero() {
        let bucket = TokenSnapshot::default();
        let applied = bucket_apply(&bucket, &snap(Some(10), None, None, None, None));
        assert_eq!(applied.input, Some(10));
        assert_eq!(applied.output, None); // still unknown, not 0
    }

    #[test]
    fn cumulative_usage_yields_deltas_and_survives_resets() {
        let mut tracker = CumulativeUsage::new();
        // Codex-style cumulative readings.
        let d1 = tracker.absorb(&snap(Some(1000), Some(50), Some(800), None, None));
        assert_eq!(d1, snap(Some(1000), Some(50), Some(800), None, None));
        let d2 = tracker.absorb(&snap(Some(1600), Some(90), Some(1400), None, None));
        assert_eq!(d2, snap(Some(600), Some(40), Some(600), None, None));
        // Reset (counter goes backwards): fresh delta, never negative.
        let d3 = tracker.absorb(&snap(Some(200), Some(10), Some(100), None, None));
        assert_eq!(d3, snap(Some(200), Some(10), Some(100), None, None));
        // A skipped component neither produces a delta nor clears state.
        let d4 = tracker.absorb(&snap(Some(300), None, None, None, None));
        assert_eq!(d4, snap(Some(100), None, None, None, None));
        let d5 = tracker.absorb(&snap(Some(300), Some(15), Some(100), None, None));
        assert_eq!(d5, snap(Some(0), Some(5), Some(0), None, None));
    }

    #[test]
    fn one_dipping_counter_starts_an_epoch_for_the_whole_reading() {
        let mut tracker = CumulativeUsage::new();
        tracker.absorb(&snap(Some(1000), Some(50), Some(800_000), None, None));
        // Only the cached counter dipped. The old per-component rule re-added
        // the whole 790,000 cached tokens while subtracting input/output; an
        // epoch counts the reading once, consistently.
        let delta = tracker.absorb(&snap(Some(1200), Some(60), Some(790_000), None, None));
        assert_eq!(delta, snap(Some(1200), Some(60), Some(790_000), None, None));
        let next = tracker.absorb(&snap(Some(1300), Some(70), Some(795_000), None, None));
        assert_eq!(next, snap(Some(100), Some(10), Some(5_000), None, None));
    }

    #[test]
    fn exclusive_remainder_removes_every_reported_subset() {
        assert_eq!(
            exclusive_remainder(Some(1000), &[Some(800)]),
            (Some(200), false)
        );
        // Cache reads AND cache writes inside one inclusive input.
        assert_eq!(
            exclusive_remainder(Some(136_067), &[Some(76_154), Some(59_898)]),
            (Some(15), false)
        );
        assert_eq!(
            exclusive_remainder(Some(1000), &[None, None]),
            (Some(1000), false)
        );
        assert_eq!(exclusive_remainder(None, &[Some(800)]), (None, false));
        // Malformed (subsets > total) clamps instead of going negative.
        assert_eq!(
            exclusive_remainder(Some(100), &[Some(150)]),
            (Some(0), true)
        );
    }

    #[test]
    fn split_subset_never_lets_the_parts_exceed_the_total() {
        assert_eq!(
            split_subset(Some(100), Some(30)),
            (Some(70), Some(30), false)
        );
        assert_eq!(
            split_subset(Some(100), Some(130)),
            (Some(0), Some(100), true)
        );
        assert_eq!(split_subset(Some(100), None), (Some(100), None, false));
        assert_eq!(split_subset(None, Some(5)), (None, Some(5), false));
    }

    #[test]
    fn bucket_apply_carries_new_facts() {
        let bucket = TokenSnapshot {
            cache_write_1h: Some(5),
            service_tier: Some(crate::rows::ServiceTier::Standard),
            ..TokenSnapshot::default()
        };
        let delta = TokenSnapshot {
            cache_write_1h: Some(7),
            web_searches: Some(1),
            billed_pico_usd: Some(100),
            route: Some("openrouter".to_owned()),
            ..TokenSnapshot::default()
        };
        let applied = bucket_apply(&bucket, &delta);
        assert_eq!(applied.cache_write_1h, Some(12));
        assert_eq!(applied.web_searches, Some(1));
        assert_eq!(applied.billed_pico_usd, Some(100));
        assert_eq!(
            applied.service_tier,
            Some(crate::rows::ServiceTier::Standard)
        );
        assert_eq!(applied.route.as_deref(), Some("openrouter"));
    }

    #[test]
    fn line_deltas() {
        assert_eq!(line_delta(Some("a\nb"), "a\nb\nc"), (Some(3), Some(2)));
        assert_eq!(line_delta(None, "x\ny"), (Some(2), None));
        assert_eq!(line_delta(Some(""), ""), (Some(0), Some(0)));
    }
}
