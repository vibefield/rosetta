//! R1 writer gaps closed end to end through the real writer (`writer.rs`,
//! `flips.rs`, `emit.rs`), the R1 brief §10 fixtures generated in-test:
//!
//! - **D15 at both writer call sites.** A fork copies its origin's
//!   response into another tree; the copy's tree flips first, and the
//!   origin then displaces the copy's usage under a watched subscriber. When
//!   the origin's own transaction displaces it, `writer::turn`'s step
//!   invalidates the copy; when the origin is crossed, its usage waits for
//!   its tree's flip fold, and `flips::flip`'s batch invalidates the copy.
//!   Either way the copy's subscriber gets a second resync barrier right
//!   after the displacing commit. (Mutations: dropping either call site's
//!   invalidation leaves one barrier, and its test fails.)
//! - **A crossed session's chunked source in lane B while lane S commits**
//!   (T6 review finding 1). Lane S terminals commit between the big-lane
//!   item's Stage slices, whose staged fold deltas stay open; the build ends
//!   where the runtime admitted whole ends. (Mutation: checking the
//!   runtime-wide `staged_slices() == 0` after every terminal fails the
//!   build.)
//! - **ProducerEmit hold and retry under the real `LaneBudgets`.** With the
//!   lane split forced low and the class limits scaled to a few KiB, lanes
//!   hold their emissions and the sequencers retry them; every held
//!   emission is sent, and the pooled build ends where the serial driver's
//!   blocking emission (`--capture-workers 0`) and the runtime admitted
//!   whole end. (Mutations: a retry that drops its emission, or sends it
//!   without acquiring, fails.)

use std::sync::Mutex;

use rosetta_core::ids;
use rosetta_server::{HubItem, Subscription};
use rosetta_store::CommitKind;

use super::super::emit::{HoldCounts, TEST_BUDGETS, TestBudgets};
use super::*;
use crate::engine::now_ms;
use crate::test_support::crossing::{CROSSING, CROSSING_SPLIT, crossed_big_lane, fork_crossed};
use crate::test_support::fixtures::{claude_fork, claude_root};

const KIB: u64 = 1024;

/// One fresh streamed build through the real writer, into `store`, with
/// `hub` the read side's hub.
fn run(
    requests: Vec<ColdRuntimeRequest>,
    extra: &[&str],
    tune: impl FnOnce(&mut crate::Flags),
    store: &mut Store,
    hub: &Hub,
) -> (Result<Vec<String>, String>, Vec<String>) {
    let temp = tempfile::TempDir::new().unwrap();
    let mut args = vec![
        "--home".to_string(),
        temp.path().display().to_string(),
        "--db".to_string(),
        temp.path().join("projection.sqlite").display().to_string(),
    ];
    args.extend(extra.iter().map(|arg| (*arg).to_string()));
    let mut flags = crate::parse_flags(&args).unwrap();
    tune(&mut flags);
    flags.cold_walk.probe_counters = Some(Arc::new(rosetta_pipeline::ProbeCounters::default()));
    let health = Arc::new(DaemonHealth::new(0));
    for request in &requests {
        health.ingest_register(&request.capture.identity, 0, 0);
    }
    let board = Arc::new(ProgressBoard::new());
    let stop = AtomicBool::new(false);
    let instrument = Instrument::new();
    let mut failures = Vec::new();
    let result = build_streamed(
        requests,
        StreamedCold {
            flags: &flags,
            store,
            instrument: &instrument,
            hub,
            health: &health,
            board: &board,
            stop: &stop,
            started: Instant::now(),
        },
        &mut failures,
    )
    .map(|applied| {
        applied
            .into_iter()
            .map(|output| output.runtime.id)
            .collect()
    });
    (result, failures)
}

fn cold_store() -> Store {
    let mut store = Store::open_in_memory_for_rebuild().unwrap();
    store.use_precomputed_bulk_ordinals().unwrap();
    store.observe_meta_effects();
    store
}

/// What the watched copy's subscriber saw, in commit order.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Seen {
    Commit(CommitKind),
    /// A resync barrier raised since the previous commit.
    Barrier,
}

/// A watched session's subscriber and what it saw.
struct Watched {
    hub: Arc<Hub>,
    subscription: Arc<Subscription>,
    log: Arc<Mutex<Vec<Seen>>>,
}

impl Watched {
    /// Record the barriers received since the last drain, resyncing each
    /// (as a client's replacement snapshot does), so none hides the next.
    fn drain(&self) {
        while let Some(item) = self.subscription.try_recv() {
            if matches!(item, HubItem::Resync) {
                self.log.lock().unwrap().push(Seen::Barrier);
                self.hub.resync(&self.subscription, now_ms());
            }
        }
    }
}

