//! A small synthetic image corpus, built in-process. No file or home directory
//! is read: every sample is encoded here or hand-assembled from chunks.
use crate::MediaError;
use image::{DynamicImage, ImageEncoder, ImageFormat, Rgba, RgbaImage};
use std::io::Cursor;

pub(crate) struct Sample {
    pub name: &'static str,
    pub bytes: Vec<u8>,
}
fn sample(name: &'static str, bytes: Vec<u8>) -> Sample {
    Sample { name, bytes }
}

/// Inputs `prepare()` turns into a WebP bundle, built once per test binary.
/// Take it before admitting a codec permit: building it runs `prepare()`.
pub(crate) fn decodable() -> &'static [Sample] {
    static CORPUS: std::sync::OnceLock<Vec<Sample>> = std::sync::OnceLock::new();
    CORPUS.get_or_init(build_decodable)
}

fn build_decodable() -> Vec<Sample> {
    let animated_webp = crate::prepare(&gif_frames(40, 30, 3, gif::Repeat::Finite(3)))
        .unwrap()
        .asset
        .bytes
        .clone();
    let wide_animation = crate::prepare(&gif_frames(1700, 20, 2, gif::Repeat::Infinite))
        .unwrap()
        .asset
        .bytes
        .clone();
    vec![
        sample("png-rgba-400x600", png_rgba(400, 600)),
        sample("png-rgba16-64x48", png16(64, 48)),
        sample("png-gray-37x23", encoded(gray(37, 23), ImageFormat::Png)),
        sample("png-1700x300", png_rgba(1700, 300)),
        sample("png-1600x200", png_rgba(1600, 200)),
        sample("png-wide-17000x20", png_rgba(17000, 20)),
        sample("png-20x80", png_rgba(20, 80)),
        sample("png-square-100", png_rgba(100, 100)),
        sample("png-1x1", png_rgba(1, 1)),
        sample("png-p3-icc", png_icc(32, 32)),
        sample("png-exif6-40x24", png_exif(40, 24, 6)),
        sample("jpeg-exif1", jpeg(300, 200, Some(1))),
        sample("jpeg-exif3", jpeg(300, 200, Some(3))),
        sample("jpeg-exif6", jpeg(300, 200, Some(6))),
        sample("jpeg-exif8", jpeg(300, 200, Some(8))),
        sample("jpeg-exif8-1700x300", jpeg(1700, 300, Some(8))),
        sample("jpeg-exif6-17000x24", jpeg(17000, 24, Some(6))),
        sample("jpeg-plain", jpeg(64, 64, None)),
        sample("jpeg-p3-icc", jpeg_icc(64, 64)),
        sample(
            "gif-3-frames",
            gif_frames(40, 30, 3, gif::Repeat::Finite(3)),
        ),
        sample("gif-1-frame", gif_frames(24, 24, 1, gif::Repeat::Infinite)),
        sample("gif-partial-frames", gif_partial()),
        sample(
            "gif-wide-1700x20",
            gif_frames(1700, 20, 2, gif::Repeat::Infinite),
        ),
        sample("apng-3-frames", apng(48, 32, 3, 2, false)),
        sample("apng-thumbnail", apng(48, 32, 2, 0, true)),
        sample(
            "apng-declares-fewer-frames",
            with_actl_frames(apng(48, 32, 3, 0, false), 1),
        ),
        sample("webp-animated", animated_webp),
        sample("webp-animated-1700x20", wide_animation),
        sample("webp-lossless-alpha", webp_lossless(50, 40)),
        sample(
            "webp-lossy",
            crate::pixels::encode(&rgba(300, 200)).unwrap(),
        ),
        sample("bmp-rgba", encoded(rgba(70, 50), ImageFormat::Bmp)),
        sample(
            "bmp-rgb",
            encoded(
                DynamicImage::ImageRgba8(rgba(33, 21)).to_rgb8(),
                ImageFormat::Bmp,
            ),
        ),
        sample("ico-48", encoded(rgba(48, 48), ImageFormat::Ico)),
    ]
}

