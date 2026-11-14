//! The capture seam and its replay join, with temp files and hand-built
//! facts: every status a file can end with, the ledger-first lookup, the
//! reader seam that moves a file under its read, and the replay slots.

use std::io::Cursor;
use std::path::Path;
use std::time::{Duration, SystemTime};

use base64::Engine;
use rosetta_adapter_support::NormalizeCtx;
use rosetta_adapter_support::presented_media::{PresentedFile, push};
use rosetta_core::ProjectionAsset;
use rosetta_core::presented_media::VIDEO_MAX_BYTES;
use rosetta_core::timeline::{AttachmentRef, Dimensions, EvidenceOrder};
use tempfile::TempDir;

use super::prior::Prior;
use super::*;

const MIB: u64 = 1024 * 1024;
const PNG: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+ip1sAAAAASUVORK5CYII=";

pub(super) fn png() -> Vec<u8> {
    base64::engine::general_purpose::STANDARD
        .decode(PNG)
        .unwrap()
}

/// The 28 bytes a padded video starts with: an `ftyp` box of `major` and
/// one compatible brand, then the header of a `free` box that runs to `len`.
pub(super) fn video_header(major: &[u8; 4], len: usize) -> Vec<u8> {
    assert!(len >= 28);
    let mut bytes = 20_u32.to_be_bytes().to_vec();
    bytes.extend_from_slice(b"ftyp");
    bytes.extend_from_slice(major);
    bytes.extend_from_slice(&[0, 0, 2, 0]);
    bytes.extend_from_slice(b"mp41");
    bytes.extend_from_slice(&u32::try_from(len - 20).unwrap().to_be_bytes());
    bytes.extend_from_slice(b"free");
    bytes
}

/// A well-formed MP4 of exactly `len` bytes; `seed` makes its content its own.
pub(super) fn mp4(len: usize, seed: u8) -> Vec<u8> {
    let mut bytes = video_header(b"isom", len);
    bytes.resize(len, seed);
    bytes
}

fn ctx(offset: u64, generation: u64) -> NormalizeCtx {
    let mut ctx = NormalizeCtx::new("rt", "root");
    ctx.src_offset = Some(offset as i64);
    ctx.source_identity = Some("source.jsonl".into());
    ctx.source_generation = generation;
    ctx
}

/// One record's ops as an adapter emits them: the reference update, then one
/// fact per presentable file.
pub(super) fn record(offset: u64, generation: u64, files: &[(&str, u64, &str)]) -> Vec<IngestOp> {
    let files: Vec<PresentedFile<'_>> = files
        .iter()
        .map(|(path, len, media_type)| PresentedFile {
            path,
            cwd: None,
            len: *len,
            media_type,
        })
        .collect();
    let mut ops = Vec::new();
    let facts = push(
        &ctx(offset, generation),
        "call",
        &format!("result-{offset}"),
        &files,
        Some(7),
        &mut ops,
    );
    assert_eq!(facts.len(), files.len(), "every test file is presentable");
    ops.extend(facts.into_iter().map(IngestOp::PresentedMedia));
    ops
}

pub(super) fn facts(ops: &[IngestOp]) -> Vec<PresentedMedia> {
    ops.iter()
        .filter_map(|op| match op {
            IngestOp::PresentedMedia(fact) => Some(fact.clone()),
            _ => None,
        })
        .collect()
}

pub(super) fn members(ops: &[IngestOp]) -> Vec<&ProjectionAsset> {
    ops.iter()
        .filter_map(|op| match op {
            IngestOp::ProjectionAsset { asset } => Some(asset),
            _ => None,
        })
        .collect()
}

/// The upgrades among `ops`: each tool update whose one attachment is an asset.
pub(super) fn upgrades(ops: &[IngestOp]) -> Vec<(EvidenceOrder, AttachmentRef)> {
    ops.iter()
        .filter_map(|op| match op {
            IngestOp::ToolUpdate {
                evidence, patch, ..
            } => match patch.attachments.as_slice() {
                [attachment] if matches!(attachment.source, AttachmentSource::Asset { .. }) => {
                    Some((evidence.clone(), attachment.clone()))
                }
                _ => None,
            },
            _ => None,
        })
        .collect()
}

pub(super) fn write(dir: &Path, name: &str, bytes: &[u8]) -> String {
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    path.to_str().unwrap().to_owned()
}

