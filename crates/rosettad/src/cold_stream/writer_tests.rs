//! T6 writer tests over a whole streamed build (fixtures generated in-test,
//! never a home directory): per-tree flips reach the end state of a runtime
//! admitted whole, a refused tree never flips while earlier flips stay
//! readable (test 11), a writer failure leaves no producer blocked (test
//! 14), and a parked `Closed` waits for its item's terminal (test 15).

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use rosetta_adapter_claude_code::ClaudeCodeAdapter;
use rosetta_bench_corpus::{Agent, CorpusSpec, generate};
use rosetta_pipeline::{IngestOptions, discover_root};
use rosetta_server::{HubConfig, RuntimeIdentity};

use super::emit::BudgetClass;
use super::writer::{BEGUN_BESIDE_ITEM, FAIL_AT_STEP, PARKED_BEHIND_ITEM};
use super::*;

fn request(
    adapter: &'static dyn AgentAdapter,
    root: &Path,
    opts: IngestOptions,
) -> ColdRuntimeRequest {
    let census = discover_root(
        adapter,
        &root.display().to_string(),
        root,
        &opts,
        &Instrument::new(),
    )
    .unwrap();
    assert!(census.installed, "corpus at {}", root.display());
    ColdRuntimeRequest {
        name: format!("{} {}", adapter.kind(), root.display()),
        capture: ColdCaptureRequest {
            adapter,
            identity: RuntimeIdentity::new(&census.runtime.id, adapter.kind(), None),
            runtime: census.runtime,
            root: root.to_path_buf(),
            inventory: census.inventory,
            discovery_truncated: census.walk_truncated,
            admission: adapter.cold_admission(),
            ledger_root: root.join("unused-ledger"),
            opts,
            write_raw_ledger: false,
            async_raw_ledger: false,
            ledger_codec: rosetta_capture::SegmentCodec::Plain,
            instrument: Arc::new(Instrument::new()),
        },
    }
}

fn corpus(root: &Path, sessions: u64, lines: u64, seed: u64) {
    std::fs::create_dir_all(root).unwrap();
    generate(
        &CorpusSpec::new(Agent::ClaudeCode, sessions, lines, seed),
        root,
    )
    .unwrap();
}

struct Built {
    store: Store,
    result: Result<Vec<String>, String>,
    failures: Vec<String>,
    board: Arc<ProgressBoard>,
    probe: Arc<rosetta_pipeline::ProbeCounters>,
}

/// One fresh streamed build of `requests` under `flags` (plus `extra`).
fn build(requests: Vec<ColdRuntimeRequest>, extra: &[&str], chunk: Option<u64>) -> Built {
    build_tuned(requests, extra, chunk, |_| {})
}

/// [`build`], with the parsed flags adjusted by `tune` (hidden test knobs).
fn build_tuned(
    requests: Vec<ColdRuntimeRequest>,
    extra: &[&str],
    chunk: Option<u64>,
    tune: impl FnOnce(&mut crate::Flags),
) -> Built {
    let temp = tempfile::TempDir::new().unwrap();
    let mut args = vec![
        "--home".to_string(),
        temp.path().display().to_string(),
        "--db".to_string(),
        temp.path().join("projection.sqlite").display().to_string(),
    ];
    args.extend(extra.iter().map(|arg| (*arg).to_string()));
    let mut flags = crate::parse_flags(&args).unwrap();
    flags.cold_chunk_bytes = chunk.or(flags.cold_chunk_bytes);
    tune(&mut flags);
    let probe = Arc::new(rosetta_pipeline::ProbeCounters::default());
    flags.cold_walk.probe_counters = Some(Arc::clone(&probe));
    let mut store = Store::open_in_memory_for_rebuild().unwrap();
    store.use_precomputed_bulk_ordinals().unwrap();
    store.observe_meta_effects();
    let hub = Hub::new(HubConfig::default(), 0);
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
            store: &mut store,
            instrument: &instrument,
            hub: &hub,
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
    Built {
        store,
        result,
        failures,
        board,
        probe,
    }
}

