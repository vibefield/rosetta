//! Assemble full-canvas WebP frames encoded by libwebp. Keeping the recorded
//! ANMF durations explicitly avoids encoder heuristics changing the last delay.
use crate::{
    MAX_ANIMATION_PIXELS, MAX_FRAMES, MAX_INPUT_BYTES, MediaError, PreparedImage, check_dimensions,
    pixels, prepared, variants,
};
use image::{
    DynamicImage, Frames,
    metadata::{LoopCount, Orientation},
};
use rosetta_core::assets::{ImageVariant, ImageVariantBytes};

pub(super) fn prepare(
    frames: Frames<'_>,
    loops: LoopCount,
    orientation: Orientation,
    transform: Option<pixels::ColorTransform>,
) -> Result<PreparedImage, MediaError> {
    let mut full = Vec::new();
    let mut preview = Vec::new();
    let mut posters = None;
    let mut dimensions = (0, 0);
    let mut preview_dimensions = (0, 0);
    let mut total_pixels = 0;
    let mut count = 0;
    for frame in frames {
        count += 1;
        if count > MAX_FRAMES {
            return Err(MediaError::Limit);
        }
        let frame = frame?;
        let (numerator, denominator) = frame.delay().numer_denom_ms();
        let duration =
            (u64::from(numerator) / u64::from(denominator).max(1)).clamp(1, 0xff_ffff) as u32;
        let mut decoded = DynamicImage::ImageRgba8(frame.into_buffer());
        check_dimensions(decoded.width(), decoded.height())?;
        total_pixels += u64::from(decoded.width()) * u64::from(decoded.height());
        if total_pixels > MAX_ANIMATION_PIXELS {
            return Err(MediaError::Limit);
        }
        decoded.apply_orientation(orientation);
        let mut pixels = decoded.into_rgba8();
        pixels::convert_color(&mut pixels, transform.as_ref())?;
        let full_pixels = pixels::resize(&pixels, ImageVariant::Full)?;
        let full_encoded = pixels::encode(&full_pixels)?;
        if count == 1 {
            dimensions = full_pixels.dimensions();
            posters = Some(variants(
                &pixels,
                (pixels.dimensions() == dimensions).then_some(full_encoded.as_slice()),
                ImageVariant::DERIVED
                    .into_iter()
                    .filter(|variant| *variant != ImageVariant::Preview),
            )?);
        } else if dimensions != full_pixels.dimensions() {
            return Err(MediaError::Invalid);
        }
        add_frame(&mut full, &full_encoded, dimensions, duration)?;
        let pixels = pixels::resize(&pixels, ImageVariant::Preview)?;
        preview_dimensions = pixels.dimensions();
        let preview_encoded = if preview_dimensions == dimensions {
            full_encoded
        } else {
            pixels::encode(&pixels)?
        };
        add_frame(&mut preview, &preview_encoded, preview_dimensions, duration)?;
        if full.len() + preview.len() > MAX_INPUT_BYTES * 2 {
            return Err(MediaError::Limit);
        }
    }
    let mut posters = posters.ok_or(MediaError::Invalid)?;
    let preview = wrap(preview, preview_dimensions, loops)?;
    posters.push(ImageVariantBytes {
        variant: ImageVariant::Preview,
        bytes: preview,
    });
    prepared(
        wrap(full, dimensions, loops)?,
        posters,
        dimensions.0,
        dimensions.1,
    )
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
    let loops = match loops {
        LoopCount::Infinite => 0,
        LoopCount::Finite(value) => u16::try_from(value.get()).map_err(|_| MediaError::Limit)?,
    };
    let mut animation = vec![0; 4];
    animation.extend_from_slice(&loops.to_le_bytes());
    chunk(b"ANIM", &animation, &mut data);
    data.extend(frames);
    let mut result = b"RIFF".to_vec();
    result.extend_from_slice(&(data.len() as u32).to_le_bytes());
    result.extend(data);
    Ok(result)
}