pub(super) fn captured(fact: &PresentedMedia, bytes: Vec<u8>) -> CapturedDocument {
    CapturedDocument {
        path: fact.path.clone(),
        captured_at: 1234,
        status: Status::Captured,
        recovered_from: None,
        media: Some(claim(fact)),
        bytes,
    }
}

pub(super) fn statuses(frames: &[(u64, CapturedDocument)]) -> Vec<Status> {
    frames.iter().map(|(_, document)| document.status).collect()
}

#[test]
fn a_captured_frame_projects_the_member_and_the_upgrade_of_its_slot() {
    let bytes = mp4(64, 1);
    let ops = record(40, 2, &[("/work/clip.mp4", 64, "video/mp4")]);
    let fact = facts(&ops).remove(0);
    let mut projected = Vec::new();
    assert!(project(
        &fact,
        &captured(&fact, bytes.clone()),
        &mut projected
    ));
    assert_eq!(projected.len(), 2, "the member, then the upgrade");
    let [member] = members(&projected)[..] else {
        panic!("{projected:?}")
    };
    assert_eq!(member.bytes, bytes, "the member is the recorded bytes");
    assert_eq!(member.media_type, "video/mp4");
    let [(evidence, upgraded)] = &upgrades(&projected)[..] else {
        panic!("{projected:?}")
    };
    assert_eq!(*evidence, fact.evidence);
    assert_eq!(upgraded.id, fact.attachment.id);
    assert_eq!(upgraded.kind, fact.attachment.kind);
    assert_eq!(upgraded.name.as_deref(), Some("clip.mp4"));
    assert_eq!(upgraded.media_type.as_deref(), Some("video/mp4"));
    assert_eq!(upgraded.byte_len, Some(64));
    assert_eq!(
        upgraded.source,
        AttachmentSource::Asset {
            asset: member.reference()
        }
    );
    upgraded.validate().unwrap();
    let IngestOp::ToolUpdate {
        session_id,
        correlation_key,
        at,
        ..
    } = &projected[1]
    else {
        panic!("{projected:?}")
    };
    assert_eq!(
        (session_id.as_str(), correlation_key.as_str(), *at),
        ("root", "call", Some(7))
    );

    // An image carries the size its header proves.
    let ops = record(
        41,
        2,
        &[("/work/shot.png", png().len() as u64, "image/png")],
    );
    let image = facts(&ops).remove(0);
    let mut projected = Vec::new();
    assert!(project(&image, &captured(&image, png()), &mut projected));
    assert_eq!(
        upgrades(&projected)[0].1.dimensions,
        Some(Dimensions {
            width: 1,
            height: 1
        })
    );
}

#[test]
fn a_frame_that_is_not_its_facts_capture_projects_nothing() {
    let bytes = mp4(64, 1);
    let ops = record(40, 2, &[("/work/clip.mp4", 64, "video/mp4")]);
    let fact = facts(&ops).remove(0);
    let good = captured(&fact, bytes.clone());
    type Break = fn(&mut CapturedDocument);
    let cases: [(&str, Break); 10] = [
        ("missing", |d| {
            d.status = Status::Missing;
            d.bytes.clear()
        }),
        ("too large", |d| d.status = Status::TooLarge),
        ("unsupported", |d| d.status = Status::Unsupported),
        ("changed", |d| d.status = Status::ChangedDuringRead),
        ("an html frame", |d| d.media = None),
        ("another type", |d| {
            d.media.as_mut().unwrap().media_type = "video/quicktime".into()
        }),
        ("another size", |d| {
            d.media.as_mut().unwrap().expected_len = 65
        }),
        ("another path", |d| d.path = "/work/other.mp4".into()),
        ("short bytes", |d| d.bytes.truncate(63)),
        ("not a video", |d| d.bytes = vec![b'x'; 64]),
    ];
    for (name, break_it) in cases {
        let mut document = good.clone();
        break_it(&mut document);
        let mut projected = Vec::new();
        assert!(!project(&fact, &document, &mut projected), "{name}");
        assert!(projected.is_empty(), "{name}");
    }
    // A fact that could not be applied is refused whatever the frame holds.
    let mut broken = fact.clone();
    broken.correlation_key.clear();
    let mut projected = Vec::new();
    assert!(!project(&broken, &good, &mut projected));
    // The declared type decides: MP4 bytes are not the QuickTime they claim.
    let ops = record(40, 2, &[("/work/clip.mov", 64, "video/quicktime")]);
    let mov = facts(&ops).remove(0);
    assert!(!project(&mov, &captured(&mov, bytes), &mut projected));
    assert!(projected.is_empty());
}

