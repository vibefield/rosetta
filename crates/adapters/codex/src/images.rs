//! Codex ContentItem images and its local-image delimiter triplet.
//! Spec: groundtruth rollout/content-blocks, pinned protocol writer 0.150.1.

use rosetta_adapter_support::media;
use rosetta_core::ProjectionAsset;
use rosetta_core::timeline::{
    AttachmentKind, AttachmentRef, AttachmentUnavailableReason, CITATION_RANGE_LIMIT, TextRange,
};

use crate::raw::rollout::ContentBlock;

/// The output union uses the same ContentItem image blocks as authored prose.
/// Inspect only that declared array, never arbitrary tool text or nested JSON.
pub(crate) fn tool_output(
    ctx: &rosetta_adapter_support::adapter::NormalizeCtx,
    owner: &str,
    output: Option<&serde_json::Value>,
    at: Option<rosetta_core::rows::Ms>,
    ops: &mut Vec<rosetta_core::IngestOp>,
) {
    let Some(blocks) = output.and_then(serde_json::Value::as_array) else {
        return;
    };
    let images = blocks
        .iter()
        .enumerate()
        .filter(|(_, block)| block["type"] == "input_image")
        .map(|(index, block)| {
            let id = format!("image-{index}");
            match block["image_url"].as_str() {
                Some(url) => media::from_url(&id, AttachmentKind::Image, url, None),
                None => media::unavailable(
                    &id,
                    AttachmentKind::Image,
                    AttachmentUnavailableReason::InvalidData,
                ),
            }
        });
    let occurrence = format!(
        "{}:{}",
        ctx.evidence(0).source_identity.0,
        ctx.src_offset.unwrap_or(0)
    );
    media::push_tool_attachments(ctx, owner, &occurrence, images, at, ops);
}

pub(crate) fn authored_content(
    blocks: &[ContentBlock],
) -> (Option<String>, Vec<AttachmentRef>, Vec<ProjectionAsset>) {
    let mut text = Vec::new();
    let mut attachments = Vec::new();
    let mut assets = Vec::new();
    let mut index = 0;
    while index < blocks.len() {
        let block = &blocks[index];
        // These are native block delimiters, not XML. The writer deliberately
        // does not escape quotes or angle brackets in a local path.
        let envelope = opener(block).filter(|_| {
            blocks
                .get(index + 1)
                .is_some_and(|b| b.block_type == "input_image")
                && blocks.get(index + 2).is_some_and(|b| {
                    b.block_type == "input_text" && b.text.as_deref() == Some("</image>")
                })
        });
        let (image, image_index, label, path) = if let Some((label, path)) = envelope {
            let image_index = index + 1;
            index += 3;
            (&blocks[image_index], image_index, label, path)
        } else if block.block_type == "input_image" {
            index += 1;
            (block, index - 1, None, None)
        } else {
            if let Some(value) = block.text.as_deref().filter(|s| !s.is_empty()) {
                text.push(value);
            }
            index += 1;
            continue;
        };
        let id = format!("image-{image_index}");
        let (mut attachment, asset) =
            match image.image_url.as_ref().and_then(serde_json::Value::as_str) {
                Some(url) => media::from_url(&id, AttachmentKind::Image, url, None),
                None => media::unavailable(
                    &id,
                    AttachmentKind::Image,
                    AttachmentUnavailableReason::InvalidData,
                ),
            };
        attachment.label = label.map(str::to_owned);
        attachment.name = path
            .and_then(|p| p.rsplit('/').next())
            .and_then(media::bounded_name);
        attachments.push(attachment);
        assets.extend(asset);
    }
    let text = text.join("\n");
    for index in 0..attachments.len() {
        let Some(label) = attachments[index].label.clone() else {
            continue;
        };
        if attachments
            .iter()
            .filter(|a| a.label.as_deref() == Some(&label))
            .count()
            != 1
        {
            continue;
        }
        attachments[index].message_ranges = text
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
    }
    ((!text.is_empty()).then_some(text), attachments, assets)
}

fn opener(block: &ContentBlock) -> Option<(Option<&str>, Option<&str>)> {
    if block.block_type != "input_text" {
        return None;
    }
    let text = block.text.as_deref()?;
    if text == "<image>" {
        return Some((None, None));
    }
    let (label, path) = text
        .strip_prefix("<image name=")?
        .strip_suffix("\">")?
        .split_once(" path=\"")?;
    let number = label.strip_prefix("[Image #")?.strip_suffix(']')?;
    if number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) || label.len() > 64 {
        return None;
    }
    Some((Some(label), Some(path)))
}
