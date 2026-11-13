//! The held captures and their budget, then [`PriorCaptures::load`] against
//! real ledgers in temp directories.

use std::fs;

use rosetta_capture::segment_index::INDEX_FILE;
use rosetta_capture::{Ledger, SegmentCodec, SourceDef};

use super::*;

fn claim(expected_len: u64) -> MediaClaim {
    MediaClaim {
        media_type: "video/mp4".into(),
        expected_len,
    }
}

fn captured(path: &str, bytes: &[u8]) -> CapturedDocument {
    CapturedDocument {
        path: path.into(),
        captured_at: 1,
        status: DocumentCaptureStatus::Captured,
        recovered_from: None,
        media: Some(claim(bytes.len() as u64)),
        bytes: bytes.to_vec(),
    }
}

fn prior(generation: u64, offset: u64, path: &str, bytes: &[u8]) -> Prior {
    Prior {
        generation,
        offset,
        document: captured(path, bytes),
    }
}

fn found(loaded: &Loaded, offset: u64, path: &str, len: u64) -> Option<(u64, u64, Vec<u8>)> {
    loaded
        .find(offset, path, &claim(len))
        .map(|prior| (prior.generation, prior.offset, prior.document.bytes.clone()))
}

#[test]
fn only_captured_media_frames_are_kept() {
    let mut loaded = Loaded::default();
    // An HTML document has no claim.
    loaded.insert(Prior {
        generation: 1,
        offset: 0,
        document: CapturedDocument {
            media: None,
            ..captured("/a.mp4", b"html")
        },
    });
    // An observation that captured nothing has no bytes to carry.
    for status in [
        DocumentCaptureStatus::Missing,
        DocumentCaptureStatus::TooLarge,
        DocumentCaptureStatus::Unsupported,
        DocumentCaptureStatus::ChangedDuringRead,
    ] {
        loaded.insert(Prior {
            generation: 1,
            offset: 0,
            document: CapturedDocument {
                status,
                bytes: Vec::new(),
                ..captured("/a.mp4", b"gone")
            },
        });
    }
    assert_eq!(found(&loaded, 0, "/a.mp4", 4), None);
    assert_eq!(loaded.bytes, 0);

    loaded.insert(prior(1, 0, "/a.mp4", b"kept"));
    assert_eq!(
        found(&loaded, 0, "/a.mp4", 4),
        Some((1, 0, b"kept".to_vec()))
    );
    assert_eq!(loaded.bytes, 4);
}

#[test]
fn the_newest_generation_wins_and_the_first_frame_within_one() {
    let mut loaded = Loaded::default();
    loaded.insert(prior(2, 10, "/a.mp4", b"two!"));
    // Same generation: the first inserted stays.
    loaded.insert(prior(2, 10, "/a.mp4", b"late"));
    assert_eq!(
        found(&loaded, 10, "/a.mp4", 4),
        Some((2, 10, b"two!".to_vec()))
    );
    // A lower generation never replaces a higher one.
    loaded.insert(prior(1, 10, "/a.mp4", b"old!"));
    assert_eq!(
        found(&loaded, 10, "/a.mp4", 4),
        Some((2, 10, b"two!".to_vec()))
    );
    // A higher generation replaces, and the bytes are accounted once.
    loaded.insert(prior(5, 10, "/a.mp4", b"five"));
    assert_eq!(
        found(&loaded, 10, "/a.mp4", 4),
        Some((5, 10, b"five".to_vec()))
    );
    assert_eq!(loaded.bytes, 4);
    assert_eq!(loaded.entries.len(), 1);
}