/// Inputs both `probe()` and `prepare()` reject with this class.
pub(crate) fn rejected() -> Vec<(Sample, MediaError)> {
    vec![
        (sample("empty", Vec::new()), MediaError::Invalid),
        (
            sample("text", b"not an image".to_vec()),
            MediaError::Unsupported,
        ),
        (
            sample("tiff", b"II*\0\x08\0\0\0\0\0\0\0".to_vec()),
            MediaError::Unsupported,
        ),
        (
            sample("png-header-only", b"\x89PNG\r\n\x1a\nheader only".to_vec()),
            MediaError::Invalid,
        ),
        (
            sample("oversized-input", vec![0; crate::MAX_INPUT_BYTES + 1]),
            MediaError::Limit,
        ),
        (
            sample("png-header-64mp", png_header(8000, 8000, 8, 6, &[])),
            MediaError::Limit,
        ),
        (
            sample("png-header-too-wide", png_header(70000, 1, 8, 6, &[])),
            MediaError::Invalid,
        ),
        (
            sample(
                "gif-257-frames",
                gif_frames(2, 2, 257, gif::Repeat::Infinite),
            ),
            MediaError::Limit,
        ),
        (
            sample(
                "gif-no-frames",
                b"GIF89a\x02\0\x02\0\x80\0\0\0\0\0\xff\xff\xff\x3b".to_vec(),
            ),
            MediaError::Invalid,
        ),
        (
            sample("gif-frame-over-alloc", gif_oversized_frame()),
            MediaError::Invalid,
        ),
        (
            sample("apng-plays-over-u16", apng(8, 8, 2, 70000, false)),
            MediaError::Limit,
        ),
        (sample("apng-16-bit", apng16(8, 8)), MediaError::Invalid),
        (
            sample("apng-canvas-over-alloc", apng_over_alloc()),
            MediaError::Invalid,
        ),
        (
            sample(
                "apng-declares-more-frames",
                with_actl_frames(apng(48, 32, 3, 0, false), 5),
            ),
            MediaError::Invalid,
        ),
        (
            sample(
                "apng-thumbnail-declares-more-frames",
                with_actl_frames(apng(48, 32, 2, 0, true), 4),
            ),
            MediaError::Invalid,
        ),
        (
            sample("apng-first-frame-over-alloc", apng_thumbnail_over_alloc()),
            MediaError::Invalid,
        ),
        (
            sample("webp-animated-no-frames", webp_no_frames()),
            MediaError::Invalid,
        ),
    ]
}

/// Headers that pass every limit while the pixels cannot be decoded: `probe()`
/// accepts them and `prepare()` (and therefore `renditions()`) answers Invalid.
pub(crate) fn pixel_corrupt() -> Vec<Sample> {
    let valid = png_rgba(64, 64);
    let (start, end) = idat(&valid);
    let mut corrupt = valid.clone();
    for byte in &mut corrupt[start + 8 + 4..end - 4] {
        *byte ^= 0x5a;
    }
    let crc = crc32(&corrupt[start + 4..end - 4]);
    corrupt[end - 4..end].copy_from_slice(&crc.to_be_bytes());
    let truncated = valid[..start + 8 + (end - start - 12) / 2].to_vec();
    vec![
        sample("png-corrupt-idat", corrupt),
        sample("png-truncated-idat", truncated),
    ]
}

/// A 25 MP canvas with three 1x1 frames: 75 MP of composited frames. Only the
/// probe is run on it; `prepare()` would decode and encode two 25 MP frames first.
pub(crate) fn gif_canvas_over_animation_limit() -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut encoder =
            gif::Encoder::new(&mut out, 5000, 5000, &[0, 0, 0, 255, 255, 255]).unwrap();
        for _ in 0..3 {
            let mut frame = gif::Frame {
                width: 1,
                height: 1,
                buffer: vec![1].into(),
                ..gif::Frame::default()
            };
            frame.delay = 5;
            encoder.write_frame(&frame).unwrap();
        }
    }
    out
}

