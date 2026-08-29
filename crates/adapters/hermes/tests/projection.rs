//! Fixture-backed canonical projection for Hermes snapshot rows.
//!
//! Regenerate the full canonical-op snapshots with:
//! `UPDATE_GOLDEN=1 cargo test -p rosetta-adapter-hermes --test projection`.

use std::path::{Path, PathBuf};

use rosetta_adapter_hermes::{HermesAdapter, spec};
use rosetta_adapter_support::{AgentAdapter, LineStatus, NormalizeCtx};
use rosetta_capture::{SnapshotOutcome, snapshot_db};
use rosetta_core::rows::MessageKind;
use rosetta_core::{IngestOp, ids};

const VERSION: &str = "0.20.5";
const SCENARIOS: [&str; 3] = ["basic-chat", "resume", "tool-use"];

fn fixture_db(scenario: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
        "../../../groundtruth/hermes/fixtures/{VERSION}/{scenario}/hermes-home/state.db"
    ))
}

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
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
    let usage: Vec<_> = ops
        .iter()
        .filter_map(|op| match op {
            IngestOp::SessionUsage(row) => Some(row),
            _ => None,
        })
        .collect();
    assert_eq!(usage.len(), 2);
    assert_eq!(
        usage
            .iter()
            .map(|row| row.tokens.input)
            .sum::<Option<i64>>(),
        Some(256)
    );
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
    for scenario in SCENARIOS {
        assert_eq!(
            normalize_scenario(scenario),
            normalize_scenario(scenario),
            "{scenario} replayed differently"
        );
    }
}

#[test]
fn canonical_ops_match_goldens() {
    let update = std::env::var_os("UPDATE_GOLDEN").is_some();
    for scenario in SCENARIOS {
        let ops = normalize_scenario(scenario);
        assert!(!ops.is_empty(), "{scenario} produced no canonical ops");
        assert!(
            !ops.iter().any(|op| matches!(op, IngestOp::Drift(_))),
            "{scenario} produced drift: {ops:#?}"
        );

        let rendered = format!("{}\n", serde_json::to_string_pretty(&ops).unwrap());
        let golden = golden_dir().join(VERSION).join(format!("{scenario}.json"));
        if update {
            std::fs::create_dir_all(golden.parent().unwrap()).unwrap();
            std::fs::write(&golden, rendered).unwrap();
            continue;
        }
        let expected = std::fs::read_to_string(&golden).unwrap_or_else(|error| {
            panic!(
                "{}: {error} — regenerate with UPDATE_GOLDEN=1 cargo test \
                 -p rosetta-adapter-hermes --test projection",
                golden.display()
            )
        });
        assert_eq!(rendered, expected, "changed {VERSION}/{scenario}");
    }
}

#[test]
fn goldens_carry_no_personal_paths() {
    for entry in walkdir::WalkDir::new(golden_dir())
        .sort_by_file_name()
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
    {
        let text = std::fs::read_to_string(entry.path()).unwrap();
        for needle in [concat!("/", "Users/"), "/home/", concat!("C:\\\\", "Users")] {
            assert!(
                !text.contains(needle),
                "{} contains a local path ({needle})",
                entry.path().display()
            );
        }
    }
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

#[test]
fn tool_specific_and_plain_results_are_lossless_soft_drift() {
    fn project(content: &str) -> (LineStatus, Vec<IngestOp>) {
        let adapter = HermesAdapter;
        let mut ctx = NormalizeCtx::new("runtime", "session");
        let record = serde_json::json!({
            "query": "messages",
            "row": {
                "id": 7,
                "session_id": "session",
                "role": "tool",
                "content": content,
                "tool_call_id": "call-7",
                "tool_name": "terminal",
                "timestamp": 1.0,
                "active": 1,
                "compacted": 0
            }
        });
        let outcome = adapter.ingest_line(&mut ctx, spec::STATE_DB, record.to_string().as_bytes());
        (outcome.status, outcome.ops)
    }

    let (status, ops) = project(r#"{"content":"synthetic file body","file_size":19}"#);
    assert_eq!(status, LineStatus::UnknownType);
    assert!(ops.iter().any(|op| matches!(op, IngestOp::Drift(_))));
    let structured = ops.iter().find_map(|op| match op {
        IngestOp::ToolResult {
            result,
            payload,
            is_error,
            ..
        } => Some((result, payload, is_error)),
        _ => None,
    });
    let (result, payload, is_error) = structured.expect("structured result");
    assert_eq!(result.as_deref(), Some("synthetic file body"));
    assert_eq!(payload.as_ref().unwrap()["file_size"], 19);
    assert_eq!(*is_error, None);

    let (status, ops) = project("plain synthetic output");
    assert_eq!(status, LineStatus::UnknownType);
    let plain = ops.iter().find_map(|op| match op {
        IngestOp::ToolResult {
            result,
            payload,
            is_error,
            ..
        } => Some((result, payload, is_error)),
        _ => None,
    });
    let (result, payload, is_error) = plain.expect("plain result");
    assert_eq!(result.as_deref(), Some("plain synthetic output"));
    assert_eq!(*payload, None);
    assert_eq!(*is_error, None);

    let (status, ops) = project(r#"{"error":"synthetic failure"}"#);
    assert_eq!(status, LineStatus::UnknownType);
    let error = ops.iter().find_map(|op| match op {
        IngestOp::ToolResult {
            result, is_error, ..
        } => Some((result, is_error)),
        _ => None,
    });
    let (result, is_error) = error.expect("error result");
    assert_eq!(result.as_deref(), Some("synthetic failure"));
    assert_eq!(*is_error, Some(true));
}
