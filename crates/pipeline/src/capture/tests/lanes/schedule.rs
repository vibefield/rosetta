//! The lane-merge loop's scheduling: a held lane never stalls the others,
//! a held emission goes out first thing once its budget frees, the newest
//! small tree does not wait for a giant, and the probe stops the walk at
//! named sequencer states.

use std::sync::{Arc, Mutex};

use rosetta_adapter_claude_code::ClaudeCodeAdapter;
use rosetta_adapter_codex::CodexAdapter;

use super::super::super::{CapturePool, PoolBudgets};
use super::super::tiny_batches;
use super::{
    Emission, Hold, Setup, aged_home, assert_lanes_match, homes, mixed_home, run_lanes, split_at,
};
use crate::{ColdWalkOptions, LaneId, ProbeView};

/// Holds lane B until `free` says so, counting its retries.
struct HoldBig<F: FnMut(u64) -> bool> {
    retries: u64,
    free: F,
}

impl<F: FnMut(u64) -> bool> Hold for HoldBig<F> {
    fn hold(&mut self, lane: LaneId, retry: bool) -> bool {
        if !lane.is_big() {
            return false;
        }
        if retry {
            self.retries += 1;
        }
        // A walk that blocked on the hold would spin here forever: give up,
        // and let the assertions on what was sent meanwhile fail.
        self.retries <= 100_000 && !(self.free)(self.retries)
    }
}

#[test]
fn a_held_big_lane_never_stalls_lane_s() {
    let home = mixed_home();
    let opts = tiny_batches(4);
    let walk = split_at(8 * 1024);
    let small_sources = 9;
    let small_sent = Arc::new(Mutex::new(0_usize));
    let pool = CapturePool::new(4, PoolBudgets::DEFAULT);
    // B stays held for at least 50 retries and until lane S has sent every
    // source; a walk that blocked on the hold would never get there.
    let counted = Arc::clone(&small_sent);
    let mut policy = HoldBig {
        retries: 0,
        free: move |retries| retries >= 50 && *counted.lock().unwrap() >= small_sources,
    };
    struct Counting<'a, H: Hold> {
        inner: &'a mut H,
        small_sent: Arc<Mutex<usize>>,
    }
    impl<H: Hold> Hold for Counting<'_, H> {
        fn hold(&mut self, lane: LaneId, retry: bool) -> bool {
            if lane == LaneId::Small {
                *self.small_sent.lock().unwrap() += 1;
                return false;
            }
            self.inner.hold(lane, retry)
        }
    }
    let mut counting = Counting {
        inner: &mut policy,
        small_sent: Arc::clone(&small_sent),
    };
    let run = run_lanes(
        &ClaudeCodeAdapter,
        home.path(),
        &opts,
        Setup {
            pool: Some(&pool),
            walk,
            policy: Some(&mut counting),
            ..Setup::default()
        },
    );
    assert!(run.result.is_ok(), "{:?}", run.result);
    assert!(policy.retries >= 50, "{} retries", policy.retries);
    let first_big_sent = run
        .log
        .iter()
        .position(|emission| emission.lane.is_big() && emission.sent)
        .unwrap();
    let small_before: usize = run.log[..first_big_sent]
        .iter()
        .filter(|emission| emission.lane == LaneId::Small && emission.sent)
        .map(|emission| emission.paths.len())
        .sum();
    assert_eq!(
        small_before, small_sources,
        "lane S emitted everything while B held"
    );
    let profile = run.profile.unwrap();
    let mut small = profile
        .lanes
        .iter()
        .find(|lane| lane.lane == LaneId::Small)
        .unwrap()
        .clone();
    let big = profile
        .lanes
        .iter()
        .find(|lane| lane.lane == LaneId::Big(0))
        .unwrap();
    assert!(big.held.total_nanos() > 0, "B's held time is reported");
    assert!(!small.service_delay.is_empty());
    assert!(
        small.service_delay.percentile_nanos(99) < 100_000_000,
        "lane S's service delay p99 {} ns",
        small.service_delay.percentile_nanos(99)
    );
}

