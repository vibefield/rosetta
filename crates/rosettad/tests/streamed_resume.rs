//! R1 brief §10 test 7: rebuild equivalence for interleaved ledgers.
//!
//! A pooled recency walk frames its lanes' sources into the raw ledger as
//! the sequencer consumes them, so one source's frames are interleaved with
//! another lane's; and the writer, stepping a big-lane source in slices
//! between lane S's items, commits in yet another order. For five seeded
//! pooled chaos builds of each corpus, `rebuild` of the build's own ledger
//! (in process, as `rosettad rebuild` replays it) yields the fresh build's
//! logical projection over the same cursors.
//!
//! Surrogate ids are not compared. A rebuild numbers each cursor when the
//! ledger's first frame of its source arrives, so an interleaved ledger
//! numbers them in frame order, not the fresh build's path rank: the
//! logical projection is equal, the strict section (cursor and message row
//! ids) is not.
//!
//! Test 6, resume after a kill at named sequencer states, runs on the real
//! `serve` in process (`rosettad/src/serve/resume_kill_tests.rs`): its walk
//! probe and writer counters are hidden knobs no flag spells.

mod support;

use std::collections::BTreeMap;
use std::path::Path;

use rosetta_adapter_claude_code::ClaudeCodeAdapter;
use rosetta_adapter_codex::CodexAdapter;
use rosetta_adapter_support::AgentAdapter;
use rosetta_bench_corpus::Agent;
use rosetta_pipeline::{Instrument, RebuildOptions, rebuild_runtime_with_options};
use rosetta_store::Store;
use tempfile::TempDir;

use support::fixtures::{ClaudeTranscript, T0, claude_root, claude_usage, codex_nested, generated};
use support::harness::cold_store;
use support::projection_dump::{assert_same_dump, dump_ignoring};
use support::writer::{BuildSpec, Built, Schedule, Split, build};

const KIB: u64 = 1024;
/// A cursor's checkpoint is capture state (the tailer's file identity and
/// read position, the adapter's resumable state), which a rebuild derives
/// from the ledger instead: provenance, not projection.
const CHECKPOINTS: &[(&str, &str)] = &[("ingest_cursor", "checkpoint")];
const HOUR: i64 = 3_600_000;

