//! Recorded images are kept as their recorded bytes and classified from
//! headers alone. Every sample is synthesized here; nothing is read from disk.
use super::*;
use image::codecs::gif::{GifEncoder, Repeat};
use image::{DynamicImage, Frame, ImageFormat, Rgba, RgbaImage};
use rosetta_core::assets::ImageVariant;
use rosetta_core::timeline::Dimensions;
use std::io::Cursor;

fn pixels(width: u32, height: u32) -> RgbaImage {
    RgbaImage::from_fn(width, height, |x, y| {
        Rgba([(x * 7) as u8, (y * 5) as u8, 90, 200])
    })
}

fn encoded(width: u32, height: u32, format: ImageFormat) -> Vec<u8> {
    let image = DynamicImage::ImageRgba8(pixels(width, height));
    let image = if format == ImageFormat::Jpeg {
        DynamicImage::ImageRgb8(image.to_rgb8())
    } else {
        image
    };
    let mut out = Cursor::new(Vec::new());
    image.write_to(&mut out, format).unwrap();
    out.into_inner()
}

fn animated_gif(width: u32, height: u32, frames: usize) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut encoder = GifEncoder::new(&mut out);
        encoder.set_repeat(Repeat::Infinite).unwrap();
        encoder
            .encode_frames((0..frames).map(|_| Frame::new(pixels(width, height))))
            .unwrap();
    }
    out
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn chunk(out: &mut Vec<u8>, tag: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let start = out.len();
    out.extend_from_slice(tag);
    out.extend_from_slice(data);
    let crc = crc32(&out[start..]);
    out.extend_from_slice(&crc.to_be_bytes());
}

/// An 8-bit RGBA IHDR and a tiny IDAT: readers stop at the IDAT header.
fn png_header(width: u32, height: u32) -> Vec<u8> {
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = width.to_be_bytes().to_vec();
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &[0x78, 0x9c, 0x63, 0, 0]);
    chunk(&mut out, b"IEND", &[]);
    out
}

/// A valid PNG whose IDAT payload (after the zlib header) is scrambled under
/// a correct CRC: every header is sound and only a pixel decode can tell.
fn corrupt_idat_png() -> Vec<u8> {
    let mut png = encoded(64, 64, ImageFormat::Png);
    let mut offset = 8;
    loop {
        let len = u32::from_be_bytes(png[offset..offset + 4].try_into().unwrap()) as usize;
        if &png[offset + 4..offset + 8] == b"IDAT" {
            for byte in &mut png[offset + 8 + 2..offset + 8 + len] {
                *byte ^= 0x5a;
            }
            let crc = crc32(&png[offset + 4..offset + 8 + len]);
            png[offset + 8 + len..offset + 12 + len].copy_from_slice(&crc.to_be_bytes());
            return png;
        }
        offset += 12 + len;
    }
}

fn dims(width: u32, height: u32) -> Dimensions {
    Dimensions { width, height }
}

fn assert_original(
    (attachment, asset): MaterializedAttachment,
    mime: &str,
    extension: &str,
    bytes: &[u8],
    dimensions: Dimensions,
) {
    let asset = asset.unwrap_or_else(|| panic!("{mime}: {attachment:?}"));
    assert_eq!(asset.media_type, mime);
    assert_eq!(
        asset.bytes, bytes,
        "{mime}: the member is the recorded bytes"
    );
    assert_eq!(asset.digest, blake3::hash(bytes).to_hex().as_str());
    assert!(
        asset.image_variants.is_empty(),
        "{mime}: no rendition bundle"
    );
    let AttachmentSource::Asset { asset: reference } = &attachment.source else {
        panic!("{mime}: {attachment:?}")
    };
    assert_eq!(
        reference.url,
        format!("/api/assets/{}.{extension}", asset.digest)
    );
    assert_eq!(reference.media_type, mime);
    assert_eq!(reference.byte_len, bytes.len() as u64);
    assert_eq!(reference.image_variants, None);
    assert_eq!(attachment.media_type.as_deref(), Some(mime));
    assert_eq!(attachment.byte_len, Some(bytes.len() as u64));
    assert_eq!(attachment.dimensions, Some(dimensions), "{mime}");
    attachment.validate().unwrap();
}

