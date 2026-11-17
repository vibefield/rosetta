//! Pure materialization of already-classified recorded media. Vendor content
//! blocks, labels, ownership and joins belong to the calling adapter.

use base64::{Engine, engine::general_purpose::STANDARD};
use rosetta_core::ProjectionAsset;
use rosetta_core::presented_media::VIDEO_MAX_BYTES;
use rosetta_core::timeline::{
    AttachmentKind, AttachmentRef, AttachmentSource, AttachmentUnavailableReason as Missing,
    CORE_SCALAR_MAX_BYTES, Dimensions, ID_MAX_BYTES, ResourceRef,
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
    // An image is classified from its headers alone, with the display
    // kernel's limits, and kept as its recorded bytes: no pixel is decoded and
    // no codec slot is taken here. Display renditions are derived later.
    let dimensions = if mime.starts_with("image/") {
        match rosetta_media::probe(&bytes) {
            Ok(probe) if probe.media_type == mime => Some(probe.dimensions),
            Ok(_) => return unavailable(id, kind, Missing::InvalidData),
            Err(rosetta_media::MediaError::Unsupported | rosetta_media::MediaError::Limit) => {
                return unavailable(id, kind, Missing::UnsupportedMedia);
            }
            Err(_) => return unavailable(id, kind, Missing::InvalidData),
        }
    } else {
        None
    };
    let asset = ProjectionAsset::new(mime, bytes).expect("validated projection media type");
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

/// The canonical type of a file an agent sent by path, or `None` when that
/// type is not presented. Its own entry point: the inline-media table above
/// stays as it is, so a recorded `video_url` data URL is still not captured.
pub fn presented_media_type(declared: &str) -> Option<&'static str> {
    match declared.trim().to_ascii_lowercase().as_str() {
        "video/mp4" => Some("video/mp4"),
        "video/quicktime" => Some("video/quicktime"),
        _ => canonical_media_type(declared).filter(|mime| mime.starts_with("image/")),
    }
}

/// The attachment kind of a canonical presented type.
pub fn presented_kind(canonical: &str) -> Option<AttachmentKind> {
    match presented_media_type(canonical)? {
        mime if mime != canonical => None,
        mime if mime.starts_with("image/") => Some(AttachmentKind::Image),
        _ => Some(AttachmentKind::Video),
    }
}

/// The most bytes a presented file of a canonical type may have.
pub fn presented_limit(canonical: &str) -> Option<u64> {
    match presented_kind(canonical)? {
        AttachmentKind::Image => Some(rosetta_media::MAX_INPUT_BYTES as u64),
        _ => Some(VIDEO_MAX_BYTES),
    }
}

