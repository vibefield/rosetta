//! One bounded, vendor-neutral WebP pipeline. Original bytes remain ledger-only.
//! No filesystem, network, clock or vendor knowledge enters image preparation.
//!
//! @atlas layer kernels · domain media

mod animation;
mod pixels;
mod pool;

use std::fmt;
use std::io::Cursor;
use std::sync::Arc;

use image::{AnimationDecoder, DynamicImage, ImageDecoder, ImageFormat, ImageReader};
use rosetta_core::ProjectionAsset;
use rosetta_core::assets::{ImageVariant, ImageVariantBytes};
use rosetta_core::timeline::Dimensions;

pub const MAX_INPUT_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_PIXELS: u64 = 32 * 1024 * 1024;
pub const MAX_ANIMATION_PIXELS: u64 = 64 * 1024 * 1024;
pub const MAX_FRAMES: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaError {
    Invalid,
    Unsupported,
    Limit,
    Encode,
}
impl fmt::Display for MediaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Invalid => "image data could not be decoded",
            Self::Unsupported => "image format is not supported",
            Self::Limit => "image exceeds the processing limit",
            Self::Encode => "image could not be encoded",
        })
    }
}
impl std::error::Error for MediaError {}
impl From<image::ImageError> for MediaError {
    fn from(_: image::ImageError) -> Self {
        Self::Invalid
    }
}

#[derive(Clone, Debug)]
pub struct PreparedImage {
    pub asset: ProjectionAsset,
    pub dimensions: Dimensions,
}

/// At most two jobs decode/resize/encode at once, including ingestion and live
/// file requests. Repeated content coalesces and shares a 32 MiB compressed LRU.
pub fn prepare(bytes: &[u8]) -> Result<Arc<PreparedImage>, MediaError> {
    if bytes.is_empty() {
        return Err(MediaError::Invalid);
    }
    if bytes.len() > MAX_INPUT_BYTES {
        return Err(MediaError::Limit);
    }
    pool::prepare(bytes)
}

pub fn dimensions(bytes: &[u8]) -> Result<Dimensions, MediaError> {
    let (width, height) = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| MediaError::Invalid)?
        .into_dimensions()?;
    Ok(Dimensions { width, height })
}

fn limits() -> image::Limits {
    let mut value = image::Limits::default();
    value.max_image_width = Some(65536);
    value.max_image_height = Some(65536);
    value.max_alloc = Some(MAX_PIXELS * 8);
    value
}

fn check_dimensions(width: u32, height: u32) -> Result<(), MediaError> {
    if width == 0 || height == 0 || u64::from(width) * u64::from(height) > MAX_PIXELS {
        return Err(MediaError::Limit);
    }
    Ok(())
}

fn prepare_uncached(bytes: &[u8]) -> Result<PreparedImage, MediaError> {
    let format = image::guess_format(bytes).map_err(|_| MediaError::Unsupported)?;
    if !matches!(
        format,
        ImageFormat::Png
            | ImageFormat::Jpeg
            | ImageFormat::Gif
            | ImageFormat::WebP
            | ImageFormat::Bmp
            | ImageFormat::Ico
    ) {
        return Err(MediaError::Unsupported);
    }
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    reader.limits(limits());
    let mut decoder = reader.into_decoder()?;
    let (width, height) = decoder.dimensions();
    check_dimensions(width, height)?;
    let orientation = decoder.orientation()?;
    let profile = decoder.icc_profile()?.filter(|p| p.len() <= 1024 * 1024);
    let transform = pixels::color_transform(profile.as_deref());
    // Animated decoders return composited, full-canvas frames. Keep frame timing
    // and loop count; only rail/tile renditions are still posters.
    match format {
        ImageFormat::Gif => {
            drop(decoder);
            let mut source = image::codecs::gif::GifDecoder::new(Cursor::new(bytes))?;
            source.set_limits(limits())?;
            let loops = source.loop_count();
            return animation::prepare(source.into_frames(), loops, orientation, transform);
        }
        ImageFormat::WebP => {
            let mut source = image::codecs::webp::WebPDecoder::new(Cursor::new(bytes))?;
            if source.has_animation() {
                drop(decoder);
                source.set_limits(limits())?;
                let loops = source.loop_count();
                return animation::prepare(source.into_frames(), loops, orientation, transform);
            }
        }
        ImageFormat::Png => {
            let mut source = image::codecs::png::PngDecoder::new(Cursor::new(bytes))?;
            if source.is_apng()? {
                drop(decoder);
                source.set_limits(limits())?;
                let source = source.apng()?;
                let loops = source.loop_count();
                return animation::prepare(source.into_frames(), loops, orientation, transform);
            }
        }
        _ => {}
    }
    let mut decoded = DynamicImage::from_decoder(decoder)?;
    decoded.apply_orientation(orientation);
    let mut pixels = decoded.into_rgba8();
    pixels::convert_color(&mut pixels, transform.as_ref())?;
    // WebP has a 16383-pixel edge limit. Fit exceptional long captures rather
    // than crop away content or make a native encoder allocation overflow.
    let full_pixels = pixels::resize(&pixels, ImageVariant::Full)?;
    let (width, height) = full_pixels.dimensions();
    let full = pixels::encode(&full_pixels)?;
    let variants = variants(
        &pixels,
        (pixels.dimensions() == (width, height)).then_some(full.as_slice()),
        ImageVariant::DERIVED,
    )?;
    prepared(full, variants, width, height)
}

fn variants(
    pixels: &image::RgbaImage,
    original_encoding: Option<&[u8]>,
    recipes: impl IntoIterator<Item = ImageVariant>,
) -> Result<Vec<ImageVariantBytes>, MediaError> {
    recipes
        .into_iter()
        .map(|variant| {
            let resized = pixels::resize(pixels, variant)?;
            Ok(ImageVariantBytes {
                variant,
                bytes: if resized.dimensions() == pixels.dimensions()
                    && let Some(encoded) = original_encoding
                {
                    encoded.to_vec()
                } else {
                    pixels::encode(&resized)?
                },
            })
        })
        .collect()
}

fn prepared(
    bytes: Vec<u8>,
    image_variants: Vec<ImageVariantBytes>,
    width: u32,
    height: u32,
) -> Result<PreparedImage, MediaError> {
    if bytes.len() > MAX_INPUT_BYTES
        || image_variants
            .iter()
            .any(|v| v.bytes.len() > MAX_INPUT_BYTES)
    {
        return Err(MediaError::Limit);
    }
    let mut asset = ProjectionAsset::new("image/webp", bytes).ok_or(MediaError::Encode)?;
    asset.image_variants = image_variants;
    Ok(PreparedImage {
        asset,
        dimensions: Dimensions { width, height },
    })
}

#[cfg(test)]
mod tests;
