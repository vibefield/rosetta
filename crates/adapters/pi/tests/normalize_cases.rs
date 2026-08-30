use rosetta_adapter_pi::PiAdapter;
use rosetta_adapter_support::adapter::{AgentAdapter, LineStatus, NormalizeCtx};
use rosetta_core::IngestOp;
use rosetta_core::rows::{MessageKind, MessageRow};
use rosetta_core::timeline::ToolStatus;
use serde_json::{Value, json};

fn assistant(id: &str, parent: &str, text: &str) -> Value {
    json!({
        "type": "message",
        "id": id,
        "parentId": parent,
        "timestamp": "2026-08-25T05:29:08.000Z",
        "message": {
            "role": "assistant",
            "content": [{"type": "text", "text": text}],
            "api": "openai-completions",
            "provider": "mock",
            "model": "mock-model",
            "usage": {
                "input": 10,
                "output": 2,
                "cacheRead": 3,
                "cacheWrite": 4,
                "totalTokens": 19,
                "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0}
            },
            "stopReason": "stop",
            "timestamp": 1_787_635_748_000_u64
        }
    })
}

fn user(id: &str, parent: Option<&str>, text: &str) -> Value {
    json!({
        "type": "message",
        "id": id,
        "parentId": parent,
        "timestamp": "2026-08-25T05:29:07.000Z",
        "message": {
            "role": "user",
            "content": [{"type": "text", "text": text}],
            "timestamp": 1_787_635_747_000_u64
        }
    })
}

fn run(lines: &[Value]) -> (Vec<IngestOp>, Vec<LineStatus>) {
    let adapter = PiAdapter;
    let mut ctx = NormalizeCtx::new("runtime", "session");
    ctx.state = adapter.init_state();
    let mut ops = Vec::new();
    let mut statuses = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        ctx.src_cursor = Some(index as i64 + 1);
        let outcome = adapter.ingest_line(&mut ctx, "session", line.to_string().as_bytes());
        ops.extend(outcome.ops);
        statuses.push(outcome.status);
    }
    (ops, statuses)
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
fn entry_tree_metadata_survives_but_ordinals_stay_presentation_order() {
    let lines = [
        user("root", None, "root"),
        user("branch-a", Some("root"), "branch a"),
        user("deep-a", Some("branch-a"), "deep a"),
        // This sibling is presented after a deeper node on the other branch.
        // Its ordinal must not reset to branch-local 1 or sort by depth.
        user("branch-b", Some("root"), "branch b"),
    ];
    let (ops, statuses) = run(&lines);
    assert!(statuses.iter().all(|status| *status == LineStatus::Parsed));
    let rows = messages(&ops);
    assert_eq!(
        rows.iter().map(|row| row.ordinal).collect::<Vec<_>>(),
        [0, 1, 2, 3]
    );
    assert_eq!(
        rows.iter()
            .map(|row| (row.uuid.as_deref(), row.parent_uuid.as_deref()))
            .collect::<Vec<_>>(),
        [
            (Some("root"), None),
            (Some("branch-a"), Some("root")),
            (Some("deep-a"), Some("branch-a")),
            (Some("branch-b"), Some("root")),
        ]
    );
}

#[test]
fn branch_local_settings_follow_ancestry_not_the_latest_line() {
    let high = json!({
        "type": "thinking_level_change", "id": "high", "parentId": null,
        "timestamp": "2026-08-25T05:29:06Z", "thinkingLevel": "high"
    });
    let low = json!({
        "type": "thinking_level_change", "id": "low", "parentId": null,
        "timestamp": "2026-08-25T05:29:06Z", "thinkingLevel": "low"
    });
    let lines = [
        high,
        user("high-user", Some("high"), "on high branch"),
        low,
        user("low-user", Some("low"), "on low branch"),
        // The most recently presented setting is low, but this row descends
        // from high-user and therefore keeps the high branch's context.
        assistant("high-answer", "high-user", "answer"),
    ];
    let (ops, _) = run(&lines);
    let rows = messages(&ops);
    assert_eq!(
        rows.iter().map(|row| row.ordinal).collect::<Vec<_>>(),
        [0, 1, 2]
    );
    assert_eq!(rows[2].uuid.as_deref(), Some("high-answer"));
    assert_eq!(rows[2].parent_uuid.as_deref(), Some("high-user"));
    assert_eq!(rows[2].effort.as_deref(), Some("high"));
}