/// Verify the captured bytes of a presented file against the type it was
/// declared with and keep them as the member. An image passes the same
/// header probe as inline media; a video passes when its container header
/// names the declared type. `None` is every refusal: a type that is not
/// presented, bytes over the limit, a wrong signature, a failed probe.
pub fn from_presented_bytes(
    media_type: &str,
    bytes: Vec<u8>,
) -> Option<(ProjectionAsset, Option<Dimensions>)> {
    let mime = presented_media_type(media_type)?;
    if bytes.len() as u64 > presented_limit(mime)? {
        return None;
    }
    match presented_kind(mime)? {
        AttachmentKind::Image => match from_bytes("x", AttachmentKind::Image, mime, bytes) {
            (
                AttachmentRef {
                    source: AttachmentSource::Asset { .. },
                    dimensions,
                    ..
                },
                Some(asset),
            ) => Some((asset, dimensions)),
            _ => None,
        },
        _ => {
            let probe = rosetta_media::probe_video(&bytes).ok()?;
            if probe.media_type != mime {
                return None;
            }
            let dimensions = probe
                .dimensions
                .filter(|size| size.width > 0 && size.height > 0);
            Some((ProjectionAsset::new(mime, bytes)?, dimensions))
        }
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

    /// A leading `ftyp` box with one compatible brand, then a `moov` header.
    fn video(major: &[u8; 4], compatible: &[u8; 4]) -> Vec<u8> {
        let mut bytes = 20_u32.to_be_bytes().to_vec();
        bytes.extend_from_slice(b"ftyp");
        bytes.extend_from_slice(major);
        bytes.extend_from_slice(&[0, 0, 2, 0]);
        bytes.extend_from_slice(compatible);
        bytes.extend_from_slice(b"\0\0\0\x08moov");
        bytes
    }

    #[test]
    fn presented_types_are_images_and_two_video_containers() {
        for (declared, canonical, kind, limit) in [
            ("image/png", "image/png", AttachmentKind::Image, 32),
            ("image/apng", "image/png", AttachmentKind::Image, 32),
            (" IMAGE/JPG ", "image/jpeg", AttachmentKind::Image, 32),
            ("image/gif", "image/gif", AttachmentKind::Image, 32),
            ("image/webp", "image/webp", AttachmentKind::Image, 32),
            ("image/bmp", "image/bmp", AttachmentKind::Image, 32),
            (
                "image/vnd.microsoft.icon",
                "image/x-icon",
                AttachmentKind::Image,
                32,
            ),
            ("video/mp4", "video/mp4", AttachmentKind::Video, 64),
            (" Video/MP4", "video/mp4", AttachmentKind::Video, 64),
            (
                "video/quicktime",
                "video/quicktime",
                AttachmentKind::Video,
                64,
            ),
        ] {
            assert_eq!(
                presented_media_type(declared),
                Some(canonical),
                "{declared}"
            );
            assert_eq!(presented_kind(canonical), Some(kind), "{canonical}");
            assert_eq!(
                presented_limit(canonical),
                Some(limit * 1024 * 1024),
                "{canonical}"
            );
            // Every presented type is one core accepts as a fact and a member.
            assert!(rosetta_core::presented_media::MEDIA_TYPES.contains(&canonical));
            assert!(ProjectionAsset::supported_extension(canonical).is_some());
        }
        for media_type in rosetta_core::presented_media::MEDIA_TYPES {
            assert_eq!(presented_media_type(media_type), Some(media_type));
        }
        for declared in [
            "",
            "text/html",
            "text/markdown",
            "text/plain",
            "application/pdf",
            "image/svg+xml",
            "image/heic",
            "image/avif",
            "video/webm",
            "video/x-m4v",
            "audio/mp4",
            "video/mp4; codecs=avc1",
        ] {
            assert_eq!(presented_media_type(declared), None, "{declared}");
            assert_eq!(presented_kind(declared), None, "{declared}");
            assert_eq!(presented_limit(declared), None, "{declared}");
        }
    }

    #[test]
    fn presented_bytes_are_verified_against_their_declared_type() {
        let png = STANDARD.decode(PNG).unwrap();
        let (asset, dimensions) = from_presented_bytes("image/png", png.clone()).unwrap();
        assert_eq!(asset.media_type, "image/png");
        assert_eq!(asset.bytes, png);
        assert_eq!(
            asset.digest,
            blake3::hash(&png).to_hex().to_string(),
            "the member is the recorded bytes"
        );
        assert_eq!(
            dimensions,
            Some(Dimensions {
                width: 1,
                height: 1
            })
        );
        asset.reference().validate().unwrap();

        let mp4 = video(b"isom", b"mp41");
        let (asset, dimensions) = from_presented_bytes("video/mp4", mp4.clone()).unwrap();
        assert_eq!(
            (asset.media_type.as_str(), &asset.bytes, dimensions),
            ("video/mp4", &mp4, None)
        );
        assert!(asset.url().unwrap().ends_with(".mp4"));
        asset.reference().validate().unwrap();

        let mov = video(b"qt  ", b"qt  ");
        let (asset, dimensions) = from_presented_bytes("video/quicktime", mov.clone()).unwrap();
        assert_eq!(
            (asset.media_type.as_str(), &asset.bytes, dimensions),
            ("video/quicktime", &mov, None)
        );
        assert!(asset.url().unwrap().ends_with(".mov"));

        // Wrong magic, in each direction.
        assert_eq!(from_presented_bytes("image/png", mp4.clone()), None);
        assert_eq!(from_presented_bytes("image/jpeg", png.clone()), None);
        assert_eq!(from_presented_bytes("video/mp4", png.clone()), None);
        assert_eq!(
            from_presented_bytes("video/mp4", b"not a video".to_vec()),
            None
        );
        assert_eq!(from_presented_bytes("video/mp4", Vec::new()), None);
        assert_eq!(from_presented_bytes("image/png", Vec::new()), None);
        // The detected container must be the declared one.
        assert_eq!(from_presented_bytes("video/mp4", mov.clone()), None);
        assert_eq!(from_presented_bytes("video/quicktime", mp4.clone()), None);
        // A type that is not presented is refused whatever the bytes are.
        assert_eq!(from_presented_bytes("video/webm", mp4.clone()), None);
        assert_eq!(
            from_presented_bytes("application/pdf", b"%PDF-1.7".to_vec()),
            None
        );
        assert_eq!(from_presented_bytes("text/plain", b"hello".to_vec()), None);
        // A truncated image fails the header probe, not just the signature.
        assert_eq!(from_presented_bytes("image/png", png[..20].to_vec()), None);
    }

    #[test]
    fn presented_bytes_over_the_limit_are_refused() {
        let limit = VIDEO_MAX_BYTES as usize;
        let mut video = video(b"isom", b"mp41");
        video.resize(limit, 0);
        let (asset, _) = from_presented_bytes("video/mp4", video.clone()).unwrap();
        assert_eq!(asset.bytes.len(), limit);
        video.push(0);
        assert_eq!(from_presented_bytes("video/mp4", video), None);

        let mut image = STANDARD.decode(PNG).unwrap();
        image.resize(rosetta_media::MAX_INPUT_BYTES + 1, 0);
        assert_eq!(from_presented_bytes("image/png", image), None);
    }

    /// The presented entry points are additions: inline media is classified
    /// exactly as before, so a recorded video data URL is still not captured.
    #[test]
    fn inline_video_is_still_not_materialized() {
        assert_eq!(canonical_media_type("video/mp4"), None);
        assert_eq!(canonical_media_type("video/quicktime"), None);
        let mp4 = video(b"isom", b"mp41");
        let url = format!("data:video/mp4;base64,{}", STANDARD.encode(&mp4));
        let (attachment, asset) = from_url("clip", AttachmentKind::Video, &url, None);
        assert_eq!(asset, None);
        assert_eq!(
            attachment.source,
            AttachmentSource::Unavailable {
                reason: Missing::UnsupportedMedia
            }
        );
        assert_eq!(attachment.media_type.as_deref(), Some("video/mp4"));
        let (attachment, asset) = from_base64(
            "clip",
            AttachmentKind::Video,
            "video/mp4",
            &STANDARD.encode(&mp4),
        );
        assert_eq!(asset, None);
        assert!(matches!(
            attachment.source,
            AttachmentSource::Unavailable {
                reason: Missing::UnsupportedMedia
            }
        ));
        let (attachment, asset) = from_bytes("clip", AttachmentKind::Video, "video/mp4", mp4);
        assert_eq!(asset, None);
        assert_eq!(
            attachment.source,
            AttachmentSource::Unavailable {
                reason: Missing::UnsupportedMedia
            }
        );
    }
}

#[cfg(test)]
mod recorded_originals;
