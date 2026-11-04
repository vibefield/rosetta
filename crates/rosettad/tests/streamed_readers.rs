//! R1 brief §10 tests 3, 4, 5 and 10: readers of fresh serve's shared-cache
//! RAM generation while a root-tree runtime streams into it.
//!
//! - **Reader between slices** (critique major 3). Tree `alpha` has flipped;
//!   tree `bravo`'s 2 MiB source, half of it resolved tool rows, applies in
//!   64 KiB slices. After every slice commit the writer parks until a reader
//!   thread has read `alpha`'s timeline through the read router (R1
//!   prologue and all), so dozens of reads are served between two slice
//!   commits — each with `bravo` still hidden. (Mutation: never chunking
//!   makes the source one transaction, and no read falls between slices.)
//! - **Barrier** (extends `streamed_cold.rs`'s barrier test to trees): with
//!   `alpha` flipped, `bravo` partly and then wholly landed through slices,
//!   and `delta` not walked yet, every HTTP and WS route serves `alpha` and
//!   hides the other two; `/api/activity` and the metadata mirror skip them.
//! - **Chaos readers** (test 5). Eight readers hammer every payload route and
//!   both WS streams through capture, per-tree flips, finalization and the
//!   switch to the published disk copy, on the giant-newest corpus (scaled:
//!   G = 1 MiB, chunk = 64 KiB). No 5xx beyond an aggregate's documented
//!   `not_ready` on the RAM generation, no lock refusal or retry, every
//!   session once served stays served (I8), every read that served a
//!   session equals its answer once published, and the session list shows a
//!   Pending session without activity and any other as it ends. (Mutations:
//!   a timeline route that ignores Pending serves a tree's rows mid-slices,
//!   and the answer differs from the final one; a mirror that ignores the
//!   barrier lists a Pending session's activity.)
//! - **Writer transactions** (§8.2 T5 delta 1). No streamed path opens a
//!   SQLite transaction outside `lock_order`'s `writer_tx` / `ddl_tx`: a
//!   grep over the sources lists every `transaction()` the store opens.

mod support;

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::StreamExt;
use rosetta_adapter_claude_code::ClaudeCodeAdapter;
use rosetta_pipeline::ColdWalkOptions;
use rosetta_server::{AppState, Hub, HubConfig, ProjectionReader, router};
use rosetta_store::CommitKind;
use rusqlite::{Connection, OpenFlags};
use tempfile::TempDir;

use support::fixtures::flipped_beside_sliced;
use support::harness::{shared_store, walkdir};
use support::writer::{BuildSpec, Schedule, build};

const KIB: u64 = 1024;
const MIB: usize = 1024 * 1024;
/// The fixtures' day (`fixtures::T0` onward), within the route's 7-day cap.
const WINDOW: &str = "since_ms=1788300000000&until_ms=1788800000000";

fn reader_conn(uri: &str) -> Connection {
    Connection::open_with_flags(
        uri,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_URI,
    )
    .unwrap()
}

/// A session's id and readiness by its native id, in one autocommit read;
/// `None` before its metadata committed.
fn session(conn: &Connection, native: &str) -> Option<(String, String)> {
    use rusqlite::OptionalExtension;
    conn.query_row(
        "SELECT id, stream_state FROM session WHERE native_session_id = ?1",
        [native],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .optional()
    .unwrap()
}

fn rows_of(conn: &Connection, session: &str) -> i64 {
    conn.query_row(
        "SELECT COUNT(*) FROM message WHERE session_id = ?1",
        [session],
        |row| row.get(0),
    )
    .unwrap()
}

async fn get(app: &axum::Router, uri: &str) -> (u16, serde_json::Value) {
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    let response = app
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
    let body = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| serde_json::Value::String(String::from_utf8_lossy(&bytes).into()));
    (status, body)
}

/// A socket's greeting and its first snapshot frame.
async fn snapshot(addr: SocketAddr, path: &str) -> serde_json::Value {
    use tokio_tungstenite::tungstenite::Message;
    let (mut socket, _) = tokio_tungstenite::connect_async(format!("ws://{addr}{path}"))
        .await
        .unwrap();
    let mut frames = Vec::new();
    while frames.len() < 2 {
        let message = tokio::time::timeout(Duration::from_secs(10), socket.next())
            .await
            .unwrap_or_else(|_| panic!("{path}: no frame within 10 s"));
        if let Some(Ok(Message::Text(text))) = message {
            frames.push(serde_json::from_str::<serde_json::Value>(text.as_str()).unwrap());
        }
    }
    let _ = socket.close(None).await;
    assert_eq!(frames[0]["frame"], "hello", "{path}");
    frames.pop().unwrap()
}

