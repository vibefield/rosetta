//! Usage accounting rules from the 2026-09-23 audit (PI-1, PI-2, PI-6, PI-8,
//! PI-9, PI-10), pinned by hermetic synthetic lines shaped from pi-mono
//! a8ed497 (0.87.x) and the 0.84.3 pin. Numbers and ids only; a native capture
//! of each shape is pending the 0.87.x re-pin.

use rosetta_adapter_pi::PiAdapter;
use rosetta_adapter_support::adapter::{AgentAdapter, LineStatus, NormalizeCtx};
use rosetta_core::rows::{SessionUsageRow, UsageScope};
use rosetta_core::timeline::{TimelineBody, TimelineEntry};
use rosetta_core::{IngestOp, ids};
use serde_json::{Value, json};

const HEADER_AT: &str = "2026-09-23T12:00:00.000Z";
const HEADER_MS: i64 = 1_790_164_800_000;

fn stamp(offset_s: u64) -> String {
    format!("2026-09-23T12:00:{offset_s:02}.000Z")
}

fn header(forked: bool) -> Value {
    let mut header = json!({
        "type": "session", "version": 3, "id": "native", "timestamp": HEADER_AT, "cwd": "<PROJECT>"
    });
    if forked {
        header["parentSession"] =
            json!("<ROOT>/agent/sessions/--work--/2026-09-23T11-00-00-000Z_parent.jsonl");
    }
    header
}

/// A reported usage record: every count present, `reasoning` included, the
/// shape every Pi API writes once a usage chunk arrived.
fn reported(input: u64, output: u64, cache_read: u64, cache_write: u64) -> Value {
    json!({
        "input": input, "output": output, "cacheRead": cache_read, "cacheWrite": cache_write,
        "reasoning": 0, "totalTokens": input + output + cache_read + cache_write,
        "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0}
    })
}

fn user(id: &str, parent: Option<&str>, at: u64) -> Value {
    json!({
        "type": "message", "id": id, "parentId": parent, "timestamp": stamp(at),
        "message": {"role": "user", "content": "prompt", "timestamp": HEADER_MS + at as i64 * 1000}
    })
}

fn assistant(id: &str, parent: &str, at: u64, provider: &str, model: &str) -> Value {
    json!({
        "type": "message", "id": id, "parentId": parent, "timestamp": stamp(at),
        "message": {
            "role": "assistant", "content": [{"type": "text", "text": "reply"}],
            "api": "openai-completions", "provider": provider, "model": model,
            "usage": reported(100, 10, 0, 0), "stopReason": "stop",
            "timestamp": HEADER_MS + at as i64 * 1000
        }
    })
}

fn tool_call(id: &str, parent: &str, at: u64) -> Value {
    let mut line = assistant(id, parent, at, "mock", "mock-model");
    line["message"]["content"] = json!([{"type": "toolCall", "id": format!("call-{id}"), "name": "bash", "arguments": {"command": "true"}}]);
    line["message"]["stopReason"] = json!("toolUse");
    line
}

fn tool_result(id: &str, parent: &str, at: u64) -> Value {
    json!({
        "type": "message", "id": id, "parentId": parent, "timestamp": stamp(at),
        "message": {
            "role": "toolResult", "toolCallId": format!("call-{parent}"), "toolName": "bash",
            "content": [{"type": "text", "text": "ok"}], "isError": false,
            "timestamp": HEADER_MS + at as i64 * 1000
        }
    })
}

fn model_change(id: &str, parent: Option<&str>, at: u64, model: &str) -> Value {
    json!({
        "type": "model_change", "id": id, "parentId": parent, "timestamp": stamp(at),
        "provider": "mock", "modelId": model
    })
}

fn compaction(id: &str, parent: &str, at: u64, first_kept: &str, from_hook: Option<bool>) -> Value {
    let mut line = json!({
        "type": "compaction", "id": id, "parentId": parent, "timestamp": stamp(at),
        "summary": "summary", "firstKeptEntryId": first_kept, "tokensBefore": 400_000,
        "usage": reported(300_000, 2_000, 0, 0)
    });
    if let Some(from_hook) = from_hook {
        line["fromHook"] = json!(from_hook);
    }
    line
}

