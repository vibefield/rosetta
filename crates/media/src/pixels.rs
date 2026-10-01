use crate::MediaError;
use fast_image_resize::{
    FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer,
    images::{Image, ImageRef},
};
use image::RgbaImage;
use moxcms::{ColorProfile, DataColorSpace, Layout, Transform8BitExecutor};
use rosetta_core::assets::ImageVariant;
use std::{borrow::Cow, sync::Arc};

pub(crate) type ColorTransform = Arc<Transform8BitExecutor>;

pub(crate) fn color_transform(profile: Option<&[u8]>) -> Option<ColorTransform> {
    let profile = ColorProfile::new_from_slice(profile?).ok()?;
    // JPEG decoders have already converted CMYK into RGB. An incompatible or
    // malformed profile is ignored as a browser would ignore unusable metadata.
    if profile.color_space != DataColorSpace::Rgb {
        return None;
    }
    profile
        .create_transform_8bit(
            Layout::Rgba,
            &ColorProfile::new_srgb(),
            Layout::Rgba,
            Default::default(),
        )
        .ok()
}

pub(crate) fn convert_color(
    pixels: &mut RgbaImage,
    transform: Option<&ColorTransform>,
) -> Result<(), MediaError> {
    let Some(transform) = transform else {
        return Ok(());
    };
    // One row of temporary memory; no second full-resolution RGBA allocation.
    let stride = pixels.width() as usize * 4;
    let mut row = vec![0; stride];
    for original in pixels.as_mut().chunks_exact_mut(stride) {
        transform
            .transform(original, &mut row)
            .map_err(|_| MediaError::Invalid)?;
        // Alpha is coverage, not a color channel.
        for (before, after) in original.chunks_exact(4).zip(row.chunks_exact_mut(4)) {
            after[3] = before[3];
        }
        original.copy_from_slice(&row);
    }
    Ok(())
}

pub(crate) fn resize(
    source: &RgbaImage,
    variant: ImageVariant,
) -> Result<Cow<'_, RgbaImage>, MediaError> {
    let (width, height) = source.dimensions();
    let edge = variant.edge();
    let (target_width, target_height) = if variant.is_cover() {
        let size = width.min(height).min(edge);
        (size, size)
    } else {
        let scale = (f64::from(edge) / f64::from(width.max(height))).min(1.0);
        (
            (f64::from(width) * scale).round().max(1.0) as u32,
            (f64::from(height) * scale).round().max(1.0) as u32,
        )
    };
    if (width, height) == (target_width, target_height) {
        return Ok(Cow::Borrowed(source));
    }
    let input = ImageRef::new(width, height, source.as_raw(), PixelType::U8x4)
        .map_err(|_| MediaError::Invalid)?;
    let mut output = Image::new(target_width, target_height, PixelType::U8x4);
    let mut options = ResizeOptions::new().resize_alg(ResizeAlg::Convolution(FilterType::Lanczos3));
    if variant.is_cover() {
        options = options.fit_into_destination(Some((0.5, 0.5)));
    }
    // The resizer multiplies/divides alpha, preventing dark transparent edges;
    // NEON/SSE are selected automatically, and no nested Rayon pool is enabled.
    Resizer::new()
        .resize(&input, &mut output, &options)
        .map_err(|_| MediaError::Encode)?;
    RgbaImage::from_raw(target_width, target_height, output.into_vec())
        .map(Cow::Owned)
        .ok_or(MediaError::Encode)
}

pub(crate) fn encode(pixels: &RgbaImage) -> Result<Vec<u8>, MediaError> {
    let mut config = webp::WebPConfig::new().map_err(|_| MediaError::Encode)?;
    config.lossless = 0;
    config.quality = 80.0;
    config.method = 2;
    config.alpha_quality = 100;
    config.use_sharp_yuv = 0;
    config.thread_level = 0;
    config.exact = 1;
    webp::Encoder::from_rgba(pixels.as_raw(), pixels.width(), pixels.height())
        .encode_advanced(&config)
        .map(|bytes| bytes.to_vec())
        .map_err(|_| MediaError::Encode)
}
