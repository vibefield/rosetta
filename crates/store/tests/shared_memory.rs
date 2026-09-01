use rosetta_core::IngestOp;
use rosetta_core::rows::RuntimeRow;
use rosetta_store::{Batch, Store};
use rusqlite::{Connection, OpenFlags};
use std::time::{Duration, Instant};

fn uri() -> String {
    "file::memory:?cache=shared".to_string()
}

#[test]
fn named_memory_projection_is_visible_to_committed_readers() {
    let dir = tempfile::tempdir().unwrap();
    let projection = dir.path().join("published.sqlite");
    let uri = uri();
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_URI;
    let raw_writer = Connection::open_with_flags(&uri, flags).unwrap();
    raw_writer
        .execute("CREATE TABLE raw_probe (n)", [])
        .unwrap();
    let raw_reader = Connection::open_with_flags(&uri, flags).unwrap();
    raw_reader
        .query_row("SELECT COUNT(*) FROM raw_probe", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap();
    drop(raw_reader);
    drop(raw_writer);
    let mut writer =
        Store::open_shared_in_memory_for_rebuild_with_assets(&uri, &projection).unwrap();
    assert!(writer.is_transient_memory());
    writer
        .apply(&Batch {
            ops: vec![IngestOp::Runtime(RuntimeRow {
                id: "runtime-1".into(),
                kind: "test-agent".into(),
                data_root: "/tmp/test-agent".into(),
                enabled: true,
                account: None,
                config: None,
                skills: None,
                versions_seen: None,
                discovered_at: 1,
            })],
            ..Batch::default()
        })
        .unwrap();

    let reader = Connection::open_with_flags(&uri, flags).unwrap();
    let kind: String = reader
        .query_row(
            "SELECT kind FROM agent_runtime WHERE id = 'runtime-1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(kind, "test-agent");

    writer
        .apply(&Batch {
            ops: vec![IngestOp::Runtime(RuntimeRow {
                id: "runtime-2".into(),
                kind: "second-agent".into(),
                data_root: "/tmp/second-agent".into(),
                enabled: true,
                account: None,
                config: None,
                skills: None,
                versions_seen: None,
                discovered_at: 2,
            })],
            ..Batch::default()
        })
        .unwrap();
    let count: i64 = reader
        .query_row("SELECT COUNT(*) FROM agent_runtime", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 2, "a standing reader sees later committed batches");

    let (locked_tx, locked_rx) = std::sync::mpsc::channel();
    let writer = std::thread::spawn(move || {
        writer.conn().execute_batch("BEGIN IMMEDIATE").unwrap();
        writer
            .conn()
            .execute(
                "UPDATE agent_runtime SET kind = 'after-commit' WHERE id = 'runtime-2'",
                [],
            )
            .unwrap();
        locked_tx.send(()).unwrap();
        std::thread::sleep(Duration::from_millis(75));
        writer.conn().execute_batch("COMMIT").unwrap();
    });
    locked_rx.recv().unwrap();
    let started = Instant::now();
    let kind: String = reader
        .query_row(
            "SELECT kind FROM agent_runtime WHERE id = 'runtime-2'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(kind, "after-commit");
    assert!(
        started.elapsed() >= Duration::from_millis(40),
        "the reader must wait for a coherent commit, not observe dirty RAM"
    );
    writer.join().unwrap();
}
