//! SqliteSnapshot driver integration: a fake database-first agent (the shape
//! of opencode/hermes) proves the whole path — session enumeration at meta,
//! virtual per-session sources, stamp cheap-skips, per-session generation
//! swaps on content change, vanished-session sweeps, and rebuild from the
//! ledger with no SQLite involvement at all.

use std::any::Any;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use rosetta_adapter_support::{
    AgentAdapter, FileStat, GroundTruthDoc, LineOutcome, LineStatus, NormalizeCtx, SkimResult,
    SnapshotPlan, SnapshotQuery, StreamDriver,
};
use rosetta_capture::{Ledger, ScriptedEvents, db_stamp};
use rosetta_core::rows::{MessageKind, MessageRow, RuntimeRow};
use rosetta_core::{IngestOp, ids};
use rosetta_pipeline::{
    IngestOptions, Instrument, LiveOptions, LiveRunner, Metric, discover, expand_data_root,
    meta_ingest_into, rebuild_runtime, stream_backfill_into,
};
use rosetta_store::{Store, queries};
use tempfile::TempDir;

fn assert_ram_only_reconciliation(store: &Store) {
    let private_objects: i64 = store
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM sqlite_schema
             WHERE name IN (
               'source_op', 'pending_merge', 'tool_update_fact', 'session_title_fact'
             )",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(private_objects, 0);
}

struct FakeDbAgent;

impl AgentAdapter for FakeDbAgent {
    fn kind(&self) -> &'static str {
        "fakedb"
    }

    fn layout(&self) -> &GroundTruthDoc {
        static LAYOUT: OnceLock<GroundTruthDoc> = OnceLock::new();
        LAYOUT.get_or_init(|| {
            GroundTruthDoc::parse(
                r#"{"agent":"fakedb","data_root":"~/.fakedb","streams":[
                    {"id":"db","glob":"agent.db*","file_format":"sqlite",
                     "discipline":"database","evidence":"fixture"}]}"#,
            )
            .unwrap()
        })
    }

    fn stream_driver(&self, stream_id: &str) -> Option<StreamDriver> {
        (stream_id == "db").then_some(StreamDriver::SqliteSnapshot)
    }

    fn normalizer_version(&self) -> u32 {
        1
    }

    fn snapshot_plan(&self, stream_id: &str) -> Option<SnapshotPlan> {
        (stream_id == "db").then(|| SnapshotPlan {
            sessions_sql: "SELECT id AS native_session_id, title, cwd, updated_at
                           FROM sessions ORDER BY id"
                .to_string(),
            record_queries: vec![SnapshotQuery {
                name: "messages".to_string(),
                sql: "SELECT id, role, body FROM messages WHERE session = ?1 ORDER BY id"
                    .to_string(),
            }],
        })
    }

    fn skim(&self, _stream_id: &str, _stat: &FileStat, _head: &[u8], _tail: &[u8]) -> SkimResult {
        SkimResult::default()
    }

    fn init_state(&self) -> Option<Box<dyn Any + Send>> {
        None
    }

    fn ingest_line(&self, ctx: &mut NormalizeCtx, _stream_id: &str, line: &[u8]) -> LineOutcome {
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(line) else {
            return LineOutcome {
                ops: vec![],
                status: LineStatus::HardError {
                    code: "invalid_json".to_string(),
                },
            };
        };
        if value.get("query").and_then(|v| v.as_str()) != Some("messages") {
            return LineOutcome {
                ops: vec![],
                status: LineStatus::UnknownType,
            };
        }
        let row = &value["row"];
        let id = row["id"].as_i64().unwrap_or_default();
        if row["role"].as_str() == Some("broken") {
            return LineOutcome {
                ops: vec![],
                status: LineStatus::HardError {
                    code: "broken_fixture_row".to_string(),
                },
            };
        }
        let kind = match row["role"].as_str() {
            Some("user") => MessageKind::User,
            _ => MessageKind::Agent,
        };
        let message = MessageRow {
            timeline_id: ids::timeline_id(&ctx.session_id, &format!("msg-{id}")),
            session_id: ctx.session_id.clone(),
            ordinal: ctx.take_ordinal(),
            kind,
            subtype: None,
            uuid: Some(format!("msg-{id}")),
            parent_uuid: None,
            ts: Some(id * 1_000),
            model: None,
            effort: None,
            content: row["body"].as_str().map(str::to_owned),
            tool: None,
            attachments: Vec::new(),
            citations: Vec::new(),
            group: None,
            response: None,
            block_index: None,
            branch_tool_id: None,
            usage: None,
            agent_version: ctx.agent_version.clone(),
            src_cursor: ctx.src_cursor,
            src_offset: ctx.src_offset,
        };
        LineOutcome::parsed(IngestOp::project_message(message))
    }
}