#[test]
fn assistant_blocks_split_and_usage_is_per_response() {
    let line = json!({
        "type": "message",
        "id": "response",
        "parentId": "prompt",
        "timestamp": "2026-08-25T05:29:08Z",
        "message": {
            "role": "assistant",
            "content": [
                {"type": "thinking", "thinking": "considering"},
                {"type": "text", "text": "answer"},
                {"type": "toolCall", "id": "call-1", "name": "bash", "arguments": {"command": "echo ok"}}
            ],
            "api": "openai-completions",
            "provider": "mock",
            "model": "mock-model",
            "usage": {
                "input": 10, "output": 2, "cacheRead": 3, "cacheWrite": 4,
                "totalTokens": 19,
                "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0}
            },
            "stopReason": "toolUse",
            "timestamp": 1_787_635_748_000_u64
        }
    });
    let (ops, _) = run(&[line]);
    let rows = messages(&ops);
    assert_eq!(
        rows.iter().map(|row| row.kind).collect::<Vec<_>>(),
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
    assert!(
        rows.iter()
            .all(|row| row.uuid.as_deref() == Some("response"))
    );
    assert_eq!(rows[1].usage.expect("usage on text row").thinking, None);
    assert_eq!(rows[0].usage, None);
    assert_eq!(rows[2].usage, None);
}

#[test]
fn tool_result_merges_without_a_second_timeline_row() {
    let call = json!({
        "type": "message", "id": "call-entry", "parentId": "prompt",
        "timestamp": "2026-08-25T05:29:08Z",
        "message": {
            "role": "assistant",
            "content": [{"type": "toolCall", "id": "call-1", "name": "bash", "arguments": {"command": "echo ok"}}],
            "api": "openai-completions", "provider": "mock", "model": "mock-model",
            "usage": {"input": 1, "output": 1, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 2,
                      "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0}},
            "stopReason": "toolUse", "timestamp": 1_787_635_748_000_u64
        }
    });
    let result = json!({
        "type": "message", "id": "result-entry", "parentId": "call-entry",
        "timestamp": "2026-08-25T05:29:09Z",
        "message": {"role": "toolResult", "toolCallId": "call-1", "toolName": "bash",
                    "content": [{"type": "text", "text": "ok\n"}], "isError": false,
                    "timestamp": 1_787_635_749_000_u64}
    });
    let (ops, _) = run(&[call, result]);
    assert_eq!(messages(&ops).len(), 1);
    let merges: Vec<_> = ops
        .iter()
        .filter(|op| matches!(op, IngestOp::ToolUpdate { .. }))
        .collect();
    assert_eq!(merges.len(), 1);
    assert!(matches!(
        merges[0],
        IngestOp::ToolUpdate { correlation_key, patch, .. }
            if correlation_key == "call-1"
                && patch.status.as_ref().map(|status| status.value) == Some(ToolStatus::Succeeded)
                && patch.failure.is_none()
    ));
}

#[test]
fn state_checkpoint_round_trip_and_split_resume_match_one_shot() {
    let adapter = PiAdapter;
    let lines = [
        json!({"type":"session","version":3,"id":"native","timestamp":"2026-08-25T05:29:06Z","cwd":"<PROJECT>"}),
        json!({"type":"thinking_level_change","id":"think","parentId":null,"timestamp":"2026-08-25T05:29:06Z","thinkingLevel":"high"}),
        user("prompt", Some("think"), "hello"),
        assistant("answer", "prompt", "world"),
    ];

    let mut one_ctx = NormalizeCtx::new("runtime", "session");
    one_ctx.state = adapter.init_state();
    let mut one_shot = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        one_ctx.src_cursor = Some(index as i64 + 1);
        one_shot.extend(
            adapter
                .ingest_line(&mut one_ctx, "session", line.to_string().as_bytes())
                .ops,
        );
    }

    let mut first_ctx = NormalizeCtx::new("runtime", "session");
    first_ctx.state = adapter.init_state();
    let mut resumed_ops = Vec::new();
    for (index, line) in lines[..2].iter().enumerate() {
        first_ctx.src_cursor = Some(index as i64 + 1);
        resumed_ops.extend(
            adapter
                .ingest_line(&mut first_ctx, "session", line.to_string().as_bytes())
                .ops,
        );
    }
    let checkpoint = adapter
        .checkpoint_state(first_ctx.state.as_deref())
        .expect("checkpoint serializes");
    let mut resumed_ctx = NormalizeCtx::new("runtime", "session");
    resumed_ctx.next_ordinal = first_ctx.next_ordinal;
    resumed_ctx.state = adapter
        .restore_state(checkpoint.as_ref())
        .expect("checkpoint restores");
    assert_eq!(
        adapter
            .checkpoint_state(resumed_ctx.state.as_deref())
            .expect("restored state serializes"),
        checkpoint
    );
    for (index, line) in lines[2..].iter().enumerate() {
        resumed_ctx.src_cursor = Some(index as i64 + 3);
        resumed_ops.extend(
            adapter
                .ingest_line(&mut resumed_ctx, "session", line.to_string().as_bytes())
                .ops,
        );
    }
    assert_eq!(resumed_ops, one_shot);
    assert!(
        adapter
            .restore_state(Some(&json!({"entries": []})))
            .is_err()
    );
}

#[test]
fn drift_distinguishes_unknown_types_from_known_parse_failures() {
    let adapter = PiAdapter;
    let mut ctx = NormalizeCtx::new("runtime", "session");
    ctx.state = adapter.init_state();

    let unknown = adapter.ingest_line(&mut ctx, "session", br#"{"type":"future_entry","id":"x"}"#);
    assert_eq!(unknown.status, LineStatus::UnknownType);
    assert!(matches!(unknown.ops.as_slice(), [IngestOp::Drift(_)]));

    let broken = adapter.ingest_line(
        &mut ctx,
        "session",
        br#"{"type":"message","id":"x","parentId":null}"#,
    );
    assert_eq!(
        broken.status,
        LineStatus::HardError {
            code: "parse_failed".to_owned()
        }
    );
    assert!(matches!(broken.ops.as_slice(), [IngestOp::Drift(_)]));

    let declared_no_op = json!({
        "type":"label", "id":"label", "parentId":null,
        "timestamp":"2026-08-25T05:29:06Z", "targetId":"target"
    });
    let outcome = adapter.ingest_line(&mut ctx, "session", declared_no_op.to_string().as_bytes());
    assert_eq!(outcome.status, LineStatus::Parsed);
    assert!(outcome.ops.is_empty());
}
