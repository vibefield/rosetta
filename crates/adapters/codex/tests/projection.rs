//! The negative space the fixture corpus cannot cover.
//!
//! `golden_ops.rs` pins what the committed corpus becomes; these tests pin what
//! happens to records it does NOT contain — a record type from a future
//! release, a line that will not parse, and the repeated `token_count` events a
//! live session emits mid-response. Synthetic lines here make no schema claim:
//! the ground truth stays the only source for what codex actually writes.

use rosetta_adapter_codex::CodexAdapter;
use rosetta_adapter_support::adapter::{AgentAdapter, LineOutcome, LineStatus, NormalizeCtx};
use rosetta_core::rows::{DriftFact, MessageKind, MessageRow, TokenSnapshot};
use rosetta_core::{IngestOp, ids};
use serde_json::{Value, json};

const STAMP: &str = "2026-08-25T02:35:03.577Z";

fn context() -> NormalizeCtx {
    let runtime_id = ids::runtime_id("codex", "~/.codex");
    let session_id = ids::session_id(&runtime_id, "01a036c5-0000-0000-0000-000000000000");
    let mut ctx = NormalizeCtx::new(runtime_id, session_id);
    ctx.state = CodexAdapter.init_state();
    ctx
}

fn ingest(ctx: &mut NormalizeCtx, ordinal: u64, record_type: &str, payload: Value) -> LineOutcome {
    let line = serde_json::to_vec(&json!({
        "timestamp": STAMP,
        "ordinal": ordinal,
        "type": record_type,
        "payload": payload,
    }))
    .unwrap();
    CodexAdapter.ingest_line(ctx, "rollout", &line)
}

/// An assistant message — the row usage keys to.
fn agent_row(ctx: &mut NormalizeCtx, ordinal: u64, text: &str) -> MessageRow {
    let outcome = ingest(
        ctx,
        ordinal,
        "response_item",
        json!({
            "type": "message",
            "id": format!("msg_{ordinal}"),
            "role": "assistant",
            "content": [{"type": "output_text", "text": text}],
            "internal_chat_message_metadata_passthrough": {"turn_id": "turn"},
        }),
    );
    match outcome.ops.as_slice() {
        [IngestOp::Message(row)] => (**row).clone(),
        other => panic!("expected one message row, got {other:?}"),
    }
}

fn agent_message(ctx: &mut NormalizeCtx, ordinal: u64, text: &str) -> String {
    agent_row(ctx, ordinal, text).timeline_id
}

/// A `token_count` carrying CUMULATIVE session totals.
fn token_count(ctx: &mut NormalizeCtx, ordinal: u64, input: i64, output: i64) -> Vec<IngestOp> {
    ingest(
        ctx,
        ordinal,
        "event_msg",
        json!({
            "type": "token_count",
            "info": {
                "total_token_usage": {
                    "input_tokens": input,
                    "cached_input_tokens": 0,
                    "cache_write_input_tokens": 0,
                    "output_tokens": output,
                    "reasoning_output_tokens": 0,
                    "total_tokens": input + output,
                },
            },
        }),
    )
    .ops
}

fn usage(ops: &[IngestOp]) -> (String, TokenSnapshot) {
    match ops {
        [
            IngestOp::Usage {
                response, tokens, ..
            },
        ] => (response.clone(), *tokens),
        other => panic!("expected one usage op, got {other:?}"),
    }
}

fn drifts(ops: &[IngestOp]) -> Vec<&DriftFact> {
    ops.iter()
        .filter_map(|op| match op {
            IngestOp::Drift(fact) => Some(fact),
            _ => None,
        })
        .collect()
}

#[test]
fn repeated_token_counts_never_double_count() {
    // A live response gets several token_count events. Each carries the running
    // SESSION total, and each op carries the running RESPONSE total — the
    // writer applies (new − old), so re-reading the same lines is idempotent.
    let mut ctx = context();
    let response = agent_message(&mut ctx, 1, "hello");

    let (first, tokens) = usage(&token_count(&mut ctx, 2, 100, 10));
    assert_eq!(first, response);
    assert_eq!((tokens.input, tokens.output), (Some(100), Some(10)));

    let (second, tokens) = usage(&token_count(&mut ctx, 3, 150, 20));
    assert_eq!(second, response, "both snapshots key to the same response");
    // The running total, NOT the 50/10 delta: the op replaces, it does not add.
    assert_eq!((tokens.input, tokens.output), (Some(150), Some(20)));
}

