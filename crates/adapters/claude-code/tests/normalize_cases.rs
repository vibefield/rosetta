//! Normalization cases the committed fixture corpus does not reach.
//!
//! The farm's scenarios exercise plain chat and one Bash call; the shapes below
//! are taken from the canonical ground truth's facets (`assistant-content-blocks`,
//! `tool-input`, `user-content-blocks`, `system-subtypes`) — the SPEC, never
//! intuition. They are normalization claims, not schema claims: what the raw
//! types accept stays the fixture corpus's business (`fixtures_replay.rs`).

use rosetta_adapter_claude_code::ClaudeCodeAdapter;
use rosetta_adapter_support::{AgentAdapter, LineStatus, NormalizeCtx};
use rosetta_core::rows::{DriftFact, EditKind, FileEditRow, MessageKind, MessageRow};
use rosetta_core::{IngestOp, ids};
use serde_json::{Value, json};

const CWD: &str = "/tmp/project";
const UUID: &str = "11111111-1111-1111-1111-111111111111";

fn run(lines: &[Value]) -> (Vec<IngestOp>, Vec<LineStatus>) {
    let adapter = ClaudeCodeAdapter;
    let runtime_id = ids::runtime_id(adapter.kind(), &adapter.layout().data_root);
    let mut ctx = NormalizeCtx::new(
        &runtime_id,
        ids::session_id(&runtime_id, "session-under-test"),
    );
    ctx.state = adapter.init_state();

    let mut ops = Vec::new();
    let mut statuses = Vec::new();
    for line in lines {
        let outcome = adapter.ingest_line(&mut ctx, "transcript", line.to_string().as_bytes());
        ops.extend(outcome.ops);
        statuses.push(outcome.status);
    }
    (ops, statuses)
}

/// An assistant record with the given content blocks, in the shape the
/// canonical records for the `assistant` type.
fn assistant(content: Value, usage: Option<Value>) -> Value {
    let mut message = json!({
        "id": "msg_1",
        "type": "message",
        "role": "assistant",
        "model": "claude-opus-5",
        "content": content,
        "stop_reason": "end_turn",
        "stop_sequence": null,
    });
    if let Some(usage) = usage {
        message["usage"] = usage;
    }
    json!({
        "type": "assistant",
        "parentUuid": null,
        "isSidechain": false,
        "message": message,
        "uuid": UUID,
        "timestamp": "2026-08-25T02:35:19.072Z",
        "sessionId": "session-under-test",
        "version": "2.1.241",
        "cwd": CWD,
        "gitBranch": "main",
        "userType": "external",
        "effort": "high",
    })
}

fn usage(input: u64, output: u64, thinking: u64) -> Value {
    json!({
        "input_tokens": input,
        "output_tokens": output,
        "cache_creation_input_tokens": 7,
        "cache_read_input_tokens": 9,
        "cache_creation": {},
        "inference_geo": null,
        "service_tier": "standard",
        "output_tokens_details": {"thinking_tokens": thinking},
    })
}

fn user(content: Value, extra: Value) -> Value {
    let mut line = json!({
        "type": "user",
        "parentUuid": UUID,
        "isSidechain": false,
        "message": {"role": "user", "content": content},
        "uuid": "22222222-2222-2222-2222-222222222222",
        "timestamp": "2026-08-25T02:35:20.000Z",
        "sessionId": "session-under-test",
        "version": "2.1.241",
        "cwd": CWD,
        "gitBranch": "main",
        "userType": "external",
    });
    let (Value::Object(line_fields), Value::Object(extra)) = (&mut line, extra) else {
        unreachable!("both are objects");
    };
    line_fields.extend(extra);
    line
}

fn messages(ops: &[IngestOp]) -> Vec<&MessageRow> {
    ops.iter()
        .filter_map(|op| match op {
            IngestOp::Message(row) => Some(&**row),
            _ => None,
        })
        .collect()
}

fn edits(ops: &[IngestOp]) -> Vec<&FileEditRow> {
    ops.iter()
        .filter_map(|op| match op {
            IngestOp::FileEdit(row) => Some(row),
            _ => None,
        })
        .collect()
}

fn drift(ops: &[IngestOp]) -> Vec<&DriftFact> {
    ops.iter()
        .filter_map(|op| match op {
            IngestOp::Drift(fact) => Some(fact),
            _ => None,
        })
        .collect()
}