#[test]
fn a_claim_is_matched_at_its_offset_first_then_anywhere() {
    let mut loaded = Loaded::default();
    loaded.insert(prior(1, 10, "/a.mp4", b"g1@10"));
    loaded.insert(prior(3, 40, "/a.mp4", b"g3@40"));
    loaded.insert(prior(3, 20, "/a.mp4", b"g3@20"));
    loaded.insert(prior(2, 30, "/a.mp4", b"g2@30"));
    // The exact coordinate wins even over a newer generation elsewhere.
    assert_eq!(
        found(&loaded, 10, "/a.mp4", 5),
        Some((1, 10, b"g1@10".to_vec()))
    );
    assert_eq!(
        found(&loaded, 30, "/a.mp4", 5),
        Some((2, 30, b"g2@30".to_vec()))
    );
    // Elsewhere: the newest generation, then the lowest offset.
    assert_eq!(
        found(&loaded, 99, "/a.mp4", 5),
        Some((3, 20, b"g3@20".to_vec()))
    );
    assert_eq!(
        found(&loaded, 0, "/a.mp4", 5),
        Some((3, 20, b"g3@20".to_vec()))
    );

    // Another path, size or type is another claim, at any offset.
    assert_eq!(found(&loaded, 10, "/b.mp4", 5), None);
    assert_eq!(found(&loaded, 10, "/a.mp4", 6), None);
    let quicktime = MediaClaim {
        media_type: "video/quicktime".into(),
        expected_len: 5,
    };
    assert!(loaded.find(10, "/a.mp4", &quicktime).is_none());

    // Two claims at one record are held side by side.
    loaded.insert(prior(1, 10, "/b.mp4", b"b@10!"));
    assert_eq!(
        found(&loaded, 10, "/b.mp4", 5),
        Some((1, 10, b"b@10!".to_vec()))
    );
    assert_eq!(
        found(&loaded, 10, "/a.mp4", 5),
        Some((1, 10, b"g1@10".to_vec()))
    );
    assert_eq!(loaded.entries.len(), 5);
    assert_eq!(loaded.bytes, 25);
}

#[test]
fn retained_bytes_never_exceed_the_budget() {
    let mut loaded = Loaded::with_budget(10);
    loaded.insert(prior(1, 0, "/a.mp4", b"12345"));
    loaded.insert(prior(1, 1, "/b.mp4", b"12345"));
    assert_eq!(loaded.bytes, 10, "exactly the budget is held");
    // A new claim that would pass the budget is not retained: nothing held
    // is of a lower generation than it.
    loaded.insert(prior(1, 2, "/c.mp4", b"1"));
    assert_eq!(found(&loaded, 2, "/c.mp4", 1), None);
    assert_eq!(loaded.bytes, 10);

    // A replacement gives its old bytes back first: same size fits.
    loaded.insert(prior(2, 0, "/a.mp4", b"abcde"));
    assert_eq!(
        found(&loaded, 0, "/a.mp4", 5),
        Some((2, 0, b"abcde".to_vec()))
    );
    assert_eq!(loaded.bytes, 10);

    // A frame larger than the whole budget is never held.
    let mut small = Loaded::with_budget(3);
    small.insert(prior(1, 0, "/a.mp4", b"1234"));
    assert_eq!(found(&small, 0, "/a.mp4", 4), None);
    assert_eq!(small.bytes, 0);
    small.insert(prior(1, 0, "/a.mp4", b"123"));
    assert_eq!(small.bytes, 3);
    assert!(Loaded::with_budget(0).entries.is_empty());
    assert_eq!(Loaded::default().budget, PRIOR_MAX_BYTES);
}

