//! Usage placement, fork copies, and the 2.0.2 writer shapes (ARCHITECTURE
//! §4.1.3, RESEARCH_PLAN §5.13 Kimi row).
//!
//! The records here are hermetic, content-free synthetics shaped from the
//! installed Kimi Code 2.0.2 bundle (`~/.kimi-code/bin/kimi`, byte offsets
//! `kimi@N`): the fork copy and marker (`fork()` kimi@97829976,
//! `copySessionFiles` kimi@97839892, `copyAgentWire` kimi@97836890,
//! `forkedRecord` kimi@97815218, state write kimi@97834752), the optional
//! `llm.request` fields (`llmRequestSchema` kimi@103479471), the unclamped
//! `inputOther` (`parseOpenAIUsage` kimi@98617262) and the optional
//! `usageScope` (`usageRecordSchema` kimi@101742617). Native captures of a
//! whole-session and a turn-sliced fork are pending (RESEARCH_PLAN §5.13).

use rosetta_adapter_kimi_code::KimiCodeAdapter;
use rosetta_adapter_kimi_code::normalize::KimiCodeState;
use rosetta_adapter_support::{AgentAdapter, FileStat, LineOutcome, LineStatus, NormalizeCtx};
use rosetta_core::rows::{SessionRole, SessionUsageRow, TitleKind};
use rosetta_core::timeline::{TimelineBody, TimelineEntry};
use rosetta_core::{IngestOp, ids};
use serde_json::{Value, json};

const DATA_ROOT: &str = "~/.kimi-code";
const ROOT: &str = "session_5f1c1a2e-2c8e-4d4a-9a4e-7e0d9d1a0b11";

fn context_for(native: &str) -> NormalizeCtx {
    let runtime_id = ids::runtime_id("kimi-code", DATA_ROOT);
    let mut ctx = NormalizeCtx::new(&runtime_id, ids::session_id(&runtime_id, native));
    ctx.native_session_id = Some(native.to_owned());
    ctx.state = KimiCodeAdapter.init_state();
    ctx
}

fn context() -> NormalizeCtx {
    context_for(ROOT)
}

/// What the pipeline will pass once `NormalizeCtx` carries the meta-settled
/// fork instant; today the adapter learns it through this same entry point.
fn seed_fork_instant(ctx: &mut NormalizeCtx, forked_at: i64) {
    ctx.state
        .as_mut()
        .expect("adapter state")
        .downcast_mut::<KimiCodeState>()
        .expect("kimi state")
        .observe_fork_instant(Some(forked_at));
}

fn ingest(ctx: &mut NormalizeCtx, record: &Value) -> LineOutcome {
    ctx.src_cursor = Some(ctx.src_cursor.unwrap_or(0) + 1);
    ctx.src_offset = Some(ctx.src_offset.unwrap_or(0) + 1);
    KimiCodeAdapter.ingest_line(ctx, "wire", record.to_string().as_bytes())
}

fn parsed(ctx: &mut NormalizeCtx, record: &Value) -> Vec<IngestOp> {
    let outcome = ingest(ctx, record);
    assert_eq!(outcome.status, LineStatus::Parsed, "{record}");
    outcome.ops
}

fn rows(ops: &[IngestOp]) -> Vec<&TimelineEntry> {
    ops.iter()
        .filter_map(|op| match op {
            IngestOp::Message(row) => Some(&**row),
            _ => None,
        })
        .collect()
}

fn only_row(ops: &[IngestOp]) -> TimelineEntry {
    let rows = rows(ops);
    assert_eq!(rows.len(), 1, "expected one row, got {ops:?}");
    rows[0].clone()
}

fn samples(ops: &[IngestOp]) -> Vec<&SessionUsageRow> {
    ops.iter()
        .filter_map(|op| match op {
            IngestOp::SessionUsage(sample) => Some(sample),
            _ => None,
        })
        .collect()
}

fn drift_codes(ops: &[IngestOp]) -> Vec<(&str, &str)> {
    ops.iter()
        .filter_map(|op| match op {
            IngestOp::Drift(fact) => Some((fact.record_kind.as_str(), fact.error_code.as_str())),
            _ => None,
        })
        .collect()
}