#[test]
fn assistant_blocks_split_into_one_row_each() {
    let (ops, _) = run(&[assistant(
        json!([
            {"type": "thinking", "thinking": "weighing it up", "signature": "sig"},
            {"type": "text", "text": "here is the answer"},
            {"type": "tool_use", "id": "toolu_1", "name": "Bash",
             "input": {"command": "ls", "description": "list"}},
        ]),
        Some(usage(100, 20, 5)),
    )]);

    let rows = messages(&ops);
    let kinds: Vec<MessageKind> = rows.iter().map(|row| row.kind).collect();
    assert_eq!(
        kinds,
        [
            MessageKind::Thinking,
            MessageKind::Agent,
            MessageKind::ToolUse
        ]
    );
    assert_eq!(
        rows.iter().map(|row| row.ordinal).collect::<Vec<_>>(),
        [0, 1, 2]
    );

    // Split rows share the record's uuid but never its timeline id.
    assert!(rows.iter().all(|row| row.uuid.as_deref() == Some(UUID)));
    let ids: std::collections::BTreeSet<&str> =
        rows.iter().map(|row| row.timeline_id.as_str()).collect();
    assert_eq!(ids.len(), 3);

    // Model and effort belong to the response, so every split row carries them.
    assert!(
        rows.iter()
            .all(|row| row.model.as_deref() == Some("claude-opus-5"))
    );
    assert!(rows.iter().all(|row| row.effort.as_deref() == Some("high")));
    // The per-record version stamp beats whatever the pipeline guessed.
    assert!(
        rows.iter()
            .all(|row| row.agent_version.as_deref() == Some("2.1.241"))
    );

    // Usage is reported once per response and lands on the text row.
    assert_eq!(rows[0].usage, None);
    assert_eq!(rows[2].usage, None);
    let tokens = rows[1].usage.expect("usage on the agent row");
    assert_eq!(tokens.input, Some(100));
    assert_eq!(tokens.output, Some(20));
    assert_eq!(tokens.cache_read, Some(9));
    assert_eq!(tokens.cache_write, Some(7));
    assert_eq!(tokens.thinking, Some(5));

    let tool = rows[2].tool.as_ref().expect("tool call");
    assert_eq!(tool.name, "Bash");
    assert_eq!(tool.tool_id.as_deref(), Some("toolu_1"));
    assert_eq!(tool.input["command"], json!("ls"));
    // The result arrives on a later line, never on the call itself.
    assert_eq!(tool.result, None);
}

#[test]
fn usage_falls_back_to_the_tool_row_then_to_any_row() {
    let (ops, _) = run(&[assistant(
        json!([{"type": "tool_use", "id": "toolu_1", "name": "Bash", "input": {"command": "ls"}}]),
        Some(usage(10, 2, 0)),
    )]);
    assert!(messages(&ops)[0].usage.is_some());

    // An API-error record carries no content at all; its usage must not vanish
    // with the response.
    let (ops, statuses) = run(&[assistant(json!([]), Some(usage(10, 2, 0)))]);
    let rows = messages(&ops);
    assert_eq!(statuses, [LineStatus::Parsed]);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].kind, MessageKind::Agent);
    assert_eq!(rows[0].content, None);
    assert!(rows[0].usage.is_some());
}

#[test]
fn edit_and_write_calls_also_produce_file_edits() {
    let (ops, _) = run(&[assistant(
        json!([
            {"type": "tool_use", "id": "toolu_edit", "name": "Edit", "input": {
                "file_path": "/tmp/project/src/main.rs",
                "old_string": "one\ntwo",
                "new_string": "one\ntwo\nthree",
                "replace_all": false}},
            {"type": "tool_use", "id": "toolu_write", "name": "Write", "input": {
                "file_path": "/tmp/project/README.md",
                "content": "a\nb\nc\nd"}},
        ]),
        None,
    )]);

    let rows = messages(&ops);
    let changed = edits(&ops);
    assert_eq!(changed.len(), 2);

    assert_eq!(changed[0].kind, EditKind::Edit);
    assert_eq!(changed[0].path, "/tmp/project/src/main.rs");
    assert_eq!(changed[0].lines_added, Some(3));
    assert_eq!(changed[0].lines_removed, Some(2));
    // The edit points back at the tool row that made it.
    assert_eq!(
        changed[0].message_timeline_id.as_deref(),
        Some(rows[0].timeline_id.as_str())
    );

    assert_eq!(changed[1].kind, EditKind::Write);
    assert_eq!(changed[1].lines_added, Some(4));
    // A whole-file write removes an unknown number of lines — never zero.
    assert_eq!(changed[1].lines_removed, None);

    // A read-only tool changes no file.
    let (ops, _) = run(&[assistant(
        json!([{"type": "tool_use", "id": "t", "name": "Read",
                "input": {"file_path": "/tmp/project/src/main.rs"}}]),
        None,
    )]);
    assert!(edits(&ops).is_empty());
}

