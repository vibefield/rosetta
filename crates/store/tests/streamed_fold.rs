//! A streamed fresh-cold runtime's fold writes each resolved tool row once,
//! and refuses to run where its "no stored opener can match" premise fails.
use rosetta_core::{
    EvidenceOrder, IngestOp, MessageRow, SessionRow, SourceIdentity, ToolUpdatePatch,
};
use rosetta_store::{
    Batch, CheckpointUpdate, ColdReconciliation, SourceMode, SourceWrite, Store, StoreError,
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
        .apply_streamed_sources(&mut state, vec![batch])
        .unwrap();
    // Collected, not written: tool rows wait for the admission fold.
    assert!(tool_status(&store).is_empty());

    store.fold_cold_reconciliation(state).unwrap();
    assert_eq!(
        tool_status(&store),
        vec![("tool".to_string(), "succeeded".to_string())]
    );
}

#[test]
fn a_streamed_fold_fails_closed_where_a_stored_opener_could_match() {
    let mut store = cold_store();
    let mut first = ColdReconciliation::default();
    let a = cursor(&store, "a.jsonl");
    let batch = closed(&store, "a.jsonl", vec![opener(a)]);
    store
        .apply_streamed_sources(&mut first, vec![batch])
        .unwrap();
    store.fold_cold_reconciliation(first).unwrap();

    // A second fold addressing the same correlation would have to update the
    // stored row — the retryable path's job. The streamed fold refuses.
    let mut second = ColdReconciliation::default();
    let batch = closed(&store, "b.jsonl", vec![result("b.jsonl")]);
    store
        .apply_streamed_sources(&mut second, vec![batch])
        .unwrap();
    let error = store.fold_cold_reconciliation(second).unwrap_err();
    assert!(
        matches!(&error, StoreError::ProjectionInvalid { message } if message.contains("stored tool row")),
        "{error}"
    );
}