/// The cursor table's surrogate ids by path.
fn cursor_ids(store: &Store) -> BTreeMap<String, i64> {
    let mut statement = store
        .conn()
        .prepare("SELECT path, id FROM ingest_cursor")
        .unwrap();
    statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

/// Rebuild `built`'s ledger into a fresh private projection; also returns
/// how many usage copies the replay displaced (met before their owner).
fn rebuild(adapter: &dyn AgentAdapter, built: &Built) -> (Store, u64) {
    let mut store = Store::open_in_memory_for_rebuild().unwrap();
    let report = rebuild_runtime_with_options(
        adapter,
        built.runtime.as_ref().unwrap(),
        built.ledger.as_ref().unwrap().path(),
        &mut store,
        &Instrument::new(),
        RebuildOptions::default(),
    )
    .unwrap();
    store.finish_bulk_load().unwrap();
    (store, report.stats.usage_copies_displaced)
}

/// Whether the ledger frames its sources out of path order: in the walk's
/// lane-merged order (each one-batch source's frames are contiguous; a
/// source of several batches interleaves with another lane's).
fn out_of_path_order(entries: &[String]) -> bool {
    let mut seen: Vec<&str> = Vec::new();
    for entry in entries {
        let path = entry.split(' ').next().unwrap();
        if !seen.contains(&path) {
            seen.push(path);
        }
    }
    seen.windows(2).any(|pair| pair[0] > pair[1])
}

/// Five seeded chaos builds of `home` — pooled on `CapturePool::chaos(4,
/// seed)`, or walked serially — each with a seeded writer schedule, each
/// rebuilt from its own ledger. Returns how many of the ledgers framed
/// sources out of path order, and how many usage copies the rebuilds met
/// before their owner.
fn rebuild_equivalence(
    adapter: &'static dyn AgentAdapter,
    home: &Path,
    split: Split,
    pooled: bool,
) -> (usize, u64) {
    let mut interleavings = 0;
    let mut displaced = 0;
    for seed in 1..=5 {
        let spec = BuildSpec {
            split,
            pool: pooled.then_some(seed),
            chunk: 16 * KIB,
            schedule: Schedule::Seeded(seed),
            ..BuildSpec::declared()
        };
        let mut fresh = cold_store();
        let built = build(adapter, home, &spec, &mut fresh, None);
        assert_eq!(built.result, Ok(()), "seed {seed}");
        assert!(!built.path_walk, "seed {seed}: a recency walk");
        interleavings += usize::from(out_of_path_order(&built.entries));
        let (rebuilt, copies) = rebuild(adapter, &built);
        displaced += copies;
        assert_same_dump(
            &format!("{} seed {seed}: rebuilt", adapter.kind()),
            &dump_ignoring(&fresh, CHECKPOINTS),
            &dump_ignoring(&rebuilt, CHECKPOINTS),
        );
        assert_eq!(
            cursor_ids(&rebuilt).keys().collect::<Vec<_>>(),
            cursor_ids(&fresh).keys().collect::<Vec<_>>(),
            "seed {seed}: the same cursors"
        );
    }
    (interleavings, displaced)
}

/// A fork copies its origin's response (row and advisor/compaction
/// iteration samples) into another tree. The fork is the newest tree and
/// walks lane B — its transcript, with the copy late in it, is over the
/// split — while the origin walks lane S. On the serial walk the fork's
/// frames lead the ledger, so a rebuild meets the copy first; the writer,
/// stepping the fork in slices with the origin's one transaction between
/// them, commits the origin first. Only an order-free usage owner makes
/// the two agree.
fn fork_across_lanes(home: &Path) {
    let project = claude_root(home).join("projects/-w-fork");
    let iterations = serde_json::json!([
        {"type": "message", "input_tokens": 300, "output_tokens": 40},
        {"type": "advisor_message", "model": "claude-haiku-4-5",
         "input_tokens": 90, "output_tokens": 12},
        {"type": "compaction", "input_tokens": 900, "output_tokens": 60},
    ]);
    let usage = claude_usage(300, 40, iterations);
    let mut origin = ClaudeTranscript::new("origin", None, T0);
    origin
        .prompt("plan the change")
        .reply_as(
            "msg_fork_1",
            "req_fork_1",
            "here is the plan",
            usage.clone(),
        )
        .tool("git status", "clean");
    origin.write(&project.join("origin.jsonl"), T0 + 2 * HOUR);
    let mut fork = ClaudeTranscript::new("fork", None, T0 + HOUR);
    fork.prompt("a long preamble")
        .bulk(48, 4096)
        .prompt("plan the change")
        .reply_as("msg_fork_1", "req_fork_1", "here is the plan", usage)
        .tool("git diff", "+one line");
    fork.write(&project.join("fork.jsonl"), T0 + 5 * HOUR);
    // Older single-session trees keep lane S walking beside the fork.
    for index in 0..6 {
        let id = format!("s{index}");
        let mut other = ClaudeTranscript::new(&id, None, T0);
        other.prompt("a question").bulk(2, 512);
        other.write(
            &claude_root(home).join(format!("projects/-w-{id}/{id}.jsonl")),
            T0 + HOUR - index * 60_000,
        );
    }
}

#[test]
fn a_cross_lane_fork_rebuilds_from_its_interleaved_ledger_into_the_fresh_projection() {
    let home = TempDir::new().unwrap();
    fork_across_lanes(home.path());
    let (_, displaced) = rebuild_equivalence(
        &ClaudeCodeAdapter,
        home.path(),
        Split::Bytes(64 * KIB),
        false,
    );
    assert_eq!(displaced, 5, "each rebuild met the copy before its owner");
}

#[test]
fn generated_claude_and_codex_corpora_rebuild_from_interleaved_ledgers() {
    let claude = TempDir::new().unwrap();
    generated(claude.path(), &ClaudeCodeAdapter, Agent::ClaudeCode, 17);
    let (interleaved, _) =
        rebuild_equivalence(&ClaudeCodeAdapter, claude.path(), Split::Median, true);
    assert!(
        interleaved > 0,
        "some pooled ledger framed its sources in walk order, not path order"
    );
    let codex = TempDir::new().unwrap();
    generated(codex.path(), &CodexAdapter, Agent::Codex, 19);
    rebuild_equivalence(&CodexAdapter, codex.path(), Split::Median, true);
    let nested = TempDir::new().unwrap();
    codex_nested(nested.path());
    rebuild_equivalence(&CodexAdapter, nested.path(), Split::Plan, true);
}