#[test]
fn every_file_ends_with_the_status_of_what_was_there() {
    let dir = TempDir::new().unwrap();
    let ok = write(dir.path(), "ok.mp4", &mp4(64, 1));
    let image = write(dir.path(), "shot.png", &png());
    let missing = dir.path().join("gone.mp4").to_str().unwrap().to_owned();
    let grown = write(dir.path(), "grown.mp4", &mp4(65, 2));
    let shrunk = write(dir.path(), "shrunk.mp4", &mp4(63, 3));
    let text = write(dir.path(), "text.mp4", &[b'x'; 64]);
    let mislabeled = write(dir.path(), "video.png", &mp4(64, 4));
    let folder = dir.path().join("folder.mp4");
    std::fs::create_dir(&folder).unwrap();
    let folder = folder.to_str().unwrap().to_owned();
    let link = dir.path().join("link.mp4");
    std::os::unix::fs::symlink(&ok, &link).unwrap();
    let link = link.to_str().unwrap().to_owned();
    // Neither exists: a claim over the limit is decided before any open.
    let huge_video = dir.path().join("huge.mp4").to_str().unwrap().to_owned();
    let huge_image = dir.path().join("huge.png").to_str().unwrap().to_owned();

    let png_len = png().len() as u64;
    let files = [
        (ok.as_str(), 64, "video/mp4", Status::Captured),
        (image.as_str(), png_len, "image/png", Status::Captured),
        (missing.as_str(), 64, "video/mp4", Status::Missing),
        (grown.as_str(), 64, "video/mp4", Status::ChangedDuringRead),
        (shrunk.as_str(), 64, "video/mp4", Status::ChangedDuringRead),
        (text.as_str(), 64, "video/mp4", Status::Unsupported),
        (mislabeled.as_str(), 64, "image/png", Status::Unsupported),
        (folder.as_str(), 64, "video/mp4", Status::Unsupported),
        (link.as_str(), 64, "video/mp4", Status::Unsupported),
        (
            huge_video.as_str(),
            VIDEO_MAX_BYTES + 1,
            "video/mp4",
            Status::TooLarge,
        ),
        (
            huge_image.as_str(),
            32 * MIB + 1,
            "image/png",
            Status::TooLarge,
        ),
    ];
    let listed: Vec<(&str, u64, &str)> = files
        .iter()
        .map(|(path, len, media_type, _)| (*path, *len, *media_type))
        .collect();
    let mut ops = record(40, 1, &listed);
    let before = ops.clone();
    let frames = capture(&mut ops, 0, 1, None, None);

    assert_eq!(
        statuses(&frames),
        files
            .iter()
            .map(|(_, _, _, status)| *status)
            .collect::<Vec<_>>()
    );
    for ((offset, document), (path, len, media_type, status)) in frames.iter().zip(&files) {
        assert_eq!(*offset, 40);
        assert_eq!(document.path, *path);
        assert_eq!(
            document.media,
            Some(MediaClaim {
                media_type: (*media_type).to_owned(),
                expected_len: *len
            })
        );
        assert_eq!(document.recovered_from, None);
        assert!(document.captured_at > 0);
        assert_eq!(
            document.bytes.is_empty(),
            *status != Status::Captured,
            "{path}: only a capture carries bytes"
        );
        document.validate().unwrap();
    }
    assert_eq!(frames[0].1.bytes, mp4(64, 1));
    assert_eq!(frames[1].1.bytes, png());

    // What the adapter emitted is untouched; only the two captures add ops.
    assert_eq!(ops[..before.len()], before[..]);
    let all = facts(&ops);
    let upgraded = upgrades(&ops);
    assert_eq!(upgraded.len(), 2);
    assert_eq!(members(&ops).len(), 2);
    for (index, (evidence, attachment)) in upgraded.iter().enumerate() {
        assert_eq!(*evidence, all[index].evidence);
        assert_eq!(attachment.id, all[index].attachment.id);
    }
    assert_eq!(ops.len(), before.len() + 4);

    // The device and process trees. The producer emits no fact for such a
    // path and the seam skips one that names it; observed anyway, it is
    // unsupported whatever the claim says, with nothing read or projected.
    let fact = all[0].clone();
    for path in ["/dev/null", "/dev/fd/0"] {
        let device = PresentedMedia {
            path: path.to_owned(),
            ..fact.clone()
        };
        let mut ops = vec![IngestOp::PresentedMedia(device.clone())];
        assert!(capture(&mut ops, 0, 1, None, None).is_empty(), "{path}");
        assert_eq!(ops.len(), 1, "{path}");
        let mut projected = Vec::new();
        let document = observe_file(&device, 0, &mut projected);
        assert_eq!(document.status, Status::Unsupported, "{path}");
        assert!(document.bytes.is_empty() && projected.is_empty(), "{path}");
    }
}

