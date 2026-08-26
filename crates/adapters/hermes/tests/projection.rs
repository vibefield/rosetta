//! Fixture-backed canonical projection for Hermes snapshot rows.

use std::path::{Path, PathBuf};

use rosetta_adapter_hermes::{HermesAdapter, spec};
use rosetta_adapter_support::{AgentAdapter, LineStatus, NormalizeCtx};
use rosetta_capture::{SnapshotOutcome, snapshot_db};
use rosetta_core::rows::MessageKind;
use rosetta_core::{IngestOp, ids};

fn fixture_db(scenario: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
        "../../../groundtruth/hermes/fixtures/0.20.5/{scenario}/hermes-home/state.db"
    ))
}

fn normalize_scenario(scenario: &str) -> Vec<IngestOp> {
    let adapter = HermesAdapter;
    let plan = adapter
        .snapshot_plan(spec::STATE_DB)
        .expect("snapshot plan");
    let path = fixture_db(scenario);
    let SnapshotOutcome::Ready(snapshot) = snapshot_db(&path).unwrap() else {
        panic!("{} did not produce a stable snapshot", path.display());
    };
    let sessions = snapshot.query_rows(&plan.sessions_sql, None).unwrap();
    assert_eq!(sessions.len(), 1, "fixture scenarios contain one session");
    let session: serde_json::Value = serde_json::from_str(&sessions[0]).unwrap();
    let native = session["native_session_id"].as_str().unwrap();
    let runtime_id = ids::runtime_id(adapter.kind(), "~/.hermes");
    let session_id = ids::session_id(&runtime_id, native);
    let mut ctx = NormalizeCtx::new(runtime_id, session_id);
    ctx.src_cursor = Some(7);

    let mut ops = Vec::new();
    for query in &plan.record_queries {
        for (offset, row) in snapshot
            .query_rows(&query.sql, Some(native))
            .unwrap()
            .into_iter()
            .enumerate()
        {
            ctx.src_offset = Some(offset as i64);
            let record = serde_json::json!({
                "query": &query.name,
                "row": serde_json::from_str::<serde_json::Value>(&row).unwrap(),
            });
            let outcome =
                adapter.ingest_line(&mut ctx, spec::STATE_DB, record.to_string().as_bytes());
            assert_eq!(
                outcome.status,
                LineStatus::Parsed,
                "{scenario}/{}: {row}",
                query.name
            );
            ops.extend(outcome.ops);
        }
    }
    ops
}

fn messages(ops: &[IngestOp]) -> Vec<&rosetta_core::rows::MessageRow> {
    ops.iter()
        .filter_map(|op| match op {
            IngestOp::Message(row) => Some(row.as_ref()),
            _ => None,
        })
        .collect()
}

#[test]
fn basic_chat_projects_system_workspace_and_flat_timeline() {
    let ops = normalize_scenario("basic-chat");
    assert!(matches!(ops[0], IngestOp::Message(ref row) if row.kind == MessageKind::System));
    assert!(ops.iter().any(|op| matches!(op, IngestOp::WorkspaceObs(_))));

    let rows = messages(&ops);
    assert_eq!(
        rows.iter().map(|row| row.kind).collect::<Vec<_>>(),
        [MessageKind::System, MessageKind::User, MessageKind::Agent]
    );
    assert_eq!(
        rows.iter().map(|row| row.ordinal).collect::<Vec<_>>(),
        [0, 1, 2]
    );
    assert!(rows.iter().all(|row| row.usage.is_none()));
    assert!(!ops.iter().any(|op| matches!(op, IngestOp::Drift(_))));
}

#[test]
fn tool_result_merges_without_consuming_an_ordinal() {
    let ops = normalize_scenario("tool-use");
    let rows = messages(&ops);
    assert_eq!(
        rows.iter().map(|row| row.kind).collect::<Vec<_>>(),
        [
            MessageKind::System,
            MessageKind::User,
            MessageKind::ToolUse,
            MessageKind::Agent,
        ]
    );
    assert_eq!(
        rows.iter().map(|row| row.ordinal).collect::<Vec<_>>(),
        [0, 1, 2, 3]
    );

    let call = rows
        .iter()
        .find_map(|row| row.tool.as_ref())
        .expect("tool call");
    assert_eq!(call.tool_id.as_deref(), Some("call_mock_0002"));
    assert_eq!(call.name, "terminal");
    assert_eq!(call.input["command"], "echo rosetta-mock");

    let result = ops.iter().find_map(|op| match op {
        IngestOp::ToolResult {
            tool_id,
            result,
            payload,
            is_error,
            ..
        } => Some((tool_id, result, payload, is_error)),
        _ => None,
    });
    let (tool_id, result, payload, is_error) = result.expect("tool result merge");
    assert_eq!(tool_id, "call_mock_0002");
    assert_eq!(result.as_deref(), Some("rosetta-mock"));
    assert_eq!(payload.as_ref().unwrap()["exit_code"], 0);
    assert_eq!(*is_error, Some(false));
}

#[test]
fn resume_keeps_native_message_id_order() {
    let rows = normalize_scenario("resume");
    let rows = messages(&rows);
    assert_eq!(
        rows.iter().map(|row| row.kind).collect::<Vec<_>>(),
        [
            MessageKind::System,
            MessageKind::User,
            MessageKind::Agent,
            MessageKind::User,
            MessageKind::Agent,
        ]
    );
    assert_eq!(
        rows.iter().map(|row| row.ordinal).collect::<Vec<_>>(),
        [0, 1, 2, 3, 4]
    );
    assert!(rows[3].content.as_deref().unwrap().contains("goodbye"));
}

#[test]
fn replay_is_deterministic() {
    assert_eq!(
        normalize_scenario("tool-use"),
        normalize_scenario("tool-use")
    );
}

#[test]
fn known_malformed_rows_are_hard_errors_and_new_tables_are_soft_drift() {
    let adapter = HermesAdapter;
    let mut ctx = NormalizeCtx::new("runtime", "session");
    let malformed = serde_json::json!({
        "query": "messages",
        "row": {"id": 1, "session_id": "s", "role": "assistant"},
    });
    let outcome = adapter.ingest_line(&mut ctx, spec::STATE_DB, malformed.to_string().as_bytes());
    assert!(matches!(outcome.status, LineStatus::HardError { .. }));
    assert!(
        outcome
            .ops
            .iter()
            .any(|op| matches!(op, IngestOp::Drift(_)))
    );

    let future = serde_json::json!({
        "query": "schema_inventory",
        "row": {"table_name": "future_table"},
    });
    let outcome = adapter.ingest_line(&mut ctx, spec::STATE_DB, future.to_string().as_bytes());
    assert_eq!(outcome.status, LineStatus::UnknownType);
    assert!(
        outcome
            .ops
            .iter()
            .any(|op| matches!(op, IngestOp::Drift(_)))
    );
}