pub(crate) fn rgba(width: u32, height: u32) -> RgbaImage {
    RgbaImage::from_fn(width, height, |x, y| {
        Rgba([
            (x % 253) as u8,
            (y % 251) as u8,
            ((x + y) % 241) as u8,
            if x < width / 2 { 128 } else { 255 },
        ])
    })
}
fn gray(width: u32, height: u32) -> image::GrayImage {
    image::GrayImage::from_fn(width, height, |x, y| {
        image::Luma([((x * 7 + y * 3) % 256) as u8])
    })
}
fn encoded(image: impl Into<DynamicImage>, format: ImageFormat) -> Vec<u8> {
    let mut bytes = Cursor::new(Vec::new());
    image.into().write_to(&mut bytes, format).unwrap();
    bytes.into_inner()
}
pub(crate) fn png_rgba(width: u32, height: u32) -> Vec<u8> {
    encoded(rgba(width, height), ImageFormat::Png)
}
fn png16(width: u32, height: u32) -> Vec<u8> {
    let image = image::ImageBuffer::<Rgba<u16>, _>::from_fn(width, height, |x, y| {
        Rgba([
            (x * 1021) as u16,
            (y * 977) as u16,
            40000,
            65535 - (x * 300) as u16,
        ])
    });
    encoded(DynamicImage::ImageRgba16(image), ImageFormat::Png)
}
fn png_icc(width: u32, height: u32) -> Vec<u8> {
    let profile = moxcms::ColorProfile::new_display_p3().encode().unwrap();
    let pixels = RgbaImage::from_pixel(width, height, Rgba([160, 70, 30, 128]));
    let mut out = Vec::new();
    let mut encoder = image::codecs::png::PngEncoder::new(&mut out);
    encoder.set_icc_profile(profile).unwrap();
    encoder
        .write_image(
            pixels.as_raw(),
            width,
            height,
            image::ExtendedColorType::Rgba8,
        )
        .unwrap();
    out
}
/// A little-endian TIFF IFD holding one SHORT orientation tag.
fn exif(orientation: u8) -> Vec<u8> {
    let mut tiff = b"II\x2a\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0".to_vec();
    tiff.extend_from_slice(&[orientation, 0, 0, 0, 0, 0, 0, 0]);
    tiff
}
fn png_exif(width: u32, height: u32, orientation: u8) -> Vec<u8> {
    let pixels = rgba(width, height);
    let mut out = Vec::new();
    let mut encoder = image::codecs::png::PngEncoder::new(&mut out);
    encoder.set_exif_metadata(exif(orientation)).unwrap();
    encoder
        .write_image(
            pixels.as_raw(),
            width,
            height,
            image::ExtendedColorType::Rgba8,
        )
        .unwrap();
    out
}
fn jpeg(width: u32, height: u32, orientation: Option<u8>) -> Vec<u8> {
    let pixels = DynamicImage::ImageRgba8(rgba(width, height)).to_rgb8();
    let mut out = Vec::new();
    let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 85);
    if let Some(orientation) = orientation {
        encoder.set_exif_metadata(exif(orientation)).unwrap();
    }
    encoder
        .write_image(
            pixels.as_raw(),
            width,
            height,
            image::ExtendedColorType::Rgb8,
        )
        .unwrap();
    out
}
fn jpeg_icc(width: u32, height: u32) -> Vec<u8> {
    let profile = moxcms::ColorProfile::new_display_p3().encode().unwrap();
    let pixels = image::RgbImage::from_pixel(width, height, image::Rgb([160, 70, 30]));
    let mut out = Vec::new();
    let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 90);
    encoder.set_icc_profile(profile).unwrap();
    encoder
        .write_image(
            pixels.as_raw(),
            width,
            height,
            image::ExtendedColorType::Rgb8,
        )
        .unwrap();
    out
}
fn webp_lossless(width: u32, height: u32) -> Vec<u8> {
    let pixels = rgba(width, height);
    let mut out = Vec::new();
    image::codecs::webp::WebPEncoder::new_lossless(&mut out)
        .write_image(
            pixels.as_raw(),
            width,
            height,
            image::ExtendedColorType::Rgba8,
        )
        .unwrap();
    out
}

