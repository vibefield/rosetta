//! R1 brief §10 test 6: resume after a kill at named sequencer states, on the
//! real `serve` in process.
//!
//! For each root-tree corpus a walk probe (`ColdWalkOptions.probe`, a
//! predicate over `ProbeView`) cancels the fresh build at the first event a
//! named state holds:
//!
//! - (a) both lane heads mid-source (`lanes_mid_source ≥ 2`);
//! - (b) a chunked writer item between its first and terminal transaction
//!   (`items_mid_sequence ≥ 1`);
//! - (c) a flip batch committed (`flips_committed ≥ 1`);
//! - (d) span `P` mid-group (`prefix_mid_group`);
//! - (e)–(l) eight seeded event counts (every event of a corpus with fewer).
//!
//! The writer's states (b) and (c) are read live at each event, polling
//! (without sleeping) for a bounded time for the writer to reach them, so
//! the sequencer cannot run past them on a fast walk; at the walk's last
//! event it waits longer, since the parked walk leaves the writer applying
//! what is queued. (a) depends on two pool heads interleaving; it, (b) and
//! (c) each get a few attempts. Each corpus declares the named states it produces;
//! every one must be hit, and every one it cannot produce (listed with the
//! corpus) must not be.
//!
//! The cancellation takes `serve`'s failure path: the staged ledger is
//! synced (the barrier a pause or a stop also runs), nothing is installed,
//! and the run is recorded failed. The next `serve` finds it failed and
//! waits; a resume command (the app's retry) replays the staged generation,
//! catches up and publishes. Asserted:
//! - the resumed projection equals the clean build's;
//! - no listed session is non-Pending before the resume publishes (R0
//!   `WithheldReadiness`; sampled over HTTP while it runs);
//! - every source the staged ledger left unsealed (`replay.rs`'s unsealed
//!   map) is settled: the published ledger seals it at or past that
//!   generation, and no raw generation is left unsealed;
//! - the published archive rebuilds (`rosettad rebuild`, strict) into the
//!   clean projection.
//!
//! A kill that skips the staged ledger's fsync is not observable here: the
//! sources are unchanged, so the resume's catch-up rereads whatever the
//! staged prefix lacks; only a crash or power loss would tell.
//!
//! Corpora that cannot run under `serve`: the two-transcript restart fixture
//! rewrites its transcript from a capture progress hook, an
//! `IngestOptions` hook `serve` does not take (the harness tests cover it).

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use rosetta_pipeline::{ProbeCounters, ProbeView, WalkProbe};
use tempfile::TempDir;

use super::inprocess::{control, flags, get, run, seals, spawn, staged_ledger, unsealed};
use crate::Flags;
use crate::test_support::fixtures::{
    ClaudeTranscript, T0, claude_fork, claude_nested, claude_root, codex_nested, giant_newest,
};
use crate::test_support::projection_dump::{Dump, dump_conn, dump_difference};

const KIB: u64 = 1024;
const MIB: usize = 1024 * 1024;

/// A named sequencer state (a)–(d).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Named {
    LanesMidSource,
    ItemsMidSequence,
    FlipsCommitted,
    PrefixMidGroup,
}

const NAMED: [Named; 4] = [
    Named::LanesMidSource,
    Named::ItemsMidSequence,
    Named::FlipsCommitted,
    Named::PrefixMidGroup,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Named(Named),
    /// (e)–(l): the n-th sequencer event.
    Event(u64),
}

struct Corpus {
    name: &'static str,
    agent: &'static str,
    /// The runtime's ledger directory under a ledger root.
    ledger: &'static str,
    write: fn(&Path),
    /// Hidden knobs, given the home.
    tune: fn(&mut Flags, &Path),
    /// The named states this corpus produces; it cannot produce the rest.
    produces: &'static [Named],
    /// Some source spans several capture batches, so a kill can fall
    /// between two of them and leave it unsealed. A one-batch source seals
    /// in the event that frames it.
    spans_batches: bool,
}

