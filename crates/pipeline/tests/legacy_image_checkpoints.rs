//! Adapter checkpoints written before images became recorded originals hold
//! legacy WebP bundle refs (Copilot's native joins, Grok's open messages,
//! OpenCode's prompt parts). With no normalizer bump those states are resumed
//! as they are: each restores, the rows it re-emits still name the bundle and
//! validate, and images normalized since name recorded originals beside it.
//! The bundle's member files were written by the earlier binary, so a
//! re-emitted legacy ref carries no projection op. Every byte is synthesized.
use rosetta_adapter_support::{AgentAdapter, LineStatus, NormalizeCtx};
use rosetta_core::{
    IngestOp, ProjectionAsset,
    assets::{ImageVariant, ImageVariantBytes},
    timeline::{AssetRef, AttachmentSource, TimelineBody, TimelineEntry},
};
use serde_json::{Value, json};

const PNG: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+ip1sAAAAASUVORK5CYII=";

/// A complete legacy bundle ref, as the earlier binary's normalizer made it.
fn legacy_bundle() -> AssetRef {
    let webp = |tag: &str| format!("RIFF\0\0\0\0WEBPVP8 {tag}").into_bytes();
    let mut bundle = ProjectionAsset::new("image/webp", webp("full")).unwrap();
    bundle.image_variants = ImageVariant::DERIVED
        .into_iter()
        .map(|variant| ImageVariantBytes {
            variant,
            bytes: webp(variant.name()),
        })
        .collect();
    let reference = bundle.reference();
    reference.validate().unwrap();
    reference
}

/// Rewrite every recorded image attachment in a checkpoint into the shape the
/// earlier binary persisted: a WebP bundle ref with WebP metadata.
fn as_written_before_the_switch(value: &mut Value, bundle: &AssetRef) -> usize {
    match value {
        Value::Array(items) => items
            .iter_mut()
            .map(|item| as_written_before_the_switch(item, bundle))
            .sum(),
        Value::Object(object) => {
            let recorded_image = object.get("source").is_some_and(|source| {
                source["type"] == "asset"
                    && source["asset"]["media_type"]
                        .as_str()
                        .is_some_and(|mime| mime.starts_with("image/"))
            });
            if !recorded_image {
                return object
                    .values_mut()
                    .map(|item| as_written_before_the_switch(item, bundle))
                    .sum();
            }
            object["source"]["asset"] = serde_json::to_value(bundle).unwrap();
            object.insert("media_type".into(), json!(bundle.media_type));
            object.insert("byte_len".into(), json!(bundle.byte_len));
            1
        }
        _ => 0,
    }
}

/// Checkpoint, rewrite to the legacy shape, and restore with this binary.
fn resume_from_legacy_checkpoint(adapter: &dyn AgentAdapter, ctx: &mut NormalizeCtx) {
    let mut checkpoint = adapter
        .checkpoint_state(ctx.state.as_deref())
        .unwrap()
        .expect("image state is checkpointed");
    assert!(
        as_written_before_the_switch(&mut checkpoint, &legacy_bundle()) > 0,
        "{}: the checkpoint holds a recorded image",
        adapter.kind()
    );
    ctx.state = adapter.restore_state(Some(&checkpoint)).unwrap();
}

fn ingest(
    adapter: &dyn AgentAdapter,
    ctx: &mut NormalizeCtx,
    stream: &str,
    line: Value,
) -> Vec<IngestOp> {
    ctx.src_offset = Some(ctx.src_offset.unwrap_or(0) + 1);
    let result = adapter.ingest_line(ctx, stream, line.to_string().as_bytes());
    assert_eq!(result.status, LineStatus::Parsed, "{}", adapter.kind());
    result.ops
}

fn last_message(ops: &[IngestOp]) -> &TimelineEntry {
    ops.iter()
        .rev()
        .find_map(|op| match op {
            IngestOp::Message(row) => Some(&**row),
            _ => None,
        })
        .expect("a re-emitted message")
}

/// The re-emitted row validates; returns each attachment's asset ref.
fn image_assets(row: &TimelineEntry) -> Vec<AssetRef> {
    row.validate().unwrap();
    let (TimelineBody::User { attachments, .. } | TimelineBody::Agent { attachments, .. }) =
        &row.body
    else {
        panic!("message with attachments: {row:?}")
    };
    attachments
        .iter()
        .map(|attachment| match &attachment.source {
            AttachmentSource::Asset { asset } => asset.clone(),
            other => panic!("asset: {other:?}"),
        })
        .collect()
}

fn members(ops: &[IngestOp]) -> Vec<&ProjectionAsset> {
    ops.iter()
        .filter_map(|op| match op {
            IngestOp::ProjectionAsset { asset } => Some(asset),
            _ => None,
        })
        .collect()
}

