//! Pixels to WebP renditions, shared by `prepare()` and `renditions()`. Both
//! run one code path, so a rendition is byte-identical whichever made it.
use crate::header::{Header, Source};
use crate::{MediaError, animation, pixels};
use image::{DynamicImage, RgbaImage};
use rosetta_core::assets::{ImageVariant, ImageVariantBytes};

/// Polled between frames and between encodes; `true` abandons the work.
pub(crate) type Stop<'a> = &'a dyn Fn() -> bool;

pub(crate) fn stopped(stop: Stop<'_>) -> Result<(), MediaError> {
    if stop() {
        Err(MediaError::Stopped)
    } else {
        Ok(())
    }
}

/// Which renditions to encode. Thumbs always travel together: they come from
/// one decode (for animations, of the first frame only).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Wanted {
    pub full: bool,
    pub thumbs: bool,
    pub preview: bool,
}
impl Wanted {
    pub const EVERYTHING: Self = Self {
        full: true,
        thumbs: true,
        preview: true,
    };

    fn has(self, variant: ImageVariant) -> bool {
        match variant {
            ImageVariant::Full => self.full,
            ImageVariant::Preview => self.preview,
            ImageVariant::Rail | ImageVariant::Tile | ImageVariant::Tile2x => self.thumbs,
        }
    }
}

pub(crate) const THUMBS: [ImageVariant; 3] =
    [ImageVariant::Rail, ImageVariant::Tile, ImageVariant::Tile2x];

pub(crate) struct Rendered {
    pub full: Option<Vec<u8>>,
    /// Derived variants in `ImageVariant::DERIVED` order.
    pub variants: Vec<ImageVariantBytes>,
    /// The Full rendition's size: oriented, fitted to WebP's 16383 edge.
    pub dimensions: (u32, u32),
}

/// One source's encodings. A variant that needs no resize is the encoding of
/// the source pixels, made at most once, so it is byte-identical to Full when
/// Full needs no fit either.
pub(crate) struct Encodings<'a> {
    pixels: &'a RgbaImage,
    unscaled: Option<Vec<u8>>,
}
impl<'a> Encodings<'a> {
    pub fn new(pixels: &'a RgbaImage) -> Self {
        Self {
            pixels,
            unscaled: None,
        }
    }

    pub fn get(&mut self, variant: ImageVariant) -> Result<Vec<u8>, MediaError> {
        let resized = pixels::resize(self.pixels, variant)?;
        if resized.dimensions() != self.pixels.dimensions() {
            return pixels::encode(&resized);
        }
        if let Some(bytes) = &self.unscaled {
            return Ok(bytes.clone());
        }
        let bytes = pixels::encode(self.pixels)?;
        self.unscaled = Some(bytes.clone());
        Ok(bytes)
    }
}

pub(crate) fn render(
    header: Header<'_>,
    wanted: Wanted,
    stop: Stop<'_>,
) -> Result<Rendered, MediaError> {
    let transform = pixels::color_transform(header.profile.as_deref());
    match header.source {
        Source::Still(decoder) => {
            stopped(stop)?;
            let mut decoded = DynamicImage::from_decoder(decoder)?;
            decoded.apply_orientation(header.orientation);
            let mut pixels = decoded.into_rgba8();
            pixels::convert_color(&mut pixels, transform.as_ref())?;
            still(&pixels, wanted, stop)
        }
        Source::Animation { frames, loops, .. } => {
            animation::render(frames, loops, header.orientation, transform, wanted, stop)
        }
    }
}

fn still(pixels: &RgbaImage, wanted: Wanted, stop: Stop<'_>) -> Result<Rendered, MediaError> {
    let mut encodings = Encodings::new(pixels);
    // WebP has a 16383-pixel edge limit. Fit exceptional long captures rather
    // than crop away content or make a native encoder allocation overflow.
    let full = wanted
        .full
        .then(|| encodings.get(ImageVariant::Full))
        .transpose()?;
    let mut variants = Vec::new();
    for variant in ImageVariant::DERIVED.into_iter().filter(|v| wanted.has(*v)) {
        stopped(stop)?;
        variants.push(ImageVariantBytes {
            variant,
            bytes: encodings.get(variant)?,
        });
    }
    Ok(Rendered {
        full,
        variants,
        dimensions: pixels::target_dimensions(pixels.dimensions(), ImageVariant::Full),
    })
}
