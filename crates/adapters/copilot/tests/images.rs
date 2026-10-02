//! Writer-schema 1.0.80 conformance; synthetic bytes, no private user history.
use rosetta_adapter_copilot::CopilotAdapter;
use rosetta_adapter_support::{AgentAdapter, LineStatus, NormalizeCtx};
use rosetta_core::{
    IngestOp,
    timeline::{AttachmentSource, TimelineBody, TimelineEntry},
};
use serde_json::{Value, json};
const PNG: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+ip1sAAAAASUVORK5CYII=";
fn ctx() -> NormalizeCtx {
    let mut ctx = NormalizeCtx::new("runtime", "session");
    ctx.agent_version = Some("1.0.80".into());
    ctx.state = CopilotAdapter.init_state();
    ctx
}
fn ingest(ctx: &mut NormalizeCtx, id: &str, kind: &str, data: Value) -> Vec<IngestOp> {
    ctx.src_offset = Some(ctx.src_offset.unwrap_or(0) + 1);
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

#[test]
fn tool_binary_assets_resolve_after_checkpoint_and_correct_the_same_occurrences() {
    use rosetta_core::{ToolFoldState, ToolInteraction};
    let result = json!({"toolCallId":"call", "success":true,"result":{
        "content":"", "binaryResultsForLlm":[
            {"type":"image","assetId":"a","mimeType":"image/png","byteLength":68},
            {"type":"image","assetId":"a","mimeType":"image/png","byteLength":68}
        ]
    }});
    let mut context = ctx();
    let mut tool = ToolInteraction::unknown(
        "call",
        rosetta_core::CorrelationDerivation::NativeId,
        "view_image",
    );
    let mut fold = ToolFoldState::default();
    let mut apply = |ops: &[IngestOp]| {
        for op in ops {
            if let IngestOp::ToolUpdate {
                patch, evidence, ..
            } = op
            {
                tool.apply_patch(&mut fold, patch, evidence).unwrap();
            }
        }
        tool.attachments.clone()
    };
    let first = apply(&ingest(
        &mut context,
        "owner",
        "tool.execution_complete",
        result.clone(),
    ));
    assert_eq!(first.len(), 2);
    assert_ne!(first[0].id, first[1].id);
    assert!(
        first
            .iter()
            .all(|a| matches!(a.source, AttachmentSource::Unavailable { .. }))
    );
    let checkpoint = CopilotAdapter
        .checkpoint_state(context.state.as_deref())
        .unwrap();
    assert!(!serde_json::to_string(&checkpoint).unwrap().contains(PNG));
    context.state = CopilotAdapter.restore_state(checkpoint.as_ref()).unwrap();
    let resolved = apply(&ingest(
        &mut context,
        "bytes",
        "session.binary_asset",
        binary(),
    ));
    assert_eq!(
        resolved.iter().map(|a| &a.id).collect::<Vec<_>>(),
        first.iter().map(|a| &a.id).collect::<Vec<_>>()
    );
    assert!(resolved.iter().all(|a| matches!(&a.source,AttachmentSource::Asset { asset } if asset.media_type == "image/webp" && asset.image_variants.as_ref().unwrap().len() == 4)));
    // A conflicting original source identity invalidates the already visible slots.
    let mut corrupt = binary();
    corrupt["byteLength"] = json!(99);
    let invalid = apply(&ingest(
        &mut context,
        "conflict",
        "session.binary_asset",
        corrupt,
    ));
    assert!(
        invalid
            .iter()
            .all(|a| matches!(a.source, AttachmentSource::Unavailable { .. }))
    );
    let mut earlier = ctx();
    ingest(&mut earlier, "bytes", "session.binary_asset", binary());
    let before = ingest(&mut earlier, "owner", "tool.execution_complete", result);
    let before = before
        .iter()
        .find_map(|op| match op {
            IngestOp::ToolUpdate { patch, .. } if !patch.attachments.is_empty() => {
                Some(&patch.attachments)
            }
            _ => None,
        })
        .unwrap();
    assert_eq!(&resolved, before);
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

#[test]
fn conflicting_native_assets_stay_invalid_even_when_their_encoded_pixels_match() {
    use base64::{Engine, engine::general_purpose::STANDARD};
    let mut context = ctx();
    ingest(&mut context, "bytes", "session.binary_asset", binary());
    // The decoder ignores trailing bytes, but native asset identity must not.
    let mut bytes = STANDARD.decode(PNG).unwrap();
    bytes.extend_from_slice(b"different native representation");
    let mut other = binary();
    other["data"] = json!(STANDARD.encode(&bytes));
    other["byteLength"] = json!(bytes.len());
    ingest(
        &mut context,
        "conflict",
        "session.binary_asset",
        other.clone(),
    );
    let checkpoint = CopilotAdapter
        .checkpoint_state(context.state.as_deref())
        .unwrap();
    context.state = CopilotAdapter.restore_state(checkpoint.as_ref()).unwrap();
    ingest(
        &mut context,
        "repeated-conflict",
        "session.binary_asset",
        other,
    );
    let ops = ingest(&mut context, "owner", "user.message", user());
    let TimelineBody::User { attachments, .. } = &message(&ops).body else {
        panic!("user")
    };
    assert!(
        attachments
            .iter()
            .all(|attachment| matches!(attachment.source, AttachmentSource::Unavailable { .. }))
    );
}
