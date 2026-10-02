//! Recorded tool content, with raw ImageContent as its older fallback.
use rosetta_adapter_support::media::{self, MaterializedAttachment};
use rosetta_core::timeline::{AttachmentKind, AttachmentUnavailableReason};
use serde_json::Value;

pub(crate) fn tool_output(
    content: Option<&Value>,
    raw: Option<&Value>,
) -> Vec<MaterializedAttachment> {
    let images: Vec<_> = content
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
        .filter(|(_, block)| block["type"] == "content" && block["content"]["type"] == "image")
        .map(|(index, block)| {
            let image = &block["content"];
            let id = format!("image-{index}");
            match (
                image["data"].as_str(),
                image["mimeType"].as_str(),
                image["uri"].as_str(),
            ) {
                (Some(data), Some(mime), _) => {
                    media::from_base64(&id, AttachmentKind::Image, mime, data)
                }
                (None, mime, Some(url)) => media::from_url(&id, AttachmentKind::Image, url, mime),
                _ => media::unavailable(
                    &id,
                    AttachmentKind::Image,
                    AttachmentUnavailableReason::InvalidData,
                ),
            }
        })
        .collect();
    if !images.is_empty() {
        return images;
    }
    let Some(image) = raw.and_then(|raw| raw.get("ImageContent")) else {
        return images;
    };
    vec![
        match (image["data"].as_str(), image["mime_type"].as_str()) {
            (Some(data), Some(mime)) => {
                media::from_base64("image-0", AttachmentKind::Image, mime, data)
            }
            _ => media::unavailable(
                "image-0",
                AttachmentKind::Image,
                AttachmentUnavailableReason::InvalidData,
            ),
        },
    ]
}
