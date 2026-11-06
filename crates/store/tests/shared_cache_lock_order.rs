//! Shared-cache lock order on the canonical RAM generation (W1/W2/R1).
//!
//! Fresh `serve` builds the canonical projection in a named shared-cache
//! memory database while HTTP/WS readers attach to it. Shared cache locks
//! whole tables until the transaction ends, and `unlock_notify` turns a
//! cycle into an immediate `SQLITE_LOCKED` for whichever side closes it.
//! The writer used to lock `ingest_cursor` → `message` → `session` while a
//! timeline read locks `session` → `message`, so one reader could fail the
//! sole canonical writer's transaction and park the whole build.
//!
//! These interleavings are deterministic: readers are stepped through
//! SQLite's raw API and register for the unlock notification themselves,
//! and the store's writer is paused between statements (`lock_probe`).

mod lock_probe;

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::Duration;

use lock_probe::{Pause, Recorder, is_locked, register_unlock, step_once, stepped, untrace};
use rosetta_core::{IngestOp, MessageRow, SessionRow};
use rosetta_store::{
    Batch, CheckpointUpdate, ColdReconciliation, SourceMode, SourceWrite, Store, StoreError,
    lock_order,
};
use rusqlite::{Connection, OpenFlags};
use serde_json::json;

/// The reader prologue R1, exactly as `with_read` runs it.
const PROLOGUE: &str = lock_order::READER_PROLOGUE;
/// The writer lock W1, exactly as `Store::writer_tx` runs it.
const SESSION_LOCK: &str = lock_order::SESSION_LOCK;
/// How long a paused statement or an expected wait may take before the
/// test concludes the other side is blocked.
const BLOCKED: Duration = Duration::from_millis(750);
const PATIENCE: Duration = Duration::from_secs(10);

// ------------------------------------------------------------------ fixtures

fn unique_uri(name: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!(
        "file:rosetta-lock-order-{name}-{}-{nanos}?mode=memory&cache=shared",
        std::process::id()
    )
}

fn attach(uri: &str) -> Connection {
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_URI;
    Connection::open_with_flags(uri, flags).unwrap()
}

fn session(id: &str) -> SessionRow {
    serde_json::from_value(json!({
        "id": id, "native_session_id": id, "runtime_id": "rt", "root_session_id": id,
        "role": "main", "started_at": 1, "updated_at": 1, "stream_state": "pending"
    }))
    .unwrap()
}

fn metadata() -> Batch {
    Batch {
        ops: vec![
            IngestOp::Runtime(
                serde_json::from_value(json!({
                    "id": "rt", "kind": "claude-code", "data_root": "/agent",
                    "enabled": true, "discovered_at": 1
                }))
                .unwrap(),
            ),
            IngestOp::SessionMeta(session("root")),
        ],
        checkpoints: ["a.jsonl", "b.jsonl"]
            .into_iter()
            .map(|path| CheckpointUpdate {
                runtime_id: "rt".into(),
                stream: "transcript".into(),
                path: path.into(),
                session_id: Some("root".into()),
                expected: None,
                next: format!("fresh {path}"),
                updated_at: None,
            })
            .collect(),
        source: None,
    }
}

/// A cold store with one runtime, one session and two never-read cursors.
fn populated(mut store: Store) -> Store {
    store.use_precomputed_bulk_ordinals().unwrap();
    store.apply(&metadata()).unwrap();
    store
}

/// Fresh serve's canonical RAM generation, plus its keeper connection.
fn shared(name: &str) -> (Store, String, Connection, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let uri = unique_uri(name);
    let keeper = attach(&uri);
    let store = Store::open_shared_in_memory_for_rebuild_with_assets(
        &uri,
        &dir.path().join("projection.sqlite"),
    )
    .unwrap();
    (populated(store), uri, keeper, dir)
}

fn cursor(store: &Store, path: &str) -> i64 {
    store
        .runtime_cursor_ids("rt")
        .unwrap()
        .into_iter()
        .find(|(_, candidate)| candidate == path)
        .unwrap()
        .0
}