fn unavailable_reason(mime: &str, bytes: Vec<u8>) -> Missing {
    let (attachment, asset) = from_bytes("image", AttachmentKind::Image, mime, bytes);
    assert!(asset.is_none(), "{mime}: {attachment:?}");
    attachment.validate().unwrap();
    match attachment.source {
        AttachmentSource::Unavailable { reason } => reason,
        other => panic!("{mime}: {other:?}"),
    }
}

/// This test binary runs no codec work of its own, so the process-wide
/// admission counter moves only if normalization took a codec slot.
#[test]
fn recorded_images_become_original_members_without_a_codec_job() {
    let wide = dims(20_000, 10);
    let samples = [
        (
            "image/png",
            "png",
            encoded(40, 30, ImageFormat::Png),
            dims(40, 30),
        ),
        (
            "image/png",
            "png",
            encoded(wide.width, wide.height, ImageFormat::Png),
            rosetta_media::target_dimensions(wide, ImageVariant::Full),
        ),
        (
            "image/jpeg",
            "jpg",
            encoded(30, 20, ImageFormat::Jpeg),
            dims(30, 20),
        ),
        ("image/gif", "gif", animated_gif(12, 8, 3), dims(12, 8)),
        (
            "image/webp",
            "webp",
            encoded(9, 7, ImageFormat::WebP),
            dims(9, 7),
        ),
        (
            "image/bmp",
            "bmp",
            encoded(5, 4, ImageFormat::Bmp),
            dims(5, 4),
        ),
        (
            "image/x-icon",
            "ico",
            encoded(16, 16, ImageFormat::Ico),
            dims(16, 16),
        ),
    ];
    assert_eq!(
        samples[1].3,
        dims(16_383, 8),
        "an edge over WebP's limit keeps today's fitted dimensions"
    );
    let before = rosetta_media::admitted_jobs();
    for (mime, extension, bytes, dimensions) in &samples {
        let materialized = from_bytes("image", AttachmentKind::Image, mime, bytes.clone());
        assert_original(materialized, mime, extension, bytes, *dimensions);
    }
    let ico = &samples[6].2;
    assert_original(
        from_bytes(
            "alias",
            AttachmentKind::Image,
            "image/vnd.microsoft.icon",
            ico.clone(),
        ),
        "image/x-icon",
        "ico",
        ico,
        dims(16, 16),
    );
    let bmp = &samples[5].2;
    assert_original(
        from_image_base64("generated", &STANDARD.encode(bmp)),
        "image/bmp",
        "bmp",
        bmp,
        dims(5, 4),
    );
    let png = &samples[0].2;
    assert_original(
        from_url(
            "data",
            AttachmentKind::Image,
            &format!("data:image/png;base64,{}", STANDARD.encode(png)),
            None,
        ),
        "image/png",
        "png",
        png,
        dims(40, 30),
    );
    assert_eq!(
        rosetta_media::admitted_jobs(),
        before,
        "normalization never decodes, resizes or encodes pixels"
    );
}

#[test]
fn header_limits_and_mismatches_keep_their_unavailable_reasons() {
    let png = encoded(4, 4, ImageFormat::Png);
    assert_eq!(
        unavailable_reason("image/gif", png.clone()),
        Missing::InvalidData
    );
    assert_eq!(
        unavailable_reason("image/bmp", png.clone()),
        Missing::InvalidData
    );
    assert_eq!(
        unavailable_reason("image/png", b"\x89PNG\r\n\x1a\nheader only".to_vec()),
        Missing::InvalidData
    );
    assert_eq!(
        unavailable_reason("image/png", png_header(8000, 8000)),
        Missing::UnsupportedMedia,
        "a header over 32 MP stays unsupported"
    );
    let mut oversized = png;
    oversized.resize(rosetta_media::MAX_INPUT_BYTES + 1, 0);
    assert_eq!(
        unavailable_reason("image/png", oversized),
        Missing::UnsupportedMedia
    );
    assert_eq!(
        unavailable_reason(
            "image/gif",
            animated_gif(2, 2, rosetta_media::MAX_FRAMES + 1)
        ),
        Missing::UnsupportedMedia,
        "a frame limit the headers can count stays exact"
    );
}

/// Only a pixel decode finds a scrambled IDAT, and ingestion decodes none:
/// the recorded bytes stay a member and only their renditions can fail.
#[test]
fn pixel_corruption_is_left_to_renditions_not_ingestion() {
    let corrupt = corrupt_idat_png();
    let materialized = from_bytes("image", AttachmentKind::Image, "image/png", corrupt.clone());
    assert_original(materialized, "image/png", "png", &corrupt, dims(64, 64));
}