#[cfg(unix)]
#[test]
fn a_device_or_process_path_is_refused_before_any_open() {
    use std::io::Seek;
    use std::os::fd::AsRawFd;

    let dir = TempDir::new().unwrap();
    let bytes = mp4(64, 1);
    let path = write(dir.path(), "held.mp4", &bytes);
    // A descriptor this process holds on a regular file of the claimed
    // size. Where `/dev/fd/N` duplicates descriptor N, opening it would
    // pass for that file and reading it would move the holder's offset.
    let mut held = std::fs::File::open(&path).unwrap();
    let through = format!("/dev/fd/{}", held.as_raw_fd());
    assert!(matches!(open_regular(&through), Err(Status::Unsupported)));
    let fact = PresentedMedia {
        path: through,
        ..facts(&record(40, 1, &[(path.as_str(), 64, "video/mp4")])).remove(0)
    };
    let mut projected = Vec::new();
    let document = observe_file(&fact, 0, &mut projected);
    assert_eq!(document.status, Status::Unsupported);
    assert!(document.bytes.is_empty() && projected.is_empty());
    assert_eq!(
        held.stream_position().unwrap(),
        0,
        "the held descriptor was not read through"
    );

    // The same descriptor through a directory that is a symlink to the
    // device tree: `O_NOFOLLOW` guards only the last component.
    #[cfg(unix)]
    {
        let link = dir.path().join("d");
        std::os::unix::fs::symlink("/dev", &link).unwrap();
        let around = format!("{}/fd/{}", link.to_str().unwrap(), held.as_raw_fd());
        assert!(matches!(open_regular(&around), Err(Status::Unsupported)));
        let fact = PresentedMedia {
            path: around,
            ..facts(&record(40, 1, &[(path.as_str(), 64, "video/mp4")])).remove(0)
        };
        let mut projected = Vec::new();
        let document = observe_file(&fact, 0, &mut projected);
        assert_eq!(document.status, Status::Unsupported);
        assert!(document.bytes.is_empty() && projected.is_empty());
        assert_eq!(
            held.stream_position().unwrap(),
            0,
            "the held descriptor was not read through a symlinked directory"
        );
    }

    for refused in [
        "/dev",
        "/dev/",
        "/dev/null",
        "/dev/fd/0",
        "/dev/stdin",
        "/proc",
        "/proc/self/fd/0",
        "/proc/1/mem",
        // The default macOS volume folds case.
        "/DEV/null",
        "/Proc/self/fd/0",
    ] {
        assert!(
            matches!(open_regular(refused), Err(Status::Unsupported)),
            "{refused}"
        );
    }

    // Only the root component decides: the same file by its own path, and
    // a folder that is merely named like one, open as before.
    assert!(open_regular(&path).is_ok());
    for folder in ["dev", "proc"] {
        std::fs::create_dir(dir.path().join(folder)).unwrap();
        let inside = write(&dir.path().join(folder), "clip.mp4", &bytes);
        assert!(open_regular(&inside).is_ok(), "{inside}");
    }
    // A root folder whose name only starts like one is looked for.
    for absent in ["/devices-rosetta-test/clip.mp4", "/proc.rosetta-test"] {
        assert!(
            matches!(open_regular(absent), Err(Status::Missing)),
            "{absent}"
        );
    }
}

