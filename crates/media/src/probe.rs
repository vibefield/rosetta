//! Header-only classification. `probe()` runs the shared header stage, then
//! counts animation frames from container data with the codec crates `image`
//! itself uses (plus a chunk-header walk for APNG frame sizes): no LZW,
//! inflate or VP8 bitstream is decoded. Every limit and allocation check
//! `prepare()` applies before a frame's pixels is mirrored here, in prepare's
//! order, so an input probes to prepare's class unless only its pixel data is
//! broken (see `probe`).
use crate::animation::{animation_loops, check_animation_pixels, check_frame_count};
use crate::header::{self, Header, Source};
use crate::{MAX_FRAMES, MediaError, check_input, limits, pixels};
use image::{ColorType, ImageFormat};
use rosetta_core::assets::ImageVariant;
use rosetta_core::timeline::Dimensions;
use std::io::Cursor;

/// What `prepare()` would know about an image before decoding its pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Probe {
    /// The recorded encoding: `image/png` (APNG included), `image/jpeg`,
    /// `image/gif`, `image/webp`, `image/bmp` or `image/x-icon`.
    pub media_type: &'static str,
    /// Exactly `prepare()`'s dimensions: the canvas after EXIF orientation,
    /// fitted to WebP's 16383-pixel edge.
    pub dimensions: Dimensions,
    /// Frames the container declares; 1 for a still image.
    pub frames: u32,
}

/// Classify an image from its headers alone, with `prepare()`'s limits:
/// 32 MiB input, 32 MP per frame, 256 frames and 64 MP per animation.
///
/// Ok and the dimensions equal `prepare()`'s whenever `prepare()` succeeds.
/// Otherwise the result is `prepare()`'s error, except for what only decoding
/// or encoding can find:
/// - data only a pixel decode rejects, such as a corrupt IDAT, LZW or VP8
///   stream, an ICO entry that disagrees with its directory, an animation
///   frame that does not fit its canvas, or an APNG fcTL/fdAT the png reader
///   rejects (CRC, sequence number, dispose or blend op): `probe()` accepts
///   it, or reports a frame limit `prepare()` would have reached only after
///   the broken frame, and `prepare()` and `renditions()` answer Invalid;
/// - an encoded rendition over 32 MiB, which `prepare()` and `renditions()`
///   answer with Limit.
pub fn probe(bytes: &[u8]) -> Result<Probe, MediaError> {
    check_input(bytes)?;
    let header = header::read(bytes)?;
    let frames = frames(&header, bytes)?;
    let (width, height) = pixels::target_dimensions(
        header::oriented(header.width, header.height, header.orientation),
        ImageVariant::Full,
    );
    Ok(Probe {
        media_type: media_type(header.format).ok_or(MediaError::Unsupported)?,
        dimensions: Dimensions { width, height },
        frames,
    })
}

fn media_type(format: ImageFormat) -> Option<&'static str> {
    Some(match format {
        ImageFormat::Png => "image/png",
        ImageFormat::Jpeg => "image/jpeg",
        ImageFormat::Gif => "image/gif",
        ImageFormat::WebP => "image/webp",
        ImageFormat::Bmp => "image/bmp",
        ImageFormat::Ico => "image/x-icon",
        _ => return None,
    })
}

/// The animation checks `prepare()` makes before encoding: frame count, frame
/// pixels, the allocations its decoder reserves, and the loop count. Rendering
/// runs this too, so it never accepts what probing rejects.
pub(crate) fn frames(header: &Header<'_>, bytes: &[u8]) -> Result<u32, MediaError> {
    let Source::Animation { loops, color, .. } = &header.source else {
        return Ok(1);
    };
    let canvas = u64::from(header.width) * u64::from(header.height);
    let frames = match header.format {
        ImageFormat::Gif => gif_frames(bytes, header.width, header.height)?,
        ImageFormat::WebP => {
            let decoder = image_webp::WebPDecoder::new(Cursor::new(bytes))
                .map_err(|_| MediaError::Invalid)?;
            declared_frames(decoder.num_frames(), canvas)?
        }
        ImageFormat::Png => apng_frames(bytes, canvas, *color)?,
        _ => return Err(MediaError::Unsupported),
    };
    animation_loops(*loops)?;
    Ok(frames)
}

/// The allocation budget `limits()` gives a decoder, reserved like
/// `image::Limits::reserve`.
struct Budget(u64);
impl Budget {
    fn new() -> Self {
        Self(limits().max_alloc.unwrap_or(u64::MAX))
    }
    fn reserve(&mut self, bytes: u64) -> Result<(), MediaError> {
        self.0 = self.0.checked_sub(bytes).ok_or(MediaError::Invalid)?;
        Ok(())
    }
}

