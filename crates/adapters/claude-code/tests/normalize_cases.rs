//! Normalization cases the committed fixture corpus does not reach.
//!
//! The farm's scenarios exercise plain chat and one Bash call; the shapes below
//! are taken from the canonical ground truth's facets (`assistant-content-blocks`,
//! `tool-input`, `user-content-blocks`, `system-subtypes`) — the SPEC, never
//! intuition. They are normalization claims, not schema claims: what the raw
//! types accept stays the fixture corpus's business (`fixtures_replay.rs`).

use rosetta_adapter_claude_code::ClaudeCodeAdapter;
use rosetta_adapter_support::{AgentAdapter, LineStatus, NormalizeCtx};
use rosetta_core::rows::{DriftFact, EditKind, FileEditRow, TitleKind};
use rosetta_core::timeline::{
    AttachmentKind, Notice, TimelineBody, TimelineEntry, ToolAction, ToolStatus,
};
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

fn run_with_checkpoint(lines: &[Value], split: usize) -> Vec<IngestOp> {
    let adapter = ClaudeCodeAdapter;
    let runtime_id = ids::runtime_id(adapter.kind(), &adapter.layout().data_root);
    let session_id = ids::session_id(&runtime_id, "session-under-test");
    let mut before = NormalizeCtx::new(&runtime_id, &session_id);
    before.state = adapter.init_state();
    let mut ops = Vec::new();
    for line in &lines[..split] {
        ops.extend(
            adapter
                .ingest_line(&mut before, "transcript", line.to_string().as_bytes())
                .ops,
        );
    }
    let checkpoint = adapter
        .checkpoint_state(before.state.as_deref())
        .unwrap()
        .expect("claude checkpoint");
    let mut after = NormalizeCtx::new(&runtime_id, &session_id);
    after.next_ordinal = before.next_ordinal;
    after.state = adapter.restore_state(Some(&checkpoint)).unwrap();
    for line in &lines[split..] {
        ops.extend(
            adapter
                .ingest_line(&mut after, "transcript", line.to_string().as_bytes())
                .ops,
        );
    }
    ops
}

/// An assistant record with the given content blocks, in the shape the
/// canonical records for the `assistant` type.
fn assistant(content: Value, usage: Option<Value>) -> Value {
    assistant_record("msg_1", UUID, content, usage)
}

fn assistant_record(response_id: &str, uuid: &str, content: Value, usage: Option<Value>) -> Value {
    let mut message = json!({
        "id": response_id,
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
        "uuid": uuid,
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
        TimelineBody::ToolUse { .. } | TimelineBody::Notice { .. } => None,
    }
}

fn tool(row: &TimelineEntry) -> Option<&rosetta_core::ToolInteraction> {
    match &row.body {
        TimelineBody::ToolUse { tool } => Some(tool),
        _ => None,
    }
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
    let kinds: Vec<&str> = rows.iter().map(|row| body_kind(row)).collect();
    assert_eq!(kinds, ["thinking", "agent", "tool_use"]);
    assert_eq!(
        rows.iter().map(|row| row.ordinal).collect::<Vec<_>>(),
        [0, 1, 2]
    );

    // Split rows share the record's uuid but never its timeline id.
    assert!(
        rows.iter()
            .all(|row| row.native_id.as_deref() == Some(UUID))
    );
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
            .all(|row| row.provenance.agent_version.as_deref() == Some("2.1.241"))
    );

    // Usage is reported once per response and lands on the text row.
    assert!(rows[0].usage.is_empty());
    assert!(rows[2].usage.is_empty());
    let tokens = rows[1].usage;
    assert_eq!(tokens.input, Some(100));
    assert_eq!(tokens.output, Some(20));
    assert_eq!(tokens.cache_read, Some(9));
    assert_eq!(tokens.cache_write, Some(7));
    assert_eq!(tokens.thinking, Some(5));

    let tool = tool(rows[2]).expect("tool call");
    assert_eq!(tool.native.name, "Bash");
    assert_eq!(tool.native.id.as_deref(), Some("toolu_1"));
    let ToolAction::Shell { command, .. } = &tool.action else {
        panic!("Bash must project as shell")
    };
    assert_eq!(command.as_preview(), "ls");
    assert_eq!(tool.status.value, ToolStatus::Pending);
}

