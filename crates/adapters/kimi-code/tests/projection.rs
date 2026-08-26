//! Stateful normalization, topology isolation, and drift behavior.

use std::path::{Path, PathBuf};

use rosetta_adapter_kimi_code::KimiCodeAdapter;
use rosetta_adapter_support::{AgentAdapter, FileStat, LineStatus, NormalizeCtx};
use rosetta_core::rows::{MessageKind, MessageRow, SessionRole};
use rosetta_core::{IngestOp, ids};
use serde_json::json;

const DATA_ROOT: &str = "~/.kimi-code";

fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../groundtruth/kimi-code/fixtures/0.38.0")
}

fn wire_fixture(scenario: &str, agent: &str) -> PathBuf {
    walkdir::WalkDir::new(fixtures_root().join(scenario))
        .sort_by_file_name()
        .into_iter()
        .filter_map(Result::ok)
        .find(|entry| {
            entry.file_type().is_file()
                && entry.file_name() == "wire.jsonl"
                && entry
                    .path()
                    .parent()
                    .and_then(Path::file_name)
                    .is_some_and(|name| name == agent)
        })
        .unwrap_or_else(|| panic!("missing {scenario}/{agent} wire fixture"))
        .into_path()
}

fn relative(path: &Path) -> String {
    path.to_string_lossy()
        .split_once("kimi-home/")
        .expect("fixture under kimi-home")
        .1
        .to_owned()
}

fn context(path: &Path) -> (String, NormalizeCtx) {
    let adapter = KimiCodeAdapter;
    let text = std::fs::read_to_string(path).unwrap();
    let relative = relative(path);
    let stat = FileStat {
        relative_path: relative,
        size: text.len() as u64,
        modified_ms: 0,
    };
    let skim = adapter.skim("wire", &stat, text.as_bytes(), b"");
    let native = skim.session_id.expect("native wire identity");
    let runtime_id = ids::runtime_id(adapter.kind(), DATA_ROOT);
    let mut ctx = NormalizeCtx::new(&runtime_id, ids::session_id(&runtime_id, &native));
    ctx.state = adapter.init_state();
    (native, ctx)
}

fn lines_with_offsets(text: &str) -> Vec<(&str, i64)> {
    let mut offset = 0i64;
    text.lines()
        .map(|line| {
            let current = offset;
            offset += line.len() as i64 + 1;
            (line, current)
        })
        .collect()
}

fn ingest_range(
    ctx: &mut NormalizeCtx,
    lines: &[(&str, i64)],
    start: usize,
    end: usize,
) -> Vec<IngestOp> {
    let mut ops = Vec::new();
    for (index, (line, offset)) in lines.iter().enumerate().take(end).skip(start) {
        ctx.src_cursor = Some(index as i64 + 1);
        ctx.src_offset = Some(*offset);
        let outcome = KimiCodeAdapter.ingest_line(ctx, "wire", line.as_bytes());
        assert_eq!(outcome.status, LineStatus::Parsed, "line {}", index + 1);
        ops.extend(outcome.ops);
    }
    ops
}

fn replay(path: &Path) -> (String, Vec<IngestOp>, NormalizeCtx) {
    let text = std::fs::read_to_string(path).unwrap();
    let lines = lines_with_offsets(&text);
    let (native, mut ctx) = context(path);
    let ops = ingest_range(&mut ctx, &lines, 0, lines.len());
    (native, ops, ctx)
}

fn skim(path: &Path) -> rosetta_adapter_support::SkimResult {
    let text = std::fs::read_to_string(path).unwrap();
    KimiCodeAdapter.skim(
        "wire",
        &FileStat {
            relative_path: relative(path),
            size: text.len() as u64,
            modified_ms: 0,
        },
        text.as_bytes(),
        b"",
    )
}

fn messages(ops: &[IngestOp]) -> Vec<&MessageRow> {
    ops.iter()
        .filter_map(|op| match op {
            IngestOp::Message(row) => Some(&**row),
            _ => None,
        })
        .collect()
}

