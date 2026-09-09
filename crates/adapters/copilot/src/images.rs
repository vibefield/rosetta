//! Exact session-scoped binary_asset joins. No filesystem or URL recovery.
use crate::raw::events::BinaryAsset;
use rosetta_adapter_support::{adapter::NormalizeCtx, media};
use rosetta_core::{
    IngestOp,
    rows::MessageRow,
    timeline::{AttachmentKind, AttachmentRef, AttachmentSource, AttachmentUnavailableReason},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct Images {
    assets: BTreeMap<String, AttachmentRef>,
    owners: BTreeMap<String, Owner>,
}
#[derive(Debug, Serialize, Deserialize)]
struct Owner {
    row: MessageRow,
    links: Vec<Link>,
}
#[derive(Debug, Serialize, Deserialize)]
struct Link {
    slot: usize,
    asset_id: String,
    mime: String,
    byte_len: Option<u64>,
}

impl Images {
    pub(crate) fn user(
        &mut self,
        mut row: MessageRow,
        attachments: &[Value],
        ops: &mut Vec<IngestOp>,
    ) {
        let mut links = Vec::new();
        for (index, value) in attachments.iter().enumerate() {
            let Some(mime) = value["mimeType"]
                .as_str()
                .filter(|mime| mime.to_ascii_lowercase().starts_with("image/"))
            else {
                continue;
            };
            let id = format!("attachment-{index}");
            let declared_len = value["byteLength"].as_u64();
            let (mut attachment, mut asset) = if value["omittedReason"].is_string() {
                media::unavailable(
                    &id,
                    AttachmentKind::Image,
                    AttachmentUnavailableReason::Omitted,
                )
            } else if let Some(data) = value["data"].as_str() {
                media::from_base64(&id, AttachmentKind::Image, mime, data)
            } else if let Some(asset_id) = value["assetId"].as_str().filter(|id| !id.is_empty()) {
                links.push(Link {
                    slot: row.attachments.len(),
                    asset_id: asset_id.to_owned(),
                    mime: mime.to_owned(),
                    byte_len: declared_len,
                });
                (
                    self.assets.get(asset_id).cloned().unwrap_or_else(|| {
                        AttachmentRef::unavailable(
                            &id,
                            AttachmentKind::Image,
                            AttachmentUnavailableReason::NotRecorded,
                        )
                    }),
                    None,
                )
            } else if let Some(path) = value["path"].as_str().filter(|path| {
                !path.is_empty() && path.len() <= rosetta_core::timeline::CORE_SCALAR_MAX_BYTES
            }) {
                (
                    AttachmentRef::new(
                        &id,
                        AttachmentKind::Image,
                        AttachmentSource::Reference {
                            reference: rosetta_core::timeline::ResourceRef::File {
                                path: path.to_owned(),
                                resolved: false,
                            },
                        },
                    ),
                    None,
                )
            } else {
                media::unavailable(
                    &id,
                    AttachmentKind::Image,
                    AttachmentUnavailableReason::NotRecorded,
                )
            };
            attachment.id = id;
            check_metadata(&mut attachment, mime, declared_len);
            if !matches!(attachment.source, AttachmentSource::Asset { .. }) {
                asset = None;
            }
            attachment.name = value["displayName"].as_str().and_then(media::bounded_name);
            ops.extend(asset.map(|asset| IngestOp::ProjectionAsset { asset }));
            row.attachments.push(attachment);
        }
        IngestOp::push_message(ops, row.clone());
        if !links.is_empty() {
            row.src_cursor = None;
            self.owners
                .insert(row.timeline_id.clone(), Owner { row, links });
        }
    }

    pub(crate) fn binary(
        &mut self,
        ctx: &NormalizeCtx,
        source: &BinaryAsset,
        ops: &mut Vec<IngestOp>,
    ) {
        // The declared MIME is the image discriminator; binary_asset also stores documents.
        if source.asset_id.is_empty() {
            return;
        }
        let is_image = source.mime_type.to_ascii_lowercase().starts_with("image/");
        if !is_image
            && !self.assets.contains_key(&source.asset_id)
            && !self.owners.values().any(|owner| {
                owner
                    .links
                    .iter()
                    .any(|link| link.asset_id == source.asset_id)
            })
        {
            return;
        }
        let (mut attachment, mut asset) = if is_image {
            media::from_base64(
                "binary",
                AttachmentKind::Image,
                &source.mime_type,
                &source.data,
            )
        } else {
            media::unavailable(
                "binary",
                AttachmentKind::Image,
                AttachmentUnavailableReason::InvalidData,
            )
        };
        check_metadata(&mut attachment, &source.mime_type, Some(source.byte_length));
        if let Some(previous) = self.assets.get(&source.asset_id)
            && previous.source != attachment.source
        {
            attachment = AttachmentRef::unavailable(
                "binary",
                AttachmentKind::Image,
                AttachmentUnavailableReason::InvalidData,
            );
        }
        if !matches!(attachment.source, AttachmentSource::Asset { .. }) {
            asset = None;
        }
        ops.extend(asset.map(|asset| IngestOp::ProjectionAsset { asset }));
        self.assets
            .insert(source.asset_id.clone(), attachment.clone());
        for owner in self.owners.values_mut() {
            let mut changed = false;
            for link in owner
                .links
                .iter()
                .filter(|link| link.asset_id == source.asset_id)
            {
                let old = &owner.row.attachments[link.slot];
                let mut resolved = attachment.clone();
                resolved.id.clone_from(&old.id);
                resolved.name.clone_from(&old.name);
                check_metadata(&mut resolved, &link.mime, link.byte_len);
                changed |= *old != resolved;
                owner.row.attachments[link.slot] = resolved;
            }
            if changed {
                let mut row = owner.row.clone();
                row.src_cursor = ctx.src_cursor;
                row.src_offset = ctx.src_offset;
                IngestOp::push_message(ops, row);
            }
        }
    }
}

fn check_metadata(attachment: &mut AttachmentRef, mime: &str, len: Option<u64>) {
    if let AttachmentSource::Asset { asset } = &attachment.source
        && (media::canonical_media_type(mime).is_none_or(|mime| mime != asset.media_type)
            || len.is_some_and(|len| len != asset.byte_len))
    {
        *attachment = AttachmentRef::unavailable(
            &attachment.id,
            AttachmentKind::Image,
            AttachmentUnavailableReason::InvalidData,
        );
    }
    if attachment.media_type.is_none() {
        attachment.media_type = media::bounded(mime, 512);
    }
}