#[test]
fn assistant_records_share_response_block_space_across_checkpoint_resume() {
    let repeated_usage = usage(100, 20, 5);
    let lines = vec![
        assistant_record(
            "msg_split",
            "11111111-1111-1111-1111-111111111101",
            json!([{"type": "thinking", "thinking": "first", "signature": "sig"}]),
            Some(repeated_usage.clone()),
        ),
        assistant_record(
            "msg_split",
            "11111111-1111-1111-1111-111111111102",
            json!([{"type": "text", "text": "second"}]),
            Some(repeated_usage.clone()),
        ),
        assistant_record(
            "msg_split",
            "11111111-1111-1111-1111-111111111103",
            json!([{"type": "tool_use", "id": "toolu_split", "name": "Bash",
                    "input": {"command": "true"}}]),
            Some(repeated_usage),
        ),
    ];

    let (one_shot, statuses) = run(&lines);
    assert!(statuses.iter().all(|status| *status == LineStatus::Parsed));
    let rows = messages(&one_shot);
    assert_eq!(rows.len(), 3);
    assert_eq!(
        rows.iter().map(|row| row.block_index).collect::<Vec<_>>(),
        [Some(0), Some(1), Some(2)]
    );
    assert!(rows.iter().all(|row| {
        row.response
            .as_ref()
            .is_some_and(|response| response.id == "msg_split")
    }));
    assert_eq!(
        rows.iter().filter(|row| !row.usage.is_empty()).count(),
        1,
        "repeated native response usage must become one canonical fact"
    );
    assert_eq!(rows[0].usage.input, Some(100));
    assert!(
        !one_shot
            .iter()
            .any(|op| { matches!(op, IngestOp::Usage { .. } | IngestOp::SessionUsage(_)) })
    );

    let adapter = ClaudeCodeAdapter;
    let runtime_id = ids::runtime_id(adapter.kind(), &adapter.layout().data_root);
    let mut before = NormalizeCtx::new(
        &runtime_id,
        ids::session_id(&runtime_id, "session-under-test"),
    );
    before.state = adapter.init_state();
    let mut resumed = Vec::new();
    for line in &lines[..1] {
        resumed.extend(
            adapter
                .ingest_line(&mut before, "transcript", line.to_string().as_bytes())
                .ops,
        );
    }
    let checkpoint = adapter
        .checkpoint_state(before.state.as_deref())
        .unwrap()
        .expect("claude checkpoint");
    let mut after = NormalizeCtx::new(
        &runtime_id,
        ids::session_id(&runtime_id, "session-under-test"),
    );
    after.next_ordinal = before.next_ordinal;
    after.state = adapter.restore_state(Some(&checkpoint)).unwrap();
    for line in &lines[1..] {
        resumed.extend(
            adapter
                .ingest_line(&mut after, "transcript", line.to_string().as_bytes())
                .ops,
        );
    }
    assert_eq!(resumed, one_shot);
}

#[test]
fn revisited_response_reuses_native_block_identity_after_checkpoint() {
    let first_uuid = "11111111-1111-1111-1111-111111111301";
    let repeated_uuid = "11111111-1111-1111-1111-111111111302";
    let mut repeated = assistant_record(
        "msg_revisited",
        repeated_uuid,
        json!([{"type": "tool_use", "id": "toolu_revisited", "name": "Bash",
                "input": {"command": "true"}}]),
        None,
    );
    repeated["parentUuid"] = json!("new-branch-parent");
    let lines = vec![
        assistant_record(
            "msg_revisited",
            first_uuid,
            json!([{"type": "text", "text": "first"}]),
            None,
        ),
        assistant_record(
            "msg_revisited",
            repeated_uuid,
            json!([{"type": "tool_use", "id": "toolu_revisited", "name": "Bash",
                    "input": {"command": "true"}}]),
            None,
        ),
        assistant_record(
            "msg_interrupting",
            "11111111-1111-1111-1111-111111111303",
            json!([{"type": "text", "text": "another response"}]),
            None,
        ),
        repeated,
        assistant_record(
            "msg_revisited",
            "11111111-1111-1111-1111-111111111304",
            json!([{"type": "text", "text": "new late block"}]),
            None,
        ),
    ];

    let (one_shot, statuses) = run(&lines);
    assert!(statuses.iter().all(|status| *status == LineStatus::Parsed));
    let rows = messages(&one_shot);
    let revisited = rows
        .iter()
        .filter(|row| {
            row.response
                .as_ref()
                .is_some_and(|response| response.id == "msg_revisited")
        })
        .copied()
        .collect::<Vec<_>>();
    assert_eq!(
        revisited
            .iter()
            .map(|row| row.block_index)
            .collect::<Vec<_>>(),
        [Some(0), Some(1), Some(1), Some(2)]
    );
    assert_eq!(revisited[1].timeline_id, revisited[2].timeline_id);
    assert_eq!(
        revisited[2].parent_native_id.as_deref(),
        Some("new-branch-parent")
    );
    assert_eq!(run_with_checkpoint(&lines, 3), one_shot);
}