#[test]
fn spawn_tools_anchor_the_subagent_thread() {
    for name in ["Agent", "Task"] {
        let (ops, _) = run(&[assistant(
            json!([{"type": "tool_use", "id": "toolu_spawn", "name": name,
                    "input": {"prompt": "go", "description": "sub"}}]),
            None,
        )]);
        assert_eq!(
            messages(&ops)[0].branch_tool_id.as_deref(),
            Some("toolu_spawn"),
            "{name} should anchor a subagent thread"
        );
    }

    let (ops, _) = run(&[assistant(
        json!([{"type": "tool_use", "id": "t", "name": "Bash", "input": {"command": "ls"}}]),
        None,
    )]);
    assert_eq!(messages(&ops)[0].branch_tool_id, None);
}

#[test]
fn tool_results_fold_onto_their_call_instead_of_becoming_rows() {
    let (ops, _) = run(&[user(
        json!([{"type": "tool_result", "tool_use_id": "toolu_1",
                "content": "rosetta-mock", "is_error": false}]),
        json!({"toolUseResult": {"stdout": "rosetta-mock", "stderr": ""}}),
    )]);

    assert!(messages(&ops).is_empty(), "a report-back is not a turn");
    // The line opens the file, so it also carries the first workspace fix.
    let [
        IngestOp::WorkspaceObs(_),
        IngestOp::ToolResult {
            tool_id,
            result,
            payload,
            is_error,
            at,
            ..
        },
    ] = &ops[..]
    else {
        panic!("expected exactly one tool result, got {ops:#?}");
    };
    assert_eq!(tool_id, "toolu_1");
    assert_eq!(result.as_deref(), Some("rosetta-mock"));
    assert_eq!(payload.as_ref().unwrap()["stdout"], json!("rosetta-mock"));
    assert_eq!(*is_error, Some(false));
    assert_eq!(*at, Some(1787625320000));
}

#[test]
fn a_turn_with_text_and_a_result_keeps_both() {
    let (ops, _) = run(&[user(
        json!([
            {"type": "tool_result", "tool_use_id": "toolu_1", "content": [
                {"type": "text", "text": "first"},
                {"type": "image", "source": {}},
                {"type": "text", "text": "second"}]},
            {"type": "text", "text": "and here is my follow-up"},
        ]),
        json!({}),
    )]);

    let rows = messages(&ops);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].kind, MessageKind::User);
    assert_eq!(rows[0].content.as_deref(), Some("and here is my follow-up"));

    let IngestOp::ToolResult { result, .. } = &ops[2] else {
        panic!("expected a tool result after the turn, got {ops:#?}");
    };
    // Block-array results keep their text and drop what has none.
    assert_eq!(result.as_deref(), Some("first\nsecond"));
}

#[test]
fn an_image_only_turn_is_still_a_turn() {
    let (ops, _) = run(&[user(
        json!([{"type": "image", "source": {"type": "base64"}}]),
        json!({}),
    )]);
    let rows = messages(&ops);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].kind, MessageKind::User);
    assert_eq!(rows[0].content, None);
}

#[test]
fn system_records_become_rows_keyed_by_subtype() {
    let (ops, _) = run(&[json!({
        "type": "system",
        "subtype": "compact_boundary",
        "content": "conversation compacted",
        "parentUuid": UUID,
        "isSidechain": false,
        "uuid": "33333333-3333-3333-3333-333333333333",
        "timestamp": "2026-08-25T02:35:21.000Z",
        "sessionId": "session-under-test",
        "version": "2.1.241",
        "cwd": CWD,
        "gitBranch": "main",
        "userType": "external",
        "compactMetadata": {"trigger": "auto"},
    })]);

    let rows = messages(&ops);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].kind, MessageKind::System);
    assert_eq!(rows[0].subtype.as_deref(), Some("compact_boundary"));
    assert_eq!(rows[0].content.as_deref(), Some("conversation compacted"));
    assert!(drift(&ops).is_empty(), "compactMetadata is a typed field");
}

#[test]
fn every_ai_title_folds_onto_one_summary_row() {
    let (ops, _) = run(&[
        json!({"type": "ai-title", "aiTitle": "first guess", "sessionId": "session-under-test"}),
        json!({"type": "ai-title", "aiTitle": "better title", "sessionId": "session-under-test"}),
    ]);

    let rows = messages(&ops);
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|row| row.kind == MessageKind::Summary));
    assert_eq!(rows[1].content.as_deref(), Some("better title"));
    // The title is a session singleton: the later record upserts the earlier.
    assert_eq!(rows[0].timeline_id, rows[1].timeline_id);
}