fn assert_recorded_original(asset: &AssetRef, ops: &[IngestOp]) {
    assert_eq!(asset.media_type, "image/png");
    assert_eq!(asset.image_variants, None);
    assert!(
        members(ops)
            .iter()
            .any(|member| member.reference() == *asset && member.image_variants.is_empty()),
        "a new original carries its member op"
    );
}

#[test]
fn copilot_native_joins_resume_their_legacy_bundle_refs() {
    let adapter = rosetta_adapter_copilot::CopilotAdapter;
    let event = |id: &str, kind: &str, data: Value| json!({"id":id,"timestamp":"2026-09-09T00:00:00Z","parentId":null,"type":kind,"data":data});
    let mut ctx = NormalizeCtx::new("runtime", "session");
    ctx.agent_version = Some("1.0.80".into());
    ctx.state = adapter.init_state();
    let binary = |asset_id: &str| {
        event(
            &format!("bytes-{asset_id}"),
            "session.binary_asset",
            json!({"type":"image","assetId":asset_id,"mimeType":"image/png","byteLength":68,"data":PNG}),
        )
    };
    ingest(&adapter, &mut ctx, "events", binary("old"));
    resume_from_legacy_checkpoint(&adapter, &mut ctx);
    let fresh = ingest(&adapter, &mut ctx, "events", binary("new"));
    let ops = ingest(
        &adapter,
        &mut ctx,
        "events",
        event(
            "owner",
            "user.message",
            json!({"content":"Compare these.","attachments":[
                {"type":"image","mimeType":"image/png","assetId":"old","displayName":"before.png"},
                {"type":"image","mimeType":"image/png","assetId":"new","displayName":"after.png"}
            ]}),
        ),
    );
    let assets = image_assets(last_message(&ops));
    assert_eq!(
        assets[0],
        legacy_bundle(),
        "the persisted join is re-emitted"
    );
    assert!(
        members(&ops).is_empty(),
        "joined members were written earlier"
    );
    assert_recorded_original(&assets[1], &fresh);
}

#[test]
fn grok_open_messages_resume_their_legacy_bundle_refs() {
    let adapter = rosetta_adapter_grok::GrokAdapter;
    let chunk = |content: Value| {
        json!({"timestamp":1788912000,"method":"session/update","params":{
            "sessionId":"native","_meta":{"eventId":"event","timestamp":1788912000},
            "update":{"sessionUpdate":"user_message_chunk","content":content,"_meta":{"promptIndex":1,"modelId":"grok","hideFromScrollback":false}}
        }})
    };
    let mut ctx = NormalizeCtx::new("runtime", "session");
    ctx.agent_version = Some("1.0.5".into());
    ctx.state = adapter.init_state();
    let image = json!({"type":"image","data":PNG,"mimeType":"image/png"});
    ingest(&adapter, &mut ctx, "updates", chunk(image.clone()));
    resume_from_legacy_checkpoint(&adapter, &mut ctx);
    let ops = ingest(
        &adapter,
        &mut ctx,
        "updates",
        chunk(json!({"type":"text","text":"compare"})),
    );
    assert_eq!(image_assets(last_message(&ops)), [legacy_bundle()]);
    assert!(members(&ops).is_empty());
    let ops = ingest(&adapter, &mut ctx, "updates", chunk(image));
    let assets = image_assets(last_message(&ops));
    assert_eq!(assets[0], legacy_bundle());
    assert_recorded_original(&assets[1], &ops);
}

#[test]
fn opencode_prompt_parts_resume_their_legacy_bundle_refs() {
    let adapter = rosetta_adapter_opencode::OpenCodeAdapter;
    let part = |id: &str, data: Value| {
        json!({"query":"timeline","row":{
            "message_id":"owner","message_data":json!({"role":"user","agent":"build","model":{"providerID":"mock","modelID":"mock"},"time":{"created":1}}).to_string(),
            "part_id":id,"part_time_created":1,"part_data":data.to_string(),"first_event_seq":1
        }})
    };
    let mut ctx = NormalizeCtx::new("runtime", "session");
    ctx.state = adapter.init_state();
    let image = json!({"type":"file","mime":"image/png","filename":"shot.png","url":format!("data:image/png;base64,{PNG}")});
    ingest(
        &adapter,
        &mut ctx,
        "opencode-db",
        part("before", image.clone()),
    );
    resume_from_legacy_checkpoint(&adapter, &mut ctx);
    let ops = ingest(
        &adapter,
        &mut ctx,
        "opencode-db",
        part("text", json!({"type":"text","text":"compare"})),
    );
    assert_eq!(image_assets(last_message(&ops)), [legacy_bundle()]);
    assert!(members(&ops).is_empty());
    let ops = ingest(&adapter, &mut ctx, "opencode-db", part("after", image));
    let assets = image_assets(last_message(&ops));
    assert_eq!(assets[0], legacy_bundle());
    assert_recorded_original(&assets[1], &ops);
}