#[test]
fn cumulative_counters_split_across_responses() {
    // Two responses in one session: the second gets the DIFFERENCE, so the
    // session's counters never land on a row twice.
    let mut ctx = context();
    let first_response = agent_message(&mut ctx, 1, "hello");
    let (keyed, tokens) = usage(&token_count(&mut ctx, 2, 100, 10));
    assert_eq!(keyed, first_response);
    assert_eq!((tokens.input, tokens.output), (Some(100), Some(10)));

    let second_response = agent_message(&mut ctx, 3, "goodbye");
    let (keyed, tokens) = usage(&token_count(&mut ctx, 4, 260, 35));
    assert_eq!(keyed, second_response);
    assert_eq!((tokens.input, tokens.output), (Some(160), Some(25)));
}

#[test]
fn usage_reported_before_a_response_row_is_carried_forward() {
    // The committed tool-use fixture reports tokens before the first agent
    // message; a session that reports them before ANY response row must not
    // lose them either.
    let mut ctx = context();
    assert!(
        token_count(&mut ctx, 1, 100, 10).is_empty(),
        "nothing to key a snapshot to yet"
    );

    let response = agent_message(&mut ctx, 2, "hello");
    let (keyed, tokens) = usage(&token_count(&mut ctx, 3, 150, 20));
    assert_eq!(keyed, response);
    // 150/20, not 50/10: the carried total is attributed, not dropped.
    assert_eq!((tokens.input, tokens.output), (Some(150), Some(20)));
}

#[test]
fn codex_input_counter_is_split_by_subtraction() {
    // `input_tokens` INCLUDES `cached_input_tokens`; the canonical model keeps
    // fresh input and cache reads apart.
    let mut ctx = context();
    let response = agent_message(&mut ctx, 1, "hello");
    let ops = ingest(
        &mut ctx,
        2,
        "event_msg",
        json!({
            "type": "token_count",
            "info": {
                "total_token_usage": {
                    "input_tokens": 1000,
                    "cached_input_tokens": 800,
                    "cache_write_input_tokens": 64,
                    "output_tokens": 50,
                    "reasoning_output_tokens": 30,
                    "total_tokens": 1050,
                },
            },
        }),
    )
    .ops;

    let (keyed, tokens) = usage(&ops);
    assert_eq!(keyed, response);
    assert_eq!(tokens.input, Some(200));
    assert_eq!(tokens.cache_read, Some(800));
    assert_eq!(tokens.cache_write, Some(64));
    assert_eq!(tokens.thinking, Some(30));
}

#[test]
fn compacted_projects_only_the_checkpoint_summary() {
    let mut ctx = context();
    let outcome = ingest(
        &mut ctx,
        1,
        "compacted",
        json!({
            "message": "Earlier work, compacted.",
            "replacement_history": [{
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": "do not duplicate me"}],
            }],
            "window_number": 1,
        }),
    );
    assert_eq!(outcome.status, LineStatus::Parsed);
    assert!(drifts(&outcome.ops).is_empty());
    match outcome.ops.as_slice() {
        [IngestOp::Message(row)] => {
            assert_eq!(row.kind, MessageKind::Summary);
            assert_eq!(row.subtype.as_deref(), Some("compacted"));
            assert_eq!(row.content.as_deref(), Some("Earlier work, compacted."));
        }
        other => panic!("expected only one summary row, got {other:?}"),
    }
}

#[test]
fn a_truly_unknown_top_level_record_is_soft_drift() {
    let mut ctx = context();
    let outcome = ingest(&mut ctx, 1, "wormhole", json!({"message": "…"}));
    assert_eq!(outcome.status, LineStatus::UnknownType);
    let drift = drifts(&outcome.ops);
    assert_eq!(drift.len(), 1);
    assert_eq!(drift[0].error_code, "unknown_type");
    assert_eq!(drift[0].record_kind, "wormhole");
}

