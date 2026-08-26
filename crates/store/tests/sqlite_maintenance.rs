use rusqlite::Connection;

use rosetta_core::IngestOp;
use rosetta_core::rows::{
    MessageKind, MessageRow, ProjectRow, RuntimeRow, SessionRole, SessionRow, StreamState,
    TokenSnapshot,
};
use rosetta_store::{Batch, CheckpointUpdate, SourceMode, SourceWrite, Store, StoreError, queries};

fn runtime() -> RuntimeRow {
    RuntimeRow {
        id: "rt1".into(),
        kind: "test".into(),
        data_root: "/tmp/test".into(),
        enabled: true,
        account: None,
        config: None,
        skills: None,
        versions_seen: None,
        discovered_at: 1,
    }
}

fn session() -> SessionRow {
    SessionRow {
        id: "s1".into(),
        native_session_id: "native-s1".into(),
        runtime_id: "rt1".into(),
        project_id: Some("p1".into()),
        parent_session_id: None,
        root_session_id: "s1".into(),
        role: SessionRole::Main,
        title: None,
        summary: None,
        started_at: 0,
        updated_at: 0,
        stream_state: StreamState::Streaming,
    }
}

fn base_batch() -> Batch {
    Batch {
        ops: vec![
            IngestOp::Runtime(runtime()),
            IngestOp::Project(ProjectRow {
                id: "p1".into(),
                name: "test".into(),
                root_paths: vec!["/tmp/test".into()],
                git_repo: None,
            }),
            IngestOp::SessionMeta(session()),
        ],
        checkpoints: vec![],
        source: None,
    }
}

fn install_cursor(store: &mut Store, path: &str) -> i64 {
    store
        .apply(&Batch {
            ops: vec![],
            checkpoints: vec![CheckpointUpdate {
                runtime_id: "rt1".into(),
                stream: "transcript".into(),
                path: path.into(),
                session_id: "s1".into(),
                expected: None,
                next: "fresh".into(),
                updated_at: None,
            }],
            source: None,
        })
        .unwrap();
    queries::cursor_state(store.conn(), "rt1", path)
        .unwrap()
        .unwrap()
        .id
}

fn message(id: &str, ordinal: i64, ts: i64) -> MessageRow {
    MessageRow {
        timeline_id: id.into(),
        session_id: "s1".into(),
        ordinal,
        kind: MessageKind::User,
        subtype: None,
        uuid: None,
        parent_uuid: None,
        ts: Some(ts),
        model: Some("model-a".into()),
        effort: None,
        content: Some(format!("searchable {id}")),
        tool: None,
        branch_tool_id: None,
        usage: None,
        agent_version: None,
        src_cursor: None,
        src_offset: Some(ordinal),
    }
}

fn query_plan(conn: &Connection, sql: &str) -> Vec<String> {
    let mut statement = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
    statement
        .query_map([], |row| row.get(3))
        .unwrap()
        .collect::<Result<Vec<String>, _>>()
        .unwrap()
}

fn assert_plan_uses(conn: &Connection, sql: &str, index: &str) {
    let plan = query_plan(conn, sql).join(" | ");
    assert!(
        plan.contains(index),
        "expected {index} for `{sql}`, got {plan}"
    );
}

