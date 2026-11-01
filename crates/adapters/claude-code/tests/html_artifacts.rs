use rosetta_adapter_claude_code::ClaudeCodeAdapter;
use rosetta_adapter_support::{AgentAdapter, NormalizeCtx};
use rosetta_core::{IngestOp, html_artifacts::HtmlArtifactPublication};
use serde_json::{Value, json};

fn request(action: &str, path: &str) -> Value {
    json!({"type":"assistant","parentUuid":null,"isSidechain":false,"uuid":"opener","timestamp":"2026-10-01T12:00:00Z","sessionId":"native","version":"2.1.283","cwd":"/work","gitBranch":"main","userType":"external",
        "message":{"id":"response","type":"message","role":"assistant","model":"claude-opus-5","stop_reason":"tool_use","content":[{"type":"tool_use","id":"publish-call","name":"Artifact","input":{"action":action,"file_path":path,"title":"A review"}}]}})
}
fn result(failed: bool) -> Value {
    json!({"type":"user","parentUuid":"opener","isSidechain":false,"uuid":"result","timestamp":"2026-10-01T12:00:01Z","sessionId":"native","version":"2.1.283","cwd":"/work","gitBranch":"main","userType":"external",
        "message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"publish-call","content":"published","is_error":failed}]},
        "toolUseResult":{"artifact_id":"native-artifact","version":2,"title":"Published review","url":"https://claude.ai/code/artifact/native-artifact","path":"review.html","updated":true}})
}
fn run(action: &str, path: &str, failed: bool, resume: bool) -> Vec<HtmlArtifactPublication> {
    let mut ctx = NormalizeCtx::new("runtime", "session");
    ctx.state = ClaudeCodeAdapter.init_state();
    ctx.src_offset = Some(10);
    let first = ClaudeCodeAdapter.ingest_line(
        &mut ctx,
        "transcript",
        &serde_json::to_vec(&request(action, path)).unwrap(),
    );
    assert!(
        !matches!(
            first.status,
            rosetta_adapter_support::LineStatus::HardError { .. }
        ),
        "{first:?}"
    );
    assert!(
        !first
            .ops
            .iter()
            .any(|op| matches!(op, IngestOp::HtmlArtifact(_))),
        "a request is not success"
    );
    if resume {
        let saved = ClaudeCodeAdapter
            .checkpoint_state(ctx.state.as_deref())
            .unwrap();
        ctx.state = ClaudeCodeAdapter.restore_state(saved.as_ref()).unwrap();
    }
    ctx.src_offset = Some(100);
    let output = ClaudeCodeAdapter.ingest_line(
        &mut ctx,
        "transcript",
        &serde_json::to_vec(&result(failed)).unwrap(),
    );
    assert!(
        !matches!(
            output.status,
            rosetta_adapter_support::LineStatus::HardError { .. }
        ),
        "{output:?}"
    );
    output
        .ops
        .into_iter()
        .filter_map(|op| {
            if let IngestOp::HtmlArtifact(row) = op {
                Some(row)
            } else {
                None
            }
        })
        .collect()
}
#[test]
fn only_confirmed_html_publish_is_a_library_entry() {
    let rows = run("publish", "review.html", false, true);
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row.path.as_deref(), Some("/work/review.html"));
    assert_eq!(row.filename, "review.html");
    assert_eq!(row.title, "Published review");
    assert_eq!(row.native_id.as_deref(), Some("native-artifact"));
    assert_eq!(row.version.as_deref(), Some("2"));
    assert_eq!(row.source_offset, 100);
    assert_eq!(rows, run("publish", "review.html", false, false));
    assert!(run("publish", "review.html", true, false).is_empty());
    assert!(run("publish", "report.md", false, false).is_empty());
    for action in ["read", "list", "watch", "quickstart"] {
        assert!(
            run(action, "review.html", false, false).is_empty(),
            "{action}"
        );
    }
}

fn ingest(ctx: &mut NormalizeCtx, record: Value) -> Vec<HtmlArtifactPublication> {
    let output =
        ClaudeCodeAdapter.ingest_line(ctx, "transcript", &serde_json::to_vec(&record).unwrap());
    assert!(
        !matches!(
            output.status,
            rosetta_adapter_support::LineStatus::HardError { .. }
        ),
        "{output:?}"
    );
    output
        .ops
        .into_iter()
        .filter_map(|op| match op {
            IngestOp::HtmlArtifact(row) => Some(row),
            _ => None,
        })
        .collect()
}

