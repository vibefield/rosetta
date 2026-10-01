//! The negative space the fixture corpus cannot cover.
//!
//! `golden_ops.rs` pins what the committed corpus becomes; these tests pin what
//! happens to records it does NOT contain — a record type from a future
//! release, a line that will not parse, and the repeated `token_count` events a
//! live session emits mid-response. Synthetic lines here make no schema claim:
//! the ground truth stays the only source for what codex actually writes.

use rosetta_adapter_codex::CodexAdapter;
use rosetta_adapter_codex::tool::CURRENT_WRITER_VERSION;
use rosetta_adapter_support::adapter::{AgentAdapter, LineOutcome, LineStatus, NormalizeCtx};
use rosetta_core::rows::{DriftFact, TokenSnapshot};
use rosetta_core::timeline::{
    ResourceRef, StatusBasis, TimelineBody, TimelineEntry, ToolAction, ToolStatus,
};
use rosetta_core::{IngestOp, ids};
use serde_json::{Value, json};

const STAMP: &str = "2026-08-25T02:35:03.577Z";

fn context() -> NormalizeCtx {
    let runtime_id = ids::runtime_id("codex", "~/.codex");
    let session_id = ids::session_id(&runtime_id, "01a036c5-0000-0000-0000-000000000000");
    let mut ctx = NormalizeCtx::new(runtime_id, session_id);
    ctx.agent_version = Some(CURRENT_WRITER_VERSION.to_owned());
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
fn agent_row(ctx: &mut NormalizeCtx, ordinal: u64, text: &str) -> TimelineEntry {
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

fn agent_message(ctx: &mut NormalizeCtx, ordinal: u64, text: &str) -> String {
    agent_row(ctx, ordinal, text).timeline_id
}

fn usage_value(input: i64, output: i64) -> Value {
    json!({
        "input_tokens": input,
        "cached_input_tokens": 0,
        "cache_write_input_tokens": 0,
        "output_tokens": output,
        "reasoning_output_tokens": 0,
        "total_tokens": input + output,
    })
}

/// A `token_count`: the running thread total plus the one request's `last`.
fn token_count(
    ctx: &mut NormalizeCtx,
    ordinal: u64,
    total: (i64, i64),
    last: (i64, i64),
) -> Vec<IngestOp> {
    ingest(
        ctx,
        ordinal,
        "event_msg",
        json!({
            "type": "token_count",
            "info": {
                "total_token_usage": usage_value(total.0, total.1),
                "last_token_usage": usage_value(last.0, last.1),
            },
        }),
    )
    .ops
}

fn ingest_at(
    ctx: &mut NormalizeCtx,
    timestamp: &str,
    ordinal: u64,
    record_type: &str,
    payload: Value,
) -> LineOutcome {
    let line = serde_json::to_vec(&json!({
        "timestamp": timestamp,
        "ordinal": ordinal,
        "type": record_type,
        "payload": payload,
    }))
    .unwrap();
    CodexAdapter.ingest_line(ctx, "rollout", &line)
}

fn session_meta(extra: Value) -> Value {
    let mut meta = json!({
        "id": "01a036c5-0000-0000-0000-000000000000",
        "timestamp": STAMP,
        "cwd": "/workspace",
        "originator": "codex_exec",
        "cli_version": "0.149.1",
        "source": "exec",
        "model_provider": "openai",
    });
    meta.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    meta
}

fn sample(ops: &[IngestOp]) -> TokenSnapshot {
    match ops {
        [IngestOp::SessionUsage(sample)] => sample.tokens.clone(),
        other => panic!("expected one request sample, got {other:?}"),
    }
}

fn usage(ops: &[IngestOp]) -> (String, TokenSnapshot) {
    match ops {
        [
            IngestOp::Usage {
                response, tokens, ..
            },
        ] => (response.clone(), tokens.clone()),
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
fn an_unchanged_total_is_not_a_new_request() {
    // Rate-limit re-emits and recomputes repeat the running total without a
    // request behind them.
    let mut ctx = context();
    let response = agent_message(&mut ctx, 1, "hello");
    let (keyed, tokens) = usage(&token_count(&mut ctx, 2, (100, 10), (100, 10)));
    assert_eq!(keyed, response);
    assert_eq!((tokens.input, tokens.output), (Some(100), Some(10)));
    assert!(token_count(&mut ctx, 3, (100, 10), (100, 10)).is_empty());
}

#[test]
fn each_request_gets_the_difference_on_its_own_row() {
    let mut ctx = context();
    let first_response = agent_message(&mut ctx, 1, "hello");
    let (keyed, tokens) = usage(&token_count(&mut ctx, 2, (100, 10), (100, 10)));
    assert_eq!(keyed, first_response);
    assert_eq!((tokens.input, tokens.output), (Some(100), Some(10)));

    let second_response = agent_message(&mut ctx, 3, "goodbye");
    let (keyed, tokens) = usage(&token_count(&mut ctx, 4, (260, 35), (160, 25)));
    assert_eq!(keyed, second_response);
    assert_eq!((tokens.input, tokens.output), (Some(160), Some(25)));
    assert_eq!(tokens.scope, rosetta_core::UsageScope::Request);
}

#[test]
fn a_request_without_a_row_becomes_a_request_sample() {
    // Poll calls return before any response row: their request is still
    // billed, and must not ride on (or be lost with) another request's row.
    let mut ctx = context();
    let tokens = sample(&token_count(&mut ctx, 1, (100, 10), (100, 10)));
    assert_eq!((tokens.input, tokens.output), (Some(100), Some(10)));

    let response = agent_message(&mut ctx, 2, "hello");
    let (keyed, tokens) = usage(&token_count(&mut ctx, 3, (150, 20), (50, 10)));
    assert_eq!(keyed, response);
    assert_eq!((tokens.input, tokens.output), (Some(50), Some(10)));
}

#[test]
fn an_empty_reasoning_row_never_swallows_its_request() {
    // The pre-2026-09-22 adapter aimed this request at the reasoning row,
    // which projection drops for its empty summary: 13% of Codex tokens.
    let mut ctx = context();
    let outcome = ingest(
        &mut ctx,
        1,
        "response_item",
        json!({"type": "reasoning", "id": "rs_1", "summary": []}),
    );
    assert!(outcome.ops.is_empty(), "empty reasoning is ledger-only");
    let tokens = sample(&token_count(&mut ctx, 2, (300, 30), (300, 30)));
    assert_eq!(tokens.total_tokens(), Some(330));
}

#[test]
fn a_seeded_first_reading_counts_only_its_own_request() {
    // A fork or resume into a new file seeds the thread total from its
    // parent; only `last` belongs to this file.
    let mut ctx = context();
    agent_message(&mut ctx, 1, "hello");
    let (_, tokens) = usage(&token_count(&mut ctx, 2, (90_000, 9_000), (120, 12)));
    assert_eq!((tokens.input, tokens.output), (Some(120), Some(12)));
}

#[test]
fn a_rebased_total_counts_the_request_not_the_whole_reading() {
    let mut ctx = context();
    agent_message(&mut ctx, 1, "one");
    usage(&token_count(&mut ctx, 2, (1_000, 100), (1_000, 100)));
    agent_message(&mut ctx, 3, "two");
    // Compaction re-based the counters: some fields dropped. The old
    // per-component rule re-added a dipped counter's whole running total.
    let (_, tokens) = usage(&token_count(&mut ctx, 4, (500, 150), (60, 6)));
    assert_eq!((tokens.input, tokens.output), (Some(60), Some(6)));
}

#[test]
fn a_forks_copied_head_burst_carries_no_usage() {
    let mut ctx = context();
    ingest_at(
        &mut ctx,
        "2026-08-25T02:35:03.100Z",
        0,
        "session_meta",
        session_meta(json!({"forked_from_id": "01a036c5-parent"})),
    );
    // The parent's history, copied in the same append.
    ingest_at(
        &mut ctx,
        "2026-08-25T02:35:03.400Z",
        1,
        "event_msg",
        json!({"type": "token_count", "info": {
            "total_token_usage": usage_value(5_000, 500),
            "last_token_usage": usage_value(700, 70)}}),
    );
    // The fork's own first request, a few seconds later.
    let own = ingest_at(
        &mut ctx,
        "2026-08-25T02:35:09.000Z",
        2,
        "event_msg",
        json!({"type": "token_count", "info": {
            "total_token_usage": usage_value(5_300, 530),
            "last_token_usage": usage_value(300, 30)}}),
    );
    let tokens = sample(&own.ops);
    assert_eq!((tokens.input, tokens.output), (Some(300), Some(30)));
}

#[test]
fn a_paginated_subagent_skips_its_parents_prefix() {
    let mut ctx = context();
    ingest(
        &mut ctx,
        0,
        "session_meta",
        session_meta(json!({"subagent_history_start_ordinal": 5})),
    );
    assert!(token_count(&mut ctx, 3, (800, 80), (800, 80)).is_empty());
    let tokens = sample(&token_count(&mut ctx, 6, (1_000, 100), (200, 20)));
    assert_eq!((tokens.input, tokens.output), (Some(200), Some(20)));
}

#[test]
fn tier_route_and_searches_are_facts_of_the_request() {
    let mut ctx = context();
    ingest(
        &mut ctx,
        0,
        "session_meta",
        session_meta(json!({"model_provider": "azure"})),
    );
    ingest(
        &mut ctx,
        1,
        "event_msg",
        json!({"type": "thread_settings_applied", "thread_settings": {"service_tier": "priority"}}),
    );
    ingest(
        &mut ctx,
        2,
        "response_item",
        json!({"type": "web_search_call", "id": "ws_1", "status": "completed",
               "action": {"type": "search", "query": "rosetta"}}),
    );
    let ops = token_count(&mut ctx, 3, (100, 10), (100, 10));
    let tokens = ops
        .iter()
        .find_map(|op| match op {
            IngestOp::Usage { tokens, .. } => Some(tokens.clone()),
            IngestOp::SessionUsage(sample) => Some(sample.tokens.clone()),
            _ => None,
        })
        .expect("usage");
    assert_eq!(
        tokens.service_tier,
        Some(rosetta_core::ServiceTier::Priority)
    );
    assert_eq!(tokens.route.as_deref(), Some("azure"));
    assert_eq!(tokens.web_searches, Some(1));
}

#[test]
fn codex_input_counter_is_split_by_subtraction() {
    // `input_tokens` INCLUDES cached reads AND cache writes; `output_tokens`
    // includes reasoning. The buckets are exclusive.
    let mut ctx = context();
    let response = agent_message(&mut ctx, 1, "hello");
    let reading = json!({
        "input_tokens": 1000,
        "cached_input_tokens": 800,
        "cache_write_input_tokens": 64,
        "output_tokens": 50,
        "reasoning_output_tokens": 30,
        "total_tokens": 1050,
    });
    let ops = ingest(
        &mut ctx,
        2,
        "event_msg",
        json!({
            "type": "token_count",
            "info": {"total_token_usage": reading, "last_token_usage": reading},
        }),
    )
    .ops;

    let (keyed, tokens) = usage(&ops);
    assert_eq!(keyed, response);
    assert_eq!(tokens.input, Some(136));
    assert_eq!(tokens.cache_read, Some(800));
    assert_eq!(tokens.cache_write, Some(64));
    assert_eq!(tokens.output, Some(20));
    assert_eq!(tokens.thinking, Some(30));
    assert_eq!(tokens.total_tokens(), Some(1050));
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
            assert!(matches!(row.body, TimelineBody::Summary { .. }));
            assert_eq!(text(row), Some("Earlier work, compacted."));
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
            assert!(matches!(row.body, TimelineBody::Thinking { .. }));
            assert_eq!(text(row), Some("Checked the evidence."));
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
            assert!(matches!(row.body, TimelineBody::Agent { .. }));
            assert_eq!(text(row), Some("Work complete."));
        }
        other => panic!("expected one system row, got {other:?}"),
    }
}

#[test]
fn injected_developer_and_system_messages_remain_ledger_only() {
    let mut ctx = context();
    for (ordinal, role) in [(1, "developer"), (2, "system")] {
        let outcome = ingest(
            &mut ctx,
            ordinal,
            "response_item",
            json!({
                "type": "message",
                "id": format!("injected-{role}"),
                "role": role,
                "content": [{"type": "input_text", "text": "secret prompt context"}],
                "internal_chat_message_metadata_passthrough": {"turn_id": "turn"},
            }),
        );
        assert_eq!(outcome.status, LineStatus::Parsed);
        assert!(
            outcome.ops.is_empty(),
            "injected {role} context crossed the canonical boundary: {:?}",
            outcome.ops
        );
    }
}

#[test]
fn injected_environment_context_does_not_masquerade_as_user_prose() {
    let mut ctx = context();
    let injected = ingest(
        &mut ctx,
        1,
        "response_item",
        json!({
            "type": "message",
            "id": "environment",
            "role": "user",
            "content": [{
                "type": "input_text",
                "text": "  <environment_context>\n  <cwd>/workspace</cwd>\n</environment_context>  ",
            }],
            "internal_chat_message_metadata_passthrough": {"turn_id": "turn"},
        }),
    );
    assert_eq!(injected.status, LineStatus::Parsed);
    assert!(
        injected.ops.is_empty(),
        "injected environment context crossed the canonical boundary: {:?}",
        injected.ops
    );

    let human = ingest(
        &mut ctx,
        2,
        "response_item",
        json!({
            "type": "message",
            "id": "human",
            "role": "user",
            "content": [{
                "type": "input_text",
                "text": "Please explain this <environment_context> element.",
            }],
            "internal_chat_message_metadata_passthrough": {"turn_id": "turn"},
        }),
    );
    match human.ops.as_slice() {
        [IngestOp::Message(row)] => {
            assert!(matches!(row.body, TimelineBody::User { .. }));
            assert_eq!(
                text(row),
                Some("Please explain this <environment_context> element.")
            );
        }
        other => panic!("expected one human message row, got {other:?}"),
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
fn command_completion_uses_native_exit_status_and_discards_success_output() {
    let mut ctx = context();
    let outcome = ingest(
        &mut ctx,
        1,
        "event_msg",
        json!({
            "type": "item_completed",
            "completed_at_ms": 1_787_625_304_177_u64,
            "item": {
                "type": "CommandExecution",
                "id": "call_1",
                "status": "completed",
                "exit_code": 0,
                "duration": {"secs": 0, "nanos": 3_791},
                "stdout": "a very large successful result is not canonical",
                "stderr": "",
                "aggregated_output": "a very large successful result is not canonical"
            }
        }),
    );
    assert_eq!(outcome.status, LineStatus::Parsed);
    match outcome.ops.as_slice() {
        [
            IngestOp::ToolUpdate {
                correlation_key,
                patch,
                at,
                ..
            },
        ] => {
            assert_eq!(correlation_key, "call_1");
            assert_eq!(*at, Some(1_787_625_304_177));
            assert_eq!(
                patch.status.as_ref().map(|status| status.value),
                Some(ToolStatus::Succeeded)
            );
            assert_eq!(
                patch.status.as_ref().map(|status| status.basis),
                Some(StatusBasis::NativeState)
            );
            assert!(patch.failure.is_none());
            assert!(patch.request.is_none());
            assert!(patch.outcome.is_none());
            assert_eq!(
                patch.timing.as_ref().and_then(|timing| timing.duration_ms),
                Some(1)
            );
        }
        other => panic!("expected one command completion update, got {other:?}"),
    }
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
        [IngestOp::Message(row)] => tool(row).cloned().expect("tool call"),
        other => panic!("expected one tool row, got {other:?}"),
    };
    assert_eq!(tool.native.id.as_deref(), Some("call_1"));
    let ToolAction::Shell { command, .. } = &tool.action else {
        panic!("exec_command must project as shell")
    };
    assert_eq!(command.as_preview(), "echo hi");
    assert_eq!(tool.status.value, ToolStatus::Pending);

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
            IngestOp::ToolUpdate {
                correlation_key,
                patch,
                ..
            },
        ] => {
            assert_eq!(correlation_key, "call_1");
            // The canonical types `output` as a bare string and declares no
            // status field: success is unknown, never guessed.
            assert_eq!(
                patch.status.as_ref().map(|status| status.value),
                Some(ToolStatus::Unknown)
            );
            assert!(patch.failure.is_none());
        }
        other => panic!("expected one tool result, got {other:?}"),
    }
}

#[test]
fn executor_transport_updates_the_original_long_running_command() {
    let mut ctx = context();
    let call = ingest(
        &mut ctx,
        1,
        "response_item",
        json!({
            "type": "custom_tool_call",
            "id": "ctc_command",
            "status": "completed",
            "name": "exec",
            "call_id": "call_command",
            "input": r#"const r = await tools.exec_command({cmd:"gh pr checks 52 --watch", yield_time_ms:1000}); text(JSON.stringify(r));"#,
        }),
    );
    match call.ops.as_slice() {
        [IngestOp::Message(row)] => {
            let tool = tool(row).expect("command row");
            assert!(matches!(tool.action, ToolAction::Shell { .. }));
        }
        other => panic!("expected the original command row, got {other:?}"),
    }

    let launched = ingest(
        &mut ctx,
        2,
        "response_item",
        json!({
            "type": "custom_tool_call_output",
            "call_id": "call_command",
            "output": [
                {"type":"input_text", "text":"Script completed\nOutput:\n"},
                {"type":"input_text", "text":r#"{"chunk_id":"a","wall_time_seconds":1.0,"session_id":79752,"output":"pending"}"#},
            ],
        }),
    );
    match launched.ops.as_slice() {
        [
            IngestOp::ToolUpdate {
                correlation_key,
                patch,
                ..
            },
        ] => {
            assert_eq!(correlation_key, "call_command");
            assert_eq!(
                patch.status.as_ref().map(|status| status.value),
                Some(ToolStatus::Running)
            );
            let Some(outcome) = patch.outcome.as_ref() else {
                panic!("process id outcome")
            };
            assert!(matches!(
                &outcome.action,
                ToolAction::Shell {
                    outcome: Some(value),
                    ..
                } if value.process_id.as_deref() == Some("79752") && value.exit_code.is_none()
            ));
        }
        other => panic!("expected a running update, got {other:?}"),
    }

    let poll = ingest(
        &mut ctx,
        3,
        "response_item",
        json!({
            "type": "custom_tool_call",
            "id": "ctc_poll",
            "status": "completed",
            "name": "exec",
            "call_id": "call_poll",
            "input": r#"const r = await tools.write_stdin({session_id:79752, chars:"", yield_time_ms:30000}); text(JSON.stringify(r));"#,
        }),
    );
    assert!(poll.ops.is_empty(), "an empty poll is transport, not a row");

    let yielded = ingest(
        &mut ctx,
        4,
        "response_item",
        json!({
            "type": "custom_tool_call_output",
            "call_id": "call_poll",
            "output": "Script running with cell ID 1144\nWall time 11.0 seconds\nOutput:\n",
        }),
    );
    assert!(yielded.ops.is_empty(), "a yielded cell is only a route");

    let wait = ingest(
        &mut ctx,
        5,
        "response_item",
        json!({
            "type": "function_call",
            "id": "fc_wait",
            "name": "wait",
            "call_id": "call_wait",
            "arguments": r#"{"cell_id":"1144","yield_time_ms":30000}"#,
        }),
    );
    assert!(
        wait.ops.is_empty(),
        "waiting on the wrapper is not activity"
    );

    let finished = ingest(
        &mut ctx,
        6,
        "response_item",
        json!({
            "type": "function_call_output",
            "call_id": "call_wait",
            "output": [
                {"type":"input_text", "text":"Script completed\nOutput:\n"},
                {"type":"input_text", "text":r#"{"chunk_id":"b","wall_time_seconds":25.5,"exit_code":0,"output":"all checks passed"}"#},
            ],
        }),
    );
    match finished.ops.as_slice() {
        [
            IngestOp::ToolUpdate {
                correlation_key,
                patch,
                ..
            },
        ] => {
            assert_eq!(correlation_key, "call_command");
            assert_eq!(
                patch.status.as_ref().map(|status| status.value),
                Some(ToolStatus::Succeeded)
            );
            assert_eq!(
                patch.status.as_ref().map(|status| status.basis),
                Some(StatusBasis::NativeState)
            );
            let Some(outcome) = patch.outcome.as_ref() else {
                panic!("terminal command outcome")
            };
            assert!(matches!(
                &outcome.action,
                ToolAction::Shell {
                    outcome: Some(value),
                    ..
                } if value.process_id.as_deref() == Some("79752") && value.exit_code == Some(0)
            ));
        }
        other => panic!("expected one terminal update for the original command, got {other:?}"),
    }
}

#[test]
fn a_literal_parallel_command_batch_keeps_both_commands() {
    let mut ctx = context();
    let call = ingest(
        &mut ctx,
        1,
        "response_item",
        json!({
            "type": "custom_tool_call",
            "id": "ctc_batch",
            "status": "completed",
            "name": "exec",
            "call_id": "call_batch",
            "input": r#"const rs = await Promise.all([
                tools.exec_command({cmd:"gh run view 1 --json status"}),
                tools.exec_command({cmd:"gh run view 2 --json status"})
            ]); for (const r of rs) text(r.output);"#,
        }),
    );
    assert_eq!(call.ops.len(), 2);
    let commands = call
        .ops
        .iter()
        .map(|op| match op {
            IngestOp::Message(row) => match tool(row).map(|tool| &tool.action) {
                Some(ToolAction::Shell { command, .. }) => command.as_preview(),
                other => panic!("expected shell child, got {other:?}"),
            },
            other => panic!("expected only child rows, got {other:?}"),
        })
        .collect::<Vec<_>>();
    assert_eq!(
        commands,
        ["gh run view 1 --json status", "gh run view 2 --json status"]
    );

    let output = ingest(
        &mut ctx,
        2,
        "response_item",
        json!({
            "type": "custom_tool_call_output",
            "call_id": "call_batch",
            "output": [
                {"type":"input_text", "text":"Script completed\nOutput:\n"},
                {"type":"input_text", "text":"{\"status\":\"completed\"}"},
                {"type":"input_text", "text":"{\"status\":\"in_progress\"}"},
            ],
        }),
    );
    assert_eq!(output.ops.len(), 2);
    assert!(output.ops.iter().all(|op| matches!(
        op,
        IngestOp::ToolUpdate { patch, .. }
            if patch.status.as_ref().is_some_and(|status|
                status.value == ToolStatus::Unknown
                    && status.basis == StatusBasis::NativeTerminalRecord)
    )));
}

#[test]
fn a_custom_tool_call_projects_only_reviewed_patch_facts() {
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
            let tool = tool(row).expect("custom tool call");
            assert_eq!(tool.native.id.as_deref(), Some("call_1"));
            assert_eq!(tool.native.name, "apply_patch");
            let ToolAction::FileChange { changes } = &tool.action else {
                panic!("apply_patch must project as file_change")
            };
            assert_eq!(changes.len(), 1);
            assert!(matches!(
                &changes[0].path,
                ResourceRef::File { path, .. } if path == "added.txt"
            ));
            assert_eq!(
                (changes[0].lines_added, changes[0].lines_removed),
                (Some(1), None)
            );
            assert_eq!(
                changes[0].diff.as_ref().map(|diff| diff.as_preview()),
                Some("--- /dev/null\n+++ added.txt\n@@\n+hello")
            );
            assert_eq!(edit.message_timeline_id.as_deref(), Some(&*row.timeline_id));
            assert_eq!(edit.path, "added.txt");
            assert_eq!((edit.lines_added, edit.lines_removed), (Some(1), None));
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
            IngestOp::ToolUpdate {
                correlation_key,
                patch,
                ..
            },
        ] => {
            assert_eq!(correlation_key, "call_1");
            assert_eq!(
                patch.status.as_ref().map(|status| status.value),
                Some(ToolStatus::Unknown)
            );
            assert!(patch.failure.is_none());
        }
        other => panic!("expected one custom tool result, got {other:?}"),
    }
}