fn agent_reply(cursor_id: i64, timeline: &str, ordinal: i64, ts: i64) -> IngestOp {
    let row: MessageRow = serde_json::from_value(json!({
        "timeline_id": timeline, "session_id": "root", "ordinal": ordinal, "kind": "agent",
        "ts": ts, "content": "done", "attachments": [], "citations": [],
        "src_cursor": cursor_id, "src_offset": ordinal * 10
    }))
    .unwrap();
    IngestOp::project_message(row).pop().unwrap()
}

/// One closed source of `root`, as the streamed writer applies it.
fn closed(store: &Store, path: &str) -> Batch {
    let cursor_id = cursor(store, path);
    // Final ordinals, as the streamed preparer assigns them: path order.
    let first = if path == "a.jsonl" { 0 } else { 2 };
    Batch {
        ops: vec![
            agent_reply(cursor_id, &format!("{path}-1"), first, 12),
            agent_reply(cursor_id, &format!("{path}-2"), first + 1, 13),
        ],
        checkpoints: vec![CheckpointUpdate {
            runtime_id: "rt".into(),
            stream: "transcript".into(),
            path: path.into(),
            session_id: Some("root".into()),
            expected: Some(format!("fresh {path}")),
            next: format!("final {path}"),
            updated_at: None,
        }],
        source: Some(SourceWrite {
            cursor_id,
            session_id: "root".into(),
            generation: 1,
            first_sequence: 0,
            mode: SourceMode::Activate,
        }),
    }
}

fn locked_error(error: &StoreError) -> bool {
    let text = error.to_string();
    text.contains("locked") || text.contains("lock-order")
}

/// Run one store write on its own thread, as the daemon's writer does.
fn spawn_writer<T: Send + 'static>(
    store: Store,
    write: impl FnOnce(&mut Store) -> Result<T, StoreError> + Send + 'static,
) -> Receiver<(Store, Result<T, StoreError>)> {
    let (done, outcome) = mpsc::channel();
    thread::spawn(move || {
        let mut store = store;
        let result = write(&mut store);
        let _ = done.send((store, result));
    });
    outcome
}

// ------------------------------------------------- the raw SQL lock models

/// The minimal model of the writer/reader shapes: a `session` table the
/// reader opens with, a `message` table the writer opens with.
fn model(name: &str) -> (String, Connection) {
    let uri = unique_uri(name);
    let keeper = attach(&uri);
    keeper
        .execute_batch(
            "CREATE TABLE session (id TEXT PRIMARY KEY, updated_at INTEGER NOT NULL);
             CREATE TABLE message (id INTEGER PRIMARY KEY, session_id TEXT NOT NULL);
             CREATE TABLE projection_state (singleton INTEGER PRIMARY KEY, revision INTEGER);
             INSERT INTO session VALUES ('root', 1);
             INSERT INTO projection_state VALUES (1, 0);",
        )
        .unwrap();
    (uri, keeper)
}

/// A writer whose statements run one at a time on its own thread, each
/// through rusqlite's ordinary waiting step.
struct ModelWriter {
    run: Sender<&'static str>,
    done: Receiver<Result<(), rusqlite::Error>>,
    thread: thread::JoinHandle<()>,
}

impl ModelWriter {
    fn attach(uri: &str) -> Self {
        let conn = attach(uri);
        let (run, statements) = mpsc::channel::<&'static str>();
        let (results, done) = mpsc::channel();
        let thread = thread::spawn(move || {
            for sql in statements {
                let result = if sql.contains("?1") {
                    conn.execute(sql, [""]).map(|_| ())
                } else {
                    conn.execute_batch(sql)
                };
                if results.send(result).is_err() {
                    break;
                }
            }
        });
        Self { run, done, thread }
    }

    fn start(&self, sql: &'static str) {
        self.run.send(sql).unwrap();
    }

    /// The statement's outcome, or `None` while it is still waiting.
    fn outcome(&self, within: Duration) -> Option<Result<(), rusqlite::Error>> {
        match self.done.recv_timeout(within) {
            Ok(result) => Some(result),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => panic!("model writer exited"),
        }
    }

    fn run(&self, sql: &'static str) -> Result<(), rusqlite::Error> {
        self.start(sql);
        self.outcome(PATIENCE)
            .expect("model statement never finished")
    }

    fn finish(self) {
        drop(self.run);
        self.thread.join().unwrap();
    }
}