#[test]
fn current_reasoning_and_agent_message_shapes_are_visible_without_drift() {
    let mut ctx = context();
    let reasoning = ingest(
        &mut ctx,
        1,
        "response_item",
        json!({
            "type": "reasoning",
            "id": "rs_1",
            "summary": [{"type": "summary_text", "text": "Checked the evidence."}],
            "encrypted_content": "opaque",
            "internal_chat_message_metadata_passthrough": {"turn_id": "turn"},
        }),
    );
    assert!(drifts(&reasoning.ops).is_empty());
    match reasoning.ops.as_slice() {
        [IngestOp::Message(row)] => {
            assert_eq!(row.kind, MessageKind::Thinking);
            assert_eq!(row.content.as_deref(), Some("Checked the evidence."));
        }
        other => panic!("expected one thinking row, got {other:?}"),
    }

    let delivery = ingest(
        &mut ctx,
        2,
        "response_item",
        json!({
            "type": "agent_message",
            "id": "amsg_1",
            "author": "/root/worker",
            "recipient": "/root",
            "content": [{"type": "input_text", "text": "Work complete."}],
        }),
    );
    assert!(drifts(&delivery.ops).is_empty());
    match delivery.ops.as_slice() {
        [IngestOp::Message(row)] => {
            assert_eq!(row.kind, MessageKind::System);
            assert_eq!(row.subtype.as_deref(), Some("agent_message"));
            assert_eq!(row.content.as_deref(), Some("Work complete."));
        }
        other => panic!("expected one system row, got {other:?}"),
    }
}

#[test]
fn declared_subtypes_are_silent_and_undeclared_ones_drift() {
    let mut ctx = context();
    // Declared by the canonical, deliberately unprojected: no ops, no alarm.
    let outcome = ingest(
        &mut ctx,
        1,
        "event_msg",
        json!({"type": "task_started", "turn_id": "turn"}),
    );
    assert_eq!(outcome.status, LineStatus::Parsed);
    assert!(outcome.ops.is_empty());

    // Not in the canonical's vocabulary: the adapter is behind its ground truth.
    let outcome = ingest(&mut ctx, 2, "event_msg", json!({"type": "wormhole_opened"}));
    assert_eq!(outcome.status, LineStatus::UnknownType);
    let drift = drifts(&outcome.ops);
    assert_eq!(drift.len(), 1);
    assert_eq!(drift[0].error_code, "unknown_subtype");
    assert_eq!(drift[0].record_kind, "event_msg/wormhole_opened");
}

#[test]
fn a_novel_field_on_a_known_record_is_one_drift_fact() {
    // Permissive parsing keeps the line; the new field still has to be visible,
    // aggregated by shape rather than one row per record.
    let mut ctx = context();
    let outcome = ingest(
        &mut ctx,
        1,
        "response_item",
        json!({
            "type": "message",
            "id": "msg_1",
            "role": "user",
            "content": [{"type": "input_text", "text": "hi"}],
            "internal_chat_message_metadata_passthrough": {"turn_id": "turn"},
            "attachments": ["something new"],
        }),
    );
    assert_eq!(outcome.status, LineStatus::Parsed);
    let drift = drifts(&outcome.ops);
    assert_eq!(drift.len(), 1);
    assert_eq!(drift[0].error_code, "unknown_fields");
    assert_eq!(drift[0].record_kind, "response_item/message");
    assert_eq!(
        drift[0].shape_hash,
        ids::shape_hash(&[".payload.attachments".to_owned()])
    );
    // The row itself still lands.
    assert!(
        outcome
            .ops
            .iter()
            .any(|op| matches!(op, IngestOp::Message(_)))
    );
}

#[test]
fn a_line_that_will_not_parse_is_a_hard_error() {
    let mut ctx = context();
    let outcome = CodexAdapter.ingest_line(&mut ctx, "rollout", b"{ this is not json");
    assert_eq!(
        outcome.status,
        LineStatus::HardError {
            code: "invalid_json".to_owned()
        }
    );
    assert_eq!(drifts(&outcome.ops).len(), 1);

    // A declared typed field whose representation moved is the louder failure:
    // it means the ground truth is stale, not that a file was torn. Missing
    // version-specific context fields are deliberately accepted.
    let outcome = ingest(
        &mut ctx,
        1,
        "turn_context",
        json!({"turn_id": "turn", "cwd": 42}),
    );
    assert_eq!(
        outcome.status,
        LineStatus::HardError {
            code: "parse_failed".to_owned()
        }
    );
    assert_eq!(drifts(&outcome.ops)[0].record_kind, "turn_context");
}

