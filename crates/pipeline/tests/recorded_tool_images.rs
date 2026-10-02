//! Writer-shaped tool media follows the same pure optimizer as authored media.
//! These synthetic records exercise declared shapes; they are not native captures.
use rosetta_adapter_support::{AgentAdapter, LineStatus, NormalizeCtx};
use rosetta_core::{
    IngestOp,
    timeline::{AttachmentRef, AttachmentSource, TimelineBody},
};
use serde_json::{Value, json};

const PNG: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+ip1sAAAAASUVORK5CYII=";

fn attachments(ops: &[IngestOp]) -> Vec<&AttachmentRef> {
    ops.iter()
        .flat_map(|op| match op {
            IngestOp::ToolUpdate { patch, .. } => patch.attachments.iter(),
            IngestOp::Message(row) => match &row.body {
                TimelineBody::ToolUse { tool } => tool.attachments.iter(),
                _ => [].iter(),
            },
            _ => [].iter(),
        })
        .collect()
}

fn check(adapter: &dyn AgentAdapter, stream: &str, record: Value, expected: usize) {
    let mut ctx = NormalizeCtx::new("runtime", "session");
    ctx.state = adapter.init_state();
    ctx.src_offset = Some(10);
    let result = adapter.ingest_line(&mut ctx, stream, record.to_string().as_bytes());
    assert_eq!(
        result.status,
        LineStatus::Parsed,
        "{}: {:?}",
        adapter.kind(),
        result.ops
    );
    let refs = attachments(&result.ops);
    assert_eq!(refs.len(), expected, "{}: {:?}", adapter.kind(), result.ops);
    for attachment in refs {
        attachment.validate().unwrap();
        let AttachmentSource::Asset { asset } = &attachment.source else {
            panic!("{attachment:?}")
        };
        assert_eq!(asset.media_type, "image/webp");
        assert_eq!(asset.image_variants.as_ref().unwrap().len(), 4);
        assert!(
            result
                .ops
                .iter()
                .any(|op| matches!(op, IngestOp::ProjectionAsset { asset: bytes }
            if bytes.reference() == *asset && &bytes.bytes[8..12] == b"WEBP"))
        );
    }
    assert!(!serde_json::to_string(&result.ops).unwrap().contains(PNG));
    let checkpoint = adapter.checkpoint_state(ctx.state.as_deref()).unwrap();
    assert!(!serde_json::to_string(&checkpoint).unwrap().contains(PNG));
}

