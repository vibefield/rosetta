use super::*;
use rosetta_core::assets::ImageVariant;
use std::io::Cursor;

fn png(width: u32, height: u32, color: [u8; 4]) -> Vec<u8> {
    let pixels = image::RgbaImage::from_pixel(width, height, image::Rgba(color));
    let mut bytes = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(pixels)
        .write_to(&mut bytes, image::ImageFormat::Png)
        .unwrap();
    bytes.into_inner()
}

async fn image_get(h: &Harness, url: &str) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let response = h
        .app
        .clone()
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

#[tokio::test]
async fn canonical_renditions_are_individually_content_addressed_and_durable() {
    let mut h = harness();
    let prepared = rosetta_media::prepare(&png(400, 800, [70, 100, 190, 128])).unwrap();
    let reference = prepared.asset.reference();
    reference.validate().unwrap();
    h.store
        .apply(&Batch {
            ops: vec![IngestOp::ProjectionAsset {
                asset: prepared.asset.clone(),
            }],
            ..Batch::default()
        })
        .unwrap();
    let mut names = std::collections::BTreeSet::new();
    for member in
        std::iter::once(&reference).chain(reference.image_variants.as_ref().unwrap().values())
    {
        let (status, headers, bytes) = image_get(&h, &member.url).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers["content-type"], "image/webp");
        assert!(
            headers["cache-control"]
                .to_str()
                .unwrap()
                .contains("immutable")
        );
        assert_eq!(bytes.len() as u64, member.byte_len);
        assert_eq!(blake3::hash(&bytes).to_hex().as_str(), member.digest);
        names.insert(member.url.clone());
    }
    assert_eq!(
        std::fs::read_dir(rosetta_store::assets::asset_root(&h.db))
            .unwrap()
            .count(),
        names.len()
    );
    let rail = &reference.image_variants.as_ref().unwrap()[&ImageVariant::Rail];
    assert_eq!(
        rosetta_media::dimensions(&image_get(&h, &rail.url).await.2)
            .unwrap()
            .height,
        160
    );
    let tile = &reference.image_variants.as_ref().unwrap()[&ImageVariant::Tile];
    assert_eq!(
        rosetta_media::dimensions(&image_get(&h, &tile.url).await.2).unwrap(),
        rosetta_core::timeline::Dimensions {
            width: 320,
            height: 320
        }
    );
    assert_eq!(
        image_get(&h, &format!("{}?image=tile&v=999", reference.url))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        image_get(&h, &format!("{}?image=arbitrary", reference.url))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn live_image_urls_track_file_changes_without_storing_originals() {
    let mut h = harness();
    let path = h._dir.path().join("image-without-extension");
    let original = png(600, 400, [230, 20, 30, 255]);
    std::fs::write(&path, &original).unwrap();
    let mut row = message("live-image", MessageKind::ToolUse, "");
    row.content = None;
    row.tool = Some(ToolInteraction {
        attachments: Vec::new(),
        correlation: ToolCorrelation {
            key: "image-read".into(),
            derivation: CorrelationDerivation::NativeId,
        },
        native: NativeToolRef {
            id: Some("image-read".into()),
            namespace: None,
            name: "Read".into(),
            provider: None,
        },
        status: ToolStatusFact {
            value: ToolStatus::Succeeded,
            basis: StatusBasis::NativeTerminalRecord,
        },
        action: ToolAction::FileRead {
            targets: vec![ReadTarget {
                resource: ResourceRef::File {
                    path: path.to_string_lossy().into_owned(),
                    resolved: true,
                },
                range: None,
                media_kind: None,
            }],
            outcome: None,
        },
        failure: None,
        timing: None,
    });
    h.append(vec![row]);
    let url = "/api/sessions/s1/timeline/live-image/read/0?image=tile&v=1";
    let (status, headers, first) = image_get(&h, url).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["content-type"], "image/webp");
    assert_eq!(headers["cache-control"], "private, no-cache");
    assert_eq!(image_get(&h, url).await.2, first);
    let validated = h
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri(url)
                .header("if-none-match", &headers["etag"])
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(validated.status(), StatusCode::NOT_MODIFIED);
    assert!(
        validated
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .is_empty()
    );

    let full_url = "/api/sessions/s1/timeline/live-image/read/0?image=full&v=1";
    let full = image_get(&h, full_url).await.2;
    assert_eq!(rosetta_media::dimensions(&full).unwrap().width, 600);
    assert_eq!(
        std::fs::read(&path).unwrap(),
        original,
        "source file remains unchanged"
    );
    std::fs::write(&path, png(600, 400, [30, 20, 230, 255])).unwrap();
    let (_, next_headers, second) = image_get(&h, url).await;
    assert_ne!(second, first);
    assert_ne!(next_headers["etag"], headers["etag"]);
    let cache = rosetta_store::assets::asset_root(&h.db).join(".live-images-v1");
    for directory in std::fs::read_dir(cache).unwrap() {
        for file in std::fs::read_dir(directory.unwrap().path()).unwrap() {
            let file = file.unwrap();
            assert_eq!(file.path().extension().unwrap(), "webp");
            assert_eq!(&std::fs::read(file.path()).unwrap()[8..12], b"WEBP");
        }
    }
    assert_eq!(
        image_get(
            &h,
            "/api/sessions/s1/timeline/live-image/read/1?image=rail&v=1"
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    let forbidden = h
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri(url)
                .header("origin", "https://example.com")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);
    let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" width="40" height="20"><rect width="40" height="20" fill="#abc"/></svg>"##;
    std::fs::write(&path, svg).unwrap();
    let (status, headers, payload) = image_get(&h, url).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["content-type"], "image/svg+xml");
    assert!(
        headers["content-security-policy"]
            .to_str()
            .unwrap()
            .contains("sandbox")
    );
    assert_eq!(payload, svg);
}
