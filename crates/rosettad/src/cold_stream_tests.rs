//! Per-runtime capture start: metadata is planned concurrently but goes
//! public in registry order, every runtime is admitted, the first runtime
//! that cannot be planned (in registry order) fails the start, and a stop
//! before metadata captures nothing.

use std::path::Path;

use rosetta_adapter_antigravity::AntigravityAdapter;
use rosetta_adapter_claude_code::ClaudeCodeAdapter;
use rosetta_adapter_codex::CodexAdapter;
use rosetta_bench_corpus::{Agent, CorpusSpec, generate};
use rosetta_pipeline::{IngestOptions, discover_root};
use rosetta_server::{HubConfig, RuntimeIdentity};

use super::*;

/// One runtime over `root`, identified by `data_root`, the way fresh serve
/// builds it from a discovered or enrolled target (no raw ledger here).
fn request(adapter: &'static dyn AgentAdapter, data_root: &str, root: &Path) -> ColdRuntimeRequest {
    let opts = IngestOptions::default();
    let census = discover_root(adapter, data_root, root, &opts, &Instrument::new()).unwrap();
    assert!(
        census.installed,
        "{} corpus at {}",
        adapter.kind(),
        root.display()
    );
    ColdRuntimeRequest {
        name: format!("{} {data_root}", adapter.kind()),
        capture: ColdCaptureRequest {
            adapter,
            identity: RuntimeIdentity::new(&census.runtime.id, adapter.kind(), None),
            runtime: census.runtime,
            root: root.to_path_buf(),
            inventory: census.inventory,
            discovery_truncated: census.walk_truncated,
            ledger_root: root.join("unused-ledger"),
            opts,
            write_raw_ledger: false,
            async_raw_ledger: false,
            ledger_codec: rosetta_capture::SegmentCodec::Plain,
            instrument: Arc::new(Instrument::new()),
        },
    }
}

fn corpus(root: &Path, agent: Agent, sessions: u64) {
    std::fs::create_dir_all(root).unwrap();
    generate(&CorpusSpec::new(agent, sessions, 20, 5), root).unwrap();
}

fn antigravity_fixture(root: &Path) {
    let fixtures =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../groundtruth/antigravity/fixtures");
    let version = std::fs::read_dir(fixtures)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let scenario = version.join("basic-chat");
    let data = std::fs::read_dir(&scenario)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.is_dir())
        .unwrap();
    copy_tree(&data, root);
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &to.join(entry.file_name()));
        } else {
            std::fs::copy(entry.path(), to.join(entry.file_name())).unwrap();
        }
    }
}

/// A subagent sidecar above the adapter's document limit: its runtime's
/// metadata plan fails with an error naming this path.
fn oversized_sidecar(root: &Path, project: &str) -> String {
    let relative = format!("projects/{project}/session/subagents/agent-big.meta.json");
    let path = root.join(&relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        format!(r#"{{"agentType":"{}"}}"#, "x".repeat(70_000)),
    )
    .unwrap();
    relative
}

struct Built {
    store: Store,
    health: Arc<DaemonHealth>,
    result: Result<Vec<String>, String>,
}

fn build(requests: Vec<ColdRuntimeRequest>, stopped: bool) -> Built {
    let temp = tempfile::TempDir::new().unwrap();
    let flags = crate::parse_flags(&[
        "--home".to_string(),
        temp.path().display().to_string(),
        "--db".to_string(),
        temp.path().join("projection.sqlite").display().to_string(),
    ])
    .unwrap();
    let mut store = Store::open_in_memory_for_rebuild().unwrap();
    store.use_precomputed_bulk_ordinals().unwrap();
    store.observe_meta_effects();
    let hub = Hub::new(HubConfig::default(), 0);
    let health = Arc::new(DaemonHealth::new(0));
    for request in &requests {
        health.ingest_register(&request.capture.identity, 0, 0);
    }
    let board = Arc::new(ProgressBoard::new());
    let stop = AtomicBool::new(stopped);
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
        assert!(failures.is_empty(), "{failures:?}");
        applied
            .into_iter()
            .map(|output| output.runtime.id)
            .collect()
    });
    Built {
        store,
        health,
        result,
    }
}