fn metadata(created_at: u64) -> Value {
    json!({"type":"metadata","protocol_version":"1.5","created_at":created_at})
}

fn profile_bind(time: u64, alias: &str, effort: &str) -> Value {
    json!({"type":"profile.bind","agentId":"main","modelAlias":alias,"profileName":"agent",
        "thinkingEffort":effort,"systemPrompt":"x","environmentDisclosure":{"cwd":"/work"},
        "agentsMdPaths":[],"activeToolNames":[],"disallowedTools":[],"time":time})
}

/// The 0.38.0 shape: every field present.
fn llm_request(time: u64, kind: &str, model: &str, alias: &str) -> Value {
    json!({"type":"llm.request","agentId":"main","kind":kind,"provider":"openai","model":model,
        "modelAlias":alias,"thinkingEffort":"high","thinkingKeep":"all","maxTokens":262144,
        "toolSelect":true,"systemPromptHash":"h","toolsHash":"t","messageCount":3,"time":time})
}

fn usage_record(time: u64, model: &str, scope: Option<&str>, usage: (i64, u64, u64)) -> Value {
    let mut record = json!({"type":"usage.record","agentId":"main","model":model,
        "usage":{"inputOther":usage.0,"output":usage.1,"inputCacheRead":usage.2,"inputCacheCreation":0},
        "time":time});
    if let Some(scope) = scope {
        record["usageScope"] = json!(scope);
    }
    record
}

fn step_begin(time: u64, uuid: &str, turn: &str) -> Value {
    json!({"type":"context.append_loop_event","agentId":"main",
        "event":{"type":"step.begin","uuid":uuid,"turnId":turn,"step":1},"time":time})
}

fn step_end(time: u64, uuid: &str, turn: &str) -> Value {
    json!({"type":"context.append_loop_event","agentId":"main",
        "event":{"type":"step.end","uuid":uuid,"turnId":turn,"step":1,"finishReason":"end_turn",
            "usage":{"inputOther":0,"output":0,"inputCacheRead":0,"inputCacheCreation":0}},
        "time":time})
}

fn think(time: u64, uuid: &str, step: &str, text: &str) -> Value {
    json!({"type":"context.append_loop_event","agentId":"main",
        "event":{"type":"content.part","uuid":uuid,"turnId":"0","step":1,"stepUuid":step,
            "part":{"type":"think","think":text}},"time":time})
}

fn text(time: u64, uuid: &str, step: &str, text: &str) -> Value {
    json!({"type":"context.append_loop_event","agentId":"main",
        "event":{"type":"content.part","uuid":uuid,"turnId":"0","step":1,"stepUuid":step,
            "part":{"type":"text","text":text}},"time":time})
}

fn tool_call(time: u64, uuid: &str, step: &str, call: &str) -> Value {
    json!({"type":"context.append_loop_event","agentId":"main",
        "event":{"type":"tool.call","uuid":uuid,"turnId":"0","step":1,"stepUuid":step,
            "toolCallId":call,"name":"Shell","args":{"command":"true"}},"time":time})
}

fn turn_ended(time: u64, turn: u64) -> Value {
    json!({"type":"turn.ended","agentId":"main","turnId":turn,"reason":"completed",
        "durationMs":10,"time":time})
}

/// `forkedRecord(agentId)` kimi@97815218.
fn forked(time: u64, agent: &str) -> Value {
    json!({"type":"forked","agentId":agent,"time":time})
}

fn step_sample_key(ctx: &NormalizeCtx, step: &str) -> String {
    ids::usage_id(&ctx.session_id, &format!("step:{step}"))
}

