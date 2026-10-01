//! ACP writer-derived chunks in source order, including a checkpoint boundary.
use rosetta_adapter_grok::GrokAdapter;
use rosetta_adapter_support::{AgentAdapter, LineStatus, NormalizeCtx};
use rosetta_core::{
    IngestOp,
    timeline::{AttachmentSource, TimelineBody},
};
use serde_json::{Value, json};
fn ingest(ctx: &mut NormalizeCtx, content: Value, hidden: bool) -> Vec<IngestOp> {
    let result=GrokAdapter.ingest_line(ctx,"updates",json!({"timestamp":1788912000,"method":"session/update","params":{
        "sessionId":"native","_meta":{"eventId":"reused-event-counter","timestamp":1788912000},
        "update":{"sessionUpdate":"user_message_chunk","content":content,"_meta":{"promptIndex":1,"modelId":"grok","hideFromScrollback":hidden}}
    }}).to_string().as_bytes());
    assert_eq!(result.status, LineStatus::Parsed);
    result.ops
}
#[test]
fn chunks_share_one_user_message_and_images_do_not_consume_text_slots() {
    let mut ctx = NormalizeCtx::new("runtime", "session");
    ctx.agent_version = Some("1.0.5".into());
    ctx.state = GrokAdapter.init_state();
    ingest(
        &mut ctx,
        json!({"type":"image","data":"broken","mimeType":"image/png"}),
        false,
    );
    ingest(&mut ctx, json!({"type":"text","text":"before"}), false);
    let checkpoint = GrokAdapter.checkpoint_state(ctx.state.as_deref()).unwrap();
    ctx.state = GrokAdapter.restore_state(checkpoint.as_ref()).unwrap();
    let png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+ip1sAAAAASUVORK5CYII=";
    ingest(
        &mut ctx,
        json!({"type":"image","data":png,"mimeType":"image/png"}),
        false,
    );
    ingest(
        &mut ctx,
        json!({"type":"image","data":png,"mimeType":"image/png"}),
        false,
    );
    let ops = ingest(&mut ctx, json!({"type":"text","text":"after"}), false);
    let row = ops
        .iter()
        .find_map(|op| {
            if let IngestOp::Message(row) = op {
                Some(row)
            } else {
                None
            }
        })
        .unwrap();
    row.validate().unwrap();
    let TimelineBody::User { text, attachments } = &row.body else {
        panic!("user")
    };
    assert_eq!(text.as_ref().unwrap().as_preview(), "before\nafter");
    assert_eq!(attachments.len(), 3);
    assert!(matches!(
        attachments[0].source,
        AttachmentSource::Unavailable { .. }
    ));
    assert_eq!(attachments[1].source, attachments[2].source);
    assert_ne!(attachments[1].id, attachments[2].id);
    assert!(
        ingest(
            &mut ctx,
            json!({"type":"image","data":png,"mimeType":"image/png"}),
            true
        )
        .is_empty()
    );
}
