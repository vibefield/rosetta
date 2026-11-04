//! Readers hammer every payload read route — REST and the WS session and
//! collaboration snapshots — while a fresh build fills the shared-cache RAM
//! generation, through admission and finalization. Under the shared-cache
//! lock discipline (W1/W2/R1) no read and no writer transaction is ever
//! refused a lock, and the readers leave no trace in the final projection:
//! it equals the same build's private projection, row for row.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::StreamExt;
use rosetta_adapter_claude_code::ClaudeCodeAdapter;
use rosetta_adapter_codex::CodexAdapter;
use rosetta_adapter_grok::GrokAdapter;
use rosetta_adapter_support::AgentAdapter;
use rosetta_pipeline::{
    BlockingEmit, ColdWalkOptions, IngestOptions, Instrument, StreamedPreparer, StreamedRuntime,
    discover, expand_data_root, meta_ingest_inventory_into, stream_closed_sources_into,
    streamed_setup,
};
use rosetta_store::Store;
use rusqlite::types::Value;
use tempfile::TempDir;

const READERS: usize = 8;

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    let mut entries = std::fs::read_dir(from)
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let source = entry.path();
        let destination = to.join(entry.file_name());
        if source.is_dir() {
            copy_tree(&source, &destination);
        } else {
            std::fs::copy(source, destination).unwrap();
        }
    }
}

/// One recorded fixture scenario's data root.
fn fixture_root(kind: &str, scenario: &str) -> PathBuf {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../groundtruth")
        .join(kind)
        .join("fixtures");
    let mut versions = std::fs::read_dir(&fixtures)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    versions.sort();
    let scenario = versions
        .into_iter()
        .map(|version| version.join(scenario))
        .find(|path| path.is_dir())
        .unwrap();
    std::fs::read_dir(&scenario)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.is_dir())
        .unwrap()
}

/// Generated Claude Code and Codex histories plus Grok's tool-use fixture
/// (which carries a plan), in one home.
fn home() -> TempDir {
    use rosetta_bench_corpus::{Agent, CorpusSpec, generate};
    let home = TempDir::new().unwrap();
    for (adapter, agent, seed) in [
        (
            &ClaudeCodeAdapter as &dyn AgentAdapter,
            Agent::ClaudeCode,
            41,
        ),
        (&CodexAdapter, Agent::Codex, 43),
    ] {
        let root = expand_data_root(&adapter.layout().data_root, home.path());
        std::fs::create_dir_all(&root).unwrap();
        generate(&CorpusSpec::new(agent, 24, 6_000, seed), &root).unwrap();
    }
    copy_tree(
        &fixture_root("grok", "tool-use"),
        &expand_data_root(&GrokAdapter.layout().data_root, home.path()),
    );
    home
}

fn adapters() -> [&'static dyn AgentAdapter; 3] {
    [&ClaudeCodeAdapter, &CodexAdapter, &GrokAdapter]
}

/// Fresh serve's writer steps for one runtime: discover, skim and publish
/// metadata, stream closed sources into the canonical store as they close,
/// and admit the runtime at its producer's close.
fn ingest(adapter: &'static dyn AgentAdapter, home: &Path, canonical: &mut Store) {
    let opts = IngestOptions::default();
    let instrument = Instrument::new();
    let discovery = discover(&[adapter], home, &opts, &instrument)
        .unwrap()
        .pop()
        .unwrap();
    let mut runtime = discovery.runtime;
    runtime.enabled = true;
    let mut planner = Store::open_in_memory_for_cold_planning().unwrap();
    let (_, files) = meta_ingest_inventory_into(
        adapter,
        &runtime,
        &discovery.root,
        &mut planner,
        &discovery.inventory,
        discovery.walk_truncated,
        &opts,
        &instrument,
        None,
    )
    .unwrap();
    for batch in planner.planned_metadata_batches().unwrap() {
        canonical.apply(&batch).unwrap();
    }
    let setup = streamed_setup(&files, &mut planner).unwrap();
    let mut writer =
        StreamedRuntime::open(adapter, &runtime, setup, canonical, &instrument).unwrap();
    let mut preparer = StreamedPreparer::new(&files);
    stream_closed_sources_into(
        adapter,
        &runtime,
        &discovery.root,
        &mut planner,
        &files,
        &opts,
        &instrument,
        None,
        None,
        &ColdWalkOptions::default(),
        &mut BlockingEmit(|_, mut sources| {
            preparer.prepare(&mut sources)?;
            writer.apply(canonical, sources, &instrument).map(|_| ())
        }),
    )
    .unwrap();
    let prepared = preparer.finish().unwrap();
    writer.admit(canonical, &instrument, &prepared).unwrap();
}