#[test]
fn a_newer_generation_takes_the_room_of_older_ones_lowest_first() {
    // Two rewrites shifted every offset, so each generation's carried copy
    // of `/a.mp4` is its own entry; the third generation also captured a
    // new file. In ledger order the copies alone fill the budget.
    let mut loaded = Loaded::with_budget(12);
    loaded.insert(prior(1, 10, "/a.mp4", b"aaaa"));
    loaded.insert(prior(2, 20, "/a.mp4", b"aaaa"));
    loaded.insert(prior(3, 30, "/a.mp4", b"aaaa"));
    assert_eq!(loaded.bytes, 12);
    loaded.insert(prior(3, 31, "/new.mp4", b"yyyy"));
    assert_eq!(
        found(&loaded, 31, "/new.mp4", 4),
        Some((3, 31, b"yyyy".to_vec())),
        "the newest generation's only capture of a file is held"
    );
    // The lowest generation went, and only as much as was needed.
    assert_eq!(
        held(&loaded),
        [
            row(2, 20, "/a.mp4", MP4, b"aaaa"),
            row(3, 30, "/a.mp4", MP4, b"aaaa"),
            row(3, 31, "/new.mp4", MP4, b"yyyy"),
        ]
    );
    assert_eq!(loaded.bytes, 12);
    // `find` is unchanged: the coordinate first, then the newest generation.
    assert_eq!(
        found(&loaded, 20, "/a.mp4", 4),
        Some((2, 20, b"aaaa".to_vec()))
    );
    assert_eq!(
        found(&loaded, 10, "/a.mp4", 4),
        Some((3, 30, b"aaaa".to_vec()))
    );

    // Several older frames go for one larger frame: generation 2 before
    // generation 3, and within a generation the lowest offset first.
    let mut loaded = Loaded::with_budget(12);
    loaded.insert(prior(3, 5, "/c.mp4", b"333"));
    loaded.insert(prior(2, 9, "/b.mp4", b"222"));
    loaded.insert(prior(2, 7, "/a.mp4", b"222"));
    loaded.insert(prior(3, 6, "/d.mp4", b"333"));
    assert_eq!(loaded.bytes, 12);
    loaded.insert(prior(4, 1, "/e.mp4", b"4444444"));
    assert_eq!(
        held(&loaded),
        [
            row(4, 1, "/e.mp4", MP4, b"4444444"),
            row(3, 6, "/d.mp4", MP4, b"333"),
        ]
    );
    assert_eq!(loaded.bytes, 10);

    // An equal or higher generation is never given up, and a frame that
    // still does not fit is refused with nothing given up for it.
    let mut loaded = Loaded::with_budget(10);
    loaded.insert(prior(1, 0, "/a.mp4", b"11111"));
    loaded.insert(prior(5, 1, "/b.mp4", b"55555"));
    let before = held(&loaded);
    loaded.insert(prior(5, 2, "/c.mp4", b"555555"));
    loaded.insert(prior(4, 2, "/c.mp4", b"444444"));
    loaded.insert(prior(9, 2, "/c.mp4", b"99999999999"));
    assert_eq!(held(&loaded), before);
    assert_eq!(loaded.bytes, 10);
    // Generation 1 alone is room enough for five bytes of generation 4.
    loaded.insert(prior(4, 2, "/c.mp4", b"44444"));
    assert_eq!(
        held(&loaded),
        [
            row(5, 1, "/b.mp4", MP4, b"55555"),
            row(4, 2, "/c.mp4", MP4, b"44444"),
        ]
    );

    // A replacement counts its own old bytes as free before it asks the
    // older generations for more.
    let mut loaded = Loaded::with_budget(10);
    loaded.insert(prior(1, 0, "/a.mp4", b"1111"));
    loaded.insert(prior(2, 1, "/b.mp4", b"222222"));
    loaded.insert(prior(3, 1, "/b.mp4", b"333333"));
    assert_eq!(loaded.entries.len(), 2, "same size: nothing else moves");
    loaded.insert(prior(4, 1, "/b.mp4", b"4444444444"));
    assert_eq!(held(&loaded), [row(4, 1, "/b.mp4", MP4, b"4444444444")]);
    assert_eq!(loaded.bytes, 10);
}

#[test]
fn an_empty_ledger_directory_holds_no_earlier_capture() {
    let dir = tempfile::tempdir().unwrap();
    let loaded = PriorCaptures::new(dir.path()).load("session", "a.jsonl", u64::MAX);
    assert!(loaded.entries.is_empty());
    assert_eq!(loaded.bytes, 0);
}

// ---- `PriorCaptures::load` against real ledgers -------------------------

const SESSION: &str = "session-a";
const TRANSCRIPT: &str = "projects/a.jsonl";
/// Above the segment cap below, so a flush after one such frame rotates.
const LEN: usize = 600;

fn source(session: &str, relative_path: &str) -> SourceDef {
    SourceDef {
        ledger_only: false,
        relative_path: relative_path.to_string(),
        stream_id: "transcript".to_string(),
        session_id: session.to_string(),
        native_session_id: format!("native:{session}"),
        agent_version: Some("1.0".to_string()),
    }
}

fn transcript() -> SourceDef {
    source(SESSION, TRANSCRIPT)
}

/// Deterministic bytes no codec shrinks, distinct per seed.
fn noise(seed: u64, len: usize) -> Vec<u8> {
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 24) as u8
        })
        .collect()
}

/// Frames spread over many segments: the cap is below one media frame.
fn open_ledger(dir: &Path) -> Ledger {
    Ledger::open(dir).unwrap().with_max_segment_bytes(512)
}