/// Fresh serve's read side of one shared generation: the hub (holding the
/// admission barrier), the router, and a bound server for the sockets.
struct ReadSide {
    hub: Arc<Hub>,
    app: axum::Router,
    addr: SocketAddr,
    projection: Arc<ProjectionReader>,
    _server: tokio::runtime::Runtime,
}

fn read_side(uri: &str, db: &Path) -> ReadSide {
    let hub = Arc::new(Hub::new(HubConfig::default(), 1));
    hub.set_hide_pending(true);
    let projection = Arc::new(ProjectionReader::shared_memory(uri).unwrap());
    let app =
        router(AppState::new(hub.clone(), db.to_path_buf()).with_projection(projection.clone()));
    let server = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let listener = server
        .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
        .unwrap();
    let addr = listener.local_addr().unwrap();
    let served = app.clone();
    server.spawn(async move { axum::serve(listener, served).await.unwrap() });
    ReadSide {
        hub,
        app,
        addr,
        projection,
        _server: server,
    }
}

/// One read a parked writer waited for.
#[derive(Debug)]
struct Read {
    after: CommitKind,
    alpha: (u16, usize, String),
    bravo: (u16, usize, String),
}

/// What [`between_slices`] saw.
#[derive(Debug, Default)]
struct Between {
    commits: BTreeMap<String, u64>,
    reads: Vec<Read>,
}

/// Build `alpha` and `bravo` with every slice commit followed by one
/// reader-thread read, the writer parked until it completes.
fn between_slices(chunk: u64) -> Between {
    let home = TempDir::new().unwrap();
    // About 2 MiB of turns and 1 MiB of resolved tool rows (a tool row
    // keeps a bounded command preview, about 300 approx bytes).
    flipped_beside_sliced(home.path(), 2 * MIB, 3_600);
    let dir = TempDir::new().unwrap();
    let (mut store, uri) = shared_store(dir.path(), "between-slices");
    let side = read_side(&uri, &dir.path().join("projection.sqlite"));
    let (ask, asked) = mpsc::channel::<CommitKind>();
    let (answer, answered) = mpsc::channel::<Read>();
    let reader = {
        let (app, uri) = (side.app.clone(), uri.clone());
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let conn = reader_conn(&uri);
            while let Ok(after) = asked.recv() {
                let read = |native: &str| {
                    let (id, state) = session(&conn, native).expect("metadata committed");
                    let (status, page) =
                        runtime.block_on(get(&app, &format!("/api/sessions/{id}/timeline")));
                    let entries = page["entries"].as_array().map_or(0, Vec::len);
                    (status, entries, state)
                };
                let read = Read {
                    after,
                    alpha: read("alpha"),
                    bravo: read("bravo"),
                };
                answer.send(read).unwrap();
            }
        })
    };
    let seen: Arc<Mutex<Between>> = Arc::default();
    {
        let seen = seen.clone();
        store.set_commit_observer(Some(Box::new(move |kind| {
            let mut seen = seen.lock().unwrap();
            *seen.commits.entry(format!("{kind:?}")).or_default() += 1;
            if matches!(kind, CommitKind::StageSlice | CommitKind::ResolvedSlice) {
                // Parked: nothing commits until the reader has read.
                ask.send(kind).unwrap();
                seen.reads.push(answered.recv().unwrap());
            }
        })));
    }
    let spec = BuildSpec {
        chunk,
        schedule: Schedule::Drain,
        walk: ColdWalkOptions::default(),
        ..BuildSpec::declared()
    };
    let built = build(&ClaudeCodeAdapter, home.path(), &spec, &mut store, None);
    assert_eq!(built.result, Ok(()));
    assert!(built.flips >= 2, "alpha flipped on its own: {built:?}");
    store.set_commit_observer(None);
    reader.join().unwrap();
    let report = side.projection.shared_cache_locks().unwrap().report();
    assert_eq!(report.reader_locked_retries, 0, "{report:#?}");
    assert_eq!(report.reader_locked_refusals, 0, "{report:#?}");
    assert_eq!(report.writer_lock_order_violations, 0, "{report:#?}");
    std::mem::take(&mut *seen.lock().unwrap())
}