fn database_locked(result: &Result<(), rusqlite::Error>) -> bool {
    matches!(
        result,
        Err(rusqlite::Error::SqliteFailure(error, _))
            if error.code == rusqlite::ErrorCode::DatabaseLocked
    )
}

#[test]
fn model_a_message_first_writer_is_the_victim_of_a_session_first_reader() {
    // Without the discipline: the writer locks message, the reader holds
    // session and waits on message, then the writer touches session.
    let (uri, _keeper) = model("a-without");
    let reader = attach(&uri);
    let writer = ModelWriter::attach(&uri);
    reader.execute_batch("BEGIN DEFERRED").unwrap();
    assert!(stepped(step_once(&reader, PROLOGUE, Some(""))));
    writer.run("BEGIN").unwrap();
    writer
        .run("INSERT INTO message (session_id) VALUES ('root')")
        .unwrap();
    assert!(is_locked(step_once(
        &reader,
        "SELECT COUNT(*) FROM message",
        None
    )));
    let notified = register_unlock(&reader).expect("the reader waits on the writer");
    let touched = writer.run("UPDATE session SET updated_at = 2 WHERE id = 'root'");
    assert!(
        database_locked(&touched),
        "the writer closes the cycle and is refused: {touched:?}"
    );
    writer.run("ROLLBACK").unwrap();
    notified.recv_timeout(PATIENCE).unwrap();
    reader.execute_batch("COMMIT").unwrap();
    writer.finish();

    // W1: the writer takes session first, so it waits for the reader's
    // whole transaction instead of locking anything the reader needs.
    let (uri, _keeper) = model("a-with");
    let reader = attach(&uri);
    let writer = ModelWriter::attach(&uri);
    reader.execute_batch("BEGIN DEFERRED").unwrap();
    assert!(stepped(step_once(&reader, PROLOGUE, Some(""))));
    writer.run("BEGIN").unwrap();
    writer.start(SESSION_LOCK);
    assert!(writer.outcome(BLOCKED).is_none(), "W1 waits for the reader");
    assert!(stepped(step_once(
        &reader,
        "SELECT COUNT(*) FROM message",
        None
    )));
    reader.execute_batch("COMMIT").unwrap();
    writer.outcome(PATIENCE).unwrap().unwrap();
    writer
        .run("INSERT INTO message (session_id) VALUES ('root')")
        .unwrap();
    writer
        .run("UPDATE session SET updated_at = 2 WHERE id = 'root'")
        .unwrap();
    writer.run("COMMIT").unwrap();
    writer.finish();
}

#[test]
fn model_b_writer_first_alone_deadlocks_a_projection_state_first_reader() {
    // W1 without R1: the artifacts list reads projection_state before
    // session, so the writer's session lock does not keep it out.
    let (uri, _keeper) = model("b-without");
    let reader = attach(&uri);
    let writer = ModelWriter::attach(&uri);
    reader.execute_batch("BEGIN DEFERRED").unwrap();
    assert!(stepped(step_once(
        &reader,
        "SELECT revision FROM projection_state WHERE singleton = 1",
        None
    )));
    writer.run("BEGIN").unwrap();
    writer.run(SESSION_LOCK).unwrap();
    assert!(is_locked(step_once(
        &reader,
        "SELECT COUNT(*) FROM session",
        None
    )));
    let notified = register_unlock(&reader).expect("the reader waits on the writer");
    let marked = writer.run("UPDATE projection_state SET revision = 1 WHERE singleton = 1");
    assert!(database_locked(&marked), "{marked:?}");
    writer.run("ROLLBACK").unwrap();
    notified.recv_timeout(PATIENCE).unwrap();
    reader.execute_batch("COMMIT").unwrap();
    writer.finish();

    // W1 + R1: the reader's first lock is session, so either it waits for
    // the writer before touching projection_state, or the writer waits.
    let (uri, _keeper) = model("b-with");
    let reader = attach(&uri);
    let writer = ModelWriter::attach(&uri);
    reader.execute_batch("BEGIN DEFERRED").unwrap();
    assert!(stepped(step_once(&reader, PROLOGUE, Some(""))));
    assert!(stepped(step_once(
        &reader,
        "SELECT revision FROM projection_state WHERE singleton = 1",
        None
    )));
    writer.run("BEGIN").unwrap();
    writer.start(SESSION_LOCK);
    assert!(writer.outcome(BLOCKED).is_none(), "W1 waits for the reader");
    assert!(stepped(step_once(
        &reader,
        "SELECT COUNT(*) FROM session",
        None
    )));
    reader.execute_batch("COMMIT").unwrap();
    writer.outcome(PATIENCE).unwrap().unwrap();
    writer
        .run("UPDATE projection_state SET revision = 1 WHERE singleton = 1")
        .unwrap();
    writer.run("COMMIT").unwrap();
    writer.finish();
}