#[test]
fn checkpoint_round_trip_and_split_resume_match_one_shot() {
    let path = wire_fixture("tool-use", "main");
    let text = std::fs::read_to_string(&path).unwrap();
    let lines = lines_with_offsets(&text);
    let split = lines
        .iter()
        .position(|(line, _)| line.contains(r#""type":"usage.record""#))
        .expect("usage record")
        + 1;

    let (_, one_shot, _) = replay(&path);
    let (_, mut first_ctx) = context(&path);
    let mut resumed = ingest_range(&mut first_ctx, &lines, 0, split);
    let checkpoint = KimiCodeAdapter
        .checkpoint_state(first_ctx.state.as_deref())
        .expect("checkpoint serializes")
        .expect("state payload");
    assert!(checkpoint["pending_usage"].is_object());

    let (_, mut second_ctx) = context(&path);
    second_ctx.next_ordinal = first_ctx.next_ordinal;
    second_ctx.state = KimiCodeAdapter
        .restore_state(Some(&checkpoint))
        .expect("checkpoint restores");
    let round_trip = KimiCodeAdapter
        .checkpoint_state(second_ctx.state.as_deref())
        .expect("restored state serializes")
        .expect("state payload");
    assert_eq!(round_trip, checkpoint);

    resumed.extend(ingest_range(&mut second_ctx, &lines, split, lines.len()));
    assert_eq!(resumed, one_shot);
}

#[test]
fn invalid_checkpoint_payload_is_rejected() {
    assert!(
        KimiCodeAdapter
            .restore_state(Some(&json!({"unexpected": true})))
            .is_err()
    );
    assert!(KimiCodeAdapter.restore_state(Some(&json!(7))).is_err());
}

#[test]
fn fixture_topology_matches_native_state_facts() {
    let root_path = wire_fixture("subagent", "main");
    let child_path = wire_fixture("subagent", "agent-0");
    let state_path = root_path
        .ancestors()
        .nth(3)
        .expect("session directory")
        .join("state.json");
    let state: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(state_path).unwrap()).unwrap();
    let root_native = state["id"].as_str().expect("state.id");
    assert_eq!(state["agents"]["main"]["type"], "main");
    assert_eq!(state["agents"]["agent-0"]["type"], "sub");
    assert_eq!(state["agents"]["agent-0"]["parentAgentId"], "main");

    let root = skim(&root_path);
    assert_eq!(root.session_id.as_deref(), Some(root_native));
    assert_eq!(root.parent_session_id, None);
    assert_eq!(root.root_session_id, None);
    assert_eq!(root.role, Some(SessionRole::Main));

    let child = skim(&child_path);
    assert_eq!(
        child.session_id.as_deref(),
        Some(format!("{root_native}/agents/agent-0").as_str())
    );
    assert_eq!(child.parent_session_id.as_deref(), Some(root_native));
    assert_eq!(child.root_session_id.as_deref(), Some(root_native));
    assert_eq!(child.role, Some(SessionRole::Subagent));
}

#[test]
fn root_and_child_wires_have_separate_ordinals_and_state() {
    let (root_native, root_ops, root_ctx) = replay(&wire_fixture("subagent", "main"));
    let (child_native, child_ops, child_ctx) = replay(&wire_fixture("subagent", "agent-0"));
    assert_ne!(root_native, child_native);
    assert_ne!(root_ctx.session_id, child_ctx.session_id);

    let root = messages(&root_ops);
    let child = messages(&child_ops);
    assert_eq!(root.first().expect("root timeline").ordinal, 0);
    assert_eq!(child.first().expect("child timeline").ordinal, 0);
    assert_eq!(root.last().unwrap().ordinal, root.len() as i64 - 1);
    assert_eq!(child.last().unwrap().ordinal, child.len() as i64 - 1);
    assert!(
        root.iter()
            .any(|row| row.branch_tool_id.as_deref() == Some("call_mock_0005"))
    );
    assert!(child.iter().all(|row| row.branch_tool_id.is_none()));

    let root_state = KimiCodeAdapter
        .checkpoint_state(root_ctx.state.as_deref())
        .unwrap()
        .unwrap();
    let child_state = KimiCodeAdapter
        .checkpoint_state(child_ctx.state.as_deref())
        .unwrap()
        .unwrap();
    assert!(root_state["pending_usage"].is_null());
    assert!(child_state["pending_usage"].is_null());
}

#[test]
fn duplicate_prompt_and_usage_rails_emit_once() {
    let (_, ops, _) = replay(&wire_fixture("tool-use", "main"));
    let rows = messages(&ops);
    assert_eq!(
        rows.iter()
            .filter(|row| row.kind == MessageKind::User)
            .count(),
        1,
        "turn.prompt must not duplicate context.append_message"
    );
    let response_rows: Vec<_> = rows
        .iter()
        .filter(|row| {
            matches!(
                row.kind,
                MessageKind::Agent | MessageKind::Thinking | MessageKind::ToolUse
            )
        })
        .collect();
    assert_eq!(response_rows.len(), 2);
    assert!(response_rows.iter().all(|row| row.usage.is_some()));
    assert!(
        ops.iter().all(|op| !matches!(op, IngestOp::Usage { .. })),
        "step.end must not duplicate usage.record"
    );
}

#[test]
fn tool_results_are_merge_ops_not_timeline_rows() {
    let (_, ops, _) = replay(&wire_fixture("tool-use", "main"));
    let tool_rows: Vec<_> = messages(&ops)
        .into_iter()
        .filter(|row| row.kind == MessageKind::ToolUse)
        .collect();
    assert_eq!(tool_rows.len(), 1);
    assert_eq!(
        tool_rows[0]
            .tool
            .as_ref()
            .and_then(|tool| tool.tool_id.as_deref()),
        Some("call_mock_0001")
    );
    let results: Vec<_> = ops
        .iter()
        .filter(|op| matches!(op, IngestOp::ToolResult { .. }))
        .collect();
    assert_eq!(results.len(), 1);
}

fn synthetic_context() -> NormalizeCtx {
    let runtime_id = ids::runtime_id("kimi-code", DATA_ROOT);
    let mut ctx = NormalizeCtx::new(
        &runtime_id,
        ids::session_id(&runtime_id, "session_under_test"),
    );
    ctx.state = KimiCodeAdapter.init_state();
    ctx
}

#[test]
fn undeclared_types_drift_and_declared_unprojected_types_are_silent() {
    let mut ctx = synthetic_context();
    let outcome =
        KimiCodeAdapter.ingest_line(&mut ctx, "wire", br#"{"type":"wormhole.opened","time":1}"#);
    assert_eq!(outcome.status, LineStatus::UnknownType);
    assert!(matches!(outcome.ops.as_slice(), [IngestOp::Drift(_)]));

    let outcome =
        KimiCodeAdapter.ingest_line(&mut ctx, "wire", br#"{"type":"context.apply_compaction"}"#);
    assert_eq!(outcome.status, LineStatus::Parsed);
    assert!(outcome.ops.is_empty());

    let outcome = KimiCodeAdapter.ingest_line(
        &mut ctx,
        "wire",
        br#"{"type":"context.append_loop_event","event":{"type":"wormhole.opened"},"time":1}"#,
    );
    assert_eq!(outcome.status, LineStatus::UnknownType);
    assert!(matches!(outcome.ops.as_slice(), [IngestOp::Drift(_)]));
}

#[test]
fn malformed_known_types_are_hard_errors_with_drift() {
    let mut ctx = synthetic_context();
    let outcome =
        KimiCodeAdapter.ingest_line(&mut ctx, "wire", br#"{"type":"profile.bind","time":1}"#);
    assert_eq!(
        outcome.status,
        LineStatus::HardError {
            code: "parse_failed".to_owned()
        }
    );
    assert!(matches!(outcome.ops.as_slice(), [IngestOp::Drift(_)]));

    let outcome = KimiCodeAdapter.ingest_line(
        &mut ctx,
        "wire",
        br#"{"type":"context.append_loop_event","event":{"type":"tool.call","uuid":"u","toolCallId":"t","args":{}},"time":1}"#,
    );
    assert_eq!(
        outcome.status,
        LineStatus::HardError {
            code: "missing_name".to_owned()
        }
    );
    assert!(matches!(outcome.ops.as_slice(), [IngestOp::Drift(_)]));
}
