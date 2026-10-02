//! Pure materialization of already-classified recorded media. Vendor content
//! blocks, labels, ownership and joins belong to the calling adapter.

use base64::{Engine, engine::general_purpose::STANDARD};
use rosetta_core::ProjectionAsset;
use rosetta_core::timeline::{
    AttachmentKind, AttachmentRef, AttachmentSource, AttachmentUnavailableReason as Missing,
    CORE_SCALAR_MAX_BYTES, ID_MAX_BYTES, ResourceRef,
};

pub type MaterializedAttachment = (AttachmentRef, Option<ProjectionAsset>);

/// Attach recorded output occurrences to their proven tool owner. The separate
/// media patch cannot collide with a lifecycle patch from the same native record.
pub fn push_tool_attachments(
    ctx: &crate::adapter::NormalizeCtx,
    correlation_key: &str,
    occurrence: &str,
    media: impl IntoIterator<Item = MaterializedAttachment>,
    at: Option<rosetta_core::rows::Ms>,
    ops: &mut Vec<rosetta_core::IngestOp>,
) {
    let scope = blake3::hash(occurrence.as_bytes()).to_hex().to_string();
    let attachments: Vec<_> = media
        .into_iter()
        .map(|(mut attachment, asset)| {
            attachment.id = format!("{scope}:{}", attachment.id);
            attachment.message_ranges.clear();
            ops.extend(asset.map(|asset| rosetta_core::IngestOp::ProjectionAsset { asset }));
            attachment
        })
        .collect();
    if attachments.is_empty() {
        return;
    }
    rosetta_core::IngestOp::push_tool_update(
        ops,
        ctx.session_id.clone(),
        correlation_key.to_owned(),
        ctx.evidence(u32::MAX - 1),
        rosetta_core::ToolUpdatePatch {
            attachments,
            ..Default::default()
        },
        at,
    );
}

pub fn bounded_name(value: &str) -> Option<String> {
    bounded(value, CORE_SCALAR_MAX_BYTES)
}

pub fn bounded(value: &str, limit: usize) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    let mut end = value.len().min(limit);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    Some(value[..end].to_owned())
}

pub fn unavailable(id: &str, kind: AttachmentKind, reason: Missing) -> MaterializedAttachment {
    (AttachmentRef::unavailable(id, kind, reason), None)
}

pub fn from_base64(
    id: &str,
    kind: AttachmentKind,
    mime: &str,
    data: &str,
) -> MaterializedAttachment {
    let Some(mime) = canonical_media_type(mime)
        .filter(|m| kind != AttachmentKind::Image || m.starts_with("image/"))
    else {
        let mut result = unavailable(id, kind, Missing::UnsupportedMedia);
        result.0.media_type = bounded(mime, ID_MAX_BYTES);
        return result;
    };
    if mime.starts_with("image/") && data.len() > rosetta_media::MAX_INPUT_BYTES.div_ceil(3) * 4 {
        return unavailable(id, kind, Missing::UnsupportedMedia);
    }
    match STANDARD.decode(data) {
        Ok(bytes) => from_bytes(id, kind, mime, bytes),
        Err(_) => unavailable(id, kind, Missing::InvalidData),
    }
}

/// Some native image-generation records store Base64 without a MIME field.
/// Determine the encoding from those recorded bytes, never a filename hint.
pub fn from_image_base64(id: &str, data: &str) -> MaterializedAttachment {
    if data.len() > rosetta_media::MAX_INPUT_BYTES.div_ceil(3) * 4 {
        return unavailable(id, AttachmentKind::Image, Missing::UnsupportedMedia);
    }
    let Ok(bytes) = STANDARD.decode(data) else {
        return unavailable(id, AttachmentKind::Image, Missing::InvalidData);
    };
    let Some(mime) = [
        "image/png",
        "image/jpeg",
        "image/gif",
        "image/webp",
        "image/bmp",
        "image/x-icon",
    ]
    .into_iter()
    .find(|mime| magic_matches(mime, &bytes)) else {
        return unavailable(id, AttachmentKind::Image, Missing::UnsupportedMedia);
    };
    from_bytes(id, AttachmentKind::Image, mime, bytes)
}

pub fn from_bytes(
    id: &str,
    kind: AttachmentKind,
    mime: &str,
    bytes: Vec<u8>,
) -> MaterializedAttachment {
    let Some(mime) = canonical_media_type(mime)
        .filter(|m| kind != AttachmentKind::Image || m.starts_with("image/"))
    else {
        let mut result = unavailable(id, kind, Missing::UnsupportedMedia);
        result.0.media_type = bounded(mime, ID_MAX_BYTES);
        return result;
    };
    if bytes.is_empty() || !magic_matches(mime, &bytes) {
        return unavailable(id, kind, Missing::InvalidData);
    }
    let (asset, dimensions) = if mime.starts_with("image/") {
        match rosetta_media::prepare(&bytes) {
            Ok(prepared) => (prepared.asset.clone(), Some(prepared.dimensions)),
            Err(rosetta_media::MediaError::Unsupported | rosetta_media::MediaError::Limit) => {
                return unavailable(id, kind, Missing::UnsupportedMedia);
            }
            Err(_) => return unavailable(id, kind, Missing::InvalidData),
        }
    } else {
        (
            ProjectionAsset::new(mime, bytes).expect("validated projection media type"),
            None,
        )
    };
    let mime = asset.media_type.as_str();
    let reference = asset.reference();
    let mut attachment = AttachmentRef::new(id, kind, AttachmentSource::Asset { asset: reference });
    attachment.media_type = Some(mime.into());
    attachment.byte_len = Some(asset.bytes.len() as u64);
    attachment.dimensions = dimensions;
    (attachment, Some(asset))
}

