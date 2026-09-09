//! Pure materialization of already-classified authored media. Vendor content
//! blocks, labels, ownership and joins belong to the calling adapter.

use base64::{Engine, engine::general_purpose::STANDARD};
use rosetta_core::ProjectionAsset;
use rosetta_core::timeline::{
    AssetRef, AttachmentKind, AttachmentRef, AttachmentSource,
    AttachmentUnavailableReason as Missing, CORE_SCALAR_MAX_BYTES, Dimensions, ID_MAX_BYTES,
    ResourceRef,
};

pub type MaterializedAttachment = (AttachmentRef, Option<ProjectionAsset>);

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
    match STANDARD.decode(data) {
        Ok(bytes) => from_bytes(id, kind, mime, bytes),
        Err(_) => unavailable(id, kind, Missing::InvalidData),
    }
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
    let dimensions = image_dimensions(mime, &bytes);
    let asset = ProjectionAsset::new(mime, bytes).expect("validated projection media type");
    let reference = AssetRef {
        url: asset.url().expect("validated projection media type"),
        digest: asset.digest.clone(),
        media_type: mime.into(),
        byte_len: asset.bytes.len() as u64,
    };
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
        "image/png" => Some("image/png"),
        "image/jpeg" | "image/jpg" => Some("image/jpeg"),
        "image/gif" => Some("image/gif"),
        "image/webp" => Some("image/webp"),
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
        "application/pdf" => bytes.starts_with(b"%PDF-"),
        "text/plain; charset=utf-8" => std::str::from_utf8(bytes).is_ok(),
        _ => false,
    }
}

fn image_dimensions(mime: &str, b: &[u8]) -> Option<Dimensions> {
    let be32 = |offset| {
        Some(u32::from_be_bytes(
            b.get(offset..offset + 4)?.try_into().ok()?,
        ))
    };
    let le16 = |offset| {
        Some(u32::from(u16::from_le_bytes(
            b.get(offset..offset + 2)?.try_into().ok()?,
        )))
    };
    let (width, height) = match mime {
        "image/png" if b.get(12..16) == Some(b"IHDR") => (be32(16)?, be32(20)?),
        "image/gif" => (le16(6)?, le16(8)?),
        "image/webp" => {
            let tag = b.get(12..16)?;
            if tag == b"VP8X" {
                let le24 = |o| {
                    Some(u32::from_le_bytes([
                        *b.get(o)?,
                        *b.get(o + 1)?,
                        *b.get(o + 2)?,
                        0,
                    ]))
                };
                (le24(24)? + 1, le24(27)? + 1)
            } else if tag == b"VP8L" && b.get(20) == Some(&0x2f) {
                let packed = u32::from_le_bytes(b.get(21..25)?.try_into().ok()?);
                ((packed & 0x3fff) + 1, ((packed >> 14) & 0x3fff) + 1)
            } else if tag == b"VP8 " && b.get(23..26) == Some(&[0x9d, 0x01, 0x2a]) {
                (le16(26)? & 0x3fff, le16(28)? & 0x3fff)
            } else {
                return None;
            }
        }
        "image/jpeg" => jpeg_dimensions(b)?,
        _ => return None,
    };
    (width > 0 && height > 0).then_some(Dimensions { width, height })
}

fn jpeg_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    let b = &bytes[..bytes.len().min(64 * 1024)];
    let mut offset = 2;
    while offset + 4 <= b.len() {
        if b[offset] != 0xff {
            return None;
        }
        while b.get(offset) == Some(&0xff) {
            offset += 1;
        }
        let marker = *b.get(offset)?;
        offset += 1;
        if matches!(marker, 0xd9 | 0xda) {
            return None;
        }
        if matches!(marker, 0x01 | 0xd0..=0xd8) {
            continue;
        }
        let size = usize::from(u16::from_be_bytes(
            b.get(offset..offset + 2)?.try_into().ok()?,
        ));
        if size < 2 {
            return None;
        }
        if matches!(marker, 0xc0..=0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf) && size >= 7 {
            let height = u32::from(u16::from_be_bytes(
                b.get(offset + 3..offset + 5)?.try_into().ok()?,
            ));
            let width = u32::from(u16::from_be_bytes(
                b.get(offset + 5..offset + 7)?.try_into().ok()?,
            ));
            return Some((width, height));
        }
        offset = offset.checked_add(size)?;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aH3sAAAAASUVORK5CYII=";

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

    #[test]
    fn bounded_header_dimensions_cover_the_image_allowlist() {
        assert_eq!(
            image_dimensions("image/gif", b"GIF89a\x20\x00\x10\x00"),
            Some(Dimensions {
                width: 32,
                height: 16
            })
        );
        let jpeg = b"\xff\xd8\xff\xc0\x00\x0b\x08\x00\x10\x00\x20\x01\x01\x11\x00";
        assert_eq!(
            image_dimensions("image/jpeg", jpeg),
            Some(Dimensions {
                width: 32,
                height: 16
            })
        );
        let mut webp =
            b"RIFF\x00\x00\x00\x00WEBPVP8X\x0a\x00\x00\x00\x00\x00\x00\x00\x1f\x00\x00\x0f\x00\x00"
                .to_vec();
        assert_eq!(
            image_dimensions("image/webp", &webp),
            Some(Dimensions {
                width: 32,
                height: 16
            })
        );
        webp.truncate(24);
        assert_eq!(image_dimensions("image/webp", &webp), None);
        assert_eq!(
            image_dimensions("image/jpeg", b"\xff\xd8\xff\xc0\x00"),
            None
        );
        assert_eq!(
            image_dimensions("image/gif", b"GIF89a\x00\x00\x00\x00"),
            None
        );
    }
}
