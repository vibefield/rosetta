//! Every projection the capture seam can emit goes through a real store:
//! names and paths at the fact's bounds, and video headers written to be
//! hostile. A store validation error is fatal to the daemon, so nothing a
//! projection emits may raise one.

use std::path::Path;

use rosetta_core::MessageRow;
use rosetta_core::timeline::{AttachmentRef, Dimensions, TimelineBody};
use rosetta_store::{Batch, CheckpointUpdate, SourceMode, SourceWrite, Store, queries};
use serde_json::json;
use tempfile::TempDir;

use super::tests::{captured, facts, mp4, png, record, video_header};
use super::*;

/// `moov` > `trak` > `tkhd` (version 0) recording `width` x `height` under
/// the identity matrix: a size is read only through a matrix that proves it.
fn movie(width: u32, height: u32) -> Vec<u8> {
    let mut tkhd = vec![0_u8; 84];
    for (cell, value) in [(0, 0x0001_0000_u32), (4, 0x0001_0000), (8, 0x4000_0000)] {
        tkhd[40 + 4 * cell..44 + 4 * cell].copy_from_slice(&value.to_be_bytes());
    }
    tkhd[76..80].copy_from_slice(&(width << 16).to_be_bytes());
    tkhd[80..84].copy_from_slice(&(height << 16).to_be_bytes());
    let boxed = |kind: &[u8; 4], body: &[u8]| {
        let mut bytes = u32::try_from(body.len() + 8)
            .unwrap()
            .to_be_bytes()
            .to_vec();
        bytes.extend_from_slice(kind);
        bytes.extend_from_slice(body);
        bytes
    };
    boxed(b"moov", &boxed(b"trak", &boxed(b"tkhd", &tkhd)))
}

fn ftyp(major: &[u8; 4]) -> Vec<u8> {
    video_header(major, 28)[..20].to_vec()
}

/// A store with the tool row every test fact upgrades, and its cursor.
fn store_with_opener(projection: &Path) -> (Store, i64) {
    let mut store = Store::open(projection).unwrap();
    store
        .apply(&Batch {
            ops: vec![
                IngestOp::Runtime(
                    serde_json::from_value(json!({
                        "id": "rt", "kind": "claude-code", "data_root": "/agent",
                        "enabled": true, "discovered_at": 1
                    }))
                    .unwrap(),
                ),
                IngestOp::SessionMeta(
                    serde_json::from_value(json!({
                        "id": "root", "native_session_id": "root", "runtime_id": "rt",
                        "root_session_id": "root", "role": "main",
                        "started_at": 1, "updated_at": 1, "stream_state": "streaming"
                    }))
                    .unwrap(),
                ),
            ],
            checkpoints: vec![CheckpointUpdate {
                runtime_id: "rt".into(),
                stream: "transcript".into(),
                path: "source.jsonl".into(),
                session_id: Some("root".into()),
                expected: None,
                next: "fresh".into(),
                updated_at: None,
            }],
            source: None,
        })
        .unwrap();
    let cursor_id = queries::cursor_state(store.conn(), "rt", "source.jsonl")
        .unwrap()
        .unwrap()
        .id;
    (store, cursor_id)
}

fn opener(cursor_id: i64) -> Vec<IngestOp> {
    let row: MessageRow = serde_json::from_value(json!({
        "timeline_id": "tool", "session_id": "root", "ordinal": 0, "kind": "tool_use",
        "ts": 5, "attachments": [], "citations": [], "src_cursor": cursor_id, "src_offset": 0,
        "tool": {
            "correlation": {"key": "call", "derivation": "native_id"},
            "native": {"id": "call", "name": "Bash"},
            "status": {"value": "pending", "basis": "request_observed"},
            "action": {"type": "shell", "command": {"type": "inline", "text": "true"},
                "source_truncated": false, "background": false}
        }
    }))
    .unwrap();
    IngestOp::project_message(row)
}

fn tool_attachments(store: &Store) -> Vec<AttachmentRef> {
    let rows = queries::timeline_page(store.conn(), "root", None, 100).unwrap();
    let [row] = &rows[..] else {
        panic!("one tool row: {rows:?}")
    };
    let TimelineBody::ToolUse { tool } = &row.body else {
        panic!("{row:?}")
    };
    tool.attachments.clone()
}

