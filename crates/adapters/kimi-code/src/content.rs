//! Writer-declared content parts (`text | think | image_url | audio_url |
//! video_url`, `kosong/contract/message.ts` ContentPart) as they appear in
//! prompts, appended context messages, streamed assistant parts, and
//! array-valued tool results (`ExecutableToolOutput` is `string |
//! ContentPart[]`). The declared vocabulary comes from the canonical ground
//! truth; an undeclared part is soft drift, a declared part missing its body
//! is a hard error.

use rosetta_adapter_support::{LineStatus, NormalizeCtx};
use rosetta_core::IngestOp;
use rosetta_core::rows::Ms;
use rosetta_core::timeline::{AttachmentKind, AttachmentRef};
use serde_json::Value;

use crate::normalize::{UNKNOWN_SUBTYPE, drift, hard_into};
use crate::raw::wire::{ContentBlock, ToolResultBody};
use crate::spec;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ContentPartError {
    Undeclared(String),
    UnsupportedDeclared(String),
    Malformed(&'static str),
}

pub(crate) fn content_part_failure(
    ctx: &NormalizeCtx,
    stream_id: &str,
    record_kind: &str,
    error: ContentPartError,
    at: Option<Ms>,
    ops: &mut Vec<IngestOp>,
) -> LineStatus {
    match error {
        ContentPartError::Undeclared(kind) => {
            ops.push(drift(
                ctx,
                stream_id,
                &format!("{record_kind}/{kind}"),
                UNKNOWN_SUBTYPE,
                &[],
                at,
            ));
            LineStatus::UnknownType
        }
        ContentPartError::UnsupportedDeclared(kind) => hard_into(
            ctx,
            stream_id,
            &format!("{record_kind}/{kind}"),
            "unsupported_declared_content_part",
            at,
            ops,
        ),
        ContentPartError::Malformed(code) => hard_into(ctx, stream_id, record_kind, code, at, ops),
    }
}

pub(crate) fn validate_content_part(block: &ContentBlock) -> Result<(), ContentPartError> {
    if !spec::declared_content_part_types().contains(&block.block_type) {
        return Err(ContentPartError::Undeclared(block.block_type.clone()));
    }
    match block.block_type.as_str() {
        "text" if block.text.is_none() => Err(ContentPartError::Malformed("missing_text")),
        "think" if block.think.is_none() => Err(ContentPartError::Malformed("missing_think")),
        "image_url" if block.image_url.is_none() => {
            Err(ContentPartError::Malformed("missing_image_url"))
        }
        "audio_url" if block.audio_url.is_none() => {
            Err(ContentPartError::Malformed("missing_audio_url"))
        }
        "video_url" if block.video_url.is_none() => {
            Err(ContentPartError::Malformed("missing_video_url"))
        }
        "text" | "think" | "image_url" | "audio_url" | "video_url" => Ok(()),
        other => Err(ContentPartError::UnsupportedDeclared(other.to_owned())),
    }
}

pub(crate) fn content_text(blocks: &[ContentBlock]) -> Result<Option<String>, ContentPartError> {
    let mut text = Vec::with_capacity(blocks.len());
    for block in blocks {
        validate_content_part(block)?;
        if block.block_type == "text" {
            text.push(block.text.as_deref().expect("validated text part"));
        }
    }
    let joined = text.join("\n");
    Ok((!joined.is_empty()).then_some(joined))
}

/// Only authored content reaches this helper. URL references never trigger IO;
/// a data URL materializes only the bytes actually recorded in the ledger.
pub(crate) fn content_attachments(
    blocks: &[ContentBlock],
) -> (Vec<AttachmentRef>, Vec<rosetta_core::ProjectionAsset>) {
    use rosetta_adapter_support::media;
    let mut attachments = Vec::new();
    let mut assets = Vec::new();
    for (index, block) in blocks.iter().enumerate() {
        let (kind, source) = match block.block_type.as_str() {
            "image_url" => (AttachmentKind::Image, block.image_url.as_ref()),
            "audio_url" => (AttachmentKind::Audio, block.audio_url.as_ref()),
            "video_url" => (AttachmentKind::Video, block.video_url.as_ref()),
            _ => continue,
        };
        let id = format!("media-{index}");
        let (mut attachment, asset) = match source {
            Some(source) => media::from_url(&id, kind, &source.url, None),
            None => media::unavailable(
                &id,
                kind,
                rosetta_core::timeline::AttachmentUnavailableReason::InvalidData,
            ),
        };
        attachment.name = source
            .and_then(|source| source.id.as_deref())
            .and_then(media::bounded_name);
        attachments.push(attachment);
        assets.extend(asset);
    }
    (attachments, assets)
}

pub(crate) fn result_text(result: &ToolResultBody) -> Result<Option<String>, ContentPartError> {
    match &result.output {
        Value::String(text) => Ok(Some(text.clone())),
        Value::Array(blocks) => {
            let blocks = serde_json::from_value::<Vec<ContentBlock>>(Value::Array(blocks.clone()))
                .map_err(|_| ContentPartError::Malformed("invalid_result_blocks"))?;
            content_text(&blocks)
        }
        _ => Err(ContentPartError::Malformed("unsupported_result_output")),
    }
}

#[cfg(test)]
mod tests {
    use super::result_text;
    use crate::raw::wire::ToolResultBody;
    use rosetta_adapter_support::Unknown;
    use serde_json::json;

    #[test]
    fn result_output_preserves_string_and_text_blocks() {
        let body = ToolResultBody {
            output: json!("plain"),
            is_error: None,
            note: None,
            unknown: Unknown::new(),
        };
        assert_eq!(result_text(&body), Ok(Some("plain".to_owned())));

        let body = ToolResultBody {
            output: json!([
                {"type": "text", "text": "first"},
                {"type": "text", "text": "second"}
            ]),
            is_error: Some(false),
            note: None,
            unknown: Unknown::new(),
        };
        assert_eq!(result_text(&body), Ok(Some("first\nsecond".to_owned())));
    }
}