#[test]
fn an_empty_think_part_never_owns_the_request() {
    let mut ctx = context();
    parsed(&mut ctx, &profile_bind(1, "kimi-code/k3", "high"));
    parsed(&mut ctx, &step_begin(10, "step-1", "0"));
    parsed(&mut ctx, &llm_request(11, "loop", "k3", "kimi-code/k3"));
    parsed(
        &mut ctx,
        &usage_record(12, "kimi-code/k3", Some("turn"), (100, 20, 300)),
    );
    // K3 opens every step with a think part; 93 of 248 were empty locally.
    let empty = parsed(&mut ctx, &think(13, "part-1", "step-1", ""));
    assert!(
        empty.is_empty(),
        "an empty think part projects to nothing: {empty:?}"
    );
    let reply = parsed(&mut ctx, &text(14, "part-2", "step-1", "reply"));
    let row = only_row(&reply);
    assert!(matches!(row.body, TimelineBody::Agent { .. }));
    assert_eq!(row.usage.input, Some(100));
    assert_eq!(row.usage.output, Some(20));
    assert_eq!(row.usage.cache_read, Some(300));
    assert_eq!(row.model.as_deref(), Some("k3"));
    assert_eq!(row.effort.as_deref(), Some("high"));
    assert!(parsed(&mut ctx, &step_end(15, "step-1", "0")).is_empty());
}

#[test]
fn the_first_visible_row_of_the_step_owns_the_request() {
    let mut ctx = context();
    parsed(&mut ctx, &step_begin(10, "step-1", "0"));
    parsed(&mut ctx, &llm_request(11, "loop", "k3", "kimi-code/k3"));
    parsed(
        &mut ctx,
        &usage_record(12, "kimi-code/k3", Some("turn"), (50, 5, 0)),
    );
    let thinking = parsed(&mut ctx, &think(13, "part-1", "step-1", "reasoning"));
    let row = only_row(&thinking);
    assert!(matches!(row.body, TimelineBody::Thinking { .. }));
    assert!(row.usage.is_empty(), "a think row never owns the request");
    let call = parsed(&mut ctx, &tool_call(14, "part-2", "step-1", "call-1"));
    assert_eq!(only_row(&call).usage.input, Some(50));
    let reply = parsed(&mut ctx, &text(15, "part-3", "step-1", "after the call"));
    assert!(only_row(&reply).usage.is_empty());
    assert!(parsed(&mut ctx, &step_end(16, "step-1", "0")).is_empty());
}

#[test]
fn a_step_without_a_visible_row_flushes_a_step_sample() {
    let mut ctx = context();
    parsed(&mut ctx, &step_begin(10, "step-1", "0"));
    parsed(&mut ctx, &llm_request(11, "loop", "k3", "kimi-code/k3"));
    parsed(
        &mut ctx,
        &usage_record(12, "kimi-code/k3", Some("turn"), (70, 7, 700)),
    );
    assert!(
        only_row(&parsed(
            &mut ctx,
            &think(13, "part-1", "step-1", "only reasoning")
        ))
        .usage
        .is_empty()
    );
    let closed = parsed(&mut ctx, &step_end(14, "step-1", "0"));
    let [IngestOp::SessionUsage(sample)] = closed.as_slice() else {
        panic!("expected one step sample, got {closed:?}");
    };
    assert_eq!(sample.key, step_sample_key(&ctx, "step-1"));
    assert_eq!(sample.ts, 14);
    assert_eq!(sample.model.as_deref(), Some("k3"));
    assert_eq!(
        (sample.tokens.input, sample.tokens.output),
        (Some(70), Some(7))
    );
    assert_eq!(sample.tokens.cache_read, Some(700));
    // The next step starts clean.
    parsed(&mut ctx, &step_begin(20, "step-2", "0"));
    parsed(&mut ctx, &llm_request(21, "loop", "k3", "kimi-code/k3"));
    assert!(
        only_row(&parsed(&mut ctx, &text(22, "part-2", "step-2", "reply")))
            .usage
            .is_empty()
    );
}