#[test]
fn a_flipped_tree_is_read_between_every_two_slices_of_a_hidden_one() {
    let between = between_slices(64 * KIB);
    eprintln!("{:?}", between.commits);
    assert!(
        between.reads.len() >= 45,
        "{} reads between slices: {:?}",
        between.reads.len(),
        between.commits
    );
    let stage = between.commits.get("StageSlice").copied().unwrap_or(0);
    let resolved = between.commits.get("ResolvedSlice").copied().unwrap_or(0);
    assert!(stage >= 32 && resolved >= 16, "{:?}", between.commits);
    for read in &between.reads {
        // Alpha is flipped and whole; bravo, between its slices, is hidden.
        assert_eq!(read.alpha.0, 200, "{read:?}");
        assert!(read.alpha.1 > 0, "{read:?}");
        assert_eq!(read.alpha.2, "indexed", "{read:?}");
        assert_eq!((read.bravo.0, read.bravo.1), (200, 0), "{read:?}");
        assert_eq!(read.bravo.2, "pending", "{read:?}");
    }
    assert!(
        between
            .reads
            .iter()
            .any(|read| read.after == CommitKind::ResolvedSlice),
        "reads between resolved-row slices too"
    );
}

/// One run of the route matrix: who is served and who is hidden.
#[derive(Debug, Default)]
struct Matrix {
    runs: Vec<(CommitKind, i64)>,
    failures: Vec<String>,
}

