//! Zstd-frames segments against plain ones: the same decoded bytes, entries
//! and segment index whether the ledger or a capture worker compressed a
//! batch; a prefix under any truncation of the last frames; torn under
//! corruption; left alone by recompression unless forced; indexed like any
//! other segment.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tempfile::TempDir;

use super::*;
use crate::ledger::{Ledger, LedgerEntry, PrebuiltFrames, SourceDef, frame_prefix, scan};
use crate::recompress::{RecompressOutcome, recompress_segment};
use crate::segment_index::{INDEX_FILE, rebuild_segment_index};

const LEVEL: i32 = DEFAULT_ZSTD_LEVEL;
const FRAMES: SegmentCodec = SegmentCodec::ZstdFrames { level: LEVEL };
const SOURCES: u32 = 6;

fn source(index: u32) -> SourceDef {
    SourceDef {
        ledger_only: false,
        relative_path: format!("s{index}.jsonl"),
        stream_id: format!("stream:{index}"),
        session_id: format!("session:{index}"),
        native_session_id: format!("native:{index}"),
        agent_version: Some("1.0".to_string()),
    }
}

/// Words from a small vocabulary, so records compress but not to nothing.
fn record(source: u32, index: u64) -> Vec<u8> {
    const WORDS: [&str; 12] = [
        "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf", "hotel", "india",
        "juliet", "kilo", "lima",
    ];
    let mut state = (u64::from(source) << 32 | index).wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let text: Vec<&str> = (0..24)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            WORDS[(state % WORDS.len() as u64) as usize]
        })
        .collect();
    format!(
        "{{\"type\":\"assistant\",\"source\":{source},\"index\":{index},\"text\":\"{}\"}}\n",
        text.join(" ")
    )
    .into_bytes()
}

/// How capture batches reach the ledger.
#[derive(Debug, Clone, Copy)]
enum Feed {
    /// The serial driver: `append_raw_owned`, then `complete_generation`.
    Raw,
    /// Prebuilt frames, compressed by their builder (a capture worker) or
    /// left for the ledger to compress.
    Prebuilt { compressed: bool },
}

/// One scripted history: metadata, keyed sources with big batches, one-batch
/// sources, a source of batches too small to compress, an unkeyed source
/// written record by record, seals, checkpoints and rotation.
fn write_history(dir: &Path, codec: SegmentCodec, feed: Feed, asynchronous: bool) {
    let mut ledger = Ledger::open(dir)
        .unwrap()
        .with_codec(codec)
        .with_max_segment_bytes(48 * 1024)
        .defer_sync_until_flush();
    if asynchronous {
        ledger = ledger.into_async_bounded(128 * 1024);
    }
    ledger.append_meta(7, "{\"ops\":[\"first\"]}").unwrap();
    ledger.checkpoint().unwrap();
    ledger
        .set_source_keys(Arc::new(
            (0..SOURCES).map(|index| (source(index), index)).collect(),
        ))
        .unwrap();
    let mut worker = FrameCompressor::new(LEVEL).unwrap();
    for index in 0..SOURCES {
        let batches = if index == 2 { 1 } else { 3 };
        // Source 4's batches stay under the compression threshold.
        let per_batch = if index == 4 { 3 } else { 60 };
        for batch in 0..batches {
            let records: Vec<(u64, Vec<u8>)> = (0..per_batch)
                .map(|at| {
                    let at = batch * per_batch + at;
                    (at * 1_000, record(index, at))
                })
                .collect();
            let seals = batch + 1 == batches;
            match feed {
                Feed::Raw => {
                    ledger.append_raw_owned(&source(index), 1, records).unwrap();
                    if seals {
                        ledger
                            .complete_generation(&source(index), 1, 9, true)
                            .unwrap();
                    }
                }
                Feed::Prebuilt { compressed } => {
                    let mut frames = PrebuiltFrames::new(index, 1);
                    frames.push_records(records).unwrap();
                    if seals {
                        frames.push_seal(9, true);
                    }
                    if compressed && codec.batch_level().is_some() {
                        frames.compress(&mut worker).unwrap();
                    }
                    ledger.append_prebuilt(&source(index), frames).unwrap();
                }
            }
            ledger.checkpoint().unwrap();
        }
    }
    // An inline source outside the keys, record by record.
    let inline = SourceDef {
        relative_path: "inline.db#one".to_string(),
        ..source(99)
    };
    for at in 0..20_u64 {
        ledger
            .append_raw(&inline, 1, [(at, record(99, at))])
            .unwrap();
    }
    ledger.complete_generation(&inline, 1, 9, true).unwrap();
    ledger.append_meta(7, "{\"ops\":[\"last\"]}").unwrap();
    ledger.checkpoint().unwrap();
    ledger.flush().unwrap();
}

