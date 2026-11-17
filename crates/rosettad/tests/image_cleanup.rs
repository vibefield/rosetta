//! A fresh RAM import into an existing `.assets` (the user deleted only the
//! `.sqlite`) prunes the image members and rendition caches its new rows do
//! not name, right after publication and before live work; everything else
//! in the directory, every member those rows name and their caches stay.
use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let destination = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_tree(&entry.path(), &destination);
        } else {
            std::fs::copy(entry.path(), destination).unwrap();
        }
    }
}

fn basic_chat(home: &Path) {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../groundtruth/claude-code/fixtures/2.1.241/basic-chat/claude-config");
    copy_tree(&fixture, &home.join(".claude"));
}

/// A fresh `serve --ticks 0` of `home` into `db`; returns stderr.
fn fresh_serve(home: &Path, db: &Path) -> String {
    assert!(!db.exists(), "a fresh import starts without its .sqlite");
    let output = Command::new(env!("CARGO_BIN_EXE_rosettad"))
        .args([
            "serve",
            "--db",
            db.to_str().unwrap(),
            "--home",
            home.to_str().unwrap(),
            "--agent",
            "claude-code",
            "--addr",
            "127.0.0.1:0",
            "--ticks",
            "0",
        ])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{stderr}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(!stderr.contains("image cache cleanup deferred"), "{stderr}");
    stderr
}

/// Every member name the published projection's rows point at.
fn referenced(db: &Path) -> BTreeSet<String> {
    let conn =
        rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    rosetta_store::assets::image_refs::referenced_names(&conn).unwrap()
}

#[test]
fn a_fresh_import_keeps_every_member_and_cache_its_rows_name() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    basic_chat(&home);
    // One more user turn carrying an inline PNG, in the same session.
    let sessions = home.join(".claude/projects/-private-tmp-rosetta-farm-basic-chat-project");
    let transcript = sessions.join("665e3e71-7c12-4810-925a-e8b92cb6f29d.jsonl");
    let mut lines = std::fs::read_to_string(&transcript).unwrap();
    lines.push_str(
        &serde_json::json!({
            "type": "user",
            "parentUuid": "817e4ddf-9ce2-46ee-9320-7afbb1034d20",
            "isSidechain": false,
            "message": {"role": "user", "content": [
                {"type": "text", "text": "and this picture"},
                {"type": "image", "source": {"type": "base64", "media_type": "image/png",
                    "data": "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+ip1sAAAAASUVORK5CYII="}},
            ]},
            "uuid": "22222222-2222-2222-2222-222222222222",
            "timestamp": "2026-08-25T02:35:20.000Z",
            "sessionId": "665e3e71-7c12-4810-925a-e8b92cb6f29d",
            "version": "2.1.241",
            "cwd": "/private/tmp/rosetta-farm/basic-chat/project",
            "gitBranch": "HEAD",
            "userType": "external",
        })
        .to_string(),
    );
    lines.push('\n');
    std::fs::write(&transcript, lines).unwrap();

    // The first import learns which members this home's rows name.
    let first = temp.path().join("first/projection.sqlite");
    fresh_serve(&home, &first);
    let named = referenced(&first);
    let images: BTreeSet<_> = named
        .iter()
        .filter_map(|name| rosetta_store::assets::renditions::image_base(name))
        .map(|(digest, _)| digest.to_owned())
        .collect();
    assert!(!images.is_empty(), "the turn's image is referenced");

    // The user deletes only the .sqlite: a fresh import into the same
    // `.assets`, which also holds caches of those images and stale leftovers.
    let second = temp.path().join("second/projection.sqlite");
    let assets = rosetta_store::assets::asset_root(&second);
    copy_tree(&rosetta_store::assets::asset_root(&first), &assets);
    let caches: Vec<_> = images
        .iter()
        .flat_map(|digest| {
            [
                rosetta_core::assets::ImageVariant::Tile.file_name(digest),
                rosetta_core::assets::ImageVariant::Full.file_name(digest),
            ]
        })
        .collect();
    let stale = [
        format!("{}.png", "b".repeat(64)),
        rosetta_core::assets::ImageVariant::Tile.file_name(&"c".repeat(64)),
    ];
    for name in caches.iter().chain(&stale) {
        std::fs::write(assets.join(name), b"left behind").unwrap();
    }
    fresh_serve(&home, &second);
    assert_eq!(
        referenced(&second),
        named,
        "the same rows name the same members"
    );
    for name in named.iter().chain(&caches) {
        assert!(assets.join(name).is_file(), "{name} was pruned");
    }
    for name in &stale {
        assert!(!assets.join(name).exists(), "{name} survived publication");
    }
}

#[test]
fn a_fresh_import_prunes_what_the_deleted_projection_left() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    basic_chat(&home);
    let db = temp.path().join("archive/projection.sqlite");
    let assets = rosetta_store::assets::asset_root(&db);
    std::fs::create_dir_all(assets.join(".live-images-v1/abc")).unwrap();
    let digest = |seed: char| seed.to_string().repeat(64);
    let stale = [
        format!("{}.webp", digest('a')),
        format!("{}.png", digest('b')),
        format!("{}.image-v1-tile.webp", digest('a')),
        format!("{}.image-v0-rail.webp", digest('c')),
    ];
    let kept = [
        format!("{}.txt", digest('d')),
        ".image-in-flight".to_string(),
        ".live-images-v1/abc/tile.webp".to_string(),
    ];
    for name in stale.iter().chain(&kept) {
        std::fs::write(assets.join(name), b"left behind").unwrap();
    }

    fresh_serve(&home, &db);
    for name in &stale {
        assert!(!assets.join(name).exists(), "{name} survived publication");
    }
    for name in &kept {
        assert!(assets.join(name).exists(), "{name} was removed");
    }
}