fn build(home: &Path, store: &mut Store) {
    store.use_precomputed_bulk_ordinals().unwrap();
    for adapter in adapters() {
        ingest(adapter, home, store);
    }
    store.flush_bulk_reconciliation().unwrap();
    store.finish_bulk_load().unwrap();
}

/// Every canonical table with surrogate ids mapped to stable identities,
/// as the streamed-cold equivalence tests compare projections.
fn dump(store: &Store) -> BTreeMap<String, Vec<String>> {
    let conn = store.conn();
    let cursors: BTreeMap<i64, String> = conn
        .prepare("SELECT id, runtime_id, path FROM ingest_cursor")
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                format!("{}|{}", row.get::<_, String>(1)?, row.get::<_, String>(2)?),
            ))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let messages: BTreeMap<i64, String> = conn
        .prepare("SELECT id, timeline_id FROM message")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let tables: Vec<String> = conn
        .prepare(
            "SELECT name FROM sqlite_master
             WHERE type = 'table' AND name NOT LIKE 'sqlite_%' AND name != 'source_audit'
             ORDER BY name",
        )
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let mut out = BTreeMap::new();
    for table in tables {
        let columns: Vec<String> = conn
            .prepare(&format!("PRAGMA table_info(\"{table}\")"))
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
            .into_iter()
            .filter(|column| {
                !((table == "message" || table == "ingest_cursor") && column == "id")
                    && !(table == "projection_state" && column == "projection_epoch")
                    && !(table == "agent_runtime" && column == "discovered_at")
            })
            .collect();
        let select = columns
            .iter()
            .map(|column| format!("\"{column}\""))
            .collect::<Vec<_>>()
            .join(",");
        let mut rows = conn
            .prepare(&format!("SELECT {select} FROM \"{table}\""))
            .unwrap()
            .query_map([], |row| {
                let mut fields = Vec::with_capacity(columns.len());
                for (index, column) in columns.iter().enumerate() {
                    let value: Value = row.get(index)?;
                    let text = match (&value, column.as_str()) {
                        (Value::Integer(id), name) if name.ends_with("cursor") => {
                            cursors.get(id).cloned().unwrap_or_default()
                        }
                        (Value::Integer(id), "message_id") => {
                            messages.get(id).cloned().unwrap_or_default()
                        }
                        _ => format!("{value:?}"),
                    };
                    fields.push(format!("{column}={text}"));
                }
                Ok(fields.join(" | "))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        rows.sort();
        out.insert(table, rows);
    }
    out
}

fn column_pairs(store: &Store, sql: &str) -> Vec<(String, String)> {
    store
        .conn()
        .prepare(sql)
        .unwrap()
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Value>(1)?))
        })
        .unwrap()
        .map(|row| {
            let (left, right) = row.unwrap();
            let right = match right {
                Value::Text(text) => text,
                Value::Integer(number) => number.to_string(),
                other => format!("{other:?}"),
            };
            (left, right)
        })
        .collect()
}

/// Every payload read route, over the ids the finished build holds.
fn routes(reference: &Store) -> (Vec<String>, Vec<String>) {
    let mut http = vec![
        "/api/artifacts".to_string(),
        "/api/activity".to_string(),
        "/api/sessions".to_string(),
        "/api/projects".to_string(),
        "/api/pricing".to_string(),
    ];
    let mut sockets = Vec::new();
    let sessions = column_pairs(reference, "SELECT id, runtime_id FROM session ORDER BY id");
    for (session, _) in &sessions {
        for suffix in ["", "/timeline", "/collaboration"] {
            http.push(format!("/api/sessions/{session}{suffix}"));
        }
        sockets.push(format!("/ws/sessions/{session}"));
        sockets.push(format!("/ws/collaboration/{session}"));
    }
    // One row of each kind per session: plain turns and tools.
    for (session, timeline) in column_pairs(
        reference,
        "SELECT session_id, MIN(timeline_id) FROM message GROUP BY session_id, type",
    ) {
        for suffix in ["", "/command", "/work", "/plan", "/read/0"] {
            http.push(format!(
                "/api/sessions/{session}/timeline/{timeline}{suffix}"
            ));
        }
    }
    for (session, revision) in column_pairs(reference, "SELECT session_id, revision FROM plan") {
        http.push(format!(
            "/api/sessions/{session}/plans/revisions/{revision}"
        ));
    }
    for (session, publication) in
        column_pairs(reference, "SELECT session_id, id FROM html_artifact")
    {
        http.push(format!(
            "/api/sessions/{session}/artifacts/{publication}/content"
        ));
    }
    for (project, _) in column_pairs(reference, "SELECT id, name FROM project") {
        for suffix in [
            "heatmap",
            "usage",
            "usage/day?day=2026-01-01",
            "contribution",
        ] {
            http.push(format!("/api/projects/{project}/{suffix}"));
        }
    }
    (http, sockets)
}