#[test]
fn an_unclosed_step_does_not_hand_its_request_to_the_next_step() {
    let mut ctx = context();
    parsed(&mut ctx, &step_begin(10, "step-1", "0"));
    parsed(&mut ctx, &llm_request(11, "loop", "k3", "kimi-code/k3"));
    parsed(
        &mut ctx,
        &usage_record(12, "kimi-code/k3", Some("turn"), (30, 3, 0)),
    );
    // No parts, no step.end: the process died. The next step's rows are its own.
    let reopened = parsed(&mut ctx, &step_begin(20, "step-2", "1"));
    let flushed = samples(&reopened);
    assert_eq!(flushed.len(), 1);
    assert_eq!(flushed[0].key, step_sample_key(&ctx, "step-1"));
    assert_eq!(flushed[0].tokens.input, Some(30));
    parsed(&mut ctx, &llm_request(21, "loop", "k3", "kimi-code/k3"));
    assert!(
        only_row(&parsed(&mut ctx, &text(22, "part-1", "step-2", "reply")))
            .usage
            .is_empty()
    );
}

#[test]
fn a_turn_that_ends_with_a_pending_request_flushes_it() {
    let mut ctx = context();
    parsed(&mut ctx, &step_begin(10, "step-1", "0"));
    parsed(&mut ctx, &llm_request(11, "loop", "k3", "kimi-code/k3"));
    parsed(
        &mut ctx,
        &usage_record(12, "kimi-code/k3", Some("turn"), (40, 4, 0)),
    );
    let ended = parsed(&mut ctx, &turn_ended(13, 0));
    let flushed = samples(&ended);
    assert_eq!(flushed.len(), 1);
    assert_eq!(flushed[0].key, step_sample_key(&ctx, "step-1"));
    assert_eq!(flushed[0].ts, 13);
}

/// A whole-session fork: the source's wire byte-copied, then the marker,
/// then the fork's own turn. The state document's `createdAt` is stamped
/// after the marker.
fn whole_session_fork(source_created: u64, forked_at: u64) -> Vec<Value> {
    vec![
        metadata(source_created),
        profile_bind(source_created + 20, "kimi-code/k3", "high"),
        step_begin(source_created + 100, "copied-step", "0"),
        llm_request(source_created + 101, "loop", "k3", "kimi-code/k3"),
        usage_record(
            source_created + 150,
            "kimi-code/k3",
            Some("turn"),
            (1_000, 100, 9_000),
        ),
        think(source_created + 151, "copied-think", "copied-step", ""),
        text(
            source_created + 152,
            "copied-text",
            "copied-step",
            "copied reply",
        ),
        step_end(source_created + 153, "copied-step", "0"),
        turn_ended(source_created + 154, 0),
        llm_request(source_created + 200, "compaction", "k3", "kimi-code/k3"),
        usage_record(
            source_created + 250,
            "kimi-code/k3",
            Some("session"),
            (500, 50, 0),
        ),
        forked(forked_at, "main"),
        step_begin(forked_at + 5_000, "own-step", "1"),
        llm_request(forked_at + 5_001, "loop", "k3", "kimi-code/k3"),
        usage_record(
            forked_at + 5_050,
            "kimi-code/k3",
            Some("turn"),
            (2_000, 200, 0),
        ),
        text(forked_at + 5_051, "own-text", "own-step", "own reply"),
        step_end(forked_at + 5_052, "own-step", "1"),
        turn_ended(forked_at + 5_053, 1),
    ]
}

#[test]
fn a_forks_copied_prefix_keeps_its_rows_and_drops_its_usage() {
    let source_created = 1_790_000_000_000;
    let forked_at = source_created + 60_000;
    let mut ctx = context();
    // state.json: forkedFrom + createdAt, taken after the marker was appended.
    seed_fork_instant(&mut ctx, forked_at as i64 + 10);

    let mut all = Vec::new();
    for record in whole_session_fork(source_created, forked_at) {
        all.extend(parsed(&mut ctx, &record));
    }
    let rows = rows(&all);
    let copied = rows
        .iter()
        .find(|row| row.native_id.as_deref() == Some("copied-text"))
        .expect("the copied reply keeps its row");
    assert!(
        copied.usage.is_empty(),
        "copied usage was counted in the source"
    );
    let own = rows
        .iter()
        .find(|row| row.native_id.as_deref() == Some("own-text"))
        .expect("the fork's own reply");
    assert_eq!(
        (own.usage.input, own.usage.output),
        (Some(2_000), Some(200))
    );
    assert!(
        samples(&all).is_empty(),
        "the copied compaction sample and the copied step leave nothing: {all:?}"
    );
    assert!(drift_codes(&all).is_empty(), "{:?}", drift_codes(&all));
}