#[test]
fn every_projection_applies_to_a_real_store() {
    let dir = TempDir::new().unwrap();
    let projection = dir.path().join("projection.sqlite");
    let (mut store, cursor_id) = store_with_opener(&projection);

    // Paths no filesystem would hold, at the fact's own bounds.
    let long_name = format!("/{}.mp4", "n".repeat(16_384 - 5));
    let deep = format!("/{}aclip.mp4", "d/".repeat((16_384 - 10) / 2));
    assert_eq!((long_name.len(), deep.len()), (16_384, 16_384));
    let padded = |mut bytes: Vec<u8>, kind: &[u8]| {
        bytes.extend_from_slice(kind);
        bytes
    };
    let cases: Vec<(&str, &str, &str, Vec<u8>)> = vec![
        ("an mp4", "/work/a.mp4", "video/mp4", mp4(64, 1)),
        (
            "a quicktime file",
            "/work/b.mov",
            "video/quicktime",
            [ftyp(b"qt  "), movie(1920, 1080)].concat(),
        ),
        ("a png", "/work/c.png", "image/png", png()),
        ("a name at the bound", &long_name, "video/mp4", mp4(64, 2)),
        ("a path at the bound", &deep, "video/mp4", mp4(64, 3)),
        (
            "the largest size a track header can record",
            "/work/d.mp4",
            "video/mp4",
            [ftyp(b"isom"), movie(65_535, 65_535)].concat(),
        ),
        (
            "a track header of zero by zero",
            "/work/e.mp4",
            "video/mp4",
            [ftyp(b"isom"), movie(0, 0)].concat(),
        ),
        (
            "a moov that claims four gigabytes",
            "/work/f.mp4",
            "video/mp4",
            padded(
                [ftyp(b"isom"), 0xffff_ffff_u32.to_be_bytes().to_vec()].concat(),
                b"moov",
            ),
        ),
        (
            "a box with a 64-bit size past the file",
            "/work/g.mp4",
            "video/mp4",
            [
                ftyp(b"mp42"),
                padded(1_u32.to_be_bytes().to_vec(), b"moov"),
                u64::MAX.to_be_bytes().to_vec(),
            ]
            .concat(),
        ),
        (
            "a box that runs to the end of the file",
            "/work/h.mp4",
            "video/mp4",
            [
                ftyp(b"avc1"),
                padded(0_u32.to_be_bytes().to_vec(), b"moov"),
                vec![0xff; 40],
            ]
            .concat(),
        ),
        (
            "a moov cut inside its track header",
            "/work/i.mp4",
            "video/mp4",
            [ftyp(b"isom"), movie(640, 480)[..60].to_vec()].concat(),
        ),
        ("nothing but the ftyp", "/work/j.mp4", "video/mp4", {
            ftyp(b"iso6")
        }),
        (
            "an ftyp that claims more than the file",
            "/work/k.mp4",
            "video/mp4",
            padded(4096_u32.to_be_bytes().to_vec(), b"ftypisom"),
        ),
    ];

    let mut ops = opener(cursor_id);
    let mut accepted = Vec::new();
    for (index, (name, path, media_type, bytes)) in cases.iter().enumerate() {
        let mut emitted = record(
            100 + index as u64,
            1,
            &[(path, bytes.len() as u64, media_type)],
        );
        let fact = facts(&emitted).remove(0);
        if project(&fact, &captured(&fact, bytes.clone()), &mut emitted) {
            accepted.push((*name, fact.attachment.id.clone(), bytes.clone()));
        }
        ops.extend(emitted);
    }
    let names: Vec<&str> = accepted.iter().map(|(name, _, _)| *name).collect();
    assert!(names.contains(&"a name at the bound") && names.contains(&"a path at the bound"));
    assert!(names.contains(&"the largest size a track header can record"));
    assert!(names.contains(&"a moov that claims four gigabytes"));
    assert!(!names.contains(&"an ftyp that claims more than the file"));

    store
        .apply(&Batch {
            ops,
            checkpoints: Vec::new(),
            source: Some(SourceWrite {
                cursor_id,
                session_id: "root".into(),
                generation: 1,
                first_sequence: 0,
                mode: SourceMode::Activate,
            }),
        })
        .expect("nothing a projection emits fails the store");

    let attachments = tool_attachments(&store);
    assert_eq!(attachments.len(), cases.len(), "every sent file has a slot");
    let root = rosetta_store::assets::asset_root(&projection);
    for attachment in &attachments {
        attachment.validate().unwrap();
        let captured = accepted.iter().find(|(_, id, _)| *id == attachment.id);
        match (&attachment.source, captured) {
            (AttachmentSource::Asset { asset }, Some((name, _, bytes))) => {
                assert_eq!(asset.byte_len, bytes.len() as u64, "{name}");
                let member = Path::new(&asset.url).file_name().unwrap();
                assert_eq!(std::fs::read(root.join(member)).unwrap(), *bytes, "{name}");
            }
            (AttachmentSource::Reference { .. }, None) => {}
            (source, _) => panic!("{}: {source:?}", attachment.id),
        }
    }
    let sized: Vec<Option<Dimensions>> = attachments
        .iter()
        .filter_map(|attachment| attachment.dimensions.map(Some))
        .collect();
    assert!(sized.contains(&Some(Dimensions {
        width: 65_535,
        height: 65_535
    })));
    assert!(sized.contains(&Some(Dimensions {
        width: 1920,
        height: 1080
    })));
}