/// What a kill and its resume showed.
#[derive(Debug, Default)]
struct Outcome {
    hit: Vec<State>,
    missed: Vec<State>,
    /// Sources the staged ledger left unsealed, summed over the kills.
    unsealed: usize,
    /// Reads sampled while a resume had not published yet.
    reads_before_publication: u64,
}

/// Every transcript under `root`, sorted.
fn transcripts(root: &Path) -> Vec<std::path::PathBuf> {
    let mut files = crate::test_support::harness::walkdir(root);
    files.retain(|path| path.extension().is_some_and(|ext| ext == "jsonl"));
    files
}

/// Wait up to `limit` for `holds`, polling without sleeping: a chunked item
/// passes through its slices in a few milliseconds, which a sleeping poll
/// (macOS rounds short sleeps up, more so on a loaded host) can step over.
fn within(limit: Duration, holds: impl Fn() -> bool) -> bool {
    let deadline = Instant::now() + limit;
    loop {
        if holds() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::yield_now();
    }
}

/// The probe that cancels at the first event `state` holds at. The walk
/// has `total` events; at the last one the sequencer waits longer for the
/// writer, which keeps applying what is queued while the walk is parked, so
/// a loaded host's late writer still reaches its states before the close.
fn probe(
    state: State,
    total: u64,
    counters: Arc<ProbeCounters>,
    hit: Arc<AtomicBool>,
) -> WalkProbe {
    Arc::new(move |view: &ProbeView| {
        let wait =
            |usual: u64| Duration::from_millis(if view.event >= total { 5_000 } else { usual });
        let holds = match state {
            State::Event(n) => view.event >= n,
            State::Named(Named::LanesMidSource) => view.lanes_mid_source >= 2,
            State::Named(Named::PrefixMidGroup) => view.prefix_mid_group,
            State::Named(Named::ItemsMidSequence) => within(wait(150), || {
                counters.items_mid_sequence.load(Ordering::SeqCst) >= 1
            }),
            State::Named(Named::FlipsCommitted) => within(wait(600), || {
                counters.flips_committed.load(Ordering::SeqCst) >= 1
            }),
        };
        if holds {
            hit.store(true, Ordering::SeqCst);
        }
        holds
    })
}

/// The published projection, as the equivalence tests compare it. Readiness
/// bookkeeping, cursors (a resumed source's generation lies above its
/// interrupted one) and the runtime row (discovery time) are left out.
fn published(db: &Path) -> Dump {
    let conn = rusqlite::Connection::open(db).unwrap();
    let mut dump = dump_conn(&conn, &[]);
    for table in [
        "ingest_cursor",
        "agent_runtime",
        "projection_state",
        "search_dirty",
    ] {
        dump.remove(table);
    }
    dump
}