#[test]
fn a_held_emission_is_retried_at_the_top_of_every_iteration() {
    let home = mixed_home();
    let opts = tiny_batches(4);
    let walk = split_at(8 * 1024);
    // Every source stays on the sequencer, so lane S is always ready.
    let instrument_free = Arc::new(Mutex::new(false));
    let pool = CapturePool::new(2, PoolBudgets::DEFAULT);
    let frees = Arc::clone(&instrument_free);
    let mut policy = HoldBig {
        retries: 0,
        free: move |_| *frees.lock().unwrap(),
    };
    struct FreeAfter<'a, H: Hold> {
        inner: &'a mut H,
        small_sent: usize,
        free_at: usize,
        free: Arc<Mutex<bool>>,
    }
    impl<H: Hold> Hold for FreeAfter<'_, H> {
        fn hold(&mut self, lane: LaneId, retry: bool) -> bool {
            if lane == LaneId::Small {
                self.small_sent += 1;
                if self.small_sent == self.free_at {
                    *self.free.lock().unwrap() = true;
                }
                return false;
            }
            self.inner.hold(lane, retry)
        }
    }
    let mut free_after = FreeAfter {
        inner: &mut policy,
        small_sent: 0,
        free_at: 4,
        free: Arc::clone(&instrument_free),
    };
    let run = run_lanes(
        &ClaudeCodeAdapter,
        home.path(),
        &opts,
        Setup {
            pool: Some(&pool),
            walk,
            policy: Some(&mut free_after),
            local: true,
            ..Setup::default()
        },
    );
    assert!(run.result.is_ok(), "{:?}", run.result);
    let sent: Vec<&Emission> = run.log.iter().filter(|emission| emission.sent).collect();
    let freed_at = sent
        .iter()
        .enumerate()
        .filter(|(_, emission)| emission.lane == LaneId::Small)
        .nth(3)
        .unwrap()
        .0;
    let held_at = run.log.iter().position(|emission| !emission.sent).unwrap();
    let held_lane = run.log[held_at].lane;
    let resent = sent
        .iter()
        .position(|emission| emission.lane == held_lane && emission.paths == run.log[held_at].paths)
        .unwrap();
    assert!(held_at < freed_at, "B was held before its budget freed");
    assert_eq!(
        resent,
        freed_at + 1,
        "the held emission goes out first thing after its budget frees: {:?}",
        run.log
    );
}

#[test]
fn the_newest_small_tree_does_not_wait_for_the_giant() {
    // A 6 MiB giant, the newest tree, beside small trees; G scaled to 2 MiB.
    let home = aged_home(&[
        ("a-giant", 6_000, 1_000, 1),
        ("b-small", 3, 40, 5),
        ("c-small", 3, 40, 50),
        ("d-small", 3, 40, 80),
    ]);
    let opts = tiny_batches(64);
    let budgets = PoolBudgets {
        look_ahead_bytes: 2 * 1024 * 1024,
        runtime_floor_bytes: 256 * 1024,
        head_bytes: 256 * 1024,
    };
    for (walk, small_first) in [
        (split_at(1024 * 1024), true),
        (
            ColdWalkOptions {
                single_lane: true,
                ..split_at(1024 * 1024)
            },
            false,
        ),
    ] {
        let pool = CapturePool::new(4, budgets);
        let run = run_lanes(
            &ClaudeCodeAdapter,
            home.path(),
            &opts,
            Setup {
                pool: Some(&pool),
                walk: walk.clone(),
                ..Setup::default()
            },
        );
        assert!(run.result.is_ok(), "{:?}", run.result);
        let position = |name: &str| {
            run.emitted
                .iter()
                .position(|(_, path, ..)| path.ends_with(name))
                .unwrap()
        };
        assert_eq!(
            position("b-small.jsonl") < position("a-giant.jsonl"),
            small_first,
            "single lane {}: {:?}",
            walk.single_lane,
            run.emitted
                .iter()
                .map(|(_, path, ..)| path)
                .collect::<Vec<_>>()
        );
    }
}

/// A probe that cancels the walk at the first event `state` holds for, and
/// remembers what it saw there.
fn probe_at(state: fn(&ProbeView) -> bool) -> (crate::WalkProbe, Arc<Mutex<Vec<ProbeView>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    let probe: crate::WalkProbe = Arc::new(move |view: &ProbeView| {
        log.lock().unwrap().push(*view);
        state(view)
    });
    (probe, seen)
}