fn states(store: &Store) -> BTreeMap<String, String> {
    let mut statement = store
        .conn()
        .prepare("SELECT id, stream_state FROM session")
        .unwrap();
    statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

/// Every message and tool row, by timeline id, with what readers see.
fn rows(store: &Store) -> Vec<String> {
    let mut statement = store
        .conn()
        .prepare(
            "SELECT timeline_id, session_id, ordinal, type, content, tool_status, tool_detail,
                    in_tokens, out_tokens
             FROM message ORDER BY timeline_id",
        )
        .unwrap();
    statement
        .query_map([], |row| {
            let mut fields = Vec::new();
            for index in 0..9 {
                let value: rusqlite::types::Value = row.get(index)?;
                fields.push(format!("{value:?}"));
            }
            Ok(fields.join("|"))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn transcripts(root: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|ext| ext == "jsonl") {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    walk(root, &mut out);
    out.sort();
    out
}

/// T6 test 8 at the writer: a per-tree build with every source chunked
/// (64 KiB) flips its trees as they close, and ends exactly where the
/// runtime admitted whole (the kill switch) ends; the board counts every
/// session done.
#[test]
fn per_tree_flips_reach_the_runtime_admission_end_state() {
    let home = tempfile::TempDir::new().unwrap();
    let root = home.path().join("claude");
    corpus(&root, 8, 400, 11);
    let flipped = build(
        vec![request(&ClaudeCodeAdapter, &root, IngestOptions::default())],
        &[],
        Some(64 * 1024),
    );
    assert!(flipped.failures.is_empty(), "{:?}", flipped.failures);
    flipped.result.unwrap();
    assert!(
        flipped.probe.flips_committed.load(Ordering::SeqCst) > 0,
        "trees flipped before the runtime closed"
    );
    let whole = build(
        vec![request(&ClaudeCodeAdapter, &root, IngestOptions::default())],
        &["--experiment-cold-admission", "runtime"],
        None,
    );
    whole.result.unwrap();
    assert_eq!(states(&flipped.store), states(&whole.store));
    assert_eq!(rows(&flipped.store), rows(&whole.store));
    assert!(
        states(&flipped.store)
            .values()
            .all(|state| state != "pending"),
        "{:?}",
        states(&flipped.store)
    );
    let done: u64 = flipped
        .board
        .snapshot()
        .iter()
        .map(|project| project.sessions_done)
        .sum();
    let total: u64 = flipped
        .board
        .snapshot()
        .iter()
        .map(|project| project.sessions_total)
        .sum();
    assert_eq!(done, total, "N ready reaches every session");
}

/// T6 test 11: an oversized record in the oldest tree abandons its source:
/// that tree never flips, the trees that closed before stay readable, the
/// rest of the runtime stays Pending, and publication is refused.
#[test]
fn a_refused_tree_never_flips_and_earlier_flips_stay_readable() {
    let home = tempfile::TempDir::new().unwrap();
    let root = home.path().join("claude");
    corpus(&root, 6, 60, 23);
    let files = transcripts(&root);
    assert!(files.len() >= 4, "{files:?}");
    // The oldest tree: walked last, after every other tree has flipped.
    let refused = &files[0];
    let mut text = std::fs::read_to_string(refused).unwrap();
    let native = text
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find_map(|record| record["sessionId"].as_str().map(str::to_owned))
        .unwrap();
    text.push_str(&format!(
        "{{\"type\":\"user\",\"sessionId\":\"{native}\",\"uuid\":\"huge\",\"message\":{{\"role\":\"user\",\"content\":\"{}\"}}}}\n",
        "z".repeat(200_000)
    ));
    std::fs::write(refused, text).unwrap();
    let old = std::time::SystemTime::now() - Duration::from_secs(30 * 86_400);
    std::fs::File::options()
        .write(true)
        .open(refused)
        .unwrap()
        .set_modified(old)
        .unwrap();
    let mut opts = IngestOptions::default();
    opts.tail.max_record_bytes = 64 * 1024;
    let built = build(
        vec![request(&ClaudeCodeAdapter, &root, opts)],
        &["--capture-workers", "0"],
        None,
    );
    let applied = built.result.unwrap();
    assert!(applied.is_empty(), "a refused runtime is not admitted");
    assert!(
        built
            .failures
            .iter()
            .any(|failure| failure.contains("oversized")),
        "{:?}",
        built.failures
    );
    let states = states(&built.store);
    let name = refused.file_name().unwrap().to_string_lossy().into_owned();
    let session: String = built
        .store
        .conn()
        .query_row(
            "SELECT session_id FROM ingest_cursor WHERE path LIKE '%' || ?1",
            [&name],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        states[&session], "pending",
        "the refused tree never flips: {states:?}"
    );
    let readable = states.values().filter(|state| *state != "pending").count();
    assert!(readable > 0, "earlier flips stay readable: {states:?}");
    assert!(built.probe.flips_committed.load(Ordering::SeqCst) > 0);
}

/// T6 test 14: the writer fails while one producer is blocked in a shared
/// acquire and another in a small-class one: the build's abort wakes both
/// within 5 s. (Mutation: aborting only the shared class leaves the small
/// one blocked, and this test times out.)
#[test]
fn a_writer_failure_leaves_no_producer_blocked() {
    let pool = CapturePool::new(0, PoolBudgets::DEFAULT);
    let budgets = Arc::new(LaneBudgets::new(true, None));
    let abandoned = AtomicBool::new(false);
    let (done, finished) = mpsc::channel();
    for class in [BudgetClass::Shared, BudgetClass::Small] {
        // Fill the class, then wait for more than it holds.
        budgets.budget(class).acquire(1, budgets.ticket()).unwrap();
        let (budgets, done) = (Arc::clone(&budgets), done.clone());
        std::thread::spawn(move || {
            let result = budgets
                .budget(class)
                .acquire(u64::MAX / 2, budgets.ticket());
            let _ = done.send((class, result.is_err()));
        });
    }
    std::thread::sleep(Duration::from_millis(50));
    abort_build(&abandoned, &budgets, &pool);
    for _ in 0..2 {
        let (class, failed) = finished
            .recv_timeout(Duration::from_secs(5))
            .expect("every blocked producer wakes within 5 s");
        assert!(failed, "{class:?} acquire fails once the writer is gone");
    }
    assert!(abandoned.load(Ordering::SeqCst));

    // End to end: a writer failure mid-build returns an error, promptly.
    let home = tempfile::TempDir::new().unwrap();
    let root = home.path().join("claude");
    corpus(&root, 6, 200, 5);
    FAIL_AT_STEP.with(|fail| fail.set(Some(3)));
    let started = Instant::now();
    let built = build(
        vec![request(&ClaudeCodeAdapter, &root, IngestOptions::default())],
        &[],
        Some(64 * 1024),
    );
    FAIL_AT_STEP.with(|fail| fail.set(None));
    let error = built.result.unwrap_err();
    assert!(error.contains("injected writer failure"), "{error}");
    assert!(started.elapsed() < Duration::from_secs(5));
}

/// T6 test 15: a runtime's `Closed` arrives while its last chunked item is
/// between slices: the remainder admission waits for the item's terminal
/// (and the flips before it), so the build admits cleanly. (Mutation:
/// admitting `Closed` once the queues are empty, ignoring the item in
/// progress, trips the admission's no-item-in-progress check.)
#[test]
fn a_parked_close_waits_for_its_item_in_progress() {
    let home = tempfile::TempDir::new().unwrap();
    let root = home.path().join("claude");
    corpus(&root, 4, 2_000, 9);
    PARKED_BEHIND_ITEM.with(|count| count.set(0));
    let built = build(
        vec![request(&ClaudeCodeAdapter, &root, IngestOptions::default())],
        &["--capture-workers", "0"],
        Some(16 * 1024),
    );
    assert!(built.failures.is_empty(), "{:?}", built.failures);
    built.result.unwrap();
    assert!(
        PARKED_BEHIND_ITEM.with(|count| count.get()) > 0,
        "the close was parked behind an item between its slices"
    );
    assert!(
        states(&built.store)
            .values()
            .all(|state| state != "pending")
    );
}

/// Stress: many trees, the lane split forced low (hidden
/// `ColdWalkOptions.big_source_bytes`) so the larger trees walk the big
/// lane beside lane S, and every source over 4 KiB applied in slices — on
/// the pooled walk (non-blocking emission, per-class budgets) and on the
/// serial driver. Lane S items begin while a big-lane item is between its
/// slices, every tree flips, both builds end exactly where the runtime
/// admitted whole ends, and every budget class drains (the writer
/// debug-asserts it at the last close).
#[test]
fn a_forced_lane_split_with_tiny_chunks_reaches_the_runtime_admission_end_state() {
    let home = tempfile::TempDir::new().unwrap();
    let root = home.path().join("claude");
    corpus(&root, 48, 400, 31);
    let mut sizes: Vec<u64> = transcripts(&root)
        .iter()
        .map(|path| std::fs::metadata(path).unwrap().len())
        .collect();
    sizes.sort_unstable();
    // About a third of the trees walk the big lane.
    let split = sizes[sizes.len() * 2 / 3];
    let whole = build(
        vec![request(&ClaudeCodeAdapter, &root, IngestOptions::default())],
        &["--experiment-cold-admission", "runtime"],
        None,
    );
    whole.result.unwrap();
    for workers in ["3", "0"] {
        BEGUN_BESIDE_ITEM.with(|count| count.set(0));
        let built = build_tuned(
            vec![request(&ClaudeCodeAdapter, &root, IngestOptions::default())],
            &["--capture-workers", workers],
            Some(4 * 1024),
            |flags| flags.cold_walk.big_source_bytes = Some(split),
        );
        assert!(built.failures.is_empty(), "{:?}", built.failures);
        built.result.unwrap();
        assert!(
            built.probe.flips_committed.load(Ordering::SeqCst) > 0,
            "workers {workers}: trees flipped before the close"
        );
        assert_eq!(
            states(&built.store),
            states(&whole.store),
            "workers {workers}"
        );
        assert_eq!(rows(&built.store), rows(&whole.store), "workers {workers}");
        if workers != "0" {
            assert!(
                BEGUN_BESIDE_ITEM.with(|count| count.get()) > 0,
                "lanes interleaved between a chunked item's slices"
            );
        }
    }
}

#[path = "writer_e2e_tests.rs"]
mod e2e;