async fn get(router: &axum::Router, uri: &str) -> (u16, serde_json::Value) {
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    let response = router
        .clone()
        .oneshot(
            axum::http::Request::get(uri)
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status().as_u16();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, body)
}

/// A socket must greet, then deliver its snapshot; a snapshot read that
/// fails answers an error frame instead (as it honestly does for a session
/// whose metadata is not public yet).
async fn snapshot(addr: std::net::SocketAddr, path: &str) -> Result<(), String> {
    use tokio_tungstenite::tungstenite::Message;
    let (mut socket, _) = tokio_tungstenite::connect_async(format!("ws://{addr}{path}"))
        .await
        .map_err(|error| format!("{path}: {error}"))?;
    let mut frames = Vec::new();
    while frames.len() < 2 {
        let message = tokio::time::timeout(Duration::from_secs(20), socket.next())
            .await
            .map_err(|_| format!("{path}: no snapshot within 20 s"))?;
        match message {
            Some(Ok(Message::Text(text))) => {
                let frame: serde_json::Value = serde_json::from_str(text.as_str()).unwrap();
                frames.push(frame["frame"].as_str().unwrap_or_default().to_string());
            }
            Some(Ok(_)) => {}
            other => return Err(format!("{path}: stream ended after {frames:?}: {other:?}")),
        }
    }
    let _ = socket.close(None).await;
    if frames[0] == "hello" && frames[1].ends_with("_snapshot") {
        Ok(())
    } else {
        Err(format!("{path}: frames {frames:?}"))
    }
}