/// Watch `session`; after every writer commit, record the barriers its
/// subscriber received since the previous one, then the commit.
fn watch(store: &mut Store, hub: &Arc<Hub>, session: &str) -> Arc<Watched> {
    let (subscription, _) = hub.subscribe_session(session, now_ms());
    let watched = Arc::new(Watched {
        hub: Arc::clone(hub),
        subscription: Arc::new(subscription),
        log: Arc::default(),
    });
    let observer = Arc::clone(&watched);
    store.set_commit_observer(Some(Box::new(move |kind| {
        observer.drain();
        observer.log.lock().unwrap().push(Seen::Commit(kind));
    })));
    watched
}

/// The commit kinds each barrier followed.
fn barriers_after(log: &[Seen]) -> Vec<CommitKind> {
    let mut last = None;
    let mut out = Vec::new();
    for seen in log {
        match seen {
            Seen::Commit(kind) => last = Some(*kind),
            Seen::Barrier => out.push(last.expect("a barrier follows a commit")),
        }
    }
    out
}

/// The copy's response row: its usage columns, as readers see them.
fn copy_usage(store: &Store, session: &str) -> Vec<(Option<i64>, Option<i64>)> {
    let mut statement = store
        .conn()
        .prepare(
            "SELECT in_tokens, out_tokens FROM message
             WHERE session_id = ?1 AND content LIKE '%here is the plan%'",
        )
        .unwrap();
    statement
        .query_map([session], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

/// Build the fork corpus with the copy watched; returns the barriers'
/// preceding commit kinds and the store.
fn fork_build(adapter: &'static dyn AgentAdapter, fixture: fn(&Path)) -> (Vec<CommitKind>, Store) {
    let home = tempfile::TempDir::new().unwrap();
    fixture(home.path());
    let root = claude_root(home.path());
    let request = request(adapter, &root, IngestOptions::default());
    let fork = ids::session_id(&request.capture.runtime.id, "fork");
    let hub = Arc::new(Hub::new(HubConfig::default(), 0));
    let mut store = cold_store();
    let watched = watch(&mut store, &hub, &fork);
    // Every source over 512 bytes is its own item: the fork's and the
    // origin's never share a transaction, so the fork's tree flips between.
    let (result, failures) = run(
        vec![request],
        &["--capture-workers", "0"],
        |flags| flags.cold_chunk_bytes = Some(512),
        &mut store,
        &hub,
    );
    store.set_commit_observer(None);
    // Barriers raised after the last commit.
    watched.drain();
    assert!(failures.is_empty(), "{failures:?}");
    result.unwrap();
    let log = watched.log.lock().unwrap().clone();
    let states = states(&store);
    assert_eq!(states[&fork], "indexed", "{states:?}");
    // The copy's usage was displaced: its row reads as a dropped copy.
    assert_eq!(copy_usage(&store, &fork), vec![(None, None)]);
    (barriers_after(&log), store)
}

/// D15 at `writer::turn`: the origin's own transaction displaces the
/// flipped copy's usage, and that step invalidates the copy.
#[test]
fn a_step_that_displaces_a_flipped_copy_invalidates_it() {
    let (barriers, _) = fork_build(&ClaudeCodeAdapter, claude_fork);
    assert_eq!(
        barriers.len(),
        2,
        "the flip's barrier, then D15's: {barriers:?}"
    );
    assert_eq!(barriers[0], CommitKind::Admission, "{barriers:?}");
    assert!(
        matches!(
            barriers[1],
            CommitKind::Sources | CommitKind::StageSlice | CommitKind::ResolvedSlice
        ),
        "a step displaced the copy: {barriers:?}"
    );
}

/// D15 at `flips::flip`: the crossed origin's usage lands in its tree's
/// flip fold, and that batch invalidates the copy flipped before it.
#[test]
fn a_flip_fold_that_displaces_a_flipped_copy_invalidates_it() {
    let (barriers, _) = fork_build(&CROSSING, fork_crossed);
    assert_eq!(
        barriers,
        vec![CommitKind::Admission, CommitKind::Admission],
        "the copy's flip, then the origin's flip that displaced it"
    );
}

/// T6 review finding 1 through the real writer: a crossed session's source
/// over the chunk bound walks lane B, its Stage slices staging fold deltas,
/// while lane S's terminals commit between them.
#[test]
fn lane_s_terminals_commit_between_a_crossed_big_lane_items_slices() {
    let home = tempfile::TempDir::new().unwrap();
    crossed_big_lane(home.path(), 1024 * 1024, 24);
    let root = claude_root(home.path());
    let whole = build(
        vec![request(&CROSSING, &root, IngestOptions::default())],
        &["--experiment-cold-admission", "runtime"],
        None,
    );
    whole.result.unwrap();
    BEGUN_BESIDE_ITEM.with(|count| count.set(0));
    let mut store = cold_store();
    let hub = Hub::new(HubConfig::default(), 0);
    let slices: Arc<Mutex<BTreeMap<String, u64>>> = Arc::default();
    {
        let slices = Arc::clone(&slices);
        store.set_commit_observer(Some(Box::new(move |kind| {
            *slices
                .lock()
                .unwrap()
                .entry(format!("{kind:?}"))
                .or_default() += 1;
        })));
    }
    let (result, failures) = run(
        vec![request(&CROSSING, &root, IngestOptions::default())],
        // The serial driver walks the global order: the newest tree's giant
        // first, then lane S's trees, emitted while the writer steps it.
        &["--capture-workers", "0"],
        |flags| {
            flags.cold_chunk_bytes = Some(64 * KIB);
            flags.cold_walk.big_source_bytes = Some(CROSSING_SPLIT);
        },
        &mut store,
        &hub,
    );
    store.set_commit_observer(None);
    assert!(failures.is_empty(), "{failures:?}");
    result.unwrap();
    let slices = slices.lock().unwrap().clone();
    assert!(
        slices.get("StageSlice").copied().unwrap_or(0) >= 8,
        "the big-lane source applied in slices: {slices:?}"
    );
    assert!(
        BEGUN_BESIDE_ITEM.with(|count| count.get()) > 0,
        "lane S items began between the big-lane item's slices"
    );
    assert_eq!(states(&store), states(&whole.store));
    assert_eq!(rows(&store), rows(&whole.store));
}

/// One forced-split build with the budget classes scaled down: what its
/// producers held and retried.
fn held_build(root: &Path, workers: &str, split: u64) -> (Built, Arc<HoldCounts>) {
    let counts = Arc::new(HoldCounts::default());
    TEST_BUDGETS.with(|test| {
        *test.borrow_mut() = Some(TestBudgets {
            small: 24 * KIB,
            shared: 48 * KIB,
            counts: Arc::clone(&counts),
        });
    });
    let built = build_tuned(
        vec![request(&ClaudeCodeAdapter, root, IngestOptions::default())],
        &["--capture-workers", workers],
        Some(8 * KIB),
        |flags| flags.cold_walk.big_source_bytes = Some(split),
    );
    TEST_BUDGETS.with(|test| test.borrow_mut().take());
    (built, counts)
}

/// ProducerEmit's hold and retry with the real per-class budgets, on a
/// pooled recency walk with lanes S and B: lanes hold and every held
/// emission is retried and sent; the build ends where the serial driver's
/// blocking emission and the runtime admitted whole end.
#[test]
fn held_emissions_are_retried_and_reach_the_serial_end_state() {
    let home = tempfile::TempDir::new().unwrap();
    let root = home.path().join("claude");
    corpus(&root, 32, 400, 47);
    let mut sizes: Vec<u64> = transcripts(&root)
        .iter()
        .map(|path| std::fs::metadata(path).unwrap().len())
        .collect();
    sizes.sort_unstable();
    let split = sizes[sizes.len() * 2 / 3];
    let whole = build(
        vec![request(&ClaudeCodeAdapter, &root, IngestOptions::default())],
        &["--experiment-cold-admission", "runtime"],
        None,
    );
    whole.result.unwrap();
    let (serial, blocking) = held_build(&root, "0", split);
    assert!(serial.failures.is_empty(), "{:?}", serial.failures);
    serial.result.unwrap();
    assert_eq!(
        blocking.held.load(Ordering::SeqCst),
        0,
        "the serial driver's emission blocks, never holds"
    );
    let (pooled, counts) = held_build(&root, "3", split);
    assert!(pooled.failures.is_empty(), "{:?}", pooled.failures);
    pooled.result.unwrap();
    let held = counts.held.load(Ordering::SeqCst);
    assert!(held > 0, "lanes held emissions on full classes");
    assert_eq!(
        counts.retried.load(Ordering::SeqCst),
        held,
        "every held emission was retried and sent"
    );
    assert!(pooled.probe.flips_committed.load(Ordering::SeqCst) > 0);
    assert_eq!(states(&pooled.store), states(&serial.store));
    assert_eq!(rows(&pooled.store), rows(&serial.store));
    assert_eq!(rows(&pooled.store), rows(&whole.store));
    assert_eq!(states(&pooled.store), states(&whole.store));
}