#[test]
fn a_fact_that_does_not_validate_is_not_observed_and_a_bad_claim_is_unsupported() {
    let dir = TempDir::new().unwrap();
    let ok = write(dir.path(), "ok.mp4", &mp4(64, 1));
    let good = record(40, 1, &[(ok.as_str(), 64, "video/mp4")]);
    let fact = facts(&good).remove(0);

    type Break = fn(&mut PresentedMedia);
    let cases: [(&str, Break); 3] = [
        ("a relative path", |f| f.path = "work/ok.mp4".into()),
        ("a network path", |f| f.path = "//host/share/ok.mp4".into()),
        ("a type that is not presented", |f| {
            f.media_type = "video/webm".into()
        }),
    ];
    for (name, break_it) in cases {
        let mut broken = fact.clone();
        break_it(&mut broken);
        // The seam skips it: no frame, no op.
        let mut ops = vec![IngestOp::PresentedMedia(broken.clone())];
        assert!(capture(&mut ops, 0, 1, None, None).is_empty(), "{name}");
        assert_eq!(ops.len(), 1, "{name}");
        // Observed anyway, it is unsupported and nothing is read.
        let mut ops = Vec::new();
        let document = observe_file(&broken, 0, &mut ops);
        assert_eq!(document.status, Status::Unsupported, "{name}");
        assert!(document.bytes.is_empty() && ops.is_empty(), "{name}");
    }
}

#[test]
fn one_record_captures_at_most_the_record_budget() {
    let dir = TempDir::new().unwrap();
    let ok = write(dir.path(), "ok.mp4", &mp4(64, 1));
    let ops = record(40, 1, &[(ok.as_str(), 64, "video/mp4")]);
    let fact = facts(&ops).remove(0);
    // Exactly at the budget is captured; one byte past it is not read.
    let mut projected = Vec::new();
    let fits = observe_file(&fact, RECORD_MAX_BYTES - 64, &mut projected);
    assert_eq!(fits.status, Status::Captured);
    assert_eq!(projected.len(), 2);
    let mut projected = Vec::new();
    let over = observe_file(&fact, RECORD_MAX_BYTES - 63, &mut projected);
    assert_eq!(over.status, Status::TooLarge);
    assert!(over.bytes.is_empty() && projected.is_empty());

    // Through the seam: three 48 MiB videos on one record, sparse on disk.
    let len = 48 * MIB;
    let paths: Vec<String> = (0..3)
        .map(|index| {
            let path = dir.path().join(format!("big-{index}.mp4"));
            std::fs::write(&path, video_header(b"isom", len as usize)).unwrap();
            std::fs::OpenOptions::new()
                .write(true)
                .open(&path)
                .unwrap()
                .set_len(len)
                .unwrap();
            path.to_str().unwrap().to_owned()
        })
        .collect();
    let listed: Vec<(&str, u64, &str)> = paths
        .iter()
        .map(|path| (path.as_str(), len, "video/mp4"))
        .collect();
    let mut ops = record(41, 1, &listed);
    let frames = capture(&mut ops, 0, 1, None, None);
    assert_eq!(
        statuses(&frames),
        [Status::Captured, Status::Captured, Status::TooLarge]
    );
    assert_eq!(frames[0].1.bytes.len() as u64, len);
    assert!(frames[2].1.bytes.is_empty());
    assert_eq!(upgrades(&ops).len(), 2);
}

/// A `stat` that answers from a script, then fails.
fn scripted(answers: Vec<Option<FileStat>>) -> impl FnMut() -> Option<FileStat> {
    let mut answers = answers.into_iter();
    move || answers.next().flatten()
}

fn still(len: u64) -> Option<FileStat> {
    Some(FileStat {
        is_file: true,
        len,
        modified: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(100)),
    })
}