/// Every file of a ledger directory, by name.
type Files = Vec<(String, Vec<u8>)>;

fn files(dir: &Path) -> Files {
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            (name, std::fs::read(&path).unwrap())
        })
        .collect();
    files.sort();
    files
}

fn segments(dir: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "rledger"))
        .collect();
    paths.sort();
    paths
}

/// Container-decoded bytes, magic excluded.
fn decoded(path: &Path) -> Vec<u8> {
    let mut reader = SegmentReader::open(path).unwrap().unwrap();
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes).unwrap();
    assert!(!reader.corrupt());
    bytes
}

/// Offsets where each zstd frame of a zstd-frames segment starts, and its end.
fn frame_starts(bytes: &[u8]) -> Vec<usize> {
    let mut starts = Vec::new();
    let mut at = MAGIC_LEN;
    while at < bytes.len() {
        starts.push(at);
        at += zstd::zstd_safe::find_frame_compressed_size(&bytes[at..]).unwrap();
    }
    starts.push(bytes.len());
    starts
}

#[test]
fn zstd_frames_decode_to_the_plain_bytes_whoever_compresses() {
    let plain = TempDir::new().unwrap();
    write_history(plain.path(), SegmentCodec::Plain, Feed::Raw, false);
    let plain_files = files(plain.path());
    let plain_entries = scan(plain.path()).unwrap().entries;
    let plain_segments = segments(plain.path());
    assert!(plain_segments.len() > 2, "the history rotates");

    // Where staged frames leave depends on the ledger's checkpoint policy (a
    // private asynchronous ledger coalesces checkpoints between seals), so
    // bytes compare within one mode; every mode decodes to the plain bytes.
    let mut reference: [Option<Files>; 2] = [None, None];
    for (feed, asynchronous) in [
        (Feed::Raw, false),
        (Feed::Prebuilt { compressed: false }, false),
        (Feed::Prebuilt { compressed: true }, false),
        (Feed::Raw, true),
        (Feed::Prebuilt { compressed: false }, true),
        (Feed::Prebuilt { compressed: true }, true),
    ] {
        let label = format!("{feed:?} async {asynchronous}");
        let dir = TempDir::new().unwrap();
        write_history(dir.path(), FRAMES, feed, asynchronous);
        let written = files(dir.path());
        match &reference[usize::from(asynchronous)] {
            None => reference[usize::from(asynchronous)] = Some(written.clone()),
            Some(reference) => assert!(
                &written == reference,
                "{label}: zstd-frames bytes depend on who compressed"
            ),
        }
        let frames_segments = segments(dir.path());
        assert_eq!(frames_segments.len(), plain_segments.len(), "{label}");
        let mut frames_seen = 0;
        for (frames, plain) in frames_segments.iter().zip(&plain_segments) {
            let bytes = std::fs::read(frames).unwrap();
            assert_eq!(&bytes[..MAGIC_LEN], MAGIC_ZSTD, "{label}");
            assert_eq!(
                decoded(frames),
                std::fs::read(plain).unwrap()[MAGIC_LEN..],
                "{label}: {} decodes to the plain segment",
                frames.display()
            );
            frames_seen += frame_starts(&bytes).len() - 1;
        }
        assert!(
            frames_seen > 2 * frames_segments.len(),
            "{label}: several frames per segment ({frames_seen})"
        );
        let index = |files: &[(String, Vec<u8>)]| {
            files
                .iter()
                .find(|(name, _)| name == INDEX_FILE)
                .map(|(_, bytes)| bytes.clone())
        };
        assert_eq!(index(&written), index(&plain_files), "{label}: index");
        assert_eq!(scan(dir.path()).unwrap().entries, plain_entries, "{label}");
        let on_disk: usize = written.iter().map(|(_, bytes)| bytes.len()).sum();
        let plain_bytes: usize = plain_files.iter().map(|(_, bytes)| bytes.len()).sum();
        assert!(
            on_disk * 2 < plain_bytes,
            "{label}: {on_disk} of {plain_bytes}"
        );
    }
}

/// One single-segment ledger of three sources, its bytes and entries.
fn one_segment(codec: SegmentCodec) -> (TempDir, PathBuf, Vec<u8>, Vec<LedgerEntry>) {
    let dir = TempDir::new().unwrap();
    let mut ledger = Ledger::open(dir.path()).unwrap().with_codec(codec);
    let keys: HashMap<SourceDef, u32> = (0..3).map(|index| (source(index), index)).collect();
    ledger.set_source_keys(Arc::new(keys)).unwrap();
    let mut worker = FrameCompressor::new(LEVEL).unwrap();
    for index in 0..3_u32 {
        let mut frames = PrebuiltFrames::new(index, 1);
        frames
            .push_records((0..40).map(|at| (at, record(index, at))).collect())
            .unwrap();
        frames.push_seal(9, true);
        if codec.batch_level().is_some() {
            frames.compress(&mut worker).unwrap();
        }
        ledger.append_prebuilt(&source(index), frames).unwrap();
        ledger.checkpoint().unwrap();
    }
    drop(ledger);
    let path = dir.path().join("00000000.rledger");
    let bytes = std::fs::read(&path).unwrap();
    let entries = scan(dir.path()).unwrap().entries;
    (dir, path, bytes, entries)
}

