//! Product-policy tests over synthetic records using the existing source shape.
//! They assert which sessions Rosetta admits, not new native schema facts.

use rosetta_adapter_codex::CodexAdapter;
use rosetta_adapter_support::{AgentAdapter, FileStat, LineStatus, NormalizeCtx};
use serde_json::{Value, json};

fn header(source: Value, thread_source: Option<&str>) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "timestamp": "2026-09-09T12:00:00Z", "type": "session_meta",
        "payload": {
            "id": "child", "timestamp": "2026-09-09T12:00:00Z",
            "cwd": "/fixture", "originator": "codex_exec", "cli_version": "0.150.1",
            "source": source, "thread_source": thread_source,
            "model_provider": "openai", "parent_thread_id": "parent"
        }
    }))
    .unwrap()
}

fn request() -> Vec<u8> {
    serde_json::to_vec(&json!({
        "timestamp": "2026-09-09T12:00:01Z", "ordinal": 1, "type": "response_item",
        "payload": {"type": "message", "role": "user", "content": [
            {"type": "input_text", "text": "Please review the guardian implementation"}
        ]}
    }))
    .unwrap()
}

#[test]
fn native_guardian_sources_are_ledger_only_in_both_metadata_surfaces() {
    for (source, thread_source) in [
        (json!({"subagent": {"other": "guardian"}}), Some("subagent")),
        (json!({"subagent": {"other": "guardian"}}), None),
        (json!("cli"), Some("guardian_review")),
    ] {
        let bytes = header(source.clone(), thread_source);
        let stat = FileStat {
            relative_path: "rollout.jsonl".into(),
            size: bytes.len() as u64,
            modified_ms: 0,
        };
        let skim = CodexAdapter.skim("rollout", &stat, &bytes, &[]);
        assert!(skim.ledger_only);
        assert_eq!(skim.session_id.as_deref(), Some("child"));
        let row = json!({"id": "child", "source": if source.is_string() { source.as_str().unwrap().to_owned() } else { source.to_string() }, "thread_source": thread_source});
        assert!(CodexAdapter.snapshot_session("state-db", &row).ledger_only);
    }
}

#[test]
fn guardian_normalization_and_resumed_tails_emit_nothing() {
    let mut ctx = NormalizeCtx::new("runtime", "guardian");
    ctx.state = CodexAdapter.init_state();
    let opening = header(json!({"subagent": {"other": "guardian"}}), None);
    for line in [
        &opening[..],
        &request()[..],
        b"future or malformed private review payload",
    ] {
        let result = CodexAdapter.ingest_line(&mut ctx, "rollout", line);
        assert_eq!(result.status, LineStatus::Parsed);
        assert!(
            result.ops.is_empty(),
            "no content, assets, usage or drift should escape"
        );
    }
    assert_eq!(ctx.next_ordinal, 0);
    let checkpoint = CodexAdapter.checkpoint_state(ctx.state.as_deref()).unwrap();
    let mut resumed = NormalizeCtx::new("runtime", "guardian");
    resumed.state = CodexAdapter.restore_state(checkpoint.as_ref()).unwrap();
    let result = CodexAdapter.ingest_line(&mut resumed, "rollout", &request());
    assert!(result.ops.is_empty());
    assert_eq!(resumed.next_ordinal, 0);
}

#[test]
fn ordinary_subagents_and_code_reviews_remain_projected() {
    for source in [
        json!("cli"),
        json!({"subagent": {"other": "review"}}),
        json!({"subagent": {"thread_spawn": {"parent_thread_id": "parent", "agent_nickname": "guardian"}}}),
    ] {
        let bytes = header(source.clone(), Some("subagent"));
        let stat = FileStat {
            relative_path: "rollout.jsonl".into(),
            size: bytes.len() as u64,
            modified_ms: 0,
        };
        assert!(!CodexAdapter.skim("rollout", &stat, &bytes, &[]).ledger_only);
        assert!(!CodexAdapter.snapshot_session("state-db", &json!({"id": "child", "source": source.to_string(), "thread_source": "subagent"})).ledger_only);
        let mut ctx = NormalizeCtx::new("runtime", "worker");
        ctx.state = CodexAdapter.init_state();
        CodexAdapter.ingest_line(&mut ctx, "rollout", &bytes);
        assert!(
            !CodexAdapter
                .ingest_line(&mut ctx, "rollout", &request())
                .ops
                .is_empty()
        );
    }
}