/// Eight distinct event counts in `1..=total`, seeded by the corpus name.
fn seeded_events(name: &str, total: u64) -> Vec<u64> {
    let mut seed = name.bytes().fold(0x9e37_79b9_7f4a_7c15_u64, |acc, byte| {
        (acc ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    }) | 1;
    let mut events = Vec::new();
    while events.len() < 8.min(total as usize) {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let event = 1 + seed % total;
        if !events.contains(&event) {
            events.push(event);
        }
    }
    events
}

/// Resume the staged generation of `db` as the app does: the next serve
/// finds the cancelled run failed and waits; a resume command restarts it.
/// `/api/sessions` is sampled until the serve ends. Returns the samples
/// taken while the resumed build ran and had not published.
fn resume(corpus: &Corpus, home: &Path, db: &Path, announce: &Path) -> u64 {
    let running = spawn(
        flags(home, db, corpus.agent, announce, |flags| {
            (corpus.tune)(flags, home)
        }),
        announce,
    );
    let deadline = Instant::now() + Duration::from_secs(60);
    let failed = loop {
        let (_, status) = get(&running.origin, "/api/status");
        if status["daemon"]["ingest"]["run"]["state"] == "failed" {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "{}: not parked: {status}",
            corpus.name
        );
        std::thread::sleep(Duration::from_millis(5));
    };
    assert_eq!(control(&running.origin, &failed, "resume", 1), 202);
    let mut before = 0;
    let mut violations = Vec::new();
    while !running.is_finished() {
        let (_, sessions) = get(&running.origin, "/api/sessions?limit=500");
        // Read the publication state after the list: a list sampled while
        // the status still says unpublished was read before publication.
        let (code, status) = get(&running.origin, "/api/status");
        std::thread::sleep(Duration::from_millis(2));
        if code == 200 && status["canonical_ready"] == false {
            before += 1;
            for session in sessions["sessions"].as_array().into_iter().flatten() {
                if session["stream_state"] != "pending" {
                    violations.push(session.clone());
                }
            }
        }
    }
    running.join().unwrap();
    assert!(
        violations.is_empty(),
        "{}: sessions readable before the resume published: {violations:?}",
        corpus.name
    );
    before
}

fn kill_and_resume(corpus: &Corpus, home: &Path, temp: &Path, clean: &Dump) -> Outcome {
    // A clean build that counts the sequencer's events.
    let events = Arc::new(AtomicU64::new(0));
    let counting: WalkProbe = {
        let events = Arc::clone(&events);
        Arc::new(move |view: &ProbeView| {
            events.fetch_max(view.event, Ordering::SeqCst);
            false
        })
    };
    let total_db = temp.join("counted/rosetta.db");
    run(flags(
        home,
        &total_db,
        corpus.agent,
        &temp.join("a"),
        |flags| {
            (corpus.tune)(flags, home);
            flags.cold_walk.probe = Some(counting);
        },
    ))
    .unwrap();
    assert_eq!(
        &published(&total_db),
        clean,
        "{}: a probe changes nothing",
        corpus.name
    );
    let total = events.load(Ordering::SeqCst);
    assert!(total >= 1, "{}: {total} sequencer events", corpus.name);
    let states = NAMED.iter().map(|named| State::Named(*named)).chain(
        seeded_events(corpus.name, total)
            .into_iter()
            .map(State::Event),
    );
    let mut outcome = Outcome::default();
    for (index, state) in states.enumerate() {
        // A named state a corpus produces depends on threads meeting: (a)
        // on two pool heads interleaving, (b) and (c) on the writer keeping
        // pace with the walk. Each gets a few fresh builds; one hit is
        // enough. A state the corpus cannot produce gets one, which must
        // miss.
        let attempts = match state {
            State::Named(Named::LanesMidSource)
                if corpus.produces.contains(&Named::LanesMidSource) =>
            {
                6
            }
            State::Named(named @ (Named::ItemsMidSequence | Named::FlipsCommitted))
                if corpus.produces.contains(&named) =>
            {
                4
            }
            _ => 1,
        };
        let mut hit = false;
        for attempt in 0..attempts {
            let run_dir = temp.join(format!("kill-{index}-{attempt}"));
            let db = run_dir.join("rosetta.db");
            let announce = run_dir.join("announce");
            let counters = Arc::new(ProbeCounters::default());
            let fired = Arc::new(AtomicBool::new(false));
            let killed = run(flags(home, &db, corpus.agent, &announce, |flags| {
                (corpus.tune)(flags, home);
                flags.cold_walk.probe = Some(probe(
                    state,
                    total,
                    Arc::clone(&counters),
                    Arc::clone(&fired),
                ));
                flags.cold_walk.probe_counters = Some(Arc::clone(&counters));
            }));
            if !fired.load(Ordering::SeqCst) {
                killed.unwrap();
                assert_eq!(
                    &published(&db),
                    clean,
                    "{}: {state:?} never held",
                    corpus.name
                );
                continue;
            }
            hit = true;
            let error = killed.expect_err("the probe cancels the build");
            assert!(
                error.contains("cancel"),
                "{}: {state:?}: {error}",
                corpus.name
            );
            assert!(
                !db.exists(),
                "{}: a cancelled build installs nothing",
                corpus.name
            );
            let staged = staged_ledger(&db);
            assert!(
                staged.is_dir(),
                "{}: {state:?} left no staged generation",
                corpus.name
            );
            let left = unsealed(&staged.join(corpus.ledger));
            outcome.unsealed += left.len();
            outcome.reads_before_publication += resume(corpus, home, &db, &announce);
            if let Some(difference) = dump_difference(clean, &published(&db)) {
                panic!("{}: resumed after {state:?}: {difference}", corpus.name);
            }
            let ledger = db.with_extension("ledger").join(corpus.ledger);
            let sealed = seals(&ledger);
            for (path, generation) in &left {
                assert!(
                    sealed.get(path).is_some_and(|seal| seal >= generation),
                    "{}: {path} unsealed at g{generation} after {state:?}: {sealed:?}",
                    corpus.name
                );
            }
            assert_eq!(
                unsealed(&ledger),
                BTreeMap::new(),
                "{}: the published ledger leaves a source unsealed",
                corpus.name
            );
            // The archive the resume published rebuilds, strictly, into the
            // projection it published (`rosettad rebuild`).
            let rebuilt = run_dir.join("rebuilt/rosetta.db");
            crate::cmd_rebuild(&[
                "--db".to_string(),
                rebuilt.display().to_string(),
                "--ledger".to_string(),
                db.with_extension("ledger").display().to_string(),
            ])
            .unwrap_or_else(|error| panic!("{}: rebuilding after {state:?}: {error}", corpus.name));
            if let Some(difference) = dump_difference(clean, &published(&rebuilt)) {
                panic!("{}: rebuilt after {state:?}: {difference}", corpus.name);
            }
            break;
        }
        if hit {
            outcome.hit.push(state);
        } else {
            outcome.missed.push(state);
        }
    }
    outcome
}

fn check(corpus: &Corpus) {
    let temp = TempDir::new().unwrap();
    let home = temp.path().join("home");
    (corpus.write)(&home);
    let clean_db = temp.path().join("clean/rosetta.db");
    run(flags(
        &home,
        &clean_db,
        corpus.agent,
        &temp.path().join("clean/announce"),
        |flags| (corpus.tune)(flags, &home),
    ))
    .unwrap();
    let clean = published(&clean_db);
    let outcome = kill_and_resume(corpus, &home, temp.path(), &clean);
    eprintln!("{}: {outcome:?}", corpus.name);
    for named in NAMED {
        let state = State::Named(named);
        if corpus.produces.contains(&named) {
            assert!(
                outcome.hit.contains(&state),
                "{}: {named:?} was never hit",
                corpus.name
            );
        } else {
            assert!(
                outcome.missed.contains(&state),
                "{}: {named:?} is listed as impossible, yet it held",
                corpus.name
            );
        }
    }
    let seeded = outcome
        .hit
        .iter()
        .filter(|state| matches!(state, State::Event(_)))
        .count();
    // Eight seeded event counts, or every event of a corpus with fewer.
    assert_eq!(
        seeded,
        outcome.hit.len() + outcome.missed.len() - NAMED.len(),
        "{}: every seeded event was reached",
        corpus.name
    );
    assert_eq!(
        outcome.unsealed > 0,
        corpus.spans_batches,
        "{}: kills left {} unsealed sources to settle",
        corpus.name,
        outcome.unsealed
    );
    assert!(
        outcome.reads_before_publication > 0,
        "{}: the resumes were read before they published",
        corpus.name
    );
}

/// Root `m` with a scaled subagent giant (and its meta document), a nested
/// subagent, a sidecar-only subagent under a placeholder, and an older `n`.
/// One lane, no span `P`: (a) and (d) cannot hold.
#[test]
fn claude_nested_resumes_after_a_kill_at_every_named_state() {
    check(&Corpus {
        name: "claude-nested",
        agent: "claude-code",
        ledger: "claude-code",
        write: |home| claude_nested(home, 256 * MIB / 1024),
        tune: |flags, _| flags.cold_chunk_bytes = Some(16 * KIB),
        produces: &[Named::ItemsMidSequence, Named::FlipsCommitted],
        spans_batches: false,
    });
}

/// A cross-tree fork copy (usage with advisor and compaction iterations),
/// landed copy-first. One lane, no span `P`.
#[test]
fn claude_fork_resumes_after_a_kill_at_every_named_state() {
    check(&Corpus {
        name: "claude-fork",
        agent: "claude-code",
        ledger: "claude-code",
        write: claude_fork,
        tune: |flags, _| flags.cold_chunk_bytes = Some(512),
        produces: &[Named::ItemsMidSequence, Named::FlipsCommitted],
        spans_batches: false,
    });
}

/// Codex root → child → grandchild with a guardian, a state-only session
/// and a metadata-only placeholder root: span `P` (the state database) runs
/// whole before lane S, so (a) cannot hold.
#[test]
fn codex_nested_resumes_after_a_kill_at_every_named_state() {
    check(&Corpus {
        name: "codex-nested",
        agent: "codex",
        ledger: "codex",
        write: codex_nested,
        tune: |flags, _| flags.cold_chunk_bytes = Some(128),
        produces: &[
            Named::ItemsMidSequence,
            Named::FlipsCommitted,
            Named::PrefixMidGroup,
        ],
        spans_batches: false,
    });
}

/// The giant-newest corpus, scaled (G = 1 MiB, chunk = 64 KiB). One lane.
#[test]
fn giant_newest_resumes_after_a_kill_at_every_named_state() {
    check(&Corpus {
        name: "giant-newest",
        agent: "claude-code",
        ledger: "claude-code",
        write: |home| giant_newest(home, 12, MIB, 5),
        tune: |flags, _| flags.cold_chunk_bytes = Some(64 * KIB),
        produces: &[Named::ItemsMidSequence, Named::FlipsCommitted],
        spans_batches: false,
    });
}

/// Long transcripts (several capture batches each) split into
/// lanes S and B, on a pooled walk with a small look-ahead: both lane heads
/// can be mid-source at once.
#[test]
fn two_lanes_resume_after_a_kill_at_every_named_state() {
    check(&Corpus {
        name: "two-lanes",
        agent: "claude-code",
        ledger: "claude-code",
        write: |home| {
            // Each transcript spans two capture batches (over 1,024
            // records), each a little larger than the one before.
            let projects = claude_root(home).join("projects");
            for index in 0..6_usize {
                let id = format!("long{index}");
                let mut transcript = ClaudeTranscript::new(&id, None, T0);
                transcript.bulk(560 + 40 * index, 48);
                transcript.write(
                    &projects.join(format!("-w-{id}/{id}.jsonl")),
                    T0 + index as i64 * 3_600_000,
                );
            }
        },
        tune: |flags, home| {
            flags.capture_workers = Some(4);
            flags.capture_look_ahead = Some(256 * KIB);
            flags.cold_chunk_bytes = Some(64 * KIB);
            // The median transcript: about half walk the big lane.
            let mut sizes: Vec<u64> = transcripts(&claude_root(home))
                .iter()
                .map(|path| std::fs::metadata(path).unwrap().len())
                .collect();
            sizes.sort_unstable();
            flags.cold_walk.big_source_bytes = Some(sizes[sizes.len() / 2]);
        },
        produces: &[
            Named::LanesMidSource,
            Named::ItemsMidSequence,
            Named::FlipsCommitted,
        ],
        spans_batches: true,
    });
}
