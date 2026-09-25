//! Conversation recency must survive rescans, replay, replacement and child
//! updates without treating source freshness or ingestion state as activity.
use rosetta_core::{IngestOp, MessageRow, SessionActivity, SessionRow};
use rosetta_store::hot::{Effects, MetaEffects, MetaMirror, Mutation};
use rosetta_store::{Batch, CheckpointUpdate, SourceMode, SourceWrite, Store, queries};
use serde_json::json;

fn session(id: &str, parent: Option<&str>) -> SessionRow {
    // Deliberately old wire shape: stored metadata without activity still loads.
    serde_json::from_value(json!({
        "id": id, "native_session_id": id, "runtime_id": "rt",
        "parent_session_id": parent, "root_session_id": parent.unwrap_or(id),
        "role": if parent.is_some() { "subagent" } else { "main" },
        "started_at": 9_000_000, "updated_at": 9_000_000, "stream_state": "streaming"
    }))
    .unwrap()
}

fn init(store: &mut Store) {
    apply(
        store,
        vec![
            IngestOp::Runtime(
                serde_json::from_value(json!({
                    "id": "rt", "kind": "claude-code", "data_root": "/agent",
                    "enabled": true, "discovered_at": 9_000_000
                }))
                .unwrap(),
            ),
            IngestOp::SessionMeta(session("root", None)),
        ],
    );
}

fn apply(store: &mut Store, ops: Vec<IngestOp>) {
    store
        .apply(&Batch {
            ops,
            ..Batch::default()
        })
        .unwrap();
}

fn message(id: &str, session: &str, ordinal: i64, at: Option<i64>) -> MessageRow {
    serde_json::from_value(json!({
        "timeline_id": id, "session_id": session, "ordinal": ordinal,
        "kind": "user", "ts": at, "content": "A real conversation message",
        "attachments": [], "citations": []
    }))
    .unwrap()
}

fn bounds(store: &Store, id: &str) -> Option<SessionActivity> {
    queries::session(store.conn(), id)
        .unwrap()
        .unwrap()
        .activity
}

fn span(first_at: i64, last_at: i64) -> Option<SessionActivity> {
    Some(SessionActivity { first_at, last_at })
}

fn tool_message(cursor_id: i64, at: Option<i64>) -> MessageRow {
    let mut opener = message("tool", "root", 0, at);
    opener.kind = rosetta_core::MessageKind::ToolUse;
    opener.content = None;
    opener.src_cursor = Some(cursor_id);
    opener.src_offset = Some(0);
    opener.tool = Some(
        serde_json::from_value(json!({
            "correlation": {"key": "call", "derivation": "native_id"},
            "native": {"id": "call", "name": "Bash"},
            "status": {"value": "pending", "basis": "request_observed"},
            "action": {"type": "shell", "command": {"type": "inline", "text": "true"},
                "source_truncated": false, "background": false}
        }))
        .unwrap(),
    );
    opener
}

#[test]
fn activity_ignores_metadata_noise_and_usage_only_records() {
    let mut store = Store::open_in_memory().unwrap();
    init(&mut store);
    assert_eq!(bounds(&store, "root"), None);
    apply(
        &mut store,
        IngestOp::project_message(message("old", "root", 0, Some(100))),
    );

    let mut refreshed = session("root", None);
    refreshed.updated_at = 99_000_000;
    refreshed.activity = span(99_000_000, 99_000_000); // metadata cannot assert activity
    let mut noise = message("system", "root", 1, Some(99_000_000));
    noise.kind = rosetta_core::MessageKind::System;
    let mut ops = vec![
        IngestOp::SessionMeta(refreshed),
        IngestOp::SessionUsage(
            serde_json::from_value(json!({
                "session_id": "root", "key": "summary", "ts": 99_000_000,
                "tokens": {"input": 10}
            }))
            .unwrap(),
        ),
    ];
    ops.extend(IngestOp::project_message(noise));
    ops.extend(IngestOp::project_message(message(
        "undated", "root", 2, None,
    )));
    apply(&mut store, ops);
    assert_eq!(bounds(&store, "root"), span(100, 100));

    apply(
        &mut store,
        IngestOp::project_message(message("new", "root", 3, Some(200))),
    );
    assert_eq!(bounds(&store, "root"), span(100, 200));
    apply(
        &mut store,
        vec![IngestOp::SessionMeta(session("unknown", None))],
    );
    assert_eq!(bounds(&store, "unknown"), None);
    let rows = queries::session_list(store.conn(), None, 1).unwrap();
    assert_eq!(
        rows[0].id, "root",
        "rank by activity before applying the page limit"
    );
}

