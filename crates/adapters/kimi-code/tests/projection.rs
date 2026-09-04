//! Stateful normalization, topology isolation, and drift behavior.

use std::path::{Path, PathBuf};

use rosetta_adapter_kimi_code::KimiCodeAdapter;
use rosetta_adapter_support::{AgentAdapter, FileStat, LineStatus, NormalizeCtx};
use rosetta_core::rows::{SessionRole, TaskStatus};
use rosetta_core::timeline::{
    AttachmentKind, ResourceRef, TimelineBody, TimelineEntry, ToolAction, ToolStatus,
};
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
    ctx.native_session_id = Some(native.clone());
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

fn messages(ops: &[IngestOp]) -> Vec<&TimelineEntry> {
    ops.iter()
        .filter_map(|op| match op {
            IngestOp::Message(row) => Some(&**row),
            _ => None,
        })
        .collect()
}

fn body_kind(row: &TimelineEntry) -> &'static str {
    match &row.body {
        TimelineBody::User { .. } => "user",
        TimelineBody::Agent { .. } => "agent",
        TimelineBody::Thinking { .. } => "thinking",
        TimelineBody::Collaboration { .. } => "collaboration",
        TimelineBody::ToolUse { .. } => "tool_use",
        TimelineBody::Notice { .. } => "notice",
        TimelineBody::Summary { .. } => "summary",
    }
}

fn text(row: &TimelineEntry) -> Option<&str> {
    match &row.body {
        TimelineBody::User { text, .. } | TimelineBody::Agent { text, .. } => {
            text.as_ref().map(|text| text.as_preview())
        }
        TimelineBody::Thinking { text } | TimelineBody::Summary { text, .. } => {
            Some(text.as_preview())
        }
        TimelineBody::Collaboration { event } => event.body.as_ref().map(|text| text.as_preview()),
        TimelineBody::ToolUse { .. } | TimelineBody::Notice { .. } => None,
    }
}

fn tool(row: &TimelineEntry) -> Option<&rosetta_core::ToolInteraction> {
    match &row.body {
        TimelineBody::ToolUse { tool } => Some(tool),
        _ => None,
    }
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
    // The child opens with ledger-only context records. Draft ordinals retain
    // source order here; global finalization makes the public projection dense.
    assert_eq!(child.first().expect("child timeline").ordinal, 1);
    assert!(
        root.windows(2)
            .all(|rows| rows[0].ordinal < rows[1].ordinal)
    );
    assert!(
        child
            .windows(2)
            .all(|rows| rows[0].ordinal < rows[1].ordinal)
    );
    assert!(
        root.iter()
            .any(|row| row.parent_tool_native_id.as_deref() == Some("call_mock_0005"))
    );
    assert!(child.iter().all(|row| row.parent_tool_native_id.is_none()));
    let child_session_id = ids::session_id(&root_ctx.runtime_id, &child_native);
    assert!(root_ops.iter().any(|op| matches!(
        op,
        IngestOp::ToolUpdate { patch, .. }
            if patch.delegate_child_session_id.as_deref() == Some(child_session_id.as_str())
    )));

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
        rows.iter().filter(|row| body_kind(row) == "user").count(),
        1,
        "turn.prompt must not duplicate context.append_message"
    );
    let response_rows: Vec<_> = rows
        .iter()
        .filter(|row| matches!(body_kind(row), "agent" | "thinking" | "tool_use"))
        .collect();
    assert_eq!(response_rows.len(), 2);
    assert!(response_rows.iter().all(|row| !row.usage.is_empty()));
    assert!(
        ops.iter().all(|op| !matches!(op, IngestOp::Usage { .. })),
        "step.end must not duplicate usage.record"
    );
}

#[test]
fn session_scoped_usage_is_not_misattributed_to_a_response() {
    let mut ctx = synthetic_context();
    let usage = KimiCodeAdapter.ingest_line(
        &mut ctx,
        "wire",
        br#"{"type":"usage.record","agentId":"main","model":"mock-model","usage":{"inputOther":100,"output":20,"inputCacheRead":3,"inputCacheCreation":4},"usageScope":"session","time":1}"#,
    );
    assert_eq!(usage.status, LineStatus::Parsed);
    assert!(usage.ops.is_empty());

    let response = KimiCodeAdapter.ingest_line(
        &mut ctx,
        "wire",
        br#"{"type":"context.append_loop_event","agentId":"main","event":{"type":"content.part","uuid":"response-1","turnId":"0","step":1,"stepUuid":"step-1","part":{"type":"text","text":"done"}},"time":2}"#,
    );
    let [IngestOp::Message(row)] = response.ops.as_slice() else {
        panic!("expected one response row, got {:?}", response.ops);
    };
    assert!(row.usage.is_empty());
}