#[test]
fn model_c_ddl_after_the_session_lock_deadlocks_a_reader_parked_at_its_prologue() {
    // A reader that has begun its read transaction holds the schema read
    // lock even while its prologue waits on session; DDL needs the schema
    // write lock. W1 followed by DDL is therefore a cycle.
    let (uri, _keeper) = model("c-without");
    let reader = attach(&uri);
    let writer = ModelWriter::attach(&uri);
    writer.run("BEGIN").unwrap();
    writer.run(SESSION_LOCK).unwrap();
    reader.execute_batch("BEGIN DEFERRED").unwrap();
    assert!(is_locked(step_once(&reader, PROLOGUE, Some(""))));
    let notified = register_unlock(&reader).expect("the prologue waits on W1");
    let indexed = writer.run("CREATE INDEX message_session ON message(session_id)");
    assert!(database_locked(&indexed), "{indexed:?}");
    writer.run("ROLLBACK").unwrap();
    notified.recv_timeout(PATIENCE).unwrap();
    reader.execute_batch("COMMIT").unwrap();
    writer.finish();

    // W2: a DDL transaction is DDL-first and never takes W1. It waits for
    // the parked reader, whose prologue then proceeds untouched.
    let (uri, _keeper) = model("c-with");
    let reader = attach(&uri);
    let writer = ModelWriter::attach(&uri);
    writer.run("BEGIN").unwrap();
    writer.run(SESSION_LOCK).unwrap();
    reader.execute_batch("BEGIN DEFERRED").unwrap();
    assert!(is_locked(step_once(&reader, PROLOGUE, Some(""))));
    let notified = register_unlock(&reader).expect("the prologue waits on W1");
    writer.run("COMMIT").unwrap();
    notified.recv_timeout(PATIENCE).unwrap();
    writer.run("BEGIN").unwrap();
    writer.start("CREATE INDEX message_session ON message(session_id)");
    assert!(
        writer.outcome(BLOCKED).is_none(),
        "the DDL waits out the parked reader"
    );
    assert!(stepped(step_once(&reader, PROLOGUE, Some(""))));
    assert!(stepped(step_once(
        &reader,
        "SELECT COUNT(*) FROM message",
        None
    )));
    reader.execute_batch("COMMIT").unwrap();
    writer.outcome(PATIENCE).unwrap().unwrap();
    writer
        .run("UPDATE projection_state SET revision = 1 WHERE singleton = 1")
        .unwrap();
    writer.run("COMMIT").unwrap();
    writer.finish();
}

#[test]
fn model_d_zero_row_statements_still_take_their_table_locks() {
    // W1 and R1 bind '' and match no row; the table lock lives in the
    // statement prologue, not in the row loop.
    let (uri, _keeper) = model("d");
    let reader = attach(&uri);
    let writer = attach(&uri);
    writer.execute_batch("BEGIN").unwrap();
    assert!(stepped(step_once(&writer, SESSION_LOCK, Some(""))));
    reader.execute_batch("BEGIN DEFERRED").unwrap();
    assert!(
        is_locked(step_once(&reader, PROLOGUE, Some(""))),
        "a zero-row W1 keeps the prologue out"
    );
    writer.execute_batch("COMMIT").unwrap();
    assert!(stepped(step_once(&reader, PROLOGUE, Some(""))));
    writer.execute_batch("BEGIN").unwrap();
    assert!(
        is_locked(step_once(&writer, SESSION_LOCK, Some(""))),
        "a zero-row prologue keeps W1 out"
    );
    reader.execute_batch("COMMIT").unwrap();
    assert!(stepped(step_once(&writer, SESSION_LOCK, Some(""))));
    writer.execute_batch("COMMIT").unwrap();
}