/// Once every runtime is admitted and finalized, every session exists and
/// every snapshot must succeed; before, a refused read is told apart from
/// a missing session by the lock counters alone.
fn settled_failure(settled: &AtomicBool, error: String) -> Option<String> {
    let unpublished = error.ends_with(r#"frames ["hello", "error"]"#);
    (!unpublished || settled.load(Ordering::SeqCst)).then_some(error)
}

struct Hammer {
    stop: Arc<AtomicBool>,
    settled: Arc<AtomicBool>,
    requests: Arc<AtomicU64>,
    failures: Arc<Mutex<Vec<String>>>,
    threads: Vec<std::thread::JoinHandle<()>>,
}

impl Hammer {
    fn start(
        router: axum::Router,
        addr: std::net::SocketAddr,
        http: Vec<String>,
        sockets: Vec<String>,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let settled = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(AtomicU64::new(0));
        let failures = Arc::new(Mutex::new(Vec::new()));
        let threads = (0..READERS)
            .map(|reader| {
                let (router, http, sockets) = (router.clone(), http.clone(), sockets.clone());
                let (stop, settled, requests, failures) = (
                    stop.clone(),
                    settled.clone(),
                    requests.clone(),
                    failures.clone(),
                );
                std::thread::spawn(move || {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap();
                    // Each reader walks the routes from its own offset.
                    let mut next = reader * http.len() / READERS;
                    let mut round = 0_usize;
                    while !stop.load(Ordering::SeqCst) {
                        let uri = &http[next % http.len()];
                        next += 1;
                        let (status, body) = runtime.block_on(get(&router, uri));
                        requests.fetch_add(1, Ordering::Relaxed);
                        // 503 not_ready is the honest answer of aggregates
                        // and full commands while unpublished; a lock refusal
                        // would also be one, and is counted separately.
                        if status >= 500 && !(status == 503 && body["error"]["code"] == "not_ready")
                        {
                            failures
                                .lock()
                                .unwrap()
                                .push(format!("{uri}: {status} {body}"));
                        }
                        round += 1;
                        if round.is_multiple_of(16) && !sockets.is_empty() {
                            let path = &sockets[(reader + round / 16) % sockets.len()];
                            if let Err(error) = runtime.block_on(snapshot(addr, path))
                                && let Some(error) = settled_failure(&settled, error)
                            {
                                failures.lock().unwrap().push(error);
                            }
                            requests.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                })
            })
            .collect();
        Self {
            stop,
            settled,
            requests,
            failures,
            threads,
        }
    }

    /// The build is finished: every snapshot must succeed from now on.
    fn settle(&self) {
        self.settled.store(true, Ordering::SeqCst);
    }

    fn requests(&self) -> u64 {
        self.requests.load(Ordering::Relaxed)
    }

    fn stop(self) -> Vec<String> {
        self.stop.store(true, Ordering::SeqCst);
        for thread in self.threads {
            thread.join().unwrap();
        }
        std::mem::take(&mut *self.failures.lock().unwrap())
    }
}

#[test]
fn readers_on_every_route_never_meet_a_lock_refusal_and_leave_the_projection_unchanged() {
    let home = home();

    // The reference: the same build in a private projection nobody reads.
    let mut reference = Store::open_in_memory_for_rebuild().unwrap();
    build(home.path(), &mut reference);
    let (http, sockets) = routes(&reference);
    assert!(http.len() > 100, "{} routes", http.len());

    // Fresh serve's shared-cache RAM generation and its read router.
    let temp = TempDir::new().unwrap();
    let db = temp.path().join("projection.sqlite");
    let uri = format!(
        "file:rosetta-shared-cache-readers-{}-{}?mode=memory&cache=shared",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let mut store = Store::open_shared_in_memory_for_rebuild_with_assets(&uri, &db).unwrap();
    store.observe_meta_effects();
    let projection = Arc::new(rosetta_server::ProjectionReader::shared_memory(&uri).unwrap());
    let hub = Arc::new(rosetta_server::Hub::new(
        rosetta_server::HubConfig::default(),
        1,
    ));
    let router = rosetta_server::router(
        rosetta_server::AppState::new(hub, db.clone()).with_projection(projection.clone()),
    );
    let server = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let listener = server
        .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
        .unwrap();
    let addr = listener.local_addr().unwrap();
    let served = router.clone();
    server.spawn(async move { axum::serve(listener, served).await.unwrap() });

    let hammer = Hammer::start(router, addr, http, sockets);
    while hammer.requests() < READERS as u64 {
        std::thread::sleep(Duration::from_millis(5));
    }
    build(home.path(), &mut store);
    hammer.settle();
    let during_build = hammer.requests();
    // Keep reading the finalized RAM generation, as readers do until the
    // durable copy is installed.
    std::thread::sleep(Duration::from_millis(250));
    let failures = hammer.stop();

    assert!(
        failures.is_empty(),
        "{} failed reads: {failures:#?}",
        failures.len()
    );
    let report = projection.shared_cache_locks().unwrap().report();
    assert_eq!(report.reader_locked_retries, 0, "{report:#?}");
    assert_eq!(report.reader_locked_refusals, 0, "{report:#?}");
    assert_eq!(report.writer_lock_order_violations, 0, "{report:#?}");
    assert!(
        report.streaming.reader_prologue_wait.count > 0,
        "{report:#?}"
    );
    assert!(
        report.finalize.reader_prologue_wait.count > 0,
        "{report:#?}"
    );
    assert!(report.streaming.session_lock_wait.count > 0, "{report:#?}");
    assert!(
        during_build > READERS as u64,
        "{during_build} reads during the build"
    );

    let (reference, candidate) = (dump(&reference), dump(&store));
    assert_eq!(
        reference.keys().collect::<Vec<_>>(),
        candidate.keys().collect::<Vec<_>>()
    );
    for (table, rows) in &reference {
        assert!(rows == &candidate[table], "table {table} differs");
    }
}

#[test]
fn a_fresh_serve_profile_reports_the_shared_cache_lock_evidence() {
    let temp = TempDir::new().unwrap();
    let home = temp.path().join("home");
    let data = expand_data_root(&ClaudeCodeAdapter.layout().data_root, &home);
    std::fs::create_dir_all(&data).unwrap();
    rosetta_bench_corpus::generate(
        &rosetta_bench_corpus::CorpusSpec::new(rosetta_bench_corpus::Agent::ClaudeCode, 4, 400, 7),
        &data,
    )
    .unwrap();
    let db = temp.path().join("rosetta.sqlite");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_rosettad"))
        .args([
            "serve",
            "--db",
            db.to_str().unwrap(),
            "--home",
            home.to_str().unwrap(),
            "--agent",
            "claude-code",
            "--addr",
            "127.0.0.1:0",
            "--ticks",
            "0",
            "--json",
            "--profile",
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let profiles: Vec<serde_json::Value> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|event| event["event"] == "stage-profile")
        .collect();
    let stage = |name: &str| {
        profiles
            .iter()
            .find(|event| event["stage"] == name)
            .unwrap_or_else(|| panic!("no {name} stage profile"))
    };
    // The writer's streaming window: every writer transaction took W1.
    let streaming = &stage("streamed-writer")["sharedCacheLocks"]["streaming"];
    assert!(streaming["sessionLockWait"]["count"].as_u64().unwrap() > 0);
    for key in ["p50Ms", "p99Ms", "maxMs"] {
        assert!(streaming["sessionLockWait"][key].is_number(), "{key}");
    }
    // After publication: both windows, and nothing was ever refused.
    let locks = &stage("shared-cache-locks")["locks"];
    assert!(
        locks["finalize"]["sessionLockWait"]["count"]
            .as_u64()
            .unwrap()
            >= 1
    );
    assert!(locks["finalize"]["readerPrologueWait"]["count"].is_u64());
    assert_eq!(locks["readerLockedRetries"], 0);
    assert_eq!(locks["readerLockedRefusals"], 0);
    assert_eq!(locks["writerLockOrderViolations"], 0);
}
