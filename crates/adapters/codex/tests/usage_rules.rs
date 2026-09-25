//! The usage rules the 2026-09-23 USD audit changed (RESEARCH_PLAN §5.13,
//! ARCHITECTURE §4.1.3): the per-response `token_usage_record` and its
//! `token_count` twin, the first settings snapshot's tier, replay gates on
//! files whose start ordinal sits past their last record, copied headers,
//! missed-emission deltas, and guardian usage on the parent session.
//!
//! Synthetic lines shaped from the pinned writer source (openai/codex@e7bbc79):
//! they make no schema claim beyond the ground truth's writer intermediate.

use rosetta_adapter_codex::CodexAdapter;
use rosetta_adapter_support::adapter::{AgentAdapter, LineOutcome, NormalizeCtx};
use rosetta_core::rows::{ServiceTier, TokenSnapshot, UsageScope};
use rosetta_core::{IngestOp, ids};
use serde_json::{Value, json};

const STAMP: &str = "2026-09-20T10:00:00.100Z";
const NATIVE: &str = "01a036c5-0000-0000-0000-000000000000";

fn runtime_id() -> String {
    ids::runtime_id("codex", "~/.codex")
}

fn context_for(native: &str) -> NormalizeCtx {
    let runtime_id = runtime_id();
    let session_id = ids::session_id(&runtime_id, native);
    let mut ctx = NormalizeCtx::new(runtime_id, session_id);
    ctx.state = CodexAdapter.init_state();
    ctx
}