#[test]
fn blank_leading_block_keeps_usage_once_without_consuming_a_block_index() {
    let repeated_usage = usage(10, 2, 0);
    let (ops, _) = run(&[
        assistant_record(
            "msg_blank_first",
            "11111111-1111-1111-1111-111111111151",
            json!([{"type": "thinking", "thinking": "", "signature": "sig"}]),
            Some(repeated_usage.clone()),
        ),
        assistant_record(
            "msg_blank_first",
            "11111111-1111-1111-1111-111111111152",
            json!([{"type": "text", "text": "visible"}]),
            Some(repeated_usage),
        ),
    ]);
    let rows = messages(&ops);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].block_index, Some(0));
    assert!(rows[0].usage.is_empty());
    let samples = ops
        .iter()
        .filter_map(|op| match op {
            IngestOp::SessionUsage(sample) => Some(sample),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(samples.len(), 1);
    assert_eq!(samples[0].key, "response:msg_blank_first");
    assert_eq!(samples[0].tokens.input, Some(10));
}

#[test]
fn changed_repeated_response_usage_replaces_the_first_fact() {
    let (ops, _) = run(&[
        assistant_record(
            "msg_corrected",
            "11111111-1111-1111-1111-111111111201",
            json!([{"type": "thinking", "thinking": "first", "signature": "sig"}]),
            Some(usage(10, 2, 0)),
        ),
        assistant_record(
            "msg_corrected",
            "11111111-1111-1111-1111-111111111202",
            json!([{"type": "text", "text": "second"}]),
            Some(usage(11, 3, 0)),
        ),
    ]);
    let rows = messages(&ops);
    assert_eq!(rows[0].usage.input, Some(10));
    let correction = ops.iter().find_map(|op| match op {
        IngestOp::Usage {
            response, tokens, ..
        } => Some((response, tokens)),
        _ => None,
    });
    let (target, tokens) = correction.expect("usage correction");
    assert_eq!(target, &rows[0].timeline_id);
    assert_eq!(tokens.input, Some(11));
    assert_eq!(tokens.output, Some(3));
}

#[test]
fn usage_falls_back_to_the_tool_row_then_to_any_row() {
    let (ops, _) = run(&[assistant(
        json!([{"type": "tool_use", "id": "toolu_1", "name": "Bash", "input": {"command": "ls"}}]),
        Some(usage(10, 2, 0)),
    )]);
    assert!(!messages(&ops)[0].usage.is_empty());

    // An API-error record carries no content at all; its usage must not vanish
    // with the response.
    let (ops, statuses) = run(&[assistant(json!([]), Some(usage(10, 2, 0)))]);
    let rows = messages(&ops);
    assert_eq!(statuses, [LineStatus::Parsed]);
    assert!(rows.is_empty(), "an empty response is not visible prose");
    assert!(ops.iter().any(|op| matches!(op, IngestOp::SessionUsage(_))));
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
            messages(&ops)[0].parent_tool_native_id.as_deref(),
            Some("toolu_spawn"),
            "{name} should anchor a subagent thread"
        );
    }

    let (ops, _) = run(&[assistant(
        json!([{"type": "tool_use", "id": "t", "name": "Bash", "input": {"command": "ls"}}]),
        None,
    )]);
    assert_eq!(messages(&ops)[0].parent_tool_native_id, None);
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
        IngestOp::ToolUpdate {
            correlation_key,
            patch,
            at,
            ..
        },
    ] = &ops[..]
    else {
        panic!("expected exactly one tool result, got {ops:#?}");
    };
    assert_eq!(correlation_key, "toolu_1");
    assert_eq!(
        patch.status.as_ref().map(|status| status.value),
        Some(ToolStatus::Succeeded)
    );
    assert!(patch.failure.is_none(), "successful output is noise");
    assert_eq!(*at, Some(1787625320000));
}

#[test]
fn read_image_results_do_not_cross_the_canonical_boundary() {
    let encoded_png_signature = "iVBORw0KGgo=";
    let (ops, _) = run(&[user(
        json!([{"type": "tool_result", "tool_use_id": "toolu_image"}]),
        json!({"toolUseResult": {"file": {
            "base64": encoded_png_signature,
            "type": "image/png",
            "dimensions": {"originalWidth": 1, "originalHeight": 1}
        }}}),
    )]);

    assert!(
        !ops.iter()
            .any(|op| matches!(op, IngestOp::ProjectionAsset { .. })),
        "a file-read body is neither authored media nor a canonical attachment"
    );
    let patch = ops
        .iter()
        .find_map(|op| match op {
            IngestOp::ToolUpdate { patch, .. } => Some(patch),
            _ => None,
        })
        .expect("typed lifecycle update");
    assert_eq!(
        patch.status.as_ref().map(|status| status.value),
        Some(ToolStatus::Succeeded)
    );
    assert!(patch.failure.is_none());
}

