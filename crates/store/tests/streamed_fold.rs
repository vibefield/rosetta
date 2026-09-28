//! A streamed fresh-cold runtime's fold writes each resolved tool row once,
//! and refuses to run where its "no stored opener can match" premise fails;
//! a source resolved at file close writes exactly what that fold would.
use rosetta_core::{
    EvidenceOrder, IngestOp, MessageRow, SessionRow, SourceIdentity, ToolUpdatePatch,
};
use rosetta_store::{
    ApplyStats, Batch, CheckpointUpdate, ColdReconciliation, ResolvedSourceTools, SourceMode,
    SourceWrite, Store, StoreError, StreamedSource,
};
use serde_json::json;

fn session(id: &str) -> SessionRow {
    serde_json::from_value(json!({
        "id": id, "native_session_id": id, "runtime_id": "rt", "root_session_id": id,
        "role": "main", "started_at": 1, "updated_at": 1, "stream_state": "pending"
    }))
    .unwrap()
}

fn cold_store() -> Store {
    let mut store = Store::open_in_memory_for_rebuild().unwrap();
    store.use_precomputed_bulk_ordinals().unwrap();
    let mut ops = vec![IngestOp::Runtime(
        serde_json::from_value(json!({
            "id": "rt", "kind": "claude-code", "data_root": "/agent",
            "enabled": true, "discovered_at": 1
        }))
        .unwrap(),
    )];
    ops.push(IngestOp::SessionMeta(session("root")));
    store
        .apply(&Batch {
            ops,
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
        })
        .unwrap();
    store
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

fn opener(cursor_id: i64) -> IngestOp {
    let row: MessageRow = serde_json::from_value(json!({
        "timeline_id": "tool", "session_id": "root", "ordinal": 0, "kind": "tool_use",
        "ts": 10, "attachments": [], "citations": [], "src_cursor": cursor_id, "src_offset": 0,
        "tool": {
            "correlation": {"key": "call", "derivation": "native_id"},
            "native": {"id": "call", "name": "Bash"},
            "status": {"value": "pending", "basis": "request_observed"},
            "action": {"type": "shell", "command": {"type": "inline", "text": "true"},
                "source_truncated": false, "background": false}
        }
    }))
    .unwrap();
    IngestOp::project_message(row).pop().unwrap()
}

fn result(path: &str) -> IngestOp {
    let patch: ToolUpdatePatch = serde_json::from_value(json!({
        "status": {"value": "succeeded", "basis": "native_terminal_record"}
    }))
    .unwrap();
    IngestOp::ToolUpdate {
        session_id: "root".into(),
        correlation_key: "call".into(),
        evidence: EvidenceOrder {
            source_identity: SourceIdentity(path.into()),
            generation: 1,
            record_offset: 1,
            mutation_index: 0,
        },
        patch: Box::new(patch),
        at: Some(20),
    }
}

fn closed(store: &Store, path: &str, ops: Vec<IngestOp>) -> Batch {
    Batch {
        ops,
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
            cursor_id: cursor(store, path),
            session_id: "root".into(),
            generation: 1,
            first_sequence: 0,
            mode: SourceMode::Activate,
        }),
    }
}

