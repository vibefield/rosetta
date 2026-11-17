//! Per-tree flips on the writer (R1 D8–D10).
//!
//! A runtime admitted per root tree makes each tree readable as soon as the
//! preparer has closed it and the writer has committed all its sources
//! (`StreamedRuntime::admit_trees`): one writer transaction per flip batch,
//! tails folded, ordinals renumbered and the readiness flip last. The
//! cadence (D9) flips the first ready batch at once and then at most one
//! batch per [`FLIP_INTERVAL`] per runtime, each batch capped. After each
//! batch the hub hears of it in D10's order — row effects, then each
//! changed session's resync barrier, then metadata — and the board and the
//! milestones count its sessions. A flipped tree is never retracted: a
//! later refusal of the build leaves it readable.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use rosetta_pipeline::{FlipCaps, ProbeCounters, Samples, StreamedRuntime, TreeAdmission};
use serde_json::{Value, json};

use super::slowest::{FlipRecord, OpCounts, Slowest};
use super::{StreamedCold, millis};
use crate::bench;
use crate::engine::{absorb_around, now_ms};
use crate::ingest_metrics::ColdMilestones;

const MIB: u64 = 1024 * 1024;

/// The least time between two flip batches of one runtime.
pub(super) const FLIP_INTERVAL: Duration = Duration::from_millis(100);
pub(super) const FLIP_MAX_TREES: usize = 64;
pub(super) const FLIP_MAX_SESSIONS: usize = 512;
/// Buffered tails one batch folds, by approx bytes.
pub(super) const FLIP_MAX_TAIL_BYTES: u64 = 16 * MIB;
/// The most approx bytes one apply transaction writes during capture (or one
/// op or resolved row that alone exceeds it).
pub(crate) const CHUNK_BYTES: u64 = 16 * MIB;

pub(super) const CAPS: FlipCaps = FlipCaps {
    trees: FLIP_MAX_TREES,
    sessions: FLIP_MAX_SESSIONS,
    tail_bytes: FLIP_MAX_TAIL_BYTES,
};

/// What every flip batch of the build measured.
#[derive(Debug, Default)]
pub(super) struct FlipStats {
    pub(super) batches: u64,
    pub(super) trees: u64,
    pub(super) sessions: u64,
    pub(super) wall: Samples,
    pub(super) max_tail_bytes: u64,
    pub(super) renumber_rows: u64,
    pub(super) lock_wait_ns: u64,
    /// The slowest flip transactions (under `--json --profile`).
    pub(super) slowest: Slowest<FlipRecord>,
}

impl FlipStats {
    pub(super) fn profile(&self) -> Value {
        let mut wall = self.wall.clone();
        let slowest: Vec<Value> = self
            .slowest
            .largest_first()
            .into_iter()
            .map(FlipRecord::json)
            .collect();
        json!({
            "slowestFlipTxs": slowest,
            "flipBatches": self.batches,
            "flipTrees": self.trees,
            "flipSessions": self.sessions,
            "flipWallMs": millis(wall.total_nanos()),
            "longestFlipTxMs": millis(wall.max_nanos()),
            "treeAdmissionP99Ms": millis(wall.percentile_nanos(99)),
            "maxFlipTailBytes": self.max_tail_bytes,
            "flipRenumberRows": self.renumber_rows,
            "flipLockWaitMs": millis(self.lock_wait_ns),
        })
    }
}

/// The flip cadence and its record, per runtime.
#[derive(Default)]
pub(super) struct Flips {
    last: HashMap<String, Instant>,
    pub(super) stats: FlipStats,
}

impl Flips {
    /// How long until `runtime`'s next batch may flip: zero for its first.
    pub(super) fn wait(&self, runtime: &str, now: Instant) -> Duration {
        self.last.get(runtime).map_or(Duration::ZERO, |last| {
            FLIP_INTERVAL.saturating_sub(now.saturating_duration_since(*last))
        })
    }

