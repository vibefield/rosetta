use rusqlite::Connection;

use rosetta_core::rows::{
    MessageKind, MessageRow, ProjectRow, RuntimeRow, SessionRole, SessionRow, StreamState,
    TokenSnapshot,
};
use rosetta_core::{CorrelationDerivation, IngestOp, ToolInteraction};
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
        cwd: Some("/tmp/test".into()),
        parent_session_id: None,
        delegation: None,
        root_session_id: "s1".into(),
        role: SessionRole::Main,
        title: None,
        summary: None,
        started_at: 0,
        updated_at: 0,
        activity: None,
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
                session_id: Some("s1".into()),
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

#[test]
fn cold_planner_retains_payload_without_projecting_canonical_rows() {
    let mut store = Store::open_in_memory_for_cold_planning().unwrap();
    store.apply(&base_batch()).unwrap();
    let cursor_id = install_cursor(&mut store, "session.jsonl");
    let source_batch = Batch {
        ops: vec![message_op(message("planned", 0, 10))],
        checkpoints: vec![CheckpointUpdate {
            runtime_id: "rt1".into(),
            stream: "transcript".into(),
            path: "session.jsonl".into(),
            session_id: Some("s1".into()),
            expected: Some("fresh".into()),
            next: "sealed".into(),
            updated_at: None,
        }],
        source: Some(SourceWrite {
            cursor_id,
            session_id: "s1".into(),
            generation: 1,
            first_sequence: 0,
            mode: SourceMode::Activate,
        }),
    };
    store.apply(&source_batch).unwrap();

    let message_rows: i64 = store
        .conn()
        .query_row("SELECT COUNT(*) FROM message", [], |row| row.get(0))
        .unwrap();
    assert_eq!(message_rows, 0, "planning must not project payload rows");
    let cursor = queries::cursor_state(store.conn(), "rt1", "session.jsonl")
        .unwrap()
        .unwrap();
    assert_eq!(cursor.active_generation, 1);
    assert_eq!(cursor.highest_generation, 1);
    assert_eq!(cursor.checkpoint, "sealed");

    let planned = store.take_planned_batches().unwrap();
    assert_eq!(planned.len(), 3);
    assert_eq!(planned[2].ops, source_batch.ops);
    assert_eq!(
        planned[2].source.as_ref().unwrap().mode,
        SourceMode::Activate
    );
}

#[test]
fn cold_deferred_constraints_fail_closed_before_publication() {
    let mut store = Store::open_in_memory_for_rebuild().unwrap();
    let checks_ignored: bool = store
        .conn()
        .query_row("PRAGMA ignore_check_constraints", [], |row| row.get(0))
        .unwrap();
    assert!(checks_ignored);
    store.apply(&base_batch()).unwrap();
    store
        .conn()
        .execute("UPDATE session SET title_rank = 9 WHERE id = 's1'", [])
        .unwrap();

    let error = store.finish_bulk_load().unwrap_err();
    assert!(
        matches!(error, StoreError::ProjectionInvalid { .. }),
        "finished projection must reject deferred CHECK violations: {error:?}",
    );
    let checks_ignored: bool = store
        .conn()
        .query_row("PRAGMA ignore_check_constraints", [], |row| row.get(0))
        .unwrap();
    assert!(
        !checks_ignored,
        "validation must leave CHECK enforcement on"
    );
}

#[test]
fn cold_deferred_foreign_keys_fail_closed_before_publication() {
    let mut store = Store::open_in_memory_for_rebuild().unwrap();
    let foreign_keys: bool = store
        .conn()
        .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
        .unwrap();
    assert!(!foreign_keys);
    store
        .conn()
        .execute(
            "INSERT INTO session
               (id, native_session_id, runtime_id, root_session_id, started_at, updated_at)
             VALUES ('orphan', 'orphan', 'missing-runtime', 'orphan', 0, 0)",
            [],
        )
        .unwrap();

    let error = store.finish_bulk_load().unwrap_err();
    assert!(
        matches!(error, StoreError::ProjectionInvalid { .. }),
        "finished projection must reject deferred FK violations: {error:?}",
    );
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
        attachments: Vec::new(),
        citations: Vec::new(),
        group: None,
        response: None,
        block_index: None,
        branch_tool_id: None,
        usage: None,
        agent_version: None,
        src_cursor: None,
        src_offset: Some(ordinal),
    }
}

