//! Reclaim obsolete image representations once nothing can name them: after
//! an atomic ledger rebuild, and after a fresh or resumed RAM projection is
//! installed, before its live runners write. Run it only while no writer can commit a
//! new reference. Candidates are:
//! - an image member `<64hex>.{png,jpg,gif,webp,bmp,ico}` that no row,
//!   canonical document or rendition map names. Old WebP bundle members that
//!   rows built earlier still name are references like any other and stay;
//! - a rendition cache `<64hex>.image-v<N>-<variant>.webp` whose base no row
//!   names (an orphan) or whose recipe or variant this build does not make.
//!
//! Text, document and HTML members, `.live-images-v1/` and tempfiles are
//! never touched.
use super::image_refs::referenced_names;
use super::renditions::{image_base, is_current_cache_name, is_digest, parse_cache_name};
use crate::StoreError;
use rusqlite::Connection;
use std::{collections::BTreeSet, path::Path};

pub fn prune_unreferenced_images(conn: &Connection, projection: &Path) -> Result<u64, StoreError> {
    let root = super::asset_root(projection);
    if !root.exists() {
        return Ok(0);
    }
    if !std::fs::symlink_metadata(&root)?.file_type().is_dir() {
        return Err(StoreError::AssetInvalid {
            message: "image cache root is not a regular directory".into(),
        });
    }
    let mut members = Vec::new();
    let mut caches = Vec::new();
    for entry in std::fs::read_dir(&root)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if image_base(name).is_some() {
            members.push(name.to_owned());
        } else if parse_cache_name(name).is_some() {
            caches.push(name.to_owned());
        }
    }
    // Text-only archives, and caches that are all stale, need no JSON
    // reference scan.
    let mut removed = 0;
    let mut current = Vec::new();
    for name in caches {
        if is_current_cache_name(&name) {
            current.push(name);
        } else {
            std::fs::remove_file(root.join(name))?;
            removed += 1;
        }
    }
    if members.is_empty() && current.is_empty() {
        return Ok(removed);
    }
    // Authored/collaboration attachments, structured tools and workflow
    // resources live in these typed details, rendition-map leaves included.
    let mut live = referenced_names(conn)?;
    // Externalized canonical documents can themselves own image resources.
    // Resolve their committed references before deleting anything; a missing or
    // corrupt document defers the whole cleanup, never its image resources.
    let mut pending: Vec<String> = live
        .iter()
        .filter(|name| name.ends_with(".json"))
        .cloned()
        .collect();
    let mut visited = BTreeSet::new();
    while let Some(name) = pending.pop() {
        if !visited.insert(name.clone()) {
            continue;
        }
        let Some(digest) = name.strip_suffix(".json").filter(|value| is_digest(value)) else {
            continue;
        };
        let path = root.join(&name);
        if !std::fs::symlink_metadata(&path)?.file_type().is_file() {
            return Err(StoreError::AssetInvalid {
                message: "canonical document is not a regular member".into(),
            });
        }
        let bytes = std::fs::read(path)?;
        if blake3::hash(&bytes).to_hex().as_str() != digest {
            return Err(StoreError::AssetInvalid {
                message: "canonical document failed its content hash".into(),
            });
        }
        let value: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|error| StoreError::AssetInvalid {
                message: format!("invalid canonical document: {error}"),
            })?;
        collect_urls(&value, &mut live, &mut pending);
    }
    let bases: BTreeSet<&str> = live
        .iter()
        .filter_map(|name| image_base(name))
        .map(|(digest, _)| digest)
        .collect();
    for name in members {
        if !live.contains(&name) {
            std::fs::remove_file(root.join(name))?;
            removed += 1;
        }
    }
    for name in current {
        let base = parse_cache_name(&name).map(|(digest, ..)| digest);
        if !base.is_some_and(|digest| bases.contains(digest)) {
            std::fs::remove_file(root.join(&name))?;
            removed += 1;
        }
    }
    Ok(removed)
}

