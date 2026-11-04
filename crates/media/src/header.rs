//! The header stage shared by `probe()` and every encoding path: each fallible
//! call made before a pixel is decoded, in one order. Probing and preparing
//! therefore cannot classify an input differently before pixels.
use crate::{MediaError, check_dimensions, limits};
use image::codecs::{gif::GifDecoder, png::PngDecoder, webp::WebPDecoder};
use image::metadata::{LoopCount, Orientation};
use image::{AnimationDecoder, ColorType, Frames, ImageDecoder, ImageFormat, ImageReader};
use std::io::Cursor;

/// ICC profiles above this size are ignored, as unusable metadata.
const MAX_PROFILE_BYTES: usize = 1024 * 1024;

pub(crate) struct Header<'a> {
    pub format: ImageFormat,
    /// The decoder's canvas, before orientation.
    pub width: u32,
    pub height: u32,
    pub orientation: Orientation,
    pub profile: Option<Vec<u8>>,
    pub source: Source<'a>,
}

/// What decodes the pixels. Nothing is decoded until it is read.
pub(crate) enum Source<'a> {
    Still(Box<dyn ImageDecoder + 'a>),
    /// Composited, full-canvas frames, with the recorded loop count and the
    /// decoder's sample type.
    Animation {
        frames: Frames<'a>,
        loops: LoopCount,
        color: ColorType,
    },
}

fn animation<'a>(source: impl AnimationDecoder<'a> + ImageDecoder) -> Source<'a> {
    Source::Animation {
        color: source.color_type(),
        loops: source.loop_count(),
        frames: source.into_frames(),
    }
}

pub(crate) fn read(bytes: &[u8]) -> Result<Header<'_>, MediaError> {
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
    let profile = decoder
        .icc_profile()?
        .filter(|p| p.len() <= MAX_PROFILE_BYTES);
    // Each branch drops the decoder it does not use before returning, so at
    // most one decoder of an image holds buffers while pixels decode.
    let source = match format {
        ImageFormat::Gif => {
            drop(decoder);
            let mut source = GifDecoder::new(Cursor::new(bytes))?;
            source.set_limits(limits())?;
            animation(source)
        }
        ImageFormat::WebP => {
            let mut source = WebPDecoder::new(Cursor::new(bytes))?;
            if source.has_animation() {
                drop(decoder);
                source.set_limits(limits())?;
                animation(source)
            } else {
                drop(source);
                Source::Still(Box::new(decoder))
            }
        }
        ImageFormat::Png => {
            let mut source = PngDecoder::new(Cursor::new(bytes))?;
            if source.is_apng()? {
                drop(decoder);
                source.set_limits(limits())?;
                let color = source.color_type();
                let source = source.apng()?;
                Source::Animation {
                    color,
                    loops: source.loop_count(),
                    frames: source.into_frames(),
                }
            } else {
                drop(source);
                Source::Still(Box::new(decoder))
            }
        }
        _ => Source::Still(Box::new(decoder)),
    };
    Ok(Header {
        format,
        width,
        height,
        orientation,
        profile,
        source,
    })
}

/// The canvas after EXIF orientation: the 90-degree family swaps the edges.
pub(crate) fn oriented(width: u32, height: u32, orientation: Orientation) -> (u32, u32) {
    match orientation {
        Orientation::Rotate90
        | Orientation::Rotate270
        | Orientation::Rotate90FlipH
        | Orientation::Rotate270FlipH => (height, width),
        Orientation::NoTransforms
        | Orientation::Rotate180
        | Orientation::FlipHorizontal
        | Orientation::FlipVertical => (width, height),
    }
}
