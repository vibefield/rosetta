//! Assemble full-canvas WebP frames encoded by libwebp. Keeping the recorded
//! ANMF durations explicitly avoids encoder heuristics changing the last delay.
//! Rail and tile renditions are still posters of the first frame.
use crate::render::{Encodings, Rendered, Stop, THUMBS, Wanted, stopped};
use crate::{
    MAX_ANIMATION_PIXELS, MAX_FRAMES, MAX_INPUT_BYTES, MediaError, check_dimensions, pixels,
};
use image::{
    DynamicImage, Frames,
    metadata::{LoopCount, Orientation},
};
use rosetta_core::assets::{ImageVariant, ImageVariantBytes};

/// Frame `count` (1-based) may be read at all. Probing applies the same rule
/// to header frame counts.
pub(crate) fn check_frame_count(count: usize) -> Result<(), MediaError> {
    if count > MAX_FRAMES {
        return Err(MediaError::Limit);
    }
    Ok(())
}

/// The composited frames read so far stay within the animation pixel budget.
pub(crate) fn check_animation_pixels(total: u64) -> Result<(), MediaError> {
    if total > MAX_ANIMATION_PIXELS {
        return Err(MediaError::Limit);
    }
    Ok(())
}

/// Animated renditions are recorded loop counts in a 16-bit ANIM field.
pub(crate) fn animation_loops(loops: LoopCount) -> Result<u16, MediaError> {
    match loops {
        LoopCount::Infinite => Ok(0),
        LoopCount::Finite(value) => u16::try_from(value.get()).map_err(|_| MediaError::Limit),
    }
}

pub(super) fn render(
    mut frames: Frames<'_>,
    loops: LoopCount,
    orientation: Orientation,
    transform: Option<pixels::ColorTransform>,
    wanted: Wanted,
    stop: Stop<'_>,
) -> Result<Rendered, MediaError> {
    let mut full = Vec::new();
    let mut preview = Vec::new();
    let mut posters = Vec::new();
    let mut dimensions = (0, 0);
    let mut preview_dimensions = (0, 0);
    let mut total_pixels = 0;
    let mut count = 0;
    // Animated outputs share one byte budget, as each is capped on its own.
    let outputs = usize::from(wanted.full) + usize::from(wanted.preview);
    loop {
        stopped(stop)?;
        let Some(frame) = frames.next() else {
            break;
        };
        count += 1;
        check_frame_count(count)?;
        let frame = frame?;
        let (numerator, denominator) = frame.delay().numer_denom_ms();
        let duration =
            (u64::from(numerator) / u64::from(denominator).max(1)).clamp(1, 0xff_ffff) as u32;
        let mut decoded = DynamicImage::ImageRgba8(frame.into_buffer());
        check_dimensions(decoded.width(), decoded.height())?;
        total_pixels += u64::from(decoded.width()) * u64::from(decoded.height());
        check_animation_pixels(total_pixels)?;
        decoded.apply_orientation(orientation);
        let mut pixels = decoded.into_rgba8();
        pixels::convert_color(&mut pixels, transform.as_ref())?;
        let mut encodings = Encodings::new(&pixels);
        let full_dimensions = pixels::target_dimensions(pixels.dimensions(), ImageVariant::Full);
        let full_encoded = wanted
            .full
            .then(|| encodings.get(ImageVariant::Full))
            .transpose()?;
        if count == 1 {
            dimensions = full_dimensions;
            if wanted.thumbs {
                for variant in THUMBS {
                    stopped(stop)?;
                    posters.push(ImageVariantBytes {
                        variant,
                        bytes: encodings.get(variant)?,
                    });
                }
            }
        } else if dimensions != full_dimensions {
            return Err(MediaError::Invalid);
        }
        if let Some(encoded) = &full_encoded {
            add_frame(&mut full, encoded, dimensions, duration)?;
        }
        if outputs == 0 {
            break; // posters need the first frame only
        }
        if wanted.preview {
            preview_dimensions =
                pixels::target_dimensions(pixels.dimensions(), ImageVariant::Preview);
            let encoded = encodings.get(ImageVariant::Preview)?;
            add_frame(&mut preview, &encoded, preview_dimensions, duration)?;
        }
        if full.len() + preview.len() > MAX_INPUT_BYTES * outputs {
            return Err(MediaError::Limit);
        }
    }
    if count == 0 {
        return Err(MediaError::Invalid);
    }
    if wanted.preview {
        posters.push(ImageVariantBytes {
            variant: ImageVariant::Preview,
            bytes: wrap(preview, preview_dimensions, loops)?,
        });
    }
    let full = wanted
        .full
        .then(|| wrap(full, dimensions, loops))
        .transpose()?;
    Ok(Rendered {
        full,
        variants: posters,
        dimensions,
    })
}

fn u24(value: u32, into: &mut Vec<u8>) {
    into.extend_from_slice(&value.to_le_bytes()[..3]);
}
fn chunk(tag: &[u8; 4], bytes: &[u8], into: &mut Vec<u8>) {
    into.extend_from_slice(tag);
    into.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    into.extend_from_slice(bytes);
    if !bytes.len().is_multiple_of(2) {
        into.push(0);
    }
}

fn add_frame(
    out: &mut Vec<u8>,
    webp: &[u8],
    (width, height): (u32, u32),
    duration: u32,
) -> Result<(), MediaError> {
    let mut frame = vec![0; 6]; // canvas position, x/y = 0
    u24(width - 1, &mut frame);
    u24(height - 1, &mut frame);
    u24(duration, &mut frame);
    frame.push(2); // replace the canvas; the decoder already composited disposal/blending
    let mut offset = 12;
    while offset + 8 <= webp.len() {
        let tag = &webp[offset..offset + 4];
        let len = u32::from_le_bytes(
            webp[offset + 4..offset + 8]
                .try_into()
                .map_err(|_| MediaError::Encode)?,
        ) as usize;
        let end = offset
            .checked_add(8 + len + len % 2)
            .ok_or(MediaError::Encode)?;
        let bytes = webp.get(offset..end).ok_or(MediaError::Encode)?;
        if matches!(tag, b"ALPH" | b"VP8 " | b"VP8L") {
            frame.extend_from_slice(bytes);
        }
        offset = end;
    }
    chunk(b"ANMF", &frame, out);
    Ok(())
}

fn wrap(
    frames: Vec<u8>,
    (width, height): (u32, u32),
    loops: LoopCount,
) -> Result<Vec<u8>, MediaError> {
    let mut data = b"WEBP".to_vec();
    let mut extended = vec![0x12, 0, 0, 0]; // animation + alpha allowed
    u24(width - 1, &mut extended);
    u24(height - 1, &mut extended);
    chunk(b"VP8X", &extended, &mut data);
    let loops = animation_loops(loops)?;
    let mut animation = vec![0; 4];
    animation.extend_from_slice(&loops.to_le_bytes());
    chunk(b"ANIM", &animation, &mut data);
    data.extend(frames);
    let mut result = b"RIFF".to_vec();
    result.extend_from_slice(&(data.len() as u32).to_le_bytes());
    result.extend(data);
    Ok(result)
}