/// Append one frame and cross a durable checkpoint, as a live pass does.
fn append(
    ledger: &mut Ledger,
    source: &SourceDef,
    generation: u64,
    offset: u64,
    document: CapturedDocument,
) {
    ledger
        .append_document(source, generation, offset, document)
        .unwrap();
    ledger.flush().unwrap();
}

/// One captured frame of the transcript under test, then a checkpoint.
fn put(ledger: &mut Ledger, generation: u64, offset: u64, path: &str, bytes: &[u8]) {
    append(
        ledger,
        &transcript(),
        generation,
        offset,
        captured(path, bytes),
    );
}

fn segments(dir: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "rledger"))
        .collect();
    paths.sort();
    paths
}

fn indexed_segments(dir: &Path) -> usize {
    fs::read(dir.join(INDEX_FILE))
        .map(|index| {
            index
                .split(|byte| *byte == b'\n')
                .filter(|line| !line.is_empty())
                .count()
        })
        .unwrap_or(0)
}

/// Every regular file of the directory with its bytes.
fn files(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut files: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.is_file())
        .map(|path| {
            let bytes = fs::read(&path).unwrap();
            (path, bytes)
        })
        .collect();
    files.sort();
    files
}

const MP4: &str = "video/mp4";
type Row = (u64, u64, String, String, Vec<u8>);

fn row(generation: u64, offset: u64, path: &str, media_type: &str, bytes: &[u8]) -> Row {
    (
        generation,
        offset,
        path.to_string(),
        media_type.to_string(),
        bytes.to_vec(),
    )
}

/// What a `Loaded` holds, in key order.
fn held(loaded: &Loaded) -> Vec<Row> {
    loaded
        .entries
        .values()
        .map(|prior| {
            (
                prior.generation,
                prior.offset,
                prior.document.path.clone(),
                prior.document.media.as_ref().unwrap().media_type.clone(),
                prior.document.bytes.clone(),
            )
        })
        .collect()
}

fn load(dir: &Path, through: u64) -> Loaded {
    PriorCaptures::new(dir).load(SESSION, TRANSCRIPT, through)
}

#[test]
fn load_keeps_the_newest_generation_and_the_first_frame_within_one() {
    let dir = tempfile::tempdir().unwrap();
    let (one, two, late, again) = (noise(1, LEN), noise(2, LEN), noise(3, LEN), noise(4, LEN));
    let mut ledger = open_ledger(dir.path());
    put(&mut ledger, 1, 10, "/out/a.mp4", &one);
    put(&mut ledger, 2, 10, "/out/a.mp4", &two);
    // The same coordinate again in generation 2: the first frame stays.
    put(&mut ledger, 2, 10, "/out/a.mp4", &late);
    drop(ledger);
    assert!(
        segments(dir.path()).len() >= 3,
        "frames spread over segments"
    );

    let loaded = load(dir.path(), u64::MAX);
    assert_eq!(
        found(&loaded, 10, "/out/a.mp4", LEN as u64),
        Some((2, 10, two.clone()))
    );
    assert_eq!(loaded.entries.len(), 1);
    assert_eq!(loaded.bytes, LEN as u64);

    // A lower generation appended later in the ledger never replaces a
    // higher one, and within generation 1 the first frame still wins.
    let mut ledger = open_ledger(dir.path());
    put(&mut ledger, 1, 10, "/out/a.mp4", &again);
    drop(ledger);
    let loaded = load(dir.path(), u64::MAX);
    assert_eq!(
        found(&loaded, 10, "/out/a.mp4", LEN as u64),
        Some((2, 10, two))
    );
    let loaded = load(dir.path(), 1);
    assert_eq!(
        found(&loaded, 10, "/out/a.mp4", LEN as u64),
        Some((1, 10, one))
    );
    assert_eq!(loaded.entries.len(), 1);
}

