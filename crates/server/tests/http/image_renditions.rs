//! Display renditions of recorded originals and legacy bundles, made on
//! demand from the store's rendition cache. Projections are built with store
//! APIs and in-test images; a legacy bundle is laid out as member files.
use super::*;
use rosetta_core::assets::ImageVariant;
use rosetta_core::timeline::{
    AssetRef, AttachmentKind, AttachmentRef, AttachmentSource, Dimensions,
};
use rosetta_media::{CodecPermit, Lane};
use std::collections::BTreeSet;
use std::io::Cursor;
use std::time::Duration;

fn image(width: u32, height: u32, seed: u8, format: image::ImageFormat) -> Vec<u8> {
    let pixels = image::RgbaImage::from_fn(width, height, |x, y| {
        image::Rgba([
            (x % 251) as u8 ^ seed,
            (y % 241) as u8,
            ((x + y) % 239) as u8,
            255,
        ])
    });
    let mut bytes = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(pixels)
        .write_to(&mut bytes, format)
        .unwrap();
    bytes.into_inner()
}

fn png(width: u32, height: u32, seed: u8) -> Vec<u8> {
    image(width, height, seed, image::ImageFormat::Png)
}

async fn fetch(app: Router, url: String) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let response = app
        .oneshot(Request::builder().uri(url).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec();
    (status, headers, bytes)
}

async fn get(h: &Harness, url: &str) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    fetch(h.app.clone(), url.to_owned()).await
}

/// A recorded original, materialized by the store as one member.
fn record(h: &mut Harness, media_type: &str, bytes: &[u8]) -> AssetRef {
    let asset = ProjectionAsset::new(media_type, bytes.to_vec()).unwrap();
    let reference = asset.reference();
    reference.validate().unwrap();
    assert!(reference.image_variants.is_none());
    h.store
        .apply(&Batch {
            ops: vec![IngestOp::ProjectionAsset { asset }],
            ..Batch::default()
        })
        .unwrap();
    reference
}

/// A member file placed directly in `.assets`.
fn place(h: &Harness, extension: &str, bytes: &[u8]) -> String {
    let root = rosetta_store::assets::asset_root(&h.db);
    std::fs::create_dir_all(&root).unwrap();
    let name = format!("{}.{extension}", blake3::hash(bytes).to_hex());
    std::fs::write(root.join(&name), bytes).unwrap();
    format!("/api/assets/{name}")
}

