//! The ledger as the second resort of append growth: a sent file that
//! cannot be captured today is carried from an earlier capture the source
//! already holds, and the ledger is walked only then, once, and never for a
//! claim that is too large to carry.

use std::cell::Cell;

use tempfile::TempDir;

use super::prior::Prior;
use super::tests::{captured, facts, members, mp4, record, statuses, upgrades, write};
use super::*;

const MP4: &str = "video/mp4";

fn digest(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

/// One held capture: `(generation, offset, bytes)`.
type Held<'a> = (u64, u64, &'a [u8]);

/// What the source's ledger holds: one capture of `path` per frame.
fn held(path: &str, frames: &[Held<'_>]) -> Loaded {
    let mut loaded = Loaded::default();
    for (generation, offset, bytes) in frames {
        let ops = record(*offset, *generation, &[(path, bytes.len() as u64, MP4)]);
        loaded.insert(Prior {
            generation: *generation,
            offset: *offset,
            document: captured(&facts(&ops)[0], bytes.to_vec()),
        });
    }
    loaded
}

fn shown(ops: &[IngestOp]) -> Vec<String> {
    assert_eq!(members(ops).len(), upgrades(ops).len());
    members(ops)
        .iter()
        .map(|asset| asset.digest.clone())
        .collect()
}

#[test]
fn a_file_that_cannot_be_captured_is_carried_from_the_ledger_walked_once() {
    let dir = TempDir::new().unwrap();
    let gone = dir.path().join("gone.mp4").to_str().unwrap().to_owned();
    let never = dir.path().join("never.mp4").to_str().unwrap().to_owned();
    let earlier = mp4(64, 1);
    let walks = Cell::new(0);
    let load = || {
        walks.set(walks.get() + 1);
        held(&gone, &[(1, 10, &earlier)])
    };
    let mut kept = None;

    // Two files on one record: one the source captured at an earlier
    // record, one it never captured. Neither is on disk.
    let mut ops = record(
        40,
        1,
        &[(gone.as_str(), 64, MP4), (never.as_str(), 64, MP4)],
    );
    let frames = capture(&mut ops, 40, 1, None, Some(Fallback::new(&mut kept, &load)));
    assert_eq!(statuses(&frames), [Status::Captured, Status::Missing]);
    let (offset, carried) = &frames[0];
    assert_eq!(*offset, 40);
    assert_eq!(carried.path, gone);
    assert_eq!(carried.recovered_from.as_deref(), Some("ledger:1:10"));
    assert_eq!(carried.captured_at, 1234, "when it was first captured");
    assert_eq!(carried.bytes, earlier);
    carried.validate().unwrap();
    assert_eq!(frames[1].1.path, never);
    assert_eq!(frames[1].1.recovered_from, None);
    assert_eq!(shown(&ops), [digest(&earlier)]);
    assert_eq!(walks.get(), 1, "one walk for the record's two failures");

    // A later record of the same pass reuses what the walk found.
    let mut ops = record(90, 1, &[(gone.as_str(), 64, MP4)]);
    let frames = capture(&mut ops, 40, 1, None, Some(Fallback::new(&mut kept, &load)));
    assert_eq!(statuses(&frames), [Status::Captured]);
    assert_eq!(frames[0].1.recovered_from.as_deref(), Some("ledger:1:10"));
    assert_eq!(walks.get(), 1);
    assert!(kept.is_some());
}

#[test]
fn a_file_that_is_captured_never_asks_the_ledger() {
    let dir = TempDir::new().unwrap();
    let earlier = mp4(64, 1);
    let today = mp4(64, 9);
    let path = write(dir.path(), "clip.mp4", &today);
    let walks = Cell::new(0);
    let load = || {
        walks.set(walks.get() + 1);
        held(&path, &[(1, 10, &earlier)])
    };
    let mut kept = None;
    let mut ops = record(40, 1, &[(path.as_str(), 64, MP4)]);
    let frames = capture(&mut ops, 40, 1, None, Some(Fallback::new(&mut kept, &load)));
    // Append growth reads the file first: a new send shows today's bytes.
    assert_eq!(statuses(&frames), [Status::Captured]);
    assert_eq!(frames[0].1.recovered_from, None);
    assert_eq!(frames[0].1.bytes, today);
    assert_eq!(shown(&ops), [digest(&today)]);
    assert_eq!(walks.get(), 0);
    assert!(kept.is_none());

    // A record with no fact at or above the watermark asks nothing either.
    std::fs::remove_file(&path).unwrap();
    let mut ops = record(40, 1, &[(path.as_str(), 64, MP4)]);
    assert!(capture(&mut ops, 41, 1, None, Some(Fallback::new(&mut kept, &load))).is_empty());
    assert_eq!(walks.get(), 0);
}

#[test]
fn every_failed_observation_falls_back_and_an_unusable_capture_is_passed_over() {
    let dir = TempDir::new().unwrap();
    let earlier = mp4(64, 1);
    let missing = dir.path().join("gone.mp4").to_str().unwrap().to_owned();
    let resized = write(dir.path(), "resized.mp4", &mp4(65, 2));
    let rewritten = write(dir.path(), "rewritten.mp4", &[b'x'; 64]);
    let folder = dir.path().join("folder.mp4");
    std::fs::create_dir(&folder).unwrap();
    let folder = folder.to_str().unwrap().to_owned();
    // One record at offset 40 of generation 2 sends `path`; the ledger
    // holds `frames` of `of`.
    let run = |path: &str, of: &str, frames: &[Held<'_>]| {
        let load = || held(of, frames);
        let mut kept = None;
        let mut ops = record(40, 2, &[(path, 64, MP4)]);
        let mut unused = Vec::new();
        let observed = observe_file(&facts(&ops)[0], 0, &mut unused).status;
        assert!(unused.is_empty(), "{path}: the file itself is no capture");
        let frames = capture(&mut ops, 0, 2, None, Some(Fallback::new(&mut kept, &load)));
        (observed, frames, shown(&ops))
    };

    // Gone, another size, the recorded size with other content, not a file:
    // each is carried from the capture at an earlier record.
    for (path, status) in [
        (missing.as_str(), Status::Missing),
        (resized.as_str(), Status::ChangedDuringRead),
        (rewritten.as_str(), Status::Unsupported),
        (folder.as_str(), Status::Unsupported),
    ] {
        let (observed, frames, shown) = run(path, path, &[(2, 10, &earlier)]);
        assert_eq!(observed, status, "{path}");
        assert_eq!(statuses(&frames), [Status::Captured], "{path}");
        assert_eq!(
            frames[0].1.recovered_from.as_deref(),
            Some("ledger:2:10"),
            "{path}"
        );
        assert_eq!(frames[0].1.bytes, earlier, "{path}");
        assert_eq!(shown, [digest(&earlier)], "{path}");
    }

    // Already ledgered at this very coordinate (the record was read once
    // before its batch reached the store): shown, and nothing appended.
    let (_, frames, shown) = run(&missing, &missing, &[(2, 40, &earlier)]);
    assert!(frames.is_empty());
    assert_eq!(shown, [digest(&earlier)]);

    // The same offset in an earlier generation is a copy, like any other.
    let (_, frames, shown) = run(&missing, &missing, &[(1, 40, &earlier)]);
    assert_eq!(statuses(&frames), [Status::Captured]);
    assert_eq!(frames[0].1.recovered_from.as_deref(), Some("ledger:1:40"));
    assert_eq!(shown, [digest(&earlier)]);

    // A held capture that does not project, a capture of another file, or
    // nothing held at all: the observation stands.
    let garbage = [b'x'; 64];
    let cases: [(&str, &[Held<'_>]); 3] = [
        (missing.as_str(), &[(2, 10, &garbage)]),
        (resized.as_str(), &[(2, 10, &earlier)]),
        (missing.as_str(), &[]),
    ];
    for (of, held_frames) in cases {
        let (_, frames, shown) = run(&missing, of, held_frames);
        assert_eq!(statuses(&frames), [Status::Missing]);
        assert_eq!(frames[0].1.recovered_from, None);
        assert!(shown.is_empty());
    }
}

#[test]
fn a_carried_capture_still_counts_against_the_record_budget() {
    let dir = TempDir::new().unwrap();
    let len = 48 * 1024 * 1024_usize;
    // Three 48 MiB videos on one record; the source captured each before
    // and none is on disk now. Two fit the record, the third does not.
    let paths: Vec<String> = (0..3)
        .map(|index| {
            dir.path()
                .join(format!("big-{index}.mp4"))
                .to_str()
                .unwrap()
                .to_owned()
        })
        .collect();
    let mut bytes = super::tests::video_header(b"isom", len);
    bytes.resize(len, 0);
    let load = || {
        let mut loaded = Loaded::default();
        for (index, path) in paths.iter().enumerate() {
            let ops = record(10 + index as u64, 1, &[(path.as_str(), len as u64, MP4)]);
            loaded.insert(Prior {
                generation: 1,
                offset: 10 + index as u64,
                document: captured(&facts(&ops)[0], bytes.clone()),
            });
        }
        loaded
    };
    let mut kept = None;
    let listed: Vec<(&str, u64, &str)> = paths
        .iter()
        .map(|path| (path.as_str(), len as u64, MP4))
        .collect();
    let mut ops = record(40, 1, &listed);
    let frames = capture(&mut ops, 0, 1, None, Some(Fallback::new(&mut kept, &load)));
    assert_eq!(
        statuses(&frames),
        [Status::Captured, Status::Captured, Status::TooLarge]
    );
    assert!(frames[2].1.bytes.is_empty());
    assert_eq!(frames[2].1.recovered_from, None);
    assert_eq!(upgrades(&ops).len(), 2);

    // A claim over its type's limit is not looked up at all: no capture of
    // it could be carried, so the walk would be for nothing.
    let walks = Cell::new(0);
    let counted = || {
        walks.set(walks.get() + 1);
        Loaded::default()
    };
    let mut kept = None;
    let huge = dir.path().join("huge.mp4").to_str().unwrap().to_owned();
    let mut ops = record(
        50,
        1,
        &[(
            huge.as_str(),
            rosetta_core::presented_media::VIDEO_MAX_BYTES + 1,
            MP4,
        )],
    );
    let frames = capture(
        &mut ops,
        0,
        1,
        None,
        Some(Fallback::new(&mut kept, &counted)),
    );
    assert_eq!(statuses(&frames), [Status::TooLarge]);
    assert_eq!(walks.get(), 0);
    assert!(kept.is_none());
}
