//! Synthetic values in the canonical cost-state shape; no local data in CI.
use rosetta_adapter_claude_code::ClaudeCodeAdapter;
use rosetta_adapter_support::{AgentAdapter, LineStatus, NormalizeCtx};
use rosetta_core::{IngestOp, ids};
use serde_json::{Value, json};

fn snapshot() -> Value {
    json!({
        "type": "cost-state", "sessionId": "s", "totalCostUSD": 2.29,
        "totalAPIDuration": 12, "totalAPIDurationWithoutRetries": 10,
        "totalToolDuration": 5, "totalLinesAdded": 3, "totalLinesRemoved": 1,
        "totalDuration": 42, "startTime": 1000, "hasUnknownModelCost": false,
        "modelUsage": {"claude-haiku-4-5": {
            "inputTokens": 100, "outputTokens": 200, "thinkingTokens": 50,
            "cacheReadInputTokens": 300, "cacheCreationInputTokens": 400,
            "webSearchRequests": 81, "costUSD": 2.29
        }}
    })
}

fn context() -> NormalizeCtx {
    let mut ctx = NormalizeCtx::new("runtime", ids::session_id("runtime", "s"));
    ctx.state = ClaudeCodeAdapter.init_state();
    ctx
}

fn ingest(ctx: &mut NormalizeCtx, stream: &str, value: &Value) -> Vec<IngestOp> {
    let result = ClaudeCodeAdapter.ingest_line(ctx, stream, value.to_string().as_bytes());
    assert_eq!(result.status, LineStatus::Parsed);
    result.ops
}

#[test]
fn cumulative_totals_are_not_requests_or_billed_money() {
    let mut ctx = context();
    let ops = ingest(&mut ctx, "transcript", &snapshot());
    let [IngestOp::SessionUsageTotals(total)] = ops.as_slice() else {
        panic!("{ops:?}")
    };
    assert_eq!(total.ts, 1000);
    assert_eq!(total.models.len(), 1);
    let model = &total.models[0];
    assert_eq!(
        (model.input, model.output, model.thinking),
        (100, 200, Some(50))
    );
    assert_eq!(
        (model.cache_read, model.cache_write, model.web_searches),
        (300, 400, 81)
    );
    assert_eq!(model.estimated_pico_usd, Some(2_290_000_000_000));
    assert_eq!(ingest(&mut ctx, "transcript", &snapshot()), ops);
}

#[test]
fn copied_snapshots_and_subagent_transcripts_do_not_claim_parent_totals() {
    let mut ctx = context();
    let mut copy = snapshot();
    copy["sessionId"] = json!("someone-else");
    assert!(ingest(&mut ctx, "transcript", &copy).is_empty());
    assert!(ingest(&mut ctx, "subagent-transcript", &snapshot()).is_empty());
}

#[test]
fn timestamp_attribution_survives_a_checkpoint_and_has_no_wall_clock_dependency() {
    let mut ctx = context();
    ingest(
        &mut ctx,
        "transcript",
        &json!({
            "type": "queue-operation", "sessionId": "s", "operation": "enqueue",
            "timestamp": "2026-09-01T01:00:00Z"
        }),
    );
    let state = ClaudeCodeAdapter
        .checkpoint_state(ctx.state.as_deref())
        .unwrap();
    let mut resumed = context();
    resumed.state = ClaudeCodeAdapter.restore_state(state.as_ref()).unwrap();
    let before = ingest(&mut ctx, "transcript", &snapshot());
    let after = ingest(&mut resumed, "transcript", &snapshot());
    assert_eq!(before, after);
    let IngestOp::SessionUsageTotals(total) = &after[0] else {
        panic!()
    };
    assert_eq!(total.ts, 1_788_224_400_000);
}

#[test]
fn optional_thinking_and_unknown_cost_do_not_invent_facts() {
    let mut line = snapshot();
    line["hasUnknownModelCost"] = json!(true);
    let model = &mut line["modelUsage"]["claude-haiku-4-5"];
    model.as_object_mut().unwrap().remove("thinkingTokens");
    model["costUSD"] = json!(0);
    let ops = ingest(&mut context(), "transcript", &line);
    let IngestOp::SessionUsageTotals(total) = &ops[0] else {
        panic!()
    };
    assert_eq!(total.models[0].thinking, None);
    assert_eq!(total.models[0].estimated_pico_usd, None);
}

#[test]
fn invalid_totals_drift_instead_of_inflating_accounting() {
    for (field, value) in [
        ("thinkingTokens", json!(201)),
        ("inputTokens", json!(u64::MAX)),
        ("costUSD", json!(-1)),
        ("costUSD", json!(1e100)),
    ] {
        let mut line = snapshot();
        line["modelUsage"]["claude-haiku-4-5"][field] = value;
        let ops = ingest(&mut context(), "transcript", &line);
        assert!(
            matches!(ops.as_slice(), [IngestOp::Drift(f)] if f.error_code == "invalid_usage_total"),
            "{ops:?}"
        );
    }
}

#[test]
fn new_nested_fields_are_still_reported_as_drift() {
    let mut line = snapshot();
    line["modelUsage"]["claude-haiku-4-5"]["futureCounter"] = json!(3);
    let ops = ingest(&mut context(), "transcript", &line);
    assert!(
        ops.iter()
            .any(|op| matches!(op, IngestOp::SessionUsageTotals(_)))
    );
    assert!(
        ops.iter()
            .any(|op| matches!(op, IngestOp::Drift(f) if f.error_code == "unknown_fields"))
    );
}