#[test]
fn a_file_that_moves_under_the_read_is_changed_during_read() {
    let bytes = mp4(64, 1);
    let ops = record(40, 1, &[("/work/clip.mp4", 64, "video/mp4")]);
    let fact = facts(&ops).remove(0);
    let touched = Some(FileStat {
        modified: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(101)),
        ..still(64).unwrap()
    });
    let directory = Some(FileStat {
        is_file: false,
        ..still(64).unwrap()
    });
    let longer = [bytes.clone(), vec![0; 8]].concat();
    type Case<'a> = (&'a str, &'a [u8], Vec<Option<FileStat>>, Status);
    let cases: [Case<'_>; 9] = [
        (
            "held still",
            &bytes,
            vec![still(64), still(64)],
            Status::Captured,
        ),
        (
            "rewritten in place",
            &bytes,
            vec![still(64), touched],
            Status::ChangedDuringRead,
        ),
        (
            "grew after the read",
            &bytes,
            vec![still(64), still(65)],
            Status::ChangedDuringRead,
        ),
        (
            "a short read",
            &bytes[..50],
            vec![still(64), still(64)],
            Status::ChangedDuringRead,
        ),
        (
            "a long read",
            &longer,
            vec![still(64), still(64)],
            Status::ChangedDuringRead,
        ),
        (
            "another size before any read",
            &bytes,
            vec![still(63)],
            Status::ChangedDuringRead,
        ),
        (
            "gone after the read",
            &bytes,
            vec![still(64), None],
            Status::ChangedDuringRead,
        ),
        ("no metadata", &bytes, vec![None], Status::Unsupported),
        ("not a file", &bytes, vec![directory], Status::Unsupported),
    ];
    for (name, content, answers, status) in cases {
        let mut projected = Vec::new();
        let document = read_file(
            &fact,
            &mut Cursor::new(content),
            scripted(answers),
            &mut projected,
        );
        assert_eq!(document.status, status, "{name}");
        if status == Status::Captured {
            assert_eq!(document.bytes, bytes);
            assert_eq!(projected.len(), 2);
        } else {
            assert!(document.bytes.is_empty(), "{name}: no bytes are kept");
            assert!(projected.is_empty(), "{name}: nothing is projected");
        }
        document.validate().unwrap();
    }
}

#[test]
fn the_stable_read_without_a_recorded_size_keeps_the_html_outcomes() {
    let read = |content: &[u8], answers, limit| {
        let mut bytes = Vec::new();
        let status = read_stable(
            &mut Cursor::new(content),
            scripted(answers),
            limit,
            None,
            &mut bytes,
        );
        (status, bytes.len())
    };
    assert_eq!(
        read(b"12345", vec![still(5), still(5)], 5),
        (Status::Captured, 5)
    );
    // Over the limit by its metadata: not read at all.
    assert_eq!(read(b"123456", vec![still(6)], 5), (Status::TooLarge, 0));
    // Over the limit by what the read returned: one byte past it, no more.
    assert_eq!(
        read(b"1234567890", vec![still(5), still(5)], 5),
        (Status::TooLarge, 6)
    );
    // Shorter than its metadata, and the metadata did not move.
    assert_eq!(
        read(b"123", vec![still(5), still(5)], 5).0,
        Status::ChangedDuringRead
    );
    assert_eq!(read(b"123", vec![None], 5).0, Status::Unsupported);
    assert_eq!(
        read(b"123", vec![still(3), None], 5).0,
        Status::ChangedDuringRead
    );
}

#[test]
fn only_facts_at_or_above_the_ledger_watermark_are_observed_once_each() {
    let dir = TempDir::new().unwrap();
    let ok = write(dir.path(), "ok.mp4", &mp4(64, 1));
    let mut ops = record(40, 1, &[(ok.as_str(), 64, "video/mp4")]);
    let before = ops.clone();
    // Already ledgered for this generation: not observed again.
    assert!(capture(&mut ops, 41, 1, None, None).is_empty());
    assert_eq!(ops, before);
    assert!(has_fact(&ops));
    assert!(!has_fact(&ops[..1]));

    // The same coordinate twice is one observation.
    let fact = facts(&ops).remove(0);
    ops.push(IngestOp::PresentedMedia(fact));
    let frames = capture(&mut ops, 40, 1, None, None);
    assert_eq!(statuses(&frames), [Status::Captured]);
    assert_eq!(upgrades(&ops).len(), 1);
}

