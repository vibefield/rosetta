//! The two pinned properties, over the synthetic corpus:
//! - `probe()` gives `prepare()`'s class and dimensions;
//! - `renditions()` bytes equal `prepare()`'s Full and variant bytes, for
//!   every `RenditionSet`. Each side decodes and encodes on its own, so the
//!   equality also proves the encodings are deterministic.
use super::corpus::{self, Sample};
use crate::{
    Dimensions, Lane, MediaError, PreparedImage, RenditionSet, admit, admitted_jobs, prepare,
    preview_is_full, probe, renditions, renditions_with_stop, target_dimensions,
};
use rosetta_core::assets::ImageVariant;
use std::cell::Cell;
use std::time::Duration;

const SETS: [RenditionSet; 4] = [
    RenditionSet::Thumbs,
    RenditionSet::Preview,
    RenditionSet::Full,
    RenditionSet::All,
];

fn bundled(prepared: &PreparedImage, variant: ImageVariant) -> &[u8] {
    if variant == ImageVariant::Full {
        return &prepared.asset.bytes;
    }
    &prepared
        .asset
        .image_variants
        .iter()
        .find(|v| v.variant == variant)
        .expect("prepare bundles every derived variant")
        .bytes
}

fn named(name: &str) -> &'static [u8] {
    &corpus::decodable()
        .iter()
        .find(|s| s.name == name)
        .unwrap_or_else(|| panic!("no sample {name}"))
        .bytes
}

fn animated(sample: &Sample) -> bool {
    ["gif-", "apng-", "webp-animated"]
        .iter()
        .any(|prefix| sample.name.starts_with(prefix))
}

