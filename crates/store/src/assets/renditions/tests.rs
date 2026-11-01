use super::*;
use std::collections::BTreeSet;
use std::io::Cursor;
use std::sync::Mutex as TestMutex;

/// Serializes the tests that count codec jobs or hold a codec slot.
static CODEC: TestMutex<()> = TestMutex::new(());

fn codec() -> std::sync::MutexGuard<'static, ()> {
    CODEC.lock().unwrap_or_else(|p| p.into_inner())
}

pub(crate) fn png(width: u32, height: u32) -> Vec<u8> {
    let pixels = image::RgbaImage::from_fn(width, height, |x, y| {
        image::Rgba([(x % 251) as u8, (y % 241) as u8, ((x + y) % 239) as u8, 255])
    });
    let mut bytes = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(pixels)
        .write_to(&mut bytes, image::ImageFormat::Png)
        .unwrap();
    bytes.into_inner()
}

fn gif(frames: u32) -> Vec<u8> {
    let mut bytes = Vec::new();
    {
        let mut encoder = image::codecs::gif::GifEncoder::new(&mut bytes);
        for index in 0..frames {
            let pixels =
                image::RgbaImage::from_pixel(64, 48, image::Rgba([index as u8 * 40, 90, 160, 255]));
            encoder.encode_frame(image::Frame::new(pixels)).unwrap();
        }
    }
    bytes
}

fn member(root: &Path, bytes: &[u8], extension: &str) -> String {
    fs::create_dir_all(root).unwrap();
    let name = format!("{}.{extension}", blake3::hash(bytes).to_hex());
    fs::write(root.join(&name), bytes).unwrap();
    name
}

fn names(root: &Path) -> BTreeSet<String> {
    fs::read_dir(root)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect()
}

fn cache(name: &str, variant: ImageVariant) -> String {
    variant.file_name(&name[..64])
}

fn prepared(bytes: &[u8], variant: ImageVariant) -> Vec<u8> {
    let prepared = rosetta_media::prepare(bytes).unwrap();
    if variant == ImageVariant::Full {
        return prepared.asset.bytes.clone();
    }
    prepared
        .asset
        .image_variants
        .iter()
        .find(|v| v.variant == variant)
        .unwrap()
        .bytes
        .clone()
}

