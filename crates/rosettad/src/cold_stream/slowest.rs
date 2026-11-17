//! The slowest writer transactions of a streamed build, measurement only:
//! the five slowest apply steps and the five slowest flip transactions, each
//! with what it wrote, kept in RAM and reported once in the
//! `streamed-writer` stage profile (`slowestApplySteps`, `slowestFlipTxs`).
//! A transaction is offered after it commits; an entry is built only when
//! it is kept. Counts, sizes and timings only: no path, title or content.

use rosetta_pipeline::{LaneId, StepReport, TxKind};
use rosetta_store::ApplyStats;
use serde_json::{Value, json};

/// How many transactions of each kind the profile keeps.
pub(super) const KEEP: usize = 5;

fn millis(nanos: u64) -> f64 {
    nanos as f64 / 1_000_000.0
}

/// The `keep` entries offered with the largest keys. Ties keep the entry
/// offered first.
#[derive(Debug)]
pub(super) struct Slowest<T> {
    keep: usize,
    offered: u64,
    /// `(key, offer sequence, entry)`, in no order.
    entries: Vec<(u64, u64, T)>,
}

impl<T> Default for Slowest<T> {
    fn default() -> Self {
        Self::new(KEEP)
    }
}

impl<T> Slowest<T> {
    pub(super) fn new(keep: usize) -> Self {
        Self {
            keep,
            offered: 0,
            entries: Vec::with_capacity(keep),
        }
    }

    /// Offer an entry of `key`; `entry` runs only if it is kept.
    pub(super) fn offer(&mut self, key: u64, entry: impl FnOnce() -> T) {
        let sequence = self.offered;
        self.offered += 1;
        if self.entries.len() < self.keep {
            self.entries.push((key, sequence, entry()));
            return;
        }
        let Some((position, smallest)) = self
            .entries
            .iter()
            .enumerate()
            .min_by_key(|(_, (key, sequence, _))| (*key, std::cmp::Reverse(*sequence)))
            .map(|(position, (key, _, _))| (position, *key))
        else {
            return;
        };
        if key > smallest {
            self.entries[position] = (key, sequence, entry());
        }
    }

    /// The kept entries, largest key first (ties in offer order).
    pub(super) fn largest_first(&self) -> Vec<&T> {
        let mut entries: Vec<&(u64, u64, T)> = self.entries.iter().collect();
        entries.sort_by(|left, right| right.0.cmp(&left.0).then(left.1.cmp(&right.1)));
        entries.into_iter().map(|(_, _, entry)| entry).collect()
    }
}

/// What one transaction wrote, by kind of op, from its apply statistics.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct OpCounts {
    messages: u64,
    tool_updates: u64,
    usage: u64,
    drift: u64,
    other: u64,
    assets: u64,
    messages_inserted: u64,
    messages_updated: u64,
}

impl OpCounts {
    pub(super) fn of(stats: &ApplyStats) -> Self {
        Self {
            messages: stats.profile.message_attempts,
            tool_updates: stats.profile.tool_result_attempts,
            usage: stats.profile.usage_attempts,
            drift: stats.profile.drift_attempts,
            other: stats.profile.other_attempts,
            assets: stats.projection_assets,
            messages_inserted: stats.messages_inserted,
            messages_updated: stats.messages_updated,
        }
    }

    fn json(&self) -> Value {
        json!({
            "messages": self.messages,
            "toolUpdates": self.tool_updates,
            "usage": self.usage,
            "drift": self.drift,
            "other": self.other,
            "assets": self.assets,
            "messagesInserted": self.messages_inserted,
            "messagesUpdated": self.messages_updated,
        })
    }
}

/// One apply step: its start on the milestone clock, its (runtime, lane)
/// key and what the pipeline's step report and apply statistics say.
#[derive(Debug)]
pub(super) struct StepRecord {
    pub(super) elapsed_s: f64,
    pub(super) runtime: String,
    pub(super) lane: LaneId,
    pub(super) report: StepReport,
    pub(super) ops: OpCounts,
    pub(super) lock_held_ns: u64,
    pub(super) lock_wait_ns: u64,
}

fn kind(kind: TxKind) -> &'static str {
    match kind {
        TxKind::Stage => "Stage",
        TxKind::ResolvedRows => "ResolvedRows",
        TxKind::Terminal => "Terminal",
    }
}