fn gif_frames(width: u16, height: u16, frames: usize, repeat: gif::Repeat) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut encoder = gif::Encoder::new(&mut out, width, height, &[]).unwrap();
        encoder.set_repeat(repeat).unwrap();
        for index in 0..frames {
            let mut pixels: Vec<u8> = (0..u32::from(width) * u32::from(height))
                .flat_map(|i| {
                    let shade = ((i + index as u32 * 37) % 200) as u8;
                    [shade, 255 - shade, (index * 60 % 256) as u8, 255]
                })
                .collect();
            let mut frame = gif::Frame::from_rgba_speed(width, height, &mut pixels, 30);
            frame.delay = 4 + index as u16 * 3;
            encoder.write_frame(&frame).unwrap();
        }
    }
    out
}
/// Offset sub-frames over a transparent canvas, including disposal.
fn gif_partial() -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut encoder = gif::Encoder::new(&mut out, 64, 48, &[]).unwrap();
        encoder.set_repeat(gif::Repeat::Infinite).unwrap();
        for (index, (left, top, width, height)) in
            [(0, 0, 64, 48), (10, 5, 20, 16), (30, 20, 34, 28)]
                .into_iter()
                .enumerate()
        {
            let mut pixels: Vec<u8> = (0..u32::from(width) * u32::from(height))
                .flat_map(|i| {
                    let alpha = if i % 7 == 0 { 0 } else { 255 };
                    [(index * 90) as u8, (i % 255) as u8, 200, alpha]
                })
                .collect();
            let mut frame = gif::Frame::from_rgba_speed(width, height, &mut pixels, 30);
            frame.left = left;
            frame.top = top;
            frame.delay = 7;
            frame.dispose = if index == 1 {
                gif::DisposalMethod::Background
            } else {
                gif::DisposalMethod::Keep
            };
            encoder.write_frame(&frame).unwrap();
        }
    }
    out
}
/// A frame descriptor wider than every allocation limit on a small screen.
fn gif_oversized_frame() -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut encoder = gif::Encoder::new(&mut out, 100, 100, &[0, 0, 0, 255, 255, 255]).unwrap();
        let frame = gif::Frame {
            width: 1,
            height: 1,
            buffer: vec![1].into(),
            ..gif::Frame::default()
        };
        encoder.write_frame(&frame).unwrap();
    }
    // Patch the image descriptor's width and height (after "GIF89a", the
    // screen descriptor, a 2-entry global table and the 0x2c introducer).
    let descriptor = out.iter().position(|b| *b == 0x2c).unwrap();
    out[descriptor + 5..descriptor + 7].copy_from_slice(&8200u16.to_le_bytes());
    out[descriptor + 7..descriptor + 9].copy_from_slice(&8200u16.to_le_bytes());
    out
}

