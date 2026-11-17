//! Measurement only: every transcript a walk emits carries what its own
//! capture steps measured — at least one step per batch, a wall from its
//! first step's start to its last step's end no shorter than its longest
//! step, and its largest framed record — on the serial driver, on pool
//! workers under the lane-merge walk, and on pool workers under the path
//! walk. A source captured in a physical group carries none.

use std::collections::BTreeMap;

use rosetta_adapter_claude_code::ClaudeCodeAdapter;

use super::super::super::{CapturePool, PoolBudgets};
use super::super::tiny_batches;
use super::{Setup, aged_home, run_lanes, split_at};
use crate::{ColdWalkOptions, WalkOverride};

#[test]
fn every_transcript_carries_its_own_capture_steps() {
    let sessions = [
        ("a-big", 60, 300, 50),
        ("b-small", 3, 40, 10),
        ("c-big", 40, 900, 5),
        ("d-small", 1, 25, 70),
    ];
    let home = aged_home(&sessions);
    let opts = tiny_batches(4);
    // Each file's largest line and its record count.
    let mut expected: BTreeMap<String, (u64, usize)> = BTreeMap::new();
    for entry in walkdir(home.path()) {
        let text = std::fs::read_to_string(&entry).unwrap();
        let longest = text.lines().map(str::len).max().unwrap() as u64;
        let name = entry.file_name().unwrap().to_string_lossy().into_owned();
        expected.insert(name, (longest, text.lines().count()));
    }
    assert_eq!(expected.len(), sessions.len());
    let lanes = split_at(8 * 1024);
    let path = ColdWalkOptions {
        walk_override: Some(WalkOverride::Path),
        ..lanes.clone()
    };
    for (label, workers, walk) in [
        ("serial", 0, lanes.clone()),
        ("pooled lanes", 2, lanes.clone()),
        ("pooled path", 2, path),
    ] {
        // A pool closes when its scope ends: one per walk.
        let pool = CapturePool::new(workers, PoolBudgets::DEFAULT);
        let run = run_lanes(
            &ClaudeCodeAdapter,
            home.path(),
            &opts,
            Setup {
                pool: (workers > 0).then_some(&pool),
                walk,
                ..Setup::default()
            },
        );
        assert!(run.result.is_ok(), "{label}: {:?}", run.result);
        assert_eq!(run.clocks.len(), sessions.len(), "{label}");
        for (relative, clock) in &run.clocks {
            let name = relative.rsplit('/').next().unwrap();
            let (longest, records) = expected[name];
            assert!(
                clock.steps() as usize >= records.div_ceil(4),
                "{label} {name}: {} steps",
                clock.steps()
            );
            assert!(clock.longest_step_nanos() > 0, "{label} {name}");
            assert!(
                clock.wall_nanos() >= clock.longest_step_nanos(),
                "{label} {name}"
            );
            assert_eq!(clock.max_record_bytes(), longest, "{label} {name}");
        }
    }
}

fn walkdir(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            out.extend(walkdir(&path));
        } else if path.extension().is_some_and(|ext| ext == "jsonl") {
            out.push(path);
        }
    }
    out
}