#[test]
fn model_e_a_pending_session_lock_stops_new_readers() {
    // Readers cannot starve W1: once it waits on one reader's session lock,
    // shared cache refuses every new read transaction until the writer has
    // had its turn, so the wait ends with the readers already inside.
    let (uri, _keeper) = model("e");
    let first = attach(&uri);
    let writer = ModelWriter::attach(&uri);
    first.execute_batch("BEGIN DEFERRED").unwrap();
    assert!(stepped(step_once(&first, PROLOGUE, Some(""))));
    writer.run("BEGIN").unwrap();
    writer.start(SESSION_LOCK);
    let late = attach(&uri);
    let deadline = std::time::Instant::now() + PATIENCE;
    loop {
        late.execute_batch("BEGIN DEFERRED").unwrap();
        let rc = step_once(&late, PROLOGUE, Some(""));
        late.execute_batch("ROLLBACK").unwrap();
        if is_locked(rc) {
            break;
        }
        // The writer had not asked yet; this reader got in and left.
        assert!(
            std::time::Instant::now() < deadline,
            "W1 never became pending"
        );
        thread::sleep(Duration::from_millis(5));
    }
    assert!(
        writer.outcome(BLOCKED).is_none(),
        "W1 still waits on the first reader"
    );
    first.execute_batch("COMMIT").unwrap();
    writer.outcome(PATIENCE).unwrap().unwrap();
    writer.run("COMMIT").unwrap();
    writer.finish();
}

// ------------------------------------------------- the store's own writer

#[test]
fn a_streamed_source_never_fails_against_a_session_first_reader() {
    let (store, uri, _keeper, _dir) = shared("a-store");
    let batch = closed(&store, "a.jsonl");
    let reader = attach(&uri);
    // The reader opens its snapshot with session (R1), before the writer.
    reader.execute_batch("BEGIN DEFERRED").unwrap();
    assert!(stepped(step_once(&reader, PROLOGUE, Some(""))));

    let pause = Pause::after(store.conn(), |sql| sql.starts_with("INSERT INTO message"));
    let writer = spawn_writer(store, move |store| {
        let mut fold = ColdReconciliation::default();
        store.apply_streamed_sources(&mut fold, vec![batch.into()])
    });
    if pause.reached(BLOCKED).is_some() {
        // The writer holds message without session: register this reader
        // as waiting on it first, as a timeline read would.
        assert!(is_locked(step_once(
            &reader,
            "SELECT COUNT(*) FROM message",
            None
        )));
        let notified = register_unlock(&reader);
        pause.release();
        let outcome = writer.recv_timeout(PATIENCE).map(|(_, applied)| applied);
        drop(notified);
        reader.execute_batch("ROLLBACK").unwrap();
        panic!("the writer locked message before session: {outcome:?}");
    }
    // The writer waits on the reader's session lock and holds nothing the
    // reader needs.
    assert!(pause.disarm());
    assert!(stepped(step_once(
        &reader,
        "SELECT COUNT(*) FROM message",
        None
    )));
    assert!(stepped(step_once(
        &reader,
        "SELECT COUNT(*) FROM ingest_cursor",
        None
    )));
    reader.execute_batch("COMMIT").unwrap();
    let (store, applied) = writer.recv_timeout(PATIENCE).unwrap();
    untrace(store.conn());
    applied.expect("the streamed source commits once the reader is done");
}