#[test]
fn all_established_tool_image_encodings_are_optimized() {
    let url = format!("data:image/png;base64,{PNG}");
    check(
        &rosetta_adapter_codex::CodexAdapter,
        "rollout",
        json!({"timestamp":"2026-09-09T00:00:00Z","type":"response_item","payload":{
            "type":"custom_tool_call_output","call_id":"call","output":[{"type":"input_image","image_url":url}, {"type":"input_image","image_url":url}]
        }}),
        2,
    );
    check(
        &rosetta_adapter_codex::CodexAdapter,
        "rollout",
        json!({"timestamp":"2026-09-09T00:00:00Z","type":"response_item","payload":{
            "type":"image_generation_call","id":"generated","status":"completed","result":PNG
        }}),
        1,
    );
    check(
        &rosetta_adapter_claude_code::ClaudeCodeAdapter,
        "transcript",
        json!({"type":"user","uuid":"result","parentUuid":null,"isSidechain":false,"userType":"external","version":"2.1.241","sessionId":"session","timestamp":"2026-09-09T00:00:00Z","cwd":"/fixture","gitBranch":"main",
            "message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"call","content":[{"type":"image","source":{"type":"base64","media_type":"image/png","data":PNG}}]}]}
        }),
        1,
    );
    check(
        &rosetta_adapter_pi::PiAdapter,
        "session",
        json!({"type":"message","id":"result","parentId":"call","timestamp":"2026-09-09T00:00:00Z",
            "message":{"role":"toolResult","toolCallId":"call","toolName":"read","isError":false,"timestamp":1788912000000_i64,"content":[{"type":"image","data":PNG,"mimeType":"image/png"}]}
        }),
        1,
    );
    check(
        &rosetta_adapter_kimi_code::KimiCodeAdapter,
        "wire",
        json!({"type":"context.append_loop_event","time":1,"event":{"type":"tool.result","toolCallId":"call",
            "result":{"output":[{"type":"image_url","imageUrl":{"url":url}}]}
        }}),
        1,
    );
    check(
        &rosetta_adapter_grok::GrokAdapter,
        "updates",
        json!({"timestamp":1788912000,"method":"session/update","params":{"sessionId":"session",
            "update":{"sessionUpdate":"tool_call_update","toolCallId":"call","status":"completed","content":[{"type":"content","content":{"type":"image","data":PNG,"mimeType":"image/png"}}]}
        }}),
        1,
    );
    check(
        &rosetta_adapter_grok::GrokAdapter,
        "updates",
        json!({"timestamp":1788912000,"method":"session/update","params":{"sessionId":"session",
            "update":{"sessionUpdate":"tool_call_update","toolCallId":"call","status":"completed","rawOutput":{"ImageContent":{"data":PNG,"mime_type":"image/png"}}}
        }}),
        1,
    );
    check(
        &rosetta_adapter_copilot::CopilotAdapter,
        "events",
        json!({"id":"result","timestamp":"2026-09-09T00:00:00Z","type":"tool.execution_complete","data":{"toolCallId":"call","success":true,
            "result":{"content":"","binaryResultsForLlm":[{"type":"image","data":PNG,"mimeType":"image/png"}],"contents":[{"type":"image","data":PNG,"mimeType":"image/png"}]}
        }}),
        2,
    );
    check(
        &rosetta_adapter_opencode::OpenCodeAdapter,
        "opencode-db",
        json!({"query":"timeline","row":{
            "message_id":"assistant","message_data":json!({"role":"assistant","parentID":"user","agent":"build","modelID":"mock","providerID":"mock","mode":"build","path":{"cwd":"/fixture","gitBranch":"main","root":"/fixture"},"time":{"created":1},"cost":0,"tokens":{"input":1,"output":1,"reasoning":0,"cache":{"read":0,"write":0}}}).to_string(),
            "part_id":"tool","part_time_created":1,"part_data":json!({"type":"tool","callID":"call","tool":"read","state":{"status":"completed","input":{"filePath":"/gone.png"},"output":"","title":"image","metadata":{},"time":{"start":1,"end":2},"attachments":[{"type":"file","mime":"image/png","url":url}]}}).to_string(),"first_event_seq":1
        }}),
        1,
    );
    check(
        &rosetta_adapter_opencode::OpenCodeAdapter,
        "opencode-db",
        json!({"query":"timeline-v2","row":{
            "message_id":"assistant","message_type":"assistant","seq":1,"message_time_created":1,"session_time_created":1,"fork_session_id":null,"session_directory":"/fixture",
            "message_data":json!({"type":"assistant","agent":"build","model":{"id":"mock","providerID":"mock"},"time":{"created":1},"content":[{"type":"tool","id":"call","name":"read","time":{"created":1,"completed":2},"state":{"status":"completed","input":{"filePath":"/gone.png"},"content":[{"type":"file","mime":"image/png","uri":url,"name":"result.png"}]}}]}).to_string()
        }}),
        1,
    );
}

