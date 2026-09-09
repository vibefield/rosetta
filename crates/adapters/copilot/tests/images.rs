//! Writer-schema 1.0.80 conformance; synthetic bytes, no private user history.
use rosetta_adapter_copilot::CopilotAdapter;
use rosetta_adapter_support::{AgentAdapter, LineStatus, NormalizeCtx};
use rosetta_core::{
    IngestOp,
    timeline::{AttachmentSource, TimelineBody, TimelineEntry},
};
use serde_json::{Value, json};
const PNG: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aC9sAAAAASUVORK5CYII=";
fn ctx() -> NormalizeCtx {
    let mut ctx = NormalizeCtx::new("runtime", "session");
    ctx.agent_version = Some("1.0.80".into());
    ctx.state = CopilotAdapter.init_state();
    ctx
}
fn ingest(ctx: &mut NormalizeCtx, id: &str, kind: &str, data: Value) -> Vec<IngestOp> {
    let result = CopilotAdapter.ingest_line(
        ctx,
        "events",
        json!({"id":id,"timestamp":"2026-09-09T00:00:00Z","parentId":null,"type":kind,"data":data})
            .to_string()
            .as_bytes(),
    );
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
fn binary() -> Value {
    json!({"type":"image","assetId":"a","mimeType":"image/png","byteLength":68,"data":PNG})
}
fn user() -> Value {
    json!({"content":"Compare these.","attachments":[
        {"type":"image","mimeType":"image/png","omittedReason":"too-large"},
        {"type":"image","mimeType":"image/png","assetId":"a","displayName":"one.png"},
        {"type":"image","mimeType":"image/png","assetId":"a","displayName":"two.png"}
    ]})
}
#[test]
fn exact_assets_resolve_before_or_after_the_owner_and_survive_checkpoint_resume() {
    let mut later = ctx();
    let first = ingest(&mut later, "owner", "user.message", user());
    let first = message(&first);
    let TimelineBody::User { attachments, .. } = &first.body else {
        panic!("user")
    };
    assert_eq!(attachments.len(), 3);
    assert!(
        attachments
            .iter()
            .all(|a| matches!(a.source, AttachmentSource::Unavailable { .. }))
    );
    let checkpoint = CopilotAdapter
        .checkpoint_state(later.state.as_deref())
        .unwrap();
    assert!(!checkpoint.as_ref().unwrap().to_string().contains(PNG));
    let mut resumed = ctx();
    resumed.next_ordinal = later.next_ordinal;
    resumed.state = CopilotAdapter.restore_state(checkpoint.as_ref()).unwrap();
    let resolved = ingest(&mut resumed, "bytes", "session.binary_asset", binary());
    let row = message(&resolved);
    row.validate().unwrap();
    assert_eq!(first.timeline_id, row.timeline_id);
    assert_eq!(first.ordinal, row.ordinal);
    let TimelineBody::User { attachments, .. } = &row.body else {
        panic!("user")
    };
    assert!(matches!(
        attachments[0].source,
        AttachmentSource::Unavailable { .. }
    ));
    assert!(matches!(
        attachments[1].source,
        AttachmentSource::Asset { .. }
    ));
    assert_eq!(attachments[1].source, attachments[2].source);
    assert_ne!(attachments[1].id, attachments[2].id);
    assert_eq!(attachments[1].name.as_deref(), Some("one.png"));
    let mut before = ctx();
    let bytes = ingest(&mut before, "bytes", "session.binary_asset", binary());
    assert!(!bytes.iter().any(|op| matches!(op, IngestOp::Message(_))));
    let earlier = ingest(&mut before, "owner", "user.message", user());
    assert_eq!(message(&earlier).body, row.body);
    let isolated = ingest(&mut ctx(), "owner", "user.message", user());
    assert_eq!(message(&isolated).body, first.body);
}
#[test]
fn incorrect_byte_length_is_unavailable_and_never_materializes_a_member() {
    let mut context = ctx();
    let mut data = binary();
    data["byteLength"] = json!(999);
    let ops = ingest(&mut context, "bytes", "session.binary_asset", data);
    assert!(
        !ops.iter()
            .any(|op| matches!(op, IngestOp::ProjectionAsset { .. }))
    );
    let ops = ingest(&mut context, "owner", "user.message", user());
    let TimelineBody::User { attachments, .. } = &message(&ops).body else {
        panic!("user")
    };
    assert!(
        attachments
            .iter()
            .all(|a| matches!(a.source, AttachmentSource::Unavailable { .. }))
    );
}