#[test]
fn without_a_fork_instant_the_copied_prefix_still_counts() {
    // The gate needs the bounded metadata fact. A wire read without it (no
    // state.json, or a pipeline that does not pass `forked_at` yet) is an
    // ordinary session as far as the adapter can prove.
    let source_created = 1_790_000_000_000;
    let mut ctx = context();
    let mut all = Vec::new();
    for record in whole_session_fork(source_created, source_created + 60_000) {
        all.extend(parsed(&mut ctx, &record));
    }
    let copied = rows(&all)
        .into_iter()
        .find(|row| row.native_id.as_deref() == Some("copied-text"))
        .expect("copied reply");
    assert_eq!(copied.usage.input, Some(1_000));
    assert_eq!(samples(&all).len(), 1, "the copied compaction sample");
}

#[test]
fn a_turn_sliced_fork_gates_its_subagent_wire_at_the_same_instant() {
    // `copyAgentWire` rewrites each retained wire and appends the marker;
    // subagent wires are cut at the main slice's cutoff time and the fork's
    // state.json lists every retained agent, so all share one instant.
    let source_created = 1_790_000_000_000;
    let forked_at = source_created + 60_000;
    let mut ctx = context_for(&format!("{ROOT}/agents/agent-0"));
    seed_fork_instant(&mut ctx, forked_at as i64 + 10);
    let records = [
        metadata(source_created + 300),
        step_begin(source_created + 400, "copied-step", "0"),
        llm_request(source_created + 401, "loop", "k3", "kimi-code/k3"),
        usage_record(
            source_created + 450,
            "kimi-code/k3",
            Some("turn"),
            (800, 80, 0),
        ),
        tool_call(
            source_created + 451,
            "copied-call",
            "copied-step",
            "call-copied",
        ),
        step_end(source_created + 452, "copied-step", "0"),
        forked(forked_at, "agent-0"),
        step_begin(forked_at + 7_000, "own-step", "1"),
        llm_request(forked_at + 7_001, "loop", "k3", "kimi-code/k3"),
        usage_record(
            forked_at + 7_050,
            "kimi-code/k3",
            Some("turn"),
            (900, 90, 0),
        ),
        tool_call(forked_at + 7_051, "own-call", "own-step", "call-own"),
        step_end(forked_at + 7_052, "own-step", "1"),
    ];
    let mut all = Vec::new();
    for record in records {
        all.extend(parsed(&mut ctx, &record));
    }
    let rows = rows(&all);
    assert_eq!(rows.len(), 2);
    assert!(rows[0].usage.is_empty());
    assert_eq!(rows[1].usage.input, Some(900));
    assert!(samples(&all).is_empty());
}

#[test]
fn a_2x_llm_request_without_optional_fields_parses() {
    let mut ctx = context();
    parsed(&mut ctx, &profile_bind(1, "kimi-code/k3", "high"));
    parsed(&mut ctx, &step_begin(10, "step-1", "0"));
    // `llmRequestSchema` kimi@103479471: modelAlias, thinkingEffort,
    // thinkingKeep and maxTokens optional; further optional fields are drift.
    let request = ingest(
        &mut ctx,
        &json!({"type":"llm.request","agentId":"main","kind":"loop","provider":"openai",
            "model":"k3-256k","toolSelect":true,"systemPromptHash":"h","toolsHash":"t",
            "messageCount":3,"temperature":0.6,"projection":"strict","time":11}),
    );
    assert_eq!(request.status, LineStatus::Parsed);
    assert_eq!(
        drift_codes(&request.ops),
        vec![("llm.request", "unknown_fields")]
    );
    parsed(
        &mut ctx,
        &usage_record(12, "kimi-code/k3", Some("turn"), (10, 1, 0)),
    );
    let row = only_row(&parsed(&mut ctx, &text(13, "part-1", "step-1", "reply")));
    assert_eq!(row.model.as_deref(), Some("k3-256k"));
    // The bound effort still applies when the request omits its own.
    assert_eq!(row.effort.as_deref(), Some("high"));
    assert_eq!(row.usage.input, Some(10));
}