fn collect_urls(value: &serde_json::Value, live: &mut BTreeSet<String>, pending: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(object) => {
            if let Some(name) = object
                .get("url")
                .and_then(|url| url.as_str())
                .and_then(|url| url.strip_prefix(rosetta_core::assets::IMAGE_ASSET_URL_PREFIX))
                && live.insert(name.to_owned())
                && name.ends_with(".json")
            {
                pending.push(name.to_owned());
            }
            for value in object.values() {
                collect_urls(value, live, pending);
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                collect_urls(value, live, pending);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rosetta_core::assets::ImageVariant;

    fn schema() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE message(id INTEGER PRIMARY KEY, detail TEXT, tool_detail TEXT);
             CREATE TABLE workflow_run(detail TEXT)",
        )
        .unwrap();
        conn
    }

    #[test]
    fn only_unreferenced_image_members_are_removed() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("db.sqlite");
        let root = super::super::asset_root(&db);
        std::fs::create_dir_all(&root).unwrap();
        let conn = schema();
        let full = format!("{}.webp", "a".repeat(64));
        let thumb = format!("{}.webp", "b".repeat(64));
        let old = format!("{}.png", "c".repeat(64));
        let text = format!("{}.txt", "d".repeat(64));
        let workflow = format!("{}.webp", "e".repeat(64));
        for name in [&full, &thumb, &old, &text, &workflow] {
            std::fs::write(root.join(name), b"fixture").unwrap();
        }
        let document =
            serde_json::json!({"resources": [{"asset": {"url": format!("/api/assets/{thumb}")}}]})
                .to_string();
        let document_name = format!("{}.json", blake3::hash(document.as_bytes()).to_hex());
        std::fs::write(root.join(&document_name), document).unwrap();
        let detail = serde_json::json!({"assets": [{"url": format!("/api/assets/{full}")}, {"url": format!("/api/assets/{document_name}")} ]});
        conn.execute(
            "INSERT INTO message(detail) VALUES (?)",
            [detail.to_string()],
        )
        .unwrap();
        let workflow_detail = serde_json::json!({"resources": [{"type": "asset", "asset": {"url": format!("/api/assets/{workflow}")}}]});
        conn.execute(
            "INSERT INTO workflow_run(detail) VALUES (?)",
            [workflow_detail.to_string()],
        )
        .unwrap();
        assert_eq!(prune_unreferenced_images(&conn, &db).unwrap(), 1);
        assert!(root.join(workflow).exists());
        assert!(root.join(full).exists());
        assert!(root.join(thumb).exists());
        assert!(root.join(text).exists());
        assert!(!root.join(old).exists());
    }

    #[test]
    fn stale_and_orphan_caches_go_while_referenced_members_and_their_caches_stay() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("db.sqlite");
        let root = super::super::asset_root(&db);
        std::fs::create_dir_all(root.join(".live-images-v1/abc")).unwrap();
        let conn = schema();
        let digest = |seed: char| seed.to_string().repeat(64);
        // A row built before the switch names a legacy bundle (base + leaf);
        // a newer row names a recorded original.
        let (legacy, leaf, original) = (
            format!("{}.webp", digest('a')),
            format!("{}.webp", digest('b')),
            format!("{}.png", digest('c')),
        );
        let (bmp, ico) = (
            format!("{}.bmp", digest('d')),
            format!("{}.ico", digest('e')),
        );
        let unreferenced_bundle = format!("{}.webp", digest('f'));
        let kept_cache = ImageVariant::Tile.file_name(&digest('c'));
        let kept_full = ImageVariant::Full.file_name(&digest('c'));
        let legacy_cache = ImageVariant::Tile.file_name(&digest('a'));
        let orphan = ImageVariant::Preview.file_name(&digest('9'));
        let stale = format!("{}.image-v0-tile.webp", digest('c'));
        let unknown = format!("{}.image-v1-poster.webp", digest('c'));
        let keep = [
            legacy.clone(),
            leaf.clone(),
            original.clone(),
            bmp.clone(),
            kept_cache.clone(),
            kept_full.clone(),
            legacy_cache.clone(),
            format!("{}.txt", digest('7')),
            ".image-in-flight".into(),
            ".rosetta-asset-in-flight".into(),
        ];
        let remove = [ico.clone(), unreferenced_bundle, orphan, stale, unknown];
        for name in keep.iter().chain(&remove) {
            std::fs::write(root.join(name), b"fixture").unwrap();
        }
        let detail = serde_json::json!({"type": "prose", "attachments": [
            {"source": {"type": "asset", "asset": {"url": format!("/api/assets/{legacy}"),
                "image_variants": {"rail": {"url": format!("/api/assets/{leaf}")}}}}},
            {"source": {"type": "asset", "asset": {"url": format!("/api/assets/{original}")}}},
        ]});
        let tool = serde_json::json!({"attachments": [
            {"source": {"type": "asset", "asset": {"url": format!("/api/assets/{bmp}")}}}]});
        conn.execute(
            "INSERT INTO message(detail, tool_detail) VALUES (?1, ?2)",
            [detail.to_string(), tool.to_string()],
        )
        .unwrap();
        assert_eq!(
            prune_unreferenced_images(&conn, &db).unwrap(),
            remove.len() as u64
        );
        for name in &keep {
            assert!(root.join(name).exists(), "{name} was removed");
        }
        for name in &remove {
            assert!(!root.join(name).exists(), "{name} was kept");
        }
        assert!(root.join(".live-images-v1/abc").is_dir());
        assert_eq!(prune_unreferenced_images(&conn, &db).unwrap(), 0);
    }

    /// A leading `ftyp` box (size 24: major brand, minor version, two
    /// compatible brands) and padding: what the store accepts as a video.
    fn video(major: &[u8; 4], compatible: [&[u8; 4]; 2], padding: &[u8]) -> Vec<u8> {
        let mut bytes = 24_u32.to_be_bytes().to_vec();
        bytes.extend_from_slice(b"ftyp");
        bytes.extend_from_slice(major);
        bytes.extend_from_slice(&[0, 0, 2, 0]);
        for brand in compatible {
            bytes.extend_from_slice(brand);
        }
        bytes.extend_from_slice(padding);
        bytes
    }

    /// Video members are not image bases: image GC removes neither a
    /// referenced one nor an unreferenced one, whatever else it removes.
    #[test]
    fn video_members_are_never_collected() {
        use rosetta_core::{IngestOp, ProjectionAsset};

        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("db.sqlite");
        let root = super::super::asset_root(&db);
        let conn = schema();
        // The store places the members, under the names it gives them.
        let videos: Vec<ProjectionAsset> = [
            ("video/mp4", b"isom", "referenced mp4"),
            ("video/mp4", b"isom", "unreferenced mp4"),
            ("video/quicktime", b"qt  ", "referenced mov"),
            ("video/quicktime", b"qt  ", "unreferenced mov"),
        ]
        .into_iter()
        .map(|(media_type, major, padding)| {
            let bytes = video(major, [b"isom", b"iso2"], padding.as_bytes());
            ProjectionAsset::new(media_type, bytes).unwrap()
        })
        .collect();
        let mut store = super::super::AssetStore::for_projection(&db);
        let placed = store
            .materialize_batch(
                &crate::Batch {
                    ops: videos
                        .iter()
                        .cloned()
                        .map(|asset| IngestOp::ProjectionAsset { asset })
                        .collect(),
                    ..crate::Batch::default()
                },
                true,
            )
            .unwrap();
        assert_eq!(placed.files_written, 4);
        let names: Vec<String> = videos.iter().map(|v| v.file_name().unwrap()).collect();
        assert!(names[0].ends_with(".mp4") && names[1].ends_with(".mp4"));
        assert!(names[2].ends_with(".mov") && names[3].ends_with(".mov"));
        assert!(names.iter().all(|name| image_base(name).is_none()));

        // Rows name the first mp4 (a tool attachment) and the first mov (an
        // authored one). Two image members sit beside them, one referenced.
        let kept_image = format!("{}.png", "a".repeat(64));
        let dropped_image = format!("{}.png", "b".repeat(64));
        // A rendition cache named for a video's digest has no image base.
        let stray_cache = ImageVariant::Tile.file_name(&videos[0].digest);
        for name in [&kept_image, &dropped_image, &stray_cache] {
            std::fs::write(root.join(name), b"fixture").unwrap();
        }
        let asset = |name: &str| {
            serde_json::json!({"source": {"type": "asset",
                "asset": {"url": format!("/api/assets/{name}")}}})
        };
        let detail = serde_json::json!({"type": "prose", "attachments": [asset(&names[2]), asset(&kept_image)]});
        let tool = serde_json::json!({"attachments": [asset(&names[0])]});
        conn.execute(
            "INSERT INTO message(detail, tool_detail) VALUES (?1, ?2)",
            [detail.to_string(), tool.to_string()],
        )
        .unwrap();

        assert_eq!(
            prune_unreferenced_images(&conn, &db).unwrap(),
            2,
            "the unreferenced image and the stray cache"
        );
        assert!(root.join(&kept_image).exists());
        assert!(!root.join(&dropped_image).exists());
        assert!(!root.join(&stray_cache).exists());
        let videos_intact = || {
            for (asset, name) in videos.iter().zip(&names) {
                assert_eq!(
                    std::fs::read(root.join(name)).unwrap(),
                    asset.bytes,
                    "{name} was removed or rewritten"
                );
            }
        };
        videos_intact();

        // With no row left at all, and nothing but videos on disk, the pass
        // still removes none of them.
        conn.execute("DELETE FROM message", []).unwrap();
        assert_eq!(prune_unreferenced_images(&conn, &db).unwrap(), 1);
        assert!(!root.join(&kept_image).exists());
        videos_intact();
        assert_eq!(prune_unreferenced_images(&conn, &db).unwrap(), 0);
        videos_intact();
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), videos.len());
    }

    #[test]
    fn stale_caches_alone_need_no_reference_scan() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("db.sqlite");
        let root = super::super::asset_root(&db);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join(format!("{}.image-v0-rail.webp", "a".repeat(64))),
            b"x",
        )
        .unwrap();
        // No tables at all: a scan would fail.
        let conn = Connection::open_in_memory().unwrap();
        assert_eq!(prune_unreferenced_images(&conn, &db).unwrap(), 1);
    }
}
