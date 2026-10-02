//! OpenCode authored file parts belong to their exact relational message.
use crate::raw::{PartData, payload::FilePart};
use rosetta_adapter_support::{
    adapter::NormalizeCtx,
    media::{self, MaterializedAttachment},
};
use rosetta_core::{
    IngestOp,
    rows::MessageRow,
    timeline::{AttachmentKind, AttachmentRef},
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct PromptParts {
    row: MessageRow,
    // Snapshot query order is authoritative. Replacing a part keeps its slot.
    parts: Vec<(String, Option<String>, Option<AttachmentRef>)>,
}

impl PromptParts {
    pub(crate) fn new(row: MessageRow) -> Self {
        Self {
            row,
            parts: Vec::new(),
        }
    }

    pub(crate) fn update(
        &mut self,
        ctx: &NormalizeCtx,
        id: &str,
        part: &PartData,
        ops: &mut Vec<IngestOp>,
    ) {
        let (text, attachment) = match part {
            PartData::Text(text) => (Some(text.text.clone()), None),
            PartData::File(file) => {
                let Some((attachment, asset)) = attachment(id, file) else {
                    return;
                };
                ops.extend(asset.map(|asset| IngestOp::ProjectionAsset { asset }));
                (None, Some(attachment))
            }
            _ => return,
        };
        let value = (id.to_owned(), text, attachment);
        if let Some(slot) = self.parts.iter_mut().find(|(key, ..)| key == id) {
            *slot = value;
        } else {
            self.parts.push(value);
        }
        let mut row = self.row.clone();
        let text = self
            .parts
            .iter()
            .filter_map(|(_, text, _)| text.as_deref())
            .collect::<Vec<_>>()
            .join("\n");
        row.content = (!text.is_empty()).then_some(text);
        row.attachments = self
            .parts
            .iter()
            .filter_map(|(_, _, attachment)| attachment.clone())
            .collect();
        row.src_cursor = ctx.src_cursor;
        row.src_offset = ctx.src_offset;
        IngestOp::push_message(ops, row);
    }
}

pub(crate) fn attachment(id: &str, file: &FilePart) -> Option<MaterializedAttachment> {
    if !file.mime.to_ascii_lowercase().starts_with("image/") {
        return None;
    }
    let (mut attachment, asset) =
        media::from_url(id, AttachmentKind::Image, &file.url, Some(&file.mime));
    attachment.name = file.filename.as_deref().and_then(media::bounded_name);
    Some((attachment, asset))
}

/// Completed v1 tool states own FilePart attachments, not current-file previews.
pub(crate) fn tool_output(
    ctx: &NormalizeCtx,
    key: &str,
    state: &serde_json::Value,
    at: Option<rosetta_core::rows::Ms>,
    ops: &mut Vec<IngestOp>,
) {
    let attachments = state["attachments"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
        .filter_map(|(index, value)| {
            let file: FilePart = serde_json::from_value(value.clone()).ok()?;
            attachment(&format!("image-{index}"), &file)
        });
    media::push_tool_attachments(ctx, key, key, attachments, at, ops);
}

/// 2.x Tool.Content file members carry a URI and MIME, including on errors.
pub(crate) fn tool_output_v2(
    ctx: &NormalizeCtx,
    key: &str,
    state: &serde_json::Value,
    at: Option<rosetta_core::rows::Ms>,
    ops: &mut Vec<IngestOp>,
) {
    let attachments = state["content"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
        .filter(|(_, value)| {
            value["type"] == "file"
                && value["mime"]
                    .as_str()
                    .is_some_and(|mime| mime.to_ascii_lowercase().starts_with("image/"))
        })
        .map(|(index, value)| {
            let id = format!("image-{index}");
            let (mut attachment, asset) = match value["uri"].as_str() {
                Some(uri) => {
                    media::from_url(&id, AttachmentKind::Image, uri, value["mime"].as_str())
                }
                None => media::unavailable(
                    &id,
                    AttachmentKind::Image,
                    rosetta_core::timeline::AttachmentUnavailableReason::InvalidData,
                ),
            };
            attachment.name = value["name"].as_str().and_then(media::bounded_name);
            (attachment, asset)
        });
    media::push_tool_attachments(ctx, key, key, attachments, at, ops);
}