#[test]
fn the_ledger_is_asked_before_the_filesystem() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("clip.mp4").to_str().unwrap().to_owned();
    let earlier = mp4(64, 1);
    // Today's file has the recorded size and other bytes.
    let today = mp4(64, 9);
    std::fs::write(&path, &today).unwrap();
    let digest = |bytes: &[u8]| blake3::hash(bytes).to_hex().to_string();
    let prior = |generation, offset, bytes: &[u8]| {
        let ops = record(offset, generation, &[(path.as_str(), 64, "video/mp4")]);
        let mut loaded = Loaded::default();
        loaded.insert(Prior {
            generation,
            offset,
            document: captured(&facts(&ops)[0], bytes.to_vec()),
        });
        loaded
    };
    let run = |loaded: &Loaded| {
        let mut ops = record(40, 3, &[(path.as_str(), 64, "video/mp4")]);
        let frames = capture(&mut ops, 0, 3, Some(loaded), None);
        let shown: Vec<String> = members(&ops)
            .iter()
            .map(|asset| asset.digest.clone())
            .collect();
        assert_eq!(shown.len(), upgrades(&ops).len());
        (frames, shown)
    };

    // The exact coordinate of this generation: shown, and nothing appended.
    let (frames, shown) = run(&prior(3, 40, &earlier));
    assert!(frames.is_empty());
    assert_eq!(shown, [digest(&earlier)]);

    // An earlier generation: a copy at this coordinate that says where it
    // came from and when it was first captured.
    let (frames, shown) = run(&prior(2, 40, &earlier));
    let [(offset, copy)] = &frames[..] else {
        panic!("{frames:?}")
    };
    assert_eq!(*offset, 40);
    assert_eq!(copy.status, Status::Captured);
    assert_eq!(copy.captured_at, 1234);
    assert_eq!(copy.recovered_from.as_deref(), Some("ledger:2:40"));
    assert_eq!(copy.path, path);
    assert_eq!(
        copy.media,
        Some(MediaClaim {
            media_type: "video/mp4".into(),
            expected_len: 64
        })
    );
    assert_eq!(copy.bytes, earlier);
    copy.validate().unwrap();
    assert_eq!(shown, [digest(&earlier)], "today's file was not read");

    // The same path and claim at another record of this generation: a copy.
    let (frames, shown) = run(&prior(3, 99, &earlier));
    assert_eq!(statuses(&frames), [Status::Captured]);
    assert_eq!(frames[0].1.recovered_from.as_deref(), Some("ledger:3:99"));
    assert_eq!(frames[0].1.captured_at, 1234);
    assert_eq!(shown, [digest(&earlier)]);

    // A prior that does not project is passed over for the filesystem.
    let (frames, shown) = run(&prior(2, 40, &[b'x'; 64]));
    assert_eq!(statuses(&frames), [Status::Captured]);
    assert_eq!(frames[0].1.recovered_from, None);
    assert_eq!(frames[0].1.bytes, today);
    assert_eq!(shown, [digest(&today)]);
    std::fs::remove_file(&path).unwrap();
    let (frames, shown) = run(&prior(2, 40, &[b'x'; 64]));
    assert_eq!(statuses(&frames), [Status::Missing]);
    assert!(shown.is_empty());

    // Nothing held for this claim: the filesystem, as with no ledger at all.
    let other = dir.path().join("other.mp4").to_str().unwrap().to_owned();
    let mut loaded = Loaded::default();
    let elsewhere = record(40, 2, &[(other.as_str(), 64, "video/mp4")]);
    loaded.insert(Prior {
        generation: 2,
        offset: 40,
        document: captured(&facts(&elsewhere)[0], earlier.clone()),
    });
    let (frames, _) = run(&loaded);
    assert_eq!(statuses(&frames), [Status::Missing]);

    // A carried capture counts against the record's budget like any other.
    let ops = record(40, 3, &[(path.as_str(), 64, "video/mp4")]);
    let fact = facts(&ops).remove(0);
    let mut projected = Vec::new();
    let loaded = prior(2, 40, &earlier);
    assert!(carry(&fact, &loaded, 3, RECORD_MAX_BYTES - 63, &mut projected).is_none());
    assert!(projected.is_empty());
    assert!(carry(&fact, &loaded, 3, RECORD_MAX_BYTES - 64, &mut projected).is_some());
    assert_eq!(projected.len(), 2);
}