fn scan_prefix(dir: &Path, all: &[LedgerEntry], label: &str) -> (usize, bool) {
    let scanned = scan(dir).unwrap();
    assert!(
        scanned.entries.len() <= all.len() && scanned.entries == all[..scanned.entries.len()],
        "{label}: the visible entries are not a prefix"
    );
    (scanned.entries.len(), scanned.torn_segments == 1)
}

#[test]
fn truncating_the_last_frames_anywhere_leaves_a_prefix() {
    let (dir, path, bytes, all) = one_segment(FRAMES);
    let starts = frame_starts(&bytes);
    // Three sources, each a define frame, a batch frame and a staged seal
    // frame: the last three frames are the last source's.
    assert!(starts.len() > 6, "{starts:?}");
    let from = starts[starts.len() - 4];
    let mut previous = 0;
    for cut in from..=bytes.len() {
        std::fs::write(&path, &bytes[..cut]).unwrap();
        let (visible, _) = scan_prefix(dir.path(), &all, &format!("cut at {cut}"));
        assert!(visible >= previous, "cut at {cut}: entries shrank");
        previous = visible;
        if starts.contains(&cut) {
            // A cut between frames loses nothing that was complete.
            assert_eq!(
                visible,
                decoded_entries_through(&bytes[..cut]),
                "cut at {cut}"
            );
        }
    }
    assert_eq!(previous, all.len());
}

/// Entries in a complete prefix of a zstd-frames segment.
fn decoded_entries_through(bytes: &[u8]) -> usize {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("00000000.rledger"), bytes).unwrap();
    let scanned = scan(dir.path()).unwrap();
    assert_eq!(scanned.torn_segments, 0);
    scanned.entries.len()
}

/// What flipping each byte from `from` on does to a one-segment ledger:
/// (torn, harmless, reads as a truncated container). Every flip leaves a
/// prefix of the entries; one that neither tears the segment nor leaves
/// every entry must have made zstd itself report a frame running past the
/// end of the file — exactly what a crash leaves, whose contract is a prefix.
fn flip_census(
    dir: &Path,
    path: &Path,
    bytes: &[u8],
    all: &[LedgerEntry],
    from: usize,
) -> (usize, usize, Vec<usize>) {
    let (mut torn, mut harmless, mut truncated) = (0, 0, Vec::new());
    for at in from..bytes.len() {
        let mut flipped = bytes.to_vec();
        flipped[at] ^= 0x5A;
        std::fs::write(path, &flipped).unwrap();
        let (visible, is_torn) = scan_prefix(dir, all, &format!("flip at {at}"));
        if is_torn {
            torn += 1;
        } else if visible == all.len() {
            harmless += 1;
        } else {
            let decoded = zstd::stream::decode_all(&flipped[MAGIC_LEN..]);
            assert!(
                matches!(&decoded, Err(error) if error.kind() == ErrorKind::UnexpectedEof),
                "flip at {at}: {visible} of {} entries, not torn: {decoded:?}",
                all.len()
            );
            truncated.push(at);
        }
    }
    (torn, harmless, truncated)
}

#[test]
fn a_flipped_byte_in_the_last_frames_is_torn_or_reads_as_truncated() {
    let (dir, path, bytes, all) = one_segment(FRAMES);
    let starts = frame_starts(&bytes);
    let from = starts[starts.len() - 4];
    let (torn, harmless, truncated) = flip_census(dir.path(), &path, &bytes, &all, from);
    eprintln!(
        "zstd frames: {} flipped bytes, {torn} torn, {harmless} harmless, truncated at {truncated:?}",
        bytes.len() - from
    );
    assert!(truncated.len() * 100 <= bytes.len() - from, "{truncated:?}");

    // The stream container reads the same way: a flip can make its last
    // block run past the end of the file too.
    let (dir, path, bytes, all) = one_segment(SegmentCodec::default());
    let from = bytes.len() - (bytes.len() - MAGIC_LEN) / 3;
    let (torn, harmless, truncated) = flip_census(dir.path(), &path, &bytes, &all, from);
    eprintln!(
        "zstd stream: {} flipped bytes, {torn} torn, {harmless} harmless, truncated at {truncated:?}",
        bytes.len() - from
    );
}

