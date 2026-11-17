//! Recorded video members on the asset route. A sent MP4 or MOV is one
//! content-addressed member, served whole like any other: its own media
//! type, immutable, digest-verified on every read, and never an image.
use super::*;
use rosetta_core::assets::ImageVariant;
use rosetta_core::timeline::AssetRef;
use std::collections::BTreeSet;

const IMMUTABLE: &str = "private, max-age=31536000, immutable";

/// A leading `ftyp` box (size 24: major brand, minor version, two compatible
/// brands), then `padding` bytes that differ along the file, so a truncated
/// or reordered body cannot compare equal.
fn video(major: &[u8; 4], padding: usize) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(24 + padding);
    bytes.extend_from_slice(&24_u32.to_be_bytes());
    bytes.extend_from_slice(b"ftyp");
    bytes.extend_from_slice(major);
    bytes.extend_from_slice(&[0, 0, 2, 0]);
    bytes.extend_from_slice(b"isomiso2");
    bytes.extend((0..padding).map(|index| (index % 251) as u8 ^ (index >> 16) as u8));
    bytes
}

/// A video member, materialized by the store as ingestion would.
fn record(h: &mut Harness, media_type: &str, bytes: &[u8]) -> AssetRef {
    let asset = ProjectionAsset::new(media_type, bytes.to_vec()).unwrap();
    let reference = asset.reference();
    reference.validate().unwrap();
    h.store
        .apply(&Batch {
            ops: vec![IngestOp::ProjectionAsset { asset }],
            ..Batch::default()
        })
        .unwrap();
    reference
}

async fn fetch(
    h: &Harness,
    url: &str,
    headers: &[(&str, &str)],
) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let mut request = Request::builder().uri(url);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = h
        .app
        .clone()
        .oneshot(request.body(Body::empty()).unwrap())
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

fn files(h: &Harness) -> BTreeSet<String> {
    std::fs::read_dir(rosetta_store::assets::asset_root(&h.db))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect()
}

fn assert_video_headers(headers: &axum::http::HeaderMap, media_type: &str) {
    assert_eq!(headers["content-type"], media_type);
    assert_eq!(headers["x-content-type-options"], "nosniff");
    assert_eq!(headers["cache-control"], IMMUTABLE);
}