#[test]
fn load_excludes_every_frame_that_is_not_this_sources_captured_media() {
    let dir = tempfile::tempdir().unwrap();
    let wanted = noise(10, LEN);
    let other_session = noise(11, LEN);
    let other_transcript = noise(12, LEN);
    let other_file = noise(13, LEN);
    let other_size = noise(14, LEN + 100);
    let other_type = noise(15, LEN);
    let newer = noise(16, LEN);
    let quicktime = MediaClaim {
        media_type: "video/quicktime".into(),
        expected_len: LEN as u64,
    };

    let mut ledger = open_ledger(dir.path());
    // Another source that names the same file, before and after.
    let elsewhere = source("session-b", TRANSCRIPT);
    append(
        &mut ledger,
        &elsewhere,
        1,
        0,
        captured("/out/a.mp4", &other_session),
    );
    // An HTML document at the coordinate, with and without provenance.
    for recovered_from in [None, Some("/archive/a.mp4".to_string())] {
        let html = CapturedDocument {
            media: None,
            recovered_from,
            ..captured("/out/a.mp4", &noise(17, LEN))
        };
        append(&mut ledger, &transcript(), 1, 0, html);
    }
    // Observations that captured nothing.
    for status in [
        DocumentCaptureStatus::Missing,
        DocumentCaptureStatus::TooLarge,
        DocumentCaptureStatus::Unsupported,
        DocumentCaptureStatus::ChangedDuringRead,
    ] {
        let empty = CapturedDocument {
            status,
            bytes: Vec::new(),
            ..captured("/out/a.mp4", &wanted)
        };
        append(&mut ledger, &transcript(), 1, 0, empty.clone());
        append(&mut ledger, &transcript(), 2, 5, empty);
    }
    put(&mut ledger, 1, 0, "/out/a.mp4", &wanted);
    // The same session's other transcript.
    let sibling = source(SESSION, "projects/b.jsonl");
    append(
        &mut ledger,
        &sibling,
        1,
        0,
        captured("/out/a.mp4", &other_transcript),
    );
    // Another file, another size and another type are other claims.
    put(&mut ledger, 1, 0, "/out/b.mp4", &other_file);
    put(&mut ledger, 1, 20, "/out/a.mp4", &other_size);
    let retyped = CapturedDocument {
        media: Some(quicktime.clone()),
        ..captured("/out/a.mp4", &other_type)
    };
    append(&mut ledger, &transcript(), 1, 20, retyped);
    // A generation above `through`.
    put(&mut ledger, 3, 0, "/out/a.mp4", &newer);
    append(
        &mut ledger,
        &elsewhere,
        2,
        0,
        captured("/out/a.mp4", &other_session),
    );
    drop(ledger);
    assert!(segments(dir.path()).len() > 8);

    let loaded = load(dir.path(), 2);
    assert_eq!(
        held(&loaded),
        [
            row(1, 0, "/out/a.mp4", MP4, &wanted),
            row(1, 0, "/out/b.mp4", MP4, &other_file),
            row(1, 20, "/out/a.mp4", MP4, &other_size),
            row(1, 20, "/out/a.mp4", "video/quicktime", &other_type),
        ]
    );
    assert_eq!(loaded.bytes, (4 * LEN + 100) as u64);
    // The claim is the path, the type and the size: the frames of another
    // file, size or type never answer for this one, at any offset.
    for offset in [0, 5, 20, 99] {
        assert_eq!(
            found(&loaded, offset, "/out/a.mp4", LEN as u64),
            Some((1, 0, wanted.clone())),
            "offset {offset}"
        );
    }
    assert_eq!(
        loaded
            .find(0, "/out/a.mp4", &quicktime)
            .map(|prior| (prior.offset, prior.document.bytes.clone())),
        Some((20, other_type))
    );
    assert_eq!(found(&loaded, 0, "/out/c.mp4", LEN as u64), None);

    // `through` is inclusive, and nothing below it is dropped.
    let loaded = load(dir.path(), 3);
    assert_eq!(
        found(&loaded, 0, "/out/a.mp4", LEN as u64),
        Some((3, 0, newer))
    );
    assert_eq!(loaded.entries.len(), 4);
    assert!(load(dir.path(), 0).entries.is_empty());

    // Each source sees only its own frames.
    let captures = PriorCaptures::new(dir.path());
    let theirs = captures.load("session-b", TRANSCRIPT, 1);
    assert_eq!(
        held(&theirs),
        [row(1, 0, "/out/a.mp4", MP4, &other_session)]
    );
    let siblings = captures.load(SESSION, "projects/b.jsonl", u64::MAX);
    assert_eq!(
        held(&siblings),
        [row(1, 0, "/out/a.mp4", MP4, &other_transcript)]
    );
    assert!(
        captures
            .load("session-c", TRANSCRIPT, u64::MAX)
            .entries
            .is_empty()
    );
    assert!(
        captures
            .load(SESSION, "projects/c.jsonl", u64::MAX)
            .entries
            .is_empty()
    );
}