fn branch_summary(id: &str, parent: &str, at: u64, from_hook: Option<bool>) -> Value {
    let mut line = json!({
        "type": "branch_summary", "id": id, "parentId": parent, "timestamp": stamp(at),
        "fromId": parent, "summary": "summary", "usage": reported(50_000, 500, 0, 0)
    });
    if let Some(from_hook) = from_hook {
        line["fromHook"] = json!(from_hook);
    }
    line
}

fn usage_entry(id: &str, parent: &str, at: u64, kind: &str, provider: &str, model: &str) -> Value {
    json!({
        "type": "usage", "id": id, "parentId": parent, "timestamp": stamp(at),
        "kind": kind, "provider": provider, "model": model, "usage": reported(0, 1, 50_000, 0)
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

fn parsed(lines: &[Value]) -> Vec<IngestOp> {
    let (ops, statuses) = run(lines);
    assert!(
        statuses.iter().all(|status| *status == LineStatus::Parsed),
        "{statuses:?}"
    );
    assert!(
        !ops.iter().any(|op| matches!(op, IngestOp::Drift(_))),
        "unexpected drift"
    );
    ops
}

fn messages(ops: &[IngestOp]) -> Vec<&TimelineEntry> {
    ops.iter()
        .filter_map(|op| match op {
            IngestOp::Message(row) => Some(&**row),
            _ => None,
        })
        .collect()
}

fn samples(ops: &[IngestOp]) -> Vec<&SessionUsageRow> {
    ops.iter()
        .filter_map(|op| match op {
            IngestOp::SessionUsage(row) => Some(row),
            _ => None,
        })
        .collect()
}

fn summaries(ops: &[IngestOp]) -> Vec<&TimelineEntry> {
    messages(ops)
        .into_iter()
        .filter(|row| matches!(row.body, TimelineBody::Summary { .. }))
        .collect()
}

fn assistants(ops: &[IngestOp]) -> Vec<&TimelineEntry> {
    messages(ops)
        .into_iter()
        .filter(|row| matches!(row.body, TimelineBody::Agent { .. }))
        .collect()
}

// PI-1: `usage` entries (pi-mono a8ed497 `session-manager.ts:80-89`,
// `cache-warmer.ts:331-349`) are recorded requests outside the tree.
#[test]
fn usage_entries_become_session_samples_keyed_by_entry_id() {
    let ops = parsed(&[
        header(false),
        json!({"type": "thinking_level_change", "id": "think", "parentId": null,
               "timestamp": stamp(1), "thinkingLevel": "high"}),
        user("u1", Some("think"), 2),
        assistant("a1", "u1", 3, "anthropic", "claude-sonnet-4-5"),
        usage_entry(
            "warm",
            "a1",
            4,
            "cache_warm",
            "anthropic",
            "claude-sonnet-4-5",
        ),
        usage_entry(
            "other",
            "warm",
            5,
            "future_kind",
            "anthropic",
            "claude-sonnet-4-5",
        ),
        // Ancestry passes through usage entries like any other silent entry.
        user("u2", Some("other"), 6),
        assistant("a2", "u2", 7, "anthropic", "claude-sonnet-4-5"),
    ]);
    let samples = samples(&ops);
    assert_eq!(samples.len(), 2);

    let warm = samples[0];
    assert_eq!(warm.key, ids::usage_id("session", "warm"));
    assert_eq!(warm.ts, HEADER_MS + 4_000);
    assert_eq!(warm.model.as_deref(), Some("claude-sonnet-4-5"));
    assert_eq!(warm.tokens.route.as_deref(), Some("anthropic"));
    assert_eq!(warm.tokens.scope, UsageScope::Request);
    assert_eq!(warm.tokens.requests, None);
    assert_eq!(warm.tokens.cache_read, Some(50_000));
    assert_eq!(warm.tokens.output, Some(1));
    assert_eq!(warm.tokens.thinking, Some(0));

    // Unknown kinds are still usage, but Pi does not say how many requests.
    let other = samples[1];
    assert_eq!(other.key, ids::usage_id("session", "other"));
    assert_eq!(other.tokens.scope, UsageScope::Aggregate);
    assert_eq!(other.tokens.requests, None);

    assert!(
        summaries(&ops).is_empty(),
        "usage entries have no timeline row"
    );
    let replies = assistants(&ops);
    assert_eq!(replies.len(), 2);
    assert_eq!(replies[1].effort.as_deref(), Some("high"));
}

#[test]
fn a_forks_copied_usage_entries_keep_no_usage() {
    let copied = json!({
        "type": "usage", "id": "copied", "parentId": null, "timestamp": "2026-09-23T11:30:00.000Z",
        "kind": "cache_warm", "provider": "anthropic", "model": "claude-sonnet-4-5",
        "usage": reported(0, 1, 50_000, 0)
    });
    let ops = parsed(&[
        header(true),
        copied,
        usage_entry(
            "own",
            "copied",
            1,
            "cache_warm",
            "anthropic",
            "claude-sonnet-4-5",
        ),
    ]);
    let samples = samples(&ops);
    assert_eq!(samples.len(), 1);
    assert_eq!(samples[0].key, ids::usage_id("session", "own"));
}

// PI-2: one summarization request, two when the cut splits a turn with
// history (`compaction.ts:1016-1076` at a8ed497; `:873-932` at 0.84.3).
fn compaction_usage(lines: &[Value]) -> (UsageScope, Option<i64>) {
    let ops = parsed(lines);
    let rows = summaries(&ops);
    assert_eq!(rows.len(), 1);
    let usage = &rows[0].usage;
    assert_eq!(usage.input, Some(300_000));
    (usage.scope, usage.requests)
}

#[test]
fn compaction_at_a_turn_start_is_one_request() {
    let lines = [
        header(false),
        user("u1", None, 1),
        assistant("a1", "u1", 2, "mock", "mock-model"),
        user("u2", Some("a1"), 3),
        assistant("a2", "u2", 4, "mock", "mock-model"),
        compaction("c1", "a2", 5, "u2", Some(false)),
    ];
    assert_eq!(compaction_usage(&lines), (UsageScope::Request, None));
}

#[test]
fn compaction_that_splits_a_turn_with_history_is_two_requests() {
    let lines = [
        header(false),
        user("u1", None, 1),
        assistant("a1", "u1", 2, "mock", "mock-model"),
        user("u2", Some("a1"), 3),
        tool_call("a2", "u2", 4),
        tool_result("r2", "a2", 5),
        assistant("a3", "r2", 6, "mock", "mock-model"),
        // Cut inside u2's turn: history [u1, a1] plus the turn prefix.
        compaction("c1", "a3", 7, "a3", Some(false)),
    ];
    assert_eq!(compaction_usage(&lines), (UsageScope::Aggregate, Some(2)));
}

#[test]
fn compaction_that_splits_the_first_turn_is_one_request() {
    let lines = [
        header(false),
        model_change("m1", None, 0, "mock-model"),
        user("u1", Some("m1"), 1),
        tool_call("a1", "u1", 2),
        tool_result("r1", "a1", 3),
        assistant("a2", "r1", 4, "mock", "mock-model"),
        // Nothing precedes u1: only the turn-prefix request runs.
        compaction("c1", "a2", 5, "a2", Some(false)),
    ];
    assert_eq!(compaction_usage(&lines), (UsageScope::Request, None));
}

#[test]
fn retain_none_compaction_is_one_request() {
    let lines = [
        header(false),
        user("u1", None, 1),
        tool_call("a1", "u1", 2),
        tool_result("r1", "a1", 3),
        assistant("a2", "r1", 4, "mock", "mock-model"),
        compaction("c1", "a2", 5, "c1", Some(false)),
    ];
    assert_eq!(compaction_usage(&lines), (UsageScope::Request, None));
}

#[test]
fn second_compaction_counts_history_from_the_previous_kept_range() {
    let before = [
        header(false),
        user("u1", None, 1),
        assistant("a1", "u1", 2, "mock", "mock-model"),
        user("u2", Some("a1"), 3),
        assistant("a2", "u2", 4, "mock", "mock-model"),
        compaction("c1", "a2", 5, "u2", Some(false)),
    ];
    let ops_before = parsed(&before);
    assert_eq!(summaries(&ops_before).len(), 1);

    // The turn split by c2 starts at c1's first kept entry: no history left.
    let mut at_boundary = before.to_vec();
    at_boundary.extend([
        tool_call("a3", "c1", 6),
        tool_result("r3", "a3", 7),
        assistant("a4", "r3", 8, "mock", "mock-model"),
        compaction("c2", "a4", 9, "a4", Some(false)),
    ]);
    let ops = parsed(&at_boundary);
    let rows = summaries(&ops);
    assert_eq!(rows.len(), 2);
    assert_eq!(
        (rows[1].usage.scope, rows[1].usage.requests),
        (UsageScope::Request, None)
    );

    // A new turn after c1: its history is c1's kept tail.
    let mut after_tail = before.to_vec();
    after_tail.extend([
        user("u3", Some("c1"), 6),
        tool_call("a3", "u3", 7),
        tool_result("r3", "a3", 8),
        assistant("a4", "r3", 9, "mock", "mock-model"),
        compaction("c2", "a4", 10, "a4", Some(false)),
    ]);
    let ops = parsed(&after_tail);
    let rows = summaries(&ops);
    assert_eq!(
        (rows[1].usage.scope, rows[1].usage.requests),
        (UsageScope::Aggregate, Some(2))
    );

    // After a retain-none compaction nothing precedes the new turn.
    let retain_none = [
        header(false),
        user("u1", None, 1),
        assistant("a1", "u1", 2, "mock", "mock-model"),
        compaction("c1", "a1", 3, "c1", Some(false)),
        user("u2", Some("c1"), 4),
        tool_call("a2", "u2", 5),
        tool_result("r2", "a2", 6),
        assistant("a3", "r2", 7, "mock", "mock-model"),
        compaction("c2", "a3", 8, "a3", Some(false)),
    ];
    let ops = parsed(&retain_none);
    let rows = summaries(&ops);
    assert_eq!(
        (rows[1].usage.scope, rows[1].usage.requests),
        (UsageScope::Request, None)
    );
}

#[test]
fn compaction_request_count_survives_a_checkpoint_resume() {
    let adapter = PiAdapter;
    let lines = [
        header(false),
        user("u1", None, 1),
        assistant("a1", "u1", 2, "mock", "mock-model"),
        user("u2", Some("a1"), 3),
        tool_call("a2", "u2", 4),
        tool_result("r2", "a2", 5),
        assistant("a3", "r2", 6, "mock", "mock-model"),
        compaction("c1", "a3", 7, "a3", Some(false)),
    ];
    let (one_shot, _) = run(&lines);

    let mut first = NormalizeCtx::new("runtime", "session");
    first.state = adapter.init_state();
    let mut resumed = Vec::new();
    for (index, line) in lines[..5].iter().enumerate() {
        first.src_cursor = Some(index as i64 + 1);
        resumed.extend(
            adapter
                .ingest_line(&mut first, "session", line.to_string().as_bytes())
                .ops,
        );
    }
    let checkpoint = adapter
        .checkpoint_state(first.state.as_deref())
        .expect("checkpoint serializes");
    let mut second = NormalizeCtx::new("runtime", "session");
    second.next_ordinal = first.next_ordinal;
    second.state = adapter
        .restore_state(checkpoint.as_ref())
        .expect("checkpoint restores");
    for (index, line) in lines[5..].iter().enumerate() {
        second.src_cursor = Some(index as i64 + 6);
        resumed.extend(
            adapter
                .ingest_line(&mut second, "session", line.to_string().as_bytes())
                .ops,
        );
    }
    assert_eq!(resumed, one_shot);
    let rows = summaries(&one_shot);
    assert_eq!(
        (rows[0].usage.scope, rows[0].usage.requests),
        (UsageScope::Aggregate, Some(2))
    );
}

// PI-6: summaries run on the session's current model, the latest in file
// order (`agent-session.ts:2414`, `:3675`); an extension's (`fromHook`) model
// is unrecorded.
#[test]
fn summaries_take_the_latest_model_in_file_order() {
    let ops = parsed(&[
        header(false),
        model_change("m1", None, 0, "picked-model"),
        user("u1", Some("m1"), 1),
        // The provider served a different id than the one picked.
        {
            let mut line = assistant("a1", "u1", 2, "mock", "picked-model");
            line["message"]["responseModel"] = json!("served-model");
            line
        },
        branch_summary("b1", "a1", 3, Some(false)),
        model_change("m2", Some("a1"), 4, "switched-model"),
        user("u2", Some("m2"), 5),
        assistant("a2", "u2", 6, "mock", "switched-model"),
        compaction("c1", "a2", 7, "u2", Some(false)),
        compaction("c2", "c1", 8, "u2", Some(true)),
        branch_summary("b2", "c2", 9, Some(true)),
    ]);
    let rows = summaries(&ops);
    assert_eq!(
        rows.iter()
            .map(|row| row.model.as_deref())
            .collect::<Vec<_>>(),
        [Some("served-model"), Some("switched-model"), None, None]
    );
    assert!(rows.iter().all(|row| !row.usage.is_empty()));
}

#[test]
fn a_resumed_session_without_model_change_still_prices_its_summary() {
    let ops = parsed(&[
        header(false),
        user("u1", None, 1),
        assistant("a1", "u1", 2, "mock", "resumed-model"),
        user("u2", Some("a1"), 3),
        assistant("a2", "u2", 4, "mock", "resumed-model"),
        compaction("c1", "a2", 5, "u2", None),
    ]);
    let rows = summaries(&ops);
    assert_eq!(rows[0].model.as_deref(), Some("resumed-model"));
}

// PI-8: a missing usage count must not demote the untagged message to
// `Other` and drop the turn.
#[test]
fn a_missing_usage_count_keeps_the_assistant_turn() {
    let mut line = assistant("a1", "u1", 2, "mock", "mock-model");
    line["message"]["usage"] = json!({"input": 10, "output": 2, "reasoning": 1});
    let ops = parsed(&[header(false), user("u1", None, 1), line]);
    let replies = assistants(&ops);
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0].usage.input, Some(10));
    assert_eq!(replies[0].usage.output, Some(1));
    assert_eq!(replies[0].usage.thinking, Some(1));
    assert_eq!(replies[0].usage.cache_read, None);
    assert_eq!(replies[0].usage.cache_write, None);
}

// PI-9: the all-zero placeholder every API starts from
// (`openai-completions.ts:314-321` at a8ed497) is unreported, not $0.
#[test]
fn never_reported_usage_is_unreported_not_zero() {
    let mut placeholder = assistant("a1", "u1", 2, "mock", "mock-model");
    placeholder["message"]["usage"] = json!({
        "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0,
        "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0}
    });
    placeholder["message"]["stopReason"] = json!("error");
    let mut reported_zero = assistant("a2", "a1", 3, "mock", "mock-model");
    reported_zero["message"]["usage"] = json!({
        "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "reasoning": 0, "totalTokens": 0,
        "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0}
    });
    let ops = parsed(&[
        header(false),
        user("u1", None, 1),
        placeholder,
        reported_zero,
    ]);
    let replies = assistants(&ops);
    assert_eq!(replies.len(), 2);
    assert!(replies[0].usage.is_empty());
    assert_eq!(replies[1].usage.input, Some(0));
    assert_eq!(replies[1].usage.route.as_deref(), Some("mock"));
}

// PI-10: free-form custom provider ids (`model-config.ts:243`) and an empty
// `responseModel`.
#[test]
fn custom_provider_ids_route_by_their_leading_segment() {
    let mut empty_response_model = assistant("a2", "a1", 3, "mock", "mock-model");
    empty_response_model["message"]["responseModel"] = json!("");
    let ops = parsed(&[
        header(false),
        user("u1", None, 1),
        assistant("a1", "u1", 2, "acme/internal", "in-house"),
        empty_response_model,
    ]);
    let replies = assistants(&ops);
    assert_eq!(replies[0].usage.route.as_deref(), Some("acme"));
    assert_eq!(replies[0].model.as_deref(), Some("in-house"));
    assert_eq!(replies[1].usage.route.as_deref(), Some("mock"));
    assert_eq!(replies[1].model.as_deref(), Some("mock-model"));
}