impl StepRecord {
    pub(super) fn json(&self) -> Value {
        let report = &self.report;
        json!({
            "elapsedS": self.elapsed_s,
            "runtime": self.runtime,
            "lane": self.lane.to_string(),
            "kind": kind(report.kind),
            "sources": report.sources,
            "approxBytes": report.approx_bytes,
            "assetBytes": report.asset_bytes,
            "immediateBytes": report.immediate_bytes(),
            "resolvedBytes": report.resolved_bytes,
            "ops": self.ops.json(),
            "wallMs": millis(report.wall_nanos),
            "lockHeldMs": millis(self.lock_held_ns),
            "lockWaitMs": millis(self.lock_wait_ns),
        })
    }
}

/// One flip transaction (a batch of trees): its start on the milestone
/// clock, its runtime and what it admitted.
#[derive(Debug)]
pub(super) struct FlipRecord {
    pub(super) elapsed_s: f64,
    pub(super) runtime: String,
    pub(super) trees: usize,
    pub(super) sessions: usize,
    pub(super) roots: usize,
    pub(super) tail_bytes: u64,
    pub(super) renumber_rows: u64,
    pub(super) ops: OpCounts,
    pub(super) wall_ns: u64,
    pub(super) lock_held_ns: u64,
    pub(super) lock_wait_ns: u64,
}

impl FlipRecord {
    pub(super) fn json(&self) -> Value {
        json!({
            "elapsedS": self.elapsed_s,
            "runtime": self.runtime,
            "kind": "Flip",
            "trees": self.trees,
            "sessions": self.sessions,
            "roots": self.roots,
            "tailBytes": self.tail_bytes,
            "renumberRows": self.renumber_rows,
            "ops": self.ops.json(),
            "wallMs": millis(self.wall_ns),
            "lockHeldMs": millis(self.lock_held_ns),
            "lockWaitMs": millis(self.lock_wait_ns),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_keeps_the_largest_keys_and_builds_only_what_it_keeps() {
        let mut slowest = Slowest::new(3);
        let mut built = Vec::new();
        for (key, label) in [(5, "a"), (1, "b"), (9, "c"), (7, "e"), (5, "d"), (2, "f")] {
            slowest.offer(key, || {
                built.push(label);
                label
            });
        }
        assert_eq!(slowest.largest_first(), [&"c", &"e", &"a"]);
        // "e" displaced "b"; "d" ties "a" (offered first) and is never
        // built, nor is "f", smaller than everything kept.
        assert_eq!(built, ["a", "b", "c", "e"]);
    }

    #[test]
    fn ties_keep_offer_order_and_an_empty_recorder_reports_nothing() {
        let mut slowest = Slowest::new(2);
        assert!(slowest.largest_first().is_empty());
        for label in ["a", "b", "c"] {
            slowest.offer(4, || label);
        }
        assert_eq!(slowest.largest_first(), [&"a", &"b"]);
        let mut none: Slowest<u8> = Slowest::new(0);
        none.offer(1, || {
            unreachable!("a recorder that keeps nothing builds nothing")
        });
        assert!(none.largest_first().is_empty());
        assert_eq!(Slowest::<u8>::default().keep, KEEP);
    }

    #[test]
    fn a_step_record_splits_its_bytes_and_separates_lock_hold_from_wall() {
        let record = StepRecord {
            elapsed_s: 1.5,
            runtime: "r".into(),
            lane: LaneId::Small,
            report: StepReport {
                kind: TxKind::Terminal,
                approx_bytes: 100,
                asset_bytes: 30,
                resolved_bytes: 20,
                sources: 3,
                wall_nanos: 4_000_000,
            },
            ops: OpCounts::default(),
            lock_held_ns: 1_000_000,
            lock_wait_ns: 0,
        };
        let json = record.json();
        assert_eq!(json["kind"], "Terminal");
        assert_eq!(json["lane"], "S");
        assert_eq!(json["immediateBytes"], 50);
        assert_eq!(json["wallMs"], 4.0);
        assert_eq!(json["lockHeldMs"], 1.0);
        assert_eq!(json["ops"]["messages"], 0);
    }
}