#[test]
fn the_open_segment_is_read_and_the_index_only_narrows() {
    let dir = tempfile::tempdir().unwrap();
    let index = dir.path().join(INDEX_FILE);
    let (first, second, open, buffered) =
        (noise(20, LEN), noise(21, LEN), noise(22, 16), noise(23, 16));
    let mut ledger = open_ledger(dir.path());
    put(&mut ledger, 1, 0, "/out/a.mp4", &first);
    put(&mut ledger, 1, 100, "/out/b.mp4", &second);
    // A small frame: its segment stays under the cap, so it stays open.
    put(&mut ledger, 2, 200, "/out/c.mp4", &open);
    assert_eq!(segments(dir.path()).len(), 3);
    assert_eq!(indexed_segments(dir.path()), 2, "the last segment is open");

    let before = files(dir.path());
    let loaded = load(dir.path(), u64::MAX);
    let expected = [
        row(1, 0, "/out/a.mp4", MP4, &first),
        row(1, 100, "/out/b.mp4", MP4, &second),
        row(2, 200, "/out/c.mp4", MP4, &open),
    ];
    assert_eq!(held(&loaded), expected);
    assert_eq!(files(dir.path()), before, "the lookup writes nothing");

    // The writer holds a frame it has not flushed: the reader stops at
    // the open segment's tail and keeps everything before it.
    ledger
        .append_document(&transcript(), 2, 300, captured("/out/d.mp4", &buffered))
        .unwrap();
    let loaded = load(dir.path(), u64::MAX);
    assert_eq!(held(&loaded)[..3], expected);
    ledger.flush().unwrap();
    let loaded = load(dir.path(), u64::MAX);
    assert_eq!(
        found(&loaded, 300, "/out/d.mp4", 16),
        Some((2, 300, buffered))
    );
    let complete = held(&loaded);
    assert_eq!(complete.len(), 4);

    // No index at all: every segment is walked, and none is written.
    fs::remove_file(&index).unwrap();
    assert_eq!(held(&load(dir.path(), u64::MAX)), complete);
    assert!(!index.exists());

    // An index that knows only what closed since: the same answer.
    drop(ledger);
    assert!(indexed_segments(dir.path()) < segments(dir.path()).len());
    assert_eq!(held(&load(dir.path(), u64::MAX)), complete);
}

#[test]
fn a_truncated_tail_ends_only_its_own_segment() {
    for codec in [SegmentCodec::default(), SegmentCodec::Plain] {
        let dir = tempfile::tempdir().unwrap();
        let (a, b, c, d) = (
            noise(30, LEN),
            noise(31, LEN),
            noise(32, LEN),
            noise(33, LEN),
        );
        let mut ledger = open_ledger(dir.path()).with_codec(codec);
        put(&mut ledger, 1, 0, "/out/a.mp4", &a);
        // Two frames in one segment: no checkpoint between them.
        ledger
            .append_document(&transcript(), 1, 100, captured("/out/b.mp4", &b))
            .unwrap();
        put(&mut ledger, 1, 200, "/out/c.mp4", &c);
        put(&mut ledger, 1, 300, "/out/d.mp4", &d);
        drop(ledger);
        let paths = segments(dir.path());
        assert_eq!(paths.len(), 3, "{codec:?}");
        assert_eq!(load(dir.path(), u64::MAX).entries.len(), 4, "{codec:?}");

        let torn = &paths[1];
        let whole = fs::metadata(torn).unwrap().len();
        // The tail cut inside the last frame, then the header cut, then
        // nothing left: the neighbours' frames are found every time.
        for (keep, label) in [(whole - 40, "tail"), (2, "header"), (0, "empty")] {
            fs::OpenOptions::new()
                .write(true)
                .open(torn)
                .unwrap()
                .set_len(keep)
                .unwrap();
            let loaded = load(dir.path(), u64::MAX);
            assert_eq!(
                found(&loaded, 0, "/out/a.mp4", LEN as u64),
                Some((1, 0, a.clone())),
                "{codec:?} {label}"
            );
            assert_eq!(
                found(&loaded, 300, "/out/d.mp4", LEN as u64),
                Some((1, 300, d.clone())),
                "{codec:?} {label}"
            );
            assert_eq!(
                found(&loaded, 200, "/out/c.mp4", LEN as u64),
                None,
                "{codec:?} {label}"
            );
            // A plain segment keeps every whole frame before the cut.
            if codec == SegmentCodec::Plain {
                assert_eq!(
                    found(&loaded, 100, "/out/b.mp4", LEN as u64),
                    (label == "tail").then(|| (1, 100, b.clone())),
                    "{label}"
                );
            }
        }
    }
}