#[test]
fn a_later_delivery_link_keeps_native_identity_across_checkpoint_without_claiming_a_version() {
    let mut ctx = NormalizeCtx::new("runtime", "session");
    ctx.state = ClaudeCodeAdapter.init_state();
    ctx.src_offset = Some(10);
    assert!(ingest(&mut ctx, request("publish", "review.html")).is_empty());
    ctx.src_offset = Some(100);
    let published = ingest(&mut ctx, result(false)).pop().unwrap();
    let checkpoint = ClaudeCodeAdapter
        .checkpoint_state(ctx.state.as_deref())
        .unwrap();
    ctx.state = ClaudeCodeAdapter
        .restore_state(checkpoint.as_ref())
        .unwrap();
    ctx.src_offset = Some(200);
    let mut delivery = request("publish", "unused.html");
    delivery["uuid"] = json!("delivery");
    delivery["message"]["id"] = json!("delivery-response");
    delivery["message"]["stop_reason"] = json!("end_turn");
    delivery["message"]["content"] =
        json!([{"type":"text","text":"[Review the design](review.html)"}]);
    let delivered = ingest(&mut ctx, delivery).pop().unwrap();
    assert_ne!(published.id, delivered.id);
    assert_eq!(published.artifact_id, delivered.artifact_id);
    assert_eq!(published.native_id, delivered.native_id);
    assert_eq!(published.url, delivered.url);
    assert_eq!(delivered.title, "Review the design");
    assert_eq!(delivered.source_offset, 200);
    assert_eq!(delivered.version, None);

    ctx.src_offset = Some(300);
    let mut attachment = request("publish", "review.html");
    attachment["uuid"] = json!("attachment");
    attachment["message"]["id"] = json!("attachment-response");
    attachment["message"]["content"][0]["id"] = json!("attachment-call");
    attachment["message"]["content"][0]["name"] = json!("SendUserFile");
    assert!(ingest(&mut ctx, attachment).is_empty());
    ctx.src_offset = Some(400);
    let mut reply = result(false);
    reply["uuid"] = json!("attachment-result");
    reply["message"]["content"][0]["tool_use_id"] = json!("attachment-call");
    reply.as_object_mut().unwrap().remove("toolUseResult");
    let attached = ingest(&mut ctx, reply).pop().unwrap();
    assert_eq!(attached.artifact_id, published.artifact_id);
    assert_eq!(attached.url, published.url);
    assert_eq!(attached.version, None);
}

#[test]
fn native_files_map_uses_typed_source_paths_and_send_user_file_requires_success() {
    for (input, expected) in [
        (
            json!({"action":"publish","files":{"index.html":{"from":"review.html","contentType":"text/html"}}}),
            Some("review.html"),
        ),
        (
            json!({"action":"publish","files":{"index.html":"literal-content.html"}}),
            Some("index.html"),
        ),
        (
            json!({"action":"publish","files":{"one.html":{"from":"one.html"},"two.html":{"from":"two.html"}}}),
            None,
        ),
    ] {
        let mut ctx = NormalizeCtx::new("runtime", "session");
        ctx.state = ClaudeCodeAdapter.init_state();
        let mut opener = request("publish", "unused.html");
        opener["message"]["content"][0]["input"] = input;
        assert!(ingest(&mut ctx, opener).is_empty());
        let rows = ingest(&mut ctx, result(false));
        assert_eq!(rows.first().map(|row| row.filename.as_str()), expected);
    }
    for failed in [false, true] {
        let mut ctx = NormalizeCtx::new("runtime", "session");
        ctx.state = ClaudeCodeAdapter.init_state();
        let mut opener = request("publish", "review.html");
        opener["message"]["content"][0]["name"] = json!("SendUserFile");
        assert!(ingest(&mut ctx, opener).is_empty());
        let mut reply = result(failed);
        reply.as_object_mut().unwrap().remove("toolUseResult");
        assert_eq!(ingest(&mut ctx, reply).len(), usize::from(!failed));
    }
}

#[test]
fn inline_html_is_preserved_as_content_after_success_and_checkpoint() {
    for failed in [false, true] {
        let mut ctx = NormalizeCtx::new("runtime", "session");
        ctx.state = ClaudeCodeAdapter.init_state();
        let mut opener = request("publish", "unused.html");
        opener["message"]["content"][0]["input"] =
            json!({"action":"publish","files":{"index.html":"literal-content.html"}});
        assert!(ingest(&mut ctx, opener).is_empty());
        let checkpoint = ClaudeCodeAdapter
            .checkpoint_state(ctx.state.as_deref())
            .unwrap();
        ctx.state = ClaudeCodeAdapter
            .restore_state(checkpoint.as_ref())
            .unwrap();
        let output = ClaudeCodeAdapter.ingest_line(
            &mut ctx,
            "transcript",
            &serde_json::to_vec(&result(failed)).unwrap(),
        );
        let bytes = output.ops.iter().find_map(|op| match op {
            IngestOp::ProjectionAsset { asset } => Some(asset.bytes.as_slice()),
            _ => None,
        });
        if failed {
            assert_eq!(bytes, None);
            continue;
        }
        assert_eq!(
            bytes,
            Some(b"literal-content.html".as_slice()),
            "inline text must never be treated as a filesystem path"
        );
        assert!(output.ops.iter().any(|op| matches!(op,IngestOp::HtmlArtifactContent(row) if row.path.is_none() && row.captured_at.is_none())));
    }
}