#[test]
fn malformed_or_mislabeled_read_images_are_also_discarded() {
    for (encoded, media_type) in [("not base64", "image/png"), ("iVBORw0KGgo=", "image/jpeg")] {
        let (ops, _) = run(&[user(
            json!([{"type": "tool_result", "tool_use_id": "toolu_image"}]),
            json!({"toolUseResult": {"file": {
                "base64": encoded,
                "type": media_type
            }}}),
        )]);
        assert!(
            !ops.iter()
                .any(|op| matches!(op, IngestOp::ProjectionAsset { .. })),
            "unverified image bytes must not be written"
        );
        assert!(
            ops.iter()
                .any(|op| matches!(op, IngestOp::ToolUpdate { .. }))
        );
    }
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
    assert_eq!(body_kind(rows[0]), "user");
    assert_eq!(text(rows[0]), Some("and here is my follow-up"));

    let IngestOp::ToolUpdate { patch, .. } = &ops[2] else {
        panic!("expected a tool update after the turn, got {ops:#?}");
    };
    assert_eq!(
        patch.status.as_ref().map(|status| status.value),
        Some(ToolStatus::Succeeded)
    );
    assert!(
        patch.failure.is_none(),
        "successful result text is discarded"
    );
}

#[test]
fn valid_authored_image_becomes_a_typed_content_addressed_attachment() {
    let (ops, _) = run(&[user(
        json!([{"type": "image", "source": {
            "type": "base64", "media_type": "image/png", "data": "iVBORw0KGgpib2R5"
        }}]),
        json!({}),
    )]);
    let rows = messages(&ops);
    assert_eq!(rows.len(), 1);
    let TimelineBody::User { text, attachments } = &rows[0].body else {
        panic!("authored media must remain a user turn")
    };
    assert!(text.is_none());
    assert_eq!(attachments.len(), 1);
    assert_eq!(attachments[0].kind, AttachmentKind::Image);
    assert!(
        ops.iter()
            .any(|op| matches!(op, IngestOp::ProjectionAsset { .. }))
    );
}

#[test]
fn system_compaction_becomes_a_closed_notice() {
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
    assert!(matches!(
        rows[0].body,
        TimelineBody::Notice {
            notice: Notice::Compaction { .. }
        }
    ));
    assert!(drift(&ops).is_empty(), "compactMetadata is a typed field");
}

#[test]
fn every_ai_title_projects_as_a_ranked_session_title_fact() {
    let (ops, _) = run(&[
        json!({"type": "ai-title", "aiTitle": "first guess", "sessionId": "session-under-test"}),
        json!({"type": "ai-title", "aiTitle": "better title", "sessionId": "session-under-test"}),
    ]);

    let titles: Vec<_> = ops
        .iter()
        .filter_map(|op| match op {
            IngestOp::SessionTitle(title) => Some(title),
            _ => None,
        })
        .collect();
    assert_eq!(titles.len(), 2);
    assert!(
        titles
            .iter()
            .all(|title| title.kind == TitleKind::Generated)
    );
    assert_eq!(titles[1].title, "better title");
}

#[test]
fn explicit_custom_title_is_a_distinct_session_singleton() {
    let (ops, statuses) = run(&[json!({
        "type": "custom-title",
        "customTitle": "Keep this exact title",
        "sessionId": "session-under-test"
    })]);
    assert_eq!(statuses, [LineStatus::Parsed]);
    let [IngestOp::SessionTitle(title)] = ops.as_slice() else {
        panic!("custom title must be a session singleton fact")
    };
    assert_eq!(title.kind, TitleKind::Explicit);
    assert_eq!(title.title, "Keep this exact title");
    assert!(drift(&ops).is_empty());
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
fn a_declared_unprojected_type_still_reports_new_fields() {
    let (ops, statuses) = run(&[json!({
        "type": "attachment",
        "attachment": {"type": "file"},
        "uuid": "44444444-4444-4444-4444-444444444444",
        "sessionId": "session-under-test",
        "futureField": true,
    })]);
    assert_eq!(statuses, [LineStatus::Parsed]);
    let facts = drift(&ops);
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0].error_code, "unknown_fields");
    assert_eq!(facts[0].record_kind, "attachment");
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
