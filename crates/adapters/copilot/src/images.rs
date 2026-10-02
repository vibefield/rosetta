//! Exact session-scoped binary_asset joins. No filesystem or URL recovery.
use crate::raw::events::BinaryAsset;
use base64::{Engine, engine::general_purpose::STANDARD};
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
    assets: BTreeMap<String, Recorded>,
    owners: BTreeMap<String, Owner>,
    #[serde(default)]
    tool_owners: BTreeMap<String, ToolOwner>,
}
// Source metadata belongs to the exact native join, never to the encoded
// AssetRef. Only the selected source's bounded proof survives a checkpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Recorded {
    attachment: AttachmentRef,
    mime: String,
    byte_len: u64,
    digest: String,
}

fn recorded(id: &str, mime: &str, data: &str) -> (Recorded, Option<rosetta_core::ProjectionAsset>) {
    let decoded = if data.len() <= 32 * 1024 * 1024 / 3 * 4 + 4 {
        STANDARD.decode(data).ok()
    } else {
        None
    };
    let byte_len = decoded.as_ref().map_or(0, |bytes| bytes.len() as u64);
    let digest = decoded.as_ref().map_or_else(String::new, |bytes| {
        blake3::hash(bytes).to_hex().to_string()
    });
    let (attachment, asset) = if let Some(bytes) = decoded {
        media::from_bytes(id, AttachmentKind::Image, mime, bytes)
    } else {
        media::unavailable(
            id,
            AttachmentKind::Image,
            AttachmentUnavailableReason::InvalidData,
        )
    };
    (
        Recorded {
            attachment,
            mime: media::canonical_media_type(mime).unwrap_or("").into(),
            byte_len,
            digest,
        },
        asset,
    )
}

#[derive(Debug, Serialize, Deserialize)]
struct Owner {
    row: MessageRow,
    links: Vec<Link>,
}
#[derive(Debug, Serialize, Deserialize)]
struct ToolOwner {
    attachments: Vec<AttachmentRef>,
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
        let (attachments, links) = self.collect(attachments, ops);
        row.attachments = attachments;
        IngestOp::push_message(ops, row.clone());
        if !links.is_empty() {
            row.src_cursor = None;
            self.owners
                .insert(row.timeline_id.clone(), Owner { row, links });
        }
    }

    fn collect(
        &self,
        attachments: &[Value],
        ops: &mut Vec<IngestOp>,
    ) -> (Vec<AttachmentRef>, Vec<Link>) {
        let mut recorded_attachments = Vec::new();
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
                let (source, asset) = recorded(&id, mime, data);
                let mut attachment = source.attachment.clone();
                check_metadata(&mut attachment, &source, mime, declared_len);
                (attachment, asset)
            } else if let Some(asset_id) = value["assetId"].as_str().filter(|id| !id.is_empty()) {
                links.push(Link {
                    slot: recorded_attachments.len(),
                    asset_id: asset_id.to_owned(),
                    mime: mime.to_owned(),
                    byte_len: declared_len,
                });
                (
                    self.assets
                        .get(asset_id)
                        .map(|source| {
                            let mut attachment = source.attachment.clone();
                            check_metadata(&mut attachment, source, mime, declared_len);
                            attachment
                        })
                        .unwrap_or_else(|| {
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

            if !matches!(attachment.source, AttachmentSource::Asset { .. }) {
                asset = None;
            }
            attachment.name = value["displayName"].as_str().and_then(media::bounded_name);
            ops.extend(asset.map(|asset| IngestOp::ProjectionAsset { asset }));
            recorded_attachments.push(attachment);
        }
        (recorded_attachments, links)
    }

    pub(crate) fn tool(
        &mut self,
        ctx: &NormalizeCtx,
        key: &str,
        result: &Value,
        at: Option<rosetta_core::rows::Ms>,
        ops: &mut Vec<IngestOp>,
    ) {
        // Both writer-declared collections can contain recorded media. Keep
        // their occurrences even when their bytes happen to be identical.
        let values: Vec<_> = ["binaryResultsForLlm", "contents"]
            .into_iter()
            .flat_map(|field| result[field].as_array().into_iter().flatten().cloned())
            .collect();
        let (attachments, links) = self.collect(&values, ops);
        media::push_tool_attachments(
            ctx,
            key,
            key,
            attachments.iter().cloned().map(|a| (a, None)),
            at,
            ops,
        );
        if !links.is_empty() {
            self.tool_owners
                .insert(key.to_owned(), ToolOwner { attachments, links });
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
            && !self.tool_owners.values().any(|owner| {
                owner
                    .links
                    .iter()
                    .any(|link| link.asset_id == source.asset_id)
            })
        {
            return;
        }
        let (mut source_image, mut asset) = recorded("binary", &source.mime_type, &source.data);
        let mut attachment = source_image.attachment.clone();
        check_metadata(
            &mut attachment,
            &source_image,
            &source.mime_type,
            Some(source.byte_length),
        );
        if let Some(previous) = self.assets.get(&source.asset_id)
            && (matches!(
                previous.attachment.source,
                AttachmentSource::Unavailable {
                    reason: AttachmentUnavailableReason::InvalidData
                }
            ) || previous.digest != source_image.digest
                || previous.mime != source_image.mime
                || previous.byte_len != source_image.byte_len)
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
        source_image.attachment = attachment.clone();
        self.assets
            .insert(source.asset_id.clone(), source_image.clone());
        for (key, owner) in &mut self.tool_owners {
            let mut changed = false;
            for link in owner
                .links
                .iter()
                .filter(|link| link.asset_id == source.asset_id)
            {
                let old = &owner.attachments[link.slot];
                let mut resolved = attachment.clone();
                resolved.id.clone_from(&old.id);
                resolved.name.clone_from(&old.name);
                check_metadata(&mut resolved, &source_image, &link.mime, link.byte_len);
                changed |= *old != resolved;
                owner.attachments[link.slot] = resolved;
            }
            if changed {
                media::push_tool_attachments(
                    ctx,
                    key,
                    key,
                    owner.attachments.iter().cloned().map(|a| (a, None)),
                    None,
                    ops,
                );
            }
        }
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
                check_metadata(&mut resolved, &source_image, &link.mime, link.byte_len);
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

fn check_metadata(attachment: &mut AttachmentRef, source: &Recorded, mime: &str, len: Option<u64>) {
    if let AttachmentSource::Asset { .. } = &attachment.source
        && (media::canonical_media_type(mime).is_none_or(|mime| mime != source.mime)
            || len.is_some_and(|len| len != source.byte_len))
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
