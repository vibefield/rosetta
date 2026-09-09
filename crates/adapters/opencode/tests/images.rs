//! Writer FilePart cases assembled by exact message id in snapshot order.
use rosetta_adapter_opencode::OpenCodeAdapter;
use rosetta_adapter_support::{AgentAdapter, LineStatus, NormalizeCtx};
use rosetta_core::{
    IngestOp,
    timeline::{AttachmentSource, TimelineBody, TimelineEntry},
};
use serde_json::{Value, json};
fn ingest(ctx: &mut NormalizeCtx, message: &str, part: &str, value: Value) -> Vec<IngestOp> {
    let result=OpenCodeAdapter.ingest_line(ctx,"opencode-db",json!({"query":"timeline","row":{
        "message_id":message,"message_data":json!({"role":"user","agent":"build","model":{"providerID":"mock","modelID":"mock"},"time":{"created":1}}).to_string(),
        "part_id":part,"part_time_created":1,"part_data":value.to_string(),"first_event_seq":1
    }}).to_string().as_bytes());
    assert_eq!(result.status, LineStatus::Parsed);
    result.ops
}
fn message(ops: &[IngestOp]) -> &TimelineEntry {
    ops.iter()
        .find_map(|op| {
            if let IngestOp::Message(row) = op {
                Some(&**row)
            } else {
                None
            }
        })
        .unwrap()
}
#[test]
fn images_and_text_replace_the_same_prompt_without_changing_other_messages() {
    let mut ctx = NormalizeCtx::new("runtime", "session");
    ctx.state = OpenCodeAdapter.init_state();
    let first = ingest(
        &mut ctx,
        "owner",
        "text",
        json!({"type":"text","text":"before"}),
    );
    let first = message(&first);
    ingest(
        &mut ctx,
        "owner",
        "missing",
        json!({"type":"file","mime":"image/png","url":"data:image/png;base64,invalid"}),
    );
    ingest(
        &mut ctx,
        "other",
        "other-text",
        json!({"type":"text","text":"different prompt"}),
    );
    let image = json!({"type":"file","mime":"image/png","filename":"test.png","url":"data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aC9sAAAAASUVORK5CYII="});
    ingest(&mut ctx, "owner", "image-one", image.clone());
    ingest(&mut ctx, "owner", "image-two", image);
    let last = ingest(
        &mut ctx,
        "owner",
        "text",
        json!({"type":"text","text":"updated"}),
    );
    let last = message(&last);
    last.validate().unwrap();
    assert_eq!(first.timeline_id, last.timeline_id);
    assert_eq!(first.ordinal, last.ordinal);
    let TimelineBody::User { text, attachments } = &last.body else {
        panic!("user")
    };
    assert_eq!(text.as_ref().unwrap().as_preview(), "updated");
    assert_eq!(attachments.len(), 3);
    assert!(matches!(
        attachments[0].source,
        AttachmentSource::Unavailable { .. }
    ));
    assert_eq!(attachments[1].source, attachments[2].source);
    assert_ne!(attachments[1].id, attachments[2].id);
    let mut fresh = NormalizeCtx::new("runtime", "session");
    fresh.state = OpenCodeAdapter.init_state();
    let replaced = ingest(
        &mut fresh,
        "owner",
        "text",
        json!({"type":"text","text":"updated"}),
    );
    let TimelineBody::User { attachments, .. } = &message(&replaced).body else {
        panic!("user")
    };
    assert!(
        attachments.is_empty(),
        "a fresh snapshot must not keep removed file parts"
    );
}
