use super::*;
use image::{AnimationDecoder, DynamicImage, ImageFormat, Rgba, RgbaImage};
use rosetta_core::assets::ImageVariant;
use std::{io::Cursor, sync::Arc};

fn png(width: u32, height: u32) -> Vec<u8> {
    let image = RgbaImage::from_fn(width, height, |x, y| {
        Rgba([
            (x % 253) as u8,
            (y % 251) as u8,
            100,
            if x < width / 2 { 128 } else { 255 },
        ])
    });
    let mut bytes = Cursor::new(Vec::new());
    DynamicImage::ImageRgba8(image)
        .write_to(&mut bytes, ImageFormat::Png)
        .unwrap();
    bytes.into_inner()
}

#[test]
fn full_and_display_variants_have_truthful_dimensions_alpha_and_metadata() {
    let input = png(800, 1200);
    let output = prepare(&input).unwrap();
    assert_eq!(output.asset.media_type, "image/webp");
    assert_eq!(
        output.asset.digest,
        blake3::hash(&output.asset.bytes).to_hex().to_string()
    );
    assert_eq!(
        output.dimensions,
        Dimensions {
            width: 800,
            height: 1200
        }
    );
    let decoded = image::load_from_memory(&output.asset.bytes)
        .unwrap()
        .to_rgba8();
    assert_eq!(decoded.get_pixel(100, 100)[3], 128);
    assert_eq!(decoded.get_pixel(700, 100)[3], 255);
    for member in &output.asset.image_variants {
        let size = dimensions(&member.bytes).unwrap();
        assert!(size.width.max(size.height) <= member.variant.edge());
        if member.variant.is_cover() {
            assert_eq!(size.width, size.height);
        } else {
            assert!((f64::from(size.width) / f64::from(size.height) - 2.0 / 3.0).abs() < 0.01);
        }
    }
    assert_eq!(output.asset.image_variants.len(), 4);
    let repeated = prepare(&input).unwrap();
    assert!(Arc::ptr_eq(&output, &repeated));
    assert!(!format!("{:?}", output.asset).contains("RIFF"));
}

#[test]
fn malformed_and_oversized_inputs_fail_before_publication() {
    assert_eq!(
        prepare(b"\x89PNG\r\n\x1a\nheader only").unwrap_err(),
        MediaError::Invalid
    );
    assert_eq!(
        prepare(&vec![0; MAX_INPUT_BYTES + 1]).unwrap_err(),
        MediaError::Limit
    );
    assert!(prepare(b"not an image").is_err());
    assert_eq!(check_dimensions(50000, 50000), Err(MediaError::Limit));
}

#[test]
fn small_sources_are_not_upscaled_and_cover_never_leaks_into_previews() {
    let result = prepare(&png(20, 80)).unwrap();
    for member in &result.asset.image_variants {
        assert_eq!(
            dimensions(&member.bytes).unwrap(),
            if member.variant.is_cover() {
                Dimensions {
                    width: 20,
                    height: 20,
                }
            } else {
                Dimensions {
                    width: 20,
                    height: 80,
                }
            }
        );
    }
}

#[test]
fn concurrent_occurrences_share_one_prepared_content_identity() {
    let bytes = Arc::new(png(213, 157));
    let workers: Vec<_> = (0..8)
        .map(|_| {
            let bytes = bytes.clone();
            std::thread::spawn(move || prepare(&bytes).unwrap())
        })
        .collect();
    let outputs: Vec<_> = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect();
    assert!(
        outputs
            .iter()
            .all(|output| Arc::ptr_eq(output, &outputs[0]))
    );
}