#[test]
fn recompression_leaves_zstd_frames_alone_unless_forced() {
    let dir = TempDir::new().unwrap();
    write_history(
        dir.path(),
        FRAMES,
        Feed::Prebuilt { compressed: true },
        true,
    );
    let before = scan(dir.path()).unwrap().entries;
    for path in segments(dir.path()) {
        let bytes = std::fs::read(&path).unwrap();
        let prefix = |path: &Path| {
            let mut reader = SegmentReader::open(path).unwrap().unwrap();
            let prefix = frame_prefix(&mut reader).unwrap();
            assert!(prefix.clean);
            (prefix.bytes, prefix.digest)
        };
        let expected = prefix(&path);
        assert!(matches!(
            recompress_segment(&path, SegmentCodec::default(), false).unwrap(),
            RecompressOutcome::Unchanged { .. }
        ));
        assert!(
            std::fs::read(&path).unwrap() == bytes,
            "unchanged means untouched"
        );
        assert!(matches!(
            recompress_segment(&path, SegmentCodec::default(), true).unwrap(),
            RecompressOutcome::Rewritten { .. }
        ));
        let rewritten = std::fs::read(&path).unwrap();
        assert_eq!(&rewritten[..MAGIC_LEN], MAGIC_ZSTD);
        assert_eq!(
            frame_starts(&rewritten).len(),
            2,
            "one stream after --force"
        );
        assert_eq!(prefix(&path), expected, "byte-transparent");
    }
    assert_eq!(scan(dir.path()).unwrap().entries, before);
}

#[test]
fn the_segment_index_rebuilds_from_zstd_frames() {
    let dir = TempDir::new().unwrap();
    write_history(
        dir.path(),
        FRAMES,
        Feed::Prebuilt { compressed: true },
        true,
    );
    let index = dir.path().join(INDEX_FILE);
    let written = std::fs::read(&index).unwrap();
    std::fs::remove_file(&index).unwrap();
    assert_eq!(
        rebuild_segment_index(dir.path()).unwrap(),
        segments(dir.path()).len()
    );
    assert_eq!(std::fs::read(&index).unwrap(), written);
}

#[test]
fn a_kill_after_a_checkpoint_keeps_every_checkpointed_frame() {
    let dir = TempDir::new().unwrap();
    // Durable checkpoints: each one emits the staged frame and flushes.
    let mut ledger = Ledger::open(dir.path()).unwrap().with_codec(FRAMES);
    for at in 0..100_u64 {
        ledger
            .append_raw(&source(0), 1, [(at, record(0, at))])
            .unwrap();
    }
    ledger.checkpoint().unwrap();
    ledger
        .append_raw_owned(
            &source(0),
            1,
            (100..160).map(|at| (at, record(0, at))).collect(),
        )
        .unwrap();
    ledger.checkpoint().unwrap();
    // Staged and buffered, never checkpointed — and the process "dies".
    ledger
        .append_raw_owned(
            &source(0),
            1,
            (160..220).map(|at| (at, record(0, at))).collect(),
        )
        .unwrap();
    for at in 220..230_u64 {
        ledger
            .append_raw(&source(0), 1, [(at, record(0, at))])
            .unwrap();
    }
    std::mem::forget(ledger);

    let scanned = scan(dir.path()).unwrap();
    assert_eq!(scanned.torn_segments, 0);
    let raw: Vec<Vec<u8>> = scanned
        .entries
        .into_iter()
        .filter_map(|entry| match entry {
            LedgerEntry::Raw { payload, .. } => Some(payload),
            _ => None,
        })
        .collect();
    assert_eq!(raw, (0..160).map(|at| record(0, at)).collect::<Vec<_>>());
}

#[test]
fn a_ledger_refuses_compressed_frames_it_does_not_write() {
    let mut worker = FrameCompressor::new(1).unwrap();
    let mut frames = PrebuiltFrames::new(0, 1);
    frames
        .push_records((0..60).map(|at| (at, record(0, at))).collect())
        .unwrap();
    frames.compress(&mut worker).unwrap();
    assert!(frames.is_compressed());
    for codec in [SegmentCodec::Plain, SegmentCodec::default(), FRAMES] {
        let dir = TempDir::new().unwrap();
        let mut ledger = Ledger::open(dir.path()).unwrap().with_codec(codec);
        ledger
            .set_source_keys(Arc::new(HashMap::from([(source(0), 0)])))
            .unwrap();
        let refused = ledger.append_prebuilt(&source(0), frames.clone());
        assert!(
            matches!(refused, Err(crate::CaptureError::InvalidConfig(_))),
            "{codec:?}: {refused:?}"
        );
    }
}