#[test]
fn ancestor_activity_refreshes_on_child_messages_and_parent_refinement() {
    let mut store = Store::open_in_memory().unwrap();
    init(&mut store);
    apply(
        &mut store,
        vec![
            IngestOp::SessionMeta(session("a-parent", None)),
            IngestOp::SessionMeta(session("child", Some("root"))),
            IngestOp::SessionMeta(session("leaf", Some("child"))),
        ],
    );
    apply(
        &mut store,
        IngestOp::project_message(message("parent", "root", 0, Some(100))),
    );
    apply(
        &mut store,
        IngestOp::project_message(message("leaf-1", "leaf", 0, Some(200))),
    );
    let mut mirror = MetaMirror::default();
    mirror.warm(store.conn()).unwrap();
    assert_eq!(mirror.session("root").unwrap().activity, span(100, 200));

    apply(
        &mut store,
        IngestOp::project_message(message("leaf-2", "leaf", 1, Some(300))),
    );
    let effects = Effects {
        touched_sessions: ["leaf".into()].into(),
        ..Effects::default()
    };
    let changed = mirror.refresh(store.conn(), &effects).unwrap();
    assert_eq!(mirror.session("root").unwrap().activity, span(100, 300));
    assert_eq!(mirror.session("child").unwrap().activity, span(200, 300));
    assert!(
        changed
            .iter()
            .any(|event| matches!(event, Mutation::Session(s) if s.id == "root"))
    );

    // Stronger parent refinement moves this subtree; the old root must lose
    // its contribution even though only the child was named in the effects.
    apply(
        &mut store,
        vec![IngestOp::SessionMeta(session("child", Some("a-parent")))],
    );
    mirror
        .refresh_meta(
            store.conn(),
            &MetaEffects {
                sessions: ["child".into()].into(),
                ..MetaEffects::default()
            },
        )
        .unwrap();
    assert_eq!(mirror.session("root").unwrap().activity, span(100, 100));
    assert_eq!(mirror.session("a-parent").unwrap().activity, span(200, 300));
}

fn cursor(store: &mut Store) -> i64 {
    store
        .apply(&Batch {
            checkpoints: vec![CheckpointUpdate {
                runtime_id: "rt".into(),
                stream: "transcript".into(),
                path: "source.jsonl".into(),
                session_id: Some("root".into()),
                expected: None,
                next: "fresh".into(),
                updated_at: Some(99_000_000),
            }],
            ..Batch::default()
        })
        .unwrap();
    queries::cursor_state(store.conn(), "rt", "source.jsonl")
        .unwrap()
        .unwrap()
        .id
}

fn generation(store: &mut Store, cursor_id: i64, generation: u64, at: Option<i64>) {
    let ops = at
        .map(|at| {
            let mut row = message("source-row", "root", 1, Some(at));
            row.src_cursor = Some(cursor_id);
            row.src_offset = Some(0);
            IngestOp::project_message(row)
        })
        .unwrap_or_default();
    store
        .apply(&Batch {
            ops,
            source: Some(SourceWrite {
                cursor_id,
                session_id: "root".into(),
                generation,
                first_sequence: 0,
                mode: SourceMode::Activate,
            }),
            ..Batch::default()
        })
        .unwrap();
}

#[test]
fn replacing_or_emptying_a_source_can_move_activity_backwards() {
    let mut store = Store::open_in_memory().unwrap();
    init(&mut store);
    apply(
        &mut store,
        IngestOp::project_message(message("retained", "root", 0, Some(100))),
    );
    let cursor = cursor(&mut store);
    generation(&mut store, cursor, 1, Some(500));
    assert_eq!(bounds(&store, "root"), span(100, 500));
    generation(&mut store, cursor, 2, Some(200));
    assert_eq!(bounds(&store, "root"), span(100, 200));
    generation(&mut store, cursor, 3, None);
    assert_eq!(bounds(&store, "root"), span(100, 100));
}

