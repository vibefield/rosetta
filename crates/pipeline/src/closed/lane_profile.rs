//! What one lane-merge walk measured, per lane: measurement only, reported
//! in the producer's `parallel-stream-prepare` stage profile.

use std::time::Duration;

use super::walk_order::LaneId;

/// Exact samples of one duration, in nanoseconds.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Samples {
    values: Vec<u64>,
    sorted: bool,
}

impl Samples {
    pub fn record(&mut self, duration: Duration) {
        self.values
            .push(u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX));
        self.sorted = false;
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    fn sort(&mut self) {
        if !self.sorted {
            self.values.sort_unstable();
            self.sorted = true;
        }
    }

    /// The `percent` (0–100) percentile by nearest rank; 0 with no samples.
    pub fn percentile_nanos(&mut self, percent: u64) -> u64 {
        self.sort();
        if self.values.is_empty() {
            return 0;
        }
        let rank = (percent.min(100) as usize * self.values.len()).div_ceil(100);
        self.values[rank.saturating_sub(1).min(self.values.len() - 1)]
    }

    pub fn max_nanos(&self) -> u64 {
        self.values.iter().copied().max().unwrap_or(0)
    }

    pub fn total_nanos(&self) -> u64 {
        self.values.iter().copied().fold(0, u64::saturating_add)
    }
}

/// One lane of one runtime's walk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaneProfile {
    pub lane: LaneId,
    /// Capture units: transcripts and physical groups.
    pub units: u64,
    pub sources: u64,
    /// The lane's sources' bytes, as the plan measured them.
    pub bytes: u64,
    /// Sequencer waits while this lane's head was a candidate and had no
    /// output yet.
    pub head_wait_nanos: u64,
    /// Each held emission, from its hold to its send.
    pub held: Samples,
    /// From the walk's start to the lane's last emission.
    pub end_nanos: u64,
    /// Sequencer work done inline for this lane: groups, inline passes,
    /// sibling heals, closing units and preparing emissions.
    pub inline_nanos: u64,
    /// Each event of the lane's head, from "queued, the lane holds no
    /// emission and span P has run" to "the sequencer takes it".
    pub service_delay: Samples,
    /// The most of this lane's payload the sequencer held unemitted at once:
    /// its head's bytes so far, closed sources not emitted yet and a held
    /// emission.
    pub peak_unemitted_head_bytes: u64,
}

impl LaneProfile {
    pub(crate) fn new(lane: LaneId) -> Self {
        Self {
            lane,
            units: 0,
            sources: 0,
            bytes: 0,
            head_wait_nanos: 0,
            held: Samples::default(),
            end_nanos: 0,
            inline_nanos: 0,
            service_delay: Samples::default(),
            peak_unemitted_head_bytes: 0,
        }
    }
}

/// One runtime's lane-merge walk.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WalkProfile {
    pub lanes: Vec<LaneProfile>,
    /// Consecutive events the sequencer took from different lanes.
    pub lane_switches: u64,
    /// Sequencer waits during which every live lane holding no emission was
    /// a big lane: the big-lane chain the sequencer itself waited on.
    pub big_lane_chain_nanos: u64,
}