fn column(store: &Store, sql: &str) -> Vec<String> {
    let mut statement = store.conn().prepare(sql).unwrap();
    statement
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

#[test]
fn metadata_goes_public_in_registry_order_and_every_runtime_is_admitted() {
    let home = tempfile::TempDir::new().unwrap();
    let large = home.path().join("large-claude");
    let codex = home.path().join("codex");
    let antigravity = home.path().join("antigravity");
    let small = home.path().join("small-claude");
    // The first runtime plans longest, so the others finish planning first.
    corpus(&large, Agent::ClaudeCode, 80);
    corpus(&codex, Agent::Codex, 3);
    antigravity_fixture(&antigravity);
    corpus(&small, Agent::ClaudeCode, 1);
    let requests = vec![
        request(&ClaudeCodeAdapter, "~/.claude", &large),
        request(
            &AntigravityAdapter,
            "~/.gemini/antigravity-cli",
            &antigravity,
        ),
        request(&CodexAdapter, "~/.codex", &codex),
        request(&ClaudeCodeAdapter, &small.display().to_string(), &small),
    ];
    let registry: Vec<String> = requests
        .iter()
        .map(|request| request.capture.runtime.id.clone())
        .collect();

    let built = build(requests, false);
    let mut admitted = built.result.unwrap();
    admitted.sort();
    let mut expected = registry.clone();
    expected.sort();
    assert_eq!(admitted, expected, "every runtime is admitted");
    // A runtime row is inserted by its runtime's first metadata batch.
    assert_eq!(
        column(&built.store, "SELECT id FROM agent_runtime ORDER BY rowid"),
        registry
    );
    let owners = column(
        &built.store,
        "SELECT DISTINCT runtime_id FROM session ORDER BY runtime_id",
    );
    assert_eq!(owners, expected, "every runtime's sessions are public");
    let status = built
        .health
        .snapshot(&Hub::new(HubConfig::default(), 0), 1)
        .ingest;
    assert!(status.metadata_ready_at.is_some(), "{status:?}");
    assert!(
        status
            .runtimes
            .iter()
            .all(|runtime| runtime.state == rosetta_server::BackfillState::Done),
        "{status:?}"
    );
}

#[test]
fn the_first_unplannable_runtime_in_registry_order_fails_the_start() {
    let home = tempfile::TempDir::new().unwrap();
    let good = home.path().join("good");
    let first_bad = home.path().join("first-bad");
    let second_bad = home.path().join("second-bad");
    corpus(&good, Agent::ClaudeCode, 30);
    // The earlier failure plans longer, so the later one fails first in time.
    corpus(&first_bad, Agent::ClaudeCode, 60);
    let first = oversized_sidecar(&first_bad, "-w-first");
    corpus(&second_bad, Agent::ClaudeCode, 1);
    let second = oversized_sidecar(&second_bad, "-w-second");
    let requests = vec![
        request(&ClaudeCodeAdapter, &good.display().to_string(), &good),
        request(
            &ClaudeCodeAdapter,
            &first_bad.display().to_string(),
            &first_bad,
        ),
        request(
            &ClaudeCodeAdapter,
            &second_bad.display().to_string(),
            &second_bad,
        ),
    ];

    let built = build(requests, false);
    let error = built.result.unwrap_err();
    assert!(error.contains(&first), "{error}");
    assert!(!error.contains(&second), "{error}");
    assert_eq!(
        built
            .health
            .snapshot(&Hub::new(HubConfig::default(), 0), 1)
            .ingest
            .metadata_ready_at,
        None,
        "a failed metadata plan never completes the first pass"
    );
}

#[test]
fn a_stop_before_metadata_captures_nothing() {
    let home = tempfile::TempDir::new().unwrap();
    let root = home.path().join("claude");
    corpus(&root, Agent::ClaudeCode, 2);
    let built = build(vec![request(&ClaudeCodeAdapter, "~/.claude", &root)], true);
    assert_eq!(
        built.result.unwrap_err(),
        "stopped before cold metadata completed"
    );
    assert!(column(&built.store, "SELECT id FROM session").is_empty());
    assert_eq!(
        built
            .health
            .snapshot(&Hub::new(HubConfig::default(), 0), 1)
            .ingest
            .metadata_ready_at,
        None,
        "an interrupted first pass has no completion stamp"
    );
}