fn files(h: &Harness) -> BTreeSet<String> {
    std::fs::read_dir(rosetta_store::assets::asset_root(&h.db))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect()
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

fn rendition_url(reference: &AssetRef, variant: ImageVariant) -> String {
    format!("{}?image={}&v=1", reference.url, variant.name())
}

#[tokio::test]
async fn recorded_originals_render_byte_identical_webp_renditions() {
    let mut h = harness();
    let source = png(400, 800, 1);
    let original = record(&mut h, "image/png", &source);

    let (status, headers, bytes) = get(&h, &original.url).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["content-type"], "image/png");
    assert_eq!(headers["x-content-type-options"], "nosniff");
    assert_eq!(bytes, source, "the plain URL is the recorded original");

    let tile_url = rendition_url(&original, ImageVariant::Tile);
    let (status, headers, tile) = get(&h, &tile_url).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["content-type"], "image/webp");
    assert!(
        headers["cache-control"]
            .to_str()
            .unwrap()
            .contains("immutable")
    );
    assert_eq!(tile, prepared(&source, ImageVariant::Tile));
    let digest = &original.digest;
    let thumbs: BTreeSet<_> = [ImageVariant::Rail, ImageVariant::Tile2x, ImageVariant::Tile]
        .into_iter()
        .map(|v| v.file_name(digest))
        .chain([format!("{digest}.png")])
        .collect();
    assert_eq!(files(&h), thumbs);
    assert_eq!(get(&h, &tile_url).await.2, tile);
    assert_eq!(files(&h), thumbs, "a cached tile writes nothing");

    // At most 1600 px: preview is Full's encoding, one file under two names.
    let full = prepared(&source, ImageVariant::Full);
    let preview = get(&h, &rendition_url(&original, ImageVariant::Preview)).await;
    assert_eq!((preview.0, preview.2), (StatusCode::OK, full.clone()));
    let (status, headers, one_to_one) =
        get(&h, &rendition_url(&original, ImageVariant::Full)).await;
    assert_eq!((status, one_to_one), (StatusCode::OK, full));
    assert_eq!(headers["content-type"], "image/webp");
    assert_eq!(files(&h).len(), thumbs.len() + 2);
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let root = rosetta_store::assets::asset_root(&h.db);
        let inode = |v: ImageVariant| {
            std::fs::metadata(root.join(v.file_name(digest)))
                .unwrap()
                .ino()
        };
        assert_eq!(inode(ImageVariant::Preview), inode(ImageVariant::Full));
    }

    let validated = h
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri(&tile_url)
                .header("if-none-match", &headers["etag"])
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        validated.status(),
        StatusCode::OK,
        "a different rendition's tag"
    );
    let tile_tag = get(&h, &tile_url).await.1["etag"].clone();
    let validated = h
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri(&tile_url)
                .header("if-none-match", tile_tag)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(validated.status(), StatusCode::NOT_MODIFIED);

    // Over 1600 px: preview is its own file and Full is made on first 1:1.
    let wide_source = png(2000, 300, 2);
    let wide = record(&mut h, "image/png", &wide_source);
    let before = files(&h).len();
    assert_eq!(
        get(&h, &rendition_url(&wide, ImageVariant::Preview))
            .await
            .2,
        prepared(&wide_source, ImageVariant::Preview)
    );
    assert_eq!(files(&h).len(), before + 1);
    assert_eq!(
        get(&h, &rendition_url(&wide, ImageVariant::Full)).await.2,
        prepared(&wide_source, ImageVariant::Full)
    );
    assert_eq!(files(&h).len(), before + 2);

    // A torn cache file is a miss that is repaired.
    let tile_path =
        rosetta_store::assets::asset_root(&h.db).join(ImageVariant::Tile.file_name(digest));
    std::fs::write(&tile_path, &tile[..tile.len() / 2]).unwrap();
    assert_eq!(get(&h, &tile_url).await.2, tile);
    assert_eq!(std::fs::read(&tile_path).unwrap(), tile);

    // A recorded WebP original is re-encoded for 1:1 too, never passed through.
    let webp_source = image(64, 40, 3, image::ImageFormat::WebP);
    let webp = record(&mut h, "image/webp", &webp_source);
    let (status, headers, bytes) = get(&h, &rendition_url(&webp, ImageVariant::Full)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["content-type"], "image/webp");
    assert_ne!(bytes, webp_source);
    assert_eq!(bytes, prepared(&webp_source, ImageVariant::Full));

    let missing = format!(
        "/api/assets/{}.png",
        blake3::hash(b"never recorded").to_hex()
    );
    assert_eq!(
        get(&h, &format!("{missing}?image=tile&v=1")).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        get(&h, &format!("{}?image=tile&v=999", original.url))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        get(&h, &format!("{}?image=arbitrary", original.url))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn bmp_and_ico_members_are_routed_and_rendered() {
    let h = harness();
    for (format, extension, media_type) in [
        (image::ImageFormat::Bmp, "bmp", "image/bmp"),
        (image::ImageFormat::Ico, "ico", "image/x-icon"),
    ] {
        let source = image(48, 32, 4, format);
        let url = place(&h, extension, &source);
        let (status, headers, bytes) = get(&h, &url).await;
        assert_eq!(status, StatusCode::OK, "{extension}");
        assert_eq!(headers["content-type"], media_type);
        assert_eq!(bytes, source);
        let (status, headers, tile) = get(&h, &format!("{url}?image=tile&v=1")).await;
        assert_eq!(status, StatusCode::OK, "{extension}");
        assert_eq!(headers["content-type"], "image/webp");
        assert_eq!(tile, prepared(&source, ImageVariant::Tile));
    }
}

#[tokio::test]
async fn mixed_projection_serves_legacy_bundles_and_recorded_originals() {
    let mut h = harness();
    // Built by an earlier binary: a complete WebP bundle, as member files.
    let legacy_source = png(300, 200, 5);
    let bundle = rosetta_media::prepare(&legacy_source).unwrap();
    place(&h, "webp", &bundle.asset.bytes);
    for member in &bundle.asset.image_variants {
        place(&h, "webp", &member.bytes);
    }
    let legacy = bundle.asset.reference();
    legacy.validate().unwrap();
    // Normalized by this binary: the recorded original alone.
    let original_source = png(260, 180, 6);
    let original = record(&mut h, "image/png", &original_source);
    let attachment = |id: &str, asset: &AssetRef, dimensions: Dimensions| {
        let mut value = AttachmentRef::new(
            id,
            AttachmentKind::Image,
            AttachmentSource::Asset {
                asset: asset.clone(),
            },
        );
        value.media_type = Some(asset.media_type.clone());
        value.byte_len = Some(asset.byte_len);
        value.dimensions = Some(dimensions);
        value
    };
    let mut row = message("two-images", MessageKind::User, "before and after");
    row.attachments = vec![
        attachment("legacy", &legacy, bundle.dimensions),
        attachment(
            "original",
            &original,
            rosetta_media::probe(&original_source).unwrap().dimensions,
        ),
    ];
    h.append(vec![row]);

    let (status, body) = h.get("/api/sessions/s1/timeline?limit=10").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let page: TimelinePage = serde_json::from_value(body).unwrap();
    let TimelineBody::User { attachments, .. } = &page.entries[0].body else {
        panic!("not a user entry");
    };
    let sources: Vec<_> = attachments
        .iter()
        .map(|a| match &a.source {
            AttachmentSource::Asset { asset } => asset.clone(),
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(sources, [legacy.clone(), original.clone()]);

    // Legacy URLs are served exactly as before.
    for member in std::iter::once(&legacy).chain(legacy.image_variants.as_ref().unwrap().values()) {
        let (status, headers, bytes) = get(&h, &member.url).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers["content-type"], "image/webp");
        assert_eq!(blake3::hash(&bytes).to_hex().as_str(), member.digest);
    }
    // A legacy base still repairs a missing rendition from its own pixels.
    let (status, headers, _) = get(&h, &rendition_url(&legacy, ImageVariant::Tile)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["content-type"], "image/webp");
    // The original's renditions are the recipe's bytes.
    assert_eq!(get(&h, &original.url).await.2, original_source);
    assert_eq!(
        get(&h, &rendition_url(&original, ImageVariant::Rail))
            .await
            .2,
        prepared(&original_source, ImageVariant::Rail)
    );

    // GC keeps both representations and their caches, and prunes only what
    // no row names.
    let kept = files(&h);
    assert_eq!(
        rosetta_store::assets::prune_unreferenced_images(h.store.conn(), &h.db).unwrap(),
        0
    );
    assert_eq!(files(&h), kept);
    let root = rosetta_store::assets::asset_root(&h.db);
    let orphan = ImageVariant::Tile.file_name(&"9".repeat(64));
    std::fs::write(root.join(&orphan), b"orphan").unwrap();
    place(&h, "webp", &prepared(&png(20, 20, 7), ImageVariant::Full));
    assert_eq!(
        rosetta_store::assets::prune_unreferenced_images(h.store.conn(), &h.db).unwrap(),
        2
    );
    assert_eq!(files(&h), kept);
}

async fn slot(lane: Lane) -> CodecPermit {
    match lane {
        Lane::Foreground => tokio::task::spawn_blocking(|| rosetta_media::admit(Lane::Foreground))
            .await
            .unwrap(),
        Lane::Background => loop {
            if let Some(permit) = rosetta_media::try_admit(Lane::Background) {
                return permit;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        },
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cached_tiles_return_while_cold_bases_wait_for_a_codec_slot() {
    let mut h = harness();
    let mut cached = Vec::new();
    for seed in 0..8 {
        let source = png(120, 90, seed);
        let reference = record(&mut h, "image/png", &source);
        let url = rendition_url(&reference, ImageVariant::Tile);
        assert_eq!(get(&h, &url).await.0, StatusCode::OK);
        cached.push((url, prepared(&source, ImageVariant::Tile)));
    }
    let cold: Vec<_> = (0..5)
        .map(|seed| {
            let source = png(900, 700, 100 + seed);
            (record(&mut h, "image/png", &source), source)
        })
        .collect();

    // The sweep holds the background slot. The other slot is held too, so
    // every cold request blocks on codec admission: four of them hold all
    // four worker permits and the fifth waits for a permit.
    let background = slot(Lane::Background).await;
    let foreground = slot(Lane::Foreground).await;
    let waiting: Vec<_> = cold
        .iter()
        .map(|(reference, _)| {
            tokio::spawn(fetch(
                h.app.clone(),
                rendition_url(reference, ImageVariant::Tile),
            ))
        })
        .collect();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(waiting.iter().all(|task| !task.is_finished()));

    let started = std::time::Instant::now();
    let hits: Vec<_> = cached
        .iter()
        .map(|(url, _)| tokio::spawn(fetch(h.app.clone(), url.clone())))
        .collect();
    for ((_, expected), hit) in cached.iter().zip(hits) {
        let (status, headers, bytes) = tokio::time::timeout(Duration::from_secs(10), hit)
            .await
            .expect("a cached tile queued behind the cold encodes")
            .unwrap();
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers["content-type"], "image/webp");
        assert_eq!(&bytes, expected);
    }
    assert!(started.elapsed() < Duration::from_secs(10));
    assert!(waiting.iter().all(|task| !task.is_finished()));

    // With the background slot still held, the cold bases encode one at a
    // time in the remaining slot.
    drop(foreground);
    for ((_, source), task) in cold.iter().zip(waiting) {
        let (status, _, bytes) = tokio::time::timeout(Duration::from_secs(120), task)
            .await
            .expect("a cold tile never encoded")
            .unwrap();
        assert_eq!(status, StatusCode::OK);
        assert_eq!(bytes, prepared(source, ImageVariant::Tile));
    }
    drop(background);
}