fn apng(width: u32, height: u32, frames: u32, plays: u32, thumbnail: bool) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_animated(frames, plays).unwrap();
        if thumbnail {
            encoder.set_sep_def_img(true).unwrap();
        }
        let mut writer = encoder.write_header().unwrap();
        for index in 0..frames + u32::from(thumbnail) {
            writer.set_frame_delay(1 + index as u16, 20).unwrap();
            let data: Vec<u8> = (0..width * height)
                .flat_map(|i| [(i * 5 % 256) as u8, (index * 70 % 256) as u8, 90, 200])
                .collect();
            writer.write_image_data(&data).unwrap();
        }
    }
    out
}
fn apng16(width: u32, height: u32) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Sixteen);
        encoder.set_animated(2, 0).unwrap();
        let mut writer = encoder.write_header().unwrap();
        for _ in 0..2 {
            writer
                .write_image_data(&vec![0x40; (width * height * 8) as usize])
                .unwrap();
        }
    }
    out
}
/// An RGBA APNG canvas just past 16 Mi pixels: its first frame cannot fit the
/// decoder's allocation limit. The IDAT is never reached by a header reader.
fn apng_over_alloc() -> Vec<u8> {
    let (width, height) = (4097u32, 4096u32);
    let mut actl = 2u32.to_be_bytes().to_vec();
    actl.extend_from_slice(&0u32.to_be_bytes());
    let mut fctl = 0u32.to_be_bytes().to_vec();
    for value in [width, height, 0, 0] {
        fctl.extend_from_slice(&value.to_be_bytes());
    }
    fctl.extend_from_slice(&[0, 1, 0, 10, 0, 0]);
    png_header(width, height, 8, 6, &[(b"acTL", actl), (b"fcTL", fctl)])
}
/// A 21.6 MP grayscale APNG whose default image is not a frame. The default
/// image's raw buffer fits beside the two RGBA canvases; the first frame's raw
/// buffer and RGBA do not. Only the fcTL after the IDAT tells, so the probe
/// must read it. The planes are zero, so they deflate to almost nothing.
fn apng_thumbnail_over_alloc() -> Vec<u8> {
    let (width, height) = (4800, 4500);
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(png::ColorType::Grayscale);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(png::Compression::Fast);
        encoder.set_animated(1, 0).unwrap();
        encoder.set_sep_def_img(true).unwrap();
        let mut writer = encoder.write_header().unwrap();
        let plane = vec![0; (width * height) as usize];
        writer.write_image_data(&plane).unwrap();
        writer.write_image_data(&plane).unwrap();
    }
    out
}
/// The same APNG with its acTL `num_frames` rewritten (and the CRC fixed).
fn with_actl_frames(mut png: Vec<u8>, frames: u32) -> Vec<u8> {
    let mut offset = 8;
    loop {
        let len = u32::from_be_bytes(png[offset..offset + 4].try_into().unwrap()) as usize;
        if &png[offset + 4..offset + 8] == b"acTL" {
            png[offset + 8..offset + 12].copy_from_slice(&frames.to_be_bytes());
            let crc = crc32(&png[offset + 4..offset + 8 + len]);
            png[offset + 8 + len..offset + 12 + len].copy_from_slice(&crc.to_be_bytes());
            return png;
        }
        offset += 12 + len;
    }
}
/// IHDR, optional chunks, and a tiny IDAT. Readers stop at the IDAT header.
fn png_header(
    width: u32,
    height: u32,
    depth: u8,
    color: u8,
    chunks: &[(&[u8; 4], Vec<u8>)],
) -> Vec<u8> {
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = width.to_be_bytes().to_vec();
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[depth, color, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    for (tag, data) in chunks {
        chunk(&mut out, tag, data);
    }
    chunk(&mut out, b"IDAT", &[0x78, 0x9c, 0x63, 0, 0]);
    chunk(&mut out, b"IEND", &[]);
    out
}
fn chunk(out: &mut Vec<u8>, tag: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let start = out.len();
    out.extend_from_slice(tag);
    out.extend_from_slice(data);
    let crc = crc32(&out[start..]);
    out.extend_from_slice(&crc.to_be_bytes());
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
/// The byte range of the first IDAT chunk: length, tag, data and CRC.
fn idat(png: &[u8]) -> (usize, usize) {
    let mut offset = 8;
    loop {
        let len = u32::from_be_bytes(png[offset..offset + 4].try_into().unwrap()) as usize;
        if &png[offset + 4..offset + 8] == b"IDAT" {
            return (offset, offset + 12 + len);
        }
        offset += 12 + len;
    }
}
/// VP8X with the animation flag and an ANIM chunk, but no ANMF frame.
fn webp_no_frames() -> Vec<u8> {
    let mut body = b"WEBP".to_vec();
    let mut vp8x = vec![0x02, 0, 0, 0];
    vp8x.extend_from_slice(&[7, 0, 0, 7, 0, 0]); // 8x8 canvas
    for (tag, data) in [(b"VP8X", vp8x), (b"ANIM", vec![0, 0, 0, 0, 0, 0])] {
        body.extend_from_slice(tag);
        body.extend_from_slice(&(data.len() as u32).to_le_bytes());
        body.extend_from_slice(&data);
    }
    let mut out = b"RIFF".to_vec();
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend(body);
    out
}