#[test]
fn reconciliation_and_bucket_queries_have_matching_indexes() {
    let store = Store::open_in_memory().unwrap();
    for (table, index) in [
        ("message", "message_src"),
        ("file_edit", "file_edit_src"),
        ("git_commit", "git_commit_src"),
        ("task", "task_src"),
        ("plan", "plan_src"),
        ("workspace_obs", "workspace_obs_src"),
        ("pending_merge", "pending_merge_src"),
    ] {
        assert_plan_uses(
            store.conn(),
            &format!("SELECT 1 FROM {table} WHERE src_cursor = 1 LIMIT 1"),
            index,
        );
    }
    assert_plan_uses(
        store.conn(),
        "SELECT SUM(in_tokens) FROM message
         WHERE session_id = 's1'
           AND COALESCE(ts, 0) >= 0 AND COALESCE(ts, 0) < 3600000
           AND COALESCE(model, 'unknown') = 'model-a'",
        "message_bucket_key",
    );
    let bucket_plan = query_plan(
        store.conn(),
        "SELECT SUM(in_tokens) FROM message
         WHERE session_id = 's1'
           AND COALESCE(ts, 0) >= 0 AND COALESCE(ts, 0) < 3600000
           AND COALESCE(model, 'unknown') = 'model-a'",
    )
    .join(" | ");
    assert!(
        bucket_plan.contains("session_id=? AND <expr>=? AND <expr>>? AND <expr><?"),
        "bucket index must use both equality keys before the time range: {bucket_plan}"
    );
    assert_plan_uses(
        store.conn(),
        "SELECT 1 FROM file_edit WHERE message_id = 1",
        "file_edit_message",
    );
    let duplicate: i64 = store
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM sqlite_schema
             WHERE type = 'index' AND name = 'source_op_cursor_generation'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(duplicate, 0, "the WITHOUT ROWID primary key is sufficient");
}