#[test]
fn a_missing_ledger_directory_holds_nothing_and_is_not_created() {
    let dir = tempfile::tempdir().unwrap();
    let absent = dir.path().join("absent").join("ledger");
    let loaded = PriorCaptures::new(&absent).load(SESSION, TRANSCRIPT, u64::MAX);
    assert!(loaded.entries.is_empty());
    assert_eq!(loaded.bytes, 0);
    assert_eq!(loaded.budget, PRIOR_MAX_BYTES);
    assert!(!dir.path().join("absent").exists());

    // A path that is not a directory cannot be listed: still no panic.
    let file = dir.path().join("not-a-directory");
    fs::write(&file, b"x").unwrap();
    let loaded = PriorCaptures::new(&file).load(SESSION, TRANSCRIPT, u64::MAX);
    assert!(loaded.entries.is_empty());
}

#[test]
fn an_unreadable_segment_gives_an_empty_answer_not_a_partial_one() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = (noise(40, LEN), noise(41, LEN));
    let elsewhere = source("session-b", "projects/b.jsonl");
    let mut ledger = open_ledger(dir.path());
    put(&mut ledger, 1, 0, "/out/a.mp4", &a);
    append(
        &mut ledger,
        &elsewhere,
        1,
        0,
        captured("/out/x.mp4", &noise(42, LEN)),
    );
    put(&mut ledger, 2, 0, "/out/a.mp4", &b);
    drop(ledger);
    let paths = segments(dir.path());
    assert_eq!(paths.len(), 3);
    assert_eq!(indexed_segments(dir.path()), 3);

    // The other source's segment can be opened but not read.
    fs::remove_file(&paths[1]).unwrap();
    fs::create_dir(&paths[1]).unwrap();

    // The index says that segment holds nothing of this source, so the
    // scoped walk never opens it.
    let loaded = load(dir.path(), u64::MAX);
    assert_eq!(found(&loaded, 0, "/out/a.mp4", LEN as u64), Some((2, 0, b)));
    // The source it does hold reads it, fails, and gets nothing.
    let theirs = PriorCaptures::new(dir.path()).load("session-b", "projects/b.jsonl", u64::MAX);
    assert!(theirs.entries.is_empty());

    // With no index the walk reaches it after segment 0 gave a frame.
    // Returning that frame alone would carry generation 1 over the
    // generation 2 behind the failure: the answer is empty instead.
    fs::remove_file(dir.path().join(INDEX_FILE)).unwrap();
    let loaded = load(dir.path(), u64::MAX);
    assert!(loaded.entries.is_empty());
    assert_eq!(loaded.bytes, 0);

    // An index that cannot be read is the same: nothing, no panic.
    fs::remove_dir(&paths[1]).unwrap();
    assert_eq!(load(dir.path(), u64::MAX).entries.len(), 1);
    fs::create_dir(dir.path().join(INDEX_FILE)).unwrap();
    assert!(load(dir.path(), u64::MAX).entries.is_empty());
}

