//! After an upgrade one projection holds both image representations: rows
//! normalized by an earlier binary name a legacy WebP bundle whose members it
//! wrote at ingestion, and rows normalized since name the recorded original.
//! Both are valid, both stay referenced, and only the old form is refused as
//! a new projection op. Every byte here is synthesized; no image is decoded.

use rosetta_core::assets::{ImageVariant, ImageVariantBytes};
use rosetta_core::rows::{MessageKind, MessageRow, ProjectRow, RuntimeRow, SessionRow};
use rosetta_core::rows::{SessionRole, StreamState};
use rosetta_core::timeline::{
    AssetRef, AttachmentKind, AttachmentRef, AttachmentSource, Dimensions, TimelineBody,
};
use rosetta_core::{IngestOp, ProjectionAsset};
use rosetta_store::{Batch, Store, StoreError, assets, queries};
use std::path::Path;

fn base() -> Batch {
    Batch {
        ops: vec![
            IngestOp::Runtime(RuntimeRow {
                id: "rt1".into(),
                kind: "claude-code".into(),
                data_root: "/fixture/runtime".into(),
                enabled: true,
                account: None,
                config: None,
                skills: None,
                versions_seen: None,
                discovered_at: 1_000,
                label: None,
                provenance: Default::default(),
            }),
            IngestOp::Project(ProjectRow {
                id: "p1".into(),
                name: "rosetta".into(),
                root_paths: vec!["/w/rosetta".into()],
                git_repo: None,
            }),
            IngestOp::SessionMeta(SessionRow {
                id: "s1".into(),
                native_session_id: "native-s1".into(),
                runtime_id: "rt1".into(),
                project_id: Some("p1".into()),
                cwd: Some("/w/rosetta".into()),
                parent_session_id: None,
                delegation: None,
                root_session_id: "s1".into(),
                role: SessionRole::Main,
                title: None,
                summary: None,
                started_at: 10_000,
                updated_at: 10_000,
                activity: None,
                stream_state: StreamState::Streaming,
            }),
        ],
        ..Batch::default()
    }
}

fn prompt(timeline_id: &str, ordinal: i64, images: &[(&str, &AssetRef)]) -> MessageRow {
    MessageRow {
        timeline_id: timeline_id.into(),
        session_id: "s1".into(),
        ordinal,
        kind: MessageKind::User,
        subtype: None,
        uuid: None,
        parent_uuid: None,
        ts: Some(3_600_000 + ordinal * 1000),
        model: None,
        effort: None,
        content: Some("two screenshots".into()),
        tool: None,
        attachments: images
            .iter()
            .map(|(id, asset)| {
                let mut attachment = AttachmentRef::new(
                    *id,
                    AttachmentKind::Image,
                    AttachmentSource::Asset {
                        asset: (*asset).clone(),
                    },
                );
                attachment.media_type = Some(asset.media_type.clone());
                attachment.byte_len = Some(asset.byte_len);
                attachment.dimensions = Some(Dimensions {
                    width: 4,
                    height: 2,
                });
                attachment
            })
            .collect(),
        citations: Vec::new(),
        group: None,
        response: None,
        block_index: None,
        branch_tool_id: None,
        usage: None,
        agent_version: None,
        src_cursor: None,
        src_offset: None,
    }
}

/// A legacy bundle: a WebP base and four WebP variants, each content
/// addressed. The bytes need only the WebP magic; nothing decodes them.
fn legacy_bundle() -> ProjectionAsset {
    let webp = |tag: &str| format!("RIFF\0\0\0\0WEBPVP8 {tag}").into_bytes();
    let mut bundle = ProjectionAsset::new("image/webp", webp("full")).unwrap();
    bundle.image_variants = ImageVariant::DERIVED
        .into_iter()
        .map(|variant| ImageVariantBytes {
            variant,
            bytes: webp(variant.name()),
        })
        .collect();
    bundle
}

/// Write the bundle's members as the earlier binary's store did; a projection
/// op can no longer carry them.
fn place_legacy_members(projection: &Path, bundle: &ProjectionAsset) -> Vec<String> {
    let root = assets::asset_root(projection);
    std::fs::create_dir_all(&root).unwrap();
    let mut names = vec![bundle.file_name().unwrap()];
    std::fs::write(root.join(&names[0]), &bundle.bytes).unwrap();
    for variant in &bundle.image_variants {
        let name = format!("{}.webp", blake3::hash(&variant.bytes).to_hex());
        std::fs::write(root.join(&name), &variant.bytes).unwrap();
        names.push(name);
    }
    names
}

fn member_names(projection: &Path) -> Vec<String> {
    let mut names: Vec<_> = std::fs::read_dir(assets::asset_root(projection))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

#[test]
fn legacy_bundle_refs_and_recorded_originals_validate_and_stay_referenced_together() {
    let directory = tempfile::tempdir().unwrap();
    let projection = directory.path().join("rosetta.sqlite");
    let mut store = Store::open(&projection).unwrap();
    store.apply(&base()).unwrap();

    let bundle = legacy_bundle();
    let mut expected = place_legacy_members(&projection, &bundle);
    let legacy = bundle.reference();
    assert_eq!(legacy.image_variants.as_ref().map(|v| v.len()), Some(4));

    let original =
        ProjectionAsset::new("image/png", b"\x89PNG\r\n\x1a\nrecorded pixels".to_vec()).unwrap();
    let recorded = original.reference();
    assert_eq!(recorded.image_variants, None);
    expected.push(original.file_name().unwrap());
    expected.sort();

    // An earlier row names only the bundle; a newer one names both forms.
    let mut ops = vec![IngestOp::ProjectionAsset {
        asset: original.clone(),
    }];
    ops.extend(IngestOp::project_message(prompt(
        "earlier",
        0,
        &[("legacy", &legacy)],
    )));
    ops.extend(IngestOp::project_message(prompt(
        "mixed",
        1,
        &[("legacy", &legacy), ("recorded", &recorded)],
    )));
    let stats = store
        .apply(&Batch {
            ops,
            ..Batch::default()
        })
        .unwrap();
    assert_eq!(stats.projection_assets, 1);
    assert_eq!(stats.asset_files_written, 1, "one file per recorded image");

    let page = queries::timeline_page(store.conn(), "s1", None, 10).unwrap();
    let sources: Vec<Vec<AssetRef>> = page
        .iter()
        .map(|entry| {
            let TimelineBody::User { attachments, .. } = &entry.body else {
                panic!("user prompt: {entry:?}")
            };
            attachments
                .iter()
                .map(|attachment| {
                    attachment.validate().unwrap();
                    let AttachmentSource::Asset { asset } = &attachment.source else {
                        panic!("asset: {attachment:?}")
                    };
                    asset.clone()
                })
                .collect()
        })
        .collect();
    assert_eq!(
        sources,
        vec![vec![legacy.clone()], vec![legacy.clone(), recorded.clone()]]
    );

    assert_eq!(member_names(&projection), expected);
    assert_eq!(
        assets::prune_unreferenced_images(store.conn(), &projection).unwrap(),
        0,
        "legacy bundle members and recorded originals are both referenced"
    );
    assert_eq!(member_names(&projection), expected);

    // The old form is readable but never written again.
    let error = store
        .apply(&Batch {
            ops: vec![IngestOp::ProjectionAsset { asset: bundle }],
            ..Batch::default()
        })
        .unwrap_err();
    assert!(matches!(error, StoreError::AssetInvalid { .. }), "{error}");
    assert_eq!(member_names(&projection), expected);
}