#[test]
fn one_batch_rebuilds_each_changed_bucket_once() {
    let mut store = Store::open_in_memory().unwrap();
    store.apply(&base_batch()).unwrap();
    let ops = (0..200)
        .map(|ordinal| {
            let mut row = message(&format!("m-{ordinal}"), ordinal, 1_000 + ordinal);
            row.usage = Some(TokenSnapshot {
                output: Some(1),
                ..Default::default()
            });
            IngestOp::Message(Box::new(row))
        })
        .collect();
    let stats = store
        .apply(&Batch {
            ops,
            checkpoints: vec![],
            source: None,
        })
        .unwrap();
    assert_eq!(stats.bucket_rebuilds, 1);
    let (messages, output): (i64, i64) = store
        .conn()
        .query_row(
            "SELECT message_count, out_tokens FROM usage_bucket",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((messages, output), (200, 200));
}

#[test]
fn initial_activation_skips_the_sweep_and_rewrite_sweeps_once() {
    let mut store = Store::open_in_memory().unwrap();
    store.apply(&base_batch()).unwrap();
    store
        .apply(&Batch {
            ops: vec![],
            checkpoints: vec![CheckpointUpdate {
                runtime_id: "rt1".into(),
                stream: "transcript".into(),
                path: "session.jsonl".into(),
                session_id: "s1".into(),
                expected: None,
                next: "fresh".into(),
                updated_at: None,
            }],
            source: None,
        })
        .unwrap();
    let cursor = queries::cursor_state(store.conn(), "rt1", "session.jsonl")
        .unwrap()
        .unwrap();

    let mut first = message("first", 0, 1_000);
    first.src_cursor = Some(cursor.id);
    let first_stats = store
        .apply(&Batch {
            ops: vec![IngestOp::Message(Box::new(first))],
            checkpoints: vec![],
            source: Some(SourceWrite {
                cursor_id: cursor.id,
                session_id: "s1".into(),
                generation: 1,
                first_sequence: 0,
                mode: SourceMode::Activate,
            }),
        })
        .unwrap();
    assert_eq!(first_stats.source_sweeps, 0);
    assert_eq!(first_stats.bucket_rebuilds, 1);

    let mut second = message("second", 0, 1_000);
    second.src_cursor = Some(cursor.id);
    let rewrite_stats = store
        .apply(&Batch {
            ops: vec![IngestOp::Message(Box::new(second))],
            checkpoints: vec![],
            source: Some(SourceWrite {
                cursor_id: cursor.id,
                session_id: "s1".into(),
                generation: 2,
                first_sequence: 0,
                mode: SourceMode::Activate,
            }),
        })
        .unwrap();
    assert_eq!(rewrite_stats.source_sweeps, 1);
    assert_eq!(rewrite_stats.bucket_rebuilds, 1);
    assert!(
        queries::search(store.conn(), "first", 10)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        queries::search(store.conn(), "second", 10).unwrap().len(),
        1
    );
}

#[test]
fn cold_rebuild_defers_and_then_materializes_derived_structures() {
    let mut store = Store::open_in_memory_for_rebuild().unwrap();
    store.apply(&base_batch()).unwrap();

    let mut before_epoch = message("before-epoch", 0, -1);
    before_epoch.usage = Some(TokenSnapshot {
        input: Some(7),
        ..Default::default()
    });
    let mut tool_without_native_id = message("tool-call", 1, 10);
    tool_without_native_id.kind = MessageKind::ToolUse;
    store
        .apply(&Batch {
            ops: vec![
                IngestOp::Message(Box::new(before_epoch)),
                IngestOp::Message(Box::new(tool_without_native_id)),
            ],
            checkpoints: vec![],
            source: None,
        })
        .unwrap();

    assert!(
        queries::search(store.conn(), "before", 10)
            .unwrap()
            .is_empty()
    );
    let buckets: i64 = store
        .conn()
        .query_row("SELECT COUNT(*) FROM usage_bucket", [], |row| row.get(0))
        .unwrap();
    assert_eq!(buckets, 0);
    let deferred: i64 = store
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM sqlite_schema
             WHERE type = 'index' AND name = 'message_bucket_key'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(deferred, 0);

    store.finish_bulk_load().unwrap();
    assert_eq!(
        queries::search(store.conn(), "before", 10).unwrap().len(),
        1
    );
    let (hour, input, messages, tools): (i64, i64, i64, i64) = store
        .conn()
        .query_row(
            "SELECT hour_ts, in_tokens, message_count, tool_call_count
             FROM usage_bucket WHERE hour_ts = -3600000",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!((hour, input, messages, tools), (-3_600_000, 7, 1, 0));
    let positive_tools: i64 = store
        .conn()
        .query_row(
            "SELECT tool_call_count FROM usage_bucket WHERE hour_ts = 0",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        positive_tools, 1,
        "tool kind, not optional native id, defines calls"
    );
    let rebuilt_index: i64 = store
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM sqlite_schema
             WHERE type = 'index' AND name = 'message_bucket_key'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(rebuilt_index, 1);

    let live_stats = store
        .apply(&Batch {
            ops: vec![IngestOp::Message(Box::new(message("live", 2, 20)))],
            checkpoints: vec![],
            source: None,
        })
        .unwrap();
    assert_eq!(live_stats.bucket_rebuilds, 1);
    assert_eq!(queries::search(store.conn(), "live", 10).unwrap().len(), 1);
    assert!(matches!(
        store.finish_bulk_load(),
        Err(StoreError::BulkLoadState { .. })
    ));
}

#[test]
fn cold_first_generation_projects_without_the_source_journal() {
    let mut store = Store::open_in_memory_for_rebuild().unwrap();
    store.apply(&base_batch()).unwrap();
    let cursor_id = install_cursor(&mut store, "cold-first.jsonl");

    let mut first = message("cold-first", 0, 1_000);
    first.src_cursor = Some(cursor_id);
    let staged = store
        .apply(&Batch {
            ops: vec![IngestOp::Message(Box::new(first))],
            checkpoints: vec![],
            source: Some(SourceWrite {
                cursor_id,
                session_id: "s1".into(),
                generation: 1,
                first_sequence: 0,
                mode: SourceMode::Stage,
            }),
        })
        .unwrap();
    assert_eq!(staged.source_stage_nanos, 0);
    assert_eq!(
        store
            .conn()
            .query_row("SELECT COUNT(*) FROM source_op", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        store
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM message WHERE timeline_id = 'cold-first'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        1,
        "the scratch projection may see rows before publication"
    );

    let mut second = message("cold-second", 0, 2_000);
    second.src_cursor = Some(cursor_id);
    let activated = store
        .apply(&Batch {
            ops: vec![IngestOp::Message(Box::new(second))],
            checkpoints: vec![],
            source: Some(SourceWrite {
                cursor_id,
                session_id: "s1".into(),
                generation: 1,
                first_sequence: 1,
                mode: SourceMode::Activate,
            }),
        })
        .unwrap();
    assert_eq!(activated.source_stage_nanos, 0);
    assert_eq!(
        store
            .conn()
            .query_row("SELECT COUNT(*) FROM message", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        2
    );
    let cursor = queries::cursor_state(store.conn(), "rt1", "cold-first.jsonl")
        .unwrap()
        .unwrap();
    assert_eq!(
        (cursor.active_generation, cursor.highest_generation),
        (1, 1)
    );

    store.finish_bulk_load().unwrap();
    assert_eq!(
        queries::search(store.conn(), "searchable", 10)
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn cold_retry_before_first_activation_sweeps_the_incomplete_generation() {
    let mut store = Store::open_in_memory_for_rebuild().unwrap();
    store.apply(&base_batch()).unwrap();
    let cursor_id = install_cursor(&mut store, "cold-retry.jsonl");

    let mut stale = message("cold-stale", 0, 1_000);
    stale.src_cursor = Some(cursor_id);
    store
        .apply(&Batch {
            ops: vec![IngestOp::Message(Box::new(stale))],
            checkpoints: vec![],
            source: Some(SourceWrite {
                cursor_id,
                session_id: "s1".into(),
                generation: 1,
                first_sequence: 0,
                mode: SourceMode::Stage,
            }),
        })
        .unwrap();

    let mut fresh = message("cold-fresh", 0, 2_000);
    fresh.src_cursor = Some(cursor_id);
    let restarted = store
        .apply(&Batch {
            ops: vec![IngestOp::Message(Box::new(fresh))],
            checkpoints: vec![],
            source: Some(SourceWrite {
                cursor_id,
                session_id: "s1".into(),
                generation: 2,
                first_sequence: 0,
                mode: SourceMode::Stage,
            }),
        })
        .unwrap();
    assert_eq!(restarted.source_sweeps, 1);
    let (stale, fresh): (i64, i64) = store
        .conn()
        .query_row(
            "SELECT
               SUM(timeline_id = 'cold-stale'),
               SUM(timeline_id = 'cold-fresh')
             FROM message",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((stale, fresh), (0, 1));

    store
        .apply(&Batch {
            ops: vec![],
            checkpoints: vec![],
            source: Some(SourceWrite {
                cursor_id,
                session_id: "s1".into(),
                generation: 2,
                first_sequence: 1,
                mode: SourceMode::Activate,
            }),
        })
        .unwrap();
    store.finish_bulk_load().unwrap();
}

#[test]
fn cold_replacement_after_activation_still_swaps_from_the_journal() {
    let mut store = Store::open_in_memory_for_rebuild().unwrap();
    store.apply(&base_batch()).unwrap();
    let cursor_id = install_cursor(&mut store, "cold-rewrite.jsonl");

    let mut old = message("coldold", 0, 1_000);
    old.src_cursor = Some(cursor_id);
    store
        .apply(&Batch {
            ops: vec![IngestOp::Message(Box::new(old))],
            checkpoints: vec![],
            source: Some(SourceWrite {
                cursor_id,
                session_id: "s1".into(),
                generation: 1,
                first_sequence: 0,
                mode: SourceMode::Activate,
            }),
        })
        .unwrap();

    let mut replacement = message("coldreplacement", 0, 2_000);
    replacement.src_cursor = Some(cursor_id);
    store
        .apply(&Batch {
            ops: vec![IngestOp::Message(Box::new(replacement))],
            checkpoints: vec![],
            source: Some(SourceWrite {
                cursor_id,
                session_id: "s1".into(),
                generation: 2,
                first_sequence: 0,
                mode: SourceMode::Stage,
            }),
        })
        .unwrap();
    let (old_visible, replacement_visible, staged): (i64, i64, i64) = store
        .conn()
        .query_row(
            "SELECT
               (SELECT COUNT(*) FROM message WHERE timeline_id = 'coldold'),
               (SELECT COUNT(*) FROM message WHERE timeline_id = 'coldreplacement'),
               (SELECT COUNT(*) FROM source_op)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!((old_visible, replacement_visible, staged), (1, 0, 1));

    let activated = store
        .apply(&Batch {
            ops: vec![],
            checkpoints: vec![],
            source: Some(SourceWrite {
                cursor_id,
                session_id: "s1".into(),
                generation: 2,
                first_sequence: 1,
                mode: SourceMode::Activate,
            }),
        })
        .unwrap();
    assert_eq!(activated.source_sweeps, 1);
    assert_eq!(
        store
            .conn()
            .query_row("SELECT COUNT(*) FROM source_op", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    store.finish_bulk_load().unwrap();
    assert!(
        queries::search(store.conn(), "coldold", 10)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        queries::search(store.conn(), "coldreplacement", 10)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn abandoned_cold_generation_cannot_be_finalized() {
    let mut store = Store::open_in_memory_for_rebuild().unwrap();
    store.apply(&base_batch()).unwrap();
    let cursor_id = install_cursor(&mut store, "cold-abandon.jsonl");
    let mut partial = message("cold-partial", 0, 1_000);
    partial.src_cursor = Some(cursor_id);
    store
        .apply(&Batch {
            ops: vec![IngestOp::Message(Box::new(partial))],
            checkpoints: vec![],
            source: Some(SourceWrite {
                cursor_id,
                session_id: "s1".into(),
                generation: 1,
                first_sequence: 0,
                mode: SourceMode::Stage,
            }),
        })
        .unwrap();
    store
        .apply(&Batch {
            ops: vec![],
            checkpoints: vec![],
            source: Some(SourceWrite {
                cursor_id,
                session_id: "s1".into(),
                generation: 1,
                first_sequence: 1,
                mode: SourceMode::Abandon,
            }),
        })
        .unwrap();
    assert_eq!(
        store
            .conn()
            .query_row("SELECT COUNT(*) FROM message", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert!(matches!(
        store.finish_bulk_load(),
        Err(StoreError::BulkLoadState { .. })
    ));
}

#[test]
fn incomplete_cold_generation_cannot_be_finalized() {
    let mut store = Store::open_in_memory_for_rebuild().unwrap();
    store.apply(&base_batch()).unwrap();
    let cursor_id = install_cursor(&mut store, "cold-incomplete.jsonl");
    let mut partial = message("cold-incomplete", 0, 1_000);
    partial.src_cursor = Some(cursor_id);
    store
        .apply(&Batch {
            ops: vec![IngestOp::Message(Box::new(partial))],
            checkpoints: vec![],
            source: Some(SourceWrite {
                cursor_id,
                session_id: "s1".into(),
                generation: 1,
                first_sequence: 0,
                mode: SourceMode::Stage,
            }),
        })
        .unwrap();
    assert!(matches!(
        store.finish_bulk_load(),
        Err(StoreError::BulkLoadState { .. })
    ));
}

#[test]
fn cold_rebuild_refuses_an_existing_projection() {
    let database = tempfile::NamedTempFile::new().unwrap();
    drop(Store::open(database.path()).unwrap());
    assert!(matches!(
        Store::open_for_rebuild(database.path()),
        Err(StoreError::BulkLoadState { .. })
    ));
}

#[test]
fn file_backed_cold_rebuild_restores_live_pragmas() {
    let database = tempfile::NamedTempFile::new().unwrap();
    let mut store = Store::open_for_rebuild(database.path()).unwrap();
    store.finish_bulk_load().unwrap();

    let journal: String = store
        .conn()
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .unwrap();
    let synchronous: i64 = store
        .conn()
        .query_row("PRAGMA synchronous", [], |row| row.get(0))
        .unwrap();
    assert_eq!(journal, "wal");
    assert_eq!(synchronous, 1, "NORMAL is SQLite synchronous level 1");
}