#[test]
fn an_exec_wrapped_patch_emits_the_diff_and_file_edit_activity() {
    let mut ctx = context();
    let call = ingest(
        &mut ctx,
        1,
        "response_item",
        json!({
            "type": "custom_tool_call",
            "id": "ctc_exec_patch",
            "status": "completed",
            "call_id": "call_exec_patch",
            "name": "exec",
            "input": r#"const patch = "*** Begin Patch\n*** Update File: src/main.rs\n@@\n-old\n+new\n*** End Patch"; text(await tools.apply_patch(patch));"#,
        }),
    );
    assert_eq!(call.status, LineStatus::Parsed);
    match call.ops.as_slice() {
        [IngestOp::Message(row), IngestOp::FileEdit(edit)] => {
            let tool = tool(row).expect("wrapped patch call");
            assert_eq!(tool.native.name, "exec");
            let ToolAction::FileChange { changes } = &tool.action else {
                panic!("wrapped apply_patch must project as file_change")
            };
            assert_eq!(
                (changes[0].lines_added, changes[0].lines_removed),
                (Some(1), Some(1))
            );
            assert!(changes[0].diff.is_some());
            assert_eq!(edit.path, "src/main.rs");
            assert_eq!((edit.lines_added, edit.lines_removed), (Some(1), Some(1)));
        }
        other => panic!("expected a tool row and file edit, got {other:?}"),
    }
}