#[tokio::test]
async fn an_mp4_member_past_the_image_limit_is_served_whole() {
    let mut h = harness();
    // Past 32 MiB, the largest image the codec reads: no image limit applies.
    let source = video(b"isom", rosetta_media::MAX_INPUT_BYTES + 4096);
    assert!(source.len() > 32 * 1024 * 1024);
    let member = record(&mut h, "video/mp4", &source);
    assert!(member.url.ends_with(".mp4"), "{}", member.url);
    assert_eq!(member.byte_len, source.len() as u64);

    let (status, headers, bytes) = fetch(&h, &member.url, &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert_video_headers(&headers, "video/mp4");
    assert_eq!(bytes.len(), source.len());
    assert!(bytes == source, "the body is exactly the stored bytes");
    assert_eq!(
        files(&h),
        BTreeSet::from([format!("{}.mp4", member.digest)]),
        "serving a video writes nothing beside it"
    );
}

#[tokio::test]
async fn a_mov_member_is_quicktime_and_a_range_request_still_gets_the_whole_file() {
    let mut h = harness();
    let source = video(b"qt  ", 70_000);
    let member = record(&mut h, "video/quicktime", &source);
    assert!(member.url.ends_with(".mov"), "{}", member.url);

    let (status, headers, bytes) = fetch(&h, &member.url, &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert_video_headers(&headers, "video/quicktime");
    assert_eq!(bytes, source);

    // The route has no Range support: it answers 200 with every byte, never
    // a partial body.
    let (status, headers, bytes) = fetch(&h, &member.url, &[("range", "bytes=0-99")]).await;
    assert_eq!(status, StatusCode::OK);
    assert_video_headers(&headers, "video/quicktime");
    assert!(headers.get("content-range").is_none());
    assert_eq!(bytes, source);

    // The same bytes under the other container's name are another member,
    // and this projection has none.
    let (status, ..) = fetch(&h, &format!("/api/assets/{}.mp4", member.digest), &[]).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn an_image_variant_of_a_video_member_is_a_bad_request() {
    let mut h = harness();
    let members = [
        record(&mut h, "video/mp4", &video(b"isom", 4096)),
        record(&mut h, "video/quicktime", &video(b"qt  ", 4096)),
    ];
    let before = files(&h);
    assert_eq!(before.len(), 2);
    let variants = [
        ImageVariant::Rail,
        ImageVariant::Tile,
        ImageVariant::Tile2x,
        ImageVariant::Preview,
        ImageVariant::Full,
    ];
    for member in &members {
        // Every real variant, with and without the recipe version and a
        // retry: the handler refuses the member for what it is.
        for variant in variants {
            for suffix in ["", "&v=1", "&v=1&retry=1", "&v=999"] {
                let url = format!("{}?image={}{suffix}", member.url, variant.name());
                let (status, _, body) = fetch(&h, &url, &[]).await;
                assert_eq!(status, StatusCode::BAD_REQUEST, "{url}");
                let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
                assert_eq!(body["error"]["code"], "bad_request", "{url}");
                assert_eq!(
                    body["error"]["message"], "this asset is not an image",
                    "{url}"
                );
            }
        }
        // A name that is no variant at all is a bad request on any member.
        for variant in ["thumb", ""] {
            let url = format!("{}?image={variant}", member.url);
            assert_eq!(
                fetch(&h, &url, &[]).await.0,
                StatusCode::BAD_REQUEST,
                "{url}"
            );
        }
        // The member itself is still served.
        assert_eq!(fetch(&h, &member.url, &[]).await.0, StatusCode::OK);
    }
    assert_eq!(files(&h), before, "no rendition is made for a video");
}

#[tokio::test]
async fn a_video_name_the_route_does_not_map_is_not_found() {
    let mut h = harness();
    let source = video(b"isom", 4096);
    let member = record(&mut h, "video/mp4", &source);
    let root = rosetta_store::assets::asset_root(&h.db);
    // Even with the file on disk under that name, WebM is no member type.
    for extension in ["webm", "mkv", "m4v", "MP4", "mp4.webm"] {
        let name = format!("{}.{extension}", member.digest);
        std::fs::write(root.join(&name), &source).unwrap();
        let (status, ..) = fetch(&h, &format!("/api/assets/{name}"), &[]).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{name}");
    }
    // A mapped name with no member behind it.
    let absent = format!("/api/assets/{}.mp4", "0".repeat(64));
    assert_eq!(fetch(&h, &absent, &[]).await.0, StatusCode::NOT_FOUND);
    assert_eq!(fetch(&h, &member.url, &[]).await.0, StatusCode::OK);
}

#[tokio::test]
async fn a_video_member_whose_bytes_changed_is_refused() {
    let mut h = harness();
    let root = rosetta_store::assets::asset_root(&h.db);
    for (media_type, major) in [("video/mp4", b"isom"), ("video/quicktime", b"qt  ")] {
        let source = video(major, 4096);
        let member = record(&mut h, media_type, &source);
        let path = root.join(member.url.strip_prefix("/api/assets/").unwrap());
        assert_eq!(fetch(&h, &member.url, &[]).await.0, StatusCode::OK);

        // Same length and still a video by its header; then a shorter file.
        let mut flipped = source.clone();
        *flipped.last_mut().unwrap() ^= 1;
        for corrupt in [flipped.as_slice(), &source[..source.len() - 1]] {
            std::fs::write(&path, corrupt).unwrap();
            let (status, headers, bytes) = fetch(&h, &member.url, &[]).await;
            assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{media_type}");
            assert_ne!(
                headers.get("content-type").and_then(|v| v.to_str().ok()),
                Some(media_type)
            );
            assert!(
                bytes != corrupt,
                "the changed bytes are never sent ({media_type})"
            );
        }

        // The recorded bytes verify again once they are back.
        std::fs::write(&path, &source).unwrap();
        let (status, _, bytes) = fetch(&h, &member.url, &[]).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(bytes, source);
    }
}