    /// The first open runtime, in key order, with a batch due now; else the
    /// least time until one is.
    pub(super) fn due<'a>(
        &self,
        open: &'a HashMap<String, StreamedRuntime>,
        now: Instant,
    ) -> (Option<&'a str>, Option<Duration>) {
        let mut runtimes: Vec<&String> = open
            .iter()
            .filter(|(_, streamed)| streamed.ready_trees() > 0)
            .map(|(runtime, _)| runtime)
            .collect();
        runtimes.sort();
        let mut soonest: Option<Duration> = None;
        for runtime in runtimes {
            let wait = self.wait(runtime, now);
            if wait.is_zero() {
                return (Some(runtime.as_str()), None);
            }
            soonest = Some(soonest.map_or(wait, |soonest| soonest.min(wait)));
        }
        (None, soonest)
    }

    /// Flip one batch of `runtime`'s ready trees and publish it (D10).
    /// Returns the batch, empty when no tree was ready.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn flip(
        &mut self,
        runtime_id: &str,
        streamed: &mut StreamedRuntime,
        cx: &mut StreamedCold<'_>,
        ownership: &BTreeMap<String, Option<String>>,
        milestones: &mut ColdMilestones,
        probe: Option<&ProbeCounters>,
        stage_profiles: bool,
    ) -> Result<TreeAdmission, String> {
        let admitted = streamed
            .admit_trees(cx.store, cx.instrument, CAPS)
            .map_err(|error| format!("flipping trees of {runtime_id}: {error}"))?;
        if admitted.trees.is_empty() {
            return Ok(admitted);
        }
        self.last.insert(runtime_id.to_owned(), Instant::now());
        if let Some(probe) = probe {
            probe.flips_committed.fetch_add(1, Ordering::SeqCst);
        }
        // The flip's own fold may displace usage on a tree flipped earlier.
        let mut invalidate = admitted.changed.clone();
        for session in streamed.flipped_among(&admitted.stats.usage_displaced_sessions) {
            if !invalidate.contains(&session) {
                invalidate.push(session);
            }
        }
        absorb_around(cx.store, cx.hub, &invalidate, now_ms())?;
        // Measurement only: the batch is public on the hub from here.
        let published = cx.started.elapsed();
        for session_id in &admitted.changed {
            cx.board.session_done(
                ownership.get(session_id).and_then(Option::as_deref),
                session_id,
            );
        }
        milestones.sessions_readable(runtime_id, &admitted.changed);
        self.record(&admitted);
        if stage_profiles {
            let since = |at: Option<Instant>| {
                at.map(|at| at.saturating_duration_since(cx.started).as_secs_f64())
            };
            let roots = milestones.roots_among(&admitted.changed);
            self.stats.slowest.offer(admitted.wall_ns, || FlipRecord {
                elapsed_s: since(admitted.started_at).unwrap_or_default(),
                runtime: runtime_id.to_owned(),
                trees: admitted.trees.len(),
                sessions: admitted.changed.len(),
                roots,
                tail_bytes: admitted.tail_bytes,
                renumber_rows: admitted.renumber_rows,
                ops: OpCounts::of(&admitted.stats),
                wall_ns: admitted.wall_ns,
                lock_held_ns: admitted.lock_held_ns,
                lock_wait_ns: admitted.lock_wait_ns,
            });
            bench::emit(&json!({
                "event": "stage-profile",
                "command": "serve",
                "stage": "tree-admission",
                "agent": null,
                "runtime": runtime_id,
                "trees": admitted.trees.len(),
                "sessions": admitted.changed.len(),
                "roots": roots,
                "tailBytes": admitted.tail_bytes,
                "renumberRows": admitted.renumber_rows,
                "wallMs": millis(admitted.wall_ns),
                "lockWaitMs": millis(admitted.lock_wait_ns),
                "lockHeldMs": millis(admitted.lock_held_ns),
                "elapsedS": since(admitted.committed_at),
                "publishedS": published.as_secs_f64(),
            }));
        }
        Ok(admitted)
    }

    /// Flip every ready tree of `runtime` now, whatever the cadence: the
    /// runtime's close admits nothing before them.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn drain(
        &mut self,
        runtime_id: &str,
        streamed: &mut StreamedRuntime,
        cx: &mut StreamedCold<'_>,
        ownership: &BTreeMap<String, Option<String>>,
        milestones: &mut ColdMilestones,
        probe: Option<&ProbeCounters>,
        stage_profiles: bool,
    ) -> Result<(), String> {
        while streamed.ready_trees() > 0 {
            self.flip(
                runtime_id,
                streamed,
                cx,
                ownership,
                milestones,
                probe,
                stage_profiles,
            )?;
        }
        Ok(())
    }

    fn record(&mut self, admitted: &TreeAdmission) {
        let stats = &mut self.stats;
        stats.batches += 1;
        stats.trees += admitted.trees.len() as u64;
        stats.sessions += admitted.changed.len() as u64;
        stats.wall.record(Duration::from_nanos(admitted.wall_ns));
        stats.max_tail_bytes = stats.max_tail_bytes.max(admitted.tail_bytes);
        stats.renumber_rows += admitted.renumber_rows;
        stats.lock_wait_ns += admitted.lock_wait_ns;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_batch_is_due_at_once_and_the_next_after_the_interval() {
        let mut flips = Flips::default();
        let now = Instant::now();
        assert_eq!(flips.wait("r", now), Duration::ZERO);
        flips.last.insert("r".into(), now);
        assert_eq!(flips.wait("r", now), FLIP_INTERVAL);
        assert_eq!(
            flips.wait("r", now + Duration::from_millis(40)),
            Duration::from_millis(60)
        );
        assert_eq!(flips.wait("r", now + FLIP_INTERVAL), Duration::ZERO);
        // Each runtime keeps its own cadence.
        assert_eq!(flips.wait("other", now), Duration::ZERO);
    }
}