/// Walk GIF frame descriptors with LZW skipped. Per frame, `image`'s GIF
/// iterator reserves the RGBA frame and, unless the frame covers the canvas
/// exactly, a second canvas, beside the canvas it keeps for disposal.
fn gif_frames(bytes: &[u8], width: u32, height: u32) -> Result<u32, MediaError> {
    let mut options = gif::DecodeOptions::new();
    options.set_color_output(gif::ColorOutput::RGBA);
    options.skip_frame_decoding(true);
    let mut decoder = options
        .read_info(Cursor::new(bytes))
        .map_err(|_| MediaError::Invalid)?;
    let canvas = u64::from(width) * u64::from(height);
    let (mut count, mut total) = (0, 0);
    loop {
        let frame = match decoder.next_frame_info() {
            Ok(None) => break,
            Ok(Some(frame)) => Ok((frame.left, frame.top, frame.width, frame.height)),
            Err(_) => Err(MediaError::Invalid),
        };
        count += 1;
        check_frame_count(count)?;
        let (left, top, frame_width, frame_height) = frame?;
        let mut budget = Budget::new();
        budget.reserve(canvas * 4)?;
        budget.reserve(u64::from(frame_width) * u64::from(frame_height) * 4)?;
        if (left, top) != (0, 0)
            || (u32::from(frame_width), u32::from(frame_height)) != (width, height)
        {
            budget.reserve(canvas * 4)?;
        }
        total += canvas;
        check_animation_pixels(total)?;
    }
    if count == 0 {
        return Err(MediaError::Invalid);
    }
    Ok(count as u32)
}

/// `image`'s APNG iterator yields acTL `num_frames` frames and fails at the
/// first one the stream does not hold. It keeps two RGBA canvases; for each
/// frame it reserves beside them the raw output buffer (canvas-sized, also
/// for a skipped default image) and the frame's own RGBA. 16-bit samples
/// cannot be composited.
fn apng_frames(bytes: &[u8], canvas: u64, color: ColorType) -> Result<u32, MediaError> {
    // Configured as the `PngDecoder::new` that yields the frames: `image`'s
    // limits reach its own reservations, not the png reader.
    let mut decoder =
        png::Decoder::new_with_limits(Cursor::new(bytes), png::Limits { bytes: usize::MAX });
    decoder.set_ignore_text_chunk(false);
    decoder.set_transformations(png::Transformations::EXPAND);
    let reader = decoder.read_info().map_err(|_| MediaError::Invalid)?;
    let control = reader.info().animation_control.ok_or(MediaError::Invalid)?;
    let raw = reader.output_buffer_size().ok_or(MediaError::Invalid)? as u64;
    let sixteen_bit = matches!(
        color,
        ColorType::L16 | ColorType::La16 | ColorType::Rgb16 | ColorType::Rgba16
    );
    let sizes = apng_frame_sizes(bytes);
    let mut total = 0;
    for count in 1..=control.num_frames.min(MAX_FRAMES as u32 + 1) as usize {
        check_frame_count(count)?;
        let (width, height) = *sizes.get(count - 1).ok_or(MediaError::Invalid)?;
        if sixteen_bit {
            return Err(MediaError::Invalid);
        }
        let mut budget = Budget::new();
        budget.reserve(canvas * 8)?;
        budget.reserve(raw)?;
        budget.reserve(u64::from(width) * u64::from(height) * 4)?;
        total += canvas;
        check_animation_pixels(total)?;
    }
    if control.num_frames == 0 {
        return Err(MediaError::Invalid);
    }
    Ok(control.num_frames)
}

/// Each APNG frame's size, in stream order, from chunk headers and fcTL
/// bodies alone; IDAT and fdAT payloads are skipped, never inflated. A frame
/// is an fcTL followed by image data (IDAT for a default image that is a
/// frame, fdAT after it) before the next fcTL: the png reader keeps the last
/// fcTL before the data. The walk ends where the reader stops reading frames:
/// at IEND, a short fcTL, an fdAT before the IDAT or after neither an fcTL
/// nor another fdAT, or a chunk running past the input.
fn apng_frame_sizes(bytes: &[u8]) -> Vec<(u32, u32)> {
    let be = |field: &[u8]| u32::from_be_bytes([field[0], field[1], field[2], field[3]]);
    let mut sizes = Vec::new();
    let (mut pending, mut in_data, mut after_idat) = (None, false, false);
    let mut offset = 8;
    while let Some(header) = bytes.get(offset..offset + 8) {
        let len = be(&header[..4]) as usize;
        let Some(data) = bytes.get(offset + 8..).and_then(|rest| rest.get(..len)) else {
            break;
        };
        let tag = &header[4..];
        match tag {
            b"fcTL" if data.len() < 26 => break,
            b"fcTL" => pending = Some((be(&data[4..8]), be(&data[8..12]))),
            b"fdAT" if !after_idat || (pending.is_none() && !in_data) => break,
            b"IDAT" | b"fdAT" => {
                after_idat = true;
                sizes.extend(pending.take());
            }
            b"IEND" => break,
            _ => {}
        }
        in_data = tag == b"fdAT";
        offset += 12 + len;
    }
    sizes
}

/// A container frame count, checked as `prepare()` meets those frames.
fn declared_frames(frames: u32, canvas: u64) -> Result<u32, MediaError> {
    let mut total = 0;
    for count in 1..=frames.min(MAX_FRAMES as u32 + 1) as usize {
        check_frame_count(count)?;
        total += canvas;
        check_animation_pixels(total)?;
    }
    if frames == 0 {
        return Err(MediaError::Invalid);
    }
    Ok(frames)
}