#[test]
fn modern_block_tool_results_drop_unreviewed_text_and_structure() {
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
            IngestOp::ToolUpdate {
                correlation_key,
                patch,
                ..
            },
        ] => {
            assert_eq!(correlation_key, "call_1");
            assert_eq!(
                patch.status.as_ref().map(|status| status.value),
                Some(ToolStatus::Unknown)
            );
            assert!(patch.failure.is_none());
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
fn turn_working_directory_is_transient_tool_context() {
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
    assert!(first.ops.is_empty());

    // Same directory next turn: nothing changed, nothing to observe.
    let repeat = ingest(&mut ctx, 2, "turn_context", turn("/w/one"));
    assert!(repeat.ops.is_empty());

    let moved = ingest(&mut ctx, 3, "turn_context", turn("/w/two"));
    assert!(moved.ops.is_empty());

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
            assert!(matches!(row.body, TimelineBody::User { .. }));
            assert_eq!(row.model, None);
            assert_eq!(text(row), Some("hi"));
        }
        other => panic!("expected one message row, got {other:?}"),
    }
}

// Writer-derived ContentItem cases (0.150.1), not native capture fixtures.
const IMAGE_PNG: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+ip1sAAAAASUVORK5CYII=";

#[test]
fn image_triplets_preserve_occurrences_strip_only_paired_wrappers_and_bind_exact_spans() {
    let mut ctx = context();
    let outcome = ingest(
        &mut ctx,
        1,
        "response_item",
        json!({"type":"message","role":"user","content":[
            {"type":"input_text","text":"<image name=[Image #1] path=\"/tmp/first.png\">"},
            {"type":"input_image","image_url":"data:image/png;base64,invalid"},
            {"type":"input_text","text":"</image>"},
            {"type":"input_text","text":"<image name=[Image #4] path=\"/tmp/a \" <quoted>.png\">"},
            {"type":"input_image","image_url":format!("data:image/png;base64,{IMAGE_PNG}")},
            {"type":"input_text","text":"</image>"},
            {"type":"input_image","image_url":format!("data:image/png;base64,{IMAGE_PNG}")},
            {"type":"input_text","text":"😀 compare [Image #1] and [Image #4]. [Image #4] again. [Image #99] stays plain."}
        ]}),
    );
    assert_eq!(outcome.status, LineStatus::Parsed);
    let rows: Vec<_> = outcome
        .ops
        .iter()
        .filter_map(|op| {
            if let IngestOp::Message(row) = op {
                Some(row)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(rows.len(), 1);
    rows[0].validate().unwrap();
    let TimelineBody::User { text, attachments } = &rows[0].body else {
        panic!("user message")
    };
    let text = text.as_ref().unwrap().as_preview();
    assert!(text.starts_with("😀 compare"));
    assert!(!text.contains("<image"));
    assert_eq!(attachments.len(), 3);
    assert!(matches!(
        attachments[0].source,
        rosetta_core::timeline::AttachmentSource::Unavailable { .. }
    ));
    assert_eq!(attachments[0].message_ranges[0].start, 10);
    assert_eq!(attachments[1].name.as_deref(), Some("a \" <quoted>.png"));
    assert_eq!(attachments[1].message_ranges.len(), 2);
    assert!(attachments[2].message_ranges.is_empty());
    assert_ne!(attachments[1].id, attachments[2].id);
    assert_eq!(attachments[1].source, attachments[2].source);
    let canonical = serde_json::to_string(rows[0]).unwrap();
    assert!(!canonical.contains(IMAGE_PNG));
    assert!(!canonical.contains("/tmp/"));
}

#[test]
fn bare_missing_and_reference_images_stay_in_the_prompt_and_literal_wrappers_stay_text() {
    let mut ctx = context();
    let outcome = ingest(
        &mut ctx,
        1,
        "response_item",
        json!({"type":"message","role":"user","content":[
            {"type":"input_text","text":"<image>this is an example</image>"},
            {"type":"input_image","image_url":null},
            {"type":"input_image","image_url":"https://example.test/image.png"}
        ]}),
    );
    assert_eq!(outcome.status, LineStatus::Parsed);
    let row = outcome
        .ops
        .iter()
        .find_map(|op| {
            if let IngestOp::Message(row) = op {
                Some(row)
            } else {
                None
            }
        })
        .unwrap();
    let TimelineBody::User { text, attachments } = &row.body else {
        panic!("user")
    };
    assert_eq!(
        text.as_ref().unwrap().as_preview(),
        "<image>this is an example</image>"
    );
    assert_eq!(attachments.len(), 2);
    assert!(matches!(
        attachments[1].source,
        rosetta_core::timeline::AttachmentSource::Reference { .. }
    ));
    assert!(
        !outcome
            .ops
            .iter()
            .any(|op| matches!(op, IngestOp::ProjectionAsset { .. }))
    );
}