#[test]
fn slots_keep_the_first_fact_per_coordinate_and_evict_the_lowest_offset() {
    let at = |offset| record(offset, 1, &[("/work/clip.mp4", 64, "video/mp4")]);
    let mut slots = Slots::with_cap(2);
    slots.note(&at(30));
    slots.note(&at(10));
    assert_eq!(slots.len(), 2);
    slots.note(&at(20));
    assert_eq!(slots.len(), 2);
    assert!(slots.get(10, "/work/clip.mp4").is_none(), "the lowest goes");
    assert!(slots.get(20, "/work/clip.mp4").is_some());
    assert!(slots.get(30, "/work/clip.mp4").is_some());
    // A fact below everything held is the one that goes.
    slots.note(&at(5));
    assert!(slots.get(5, "/work/clip.mp4").is_none());
    assert_eq!(slots.len(), 2);

    // One coordinate, two facts: the first stays.
    let mut slots = Slots::default();
    let first = record(
        40,
        1,
        &[
            ("/work/a.mp4", 64, "video/mp4"),
            ("/work/b.mp4", 64, "video/mp4"),
        ],
    );
    slots.note(&first);
    assert_eq!(slots.len(), 2, "one record's files are separate slots");
    let mut again = facts(&first).remove(0);
    again.expected_len = 99;
    slots.note(&[IngestOp::PresentedMedia(again)]);
    assert_eq!(slots.get(40, "/work/a.mp4").unwrap().expected_len, 64);
    assert!(slots.get(40, "/work/c.mp4").is_none());
    assert!(slots.get(41, "/work/a.mp4").is_none());
    slots.remove(40, "/work/a.mp4");
    assert!(slots.get(40, "/work/a.mp4").is_none());
    assert_eq!(slots.len(), 1);

    // The default cap is the documented one.
    let mut slots = Slots::default();
    for offset in 0..65_540 {
        slots.note(&at(offset));
    }
    assert_eq!(slots.len(), 65_536);
    assert!(slots.get(3, "/work/clip.mp4").is_none());
    assert!(slots.get(4, "/work/clip.mp4").is_some());
}

#[test]
fn replay_takes_the_first_captured_frame_that_projects() {
    let a = mp4(64, 1);
    let b = mp4(64, 2);
    let ops = record(40, 1, &[("/work/clip.mp4", 64, "video/mp4")]);
    let fact = facts(&ops).remove(0);
    let missing = CapturedDocument {
        status: Status::Missing,
        ..captured(&fact, Vec::new())
    };
    let garbage = captured(&fact, vec![b'x'; 64]);
    let digest = |bytes: &[u8]| blake3::hash(bytes).to_hex().to_string();
    let replay = |frames: &[&CapturedDocument]| {
        let mut slots = Slots::default();
        slots.note(&ops);
        let mut projected = Vec::new();
        for frame in frames {
            project_document("root", 40, frame, &mut slots, &mut projected);
        }
        let shown: Vec<String> = members(&projected)
            .iter()
            .map(|asset| asset.digest.clone())
            .collect();
        assert_eq!(shown.len(), upgrades(&projected).len());
        assert_eq!(projected.len(), shown.len() * 2);
        (shown, slots.len())
    };
    let a_frame = captured(&fact, a.clone());
    let b_frame = captured(&fact, b);
    assert_eq!(replay(&[&missing, &a_frame]), (vec![digest(&a)], 0));
    assert_eq!(replay(&[&a_frame, &missing]), (vec![digest(&a)], 0));
    assert_eq!(replay(&[&a_frame, &b_frame]), (vec![digest(&a)], 0));
    // A captured frame that does not project leaves the slot for one that does.
    assert_eq!(replay(&[&garbage, &a_frame]), (vec![digest(&a)], 0));
    // Nothing captured: the slot stays and the row stays a reference.
    assert_eq!(replay(&[&missing, &missing]), (Vec::new(), 1));

    // A frame no fact was noted for is ignored: another record, another path.
    let mut slots = Slots::default();
    slots.note(&ops);
    let mut projected = Vec::new();
    project_document("root", 41, &a_frame, &mut slots, &mut projected);
    let other = CapturedDocument {
        path: "/work/other.mp4".into(),
        ..a_frame.clone()
    };
    project_document("root", 40, &other, &mut slots, &mut projected);
    project_document("root", 40, &a_frame, &mut Slots::default(), &mut projected);
    assert!(projected.is_empty());
    assert_eq!(slots.len(), 1);

    // An HTML frame still projects as it always has, slots untouched.
    let html = CapturedDocument {
        path: "/work/page.html".into(),
        captured_at: 5,
        status: Status::Captured,
        recovered_from: None,
        media: None,
        bytes: b"<h1>page</h1>".to_vec(),
    };
    let mut by_seam = Vec::new();
    project_document("root", 40, &html, &mut slots, &mut by_seam);
    let mut direct = Vec::new();
    crate::html_artifacts::project("root", 40, &html, &mut direct);
    assert_eq!(by_seam, direct);
    assert_eq!(by_seam.len(), 2);
    assert_eq!(slots.len(), 1);
}