fn seed_db(path: &Path) -> rusqlite::Connection {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch(
        "CREATE TABLE sessions (id TEXT PRIMARY KEY, title TEXT, cwd TEXT, updated_at INTEGER);
         CREATE TABLE messages (id INTEGER PRIMARY KEY, session TEXT, role TEXT, body TEXT);
         INSERT INTO sessions VALUES ('sess-a', 'Alpha work', '/w/alpha', 1000);
         INSERT INTO sessions VALUES ('sess-b', 'Beta work', '/w/beta', 2000);
         INSERT INTO messages VALUES (1, 'sess-a', 'user', 'ometeotl first prompt');
         INSERT INTO messages VALUES (2, 'sess-a', 'agent', 'ometeotl first reply');
         INSERT INTO messages VALUES (3, 'sess-b', 'user', 'xolotl beta prompt');",
    )
    .unwrap();
    conn
}

struct Harness {
    _home: TempDir,
    root: PathBuf,
    db: PathBuf,
    runtime: RuntimeRow,
    store: Store,
    opts: IngestOptions,
    instrument: Instrument,
}

fn harness() -> Harness {
    let adapter = &FakeDbAgent;
    let home = TempDir::new().unwrap();
    let root = expand_data_root(&adapter.layout().data_root, home.path());
    std::fs::create_dir_all(&root).unwrap();
    let db = root.join("agent.db");
    drop(seed_db(&db));

    let opts = IngestOptions::default();
    let instrument = Instrument::new();
    let discoveries = discover(&[adapter], home.path(), &opts, &instrument).unwrap();
    assert!(discoveries[0].installed);
    let mut runtime = discoveries[0].runtime.clone();
    runtime.enabled = true;

    Harness {
        _home: home,
        root,
        db,
        runtime,
        store: Store::open_in_memory().unwrap(),
        opts,
        instrument,
    }
}