#[test]
fn blank_lines_are_not_records() {
    let mut ctx = context();
    let outcome = CodexAdapter.ingest_line(&mut ctx, "rollout", b"");
    assert_eq!(outcome.status, LineStatus::Parsed);
    assert!(outcome.ops.is_empty());
}

#[test]
fn a_tool_result_merges_by_call_id() {
    let mut ctx = context();
    let call = ingest(
        &mut ctx,
        1,
        "response_item",
        json!({
            "type": "function_call",
            "id": "fc_1",
            "name": "exec_command",
            "arguments": r#"{"cmd":"echo hi"}"#,
            "call_id": "call_1",
            "internal_chat_message_metadata_passthrough": {"turn_id": "turn"},
        }),
    );
    let tool = match call.ops.as_slice() {
        [IngestOp::Message(row)] => row.tool.clone().expect("tool call"),
        other => panic!("expected one tool row, got {other:?}"),
    };
    assert_eq!(tool.tool_id.as_deref(), Some("call_1"));
    assert_eq!(tool.input, json!({"cmd": "echo hi"}));
    // The result arrives on its own record and merges later, by tool id.
    assert_eq!(tool.result, None);

    let output = ingest(
        &mut ctx,
        2,
        "response_item",
        json!({
            "type": "function_call_output",
            "id": "fco_1",
            "call_id": "call_1",
            "output": "hi\n",
            "internal_chat_message_metadata_passthrough": {"turn_id": "turn"},
        }),
    );
    match output.ops.as_slice() {
        [
            IngestOp::ToolResult {
                tool_id,
                result,
                payload,
                is_error,
                ..
            },
        ] => {
            assert_eq!(tool_id, "call_1");
            assert_eq!(result.as_deref(), Some("hi\n"));
            // The canonical types `output` as a bare string and declares no
            // status field: success is unknown, never guessed.
            assert_eq!(*payload, None);
            assert_eq!(*is_error, None);
        }
        other => panic!("expected one tool result, got {other:?}"),
    }
}

#[test]
fn a_custom_tool_call_keeps_its_freeform_input_and_pairs_with_its_output() {
    let mut ctx = context();
    let patch = "*** Begin Patch\n*** Add File: added.txt\n+hello\n*** End Patch";
    let call = ingest(
        &mut ctx,
        1,
        "response_item",
        json!({
            "type": "custom_tool_call",
            "id": "ctc_1",
            "status": "completed",
            "call_id": "call_1",
            "name": "apply_patch",
            "input": patch,
        }),
    );
    assert_eq!(call.status, LineStatus::Parsed);
    assert!(drifts(&call.ops).is_empty());
    match call.ops.as_slice() {
        [IngestOp::Message(row), IngestOp::FileEdit(edit)] => {
            let tool = row.tool.as_ref().expect("custom tool call");
            assert_eq!(tool.tool_id.as_deref(), Some("call_1"));
            assert_eq!(tool.name, "apply_patch");
            assert_eq!(tool.input, json!(patch));
            assert_eq!(edit.message_timeline_id.as_deref(), Some(&*row.timeline_id));
            assert_eq!(edit.path, "added.txt");
        }
        other => panic!("expected a tool row and file edit, got {other:?}"),
    }

    let output = ingest(
        &mut ctx,
        2,
        "response_item",
        json!({
            "type": "custom_tool_call_output",
            "call_id": "call_1",
            "name": "apply_patch",
            "output": "Done!",
        }),
    );
    assert_eq!(output.status, LineStatus::Parsed);
    assert!(drifts(&output.ops).is_empty());
    match output.ops.as_slice() {
        [
            IngestOp::ToolResult {
                tool_id, result, ..
            },
        ] => {
            assert_eq!(tool_id, "call_1");
            assert_eq!(result.as_deref(), Some("Done!"));
        }
        other => panic!("expected one custom tool result, got {other:?}"),
    }
}

