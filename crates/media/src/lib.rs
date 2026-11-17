//! One bounded, vendor-neutral image kernel: header-only probing for ingestion
//! and WebP rendition encoding for display. No filesystem, network, clock or
//! vendor knowledge enters it.
//!
//! [`probe`] classifies recorded bytes without decoding pixels. [`renditions`]
//! encodes display variants under a [`CodecPermit`]; [`prepare`] encodes a whole
//! legacy bundle (Full plus every variant) in one pooled job, for the live-file
//! cache. All three share one header stage and one encoder path, so a rendition
//! is byte-identical whichever made it.
//!
//! [`probe_video`] classifies recorded MP4 and QuickTime video from its
//! container header alone; no video is ever decoded.
//!
//! @atlas layer kernels · domain media

mod animation;
mod header;
mod pixels;
mod pool;
mod probe;
mod render;
mod video;

use std::fmt;
use std::io::Cursor;
use std::sync::Arc;

use image::ImageReader;
use rosetta_core::ProjectionAsset;
use rosetta_core::assets::{ImageVariant, ImageVariantBytes};
use rosetta_core::timeline::Dimensions;

pub use pool::{CodecPermit, Lane, admit, admitted_jobs, try_admit};
pub use probe::{Probe, probe};
pub use video::{MP4_BRANDS, VideoProbe, probe_video, video_media_type};

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
    /// The caller's stop request ended the work; nothing is wrong with the image.
    Stopped,
}
impl fmt::Display for MediaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Invalid => "image data could not be decoded",
            Self::Unsupported => "image format is not supported",
            Self::Limit => "image exceeds the processing limit",
            Self::Encode => "image could not be encoded",
            Self::Stopped => "image work was stopped",
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

/// The whole WebP bundle, Full and every derived variant, as one foreground
/// codec job (see [`admit`]). Repeated content coalesces and shares a 32 MiB
/// compressed LRU. Callers that need only an image's class and size use
/// [`probe`], which takes no codec slot.
pub fn prepare(bytes: &[u8]) -> Result<Arc<PreparedImage>, MediaError> {
    check_input(bytes)?;
    pool::prepare(bytes)
}

/// The display renditions an image's viewers ask for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenditionSet {
    /// Rail, tile and tile2x, from one decode (of the first frame only for an
    /// animation, whose thumbs are still posters).
    Thumbs,
    /// The Fit preview, at most 1600 px on the long edge; animated for an
    /// animation.
    Preview,
    /// The 1:1 view: the source fitted to WebP's 16383-pixel edge.
    Full,
    /// Thumbs and preview from one decode: every derived variant. Full is
    /// excluded; [`preview_is_full`] tells when preview already is Full.
    All,
}
impl RenditionSet {
    /// The variants `renditions` returns for this set, in this order.
    pub fn variants(self) -> &'static [ImageVariant] {
        match self {
            Self::Thumbs => &render::THUMBS,
            Self::Preview => &[ImageVariant::Preview],
            Self::Full => &[ImageVariant::Full],
            Self::All => &ImageVariant::DERIVED,
        }
    }

    fn wanted(self) -> render::Wanted {
        render::Wanted {
            full: self == Self::Full,
            thumbs: matches!(self, Self::Thumbs | Self::All),
            preview: matches!(self, Self::Preview | Self::All),
        }
    }
}

/// Encode one rendition set, byte-identical to the same variants of
/// [`prepare`] (Full included) for every input `prepare` accepts. Inputs
/// [`probe`] rejects are rejected with its error; no Full encoding is made
/// unless the set asks for it. No cache: the caller's disk is the cache.
pub fn renditions(
    permit: &CodecPermit,
    bytes: &[u8],
    set: RenditionSet,
) -> Result<Vec<ImageVariantBytes>, MediaError> {
    renditions_with_stop(permit, bytes, set, &|| false)
}

/// [`renditions`], polling `stop` before decoding, between animation frames
/// and between encodes; once it returns true the work ends with
/// [`MediaError::Stopped`].
pub fn renditions_with_stop(
    _permit: &CodecPermit,
    bytes: &[u8],
    set: RenditionSet,
    stop: &dyn Fn() -> bool,
) -> Result<Vec<ImageVariantBytes>, MediaError> {
    check_input(bytes)?;
    let header = header::read(bytes)?;
    probe::frames(&header, bytes)?;
    let rendered = render::render(header, set.wanted(), stop)?;
    let out: Vec<_> = rendered
        .full
        .map(|bytes| ImageVariantBytes {
            variant: ImageVariant::Full,
            bytes,
        })
        .into_iter()
        .chain(rendered.variants)
        .collect();
    if out.iter().any(|v| v.bytes.len() > MAX_INPUT_BYTES) {
        return Err(MediaError::Limit);
    }
    Ok(out)
}

/// The size `variant` of a source with these (oriented) dimensions is
/// encoded at.
pub fn target_dimensions(source: Dimensions, variant: ImageVariant) -> Dimensions {
    let (width, height) = pixels::target_dimensions((source.width, source.height), variant);
    Dimensions { width, height }
}

/// True when an image with these [`probe`] or [`prepare`] dimensions needs no
/// preview downscale, so its Preview rendition is byte-identical to its Full
/// one (oriented long edge at most 1600 px after the Full fit).
pub fn preview_is_full(dimensions: Dimensions) -> bool {
    target_dimensions(dimensions, ImageVariant::Preview) == dimensions
}

pub fn dimensions(bytes: &[u8]) -> Result<Dimensions, MediaError> {
    let (width, height) = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| MediaError::Invalid)?
        .into_dimensions()?;
    Ok(Dimensions { width, height })
}

fn check_input(bytes: &[u8]) -> Result<(), MediaError> {
    if bytes.is_empty() {
        return Err(MediaError::Invalid);
    }
    if bytes.len() > MAX_INPUT_BYTES {
        return Err(MediaError::Limit);
    }
    Ok(())
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
    let header = header::read(bytes)?;
    let rendered = render::render(header, render::Wanted::EVERYTHING, &|| false)?;
    let full = rendered.full.ok_or(MediaError::Encode)?;
    prepared(full, rendered.variants, rendered.dimensions)
}

fn prepared(
    bytes: Vec<u8>,
    image_variants: Vec<ImageVariantBytes>,
    (width, height): (u32, u32),
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