/// Data URLs carry recorded bytes. All other locators remain references; this
/// helper performs neither filesystem nor network IO.
pub fn from_url(
    id: &str,
    kind: AttachmentKind,
    url: &str,
    mime: Option<&str>,
) -> MaterializedAttachment {
    if url
        .get(..5)
        .is_some_and(|s| s.eq_ignore_ascii_case("data:"))
    {
        let Some((header, data)) = url[5..].split_once(',') else {
            return unavailable(id, kind, Missing::InvalidData);
        };
        let mut parts = header.split(';');
        let declared = parts.next().unwrap_or("");
        if !parts.any(|p| p.eq_ignore_ascii_case("base64")) {
            return unavailable(id, kind, Missing::InvalidData);
        }
        if mime.is_some_and(|expected| {
            canonical_media_type(expected) != canonical_media_type(declared)
        }) {
            return unavailable(id, kind, Missing::InvalidData);
        }
        return from_base64(id, kind, declared, data);
    }
    if url.is_empty() || url.len() > CORE_SCALAR_MAX_BYTES || url.trim() != url {
        return unavailable(id, kind, Missing::InvalidData);
    }
    let mut attachment = AttachmentRef::new(
        id,
        kind,
        AttachmentSource::Reference {
            reference: ResourceRef::Url {
                url: url.to_owned(),
            },
        },
    );
    attachment.media_type = mime.and_then(|m| bounded(m, ID_MAX_BYTES));
    (attachment, None)
}

pub fn canonical_media_type(mime: &str) -> Option<&'static str> {
    match mime.trim().to_ascii_lowercase().as_str() {
        "image/png" | "image/apng" => Some("image/png"),
        "image/jpeg" | "image/jpg" => Some("image/jpeg"),
        "image/gif" => Some("image/gif"),
        "image/webp" => Some("image/webp"),
        "image/bmp" => Some("image/bmp"),
        "image/x-icon" | "image/vnd.microsoft.icon" => Some("image/x-icon"),
        "application/pdf" => Some("application/pdf"),
        "text/plain" | "text/plain; charset=utf-8" => Some("text/plain; charset=utf-8"),
        _ => None,
    }
}

fn magic_matches(mime: &str, bytes: &[u8]) -> bool {
    match mime {
        "image/png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "image/jpeg" => bytes.starts_with(b"\xff\xd8\xff"),
        "image/gif" => bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
        "image/webp" => bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP",
        "image/bmp" => bytes.starts_with(b"BM"),
        "image/x-icon" => bytes.starts_with(&[0, 0, 1, 0]),
        "application/pdf" => bytes.starts_with(b"%PDF-"),
        "text/plain; charset=utf-8" => std::str::from_utf8(bytes).is_ok(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rosetta_core::timeline::Dimensions;
    const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+ip1sAAAAASUVORK5CYII=";

    #[test]
    fn bytes_deduplicate_but_occurrences_do_not() {
        let (a, bytes) = from_base64("first", AttachmentKind::Image, "image/png", PNG);
        let (b, same) = from_url(
            "second",
            AttachmentKind::Image,
            &format!("data:image/png;base64,{PNG}"),
            None,
        );
        assert_ne!(a.id, b.id);
        assert_eq!(a.source, b.source);
        assert_eq!(bytes, same);
        assert_eq!(
            a.dimensions,
            Some(Dimensions {
                width: 1,
                height: 1
            })
        );
        assert!(!serde_json::to_string(&a).unwrap().contains(PNG));
        a.validate().unwrap();
        b.validate().unwrap();
    }

    #[test]
    fn invalid_and_reference_inputs_preserve_a_slot() {
        let (bad, bytes) = from_base64("first", AttachmentKind::Image, "image/png", "not base64");
        assert!(bytes.is_none());
        assert!(matches!(
            bad.source,
            AttachmentSource::Unavailable {
                reason: Missing::InvalidData
            }
        ));
        let (unsupported, _) = from_url(
            "second",
            AttachmentKind::Image,
            "data:image/svg+xml;base64,PHN2Zy8+",
            None,
        );
        assert!(matches!(
            unsupported.source,
            AttachmentSource::Unavailable {
                reason: Missing::UnsupportedMedia
            }
        ));
        let (reference, _) = from_url(
            "third",
            AttachmentKind::Image,
            "file:///gone/image.png",
            None,
        );
        assert!(matches!(
            reference.source,
            AttachmentSource::Reference { .. }
        ));
        reference.validate().unwrap();
        let (wrong, _) = from_base64("fourth", AttachmentKind::Image, "image/jpeg", PNG);
        assert!(matches!(
            wrong.source,
            AttachmentSource::Unavailable {
                reason: Missing::InvalidData
            }
        ));
    }
}