#[test]
fn tool_results_are_merge_ops_not_timeline_rows() {
    let (_, ops, _) = replay(&wire_fixture("tool-use", "main"));
    let tool_rows: Vec<_> = messages(&ops)
        .into_iter()
        .filter(|row| body_kind(row) == "tool_use")
        .collect();
    assert_eq!(tool_rows.len(), 1);
    assert_eq!(
        tool(tool_rows[0]).and_then(|tool| tool.native.id.as_deref()),
        Some("call_mock_0001")
    );
    let results: Vec<_> = ops
        .iter()
        .filter(|op| matches!(op, IngestOp::ToolUpdate { .. }))
        .collect();
    assert_eq!(results.len(), 1);
}

#[test]
fn unpinned_agent_writer_does_not_emit_a_delegate_only_update() {
    let mut ctx = synthetic_context();
    ctx.agent_version = Some("0.39.0".into());
    let call = KimiCodeAdapter.ingest_line(
        &mut ctx,
        "wire",
        br#"{"type":"context.append_loop_event","event":{"type":"tool.call","stepUuid":"step-1","toolCallId":"call-agent","name":"Agent","args":{"prompt":"go","description":"sub"}},"time":1}"#,
    );
    assert_eq!(call.status, LineStatus::Parsed);
    let [IngestOp::Drift(_), IngestOp::Message(opener)] = call.ops.as_slice() else {
        panic!(
            "expected projector drift and unknown opener, got {:?}",
            call.ops
        );
    };
    assert!(matches!(
        tool(opener).map(|tool| &tool.action),
        Some(ToolAction::Unknown {})
    ));
    assert_eq!(opener.parent_tool_native_id, None);

    let checkpoint = KimiCodeAdapter
        .checkpoint_state(ctx.state.as_deref())
        .expect("checkpoint serializes")
        .expect("state payload");
    let mut resumed = synthetic_context();
    resumed.agent_version = ctx.agent_version.clone();
    resumed.next_ordinal = ctx.next_ordinal;
    resumed.state = KimiCodeAdapter
        .restore_state(Some(&checkpoint))
        .expect("checkpoint restores");
    let result = KimiCodeAdapter.ingest_line(
        &mut resumed,
        "wire",
        br#"{"type":"context.append_loop_event","event":{"type":"tool.result","toolCallId":"call-agent","result":{"output":[{"type":"text","text":"agent_id: child-1"}]}},"time":2}"#,
    );
    assert_eq!(result.status, LineStatus::Parsed);
    let [IngestOp::ToolUpdate { patch, .. }] = result.ops.as_slice() else {
        panic!("expected one terminal update, got {:?}", result.ops);
    };
    assert_eq!(patch.delegate_child_session_id, None);
    assert_eq!(
        patch.status.as_ref().map(|status| status.value),
        Some(ToolStatus::Succeeded)
    );
}