#[test]
fn finish_bulk_load_never_fails_against_a_session_first_reader() {
    let (store, uri, _keeper, _dir) = shared("c-store");
    let reader = attach(&uri);
    reader.execute_batch("BEGIN DEFERRED").unwrap();
    assert!(stepped(step_once(&reader, PROLOGUE, Some(""))));

    let pause = Pause::after(store.conn(), |sql| sql == "DELETE FROM usage_bucket");
    let writer = spawn_writer(store, Store::finish_bulk_load);
    if pause.reached(BLOCKED).is_some() {
        // The refold holds usage_bucket without session; a usage read of
        // this reader registers on it, then the index DDL needs the schema
        // lock this reader's open transaction holds.
        assert!(is_locked(step_once(
            &reader,
            "SELECT COUNT(*) FROM usage_bucket",
            None
        )));
        let notified = register_unlock(&reader);
        pause.release();
        let outcome = writer.recv_timeout(PATIENCE).map(|(_, finished)| finished);
        drop(notified);
        reader.execute_batch("ROLLBACK").unwrap();
        panic!("finalization locked usage_bucket before session: {outcome:?}");
    }
    assert!(pause.disarm());
    assert!(stepped(step_once(
        &reader,
        "SELECT COUNT(*) FROM usage_bucket",
        None
    )));
    reader.execute_batch("COMMIT").unwrap();
    let (store, finished) = writer.recv_timeout(PATIENCE).unwrap();
    untrace(store.conn());
    let finished = finished.map_err(|error| (locked_error(&error), error));
    finished.expect("finalization commits once the reader is done");
}

#[test]
fn a_waiting_writer_records_its_session_lock_wait() {
    // Re-run (a)'s good path and read the evidence: W1 waited out the
    // reader, and nothing was refused.
    let (store, uri, _keeper, _dir) = shared("a-evidence");
    let batch = closed(&store, "a.jsonl");
    let locks = store.shared_cache_locks().expect("the shared generation");
    let reader = attach(&uri);
    reader.execute_batch("BEGIN DEFERRED").unwrap();
    lock_order::reader_prologue(&reader).unwrap();
    let writer = spawn_writer(store, move |store| {
        let mut fold = ColdReconciliation::default();
        store.apply_streamed_sources(&mut fold, vec![batch.into()])
    });
    assert!(writer.recv_timeout(BLOCKED).is_err(), "W1 waits");
    reader.execute_batch("COMMIT").unwrap();
    let (_store, applied) = writer.recv_timeout(PATIENCE).unwrap();
    let applied = applied.unwrap();
    // The wait runs from W1, which a loaded machine may start well after
    // the spawn this test timed from; a third of it still proves the wait.
    let waited = BLOCKED / 3;
    assert!(applied.profile.session_lock_wait_nanos >= waited.as_nanos() as u64);
    let report = locks.report();
    assert!(report.streaming.session_lock_wait.max_ms >= waited.as_millis() as f64);
    assert_eq!(report.writer_lock_order_violations, 0);
}

#[test]
fn the_store_writer_tx_and_the_reader_prologue_exclude_each_other() {
    // (d) on the real schema: the store's own zero-row W1 keeps the real
    // prologue out, and the other way round.
    let (mut store, uri, _keeper, _dir) = shared("d-store");
    let reader = attach(&uri);
    let tx = store.writer_tx().unwrap();
    reader.execute_batch("BEGIN DEFERRED").unwrap();
    assert!(is_locked(step_once(
        &reader,
        lock_order::READER_PROLOGUE,
        Some("")
    )));
    tx.commit().unwrap();
    lock_order::reader_prologue(&reader).unwrap();
    let writer = spawn_writer(store, |store| store.writer_tx().map(|tx| tx.commit()));
    assert!(writer.recv_timeout(BLOCKED).is_err(), "W1 waits for R1");
    reader.execute_batch("COMMIT").unwrap();
    let (_store, committed) = writer.recv_timeout(PATIENCE).unwrap();
    committed.unwrap().unwrap();
}

/// Finalization against the real artifacts list under R1: the list reads
/// `projection_state` first, which only R1's earlier `session` lock keeps
/// from interleaving with the refold's `projection_state` write.
fn list_artifacts(conn: &Connection) {
    let page = rosetta_store::html_artifacts::list(
        conn,
        &rosetta_store::html_artifacts::ArtifactListRequest::default(),
        true,
    );
    assert!(page.is_ok(), "artifacts list failed");
}