/// Every payload route over the three trees: `served` must read whole,
/// every other hidden. Each id comes with one of its rows already in the
/// generation, when it has one, so a hidden session's exact-row route is
/// asked for a row that exists. Returns failures.
async fn route_matrix(
    side_app: &axum::Router,
    addr: SocketAddr,
    ids: &[(String, bool, Option<String>)],
) -> Vec<String> {
    let mut failures = Vec::new();
    let mut check = |ok: bool, what: String| {
        if !ok {
            failures.push(what);
        }
    };
    let (status, activity) = get(side_app, &format!("/api/activity?{WINDOW}")).await;
    check(status == 200, format!("/api/activity: {status}"));
    let active: Vec<String> = activity["sessions"]
        .as_array()
        .map(|sessions| {
            sessions
                .iter()
                .filter_map(|entry| entry["session"]["id"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let meta = snapshot(addr, "/ws/meta").await;
    for (id, served, stored) in ids {
        let (status, page) = get(side_app, &format!("/api/sessions/{id}/timeline")).await;
        let entries = page["entries"].as_array().cloned().unwrap_or_default();
        check(
            status == 200 && entries.is_empty() != *served,
            format!(
                "{id} timeline: {status}, {} entries, served {served}",
                entries.len()
            ),
        );
        if let Some(entry) = entries.first() {
            let timeline = entry["timeline_id"].as_str().unwrap();
            let (status, _) =
                get(side_app, &format!("/api/sessions/{id}/timeline/{timeline}")).await;
            check(status == 200, format!("{id} exact row: {status}"));
        }
        if let Some(timeline) = stored.as_deref().filter(|_| !*served) {
            // A row landed (through slices or whole) behind the barrier.
            let (status, body) =
                get(side_app, &format!("/api/sessions/{id}/timeline/{timeline}")).await;
            check(
                status == 404,
                format!("{id} hidden exact row {timeline}: {status} {body}"),
            );
        }
        let (status, collaboration) =
            get(side_app, &format!("/api/sessions/{id}/collaboration")).await;
        let hidden = collaboration["entries"]
            .as_array()
            .is_some_and(Vec::is_empty)
            && collaboration["response_runtimes"]
                .as_array()
                .is_some_and(Vec::is_empty);
        check(
            status == 200 && (*served || hidden),
            format!("{id} collaboration: {status} {collaboration}"),
        );
        let (status, detail) = get(side_app, &format!("/api/sessions/{id}")).await;
        check(
            status == 200 && detail["session"]["activity"].is_null() != *served,
            format!("{id} session: {status} {}", detail["session"]),
        );
        check(
            active.contains(id) == *served,
            format!(
                "{id} in /api/activity: {}, served {served}",
                active.contains(id)
            ),
        );
        let socket = snapshot(addr, &format!("/ws/sessions/{id}")).await;
        let frames = socket["entries"].as_array().map_or(0, Vec::len);
        check(
            socket["frame"] == "timeline_snapshot" && (frames == 0) != *served,
            format!("{id} socket: {} with {frames} entries", socket["frame"]),
        );
        let socket = snapshot(addr, &format!("/ws/collaboration/{id}")).await;
        check(
            socket["frame"] == "collaboration_snapshot"
                && (*served
                    || socket["response_runtimes"]
                        .as_array()
                        .is_some_and(Vec::is_empty)),
            format!("{id} collaboration socket: {socket}"),
        );
        let mirrored = meta["sessions"]
            .as_array()
            .and_then(|rows| rows.iter().find(|row| row["id"] == id.as_str()))
            .map(|row| !row["activity"].is_null());
        check(
            mirrored == Some(*served),
            format!("{id} in /ws/meta: activity {mirrored:?}, served {served}"),
        );
    }
    failures
}

/// R1 §10 test 4, and §8.2's T7 delta 2: a flipped tree is readable on every
/// HTTP and WS route while a sibling tree whose rows landed through Stage
/// and resolved-row slices stays hidden — mid-slices and once all its rows
/// are in — as does a tree not walked yet.
#[test]
fn a_flipped_tree_is_served_while_trees_landed_through_slices_stay_hidden() {
    let home = TempDir::new().unwrap();
    flipped_beside_sliced(home.path(), 256 * 1024, 600);
    let dir = TempDir::new().unwrap();
    let (mut store, uri) = shared_store(dir.path(), "barrier");
    let side = read_side(&uri, &dir.path().join("projection.sqlite"));
    let matrix: Arc<Mutex<Matrix>> = Arc::default();
    {
        let (matrix, app, addr, hub) = (
            matrix.clone(),
            side.app.clone(),
            side.addr,
            side.hub.clone(),
        );
        let conn = reader_conn(&uri);
        let client = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let mut stages = 0;
        store.set_commit_observer(Some(Box::new(move |kind| {
            if kind == CommitKind::StageSlice {
                stages += 1;
            }
            // Only while every tree is still to be decided: alpha flipped,
            // bravo between slices or landed whole, delta not walked.
            let (Some((alpha, alpha_state)), Some((bravo, bravo_state)), Some((delta, _))) = (
                session(&conn, "alpha"),
                session(&conn, "bravo"),
                session(&conn, "delta"),
            ) else {
                return;
            };
            let bravo_rows = rows_of(&conn, &bravo);
            let stored = |session: &str| -> Option<String> {
                use rusqlite::OptionalExtension;
                conn.query_row(
                    "SELECT timeline_id FROM message WHERE session_id = ?1 ORDER BY timeline_id LIMIT 1",
                    [session],
                    |row| row.get(0),
                )
                .optional()
                .unwrap()
            };
            let mid_slices = kind == CommitKind::StageSlice && stages == 8;
            let landed = kind == CommitKind::Sources && bravo_rows > 0 && bravo_state == "pending";
            if alpha_state == "pending" || !(mid_slices || landed) {
                return;
            }
            // The writer publishes metadata after each commit; the mirror
            // holds the barrier, as fresh serve's does.
            hub.rewarm(&conn).unwrap();
            let ids = [
                (alpha.clone(), true, stored(&alpha)),
                (bravo.clone(), false, stored(&bravo)),
                (delta.clone(), false, stored(&delta)),
            ];
            let failures = client.block_on(route_matrix(&app, addr, &ids));
            let mut matrix = matrix.lock().unwrap();
            matrix.runs.push((kind, bravo_rows));
            matrix.failures.extend(failures);
        })));
    }
    let spec = BuildSpec {
        chunk: 16 * KIB,
        schedule: Schedule::Drain,
        ..BuildSpec::declared()
    };
    let built = build(&ClaudeCodeAdapter, home.path(), &spec, &mut store, None);
    assert_eq!(built.result, Ok(()));
    store.set_commit_observer(None);
    let matrix = std::mem::take(&mut *matrix.lock().unwrap());
    assert!(matrix.failures.is_empty(), "{:#?}", matrix.failures);
    // Both moments were reached: mid-slices with part of bravo's rows in,
    // and its terminal with all of them.
    assert_eq!(matrix.runs.len(), 2, "{:?}", matrix.runs);
    // Bravo held rows both times, so its exact-row route was asked for one.
    assert!(
        0 < matrix.runs[0].1 && matrix.runs[0].1 < matrix.runs[1].1,
        "{:?}",
        matrix.runs
    );
    let report = side.projection.shared_cache_locks().unwrap().report();
    assert_eq!(report.reader_locked_refusals, 0, "{report:#?}");
}

/// Every `transaction()` the store crate opens, by file: the lock-order
/// constructors (W1's `writer_tx`, W2's `ddl_tx`), the private rebuild's
/// one-transaction finalization, the disk-progressive load's finalization
/// and the pricing table's own connection. No streamed path opens one: they
/// all go through `writer_tx`.
const STORE_TRANSACTIONS: [(&str, usize); 4] = [
    ("crates/store/src/lock_order.rs", 2),
    ("crates/store/src/bulk.rs", 1),
    ("crates/store/src/lib.rs", 2),
    ("crates/store/src/pricing.rs", 1),
];

/// The streamed fresh-cold paths: the store's cold stream, the pipeline's
/// streamed runtime and closed capture, and rosettad's cold stream.
const STREAMED_PATHS: [&str; 8] = [
    "crates/store/src/cold_stream.rs",
    "crates/store/src/cold_stream/",
    "crates/pipeline/src/streamed.rs",
    "crates/pipeline/src/streamed/",
    "crates/pipeline/src/closed.rs",
    "crates/pipeline/src/closed/",
    "crates/rosettad/src/cold_stream.rs",
    "crates/rosettad/src/cold_stream/",
];

/// Ways to open a SQLite transaction without `lock_order`.
const OPENERS: [&str; 5] = [
    ".transaction()",
    ".transaction_with_behavior(",
    ".unchecked_transaction(",
    "\"BEGIN",
    "Transaction::new(",
];

#[test]
fn no_streamed_path_opens_a_transaction_outside_lock_order() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut store: BTreeMap<String, usize> = BTreeMap::new();
    let mut streamed: Vec<String> = Vec::new();
    for path in walkdir(&root.join("crates")) {
        let file = path
            .strip_prefix(&root)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if path.extension().is_none_or(|ext| ext != "rs")
            || file.contains("/tests/")
            || name.ends_with("tests.rs")
        {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        // Test modules inline in a product file are not product code.
        let product = text.split("#[cfg(test)]\nmod tests").next().unwrap();
        for (number, line) in product.lines().enumerate() {
            if !OPENERS.iter().any(|opener| line.contains(opener)) {
                continue;
            }
            if STREAMED_PATHS.iter().any(|prefix| file.starts_with(prefix)) {
                streamed.push(format!("{file}:{}: {}", number + 1, line.trim()));
            }
            if file.starts_with("crates/store/src/") && line.contains(".transaction()") {
                *store.entry(file.clone()).or_default() += 1;
            }
        }
    }
    assert!(
        streamed.is_empty(),
        "a streamed path opens a transaction outside lock_order (W1/W2): {streamed:#?}"
    );
    let known: BTreeMap<String, usize> = STORE_TRANSACTIONS
        .iter()
        .map(|(file, count)| (file.to_string(), *count))
        .collect();
    assert_eq!(
        store, known,
        "the store's transactions changed: a new writer transaction opens through \
         Store::writer_tx or lock_order::writer_tx, a DDL one through ddl_tx"
    );
}

const CHAOS_READERS: usize = 8;

/// One read a chaos reader made: its route, when it ran (a global sequence
/// taken before and after), and what it saw.
#[derive(Debug)]
struct Seen {
    uri: String,
    session: Option<String>,
    start: u64,
    end: u64,
    status: u16,
    /// The session's payload was served (not the hidden answer).
    visible: bool,
    digest: blake3::Hash,
    /// The session list's entries: id, whether Pending, and the entry.
    listed: Vec<(String, bool, serde_json::Value)>,
}

/// The session list read during the build.
const LIST: &str = "/api/sessions?limit=500";

/// A session list's entries by id: whether each is Pending, and the entry.
fn list_entries(body: &serde_json::Value) -> Vec<(String, bool, serde_json::Value)> {
    body["sessions"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|entry| {
            (
                entry["id"].as_str().unwrap_or_default().to_owned(),
                entry["stream_state"] == "pending",
                entry.clone(),
            )
        })
        .collect()
}

/// Whether a 200 body serves its session's payload, by route shape.
fn serves_payload(uri: &str, body: &serde_json::Value) -> bool {
    let nonempty =
        |value: &serde_json::Value| value.as_array().is_some_and(|rows| !rows.is_empty());
    if uri.starts_with("/ws/collaboration/") || uri.ends_with("/collaboration") {
        nonempty(&body["entries"]) || nonempty(&body["response_runtimes"])
    } else if uri.starts_with("/ws/sessions/") || uri.ends_with("/timeline") {
        nonempty(&body["entries"])
    } else if uri.contains("/timeline/") {
        true
    } else {
        !body["session"]["activity"].is_null()
    }
}

/// What a body says of its session, digested: a socket's snapshot entries
/// (its stamps are per connection), otherwise the whole body.
fn digest(uri: &str, body: &serde_json::Value) -> blake3::Hash {
    let shown = if uri.starts_with("/ws/") {
        serde_json::json!([body["entries"], body["response_runtimes"]])
    } else {
        body.clone()
    };
    blake3::hash(shown.to_string().as_bytes())
}

/// The session a per-session route reads.
fn session_of(uri: &str) -> Option<String> {
    let rest = uri
        .strip_prefix("/api/sessions/")
        .or_else(|| uri.strip_prefix("/ws/sessions/"))
        .or_else(|| uri.strip_prefix("/ws/collaboration/"))?;
    let id = rest.split(['/', '?']).next()?;
    (!id.is_empty()).then(|| id.to_owned())
}

/// Every payload route of the finished reference: lists, per-session
/// detail, timeline, collaboration and one exact row per row type, both
/// sockets, and every project's aggregates.
fn chaos_routes(reference: &rosetta_store::Store) -> (Vec<String>, Vec<String>) {
    let pairs = |sql: &str| -> Vec<(String, String)> {
        let mut statement = reference.conn().prepare(sql).unwrap();
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    let mut http = vec![
        format!("/api/activity?{WINDOW}"),
        LIST.to_string(),
        "/api/projects".to_string(),
        "/api/artifacts".to_string(),
    ];
    let mut sockets = vec!["/ws/meta".to_string()];
    for (session, _) in pairs("SELECT id, runtime_id FROM session ORDER BY id") {
        for suffix in ["", "/timeline", "/collaboration"] {
            http.push(format!("/api/sessions/{session}{suffix}"));
        }
        sockets.push(format!("/ws/sessions/{session}"));
        sockets.push(format!("/ws/collaboration/{session}"));
    }
    for (session, timeline) in
        pairs("SELECT session_id, MIN(timeline_id) FROM message GROUP BY session_id, type")
    {
        http.push(format!("/api/sessions/{session}/timeline/{timeline}"));
    }
    for (project, _) in pairs("SELECT id, name FROM project") {
        for suffix in ["heatmap", "usage", "contribution"] {
            http.push(format!("/api/projects/{project}/{suffix}"));
        }
    }
    (http, sockets)
}

/// A socket's snapshot frame, or why there is none.
async fn snapshot_frame(addr: SocketAddr, path: &str) -> Result<serde_json::Value, String> {
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
                frames.push(serde_json::from_str::<serde_json::Value>(text.as_str()).unwrap());
            }
            Some(Ok(_)) => {}
            other => return Err(format!("{path}: stream ended: {other:?}")),
        }
    }
    let _ = socket.close(None).await;
    Ok(frames.pop().unwrap())
}

/// R1 §10 test 5: eight readers hammer every payload route and both WS
/// streams of the shared RAM generation while the giant-newest corpus
/// (scaled: G = 1 MiB, chunk = 64 KiB) captures, flips tree by tree,
/// finalizes and is published to disk. The build is Ok; no read answers 5xx
/// beyond the documented `not_ready` of an aggregate on the RAM generation;
/// no read meets a lock refusal or retry; a session once served is never
/// hidden again (I8); every response that served a session's payload
/// equals that session's answer after publication; and every session list
/// read shows each Pending session without activity, every other one as
/// the published list does.
#[test]
fn chaos_readers_see_each_session_hidden_then_final_never_partial() {
    let home = TempDir::new().unwrap();
    support::fixtures::giant_newest(home.path(), 12, MIB, 7);
    let spec = BuildSpec {
        chunk: 64 * KIB,
        schedule: Schedule::Drain,
        ..BuildSpec::declared()
    };
    let mut reference = support::harness::cold_store();
    let built = build(&ClaudeCodeAdapter, home.path(), &spec, &mut reference, None);
    assert_eq!(built.result, Ok(()));
    let (http, sockets) = chaos_routes(&reference);
    drop(reference);

    let dir = TempDir::new().unwrap();
    let db = dir.path().join("projection.sqlite");
    let (mut store, uri) = shared_store(dir.path(), "chaos-readers");
    store.observe_meta_effects();
    let side = read_side(&uri, &db);
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let sequence = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let readers: Vec<_> = (0..CHAOS_READERS)
        .map(|reader| {
            let (app, addr) = (side.app.clone(), side.addr);
            let (http, sockets) = (http.clone(), sockets.clone());
            let (stop, sequence) = (stop.clone(), sequence.clone());
            std::thread::spawn(move || {
                use std::sync::atomic::Ordering;
                let client = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                let mut seen = Vec::new();
                let mut next = reader * http.len() / CHAOS_READERS;
                let mut round = 0_usize;
                while !stop.load(Ordering::SeqCst) {
                    round += 1;
                    let socket = round.is_multiple_of(12);
                    let uri = if socket {
                        sockets[(reader + round / 12) % sockets.len()].clone()
                    } else {
                        next += 1;
                        http[next % http.len()].clone()
                    };
                    let start = sequence.fetch_add(1, Ordering::SeqCst);
                    let (status, body) = if socket {
                        match client.block_on(snapshot_frame(addr, &uri)) {
                            Ok(frame) => (200, frame),
                            Err(error) => (599, serde_json::Value::String(error)),
                        }
                    } else {
                        client.block_on(get(&app, &uri))
                    };
                    let end = sequence.fetch_add(1, Ordering::SeqCst);
                    let session = session_of(&uri);
                    let visible = status == 200 && session.is_some() && serves_payload(&uri, &body);
                    let listed = if uri == LIST && status == 200 {
                        list_entries(&body)
                    } else {
                        Vec::new()
                    };
                    seen.push(Seen {
                        listed,
                        digest: digest(&uri, &body),
                        uri,
                        session,
                        start,
                        end,
                        status,
                        visible,
                    });
                }
                seen
            })
        })
        .collect();

    // The mirror follows every commit, as fresh serve's writer absorbs
    // metadata after each one.
    {
        let (hub, conn) = (side.hub.clone(), reader_conn(&uri));
        store.set_commit_observer(Some(Box::new(move |_| {
            hub.rewarm(&conn).unwrap();
        })));
    }
    // Capture, flips and finalization (the harness build's finish).
    let built = build(&ClaudeCodeAdapter, home.path(), &spec, &mut store, None);
    store.set_commit_observer(None);
    assert_eq!(built.result, Ok(()));
    assert!(built.flips >= 2, "trees flipped one by one: {built:?}");
    assert!(built.slices > 0, "the giant applied in slices");
    let finalized = sequence.load(std::sync::atomic::Ordering::SeqCst);
    // Publication, as fresh serve does it: the durable copy installed, the
    // read target switched to disk, the mirror's barrier lifted.
    store
        .conn()
        .execute("VACUUM INTO ?1", [db.to_str().unwrap()])
        .unwrap();
    side.projection.publish_file(db.clone());
    let disk = Connection::open(&db).unwrap();
    side.hub.rewarm(&disk).unwrap();
    side.hub.reveal_pending(&disk).unwrap();
    side.hub.set_hide_pending(false);
    let published = sequence.load(std::sync::atomic::Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(300));
    stop.store(true, std::sync::atomic::Ordering::SeqCst);
    let seen: Vec<Seen> = readers
        .into_iter()
        .flat_map(|reader| reader.join().unwrap())
        .collect();
    let report = side.projection.shared_cache_locks().unwrap().report();
    assert_eq!(report.reader_locked_retries, 0, "{report:#?}");
    assert_eq!(report.reader_locked_refusals, 0, "{report:#?}");
    assert_eq!(report.writer_lock_order_violations, 0, "{report:#?}");
    drop(store);

    // Every session's answers once published: what each served read must
    // have shown.
    let client = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut finals: BTreeMap<String, blake3::Hash> = BTreeMap::new();
    let mut final_list: BTreeMap<String, serde_json::Value> = BTreeMap::new();
    for uri in http.iter().chain(&sockets) {
        let (status, body) = if uri.starts_with("/ws/") {
            (
                200,
                client.block_on(snapshot_frame(side.addr, uri)).unwrap(),
            )
        } else {
            client.block_on(get(&side.app, uri))
        };
        assert_eq!(status, 200, "{uri} once published: {body}");
        finals.insert(uri.clone(), digest(uri, &body));
        if uri == LIST {
            final_list = list_entries(&body)
                .into_iter()
                .map(|(id, _, entry)| (id, entry))
                .collect();
        }
    }

    let mut failures = Vec::new();
    let during = seen.iter().filter(|read| read.end < published).count();
    for read in &seen {
        let aggregate =
            read.uri.starts_with("/api/projects/") || read.uri.starts_with("/api/pricing");
        // An aggregate on the RAM generation answers not_ready (protocol
        // v7); everything else, and everything once published, answers.
        let honest_not_ready = read.status == 503 && aggregate && read.start < published;
        if read.status >= 500 && !honest_not_ready {
            failures.push(format!("{}: {}", read.uri, read.status));
        }
        if read.visible && finals.get(&read.uri) != Some(&read.digest) {
            failures.push(format!(
                "{} served at [{}, {}] differs from its final answer",
                read.uri, read.start, read.end
            ));
        }
        // The list serves every session's metadata from the start: a
        // Pending one without activity (the mirror's barrier), any other
        // as it is once published.
        for (id, pending, entry) in &read.listed {
            let ok = if *pending {
                entry["activity"].is_null()
            } else {
                final_list.get(id) == Some(entry)
            };
            if !ok {
                failures.push(format!(
                    "{LIST} at [{}, {}] listed {id} (pending {pending}) as {entry}, finally {:?}",
                    read.start,
                    read.end,
                    final_list.get(id)
                ));
            }
        }
    }
    let listed_readable = seen
        .iter()
        .filter(|read| read.end < finalized)
        .flat_map(|read| &read.listed)
        .filter(|(_, pending, _)| !pending)
        .count();
    // I8: once a read served a session, no read that began after it ended
    // hides it again.
    let mut first_served: BTreeMap<&str, u64> = BTreeMap::new();
    for read in seen.iter().filter(|read| read.visible) {
        let session = read.session.as_deref().unwrap();
        let end = first_served.entry(session).or_insert(read.end);
        *end = (*end).min(read.end);
    }
    let payload_routes = |uri: &str| uri.ends_with("/timeline") || uri.starts_with("/ws/sessions/");
    for read in &seen {
        if let Some(session) = &read.session
            && let Some(served) = first_served.get(session.as_str())
            && read.start > *served
            && payload_routes(&read.uri)
            && read.status == 200
            && !read.visible
        {
            failures.push(format!(
                "{} hid {session} at {} after serving it by {served}",
                read.uri, read.start
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} failures: {failures:#?}",
        failures.len()
    );
    let served_before_publication = seen
        .iter()
        .filter(|read| read.visible && read.end < finalized)
        .filter_map(|read| read.session.as_deref())
        .collect::<std::collections::BTreeSet<_>>();
    eprintln!(
        "{} reads, {during} before publication; {} sessions served before finalization",
        seen.len(),
        served_before_publication.len()
    );
    assert!(during > 200, "{during} reads before publication");
    assert!(
        listed_readable > 0,
        "the session list showed flipped trees before finalization"
    );
    assert!(
        served_before_publication.len() >= 2,
        "trees were read as they flipped: {served_before_publication:?}"
    );
    let hidden_then_served = seen.iter().any(|read| {
        read.status == 200
            && !read.visible
            && payload_routes(&read.uri)
            && read
                .session
                .as_deref()
                .is_some_and(|session| first_served.contains_key(session))
    });
    assert!(
        hidden_then_served,
        "some session was read hidden before it was served"
    );
}
