//! Per-unit attribution (R1 brief critique major 1): a unit's payload,
//! losses and restarts come only from its own events, whatever two lanes'
//! interleaving — a restart and heal, a source that vanishes mid-pass and one
//! gone before its first step, beside a healthy big source.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use rosetta_adapter_claude_code::ClaudeCodeAdapter;
use rosetta_adapter_support::AgentAdapter;
use tempfile::TempDir;

use super::super::super::CapturePool;
use super::super::{
    HEAL_JOURNAL, HEAL_TRANSCRIPT, RewriteOnce, assert_same_dump, claude_line, claude_user,
    tiny_batches,
};
use super::{LaneRun, Setup, aged_home, pin_age, run_lanes, split_at};
use crate::capture::tests_switch;
use crate::progress::ColdProgressObserver;
use crate::{IngestOptions, LaneId, SessionFile, expand_data_root};

/// Rewrites one source after its first batch and deletes another after its
/// first batch.
#[derive(Debug)]
struct Faults {
    rewrite: RewriteOnce,
    vanish: (std::path::PathBuf, String),
    vanished: AtomicBool,
}

impl ColdProgressObserver for Faults {
    fn source_bytes(&self, file: &SessionFile, bytes: u64) {
        self.rewrite.source_bytes(file, bytes);
        if file.relative_path == self.vanish.1 && !self.vanished.swap(true, Ordering::SeqCst) {
            std::fs::remove_file(self.vanish.0.join(&self.vanish.1)).unwrap();
        }
    }

    fn reading_finished(&self) {}
}

/// A restarting two-transcript session and two that vanish (one after its
/// first batch, one before its first step) in lane S, beside a healthy big
/// source in lane B.
struct FaultHome {
    home: TempDir,
    root: std::path::PathBuf,
}

const VANISH: &str = "projects/-w-pool/vanish.jsonl";
const GONE: &str = "projects/-w-pool/gone.jsonl";

impl FaultHome {
    fn new() -> Self {
        let home = aged_home(&[
            ("big", 300, 200, 20),
            ("vanish", 20, 40, 10),
            ("gone", 4, 40, 5),
        ]);
        let root = expand_data_root(&ClaudeCodeAdapter.layout().data_root, home.path());
        let journal = root.join(HEAL_JOURNAL);
        std::fs::create_dir_all(journal.parent().unwrap()).unwrap();
        std::fs::write(
            &journal,
            [
                r#"{"type":"started","key":"worker-1","agentId":"agent-1"}"#,
                r#"{"type":"result","key":"worker-1","result":{"ok":true}}"#,
            ]
            .join("\n")
                + "\n",
        )
        .unwrap();
        Self { home, root }
    }

    /// Restore every file, and the options whose observer breaks them.
    fn reset(&self) -> IngestOptions {
        let lines = |prefix: &str| -> String {
            (1..=5)
                .map(|index| {
                    claude_user(
                        &format!("{prefix}-{index}"),
                        &format!("{prefix} line {index}"),
                    ) + "\n"
                })
                .collect()
        };
        let transcript = self.root.join(HEAL_TRANSCRIPT);
        std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
        std::fs::write(&transcript, lines("old")).unwrap();
        pin_age(&transcript, 15);
        for (session, records, age) in [("vanish", 20, 10), ("gone", 4, 5)] {
            let path = self.root.join(format!("projects/-w-pool/{session}.jsonl"));
            let body: String = (0..records)
                .map(|index| {
                    claude_line(session, &format!("{session}-{index}"), &"x".repeat(40)) + "\n"
                })
                .collect();
            std::fs::write(&path, body).unwrap();
            pin_age(&path, age);
        }
        IngestOptions {
            progress: Some(Arc::new(Faults {
                rewrite: RewriteOnce {
                    root: self.root.clone(),
                    target: HEAL_TRANSCRIPT.into(),
                    replacement: lines("new"),
                    done: AtomicBool::new(false),
                },
                vanish: (self.root.clone(), VANISH.into()),
                vanished: AtomicBool::new(false),
            })),
            ..tiny_batches(2)
        }
    }