#[test]
fn modern_block_tool_results_keep_text_and_structure() {
    let mut ctx = context();
    let output = ingest(
        &mut ctx,
        1,
        "response_item",
        json!({
            "type": "custom_tool_call_output",
            "call_id": "call_1",
            "output": [
                {"type": "input_text", "text": "first"},
                {"type": "input_text", "text": "second"},
            ],
        }),
    );
    match output.ops.as_slice() {
        [
            IngestOp::ToolResult {
                tool_id,
                result,
                payload,
                ..
            },
        ] => {
            assert_eq!(tool_id, "call_1");
            assert_eq!(result.as_deref(), Some("first\nsecond"));
            assert_eq!(
                *payload,
                Some(json!([
                    {"type": "input_text", "text": "first"},
                    {"type": "input_text", "text": "second"},
                ]))
            );
        }
        other => panic!("expected one tool result, got {other:?}"),
    }
}

#[test]
fn legacy_records_without_ordinals_use_source_offsets() {
    let mut ctx = context();
    ctx.src_offset = Some(1234);
    let line = serde_json::to_vec(&json!({
        "timestamp": STAMP,
        "type": "response_item",
        "payload": {
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": "legacy"}],
        },
    }))
    .unwrap();
    let outcome = CodexAdapter.ingest_line(&mut ctx, "rollout", &line);
    let row = match outcome.ops.as_slice() {
        [IngestOp::Message(row)] => row,
        other => panic!("expected one message row, got {other:?}"),
    };
    assert_eq!(outcome.status, LineStatus::Parsed);
    assert_eq!(
        row.timeline_id,
        ids::timeline_id(&ctx.session_id, "rollout-offset#1234")
    );
}

#[test]
fn a_moved_working_directory_is_observed_once() {
    let mut ctx = context();
    let turn = |cwd: &str| {
        json!({
            "turn_id": "turn",
            "cwd": cwd,
            "model": "mock-model",
            "approval_policy": "never",
            "approvals_reviewer": "user",
            "current_date": "2026-08-24",
            "multi_agent_version": "v1",
            "personality": "none",
            "realtime_active": false,
            "summary": "auto",
            "timezone": "UTC",
            "collaboration_mode": {"mode": "default", "settings": {"model": "gpt-5", "reasoning_effort": "high"}},
            "permission_profile": {"type": "disabled"},
            "sandbox_policy": {"type": "read-only"},
            "workspace_roots": [],
        })
    };

    let first = ingest(&mut ctx, 1, "turn_context", turn("/w/one"));
    assert_eq!(first.ops.len(), 1, "the first cwd is always an observation");

    // Same directory next turn: nothing changed, nothing to observe.
    let repeat = ingest(&mut ctx, 2, "turn_context", turn("/w/one"));
    assert!(repeat.ops.is_empty());

    let moved = ingest(&mut ctx, 3, "turn_context", turn("/w/two"));
    match moved.ops.as_slice() {
        [IngestOp::WorkspaceObs(row)] => assert_eq!(row.cwd, "/w/two"),
        other => panic!("expected one workspace observation, got {other:?}"),
    }

    // turn_context is also where model and effort come from — codex writes them
    // once per turn, so they stamp the rows that FOLLOW.
    let row = agent_row(&mut ctx, 4, "hello");
    assert_eq!(row.model.as_deref(), Some("gpt-5"));
    assert_eq!(row.effort.as_deref(), Some("high"));
}

#[test]
fn only_assistant_rows_carry_a_model() {
    // No model produced a user turn, and saying otherwise would put the
    // assistant's model on the user's words.
    let mut ctx = context();
    let outcome = ingest(
        &mut ctx,
        1,
        "response_item",
        json!({
            "type": "message",
            "id": "msg_1",
            "role": "user",
            "content": [{"type": "input_text", "text": "hi"}],
            "internal_chat_message_metadata_passthrough": {"turn_id": "turn"},
        }),
    );
    match outcome.ops.as_slice() {
        [IngestOp::Message(row)] => {
            assert_eq!(row.kind, MessageKind::User);
            assert_eq!(row.model, None);
            assert_eq!(row.content.as_deref(), Some("hi"));
        }
        other => panic!("expected one message row, got {other:?}"),
    }
}