fn assert_parity(samples: Vec<&Sample>) {
    assert!(!samples.is_empty());
    for sample in samples {
        let name = sample.name;
        let prepared = prepare(&sample.bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
        let probed = probe(&sample.bytes).unwrap_or_else(|e| panic!("{name}: probe {e}"));
        assert_eq!(probed.dimensions, prepared.dimensions, "{name}");
        assert_eq!(
            preview_is_full(prepared.dimensions),
            bundled(&prepared, ImageVariant::Preview) == bundled(&prepared, ImageVariant::Full),
            "{name}: preview_is_full"
        );
        let permit = admit(Lane::Foreground);
        for set in SETS {
            let made = renditions(&permit, &sample.bytes, set)
                .unwrap_or_else(|e| panic!("{name} {set:?}: {e}"));
            let order: Vec<_> = made.iter().map(|v| v.variant).collect();
            assert_eq!(order, set.variants(), "{name} {set:?}");
            for rendition in &made {
                assert!(
                    rendition.bytes == bundled(&prepared, rendition.variant),
                    "{name} {set:?}: {} differs from prepare()",
                    rendition.variant.name()
                );
            }
        }
    }
}

#[test]
fn still_renditions_are_prepares_bytes() {
    assert_parity(
        corpus::decodable()
            .iter()
            .filter(|s| !animated(s))
            .collect(),
    );
}

#[test]
fn animation_renditions_are_prepares_bytes() {
    assert_parity(corpus::decodable().iter().filter(|s| animated(s)).collect());
}

#[test]
fn rejections_agree_and_renditions_reject_what_probe_rejects() {
    for (sample, class) in corpus::rejected() {
        let name = sample.name;
        assert_eq!(prepare(&sample.bytes).err(), Some(class), "{name}: prepare");
        assert_eq!(probe(&sample.bytes).err(), Some(class), "{name}: probe");
        let permit = admit(Lane::Foreground);
        for set in SETS {
            let made = renditions(&permit, &sample.bytes, set);
            assert_eq!(made.err(), Some(class), "{name} {set:?}");
        }
    }
}

/// The documented divergence: only a pixel decode finds broken pixel data.
#[test]
fn corrupt_pixel_data_probes_but_never_renders() {
    for sample in corpus::pixel_corrupt() {
        let name = sample.name;
        let probed = probe(&sample.bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(
            probed.dimensions,
            Dimensions {
                width: 64,
                height: 64
            }
        );
        assert_eq!(
            prepare(&sample.bytes).err(),
            Some(MediaError::Invalid),
            "{name}"
        );
        let permit = admit(Lane::Foreground);
        for set in SETS {
            let made = renditions(&permit, &sample.bytes, set);
            assert_eq!(made.err(), Some(MediaError::Invalid), "{name} {set:?}");
        }
    }
}

/// `prepare()` meets this limit only after encoding two 25 MP frames, so the
/// test runs the header paths alone. Thumbs would decode only the first frame,
/// yet renditions applies the probe's frame limits first.
#[test]
fn animation_pixel_budget_is_enforced_from_headers() {
    let bytes = corpus::gif_canvas_over_animation_limit();
    assert_eq!(probe(&bytes).err(), Some(MediaError::Limit));
    let permit = admit(Lane::Foreground);
    let made = renditions(&permit, &bytes, RenditionSet::Thumbs);
    assert_eq!(made.err(), Some(MediaError::Limit));
}

#[test]
fn probe_reports_the_recorded_type_and_declared_frames() {
    for (name, media_type, frames) in [
        ("png-rgba-400x600", "image/png", 1),
        ("jpeg-exif6", "image/jpeg", 1),
        ("gif-3-frames", "image/gif", 3),
        ("gif-1-frame", "image/gif", 1),
        ("gif-partial-frames", "image/gif", 3),
        ("apng-3-frames", "image/png", 3),
        ("apng-thumbnail", "image/png", 2),
        ("apng-declares-fewer-frames", "image/png", 1),
        ("webp-animated", "image/webp", 3),
        ("webp-lossy", "image/webp", 1),
        ("bmp-rgba", "image/bmp", 1),
        ("ico-48", "image/x-icon", 1),
    ] {
        let probed = probe(named(name)).unwrap();
        assert_eq!(
            (probed.media_type, probed.frames),
            (media_type, frames),
            "{name}"
        );
    }
    let probed = probe(named("jpeg-exif6-17000x24")).unwrap();
    assert_eq!(
        probed.dimensions,
        Dimensions {
            width: 23,
            height: 16383
        }
    );
}

/// Probing never takes a codec slot: it completes with both slots held.
#[test]
fn probe_needs_no_codec_slot() {
    let bytes = named("gif-3-frames");
    let before = admitted_jobs();
    let held = [admit(Lane::Foreground), admit(Lane::Foreground)];
    assert!(admitted_jobs() >= before + 2);
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || sender.send(probe(bytes).map(|p| p.frames)));
    let probed = receiver.recv_timeout(Duration::from_secs(30));
    drop(held);
    assert_eq!(probed.expect("probe waited for a codec slot"), Ok(3));
}

#[test]
fn stop_is_polled_before_each_frame_and_encode() {
    let (gif, still) = (named("gif-3-frames"), named("png-20x80"));
    let permit = admit(Lane::Foreground);
    let polls = Cell::new(0);
    let count = || {
        polls.set(polls.get() + 1);
        false
    };
    for (bytes, set, expected) in [
        (gif, RenditionSet::Preview, 4), // before frames 1, 2, 3 and the end
        (gif, RenditionSet::Full, 4),
        (gif, RenditionSet::Thumbs, 4),   // frame 1, then each poster
        (still, RenditionSet::Thumbs, 4), // the decode, then each variant
        (still, RenditionSet::All, 5),
    ] {
        polls.set(0);
        renditions_with_stop(&permit, bytes, set, &count).unwrap();
        assert_eq!(polls.get(), expected, "{set:?}");
    }
    // A stop after the first frame ends the work between frames.
    polls.set(0);
    let after_first = || {
        polls.set(polls.get() + 1);
        polls.get() > 1
    };
    let made = renditions_with_stop(&permit, gif, RenditionSet::Preview, &after_first);
    assert_eq!(made.err(), Some(MediaError::Stopped));
    assert_eq!(polls.get(), 2);
    for bytes in [gif, still] {
        let made = renditions_with_stop(&permit, bytes, RenditionSet::All, &|| true);
        assert_eq!(made.err(), Some(MediaError::Stopped));
    }
}

#[test]
fn target_dimensions_fit_crop_and_never_upscale() {
    let size = |width, height| Dimensions { width, height };
    for (source, variant, target) in [
        (size(20000, 100), ImageVariant::Full, size(16383, 82)),
        (size(800, 1200), ImageVariant::Full, size(800, 1200)),
        (size(3200, 1000), ImageVariant::Preview, size(1600, 500)),
        (size(800, 1200), ImageVariant::Preview, size(800, 1200)),
        (size(800, 1200), ImageVariant::Tile2x, size(640, 640)),
        (size(100, 50), ImageVariant::Tile, size(50, 50)),
        (size(20, 80), ImageVariant::Rail, size(20, 80)),
        (size(1, 1), ImageVariant::Rail, size(1, 1)),
    ] {
        assert_eq!(target_dimensions(source, variant), target, "{variant:?}");
    }
    assert!(preview_is_full(size(1600, 900)));
    assert!(!preview_is_full(size(1601, 10)));
    assert!(!preview_is_full(size(16383, 82)));
}