    fn run(&self, pool: Option<&CapturePool>) -> LaneRun {
        let opts = self.reset();
        let gone = self.root.join(GONE);
        run_lanes(
            &ClaudeCodeAdapter,
            self.home.path(),
            &opts,
            Setup {
                pool,
                walk: split_at(16 * 1024),
                before_walk: Some(&|| std::fs::remove_file(&gone).unwrap()),
                ..Setup::default()
            },
        )
    }
}

/// A dump without the vanished source's tail identity: the harness
/// recreates that file for every run, so its identity is the run's own.
fn comparable(dump: &BTreeMap<String, Vec<String>>) -> BTreeMap<String, Vec<String>> {
    let mut dump = dump.clone();
    for row in dump.get_mut("ingest_cursor").into_iter().flatten() {
        if row.contains(VANISH) {
            *row = row.split(" | checkpoint=").next().unwrap().to_owned();
        }
    }
    dump
}

/// Each source as it was emitted, by path.
fn closed_by_path(run: &LaneRun) -> BTreeMap<String, (LaneId, u64, u64, bool, bool)> {
    run.emitted
        .iter()
        .map(|(lane, path, payload, generation, skipped, complete)| {
            (
                path.clone(),
                (*lane, *payload, *generation, *skipped, *complete),
            )
        })
        .collect()
}

#[test]
fn every_unit_closes_from_its_own_events_whatever_the_interleaving() {
    let faults = FaultHome::new();
    let serial = faults.run(None);
    assert!(serial.result.is_ok(), "{:?}", serial.result);
    let closed = closed_by_path(&serial);
    let journal = closed
        .keys()
        .find(|path| path.ends_with("journal.jsonl"))
        .unwrap();
    // (i) The transcript restarts and only its session heals: its journal
    // replays into a second generation.
    assert_eq!(closed[HEAL_TRANSCRIPT].2, 2, "{closed:?}");
    assert_eq!(closed[journal].2, 2, "{closed:?}");
    assert_eq!(
        serial.report[16], 1,
        "one sibling healed: {:?}",
        serial.report
    );
    // (ii) The vanished source keeps what it read; the big one is whole.
    assert!(!closed[VANISH].3 && !closed[VANISH].4, "{closed:?}");
    let big = closed
        .iter()
        .find(|(path, _)| path.ends_with("big.jsonl"))
        .unwrap()
        .1;
    assert_eq!(big.0, LaneId::Big(0));
    assert!(!big.3 && big.4, "the big source closes whole: {big:?}");
    // (iii) The source gone before its first step closes skipped.
    assert!(closed[GONE].3, "{closed:?}");

    for seed in 0..20 {
        let pool = CapturePool::chaos(4, seed);
        let pooled = faults.run(Some(&pool));
        assert_eq!(pooled.result, serial.result, "seed {seed}");
        assert_eq!(closed_by_path(&pooled), closed, "seed {seed}");
        assert_eq!(pooled.report, serial.report, "seed {seed}");
        assert_same_dump(
            &format!("faults seed {seed}"),
            &comparable(&serial.canonical),
            &comparable(&pooled.canonical),
        );
    }

    // v1's mutation — attribution from differences of the walk's report —
    // misattributes under some schedule.
    tests_switch::ATTRIBUTION_FROM_GLOBAL.set(true);
    let caught = (0..20).any(|seed| {
        let pool = CapturePool::chaos(4, seed);
        let pooled = faults.run(Some(&pool));
        pooled.result != serial.result || closed_by_path(&pooled) != closed
    });
    tests_switch::ATTRIBUTION_FROM_GLOBAL.set(false);
    assert!(caught, "global attribution went unnoticed in 20 schedules");
}