fn background() -> CodecPermit {
    loop {
        if let Some(permit) = rosetta_media::try_admit(Lane::Background) {
            return permit;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[cfg(unix)]
fn inode(path: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt;
    fs::metadata(path).unwrap().ino()
}

/// Root ignores directory permissions; skip the read-only checks there.
#[cfg(unix)]
fn read_only(root: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(root, fs::Permissions::from_mode(0o555)).unwrap();
    if fs::write(root.join("probe"), b"x").is_ok() {
        let _ = fs::remove_file(root.join("probe"));
        fs::set_permissions(root, fs::Permissions::from_mode(0o755)).unwrap();
        return false;
    }
    true
}

#[test]
fn a_tile_request_writes_the_thumb_set_once() {
    let _codec = codec();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let source = png(400, 300);
    let name = member(root, &source, "png");
    let first = rendition(root, &name, ImageVariant::Tile, false).unwrap();
    assert_eq!(first.bytes, prepared(&source, ImageVariant::Tile));
    assert_eq!(first.files_written, 3);
    assert!(first.cache_error.is_none());
    let expected: BTreeSet<_> = [
        name.clone(),
        cache(&name, ImageVariant::Rail),
        cache(&name, ImageVariant::Tile2x),
        cache(&name, ImageVariant::Tile),
    ]
    .into();
    assert_eq!(names(root), expected);
    for variant in [ImageVariant::Rail, ImageVariant::Tile2x] {
        assert_eq!(
            cached(root, &name[..64], variant).unwrap(),
            prepared(&source, variant)
        );
    }
    let jobs = rosetta_media::admitted_jobs();
    let second = rendition(root, &name, ImageVariant::Rail, false).unwrap();
    assert_eq!(second.files_written, 0);
    assert_eq!(
        rosetta_media::admitted_jobs(),
        jobs,
        "a hit takes no codec slot"
    );
    assert_eq!(second.bytes, prepared(&source, ImageVariant::Rail));
}

#[test]
fn concurrent_first_views_of_one_image_encode_once() {
    let _codec = codec();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_owned();
    let source = png(700, 500);
    let name = member(&root, &source, "png");
    let jobs = rosetta_media::admitted_jobs();
    let views: Vec<_> = [ImageVariant::Tile, ImageVariant::Tile2x, ImageVariant::Rail]
        .into_iter()
        .flat_map(|variant| [variant, variant])
        .map(|variant| {
            let (root, name) = (root.clone(), name.clone());
            std::thread::spawn(move || rendition(&root, &name, variant, false).unwrap())
        })
        .collect();
    let written: u64 = views
        .into_iter()
        .map(|v| v.join().unwrap().files_written)
        .sum();
    assert_eq!(rosetta_media::admitted_jobs(), jobs + 1);
    assert_eq!(written, 3);
}

#[test]
fn tile_is_written_last() {
    let _codec = codec();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let name = member(root, &png(300, 300), "png");
    // An unwritable tile2x name stops the set before its completion marker.
    fs::create_dir(root.join(cache(&name, ImageVariant::Tile2x))).unwrap();
    let served = rendition(root, &name, ImageVariant::Tile, false).unwrap();
    assert!(served.cache_error.is_some());
    assert_eq!(served.files_written, 1);
    assert!(root.join(cache(&name, ImageVariant::Rail)).is_file());
    assert!(!root.join(cache(&name, ImageVariant::Tile)).exists());
}

#[cfg(unix)]
#[test]
fn preview_and_full_are_one_file_when_no_downscale_is_needed() {
    let _codec = codec();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let source = png(400, 800);
    let name = member(root, &source, "png");
    let full = prepared(&source, ImageVariant::Full);
    assert_eq!(prepared(&source, ImageVariant::Preview), full);
    let preview = rendition(root, &name, ImageVariant::Preview, false).unwrap();
    assert_eq!(preview.bytes, full);
    assert_eq!(preview.files_written, 2);
    let (preview_path, full_path) = (
        root.join(cache(&name, ImageVariant::Preview)),
        root.join(cache(&name, ImageVariant::Full)),
    );
    assert_eq!(inode(&preview_path), inode(&full_path));
    let hit = rendition(root, &name, ImageVariant::Full, false).unwrap();
    assert_eq!((hit.bytes, hit.files_written), (full.clone(), 0));

    // A lost name is linked again from the other one without an encode.
    fs::remove_file(&full_path).unwrap();
    let jobs = rosetta_media::admitted_jobs();
    let relinked = rendition(root, &name, ImageVariant::Full, false).unwrap();
    assert_eq!((relinked.bytes, relinked.files_written), (full, 1));
    assert_eq!(rosetta_media::admitted_jobs(), jobs);
    assert_eq!(inode(&preview_path), inode(&full_path));
}

#[test]
fn a_large_image_caches_preview_and_its_lazy_full_apart() {
    let _codec = codec();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let source = png(2000, 300);
    let name = member(root, &source, "png");
    let preview = rendition(root, &name, ImageVariant::Preview, false).unwrap();
    assert_eq!(preview.bytes, prepared(&source, ImageVariant::Preview));
    assert_eq!(preview.files_written, 1);
    assert!(!root.join(cache(&name, ImageVariant::Full)).exists());
    let full = rendition(root, &name, ImageVariant::Full, false).unwrap();
    assert_eq!(full.bytes, prepared(&source, ImageVariant::Full));
    assert_eq!(full.files_written, 1);
    assert_ne!(full.bytes, preview.bytes);
}

#[test]
fn a_torn_cache_file_is_a_miss_and_is_repaired() {
    let _codec = codec();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let source = png(200, 200);
    let name = member(root, &source, "png");
    let tile = rendition(root, &name, ImageVariant::Tile, false)
        .unwrap()
        .bytes;
    let path = root.join(cache(&name, ImageVariant::Tile));
    for torn in [&tile[..tile.len() / 2], &[][..]] {
        fs::write(&path, torn).unwrap();
        assert!(cached(root, &name[..64], ImageVariant::Tile).is_none());
        let repaired = rendition(root, &name, ImageVariant::Tile, false).unwrap();
        assert_eq!(repaired.bytes, tile);
        assert_eq!(fs::read(&path).unwrap(), tile);
    }
    // Retry replaces a cached file even when it looks valid.
    let retried = rendition(root, &name, ImageVariant::Tile, true).unwrap();
    assert_eq!((retried.bytes, retried.files_written), (tile, 3));
}

#[test]
fn base_failures_are_classified_before_any_decode() {
    let _codec = codec();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let source = png(32, 32);
    let missing = format!("{}.png", blake3::hash(&source).to_hex());
    assert!(matches!(
        rendition(root, &missing, ImageVariant::Tile, false),
        Err(RenditionError::BaseMissing)
    ));
    fs::create_dir_all(root).unwrap();
    fs::write(root.join(&missing), png(33, 32)).unwrap();
    assert!(matches!(
        rendition(root, &missing, ImageVariant::Tile, false),
        Err(RenditionError::BaseCorrupt)
    ));
    let text = member(root, b"hello", "txt");
    assert!(matches!(
        rendition(root, &text, ImageVariant::Tile, false),
        Err(RenditionError::Media(MediaError::Unsupported))
    ));
    let tiff = member(root, b"II*\0not really a tiff", "png");
    assert!(matches!(
        rendition(root, &tiff, ImageVariant::Tile, false),
        Err(RenditionError::Media(MediaError::Unsupported))
    ));
    assert_eq!(
        names(root),
        [missing, text, tiff].into_iter().collect::<BTreeSet<_>>(),
        "no cache file for a failed base"
    );
}

#[cfg(unix)]
#[test]
fn a_failed_cache_write_still_serves_the_bytes() {
    use std::os::unix::fs::PermissionsExt;
    let _codec = codec();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("assets");
    let source = png(120, 90);
    let name = member(&root, &source, "png");
    if !read_only(&root) {
        return;
    }
    let served = rendition(&root, &name, ImageVariant::Tile, false);
    fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
    let served = served.unwrap();
    assert_eq!(served.bytes, prepared(&source, ImageVariant::Tile));
    assert!(served.cache_error.is_some());
    assert_eq!(served.files_written, 0);
    assert_eq!(names(&root).len(), 1);
}

#[test]
fn completeness_is_a_stat_of_the_cache_alone() {
    let _codec = codec();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let small = png(300, 200);
    let large = png(1700, 100);
    let small_name = member(root, &small, "png");
    let large_name = member(root, &large, "png");
    let small_dims = rosetta_media::probe(&small).unwrap().dimensions;
    let large_dims = rosetta_media::probe(&large).unwrap().dimensions;
    assert!(!is_complete(root, &small_name, Some(small_dims)));
    for (name, dims) in [(&small_name, small_dims), (&large_name, large_dims)] {
        let permit = background();
        assert!(matches!(
            ensure(&permit, root, name, Some(dims), &|| false).unwrap(),
            Ensured::Made { .. }
        ));
    }
    fs::remove_file(root.join(&small_name)).unwrap();
    fs::remove_file(root.join(&large_name)).unwrap();
    assert!(is_complete(root, &small_name, Some(small_dims)));
    assert!(is_complete(root, &large_name, Some(large_dims)));
    assert!(!root.join(cache(&large_name, ImageVariant::Full)).exists());
    fs::remove_file(root.join(cache(&small_name, ImageVariant::Full))).unwrap();
    assert!(!is_complete(root, &small_name, Some(small_dims)));
    assert!(is_complete(root, &small_name, None));
    fs::remove_file(root.join(cache(&large_name, ImageVariant::Rail))).unwrap();
    assert!(!is_complete(root, &large_name, Some(large_dims)));
}

#[test]
fn stale_tempfiles_are_removed_and_fresh_ones_kept() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let old = SystemTime::now() - Duration::from_secs(3600);
    for name in [".image-old", ".rosetta-asset-old"] {
        fs::write(root.join(name), b"partial").unwrap();
        File::options()
            .write(true)
            .open(root.join(name))
            .unwrap()
            .set_modified(old)
            .unwrap();
    }
    fs::write(root.join(".image-new"), b"in flight").unwrap();
    fs::create_dir(root.join(".image-dir")).unwrap();
    assert_eq!(
        remove_stale_tempfiles(root, Duration::from_secs(600)).unwrap(),
        1
    );
    let left = names(root);
    assert!(!left.contains(".image-old"));
    assert!(left.contains(".image-new") && left.contains(".rosetta-asset-old"));
    assert!(left.contains(".image-dir"));
    assert_eq!(
        remove_stale_tempfiles(&root.join("absent"), Duration::ZERO).unwrap(),
        0
    );
}

#[test]
fn the_sweep_skips_an_image_a_viewer_is_making() {
    let _codec = codec();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let source = png(500, 400);
    let name = member(root, &source, "png");
    let dims = rosetta_media::probe(&source).unwrap().dimensions;
    let held = base_lock(&root.join(&name));
    let guard = held.lock().unwrap();
    let permit = background();
    assert_eq!(
        ensure(&permit, root, &name, Some(dims), &|| false).unwrap(),
        Ensured::Busy
    );
    drop(guard);
    let jobs = rosetta_media::admitted_jobs();
    let Ensured::Made { files, bytes } =
        ensure(&permit, root, &name, Some(dims), &|| false).unwrap()
    else {
        panic!("the sweep made nothing");
    };
    assert_eq!(
        rosetta_media::admitted_jobs(),
        jobs,
        "the sweep's own permit"
    );
    // rail, tile2x, preview, the full link, tile
    assert_eq!(files, 5);
    for variant in [
        ImageVariant::Rail,
        ImageVariant::Tile,
        ImageVariant::Tile2x,
        ImageVariant::Preview,
        ImageVariant::Full,
    ] {
        assert_eq!(
            cached(root, &name[..64], variant).unwrap(),
            prepared(&source, variant),
            "{variant:?}"
        );
    }
    let total: u64 = [
        ImageVariant::Rail,
        ImageVariant::Tile,
        ImageVariant::Tile2x,
        ImageVariant::Preview,
    ]
    .into_iter()
    .map(|v| fs::metadata(root.join(cache(&name, v))).unwrap().len())
    .sum();
    assert_eq!(bytes, total, "a link writes no bytes");
    assert_eq!(
        ensure(&permit, root, &name, Some(dims), &|| false).unwrap(),
        Ensured::Complete
    );
}

#[test]
fn the_sweep_makes_only_what_is_missing() {
    let _codec = codec();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let source = png(1800, 900);
    let name = member(root, &source, "png");
    rendition(root, &name, ImageVariant::Tile, false).unwrap();
    let tile_before = fs::metadata(root.join(cache(&name, ImageVariant::Tile)))
        .unwrap()
        .modified()
        .unwrap();
    let permit = background();
    let made = ensure(&permit, root, &name, None, &|| false).unwrap();
    let preview_len = fs::metadata(root.join(cache(&name, ImageVariant::Preview)))
        .unwrap()
        .len();
    assert_eq!(
        made,
        Ensured::Made {
            files: 1,
            bytes: preview_len
        }
    );
    assert_eq!(
        fs::metadata(root.join(cache(&name, ImageVariant::Tile)))
            .unwrap()
            .modified()
            .unwrap(),
        tile_before
    );
    assert_eq!(
        cached(root, &name[..64], ImageVariant::Preview).unwrap(),
        prepared(&source, ImageVariant::Preview)
    );
    assert!(!root.join(cache(&name, ImageVariant::Full)).exists());
}

#[test]
fn the_sweep_stops_on_request_and_writes_nothing() {
    let _codec = codec();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let name = member(root, &gif(6), "gif");
    let permit = background();
    assert!(matches!(
        ensure(&permit, root, &name, None, &|| true),
        Err(RenditionError::Media(MediaError::Stopped))
    ));
    assert_eq!(names(root).len(), 1);
    // The same animation completes when not stopped: a poster tile and an
    // animated preview.
    assert!(matches!(
        ensure(&permit, root, &name, None, &|| false).unwrap(),
        Ensured::Made { files: 5, .. }
    ));
}

#[test]
fn cache_names_follow_the_variant_grammar() {
    let digest = "a".repeat(64);
    for variant in VARIANTS {
        let name = variant.file_name(&digest);
        assert_eq!(
            parse_cache_name(&name),
            Some((digest.as_str(), IMAGE_RECIPE_VERSION, Some(variant)))
        );
        assert!(is_current_cache_name(&name));
    }
    assert_eq!(
        parse_cache_name(&format!("{digest}.image-v0-tile.webp")),
        Some((digest.as_str(), 0, Some(ImageVariant::Tile)))
    );
    assert_eq!(
        parse_cache_name(&format!("{digest}.image-v1-poster.webp")),
        Some((digest.as_str(), 1, None))
    );
    for other in [
        format!("{digest}.webp"),
        format!("{digest}.image-v-tile.webp"),
        format!("{digest}.image-v1-tile.png"),
        format!("{}.image-v1-tile.webp", "A".repeat(64)),
    ] {
        assert_eq!(parse_cache_name(&other), None, "{other}");
    }
    assert_eq!(
        image_base(&format!("{digest}.ico")),
        Some((digest.as_str(), "ico"))
    );
    assert_eq!(image_base(&format!("{digest}.txt")), None);
}