#[test]
fn an_unparsed_llm_request_falls_back_to_the_recorded_alias() {
    let mut ctx = context();
    parsed(&mut ctx, &step_begin(10, "step-1", "0"));
    parsed(&mut ctx, &llm_request(11, "loop", "k3", "kimi-code/k3"));
    parsed(
        &mut ctx,
        &usage_record(12, "kimi-code/k3", Some("turn"), (10, 1, 0)),
    );
    assert_eq!(
        only_row(&parsed(&mut ctx, &text(13, "part-1", "step-1", "reply")))
            .model
            .as_deref(),
        Some("k3")
    );
    parsed(&mut ctx, &step_end(14, "step-1", "0"));

    parsed(&mut ctx, &step_begin(20, "step-2", "0"));
    let mut broken = llm_request(21, "loop", "k3-256k", "kimi-code/k3");
    broken["messageCount"] = json!("three");
    let failed = ingest(&mut ctx, &broken);
    assert_eq!(
        failed.status,
        LineStatus::HardError {
            code: "parse_failed".to_owned()
        }
    );
    parsed(
        &mut ctx,
        &usage_record(22, "kimi-code/k3", Some("turn"), (20, 2, 0)),
    );
    let row = only_row(&parsed(&mut ctx, &text(23, "part-2", "step-2", "reply")));
    assert_eq!(
        row.model.as_deref(),
        Some("kimi-code/k3"),
        "the alias, never the stale k3"
    );
    assert_eq!(row.effort, None);
    assert_eq!(row.usage.input, Some(20));
}

#[test]
fn a_negative_input_other_clamps_with_drift() {
    let mut ctx = context();
    parsed(&mut ctx, &step_begin(10, "step-1", "0"));
    parsed(&mut ctx, &llm_request(11, "loop", "k3", "kimi-code/k3"));
    // `parseOpenAIUsage` kimi@98617262: prompt_tokens − cached_tokens, unclamped.
    let record = ingest(
        &mut ctx,
        &usage_record(12, "kimi-code/k3", Some("turn"), (-5, 9, 400)),
    );
    assert_eq!(record.status, LineStatus::Parsed);
    assert_eq!(
        drift_codes(&record.ops),
        vec![("usage.record", "usage_subset_exceeds_total")]
    );
    let row = only_row(&parsed(&mut ctx, &text(13, "part-1", "step-1", "reply")));
    assert_eq!(row.usage.input, Some(0));
    assert_eq!(row.usage.cache_read, Some(400));
    assert_eq!(row.usage.output, Some(9));
}

#[test]
fn a_missing_usage_scope_is_soft_drift() {
    let mut ctx = context();
    let record = ingest(&mut ctx, &usage_record(12, "kimi-code/k3", None, (5, 1, 0)));
    assert_eq!(record.status, LineStatus::UnknownType);
    assert_eq!(
        drift_codes(&record.ops),
        vec![("usage.record/<none>", "unknown_subtype")]
    );
}

#[test]
fn a_compaction_sample_names_the_request_model() {
    let mut ctx = context();
    parsed(&mut ctx, &profile_bind(1, "kimi-code/k3", "high"));
    parsed(
        &mut ctx,
        &llm_request(11, "compaction", "k3", "kimi-code/k3"),
    );
    let record = parsed(
        &mut ctx,
        &usage_record(12, "kimi-code/k3", Some("session"), (500, 50, 0)),
    );
    let [IngestOp::SessionUsage(sample)] = record.as_slice() else {
        panic!("expected one compaction sample, got {record:?}");
    };
    // One key per model: the API id the rows carry, not the config alias.
    assert_eq!(sample.model.as_deref(), Some("k3"));
    assert_eq!(sample.tokens.input, Some(500));
}