#[test]
fn native_tool_completion_is_activity_in_live_and_cold_paths_even_without_timing() {
    for (cold, native_timing) in [(false, false), (true, false), (false, true), (true, true)] {
        let mut store = if cold {
            Store::open_in_memory_for_rebuild().unwrap()
        } else {
            Store::open_in_memory().unwrap()
        };
        init(&mut store);
        let cursor_id = cursor(&mut store);
        let opener = tool_message(cursor_id, Some(100));
        let mut ops = IngestOp::project_message(opener);
        ops.push(IngestOp::ToolUpdate {
            session_id: "root".into(),
            correlation_key: "call".into(),
            evidence: rosetta_core::EvidenceOrder {
                source_identity: rosetta_core::SourceIdentity("test-activity".into()),
                generation: 1,
                record_offset: 1,
                mutation_index: 0,
            },
            patch: Box::new(rosetta_core::ToolUpdatePatch {
                status: Some(rosetta_core::ToolStatusFact {
                    value: rosetta_core::ToolStatus::Succeeded,
                    basis: rosetta_core::StatusBasis::NativeTerminalRecord,
                }),
                timing: native_timing.then_some(rosetta_core::ToolTiming {
                    started_at: Some(50),
                    completed_at: Some(250),
                    duration_ms: Some(200),
                }),
                ..Default::default()
            }),
            at: Some(200),
        });
        store
            .apply(&Batch {
                ops,
                source: Some(SourceWrite {
                    cursor_id,
                    session_id: "root".into(),
                    generation: 1,
                    first_sequence: 0,
                    mode: SourceMode::Activate,
                }),
                ..Batch::default()
            })
            .unwrap();
        if cold {
            store.finish_bulk_load().unwrap();
        }
        let expected = if native_timing {
            span(50, 250)
        } else {
            span(100, 200)
        };
        assert_eq!(
            bounds(&store, "root"),
            expected,
            "cold={cold}, native_timing={native_timing}"
        );
    }
}

#[test]
fn usage_cannot_supply_activity_for_an_undated_tool_during_cold_replay() {
    for cold in [false, true] {
        let mut store = if cold {
            Store::open_in_memory_for_rebuild().unwrap()
        } else {
            Store::open_in_memory().unwrap()
        };
        init(&mut store);
        let cursor_id = cursor(&mut store);
        let mut ops = IngestOp::project_message(tool_message(cursor_id, None));
        ops.push(IngestOp::Usage {
            identity: None,
            session_id: "root".into(),
            response: "tool".into(),
            ts: 9_000_000,
            model: None,
            tokens: rosetta_core::TokenSnapshot {
                input: Some(10),
                ..Default::default()
            },
        });
        store
            .apply(&Batch {
                ops,
                source: Some(SourceWrite {
                    cursor_id,
                    session_id: "root".into(),
                    generation: 1,
                    first_sequence: 0,
                    mode: SourceMode::Activate,
                }),
                ..Batch::default()
            })
            .unwrap();
        if cold {
            store.finish_bulk_load().unwrap();
        }
        assert_eq!(bounds(&store, "root"), None, "cold={cold}");
        assert_eq!(
            queries::timeline_page(store.conn(), "root", None, 10).unwrap()[0]
                .usage
                .input,
            Some(10)
        );
    }
}

#[test]
fn activity_survives_projection_reopen_and_uses_covering_indexes() {
    let temp = tempfile::TempDir::new().unwrap();
    let path = temp.path().join("projection.sqlite");
    let mut store = Store::open(&path).unwrap();
    init(&mut store);
    apply(
        &mut store,
        IngestOp::project_message(message("row", "root", 0, Some(100))),
    );
    drop(store);
    let store = Store::open(&path).unwrap();
    assert_eq!(bounds(&store, "root"), span(100, 100));
    for (aggregate, column, index) in [
        ("MIN", "activity_first_at", "message_activity_first"),
        ("MAX", "activity_last_at", "message_activity_last"),
    ] {
        let sql = format!(
            "EXPLAIN QUERY PLAN SELECT {aggregate}({column}) FROM message WHERE session_id = 'root'"
        );
        let mut stmt = store.conn().prepare(&sql).unwrap();
        let details = stmt
            .query_map([], |row| row.get::<_, String>(3))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
            .join("\n");
        assert!(
            details.contains(&format!("COVERING INDEX {index}")),
            "{details}"
        );
    }
}
