//! Claude authored source variants; generic decoding lives in adapter-support.

use rosetta_adapter_support::media::{self, MaterializedAttachment};
use rosetta_core::timeline::{
    AttachmentKind, AttachmentRef, AttachmentUnavailableReason, CITATION_RANGE_LIMIT, TextRange,
};
use serde::Deserialize;
use serde_json::Value;

pub(crate) fn tool_attachments(
    content: Option<&Value>,
    native: Option<&Value>,
) -> Vec<MaterializedAttachment> {
    let images: Vec<_> = content
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
        .filter(|(_, block)| block["type"] == "image")
        .map(|(index, block)| {
            authored_attachment(
                &format!("image-{index}"),
                AttachmentKind::Image,
                &block["source"],
            )
        })
        .collect();
    if !images.is_empty() {
        return images;
    }
    // Older Read results retain the image in the typed native file result.
    // The nested tool-result content is its model-facing representation when present.
    let Some(file) = native.and_then(|native| native.get("file")) else {
        return images;
    };
    let Some(data) = file["base64"].as_str() else {
        return images;
    };
    vec![match file["type"].as_str() {
        Some(mime) => media::from_base64("image-0", AttachmentKind::Image, mime, data),
        None => media::unavailable(
            "image-0",
            AttachmentKind::Image,
            AttachmentUnavailableReason::InvalidData,
        ),
    }]
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AuthoredSource {
    Base64 {
        media_type: String,
        data: String,
    },
    Url {
        url: String,
    },
    Text {
        data: String,
        media_type: Option<String>,
    },
}

pub(crate) fn authored_attachment(
    id: &str,
    kind: AttachmentKind,
    source: &Value,
) -> MaterializedAttachment {
    match serde_json::from_value::<AuthoredSource>(source.clone()) {
        Ok(AuthoredSource::Url { url }) => media::from_url(id, kind, &url, None),
        Ok(AuthoredSource::Base64 { media_type, data }) => {
            media::from_base64(id, kind, &media_type, &data)
        }
        Ok(AuthoredSource::Text { data, media_type })
            if kind == AttachmentKind::Document
                && media_type.as_deref().is_none_or(|mime| {
                    matches!(mime, "text/plain" | "text/plain; charset=utf-8")
                }) =>
        {
            media::from_bytes(
                id,
                kind,
                media_type.as_deref().unwrap_or("text/plain; charset=utf-8"),
                data.into_bytes(),
            )
        }
        _ => media::unavailable(id, kind, AttachmentUnavailableReason::InvalidData),
    }
}

/// The native paste IDs are parallel to image blocks, not all content blocks.
/// Ground truth: transcript / image-paste-bindings (Claude Code 2.1.267 writer).
pub(crate) fn bind_paste_images(
    text: &str,
    paste_ids: Option<&Value>,
    attachments: &mut [AttachmentRef],
) {
    let Some(raw_ids) = paste_ids.and_then(Value::as_array) else {
        return;
    };
    let image_count = attachments
        .iter()
        .filter(|attachment| attachment.kind == AttachmentKind::Image)
        .count();
    if raw_ids.len() != image_count {
        return;
    }
    // Missing, malformed or duplicate IDs cannot establish an exact binding.
    let Some(ids) = raw_ids
        .iter()
        .map(|id| {
            id.as_u64()
                .filter(|id| *id > 0 && *id <= 9_007_199_254_740_991)
        })
        .collect::<Option<Vec<_>>>()
    else {
        return;
    };
    if ids.iter().collect::<std::collections::HashSet<_>>().len() != ids.len() {
        return;
    }
    for (attachment, id) in attachments
        .iter_mut()
        .filter(|attachment| attachment.kind == AttachmentKind::Image)
        .zip(ids)
    {
        let label = format!("[Image #{id}]");
        attachment.message_ranges = text
            .match_indices(&label)
            .take(CITATION_RANGE_LIMIT)
            .filter_map(|(start, matched)| {
                Some(TextRange {
                    start: text[..start].chars().count().try_into().ok()?,
                    end: text[..start + matched.len()]
                        .chars()
                        .count()
                        .try_into()
                        .ok()?,
                })
            })
            .collect();
        attachment.label = Some(label);
    }
}