fn tool_status(store: &Store) -> Vec<(String, String)> {
    store
        .conn()
        .prepare("SELECT timeline_id, tool_status FROM message WHERE type = 'tool_use'")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

#[test]
fn a_streamed_fold_writes_each_resolved_tool_row_once_at_admission() {
    let mut store = cold_store();
    let mut state = ColdReconciliation::default();
    let a = cursor(&store, "a.jsonl");
    let batch = closed(&store, "a.jsonl", vec![opener(a), result("a.jsonl")]);
    store
        .apply_streamed_sources(&mut state, vec![batch.into()])
        .unwrap();
    // Collected, not written: tool rows wait for the admission fold.
    assert!(tool_status(&store).is_empty());

    store.fold_cold_reconciliation(state).unwrap();
    assert_eq!(
        tool_status(&store),
        vec![("tool".to_string(), "succeeded".to_string())]
    );
}

fn tool_opener(timeline: &str, key: &str, ordinal: i64, ts: Option<i64>, text: &str) -> IngestOp {
    let row: MessageRow = serde_json::from_value(json!({
        "timeline_id": timeline, "session_id": "root", "ordinal": ordinal, "kind": "tool_use",
        "ts": ts, "attachments": [], "citations": [], "src_offset": ordinal * 10,
        "tool": {
            "correlation": {"key": key, "derivation": "native_id"},
            "native": {"id": key, "name": "Bash"},
            "status": {"value": "pending", "basis": "request_observed"},
            "action": {"type": "shell", "command": {"type": "inline", "text": text},
                "source_truncated": false, "background": false}
        }
    }))
    .unwrap();
    IngestOp::project_message(row).pop().unwrap()
}

fn agent_reply(timeline: &str, ordinal: i64) -> IngestOp {
    let row: MessageRow = serde_json::from_value(json!({
        "timeline_id": timeline, "session_id": "root", "ordinal": ordinal, "kind": "agent",
        "ts": 12, "content": "done", "attachments": [], "citations": [],
        "src_offset": ordinal * 10
    }))
    .unwrap();
    IngestOp::project_message(row).pop().unwrap()
}

fn update(key: &str, offset: u64, value: &str, at: Option<i64>) -> IngestOp {
    let patch: ToolUpdatePatch = serde_json::from_value(json!({
        "status": {"value": value, "basis": "native_terminal_record"}
    }))
    .unwrap();
    IngestOp::ToolUpdate {
        session_id: "root".into(),
        correlation_key: key.into(),
        evidence: EvidenceOrder {
            source_identity: SourceIdentity("a.jsonl".into()),
            generation: 1,
            record_offset: offset,
            mutation_index: 0,
        },
        patch: Box::new(patch),
        at,
    }
}

fn usage(response: &str, ts: i64, output: i64, identity: Option<&str>) -> IngestOp {
    IngestOp::Usage {
        session_id: "root".into(),
        response: response.into(),
        ts,
        model: Some("model-x".into()),
        tokens: serde_json::from_value(json!({"input": 3, "output": output})).unwrap(),
        identity: identity.map(str::to_owned),
    }
}

fn file_edit(timeline: &str) -> IngestOp {
    IngestOp::FileEdit(
        serde_json::from_value(json!({
            "session_id": "root", "message_timeline_id": timeline, "path": "/src/a.rs",
            "kind": "edit", "lines_added": 2, "lines_removed": 1, "at": 25
        }))
        .unwrap(),
    )
}

/// Every lifecycle shape one source can carry: a losing duplicate opener, an
/// undated opener, matched and unmatched updates, usage absorbed into a
/// dated tool row, usage left for an undated one and for an agent row (one
/// with an ownership identity), and a file edit on a tool row.
fn lifecycle_source() -> Vec<IngestOp> {
    vec![
        tool_opener("tool-a", "call-a", 0, Some(10), "first"),
        tool_opener("tool-b", "call-b", 1, None, "undated"),
        agent_reply("reply", 2),
        update("call-a", 3, "succeeded", Some(20)),
        update("call-b", 4, "failed", Some(30)),
        tool_opener("tool-a", "call-a", 0, Some(11), "repeated"),
        update("call-z", 6, "succeeded", Some(40)),
        usage("tool-a", 50, 7, None),
        usage("tool-b", 51, 8, None),
        usage("reply", 52, 9, Some("msg-1:req-1")),
        file_edit("tool-a"),
        agent_reply("after", 3),
    ]
}

/// Every canonical row the lifecycle tables hold, without surrogate ids.
fn lifecycle_rows(store: &Store) -> Vec<String> {
    let mut rows = Vec::new();
    for sql in [
        "SELECT m.timeline_id, m.ordinal, m.type, m.ts, m.model, m.content,
                m.tool_correlation_key, m.tool_status, m.tool_status_basis, m.tool_detail,
                m.tool_started_at, m.tool_completed_at, m.in_tokens, m.out_tokens,
                m.activity_first_at, m.activity_last_at, c.path, m.src_offset
         FROM message m LEFT JOIN ingest_cursor c ON c.id = m.src_cursor
         ORDER BY m.timeline_id",
        "SELECT f.session_id, m.timeline_id, f.path, f.kind, f.at, c.path
         FROM file_edit f LEFT JOIN message m ON m.id = f.message_id
         LEFT JOIN ingest_cursor c ON c.id = f.src_cursor
         ORDER BY f.path",
        "SELECT id, updated_at, started_at FROM session ORDER BY id",
    ] {
        let mut statement = store.conn().prepare(sql).unwrap();
        let columns = statement.column_count();
        rows.extend(
            statement
                .query_map([], |row| {
                    (0..columns)
                        .map(|index| {
                            row.get::<_, rusqlite::types::Value>(index)
                                .map(|value| format!("{value:?}"))
                        })
                        .collect::<Result<Vec<_>, _>>()
                        .map(|fields| fields.join(" | "))
                })
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap(),
        );
    }
    rows
}

#[test]
fn a_source_resolved_at_file_close_writes_what_the_admission_fold_writes() {
    let mut folded = cold_store();
    let mut state = ColdReconciliation::default();
    let batch = closed(&folded, "a.jsonl", lifecycle_source());
    let applied = folded
        .apply_streamed_sources(&mut state, vec![batch.into()])
        .unwrap();
    let admitted = folded.fold_cold_reconciliation(state).unwrap();

    let mut early = cold_store();
    let mut state = ColdReconciliation::default();
    let (ops, resolved) = ResolvedSourceTools::resolve("a.jsonl", 1, lifecycle_source()).unwrap();
    assert_eq!(resolved.tool_rows(), 2);
    // The losing opener, both matched updates, the unmatched one and the
    // usage absorbed into tool-a are consumed; tool-a's resolved row takes
    // its winning opener's position and the rest keeps its own.
    assert_eq!(resolved.positions(), &[1, 2, 5, 8, 9, 10, 11]);
    let batch = closed(&early, "a.jsonl", ops);
    let resolved_applied = early
        .apply_streamed_sources(
            &mut state,
            vec![StreamedSource {
                batch,
                resolved: Some(resolved),
            }],
        )
        .unwrap();
    // The source's tool rows are written with it, before admission.
    assert_eq!(tool_status(&early).len(), 2);
    let resolved_admitted = early.fold_cold_reconciliation(state).unwrap();

    assert_eq!(lifecycle_rows(&folded), lifecycle_rows(&early));
    let totals = |applied: &ApplyStats, admitted: &ApplyStats| {
        (
            applied.messages_inserted + admitted.messages_inserted,
            applied.tool_results_merged + admitted.tool_results_merged,
            applied.tool_results_unmatched + admitted.tool_results_unmatched,
            applied.usage_applied + admitted.usage_applied,
            applied.profile.bulk_tool_rows_reconciled + admitted.profile.bulk_tool_rows_reconciled,
        )
    };
    assert_eq!(
        totals(&applied, &admitted),
        totals(&resolved_applied, &resolved_admitted)
    );
}

#[test]
fn a_resolved_source_needs_a_streamed_activation() {
    let mut store = cold_store();
    let mut state = ColdReconciliation::default();
    let (ops, resolved) = ResolvedSourceTools::resolve("a.jsonl", 1, lifecycle_source()).unwrap();
    let mut batch = closed(&store, "a.jsonl", ops);
    batch.source.as_mut().unwrap().mode = SourceMode::Abandon;
    let error = store
        .apply_streamed_sources(
            &mut state,
            vec![StreamedSource {
                batch,
                resolved: Some(resolved),
            }],
        )
        .unwrap_err();
    assert!(
        matches!(&error, StoreError::BulkLoadState { message } if message.contains("resolved at file close")),
        "{error}"
    );
}

#[test]
fn a_streamed_fold_fails_closed_where_a_stored_opener_could_match() {
    let mut store = cold_store();
    let mut first = ColdReconciliation::default();
    let a = cursor(&store, "a.jsonl");
    let batch = closed(&store, "a.jsonl", vec![opener(a)]);
    store
        .apply_streamed_sources(&mut first, vec![batch.into()])
        .unwrap();
    store.fold_cold_reconciliation(first).unwrap();

    // A second fold addressing the same correlation would have to update the
    // stored row — the retryable path's job. The streamed fold refuses.
    let mut second = ColdReconciliation::default();
    let batch = closed(&store, "b.jsonl", vec![result("b.jsonl")]);
    store
        .apply_streamed_sources(&mut second, vec![batch.into()])
        .unwrap();
    let error = store.fold_cold_reconciliation(second).unwrap_err();
    assert!(
        matches!(&error, StoreError::ProjectionInvalid { message } if message.contains("stored tool row")),
        "{error}"
    );
}