#[test]
fn the_artifacts_list_under_the_prologue_never_fails_finalization() {
    // Reader first: finalization's tx A waits on the reader's prologue.
    let (store, uri, _keeper, _dir) = shared("b-reader-first");
    let reader = attach(&uri);
    reader.execute_batch("BEGIN DEFERRED").unwrap();
    lock_order::reader_prologue(&reader).unwrap();
    list_artifacts(&reader);
    let writer = spawn_writer(store, Store::finish_bulk_load);
    assert!(writer.recv_timeout(BLOCKED).is_err(), "tx A waits");
    list_artifacts(&reader);
    reader.execute_batch("COMMIT").unwrap();
    let (_store, finished) = writer.recv_timeout(PATIENCE).unwrap();
    finished.unwrap();

    // Writer first: the list waits at its prologue through tx A, and tx B
    // (DDL) waits for the list's whole transaction.
    let (store, uri, _keeper, _dir) = shared("b-writer-first");
    let pause = Pause::after(store.conn(), |sql| sql == lock_order::SESSION_LOCK);
    let writer = spawn_writer(store, Store::finish_bulk_load);
    pause.reached(PATIENCE).expect("tx A takes W1");
    let (listed_tx, listed) = mpsc::channel();
    let reader = thread::spawn(move || {
        let reader = attach(&uri);
        reader.execute_batch("BEGIN DEFERRED").unwrap();
        lock_order::reader_prologue(&reader).unwrap();
        list_artifacts(&reader);
        listed_tx.send(()).unwrap();
        reader.execute_batch("COMMIT").unwrap();
    });
    assert!(listed.recv_timeout(BLOCKED).is_err(), "R1 waits for tx A");
    pause.release();
    listed.recv_timeout(PATIENCE).unwrap();
    reader.join().unwrap();
    let (store, finished) = writer.recv_timeout(PATIENCE).unwrap();
    untrace(store.conn());
    finished.unwrap();
}

#[test]
fn finalization_ddl_waits_out_a_reader_parked_at_its_prologue() {
    // (c) on the store: a reader parks behind tx A's W1 (holding the schema
    // read lock of its open transaction); tx B's DDL waits for it instead of
    // closing a cycle, and the reader's prologue then proceeds.
    let (store, uri, _keeper, _dir) = shared("c-parked");
    let locks = store.shared_cache_locks().unwrap();
    let pause = Pause::after(store.conn(), |sql| sql == lock_order::SESSION_LOCK);
    let writer = spawn_writer(store, Store::finish_bulk_load);
    pause.reached(PATIENCE).expect("tx A takes W1");
    let reader = attach(&uri);
    reader.execute_batch("BEGIN DEFERRED").unwrap();
    assert!(is_locked(step_once(
        &reader,
        lock_order::READER_PROLOGUE,
        Some("")
    )));
    let notified = register_unlock(&reader).expect("R1 waits on tx A");
    pause.release();
    notified.recv_timeout(PATIENCE).expect("tx A commits");
    // Still parked: tx B must wait on this open read transaction.
    assert!(writer.recv_timeout(BLOCKED).is_err(), "tx B waits");
    assert!(stepped(step_once(
        &reader,
        lock_order::READER_PROLOGUE,
        Some("")
    )));
    assert!(stepped(step_once(
        &reader,
        "SELECT COUNT(*) FROM usage_bucket",
        None
    )));
    reader.execute_batch("COMMIT").unwrap();
    let (store, finished) = writer.recv_timeout(PATIENCE).unwrap();
    untrace(store.conn());
    finished.unwrap();
    let report = locks.report();
    assert_eq!(report.finalize.session_lock_wait.count, 1, "tx A only");
    assert_eq!(report.streaming.session_lock_wait.count, 1, "the metadata");
    assert_eq!(report.writer_lock_order_violations, 0);
}

// ------------------------------------- what the discipline must not change

/// A build exercising every writer transaction the cold paths use: the
/// metadata batch, two closed sources, the admission fold, a renumber, the
/// readiness flip, and finalization.
fn build(store: &mut Store) {
    let mut fold = ColdReconciliation::default();
    let a = closed(store, "a.jsonl");
    let b = closed(store, "b.jsonl");
    store
        .apply_streamed_sources(&mut fold, vec![a.into()])
        .unwrap();
    let usage = cursor(store, "b.jsonl");
    let mut b = b;
    b.ops.push(IngestOp::SessionUsage(
        serde_json::from_value(json!({
            "session_id": "root", "key": format!("usage-{usage}"), "ts": 13,
            "model": "claude-haiku-4-5", "tokens": {"input": 7, "output": 3}
        }))
        .unwrap(),
    ));
    store
        .apply_streamed_sources(&mut fold, vec![b.into()])
        .unwrap();
    store.fold_cold_reconciliation(fold).unwrap();
    store
        .renumber_sessions_by_source_order(&["root".to_string()])
        .unwrap();
    store
        .set_session_stream_states(&[("root".to_string(), rosetta_core::StreamState::Indexed)])
        .unwrap();
    store.finish_bulk_load().unwrap();
}