#[test]
fn checkpoint_carries_the_open_step_and_the_fork_instant() {
    let forked_at = 1_790_000_060_010_i64;
    let mut ctx = context();
    seed_fork_instant(&mut ctx, forked_at);
    parsed(&mut ctx, &step_begin(forked_at as u64 + 100, "step-1", "0"));
    parsed(
        &mut ctx,
        &llm_request(forked_at as u64 + 101, "loop", "k3", "kimi-code/k3"),
    );
    parsed(
        &mut ctx,
        &usage_record(
            forked_at as u64 + 150,
            "kimi-code/k3",
            Some("turn"),
            (10, 1, 0),
        ),
    );
    let checkpoint = KimiCodeAdapter
        .checkpoint_state(ctx.state.as_deref())
        .expect("serializes")
        .expect("payload");
    assert_eq!(checkpoint["forked_at"], json!(forked_at));
    assert_eq!(checkpoint["step"], json!("step-1"));
    assert!(checkpoint["pending_usage"].is_object());

    let mut resumed = context();
    resumed.next_ordinal = ctx.next_ordinal;
    resumed.state = KimiCodeAdapter
        .restore_state(Some(&checkpoint))
        .expect("restores");
    let row = only_row(&parsed(
        &mut resumed,
        &text(forked_at as u64 + 151, "part-1", "step-1", "reply"),
    ));
    assert_eq!(row.usage.input, Some(10));
    // The gate survives the resume: a copied record read after it still drops.
    parsed(
        &mut resumed,
        &usage_record(
            forked_at as u64 - 1,
            "kimi-code/k3",
            Some("session"),
            (9, 9, 0),
        ),
    );
    let closed = parsed(
        &mut resumed,
        &step_end(forked_at as u64 + 152, "step-1", "0"),
    );
    assert!(closed.is_empty(), "{closed:?}");
}

#[test]
fn a_fork_state_document_enumerates_its_sessions() {
    // `fork()` kimi@97834752: forkedFrom, fresh createdAt, the source's
    // updatedAt, titleKind, the retained agents, no custom/lastTurnReason.
    let document = format!(
        r#"{{"id":"{ROOT}","version":2,"cwd":"/work/project","createdAt":1790000060010,
            "updatedAt":1790000000154,"archived":false,"title":"Fork: source title",
            "titleKind":"replaceable","forkedFrom":"session_00000000-0000-4000-8000-000000000000",
            "agents":{{"main":{{"homedir":"/h/main","type":"main"}},
                      "agent-0":{{"homedir":"/h/a0","type":"sub","parentAgentId":"main","labels":{{}}}}}},
            "isCustomTitle":false}}"#
    );
    let sessions = KimiCodeAdapter.document_sessions(
        "session-state",
        &FileStat {
            relative_path: format!("sessions/wd/{ROOT}/state.json"),
            size: document.len() as u64,
            modified_ms: 0,
        },
        document.as_bytes(),
    );
    assert_eq!(sessions.len(), 2);
    assert_eq!(sessions[0].session_id.as_deref(), Some(ROOT));
    assert_eq!(sessions[0].role, Some(SessionRole::Main));
    assert_eq!(sessions[0].title.as_deref(), Some("Fork: source title"));
    assert_eq!(sessions[0].title_kind, Some(TitleKind::Explicit));
    assert_eq!(sessions[0].cwd.as_deref(), Some("/work/project"));
    assert_eq!(sessions[0].started_at, Some(1_790_000_060_010));
    assert_eq!(sessions[1].role, Some(SessionRole::Subagent));
    let state = rosetta_adapter_kimi_code::raw::parse_session_state(&document).expect("parses");
    assert_eq!(state.fork_instant(), Some(1_790_000_060_010));
}

#[test]
fn the_forked_marker_is_declared_bookkeeping() {
    let mut ctx = context();
    let outcome = ingest(&mut ctx, &forked(1_790_000_060_000, "main"));
    assert_eq!(outcome.status, LineStatus::Parsed);
    assert!(outcome.ops.is_empty(), "{:?}", outcome.ops);
}