#[test]
fn codex_image_batches_keep_the_envelope_owner_across_yield_and_checkpoint_resume() {
    use rosetta_core::{ToolFoldState, timeline::ToolAction};
    let adapter = rosetta_adapter_codex::CodexAdapter;
    let mut ctx = NormalizeCtx::new("runtime", "session");
    ctx.agent_version = Some("0.150.1".into());
    ctx.state = adapter.init_state();
    let image = json!({"type":"input_image","image_url":format!("data:image/png;base64,{PNG}")});
    let records = [
        json!({"type":"custom_tool_call","call_id":"call","name":"exec","input":"await Promise.all([tools.view_image({path: '/gone-a.png'}), tools.view_image({path: '/gone-b.png'})]);"}),
        json!({"type":"custom_tool_call_output","call_id":"call","output":[{"type":"input_text","text":"Script running with cell ID 42"},image]}),
        json!({"type":"function_call","call_id":"wait","name":"wait","arguments":"{\"cell_id\":\"42\"}"}),
        json!({"type":"function_call_output","call_id":"wait","output":[image]}),
    ];
    let mut tool = None;
    let mut fold = ToolFoldState::default();
    for (index, payload) in records.into_iter().enumerate() {
        ctx.src_offset = Some(index as i64 * 100);
        let outcome = adapter.ingest_line(
            &mut ctx,
            "rollout",
            json!({"timestamp":"2026-09-09T00:00:00Z","type":"response_item","payload":payload})
                .to_string()
                .as_bytes(),
        );
        assert_eq!(outcome.status, LineStatus::Parsed);
        for op in outcome.ops {
            match op {
                IngestOp::Message(row) => {
                    let TimelineBody::ToolUse { tool: opened } = row.body else {
                        panic!("tool")
                    };
                    assert!(
                        tool.replace(opened).is_none(),
                        "image and wait records must not invent child rows"
                    );
                }
                IngestOp::ToolUpdate {
                    correlation_key,
                    patch,
                    evidence,
                    ..
                } => {
                    assert_eq!(correlation_key, "call");
                    tool.as_mut()
                        .unwrap()
                        .apply_patch(&mut fold, &patch, &evidence)
                        .unwrap();
                }
                _ => {}
            }
        }
        let checkpoint = adapter.checkpoint_state(ctx.state.as_deref()).unwrap();
        ctx.state = adapter.restore_state(checkpoint.as_ref()).unwrap();
    }
    let tool = tool.unwrap();
    assert!(matches!(tool.action, ToolAction::Orchestration { .. }));
    assert_eq!(tool.attachments.len(), 2);
    assert_ne!(tool.attachments[0].id, tool.attachments[1].id);
    assert_eq!(tool.attachments[0].source, tool.attachments[1].source);
}

/// Local corroboration is opt-in; private transcripts never become test fixtures.
#[test]
#[ignore = "requires a selected local rollout and tool call id"]
fn selected_local_tool_output_recovers_recorded_images() {
    use std::io::{BufRead, BufReader};
    let path = std::env::var("ROSETTA_IMAGE_ROLLOUT").expect("selected rollout");
    let call = std::env::var("ROSETTA_IMAGE_CALL_ID").expect("selected tool call");
    let adapter = rosetta_adapter_codex::CodexAdapter;
    let mut ctx = NormalizeCtx::new("local-check", "local-check");
    ctx.state = adapter.init_state();
    let mut offset = 0;
    let mut recovered = Vec::new();
    for line in BufReader::new(std::fs::File::open(path).unwrap()).lines() {
        let line = line.unwrap();
        let value: Value = serde_json::from_str(&line).unwrap();
        ctx.src_offset = Some(offset);
        offset += line.len() as i64 + 1;
        if value["type"] != "session_meta" && value["payload"]["call_id"] != call {
            continue;
        }
        let outcome = adapter.ingest_line(&mut ctx, "rollout", line.as_bytes());
        assert_eq!(outcome.status, LineStatus::Parsed);
        for attachment in attachments(&outcome.ops) {
            let AttachmentSource::Asset { asset } = &attachment.source else {
                continue;
            };
            assert_eq!(asset.media_type, "image/webp");
            assert_eq!(asset.image_variants.as_ref().unwrap().len(), 4);
            assert!(
                outcome
                    .ops
                    .iter()
                    .any(|op| matches!(op, IngestOp::ProjectionAsset { asset: bytes }
                if bytes.reference() == *asset && &bytes.bytes[8..12] == b"WEBP"))
            );
            recovered.push((asset.byte_len, attachment.dimensions));
        }
        let checkpoint = adapter.checkpoint_state(ctx.state.as_deref()).unwrap();
        ctx.state = adapter.restore_state(checkpoint.as_ref()).unwrap();
    }
    assert!(!recovered.is_empty());
    println!("Recovered optimized images (bytes, dimensions): {recovered:?}");
}
