//! Pure fold helpers: hour bucketing, cumulative→delta normalization, and the
//! None-aware token arithmetic that keeps "never reported" distinct from zero.
//! No IO, no clock, no randomness — these bench and test in isolation.

use crate::rows::{Ms, TokenSnapshot};

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
/// reported usage before.
pub fn snapshot_delta(new: &TokenSnapshot, old: Option<&TokenSnapshot>) -> TokenSnapshot {
    let old = old.copied().unwrap_or_default();
    TokenSnapshot {
        input: component_delta(new.input, old.input),
        output: component_delta(new.output, old.output),
        cache_read: component_delta(new.cache_read, old.cache_read),
        cache_write: component_delta(new.cache_write, old.cache_write),
        thinking: component_delta(new.thinking, old.thinking),
    }
}

fn component_add(bucket: Option<i64>, delta: Option<i64>) -> Option<i64> {
    match delta {
        None => bucket,
        Some(d) => Some(bucket.unwrap_or(0) + d),
    }
}

/// Apply a delta to a bucket, component-wise. A `None` delta component leaves
/// the bucket component untouched (including leaving it `None`).
pub fn bucket_apply(bucket: &TokenSnapshot, delta: &TokenSnapshot) -> TokenSnapshot {
    TokenSnapshot {
        input: component_add(bucket.input, delta.input),
        output: component_add(bucket.output, delta.output),
        cache_read: component_add(bucket.cache_read, delta.cache_read),
        cache_write: component_add(bucket.cache_write, delta.cache_write),
        thinking: component_add(bucket.thinking, delta.thinking),
    }
}

/// Turns cumulative token counters (codex `token_count`) into per-report
/// deltas. A counter that goes backwards means the source reset (new thread
/// epoch, compaction) — the new reading is treated as a fresh delta rather
/// than producing a negative one.
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
        fn step(last: &mut Option<i64>, new: Option<i64>) -> Option<i64> {
            match new {
                None => None,
                Some(n) => {
                    let delta = match *last {
                        Some(prev) if n >= prev => n - prev,
                        // Reset detected (or first reading): fresh delta.
                        _ => n,
                    };
                    *last = Some(n);
                    Some(delta)
                }
            }
        }
        TokenSnapshot {
            input: step(&mut self.last.input, reading.input),
            output: step(&mut self.last.output, reading.output),
            cache_read: step(&mut self.last.cache_read, reading.cache_read),
            cache_write: step(&mut self.last.cache_write, reading.cache_write),
            thinking: step(&mut self.last.thinking, reading.thinking),
        }
    }
}

/// Split an inclusive input counter into fresh + cached parts by subtraction
/// (spaghetti's codex rule: `input_tokens` includes `cached_input_tokens`).
/// Returns the non-cached input; `None` inputs stay `None`.
pub fn split_inclusive_input(input: Option<i64>, cached: Option<i64>) -> Option<i64> {
    match (input, cached) {
        (Some(total), Some(cached)) => Some((total - cached).max(0)),
        (total, None) => total,
        (None, Some(_)) => None,
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
    }

    #[test]
    fn inclusive_input_split() {
        assert_eq!(split_inclusive_input(Some(1000), Some(800)), Some(200));
        assert_eq!(split_inclusive_input(Some(1000), None), Some(1000));
        assert_eq!(split_inclusive_input(None, Some(800)), None);
        // Malformed (cached > total) clamps instead of going negative.
        assert_eq!(split_inclusive_input(Some(100), Some(150)), Some(0));
    }

    #[test]
    fn line_deltas() {
        assert_eq!(line_delta(Some("a\nb"), "a\nb\nc"), (Some(3), Some(2)));
        assert_eq!(line_delta(None, "x\ny"), (Some(2), None));
        assert_eq!(line_delta(Some(""), ""), (Some(0), Some(0)));
    }
}