#[test]
fn only_the_shared_generation_takes_the_session_lock() {
    // Private rebuilds, in memory and on disk, run exactly the statements
    // they always ran: no W1 anywhere.
    let dir = tempfile::tempdir().unwrap();
    for mut store in [
        populated(Store::open_in_memory_for_rebuild().unwrap()),
        populated(Store::open_for_rebuild(&dir.path().join("private.sqlite")).unwrap()),
    ] {
        assert!(store.shared_cache_locks().is_none());
        let recorder = Recorder::on(store.conn());
        build(&mut store);
        untrace(store.conn());
        let statements = recorder.statements();
        assert!(statements.iter().any(|sql| sql.starts_with("CREATE INDEX")));
        assert!(
            !statements.iter().any(|sql| sql == lock_order::SESSION_LOCK),
            "a private store never takes W1"
        );
    }

    // The shared generation opens every writer transaction with W1, except
    // finalization's DDL transaction, which opens with its DDL (W2).
    let (mut store, _uri, _keeper, _dir) = shared("disciplined");
    let recorder = Recorder::on(store.conn());
    build(&mut store);
    untrace(store.conn());
    let statements = recorder.statements();
    let mut opened = Vec::new();
    for pair in statements.windows(2) {
        if pair[0].starts_with("BEGIN") {
            opened.push(pair[1].as_str());
        }
    }
    let ddl = opened
        .iter()
        .filter(|sql| sql.starts_with("CREATE INDEX"))
        .count();
    assert_eq!(ddl, 1, "one DDL-first transaction: {opened:#?}");
    assert!(
        opened
            .iter()
            .all(|sql| *sql == lock_order::SESSION_LOCK || sql.starts_with("CREATE INDEX")),
        "{opened:#?}"
    );
    // Two sources, the fold, a renumber, the flip, and finalize tx A (the
    // metadata batch landed before recording started).
    assert_eq!(opened.len() - ddl, 6, "{opened:#?}");
}

/// Every table, index and ANALYZE statistic, row by row.
fn dump(conn: &Connection) -> Vec<String> {
    let mut out: Vec<String> = conn
        .prepare("SELECT type, name, tbl_name, COALESCE(sql, '') FROM sqlite_schema ORDER BY name")
        .unwrap()
        .query_map([], |row| {
            Ok(format!(
                "schema {} {} {} {}",
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?
            ))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let tables: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_schema WHERE type = 'table' ORDER BY name")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    for table in tables {
        let mut statement = conn.prepare(&format!("SELECT * FROM \"{table}\"")).unwrap();
        let columns: Vec<String> = statement
            .column_names()
            .into_iter()
            .map(str::to_owned)
            .collect();
        let mut rows: Vec<String> = statement
            .query_map([], |row| {
                let mut fields = Vec::new();
                for (index, column) in columns.iter().enumerate() {
                    if table == "projection_state"
                        && (column == "projection_epoch" || column == "artifacts_revision")
                    {
                        continue;
                    }
                    let value: rusqlite::types::Value = row.get(index)?;
                    fields.push(format!("{column}={value:?}"));
                }
                Ok(format!("{table} {}", fields.join(" | ")))
            })
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        rows.sort();
        out.extend(rows);
    }
    out
}

#[test]
fn split_finalization_matches_the_single_transaction_dump() {
    let mut private = populated(Store::open_in_memory_for_rebuild().unwrap());
    build(&mut private);
    let (mut shared, _uri, _keeper, _dir) = shared("split");
    build(&mut shared);
    let (private, shared) = (dump(private.conn()), dump(shared.conn()));
    assert!(private.iter().any(|row| row.starts_with("usage_bucket ")));
    assert!(private.iter().any(|row| row.starts_with("sqlite_stat1 ")));
    assert_eq!(private, shared);
}