fn message_op(row: MessageRow) -> IngestOp {
    let mut ops = IngestOp::project_message(row);
    assert_eq!(ops.len(), 1);
    ops.pop().unwrap()
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

fn assert_ram_only_reconciliation_schema(conn: &Connection) {
    let private_objects: i64 = conn
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

#[test]
fn reconciliation_and_bucket_queries_have_matching_indexes() {
    let store = Store::open_in_memory().unwrap();
    for (table, index) in [
        ("message", "message_src"),
        ("file_edit", "file_edit_src"),
        ("git_commit", "git_commit_src"),
        ("task", "task_src"),
        ("plan", "plan_src"),
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
    assert_plan_uses(
        store.conn(),
        "SELECT id FROM message
         WHERE session_id = 's1'
           AND tool_correlation_key = 'tool-1'
           AND type = 'tool_use'",
        "message_tool_correlation",
    );
    assert_plan_uses(
        store.conn(),
        "SELECT id FROM message
         WHERE session_id = 's1'
           AND tool_kind IN ('task', 'plan', 'artifact')
         ORDER BY ordinal",
        "timeline_meta_message",
    );
    assert_plan_uses(
        store.conn(),
        "SELECT id FROM task WHERE session_id = 's1' ORDER BY created_at, id",
        "task_session",
    );
    assert_ram_only_reconciliation_schema(store.conn());
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
            message_op(row)
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
                session_id: Some("s1".into()),
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
            ops: vec![message_op(first)],
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
    assert_eq!(first_stats.profile.session_renumbers, 1);

    let mut second = message("second", 0, 1_000);
    second.src_cursor = Some(cursor.id);
    let rewrite_stats = store
        .apply(&Batch {
            ops: vec![message_op(second)],
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
    assert_eq!(rewrite_stats.profile.session_renumbers, 1);
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
    tool_without_native_id.content = None;
    tool_without_native_id.tool = Some(ToolInteraction::unknown(
        "synthetic-tool-call",
        CorrelationDerivation::Synthetic,
        "unknown_fixture_tool",
    ));
    store
        .apply(&Batch {
            ops: vec![message_op(before_epoch), message_op(tool_without_native_id)],
            checkpoints: vec![],
            source: None,
        })
        .unwrap();

    assert!(
        !queries::projection_state(store.conn())
            .unwrap()
            .canonical_ready
    );
    let in_file_fts: i64 = store
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM sqlite_schema WHERE name = 'message_fts'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(in_file_fts, 0, "search is a detached projection");
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
    assert!(
        queries::projection_state(store.conn())
            .unwrap()
            .canonical_ready
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
            ops: vec![message_op(message("live", 2, 20))],
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
fn cold_first_generation_projects_without_serialized_staging() {
    let mut store = Store::open_in_memory_for_rebuild().unwrap();
    store.apply(&base_batch()).unwrap();
    let cursor_id = install_cursor(&mut store, "cold-first.jsonl");

    let mut first = message("cold-first", 0, 1_000);
    first.src_cursor = Some(cursor_id);
    store
        .apply(&Batch {
            ops: vec![message_op(first)],
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
    assert_ram_only_reconciliation_schema(store.conn());
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
            ops: vec![message_op(second)],
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
    assert_eq!(activated.profile.session_renumbers, 0);
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
fn cold_finalize_renumbers_all_sources_in_one_set_pass() {
    let mut store = Store::open_in_memory_for_rebuild().unwrap();
    store.apply(&base_batch()).unwrap();
    let later_path = install_cursor(&mut store, "z-later.jsonl");
    let earlier_path = install_cursor(&mut store, "a-earlier.jsonl");

    for (cursor_id, timeline_id) in [(later_path, "later"), (earlier_path, "earlier")] {
        let mut row = message(timeline_id, 0, 1_000);
        row.src_cursor = Some(cursor_id);
        row.src_offset = Some(0);
        let stats = store
            .apply(&Batch {
                ops: vec![message_op(row)],
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
        assert_eq!(stats.profile.session_renumbers, 0);
    }

    let timeline = |store: &Store| {
        store
            .conn()
            .query_row(
                "SELECT group_concat(timeline_id, ',') FROM
                   (SELECT timeline_id FROM message WHERE session_id = 's1' ORDER BY ordinal)",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap()
    };
    assert_eq!(
        timeline(&store),
        "later,earlier",
        "provisional arrival order"
    );

    let finalized = store.finish_bulk_load().unwrap();
    assert_eq!(finalized.renumber_rows, 2);
    assert_eq!(
        timeline(&store),
        "earlier,later",
        "canonical source-path order"
    );
    assert_eq!(
        store
            .conn()
            .query_row("SELECT COUNT(*) FROM temp.renumber_message", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        0
    );
}

#[test]
fn closed_bulk_sources_keep_precomputed_ordinals_without_a_rewrite() {
    let mut store = Store::open_in_memory_for_rebuild().unwrap();
    store.apply(&base_batch()).unwrap();
    let later_path = install_cursor(&mut store, "z-later.jsonl");
    let earlier_path = install_cursor(&mut store, "a-earlier.jsonl");
    store.use_precomputed_bulk_ordinals().unwrap();

    let batch = |cursor_id, timeline_id, ordinal| {
        let mut row = message(timeline_id, ordinal, 1_000);
        row.src_cursor = Some(cursor_id);
        row.src_offset = Some(0);
        Batch {
            ops: vec![message_op(row)],
            checkpoints: vec![],
            source: Some(SourceWrite {
                cursor_id,
                session_id: "s1".into(),
                generation: 1,
                first_sequence: 0,
                mode: SourceMode::Activate,
            }),
        }
    };
    let stats = store
        .apply_closed_bulk_sources(&[
            batch(later_path, "later", 1),
            batch(earlier_path, "earlier", 0),
        ])
        .unwrap();
    assert_eq!(stats.messages_inserted, 2);
    assert_eq!(stats.profile.ordinal_lookups, 0);
    assert_eq!(
        store
            .conn()
            .query_row(
                "SELECT group_concat(timeline_id, ',') FROM
                   (SELECT timeline_id FROM message ORDER BY ordinal)",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
        "earlier,later"
    );

    let finalized = store.finish_bulk_load().unwrap();
    assert_eq!(finalized.renumber_rows, 0);
    assert_eq!(finalized.renumber_nanos, 0);
}

#[test]
fn closed_bulk_runtime_transaction_rolls_back_every_source_on_failure() {
    let mut store = Store::open_in_memory_for_rebuild().unwrap();
    store.apply(&base_batch()).unwrap();
    let first_cursor = install_cursor(&mut store, "first.jsonl");
    let second_cursor = install_cursor(&mut store, "second.jsonl");
    store.use_precomputed_bulk_ordinals().unwrap();

    let batch = |cursor_id, timeline_id| {
        let mut row = message(timeline_id, 0, 1_000);
        row.src_cursor = Some(cursor_id);
        Batch {
            ops: vec![message_op(row)],
            checkpoints: vec![],
            source: Some(SourceWrite {
                cursor_id,
                session_id: "s1".into(),
                generation: 1,
                first_sequence: 0,
                mode: SourceMode::Activate,
            }),
        }
    };
    let error = store
        .apply_closed_bulk_sources(&[batch(first_cursor, "first"), batch(second_cursor, "second")])
        .unwrap_err();
    assert!(matches!(error, StoreError::Sqlite(_)));
    assert_eq!(
        store
            .conn()
            .query_row("SELECT COUNT(*) FROM message", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    for path in ["first.jsonl", "second.jsonl"] {
        let cursor = queries::cursor_state(store.conn(), "rt1", path)
            .unwrap()
            .unwrap();
        assert_eq!(
            (cursor.active_generation, cursor.highest_generation),
            (0, 0)
        );
    }
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
            ops: vec![message_op(stale)],
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
            ops: vec![message_op(fresh)],
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
fn cold_replacement_after_activation_swaps_from_ram() {
    let mut store = Store::open_in_memory_for_rebuild().unwrap();
    store.apply(&base_batch()).unwrap();
    let cursor_id = install_cursor(&mut store, "cold-rewrite.jsonl");

    let mut old = message("coldold", 0, 1_000);
    old.src_cursor = Some(cursor_id);
    store
        .apply(&Batch {
            ops: vec![message_op(old)],
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
            ops: vec![message_op(replacement)],
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
    let (old_visible, replacement_visible): (i64, i64) = store
        .conn()
        .query_row(
            "SELECT
               (SELECT COUNT(*) FROM message WHERE timeline_id = 'coldold'),
               (SELECT COUNT(*) FROM message WHERE timeline_id = 'coldreplacement')",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((old_visible, replacement_visible), (1, 0));
    assert_ram_only_reconciliation_schema(store.conn());

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
    assert_ram_only_reconciliation_schema(store.conn());
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
            ops: vec![message_op(partial)],
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
            ops: vec![message_op(partial)],
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
fn large_cold_session_defers_ranking_until_finalization() {
    const MESSAGES: i64 = 10_000;

    let mut store = Store::open_in_memory_for_rebuild().unwrap();
    store.apply(&base_batch()).unwrap();
    let cursor_id = install_cursor(&mut store, "cold-large.jsonl");
    let ops = (0..MESSAGES)
        .map(|ordinal| {
            let mut row = message(&format!("cold-large-{ordinal}"), ordinal, ordinal);
            row.src_cursor = Some(cursor_id);
            message_op(row)
        })
        .collect();
    let activation = store
        .apply(&Batch {
            ops,
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
    assert_eq!(activation.profile.session_renumbers, 0);

    let finalized = store.finish_bulk_load().unwrap();
    assert_eq!(finalized.renumber_rows, 0);

    let (count, minimum, maximum, distinct): (i64, i64, i64, i64) = store
        .conn()
        .query_row(
            "SELECT COUNT(*), MIN(ordinal), MAX(ordinal), COUNT(DISTINCT ordinal)
             FROM message WHERE session_id = 's1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(
        (count, minimum, maximum, distinct),
        (MESSAGES, 0, MESSAGES - 1, MESSAGES)
    );
    assert_eq!(
        store
            .conn()
            .query_row("SELECT COUNT(*) FROM temp.renumber_message", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        0
    );
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