#[test]
fn load_retains_no_frame_past_the_byte_budget() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b, c, d, newer) = (
        noise(50, LEN),
        noise(51, LEN),
        noise(52, LEN),
        noise(53, LEN),
        noise(54, LEN),
    );
    let mut ledger = open_ledger(dir.path());
    put(&mut ledger, 1, 0, "/out/a.mp4", &a);
    put(&mut ledger, 1, 100, "/out/b.mp4", &b);
    put(&mut ledger, 1, 200, "/out/c.mp4", &c);
    put(&mut ledger, 1, 300, "/out/d.mp4", &d);
    put(&mut ledger, 2, 0, "/out/a.mp4", &newer);
    drop(ledger);
    let captures = PriorCaptures::new(dir.path());
    let within = |budget| captures.load_within(SESSION, TRANSCRIPT, u64::MAX, budget);
    let len = LEN as u64;

    // Room for two frames: the first two in ledger order are held, the
    // later claims are not, and the held claim's newer generation still
    // replaces it because it gives the old bytes back first.
    for budget in [2 * len, 3 * len - 1] {
        let loaded = within(budget);
        assert_eq!(loaded.budget, budget);
        assert_eq!(loaded.bytes, 2 * len);
        assert_eq!(loaded.entries.len(), 2);
        assert_eq!(
            found(&loaded, 0, "/out/a.mp4", len),
            Some((2, 0, newer.clone()))
        );
        assert_eq!(
            found(&loaded, 100, "/out/b.mp4", len),
            Some((1, 100, b.clone()))
        );
        assert_eq!(found(&loaded, 200, "/out/c.mp4", len), None);
        assert_eq!(found(&loaded, 300, "/out/d.mp4", len), None);
    }

    // Too small for any frame, and exactly enough for all of them.
    for budget in [0, len - 1] {
        let loaded = within(budget);
        assert!(loaded.entries.is_empty());
        assert_eq!(loaded.bytes, 0);
    }
    let loaded = within(4 * len);
    assert_eq!(loaded.entries.len(), 4);
    assert_eq!(loaded.bytes, 4 * len);

    // `load` itself runs under the pass's whole budget.
    let loaded = captures.load(SESSION, TRANSCRIPT, u64::MAX);
    assert_eq!(loaded.budget, PRIOR_MAX_BYTES);
    assert_eq!(loaded.entries.len(), 4);
    assert_eq!(found(&loaded, 300, "/out/d.mp4", len), Some((1, 300, d)));
}

#[test]
fn find_after_load_matches_the_coordinate_first_then_the_claim_anywhere() {
    let dir = tempfile::tempdir().unwrap();
    let len = LEN as u64;
    let frames = [
        (1_u64, 10_u64, noise(60, LEN)),
        (3, 40, noise(61, LEN)),
        (3, 20, noise(62, LEN)),
        (2, 30, noise(63, LEN)),
    ];
    let mut ledger = open_ledger(dir.path());
    for (generation, offset, bytes) in &frames {
        put(&mut ledger, *generation, *offset, "/out/a.mp4", bytes);
    }
    // Another file sent by the record at offset 10.
    let beside = noise(64, LEN);
    put(&mut ledger, 1, 10, "/out/b.mp4", &beside);
    drop(ledger);
    assert!(segments(dir.path()).len() >= 5);

    let loaded = load(dir.path(), u64::MAX);
    assert_eq!(loaded.entries.len(), 5);
    // The exact coordinate wins, even over a newer generation elsewhere.
    for (generation, offset, bytes) in &frames {
        assert_eq!(
            found(&loaded, *offset, "/out/a.mp4", len),
            Some((*generation, *offset, bytes.clone()))
        );
    }
    // Elsewhere: the newest generation, then the lowest offset.
    for offset in [0, 11, 99] {
        assert_eq!(
            found(&loaded, offset, "/out/a.mp4", len),
            Some((3, 20, frames[2].2.clone())),
            "offset {offset}"
        );
    }
    assert_eq!(
        found(&loaded, 10, "/out/b.mp4", len),
        Some((1, 10, beside.clone()))
    );
    assert_eq!(found(&loaded, 40, "/out/b.mp4", len), Some((1, 10, beside)));
    assert_eq!(found(&loaded, 10, "/out/a.mp4", len + 1), None);
    assert_eq!(found(&loaded, 10, "/out/c.mp4", len), None);

    // A pass that may only see generations up to 2 carries from those.
    let loaded = load(dir.path(), 2);
    assert_eq!(
        found(&loaded, 40, "/out/a.mp4", len),
        Some((2, 30, frames[3].2.clone()))
    );
    assert_eq!(
        found(&loaded, 10, "/out/a.mp4", len),
        Some((1, 10, frames[0].2.clone()))
    );
}
