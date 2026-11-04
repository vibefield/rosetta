//! What one source's capture steps measured, where they ran: on a pool
//! worker, or on the thread of the serial driver. Measurement only — the
//! `bigSourceSplits` of the `parallel-stream-prepare` stage profile — a few
//! integers and two instants carried with each step's output, never
//! scheduling anything.

use std::fmt;
use std::time::{Duration, Instant};

/// One source's capture steps: the first one's start, the last one's end,
/// how many, the longest, and the largest framed record any of them read.
/// Its `Debug` is opaque, so equivalence tests that compare the closed
/// sources a walk emits never see a timing.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct CaptureClock {
    first_start: Option<Instant>,
    last_end: Option<Instant>,
    steps: u32,
    longest_step_nanos: u64,
    max_record_bytes: u64,
}

impl CaptureClock {
    /// One step that began at `started`, took `nanos` and read records of
    /// at most `max_record_bytes` framed payload.
    pub(crate) fn step(started: Instant, nanos: u64, max_record_bytes: u64) -> Self {
        Self {
            first_start: Some(started),
            last_end: Some(started + Duration::from_nanos(nanos)),
            steps: 1,
            longest_step_nanos: nanos,
            max_record_bytes,
        }
    }

    /// Fold in `other`'s steps.
    pub(crate) fn merge(&mut self, other: &Self) {
        self.first_start = match (self.first_start, other.first_start) {
            (Some(left), Some(right)) => Some(left.min(right)),
            (left, right) => left.or(right),
        };
        self.last_end = self.last_end.max(other.last_end);
        self.steps = self.steps.saturating_add(other.steps);
        self.longest_step_nanos = self.longest_step_nanos.max(other.longest_step_nanos);
        self.max_record_bytes = self.max_record_bytes.max(other.max_record_bytes);
    }

    /// From the first step's start to the last step's end; 0 with none.
    pub fn wall_nanos(&self) -> u64 {
        self.first_start
            .zip(self.last_end)
            .map_or(0, |(start, end)| {
                u64::try_from(end.saturating_duration_since(start).as_nanos()).unwrap_or(u64::MAX)
            })
    }

    pub fn steps(&self) -> u32 {
        self.steps
    }

    pub fn longest_step_nanos(&self) -> u64 {
        self.longest_step_nanos
    }

    /// The largest framed record (a JSONL line's payload) any step read.
    pub fn max_record_bytes(&self) -> u64 {
        self.max_record_bytes
    }
}

impl fmt::Debug for CaptureClock {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CaptureClock")
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_merge_into_one_wall_from_first_start_to_last_end() {
        let t0 = Instant::now();
        let mut clock = CaptureClock::default();
        assert_eq!(clock.wall_nanos(), 0);
        // Out of order, as workers return them: the wall still runs from
        // the earliest start to the latest end.
        clock.merge(&CaptureClock::step(
            t0 + Duration::from_millis(5),
            2_000_000,
            10,
        ));
        clock.merge(&CaptureClock::step(t0, 1_000_000, 300));
        clock.merge(&CaptureClock::step(
            t0 + Duration::from_millis(9),
            4_000_000,
            20,
        ));
        assert_eq!(clock.steps(), 3);
        assert_eq!(clock.wall_nanos(), 13_000_000);
        assert_eq!(clock.longest_step_nanos(), 4_000_000);
        assert_eq!(clock.max_record_bytes(), 300);
        let mut empty = CaptureClock::default();
        empty.merge(&clock);
        assert_eq!(empty, clock);
        assert_eq!(
            format!("{clock:?}"),
            format!("{:?}", CaptureClock::default())
        );
    }
}