#[test]
fn the_probe_cancels_at_named_sequencer_states() {
    // (a) Both lane heads mid-source, under some schedule.
    let home = mixed_home();
    let opts = tiny_batches(2);
    let hit = (0..20).any(|seed| {
        let (probe, seen) = probe_at(|view| view.lanes_mid_source >= 2);
        let pool = CapturePool::chaos(4, seed);
        let run = run_lanes(
            &ClaudeCodeAdapter,
            home.path(),
            &opts,
            Setup {
                pool: Some(&pool),
                walk: ColdWalkOptions {
                    probe: Some(probe),
                    ..split_at(8 * 1024)
                },
                ..Setup::default()
            },
        );
        let seen = seen.lock().unwrap();
        let events: Vec<u64> = seen.iter().map(|view| view.event).collect();
        assert!(
            events.windows(2).all(|pair| pair[1] == pair[0] + 1),
            "events count up"
        );
        match run.result {
            Err(error) => {
                assert!(error.contains("cancelled"), "{error}");
                assert!(seen.last().unwrap().lanes_mid_source >= 2);
                true
            }
            Ok(()) => false,
        }
    });
    assert!(hit, "no schedule had both lane heads mid-source");

    // (d) The Codex prefix span mid-group: its shared state database.
    let (_, home) = homes(&CodexAdapter)
        .into_iter()
        .find(|(name, _)| name == "nested-family")
        .unwrap();
    let (probe, seen) = probe_at(|view| view.prefix_mid_group);
    let run = run_lanes(
        &CodexAdapter,
        home.path(),
        &opts,
        Setup {
            walk: ColdWalkOptions {
                probe: Some(probe),
                ..ColdWalkOptions::default()
            },
            ..Setup::default()
        },
    );
    assert!(run.lanes.contains_key(&LaneId::Prefix), "{:?}", run.lanes);
    let error = run.result.expect_err("cancelled mid-group");
    assert!(error.contains("cancelled"), "{error}");
    let seen = seen.lock().unwrap();
    assert!(seen.last().unwrap().prefix_mid_group);
    assert_eq!(seen.last().unwrap().event, 1, "P runs first");
}

/// Holds span P's emissions for its first `retries` retries.
struct HoldPrefix {
    retries: u64,
    until: u64,
}

impl Hold for HoldPrefix {
    fn hold(&mut self, lane: LaneId, retry: bool) -> bool {
        if lane != LaneId::Prefix {
            return false;
        }
        if retry {
            self.retries += 1;
        }
        self.retries < self.until
    }
}

#[test]
fn a_held_prefix_keeps_every_lane_behind_it() {
    // The Codex family's shared state database is span P: it runs serially
    // and whole before any lane, even while its emission is held, so no
    // tree's source reaches the writer before a member P has not sent.
    let (_, home) = homes(&CodexAdapter)
        .into_iter()
        .find(|(name, _)| name == "nested-family")
        .unwrap();
    let opts = tiny_batches(2);
    let serial = run_lanes(&CodexAdapter, home.path(), &opts, Setup::default());
    assert!(serial.result.is_ok(), "{:?}", serial.result);
    assert!(
        serial.lanes.contains_key(&LaneId::Prefix),
        "{:?}",
        serial.lanes
    );
    for workers in [1, 4] {
        let pool = CapturePool::new(workers, PoolBudgets::DEFAULT);
        let mut policy = HoldPrefix {
            retries: 0,
            until: 25,
        };
        let run = run_lanes(
            &CodexAdapter,
            home.path(),
            &opts,
            Setup {
                pool: Some(&pool),
                policy: Some(&mut policy),
                ..Setup::default()
            },
        );
        assert_eq!(policy.retries, 25, "P was held and retried");
        let label = format!("held prefix, {workers} workers");
        let last_prefix = run
            .log
            .iter()
            .rposition(|emission| emission.lane == LaneId::Prefix && emission.sent)
            .unwrap();
        let first_lane = run
            .log
            .iter()
            .position(|emission| emission.lane != LaneId::Prefix)
            .unwrap();
        assert!(
            last_prefix < first_lane,
            "{label}: a lane emitted before P was sent: {:?}",
            run.log
        );
        assert_lanes_match(&label, &serial, &run);
    }
}