fn context() -> NormalizeCtx {
    context_for(NATIVE)
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

fn ingest(ctx: &mut NormalizeCtx, ordinal: u64, record_type: &str, payload: Value) -> LineOutcome {
    ingest_at(ctx, STAMP, ordinal, record_type, payload)
}

fn header(id: &str, cli_version: &str, extra: Value) -> Value {
    let mut meta = json!({
        "id": id,
        "timestamp": STAMP,
        "cwd": "/workspace",
        "originator": "codex_exec",
        "cli_version": cli_version,
        "source": "exec",
        "model_provider": "openai",
        "history_mode": "paginated",
    });
    meta.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    meta
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

fn token_count_payload(total: (i64, i64), last: (i64, i64)) -> Value {
    json!({
        "type": "token_count",
        "info": {
            "total_token_usage": usage_value(total.0, total.1),
            "last_token_usage": usage_value(last.0, last.1),
        },
    })
}

fn token_count(
    ctx: &mut NormalizeCtx,
    ordinal: u64,
    total: (i64, i64),
    last: (i64, i64),
) -> Vec<IngestOp> {
    ingest(ctx, ordinal, "event_msg", token_count_payload(total, last)).ops
}

/// The per-response record as `record_observed_response_completed` writes it
/// (`core/src/session/mod.rs:4645-4675`, `protocol.rs:2251-2260`).
fn record_payload(thread_id: &str, response_id: &str, usage: (i64, i64)) -> Value {
    json!({
        "thread_id": thread_id,
        "turn_id": "turn_1",
        "session_id": "sess_1",
        "root_turn_id": "turn_1",
        "response_id": response_id,
        "usage": usage_value(usage.0, usage.1),
        "turn_token_usage": usage_value(usage.0, usage.1),
        "thread_token_usage": usage_value(usage.0, usage.1),
    })
}

fn record(
    ctx: &mut NormalizeCtx,
    ordinal: u64,
    response_id: &str,
    usage: (i64, i64),
) -> Vec<IngestOp> {
    ingest(
        ctx,
        ordinal,
        "token_usage_record",
        record_payload(NATIVE, response_id, usage),
    )
    .ops
}

fn settings(tier: Option<&str>) -> Value {
    let mut snapshot = json!({"model": "gpt-6-astra", "model_provider_id": "openai"});
    if let Some(tier) = tier {
        snapshot["service_tier"] = json!(tier);
    }
    json!({"type": "thread_settings_applied", "thread_settings": snapshot})
}

fn agent_message(ctx: &mut NormalizeCtx, ordinal: u64) -> String {
    let outcome = ingest(
        ctx,
        ordinal,
        "response_item",
        json!({
            "type": "message",
            "id": format!("msg_{ordinal}"),
            "role": "assistant",
            "content": [{"type": "output_text", "text": "reply"}],
        }),
    );
    match outcome.ops.as_slice() {
        [IngestOp::Message(row)] => row.timeline_id.clone(),
        other => panic!("expected one message row, got {other:?}"),
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

fn sample(ops: &[IngestOp]) -> (String, String, TokenSnapshot) {
    match ops {
        [IngestOp::SessionUsage(sample)] => (
            sample.session_id.clone(),
            sample.key.clone(),
            sample.tokens.clone(),
        ),
        other => panic!("expected one request sample, got {other:?}"),
    }
}

fn drift_codes(ops: &[IngestOp]) -> Vec<String> {
    ops.iter()
        .filter_map(|op| match op {
            IngestOp::Drift(fact) => Some(fact.error_code.clone()),
            _ => None,
        })
        .collect()
}

fn in_out(tokens: &TokenSnapshot) -> (Option<i64>, Option<i64>) {
    (tokens.input, tokens.output)
}

// ————— CODEX-2: token_usage_record —————————————————————————————————

#[test]
fn a_record_and_its_token_count_twin_count_once() {
    let mut ctx = context();
    ingest(
        &mut ctx,
        0,
        "session_meta",
        header(NATIVE, "0.153.4", json!({})),
    );
    let first = agent_message(&mut ctx, 1);
    let (keyed, tokens) = usage(&record(&mut ctx, 2, "resp_1", (100, 10)));
    assert_eq!(keyed, first);
    assert_eq!(in_out(&tokens), (Some(100), Some(10)));
    assert_eq!(tokens.scope, UsageScope::Request);
    // The event repeats the record's usage: it only advances the total.
    assert!(token_count(&mut ctx, 3, (100, 10), (100, 10)).is_empty());

    let second = agent_message(&mut ctx, 4);
    let (keyed, tokens) = usage(&record(&mut ctx, 5, "resp_2", (50, 5)));
    assert_eq!(keyed, second);
    assert_eq!(in_out(&tokens), (Some(50), Some(5)));
    assert!(token_count(&mut ctx, 6, (150, 15), (50, 5)).is_empty());

    // A reading without a record still counts by the counter rule, from the
    // total the twins advanced.
    let (_, _, tokens) = sample(&token_count(&mut ctx, 7, (170, 17), (20, 2)));
    assert_eq!(in_out(&tokens), (Some(20), Some(2)));
}

#[test]
fn a_remote_compaction_record_without_a_twin_is_a_request_sample() {
    let mut ctx = context();
    ingest(
        &mut ctx,
        0,
        "session_meta",
        header(NATIVE, "0.153.4", json!({})),
    );
    // Remote compaction: a record, no response row, no token_count.
    let (session, key, tokens) = sample(&record(&mut ctx, 1, "resp_compact", (300, 30)));
    assert_eq!(session, ctx.session_id);
    assert_eq!(key, ids::usage_id(&ctx.session_id, "tur:resp_compact"));
    assert_eq!(in_out(&tokens), (Some(300), Some(30)));

    // The next response's record and twin are unaffected by the orphan.
    let row = agent_message(&mut ctx, 2);
    let (keyed, tokens) = usage(&record(&mut ctx, 3, "resp_2", (40, 4)));
    assert_eq!(keyed, row);
    assert_eq!(in_out(&tokens), (Some(40), Some(4)));
    assert!(token_count(&mut ctx, 4, (340, 34), (40, 4)).is_empty());
}

#[test]
fn a_token_count_that_differs_from_the_record_is_its_own_request() {
    let mut ctx = context();
    ingest(
        &mut ctx,
        0,
        "session_meta",
        header(NATIVE, "0.153.4", json!({})),
    );
    sample(&record(&mut ctx, 1, "resp_1", (100, 10)));
    let (_, _, tokens) = sample(&token_count(&mut ctx, 2, (130, 13), (30, 3)));
    assert_eq!(in_out(&tokens), (Some(30), Some(3)));
}

#[test]
fn a_record_of_another_thread_is_copied_history() {
    let mut ctx = context();
    ingest(
        &mut ctx,
        0,
        "session_meta",
        header(NATIVE, "0.153.4", json!({})),
    );
    let copied = ingest(
        &mut ctx,
        1,
        "token_usage_record",
        record_payload(
            "01a036c5-9999-0000-0000-000000000000",
            "resp_parent",
            (100, 10),
        ),
    );
    assert!(copied.ops.is_empty());
    // Not a twin of anything this file owns: the reading counts itself.
    let (_, _, tokens) = sample(&token_count(&mut ctx, 2, (100, 10), (100, 10)));
    assert_eq!(in_out(&tokens), (Some(100), Some(10)));
}

#[test]
fn a_record_is_declared_and_parses_without_drift() {
    let mut ctx = context();
    ingest(
        &mut ctx,
        0,
        "session_meta",
        header(NATIVE, "0.153.4", json!({})),
    );
    let outcome = ingest(
        &mut ctx,
        1,
        "token_usage_record",
        record_payload(NATIVE, "resp_1", (100, 10)),
    );
    assert_eq!(
        outcome.status,
        rosetta_adapter_support::adapter::LineStatus::Parsed
    );
    assert!(drift_codes(&outcome.ops).is_empty());
}

// ————— CODEX-1 / CODEX-9: the configured tier ——————————————————————————

#[test]
fn the_first_settings_snapshot_back_applies_its_tier() {
    let mut ctx = context();
    ingest(
        &mut ctx,
        0,
        "session_meta",
        header(NATIVE, "0.153.4", json!({})),
    );
    let row = agent_message(&mut ctx, 1);
    let (_, tokens) = usage(&token_count(&mut ctx, 2, (100, 10), (100, 10)));
    assert_eq!(tokens.service_tier, None);
    let (_, key, tokens) = sample(&token_count(&mut ctx, 3, (150, 15), (50, 5)));
    assert_eq!(tokens.service_tier, None);

    // Codex writes no tier for the first turn; the first snapshot names it.
    let replayed = ingest(&mut ctx, 4, "event_msg", settings(Some("fast"))).ops;
    match replayed.as_slice() {
        [
            IngestOp::Usage {
                response, tokens, ..
            },
            IngestOp::SessionUsage(sample),
        ] => {
            assert_eq!(response, &row);
            assert_eq!(tokens.service_tier, Some(ServiceTier::Priority));
            assert_eq!(in_out(tokens), (Some(100), Some(10)));
            assert_eq!(sample.key, key);
            assert_eq!(sample.tokens.service_tier, Some(ServiceTier::Priority));
            assert_eq!(in_out(&sample.tokens), (Some(50), Some(5)));
        }
        other => panic!("expected the two earlier requests re-emitted, got {other:?}"),
    }
    let (_, _, tokens) = sample(&token_count(&mut ctx, 5, (200, 20), (50, 5)));
    assert_eq!(tokens.service_tier, Some(ServiceTier::Priority));
    // Later snapshots change the tier going forward only.
    assert!(
        ingest(&mut ctx, 6, "event_msg", settings(Some("fast")))
            .ops
            .is_empty()
    );
}

#[test]
fn a_first_snapshot_without_a_tier_settles_the_file_untiered() {
    let mut ctx = context();
    ingest(
        &mut ctx,
        0,
        "session_meta",
        header(NATIVE, "0.153.4", json!({})),
    );
    sample(&token_count(&mut ctx, 1, (100, 10), (100, 10)));
    assert!(
        ingest(&mut ctx, 2, "event_msg", settings(None))
            .ops
            .is_empty()
    );
    let (_, _, tokens) = sample(&token_count(&mut ctx, 3, (150, 15), (50, 5)));
    assert_eq!(tokens.service_tier, None);
    // A tier configured later applies from then on, never backwards.
    assert!(
        ingest(&mut ctx, 4, "event_msg", settings(Some("priority")))
            .ops
            .is_empty()
    );
    let (_, _, tokens) = sample(&token_count(&mut ctx, 5, (200, 20), (50, 5)));
    assert_eq!(tokens.service_tier, Some(ServiceTier::Priority));
}

#[test]
fn a_missing_tier_key_is_unset_and_an_unknown_one_is_drift() {
    let mut ctx = context();
    ingest(
        &mut ctx,
        0,
        "session_meta",
        header(NATIVE, "0.153.4", json!({})),
    );
    ingest(&mut ctx, 1, "event_msg", settings(Some("fast")));
    let (_, _, tokens) = sample(&token_count(&mut ctx, 2, (100, 10), (100, 10)));
    assert_eq!(tokens.service_tier, Some(ServiceTier::Priority));

    // Codex omits the key when the tier is unset (protocol.rs:2200-2201).
    assert!(
        ingest(&mut ctx, 3, "event_msg", settings(None))
            .ops
            .is_empty()
    );
    let (_, _, tokens) = sample(&token_count(&mut ctx, 4, (150, 15), (50, 5)));
    assert_eq!(tokens.service_tier, None);

    let outcome = ingest(&mut ctx, 5, "event_msg", settings(Some("warp")));
    assert_eq!(drift_codes(&outcome.ops), vec!["unknown_service_tier"]);
    let (_, _, tokens) = sample(&token_count(&mut ctx, 6, (200, 20), (50, 5)));
    assert_eq!(tokens.service_tier, None);
}

// ————— CODEX-4 / CODEX-8: replay gates ———————————————————————————————

#[test]
fn a_start_ordinal_past_the_last_record_keeps_the_childs_own_usage() {
    // A guardian-style child and a `legacy_to_paginated_v1` migration put the
    // boundary one past the last record: everything below it is the
    // child's own work, except the copied burst right after the header.
    let mut ctx = context();
    ingest(
        &mut ctx,
        0,
        "session_meta",
        header(
            NATIVE,
            "0.153.4",
            json!({"subagent_history_start_ordinal": 40, "parent_thread_id": "parent",
                   "source": {"subagent": {"other": "review"}}}),
        ),
    );
    let copied = ingest_at(
        &mut ctx,
        "2026-09-20T10:00:00.400Z",
        2,
        "event_msg",
        token_count_payload((5_000, 500), (700, 70)),
    );
    assert!(copied.ops.is_empty(), "the head burst is the parent's copy");
    let own = ingest_at(
        &mut ctx,
        "2026-09-20T10:00:20.000Z",
        5,
        "event_msg",
        token_count_payload((5_300, 530), (300, 30)),
    );
    let (_, _, tokens) = sample(&own.ops);
    assert_eq!(in_out(&tokens), (Some(300), Some(30)));
}

#[test]
fn own_records_at_the_boundary_count_even_inside_the_burst() {
    let mut ctx = context();
    ingest(
        &mut ctx,
        0,
        "session_meta",
        header(
            NATIVE,
            "0.153.4",
            json!({"subagent_history_start_ordinal": 5}),
        ),
    );
    assert!(
        ingest_at(
            &mut ctx,
            "2026-09-20T10:00:00.400Z",
            3,
            "event_msg",
            token_count_payload((800, 80), (800, 80))
        )
        .ops
        .is_empty()
    );
    let own = ingest_at(
        &mut ctx,
        "2026-09-20T10:00:00.450Z",
        5,
        "event_msg",
        token_count_payload((1_000, 100), (200, 20)),
    );
    let (_, _, tokens) = sample(&own.ops);
    assert_eq!(in_out(&tokens), (Some(200), Some(20)));
}

#[test]
fn a_copied_parent_header_does_not_reconfigure_the_child() {
    let mut ctx = context();
    ingest(
        &mut ctx,
        0,
        "session_meta",
        header(
            NATIVE,
            "0.153.4",
            json!({"subagent_history_start_ordinal": 5}),
        ),
    );
    // The inherited prefix starts with the parent's own header.
    ingest_at(
        &mut ctx,
        "2026-09-20T10:00:00.101Z",
        1,
        "session_meta",
        header(
            "01a036c5-9999-0000-0000-000000000000",
            "0.152.0",
            json!({"cwd": "/elsewhere"}),
        ),
    );
    assert_eq!(ctx.agent_version.as_deref(), Some("0.153.4"));
    let copied = ingest_at(
        &mut ctx,
        "2026-09-20T10:00:00.400Z",
        2,
        "event_msg",
        token_count_payload((800, 80), (800, 80)),
    );
    assert!(copied.ops.is_empty(), "the child's gate survives the copy");
}

#[test]
fn a_plain_session_counts_its_first_second_whatever_the_writer() {
    let mut ctx = context();
    ingest(
        &mut ctx,
        0,
        "session_meta",
        header(NATIVE, "0.140.0", json!({})),
    );
    let own = ingest_at(
        &mut ctx,
        "2026-09-20T10:00:00.300Z",
        1,
        "event_msg",
        token_count_payload((100, 10), (100, 10)),
    );
    let (_, _, tokens) = sample(&own.ops);
    assert_eq!(in_out(&tokens), (Some(100), Some(10)));
}

// ————— CODEX-7: missed emissions ——————————————————————————————————————

#[test]
fn a_delta_beyond_last_splits_into_a_request_and_an_aggregate() {
    let mut ctx = context();
    ingest(
        &mut ctx,
        0,
        "session_meta",
        header(NATIVE, "0.149.1", json!({})),
    );
    agent_message(&mut ctx, 1);
    usage(&token_count(&mut ctx, 2, (100, 10), (100, 10)));
    let row = agent_message(&mut ctx, 3);
    // The total advanced by two requests' worth while one reading was missed.
    let ops = token_count(&mut ctx, 4, (1_100, 110), (400, 40));
    match ops.as_slice() {
        [
            IngestOp::Usage {
                response, tokens, ..
            },
            IngestOp::SessionUsage(rest),
        ] => {
            assert_eq!(response, &row);
            assert_eq!(in_out(tokens), (Some(400), Some(40)));
            assert_eq!(tokens.scope, UsageScope::Request);
            assert_eq!(rest.key, ids::usage_id(&ctx.session_id, "tc:2:rest"));
            assert_eq!(in_out(&rest.tokens), (Some(600), Some(60)));
            assert_eq!(rest.tokens.scope, UsageScope::Aggregate);
            assert_eq!(rest.tokens.requests, None);
        }
        other => panic!("expected the request and an aggregate remainder, got {other:?}"),
    }
}

// ————— CODEX-3: guardian usage on the parent ————————————————————————

fn guardian_header(parent: Option<&str>) -> Value {
    let mut extra = json!({
        "source": {"subagent": {"other": "guardian"}},
        "thread_source": "guardian_review",
        "subagent_history_start_ordinal": 30,
    });
    if let Some(parent) = parent {
        extra["parent_thread_id"] = json!(parent);
    }
    header("guardian-child", "0.153.4", extra)
}

#[test]
fn guardian_usage_lands_on_the_parent_as_samples() {
    let mut ctx = context_for("guardian-child");
    let parent = ids::session_id(&runtime_id(), "parent-thread");
    assert!(
        ingest(
            &mut ctx,
            0,
            "session_meta",
            guardian_header(Some("parent-thread"))
        )
        .ops
        .is_empty()
    );
    assert!(
        ingest(
            &mut ctx,
            1,
            "turn_context",
            json!({"cwd": "/workspace", "model": "codex-auto-review"})
        )
        .ops
        .is_empty()
    );
    for (ordinal, role) in [(2, "user"), (3, "assistant")] {
        let outcome = ingest(
            &mut ctx,
            ordinal,
            "response_item",
            json!({"type": "message", "role": role,
                   "content": [{"type": "input_text", "text": "review"}]}),
        );
        assert!(outcome.ops.is_empty(), "no timeline rows for a review");
    }
    assert_eq!(ctx.next_ordinal, 0);

    let own = ingest_at(
        &mut ctx,
        "2026-09-20T10:00:05.000Z",
        4,
        "event_msg",
        token_count_payload((300, 30), (300, 30)),
    );
    let (session, key, tokens) = sample(&own.ops);
    assert_eq!(session, parent);
    assert_eq!(key, ids::usage_id(&parent, "guardian:guardian-child:tc:1"));
    assert_eq!(in_out(&tokens), (Some(300), Some(30)));
    match own.ops.as_slice() {
        [IngestOp::SessionUsage(sample)] => {
            assert_eq!(sample.model.as_deref(), Some("codex-auto-review"));
        }
        other => panic!("{other:?}"),
    }

    // The per-response record and its twin behave as in any file.
    let record = ingest_at(
        &mut ctx,
        "2026-09-20T10:00:06.000Z",
        5,
        "token_usage_record",
        record_payload("guardian-child", "resp_g", (40, 4)),
    );
    let (session, key, _) = sample(&record.ops);
    assert_eq!(session, parent);
    assert_eq!(
        key,
        ids::usage_id(&parent, "guardian:guardian-child:tur:resp_g")
    );
    assert!(
        ingest_at(
            &mut ctx,
            "2026-09-20T10:00:06.100Z",
            6,
            "event_msg",
            token_count_payload((340, 34), (40, 4))
        )
        .ops
        .is_empty()
    );
    // Drift stays in the ledger too.
    assert!(
        ingest(&mut ctx, 7, "event_msg", settings(Some("warp")))
            .ops
            .is_empty()
    );

    // A resumed tail keeps forwarding to the parent.
    let checkpoint = CodexAdapter.checkpoint_state(ctx.state.as_deref()).unwrap();
    let mut resumed = context_for("guardian-child");
    resumed.state = CodexAdapter.restore_state(checkpoint.as_ref()).unwrap();
    let later = ingest_at(
        &mut resumed,
        "2026-09-20T10:00:09.000Z",
        8,
        "event_msg",
        token_count_payload((400, 40), (60, 6)),
    );
    let (session, _, tokens) = sample(&later.ops);
    assert_eq!(session, parent);
    assert_eq!(in_out(&tokens), (Some(60), Some(6)));
    assert_eq!(resumed.next_ordinal, 0);
}

#[test]
fn a_guardian_without_a_proven_parent_emits_nothing() {
    let mut ctx = context_for("guardian-child");
    ingest(&mut ctx, 0, "session_meta", guardian_header(None));
    let own = ingest_at(
        &mut ctx,
        "2026-09-20T10:00:05.000Z",
        1,
        "event_msg",
        token_count_payload((300, 30), (300, 30)),
    );
    assert!(own.ops.is_empty());
}