#[test]
fn jpeg_orientation_is_applied_before_resizing() {
    let pixels = image::RgbImage::from_pixel(30, 50, image::Rgb([220, 70, 20]));
    let mut jpeg = Cursor::new(Vec::new());
    DynamicImage::ImageRgb8(pixels)
        .write_to(&mut jpeg, ImageFormat::Jpeg)
        .unwrap();
    // Little-endian TIFF IFD with orientation 6 (90 degrees clockwise).
    let exif = b"Exif\0\0II\x2a\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0\x06\0\0\0\0\0\0\0";
    let mut bytes = vec![0xff, 0xd8, 0xff, 0xe1];
    bytes.extend_from_slice(&((exif.len() + 2) as u16).to_be_bytes());
    bytes.extend_from_slice(exif);
    bytes.extend_from_slice(&jpeg.into_inner()[2..]);
    let result = prepare(&bytes).unwrap();
    assert_eq!(
        result.dimensions,
        Dimensions {
            width: 50,
            height: 30
        }
    );
}

#[test]
fn animations_keep_frame_delays_and_loop_count_while_tiles_are_posters() {
    let mut bytes = Vec::new();
    {
        let mut encoder = image::codecs::gif::GifEncoder::new(&mut bytes);
        encoder
            .set_repeat(image::codecs::gif::Repeat::Finite(3))
            .unwrap();
        for (color, delay) in [([240, 10, 10, 255], 120), ([10, 30, 240, 255], 340)] {
            encoder
                .encode_frame(image::Frame::from_parts(
                    RgbaImage::from_pixel(40, 30, Rgba(color)),
                    0,
                    0,
                    image::Delay::from_numer_denom_ms(delay, 1),
                ))
                .unwrap();
        }
    }
    let result = prepare(&bytes).unwrap();
    let decoded = image::codecs::webp::WebPDecoder::new(Cursor::new(&result.asset.bytes)).unwrap();
    assert!(decoded.has_animation());
    assert!(
        matches!(decoded.loop_count(), image::metadata::LoopCount::Finite(value) if value.get() == 3)
    );
    let reencoded = prepare(&result.asset.bytes).unwrap();
    let replayed = image::codecs::webp::WebPDecoder::new(Cursor::new(&reencoded.asset.bytes))
        .unwrap()
        .into_frames()
        .collect_frames()
        .unwrap();
    assert_eq!(replayed.len(), 2);
    assert_eq!(replayed[1].delay().numer_denom_ms(), (340, 1));
    let frames = decoded.into_frames().collect_frames().unwrap();
    assert_eq!(frames.len(), 2);
    assert_eq!(frames[0].delay().numer_denom_ms(), (120, 1));
    assert_eq!(frames[1].delay().numer_denom_ms(), (340, 1));
    for member in &result.asset.image_variants {
        let decoder = image::codecs::webp::WebPDecoder::new(Cursor::new(&member.bytes)).unwrap();
        assert_eq!(
            decoder.has_animation(),
            member.variant == ImageVariant::Preview
        );
    }
}

#[test]
fn display_p3_is_converted_to_srgb_before_metadata_is_removed() {
    use image::ImageEncoder;
    let profile = moxcms::ColorProfile::new_display_p3().encode().unwrap();
    let pixels = RgbaImage::from_pixel(32, 32, Rgba([160, 70, 30, 128]));
    let mut encoded = Vec::new();
    let mut encoder = image::codecs::png::PngEncoder::new(&mut encoded);
    encoder.set_icc_profile(profile).unwrap();
    encoder
        .write_image(pixels.as_raw(), 32, 32, image::ExtendedColorType::Rgba8)
        .unwrap();
    let prepared = prepare(&encoded).unwrap();
    let decoded = image::load_from_memory(&prepared.asset.bytes)
        .unwrap()
        .to_rgba8();
    let pixel = decoded.get_pixel(16, 16);
    assert!(
        pixel[0] > 170 && pixel[1] < 65,
        "P3 was relabelled instead of converted: {pixel:?}"
    );
    assert_eq!(pixel[3], 128);
    let mut decoder =
        image::codecs::webp::WebPDecoder::new(Cursor::new(&prepared.asset.bytes)).unwrap();
    assert!(decoder.icc_profile().unwrap().is_none());
}