#[test]
fn database_sessions_flow_end_to_end() {
    let adapter = &FakeDbAgent;
    let mut h = harness();

    let (meta, files) = meta_ingest_into(
        adapter,
        &h.runtime,
        &h.root,
        &mut h.store,
        &h.opts,
        &h.instrument,
        None,
    )
    .unwrap();
    assert_eq!(meta.sessions, 2, "one session per db row, not per file");
    assert_eq!(meta.projects, 2);
    assert_eq!(files.len(), 2);
    assert!(files.iter().all(|file| file.relative_path.contains('#')));

    let stream = stream_backfill_into(
        adapter,
        &h.runtime,
        &h.root,
        &mut h.store,
        &files,
        &h.opts,
        &h.instrument,
        None,
    )
    .unwrap();
    assert_eq!(stream.records, 3);
    assert_eq!(stream.stats.messages_inserted, 3);
    assert_eq!(stream.hard_errors, 0);
    assert_eq!(stream.sessions_indexed, 2);
    assert_eq!(
        h.instrument.snapshot().get(Metric::PayloadBytes),
        stream.payload_bytes
    );
    assert_eq!(h.instrument.snapshot().get(Metric::SessionsDiscovered), 2);
    assert_eq!(
        queries::search(h.store.conn(), "ometeotl", 5)
            .unwrap()
            .len(),
        2
    );

    // Untouched declared content: a consistent backup is still audited, but
    // the revision check writes nothing.
    let quiet = stream_backfill_into(
        adapter,
        &h.runtime,
        &h.root,
        &mut h.store,
        &files,
        &h.opts,
        &h.instrument,
        None,
    )
    .unwrap();
    assert_eq!(quiet.batches, 0, "equal stamps must write nothing");
    assert_eq!(quiet.records, 0);

    // Same-size rewrite with the original mtime: metadata is deliberately
    // identical, yet the full declared-content audit must detect it.
    let metadata = std::fs::metadata(&h.db).unwrap();
    let modified = metadata.modified().unwrap();
    let before_stamp = db_stamp(&h.db).unwrap().unwrap();
    let conn = rusqlite::Connection::open(&h.db).unwrap();
    conn.execute(
        "UPDATE messages SET body = ?1 WHERE id = 2",
        ["changed same payload"],
    )
    .unwrap();
    drop(conn);
    assert_eq!(std::fs::metadata(&h.db).unwrap().len(), metadata.len());
    std::fs::File::options()
        .write(true)
        .open(&h.db)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(modified))
        .unwrap();
    assert_eq!(db_stamp(&h.db).unwrap().unwrap(), before_stamp);
    let audited = stream_backfill_into(
        adapter,
        &h.runtime,
        &h.root,
        &mut h.store,
        &files,
        &h.opts,
        &h.instrument,
        None,
    )
    .unwrap();
    assert_eq!(audited.generations_restarted, 1);
    assert_eq!(
        queries::search(h.store.conn(), "payload", 5).unwrap().len(),
        1
    );

    // Session A changes (an edit and an append); B's content is unchanged.
    std::thread::sleep(std::time::Duration::from_millis(5));
    let conn = rusqlite::Connection::open(&h.db).unwrap();
    conn.execute_batch(
        "UPDATE messages SET body = 'ometeotl corrected reply' WHERE id = 2;
         INSERT INTO messages VALUES (4, 'sess-a', 'user', 'ometeotl follow-up');
         UPDATE sessions SET updated_at = 5000 WHERE id = 'sess-a';",
    )
    .unwrap();
    drop(conn);

    let changed = stream_backfill_into(
        adapter,
        &h.runtime,
        &h.root,
        &mut h.store,
        &files,
        &h.opts,
        &h.instrument,
        None,
    )
    .unwrap();
    assert_eq!(
        changed.generations_restarted, 1,
        "only session A swaps generations"
    );
    assert_eq!(changed.records, 3, "only A's records are re-normalized");
    let total: i64 = h
        .store
        .conn()
        .query_row("SELECT COUNT(*) FROM message", [], |row| row.get(0))
        .unwrap();
    assert_eq!(total, 4);
    assert_eq!(
        queries::search(h.store.conn(), "corrected", 5)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        queries::search(h.store.conn(), "reply", 5).unwrap().len(),
        1,
        "the old body must be swept from FTS"
    );

    // Session B vanishes from the database entirely: an empty activation
    // sweeps its projection.
    std::thread::sleep(std::time::Duration::from_millis(5));
    let conn = rusqlite::Connection::open(&h.db).unwrap();
    conn.execute_batch(
        "DELETE FROM messages WHERE session = 'sess-b';
         DELETE FROM sessions WHERE id = 'sess-b';",
    )
    .unwrap();
    drop(conn);
    let (meta, current_files) = meta_ingest_into(
        adapter,
        &h.runtime,
        &h.root,
        &mut h.store,
        &h.opts,
        &h.instrument,
        None,
    )
    .unwrap();
    assert_eq!(meta.sessions, 1);
    assert_eq!(
        current_files.len(),
        2,
        "the removed session remains a tombstone"
    );
    let swept = stream_backfill_into(
        adapter,
        &h.runtime,
        &h.root,
        &mut h.store,
        &current_files,
        &h.opts,
        &h.instrument,
        None,
    )
    .unwrap();
    assert!(swept.generations_restarted >= 1);
    assert_eq!(
        queries::search(h.store.conn(), "xolotl", 5).unwrap().len(),
        0
    );
    assert_ram_only_reconciliation(&h.store);
}