#[test]
fn writer_declared_authored_media_projects_typed_references() {
    let mut ctx = synthetic_context();
    let message = KimiCodeAdapter.ingest_line(
        &mut ctx,
        "wire",
        br#"{"type":"context.append_message","message":{"role":"user","content":[{"type":"text","text":"before"},{"type":"image_url","imageUrl":{"url":"blobref:image","id":"image-1"}},{"type":"text","text":"after"}],"toolCalls":[],"origin":{"kind":"user"},"id":"message-media"},"time":1}"#,
    );
    assert_eq!(message.status, LineStatus::Parsed);
    let [IngestOp::Message(row)] = message.ops.as_slice() else {
        panic!("expected one message row, got {:?}", message.ops);
    };
    assert_eq!(text(row), Some("before\nafter"));
    let TimelineBody::User { attachments, .. } = &row.body else {
        panic!("expected authored user body")
    };
    assert_eq!(attachments.len(), 1);
    assert_eq!(attachments[0].kind, AttachmentKind::Image);
    assert!(matches!(attachments[0].source, ResourceRef::Url { .. }));

    let media_only = KimiCodeAdapter.ingest_line(
        &mut ctx,
        "wire",
        br#"{"type":"context.append_message","message":{"role":"user","content":[{"type":"image_url","imageUrl":{"url":"blobref:image-only"}}],"toolCalls":[],"origin":{"kind":"user"},"id":"message-media-only"},"time":2}"#,
    );
    assert_eq!(media_only.status, LineStatus::Parsed);
    let [IngestOp::Message(row)] = media_only.ops.as_slice() else {
        panic!(
            "expected one media-only message row, got {:?}",
            media_only.ops
        );
    };
    let TimelineBody::User { text, attachments } = &row.body else {
        panic!("expected media-only user body")
    };
    assert!(text.is_none());
    assert_eq!(attachments.len(), 1);

    ctx.src_offset = Some(99);
    let assistant_media = KimiCodeAdapter.ingest_line(
        &mut ctx,
        "wire",
        br#"{"type":"context.append_loop_event","event":{"type":"content.part","stepUuid":"step-media","part":{"type":"video_url","videoUrl":{"url":"blobref:video"}}},"time":3}"#,
    );
    assert_eq!(assistant_media.status, LineStatus::Parsed);
    let [IngestOp::Message(row)] = assistant_media.ops.as_slice() else {
        panic!(
            "expected one assistant media row, got {:?}",
            assistant_media.ops
        );
    };
    let TimelineBody::Agent {
        text, attachments, ..
    } = &row.body
    else {
        panic!("expected assistant media body")
    };
    assert!(text.is_none());
    assert_eq!(attachments.len(), 1);
    assert_eq!(attachments[0].kind, AttachmentKind::Video);
    assert_eq!(row.native_id, None);
}