#[test]
fn workspace_observations_record_transitions_only() {
    let elsewhere = {
        let mut line = assistant(json!([{"type": "text", "text": "x"}]), None);
        line["cwd"] = json!("/tmp/other");
        line
    };
    let (ops, _) = run(&[
        assistant(json!([{"type": "text", "text": "a"}]), None),
        assistant(json!([{"type": "text", "text": "b"}]), None),
        elsewhere,
    ]);

    let observed: Vec<&str> = ops
        .iter()
        .filter_map(|op| match op {
            IngestOp::WorkspaceObs(row) => Some(row.cwd.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(observed, [CWD, "/tmp/other"]);
}

#[test]
fn a_type_the_canonical_declares_is_handled_not_drift() {
    // `attachment` is the most common record in the corpus and the canonical
    // knows it; we simply project nothing from it. Reporting it as drift would
    // raise the alarm on every ingest forever.
    let (ops, statuses) = run(&[json!({
        "type": "attachment",
        "attachment": {"type": "file"},
        "uuid": "44444444-4444-4444-4444-444444444444",
        "sessionId": "session-under-test",
    })]);
    assert_eq!(statuses, [LineStatus::Parsed]);
    assert!(ops.is_empty());
}

#[test]
fn an_undeclared_type_is_soft_drift() {
    let (ops, statuses) = run(&[json!({
        "type": "telepathy",
        "sessionId": "session-under-test",
        "version": "9.9.9",
        "timestamp": "2026-08-25T02:35:19.072Z",
    })]);

    assert_eq!(statuses, [LineStatus::UnknownType]);
    let facts = drift(&ops);
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0].error_code, "unknown_type");
    assert_eq!(facts[0].record_kind, "telepathy");
    assert_eq!(facts[0].agent_version, "9.9.9");
    assert_eq!(facts[0].stream, "transcript");
    assert_eq!(facts[0].at, Some(1787625319072));
    assert!(!facts[0].shape_hash.is_empty());
}

#[test]
fn a_known_type_that_stops_parsing_is_the_alarm() {
    // `message` is required on an assistant record; losing it is exactly the
    // release-broke-the-schema case the raw types exist to catch.
    let (ops, statuses) = run(&[json!({
        "type": "assistant",
        "parentUuid": null,
        "isSidechain": false,
        "uuid": UUID,
        "timestamp": "2026-08-25T02:35:19.072Z",
        "sessionId": "session-under-test",
        "version": "2.1.241",
        "cwd": CWD,
        "gitBranch": "main",
        "userType": "external",
    })]);

    assert_eq!(
        statuses,
        [LineStatus::HardError {
            code: "parse_failed".to_owned()
        }]
    );
    let facts = drift(&ops);
    assert_eq!(facts[0].error_code, "parse_failed");
    assert_eq!(facts[0].record_kind, "assistant");
}

#[test]
fn unfamiliar_fields_are_reported_once_per_line() {
    let mut line = assistant(json!([{"type": "text", "text": "hi"}]), None);
    line["brandNewField"] = json!(1);
    line["message"]["anotherNewField"] = json!(2);
    let (ops, statuses) = run(&[line]);

    assert_eq!(
        statuses,
        [LineStatus::Parsed],
        "unknown fields never fail a line"
    );
    assert_eq!(messages(&ops).len(), 1, "the row still lands");
    let facts = drift(&ops);
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0].error_code, "unknown_fields");
    assert_eq!(facts[0].record_kind, "assistant");
}

#[test]
fn malformed_lines_never_panic() {
    let adapter = ClaudeCodeAdapter;
    let mut ctx = NormalizeCtx::new("runtime", "session");
    ctx.state = adapter.init_state();

    for line in [
        &b"not json at all"[..],
        b"{\"type\":\"user\"",
        &[0xff, 0xfe][..],
    ] {
        let outcome = adapter.ingest_line(&mut ctx, "transcript", line);
        assert!(matches!(outcome.status, LineStatus::HardError { .. }));
        assert_eq!(drift(&outcome.ops).len(), 1);
    }
    // Blank lines are file padding, not records.
    for line in [&b""[..], b"   ", b"\n"] {
        let outcome = adapter.ingest_line(&mut ctx, "transcript", line);
        assert_eq!(outcome.status, LineStatus::Parsed);
        assert!(outcome.ops.is_empty());
    }
}

#[test]
fn replay_is_deterministic() {
    let lines = [
        assistant(
            json!([{"type": "text", "text": "hi"}]),
            Some(usage(1, 2, 0)),
        ),
        user(json!("thanks"), json!({})),
    ];
    let (first, _) = run(&lines);
    let (second, _) = run(&lines);
    assert_eq!(first, second);
}