#[test]
fn database_glob_auxiliary_files_are_inventory_only() {
    let adapter = &FakeDbAgent;
    let mut h = harness();
    std::fs::write(h.root.join("agent.db.fts_rebuild.lock"), b"locked").unwrap();

    let (meta, files) = meta_ingest_into(
        adapter,
        &h.runtime,
        &h.root,
        &mut h.store,
        &h.opts,
        &h.instrument,
        None,
    )
    .unwrap();

    assert_eq!(meta.files_matched, 2);
    assert_eq!(meta.files_inventory_only, 1);
    assert_eq!(meta.files_vanished, 0);
    assert_eq!(meta.sessions, 2);
    assert_eq!(files.len(), 2);
    assert!(
        files
            .iter()
            .all(|file| file.relative_path.starts_with("agent.db#"))
    );
}

#[test]
fn live_database_hint_reenumerates_virtual_session_membership() {
    let adapter = &FakeDbAgent;
    let mut h = harness();
    let (_, files) = meta_ingest_into(
        adapter,
        &h.runtime,
        &h.root,
        &mut h.store,
        &h.opts,
        &h.instrument,
        None,
    )
    .unwrap();
    stream_backfill_into(
        adapter,
        &h.runtime,
        &h.root,
        &mut h.store,
        &files,
        &h.opts,
        &h.instrument,
        None,
    )
    .unwrap();

    let mut events = ScriptedEvents::default();
    events.push_changed("agent.db");
    let mut runner = LiveRunner::start(
        adapter,
        h.runtime.clone(),
        h.root.clone(),
        Box::new(events),
        &mut h.store,
        h.opts.clone(),
        LiveOptions {
            tick: Duration::ZERO,
            poll_interval: Duration::from_secs(3600),
            max_coalesced: 8,
        },
        &h.instrument,
    )
    .unwrap();

    let connection = rusqlite::Connection::open(&h.db).unwrap();
    connection
        .execute_batch(
            "INSERT INTO sessions VALUES ('sess-c', 'Gamma work', '/w/gamma', 9000);
             INSERT INTO messages VALUES (9, 'sess-c', 'user', 'newly admitted tezcatlipoca');",
        )
        .unwrap();
    drop(connection);
    runner.tick(&mut h.store, &h.instrument).unwrap();

    assert_eq!(runner.report().admissions, 1);
    assert_eq!(
        queries::search(h.store.conn(), "tezcatlipoca", 5)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn deleted_database_sweeps_every_virtual_member() {
    let adapter = &FakeDbAgent;
    let mut h = harness();
    let (_, files) = meta_ingest_into(
        adapter,
        &h.runtime,
        &h.root,
        &mut h.store,
        &h.opts,
        &h.instrument,
        None,
    )
    .unwrap();
    stream_backfill_into(
        adapter,
        &h.runtime,
        &h.root,
        &mut h.store,
        &files,
        &h.opts,
        &h.instrument,
        None,
    )
    .unwrap();
    assert_eq!(
        queries::search(h.store.conn(), "ometeotl", 5)
            .unwrap()
            .len(),
        2
    );

    std::fs::remove_file(&h.db).unwrap();
    let (_, tombstones) = meta_ingest_into(
        adapter,
        &h.runtime,
        &h.root,
        &mut h.store,
        &h.opts,
        &h.instrument,
        None,
    )
    .unwrap();
    assert_eq!(tombstones.len(), 2);
    assert!(tombstones.iter().all(|file| file.source_missing));
    let swept = stream_backfill_into(
        adapter,
        &h.runtime,
        &h.root,
        &mut h.store,
        &tombstones,
        &h.opts,
        &h.instrument,
        None,
    )
    .unwrap();
    assert!(swept.generations_restarted >= 2);
    assert!(
        queries::search(h.store.conn(), "ometeotl", 5)
            .unwrap()
            .is_empty()
    );
    assert!(
        queries::search(h.store.conn(), "xolotl", 5)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn live_database_delete_hint_reconciles_membership() {
    let adapter = &FakeDbAgent;
    let mut h = harness();
    let (_, files) = meta_ingest_into(
        adapter,
        &h.runtime,
        &h.root,
        &mut h.store,
        &h.opts,
        &h.instrument,
        None,
    )
    .unwrap();
    stream_backfill_into(
        adapter,
        &h.runtime,
        &h.root,
        &mut h.store,
        &files,
        &h.opts,
        &h.instrument,
        None,
    )
    .unwrap();

    let mut events = ScriptedEvents::default();
    events.push_changed("agent.db");
    let mut runner = LiveRunner::start(
        adapter,
        h.runtime.clone(),
        h.root.clone(),
        Box::new(events),
        &mut h.store,
        h.opts.clone(),
        LiveOptions {
            tick: Duration::ZERO,
            poll_interval: Duration::from_secs(3600),
            max_coalesced: 8,
        },
        &h.instrument,
    )
    .unwrap();
    std::fs::remove_file(&h.db).unwrap();
    runner.tick(&mut h.store, &h.instrument).unwrap();

    assert_eq!(runner.report().admissions, 1);
    assert!(
        queries::search(h.store.conn(), "ometeotl", 5)
            .unwrap()
            .is_empty()
    );
    assert!(
        queries::search(h.store.conn(), "xolotl", 5)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn repeated_hard_snapshot_generations_are_abandoned_without_staging_growth() {
    let adapter = &FakeDbAgent;
    let mut h = harness();
    let (_, files) = meta_ingest_into(
        adapter,
        &h.runtime,
        &h.root,
        &mut h.store,
        &h.opts,
        &h.instrument,
        None,
    )
    .unwrap();
    stream_backfill_into(
        adapter,
        &h.runtime,
        &h.root,
        &mut h.store,
        &files,
        &h.opts,
        &h.instrument,
        None,
    )
    .unwrap();
    let connection = rusqlite::Connection::open(&h.db).unwrap();
    connection
        .execute("UPDATE messages SET role = 'broken' WHERE id = 2", [])
        .unwrap();
    drop(connection);

    let path = files
        .iter()
        .find(|file| file.native_session_id == "sess-a")
        .unwrap()
        .relative_path
        .clone();
    let before = queries::cursor_state(h.store.conn(), &h.runtime.id, &path)
        .unwrap()
        .unwrap();
    for expected_highest in [before.highest_generation + 1, before.highest_generation + 2] {
        let report = stream_backfill_into(
            adapter,
            &h.runtime,
            &h.root,
            &mut h.store,
            &files,
            &h.opts,
            &h.instrument,
            None,
        )
        .unwrap();
        assert_eq!(report.hard_errors, 1);
        assert_ram_only_reconciliation(&h.store);
        let cursor = queries::cursor_state(h.store.conn(), &h.runtime.id, &path)
            .unwrap()
            .unwrap();
        assert_eq!(cursor.active_generation, before.active_generation);
        assert_eq!(cursor.highest_generation, expected_highest);
        assert_eq!(
            queries::search(h.store.conn(), "first reply", 5)
                .unwrap()
                .len(),
            1
        );
    }
}

#[test]
fn rebuild_reproduces_the_database_projection_without_sqlite() {
    let adapter = &FakeDbAgent;
    let mut h = harness();
    let ledger_dir = h.root.parent().unwrap().join("ledger/fakedb");
    let mut ledger = Ledger::open(&ledger_dir).unwrap();

    let (_, files) = meta_ingest_into(
        adapter,
        &h.runtime,
        &h.root,
        &mut h.store,
        &h.opts,
        &h.instrument,
        Some(&mut ledger),
    )
    .unwrap();
    stream_backfill_into(
        adapter,
        &h.runtime,
        &h.root,
        &mut h.store,
        &files,
        &h.opts,
        &h.instrument,
        Some(&mut ledger),
    )
    .unwrap();
    let original: (i64, usize) = (
        h.store
            .conn()
            .query_row("SELECT COUNT(*) FROM message", [], |row| row.get(0))
            .unwrap(),
        queries::session_list(h.store.conn(), None, 100)
            .unwrap()
            .len(),
    );
    assert_eq!(original, (3, 2));

    // The database is gone; the ledger's serialized rows are all that's left.
    std::fs::remove_file(&h.db).unwrap();
    let mut rebuilt = Store::open_in_memory().unwrap();
    let report = rebuild_runtime(
        adapter,
        &h.runtime,
        &ledger_dir,
        &mut rebuilt,
        &h.instrument,
    )
    .unwrap();
    assert_eq!(report.raw_records, 3);
    assert_eq!(report.hard_errors, 0);
    let after: (i64, usize) = (
        rebuilt
            .conn()
            .query_row("SELECT COUNT(*) FROM message", [], |row| row.get(0))
            .unwrap(),
        queries::session_list(rebuilt.conn(), None, 100)
            .unwrap()
            .len(),
    );
    assert_eq!(after, original);
    assert_eq!(
        queries::search(rebuilt.conn(), "ometeotl", 5)
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn shm_touches_are_never_hints_and_wal_touches_are() {
    let adapter = &FakeDbAgent;
    let mut h = harness();
    let (_, files) = meta_ingest_into(
        adapter,
        &h.runtime,
        &h.root,
        &mut h.store,
        &h.opts,
        &h.instrument,
        None,
    )
    .unwrap();

    // The daemon's own snapshot open writes `-shm`; treating that as a hint
    // fed the watcher the daemon's footsteps and starved the cold lanes.
    let shm = rosetta_pipeline::map_hint(adapter, &files, "agent.db-shm");
    assert!(shm.nominated.is_empty(), "-shm nominates nothing");
    assert!(!shm.needs_admission, "-shm admits nothing");

    // `-wal` carries committed content: it aliases to the database and
    // nominates every virtual source the db backs.
    let wal = rosetta_pipeline::map_hint(adapter, &files, "agent.db-wal");
    assert_eq!(wal.nominated.len(), files.len());
    assert!(wal.needs_admission, "content movement re-checks membership");
}

#[test]
fn quiet_database_polls_neither_admit_nor_reopen_the_source() {
    let adapter = &FakeDbAgent;
    let mut h = harness();
    let live = LiveOptions {
        tick: std::time::Duration::ZERO,
        poll_interval: std::time::Duration::ZERO,
        max_coalesced: 16,
    };
    let mut runner = LiveRunner::start(
        adapter,
        h.runtime.clone(),
        h.root.clone(),
        Box::new(rosetta_capture::ScriptedEvents::default()),
        &mut h.store,
        h.opts.clone(),
        live,
        &h.instrument,
    )
    .unwrap();

    // First tick backfills the database (construction meta named it). From
    // then on the corpus is frozen: polls must neither re-run bounded meta
    // (presence is not evidence) nor even OPEN the database — the stamp
    // gate stats it and stops, so the daemon generates no filesystem
    // events for the watcher to feed back.
    runner.tick(&mut h.store, &h.instrument).unwrap();
    let baseline = h.instrument.snapshot();
    for _ in 0..3 {
        runner.tick(&mut h.store, &h.instrument).unwrap();
    }
    let quiet = h.instrument.snapshot().delta_since(&baseline);
    assert_eq!(
        quiet.get(Metric::BytesRead),
        0,
        "a stamp-unchanged database is never copied or opened"
    );
    assert_eq!(
        runner.report().admissions,
        0,
        "presence is not evidence: known membership seeds silently"
    );

    // Content actually moves: the poll notices the stamp drift, re-checks
    // membership once, and the new row lands.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let conn = rusqlite::Connection::open(&h.db).unwrap();
    conn.execute_batch("INSERT INTO messages VALUES (4, 'sess-b', 'agent', 'xolotl beta reply');")
        .unwrap();
    drop(conn);
    runner.tick(&mut h.store, &h.instrument).unwrap();
    runner.tick(&mut h.store, &h.instrument).unwrap();
    assert_eq!(
        runner.report().admissions,
        1,
        "movement admits exactly once"
    );
    assert_eq!(
        queries::search(h.store.conn(), "\"beta reply\"", 5)
            .unwrap()
            .len(),
        1,
        "the moved database's new row is ingested"
    );
}