#[test]
fn writer_optional_tool_fields_and_multimodal_results_are_bounded() {
    let mut ctx = synthetic_context();
    let call = KimiCodeAdapter.ingest_line(
        &mut ctx,
        "wire",
        br#"{"type":"context.append_loop_event","event":{"type":"tool.call","stepUuid":"step-1","toolCallId":"call-media","name":"ReadMediaFile","extras":{"provider":"mock"}},"time":1}"#,
    );
    assert_eq!(call.status, LineStatus::Parsed);
    let [IngestOp::Drift(drift), IngestOp::Message(row)] = call.ops.as_slice() else {
        panic!(
            "expected projector drift and one tool call row, got {:?}",
            call.ops
        );
    };
    assert_eq!(drift.error_code, "projection_missing_field");
    assert_eq!(drift.record_kind, "tool/request/ReadMediaFile");
    let tool = tool(row).expect("tool call");
    assert_eq!(row.native_id, None);
    assert!(matches!(tool.action, ToolAction::Unknown {}));

    let result = KimiCodeAdapter.ingest_line(
        &mut ctx,
        "wire",
        br#"{"type":"context.append_loop_event","event":{"type":"tool.result","toolCallId":"call-media","result":{"output":[{"type":"text","text":"caption"},{"type":"think","think":"not searchable","encrypted":"ciphertext"},{"type":"image_url","imageUrl":{"url":"blobref:image"}},{"type":"audio_url","audioUrl":{"url":"blobref:audio"}},{"type":"video_url","videoUrl":{"url":"blobref:video"}}]}},"time":2}"#,
    );
    assert_eq!(result.status, LineStatus::Parsed);
    let [IngestOp::ToolUpdate { patch, .. }] = result.ops.as_slice() else {
        panic!("expected one tool result merge, got {:?}", result.ops);
    };
    assert_eq!(
        patch.status.as_ref().map(|status| status.value),
        Some(ToolStatus::Succeeded)
    );
    assert!(patch.failure.is_none());

    let media_only = KimiCodeAdapter.ingest_line(
        &mut ctx,
        "wire",
        br#"{"type":"context.append_loop_event","event":{"type":"tool.result","toolCallId":"call-media","result":{"output":[{"type":"image_url","imageUrl":{"url":"blobref:image-only"}}]}},"time":3}"#,
    );
    assert_eq!(media_only.status, LineStatus::Parsed);
    let [IngestOp::ToolUpdate { patch, .. }] = media_only.ops.as_slice() else {
        panic!(
            "expected one media-only result merge, got {:?}",
            media_only.ops
        );
    };
    assert_eq!(
        patch.status.as_ref().map(|status| status.value),
        Some(ToolStatus::Succeeded)
    );
    assert!(patch.failure.is_none());
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
fn undeclared_types_drift_and_declared_bookkeeping_is_schema_checked() {
    let mut ctx = synthetic_context();
    let outcome =
        KimiCodeAdapter.ingest_line(&mut ctx, "wire", br#"{"type":"wormhole.opened","time":1}"#);
    assert_eq!(outcome.status, LineStatus::UnknownType);
    assert!(matches!(outcome.ops.as_slice(), [IngestOp::Drift(_)]));

    let outcome = KimiCodeAdapter.ingest_line(
        &mut ctx,
        "wire",
        br#"{"type":"full_compaction.complete","time":1}"#,
    );
    assert_eq!(outcome.status, LineStatus::Parsed);
    assert!(outcome.ops.is_empty());

    let outcome = KimiCodeAdapter.ingest_line(
        &mut ctx,
        "wire",
        br#"{"type":"full_compaction.complete","time":1,"newField":true}"#,
    );
    assert_eq!(outcome.status, LineStatus::Parsed);
    assert!(matches!(outcome.ops.as_slice(), [IngestOp::Drift(_)]));

    let outcome = KimiCodeAdapter.ingest_line(
        &mut ctx,
        "wire",
        br#"{"type":"context.append_loop_event","event":{"type":"wormhole.opened"},"time":1}"#,
    );
    assert_eq!(outcome.status, LineStatus::UnknownType);
    assert!(matches!(outcome.ops.as_slice(), [IngestOp::Drift(_)]));
}

#[test]
fn organic_config_compaction_and_todos_project_canonical_facts() {
    let mut ctx = synthetic_context();
    let config = KimiCodeAdapter.ingest_line(
        &mut ctx,
        "wire",
        br#"{"type":"config.update","agentsMdPaths":[],"environmentDisclosure":{"cwd":"/work/kimi","date":{"disclosed":true,"value":{"localDate":"2026-08-26","timeZone":"UTC"}}},"profileName":"default","systemPrompt":"prompt","time":1}"#,
    );
    assert_eq!(config.status, LineStatus::Parsed);
    assert!(config.ops.is_empty());

    let compaction = KimiCodeAdapter.ingest_line(
        &mut ctx,
        "wire",
        br#"{"type":"context.apply_compaction","compactedCount":4,"contextSummary":"preserved context","keptUserMessageCount":1,"summary":"short summary","summaryOutputTokens":12,"tokensAfter":20,"tokensBefore":100,"time":2}"#,
    );
    assert_eq!(compaction.status, LineStatus::Parsed);
    let [IngestOp::Message(summary)] = compaction.ops.as_slice() else {
        panic!("expected compaction summary, got {:?}", compaction.ops);
    };
    assert_eq!(body_kind(summary), "summary");
    assert_eq!(text(summary), Some("preserved context"));

    let todos = KimiCodeAdapter.ingest_line(
        &mut ctx,
        "wire",
        br#"{"type":"tools.update_store","key":"todo","value":[{"status":"in_progress","title":"Audit adapters"},{"status":"done","title":"Fix snapshots"}],"time":3}"#,
    );
    assert_eq!(todos.status, LineStatus::Parsed);
    let tasks = todos
        .ops
        .iter()
        .filter_map(|op| match op {
            IngestOp::Task(task) => Some(task),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(tasks.len(), 2);
    assert_eq!(tasks[0].status, TaskStatus::InProgress);
    assert_eq!(tasks[1].status, TaskStatus::Completed);
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

    let outcome = KimiCodeAdapter.ingest_line(
        &mut ctx,
        "wire",
        br#"{"type":"context.append_message","message":{"role":"user","content":[{"type":"image_url"}],"toolCalls":[],"origin":{"kind":"user"}},"time":1}"#,
    );
    assert_eq!(
        outcome.status,
        LineStatus::HardError {
            code: "missing_image_url".to_owned()
        }
    );
    assert!(matches!(outcome.ops.as_slice(), [IngestOp::Drift(_)]));
}

#[test]
fn undeclared_content_parts_are_soft_drift() {
    let mut ctx = synthetic_context();
    let outcome = KimiCodeAdapter.ingest_line(
        &mut ctx,
        "wire",
        br#"{"type":"context.append_message","message":{"role":"user","content":[{"type":"document_url","documentUrl":{"url":"blobref:document"}}],"toolCalls":[],"origin":{"kind":"user"}},"time":1}"#,
    );
    assert_eq!(outcome.status, LineStatus::UnknownType);
    assert!(
        !outcome.ops.is_empty()
            && outcome
                .ops
                .iter()
                .all(|op| matches!(op, IngestOp::Drift(_)))
    );
}
