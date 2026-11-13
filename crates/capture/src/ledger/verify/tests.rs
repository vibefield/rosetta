use std::fs::OpenOptions;
use std::io::Write;

use tempfile::TempDir;

use super::*;
use crate::ledger::read::tests::{
    CLIP_BYTES, HTML_BYTES, RAW_PAYLOAD_BYTES, STATUSES, html, media, write_document_segment,
};
use crate::ledger::{
    CHECKSUM_BYTES, CapturedDocument, DocumentCaptureStatus, Ledger, TAG_DEFINE,
    TAG_GENERATION_COMPLETE, TAG_META_VERSIONED, TAG_RAW, serde_encode,
};
use crate::segment::{MAGIC_PLAIN, SegmentCodec};
use crate::{RecompressOutcome, recompress_segment};

fn source(path: &str) -> SourceDef {
    SourceDef {
        ledger_only: false,
        relative_path: path.to_string(),
        stream_id: "transcript".to_string(),
        session_id: format!("session:{path}"),
        native_session_id: format!("native:{path}"),
        agent_version: Some("2.0".to_string()),
    }
}

fn record(index: u64) -> Vec<u8> {
    format!("{{\"type\":\"assistant\",\"index\":{index},\"text\":\"a synthetic line {index}\"}}\n")
        .into_bytes()
}

const META: &str = "[{\"op\":\"runtime\"}]";

/// The same entry stream every writer below produces: one meta batch, then
/// two sources of twenty records each, each sealed.
fn write_through_ledger(ledger: &mut Ledger, batched: bool) {
    ledger.append_meta(7, META).unwrap();
    ledger.checkpoint().unwrap();
    for path in ["a.jsonl", "b.jsonl"] {
        let source = source(path);
        for chunk in 0..4_u64 {
            let records: Vec<(u64, Vec<u8>)> = (chunk * 5..chunk * 5 + 5)
                .map(|index| (index * 100, record(index)))
                .collect();
            if batched {
                ledger.append_raw_owned(&source, 1, records).unwrap();
            } else {
                for (offset, payload) in records {
                    ledger.append_raw(&source, 1, [(offset, payload)]).unwrap();
                }
            }
            ledger.checkpoint().unwrap();
        }
        ledger.complete_generation(&source, 1, 3, true).unwrap();
        ledger.checkpoint().unwrap();
    }
    ledger.flush().unwrap();
}

fn frame(out: &mut Vec<u8>, tag: u8, body: &[u8]) {
    let mut head = [0_u8; 5];
    head[0] = tag;
    head[1..5].copy_from_slice(&(body.len() as u32).to_be_bytes());
    let mut hasher = blake3::Hasher::new();
    hasher.update(&head);
    hasher.update(body);
    out.extend_from_slice(&head);
    out.extend_from_slice(body);
    out.extend_from_slice(&hasher.finalize().as_bytes()[..CHECKSUM_BYTES]);
}

fn define(id: u32, source: &SourceDef) -> Vec<u8> {
    serde_encode(&[
        ("ledger_only", "false"),
        ("file_id", &id.to_string()),
        ("relative_path", &source.relative_path),
        ("stream_id", &source.stream_id),
        ("session_id", &source.session_id),
        ("native_session_id", &source.native_session_id),
        (
            "agent_version",
            source.agent_version.as_deref().unwrap_or(""),
        ),
    ])
    .into_bytes()
}

/// The same entry stream as tagged frame bodies, with the given file ids and
/// one `TAG_RAW` frame per record: a meta batch, then per source its define,
/// twenty records and its seal.
fn hand_frames(ids: [u32; 2]) -> Vec<(u8, Vec<u8>)> {
    let mut frames = Vec::new();
    let mut meta = 7_u32.to_be_bytes().to_vec();
    meta.extend_from_slice(META.as_bytes());
    frames.push((TAG_META_VERSIONED, meta));
    for (id, path) in ids.into_iter().zip(["a.jsonl", "b.jsonl"]) {
        frames.push((TAG_DEFINE, define(id, &source(path))));
        for index in 0..20_u64 {
            let mut body = id.to_be_bytes().to_vec();
            body.extend_from_slice(&1_u64.to_be_bytes());
            body.extend_from_slice(&(index * 100).to_be_bytes());
            body.extend_from_slice(&record(index));
            frames.push((TAG_RAW, body));
        }
        let mut seal = id.to_be_bytes().to_vec();
        seal.extend_from_slice(&1_u64.to_be_bytes());
        seal.extend_from_slice(&3_u32.to_be_bytes());
        seal.push(1);
        frames.push((TAG_GENERATION_COMPLETE, seal));
    }
    frames
}

/// `frames` as one plain segment.
fn write_frames(dir: &Path, frames: &[(u8, Vec<u8>)]) {
    std::fs::create_dir_all(dir).unwrap();
    let mut out = MAGIC_PLAIN.to_vec();
    for (tag, body) in frames {
        frame(&mut out, *tag, body);
    }
    std::fs::write(dir.join("00000000.rledger"), out).unwrap();
}

/// The same entry stream, hand-framed into one plain segment.
fn write_by_hand(dir: &Path, ids: [u32; 2]) {
    write_frames(dir, &hand_frames(ids));
}

#[test]
fn the_entry_digest_ignores_containers_segments_ids_and_packing() {
    let temp = TempDir::new().unwrap();
    let rotated = temp.path().join("rotated");
    let mut ledger = Ledger::open(&rotated)
        .unwrap()
        .with_codec(SegmentCodec::Plain)
        .with_max_segment_bytes(512);
    write_through_ledger(&mut ledger, true);
    drop(ledger);

    let single = temp.path().join("single");
    let mut ledger = Ledger::open(&single).unwrap();
    write_through_ledger(&mut ledger, false);
    drop(ledger);

    let dense = temp.path().join("dense");
    write_by_hand(&dense, [0, 1]);
    let keyed = temp.path().join("keyed");
    write_by_hand(&keyed, [41, 7]);

    let index = std::fs::read(rotated.join(crate::segment_index::INDEX_FILE)).unwrap();
    let rotated = verify(&rotated).unwrap();
    let single = verify(&single).unwrap();
    let dense = verify(&dense).unwrap();
    let keyed = verify(&keyed).unwrap();
    assert!(rotated.segments.len() > 2, "rotation must split the stream");
    assert_eq!(single.segments.len(), 1);
    for report in [&rotated, &single, &dense, &keyed] {
        assert_eq!(report.torn_segments(), 0);
        assert_eq!(
            report.tally,
            EntryTally {
                entries: 43,
                raw_records: 40,
                raw_payload_bytes: (0..20).map(|index| record(index).len() as u64).sum::<u64>() * 2,
                seals: 2,
                metas: 1,
                ..EntryTally::default()
            }
        );
        assert_eq!(
            report
                .segments
                .iter()
                .map(|segment| segment.entries)
                .sum::<u64>(),
            43
        );
    }
    assert_eq!(rotated.entry_digest, single.entry_digest);
    assert_eq!(rotated.entry_digest, dense.entry_digest);
    assert_eq!(rotated.entry_digest, keyed.entry_digest);

    // Frame digests ignore the container but see framing: a zstd segment and
    // a hand-framed plain one with the writer's own ids decode alike, while
    // other ids or batch packing change the decoded bytes.
    assert_eq!(single.segments[0].compressed, Some(true));
    assert_eq!(dense.segments[0].compressed, Some(false));
    assert_eq!(single.frame_digest(), dense.frame_digest());
    assert_ne!(single.frame_digest(), keyed.frame_digest());
    assert_ne!(rotated.frame_digest(), single.frame_digest());

    // Closed segments are indexed; a hand-framed segment never was.
    assert_eq!(rotated.index_digest, Some(blake3::hash(&index)));
    assert!(single.index_digest.is_some());
    assert_eq!(keyed.index_digest, None);
}

#[test]
fn recompression_keeps_every_segment_digest_and_the_entry_digest() {
    let temp = TempDir::new().unwrap();
    let dir = temp.path().join("ledger");
    let mut ledger = Ledger::open(&dir)
        .unwrap()
        .with_codec(SegmentCodec::Plain)
        .with_max_segment_bytes(512);
    write_through_ledger(&mut ledger, true);
    drop(ledger);
    let before = verify(&dir).unwrap();
    assert!(before.segments.iter().all(|s| s.compressed == Some(false)));

    for segment in &before.segments {
        let outcome = recompress_segment(&segment.path, SegmentCodec::default(), true).unwrap();
        assert!(matches!(outcome, RecompressOutcome::Rewritten { .. }));
    }
    let compressed = verify(&dir).unwrap();
    assert!(
        compressed
            .segments
            .iter()
            .all(|s| s.compressed == Some(true))
    );
    // And forced again, into a different zstd configuration.
    for segment in &compressed.segments {
        recompress_segment(&segment.path, SegmentCodec::zstd(9), true).unwrap();
    }
    let forced = verify(&dir).unwrap();

    for after in [&compressed, &forced] {
        assert_eq!(after.entry_digest, before.entry_digest);
        assert_eq!(after.frame_digest(), before.frame_digest());
        assert_eq!(after.tally, before.tally);
        for (a, b) in before.segments.iter().zip(&after.segments) {
            assert_eq!(
                (a.sequence, a.decoded_bytes, a.frame_digest, a.clean),
                (b.sequence, b.decoded_bytes, b.frame_digest, b.clean)
            );
        }
    }
}

#[test]
fn torn_and_unreadable_segments_are_reported_with_their_intact_prefix() {
    let temp = TempDir::new().unwrap();
    let dir = temp.path().join("ledger");
    let mut ledger = Ledger::open(&dir)
        .unwrap()
        .with_codec(SegmentCodec::Plain)
        .with_max_segment_bytes(512);
    write_through_ledger(&mut ledger, true);
    drop(ledger);
    let before = verify(&dir).unwrap();
    let last = before.segments.last().unwrap().clone();

    // A truncated frame header after the last intact frame.
    let mut file = OpenOptions::new().append(true).open(&last.path).unwrap();
    file.write_all(&[2, 0, 0, 1]).unwrap();
    drop(file);
    let torn = verify(&dir).unwrap();
    let torn_last = torn.segments.last().unwrap();
    assert_eq!(torn.torn_segments(), 1);
    assert!(!torn_last.clean);
    assert_eq!(torn_last.decoded_bytes, last.decoded_bytes);
    assert_eq!(torn_last.frame_digest, last.frame_digest);
    assert_eq!(torn_last.disk_bytes, last.disk_bytes + 4);
    assert_eq!(torn.entry_digest, before.entry_digest);

    // A flipped byte inside the first frame's body tears it from there.
    let first = &before.segments[0];
    let mut bytes = std::fs::read(&first.path).unwrap();
    bytes[12] ^= 0xff;
    std::fs::write(&first.path, &bytes).unwrap();
    let corrupt = verify(&dir).unwrap();
    assert!(!corrupt.segments[0].clean);
    assert_eq!(corrupt.segments[0].decoded_bytes, 0);
    assert_eq!(corrupt.segments[0].entries, 0);
    assert_ne!(corrupt.entry_digest, before.entry_digest);

    // An unknown header is torn from its first byte.
    std::fs::write(dir.join("00009999.rledger"), b"NOPE").unwrap();
    let unknown = verify(&dir).unwrap();
    let odd = unknown.segments.last().unwrap();
    assert_eq!((odd.sequence, odd.compressed), (9999, None));
    assert_eq!((odd.decoded_bytes, odd.entries, odd.clean), (0, 0, false));
    assert_eq!(odd.frame_digest, blake3::Hasher::new().finalize());
}

#[test]
fn the_canonical_encoding_separates_neighbouring_fields() {
    let digest = |entries: &[LedgerEntry]| {
        let mut digest = EntryDigest::new();
        for entry in entries {
            digest.update(entry);
        }
        digest.finish()
    };
    let raw = |path: &str, payload: &[u8]| LedgerEntry::Raw {
        source: source(path),
        generation: 1,
        start_offset: 0,
        payload: payload.to_vec(),
    };
    assert_ne!(
        digest(&[raw("a", b"bc"), raw("a", b"d")]),
        digest(&[raw("a", b"b"), raw("a", b"cd")])
    );
    assert_ne!(digest(&[raw("ab", b"c")]), digest(&[raw("a", b"bc")]));
    let mut unversioned = source("a");
    unversioned.agent_version = None;
    let mut empty = source("a");
    empty.agent_version = Some(String::new());
    let with = |source: SourceDef| LedgerEntry::GenerationComplete {
        source,
        generation: 1,
        normalizer_version: 1,
        normalized_clean: true,
    };
    assert_ne!(digest(&[with(unversioned)]), digest(&[with(empty)]));
    assert_eq!(digest(&[]), blake3::Hasher::new().finalize());
}

#[test]
fn an_absent_ledger_verifies_empty() {
    let temp = TempDir::new().unwrap();
    let report = verify(&temp.path().join("absent")).unwrap();
    assert!(report.segments.is_empty());
    assert_eq!(report.tally, EntryTally::default());
    assert_eq!(report.entry_digest, EntryDigest::new().finish());
    assert_eq!(report.source_set_digest, blake3::Hasher::new().finalize());
    assert_eq!(report.index_digest, None);
}

#[test]
fn metadata_and_captured_content_have_separate_digests() {
    let temp = TempDir::new().unwrap();
    let write = |name: &str, discovered_at: u64, payload: &[u8]| {
        let dir = temp.path().join(name);
        let mut ledger = Ledger::open(&dir).unwrap();
        ledger
            .append_meta(
                7,
                &format!("[{{\"op\":\"runtime\",\"discovered_at\":{discovered_at}}}]"),
            )
            .unwrap();
        ledger
            .append_raw_owned(&source("a.jsonl"), 1, vec![(0, payload.to_vec())])
            .unwrap();
        ledger
            .complete_generation(&source("a.jsonl"), 1, 3, true)
            .unwrap();
        ledger.flush().unwrap();
        drop(ledger);
        verify(&dir).unwrap()
    };
    let first = write("first", 1_790_547_452_169, b"line");
    let rediscovered = write("rediscovered", 1_790_551_234_567, b"line");
    let changed = write("changed", 1_790_547_452_169, b"LINE");

    // Another discovery time: only the metadata differs.
    assert_ne!(rediscovered.entry_digest, first.entry_digest);
    assert_ne!(rediscovered.meta_digest, first.meta_digest);
    assert_eq!(rediscovered.source_digest, first.source_digest);
    assert_eq!(rediscovered.source_set_digest, first.source_set_digest);
    // Other captured bytes: only the source entries differ.
    assert_ne!(changed.entry_digest, first.entry_digest);
    assert_eq!(changed.meta_digest, first.meta_digest);
    assert_ne!(changed.source_digest, first.source_digest);
    assert_ne!(changed.source_set_digest, first.source_set_digest);

    let mut digest = EntryDigest::new();
    assert_eq!(digest.finish(), digest.source_digest());
    digest.update(&LedgerEntry::Meta {
        schema_version: 1,
        payload: "[]".to_string(),
    });
    assert_eq!(digest.source_digest(), EntryDigest::new().source_digest());
    assert_eq!(digest.meta_digest(), digest.finish());
}

/// A small stream of every entry kind over two sources, one ledger-only and
/// unversioned.
fn pinned_entries() -> Vec<LedgerEntry> {
    let raw = |path: &str, offset: u64, payload: &[u8]| LedgerEntry::Raw {
        source: source(path),
        generation: 1,
        start_offset: offset,
        payload: payload.to_vec(),
    };
    let mut unversioned = source("b.jsonl");
    unversioned.agent_version = None;
    unversioned.ledger_only = true;
    vec![
        LedgerEntry::Meta {
            schema_version: 7,
            payload: META.to_string(),
        },
        raw("a.jsonl", 0, b"first"),
        LedgerEntry::Raw {
            source: unversioned.clone(),
            generation: 2,
            start_offset: 5,
            payload: b"guardian".to_vec(),
        },
        LedgerEntry::Document {
            source: source("a.jsonl"),
            generation: 1,
            start_offset: 0,
            document: CapturedDocument {
                path: "/fixture/doc.html".to_string(),
                captured_at: 1_790_000_000_000,
                status: DocumentCaptureStatus::Captured,
                recovered_from: None,
                media: None,
                bytes: b"<p>doc</p>".to_vec(),
            },
        },
        raw("a.jsonl", 6, b"second"),
        LedgerEntry::GenerationComplete {
            source: unversioned,
            generation: 2,
            normalizer_version: 3,
            normalized_clean: false,
        },
        LedgerEntry::GenerationComplete {
            source: source("a.jsonl"),
            generation: 1,
            normalizer_version: 3,
            normalized_clean: true,
        },
    ]
}

fn digest_of(entries: &[LedgerEntry]) -> EntryDigest {
    let mut digest = EntryDigest::new();
    for entry in entries {
        digest.update(entry);
    }
    digest
}

/// The digests two builds are compared by keep their encoding (R1 I5): the
/// entry, source and meta values predate the per-source digests, which left
/// them unchanged. They predate the media claim too: the pinned stream holds
/// an HTML document frame, and a ledger with no media frame keeps every one
/// of these values.
#[test]
fn the_entry_source_and_meta_digests_keep_their_encoding() {
    let digest = digest_of(&pinned_entries());
    assert_eq!(digest.tally().documents, 1);
    assert_eq!(digest.tally().media_frames(), 0);
    assert_eq!(
        digest.finish().to_hex().as_str(),
        "343b655e37d643f938846ec3606af5054cebcc8d42761e2e5526b7d83cd01e47"
    );
    assert_eq!(
        digest.source_digest().to_hex().as_str(),
        "3921b10e72a6d110ab0866beeb9d65a494770c7563f1e9fd271899adae76498d"
    );
    assert_eq!(
        digest.meta_digest().to_hex().as_str(),
        "6e9673199abe8ff6c6218b14130ac3df42ea591d6f01c72b4a8f60f560b5aa91"
    );
    assert_eq!(
        digest.source_set_digest().to_hex().as_str(),
        "528d2fb8c50677c77d9d874047f86a6d1655354f7a54003e2b4e4bd194f8f914"
    );
}

/// The source set digest is exactly the per-source encodings folded in path
/// order: one source alone is its own encoding, under its path.
#[test]
fn a_single_source_set_digest_folds_that_sources_encoding() {
    let entries = pinned_entries();
    let only_a = entries
        .iter()
        .filter(|entry| match entry {
            LedgerEntry::Raw { source, .. }
            | LedgerEntry::Document { source, .. }
            | LedgerEntry::GenerationComplete { source, .. } => source.relative_path == "a.jsonl",
            LedgerEntry::Meta { .. } => false,
        })
        .cloned()
        .collect::<Vec<_>>();
    let digest = digest_of(&only_a);
    let mut folded = blake3::Hasher::new();
    folded.update(&7_u64.to_be_bytes());
    folded.update(b"a.jsonl");
    folded.update(digest.source_digest().as_bytes());
    assert_eq!(digest.source_set_digest(), folded.finalize());
}

/// Two ledgers holding the same per-source entries, interleaved differently
/// (as two root-tree walks may frame them): the source set digest agrees
/// where the order-sensitive digests cannot.
#[test]
fn source_set_digest_ignores_source_order() {
    let temp = TempDir::new().unwrap();
    // b first, then a: each source's first appearance differs between the
    // two ledgers, so folding in insertion order would disagree.
    let write = |name: &str, order: &[(&str, u64)]| {
        let dir = temp.path().join(name);
        let mut ledger = Ledger::open(&dir).unwrap();
        ledger.append_meta(7, META).unwrap();
        for (path, index) in order {
            ledger
                .append_raw(&source(path), 1, [(index * 100, record(*index))])
                .unwrap();
            if *index == 3 {
                ledger
                    .complete_generation(&source(path), 1, 3, true)
                    .unwrap();
            }
        }
        ledger.flush().unwrap();
        drop(ledger);
        verify(&dir).unwrap()
    };
    let grouped = write(
        "grouped",
        &[
            ("a.jsonl", 0),
            ("a.jsonl", 1),
            ("a.jsonl", 2),
            ("a.jsonl", 3),
            ("b.jsonl", 0),
            ("b.jsonl", 1),
            ("b.jsonl", 2),
            ("b.jsonl", 3),
        ],
    );
    let interleaved = write(
        "interleaved",
        &[
            ("b.jsonl", 0),
            ("a.jsonl", 0),
            ("b.jsonl", 1),
            ("b.jsonl", 2),
            ("a.jsonl", 1),
            ("a.jsonl", 2),
            ("b.jsonl", 3),
            ("a.jsonl", 3),
        ],
    );
    assert_eq!(grouped.tally, interleaved.tally);
    assert_eq!(grouped.source_set_digest, interleaved.source_set_digest);
    assert_ne!(grouped.entry_digest, interleaved.entry_digest);
    assert_ne!(grouped.source_digest, interleaved.source_digest);
    assert_eq!(grouped.meta_digest, interleaved.meta_digest);
}

#[test]
fn source_set_digest_sees_order_within_a_source() {
    let raw = |path: &str, index: u64| LedgerEntry::Raw {
        source: source(path),
        generation: 1,
        start_offset: index * 100,
        payload: record(index),
    };
    let ordered = digest_of(&[raw("a.jsonl", 0), raw("a.jsonl", 1), raw("b.jsonl", 0)]);
    let swapped = digest_of(&[raw("a.jsonl", 1), raw("a.jsonl", 0), raw("b.jsonl", 0)]);
    assert_eq!(ordered.tally(), swapped.tally());
    assert_ne!(ordered.source_set_digest(), swapped.source_set_digest());
    // The same entries with the other source moved still agree.
    let moved = digest_of(&[raw("b.jsonl", 0), raw("a.jsonl", 0), raw("a.jsonl", 1)]);
    assert_eq!(ordered.source_set_digest(), moved.source_set_digest());
}

#[test]
fn source_set_digest_ignores_document_captured_at() {
    let document = |captured_at: i64| LedgerEntry::Document {
        source: source("a.jsonl"),
        generation: 1,
        start_offset: 0,
        document: CapturedDocument {
            path: "/fixture/review.html".to_string(),
            captured_at,
            status: DocumentCaptureStatus::Captured,
            recovered_from: None,
            media: None,
            bytes: b"<p>review</p>".to_vec(),
        },
    };
    let first = digest_of(&[document(1_790_000_000_000)]);
    let later = digest_of(&[document(1_790_000_009_999)]);
    assert_ne!(first.finish(), later.finish());
    assert_eq!(first.source_digest(), later.source_digest());
    assert_eq!(first.source_set_digest(), later.source_set_digest());
    // Its content still counts.
    let mut other = document(1_790_000_000_000);
    if let LedgerEntry::Document { document, .. } = &mut other {
        document.bytes = b"<p>other</p>".to_vec();
    }
    assert_ne!(
        digest_of(&[other]).source_set_digest(),
        first.source_set_digest()
    );
}

/// Ignoring the order sources interleave in must not hide a lost or repeated
/// frame: dropping or duplicating one raw or seal frame changes the digest,
/// while moving one source's frames ahead of another's does not.
#[test]
fn source_set_digest_sees_a_dropped_or_duplicated_frame() {
    let temp = TempDir::new().unwrap();
    let frames = hand_frames([0, 1]);
    let digest = |name: &str, frames: &[(u8, Vec<u8>)]| {
        let dir = temp.path().join(name);
        write_frames(&dir, frames);
        let report = verify(&dir).unwrap();
        assert_eq!(report.torn_segments(), 0, "{name}");
        report
    };
    let base = digest("base", &frames);
    // Frame 0 is the meta batch, 1 a's define, 2..22 its records, 22 its
    // seal, 23 b's define, 24..44 its records, 44 its seal.
    assert_eq!(frames.len(), 45);
    assert_eq!(frames[22].0, TAG_GENERATION_COMPLETE);

    let mut b_first = frames[23..].to_vec();
    b_first.insert(0, frames[0].clone());
    b_first.extend_from_slice(&frames[1..23]);
    let reordered = digest("reordered", &b_first);
    assert_eq!(reordered.source_set_digest, base.source_set_digest);
    assert_ne!(reordered.source_digest, base.source_digest);

    let mut dropped = frames.clone();
    dropped.remove(30);
    let dropped = digest("dropped", &dropped);
    assert_eq!(dropped.tally.raw_records, base.tally.raw_records - 1);
    assert_ne!(dropped.source_set_digest, base.source_set_digest);

    let mut duplicated = frames.clone();
    duplicated.insert(6, frames[5].clone());
    let duplicated = digest("duplicated", &duplicated);
    assert_eq!(duplicated.tally.raw_records, base.tally.raw_records + 1);
    assert_ne!(duplicated.source_set_digest, base.source_set_digest);

    let mut resealed = frames.clone();
    resealed.insert(23, frames[22].clone());
    let resealed = digest("resealed", &resealed);
    assert_eq!(resealed.tally.seals, base.tally.seals + 1);
    assert_ne!(resealed.source_set_digest, base.source_set_digest);

    let mut unsealed = frames.clone();
    unsealed.remove(44);
    let unsealed = digest("unsealed", &unsealed);
    assert_eq!(unsealed.tally.seals, base.tally.seals - 1);
    assert_ne!(unsealed.source_set_digest, base.source_set_digest);

    // Moving the runs does not mask a repeat: b's last record again, after
    // a's run.
    let mut both = b_first.clone();
    both.push(frames[43].clone());
    assert_ne!(
        digest("moved-and-duplicated", &both).source_set_digest,
        base.source_set_digest
    );

    // An altered frame: one payload byte of one of b's records (the body is
    // file id, generation and offset, then the record), and a seal's
    // `normalized_clean` flag. Each frame is re-checksummed, so each reads
    // clean.
    let mut altered = frames.clone();
    let body = &mut altered[30].1;
    let at = body.len() - 2;
    body[at] ^= 1;
    let altered = digest("altered", &altered);
    assert_eq!(altered.tally, base.tally);
    assert_ne!(altered.source_set_digest, base.source_set_digest);
    let mut unclean = frames.clone();
    *unclean[22].1.last_mut().unwrap() = 0;
    let unclean = digest("unclean", &unclean);
    assert_eq!(unclean.tally, base.tally);
    assert_ne!(unclean.source_set_digest, base.source_set_digest);

    // Two of b's records swapped: the same entries, reordered within their
    // source, after the runs moved too.
    let mut swapped = b_first.clone();
    let (first, second) = (
        swapped
            .iter()
            .position(|frame| *frame == frames[30])
            .unwrap(),
        swapped
            .iter()
            .position(|frame| *frame == frames[31])
            .unwrap(),
    );
    swapped.swap(first, second);
    let swapped = digest("swapped-within-b", &swapped);
    assert_eq!(swapped.tally, base.tally);
    assert_ne!(swapped.source_set_digest, base.source_set_digest);
}

/// A segment holding document frames used to verify torn at its first one.
/// Now it verifies clean in every container, with what became of each sent
/// file counted by status.
#[test]
fn a_segment_with_document_frames_verifies_clean() {
    let temp = TempDir::new().unwrap();
    let expected = EntryTally {
        entries: 14,
        raw_records: 3,
        raw_payload_bytes: RAW_PAYLOAD_BYTES,
        documents: 8,
        document_bytes: HTML_BYTES * 2 + CLIP_BYTES * 2,
        seals: 2,
        metas: 1,
        media_captured: 2,
        media_captured_bytes: CLIP_BYTES * 2,
        media_missing: 1,
        media_too_large: 1,
        media_unsupported: 1,
        media_changed_during_read: 1,
    };
    let mut digests = Vec::new();
    for (name, codec) in [
        ("plain", SegmentCodec::Plain),
        ("frames", SegmentCodec::default()),
        ("stream", SegmentCodec::zstd(3)),
    ] {
        let dir = temp.path().join(name);
        let path = write_document_segment(&dir, codec);
        let report = verify(&dir).unwrap();
        assert_eq!(report.torn_segments(), 0, "{name}");
        assert_eq!(report.segments.len(), 1, "{name}");
        let segment = &report.segments[0];
        assert!(segment.clean, "{name}");
        assert_eq!(segment.compressed, Some(codec.is_compressed()), "{name}");
        assert_eq!(segment.entries, 14, "{name}");
        assert_eq!(report.tally, expected, "{name}");
        assert_eq!(report.tally.media_frames(), 6, "{name}");
        if !codec.is_compressed() {
            let file = std::fs::metadata(&path).unwrap().len();
            assert_eq!(segment.decoded_bytes, file - MAGIC_PLAIN.len() as u64);
        }
        digests.push((
            report.entry_digest,
            report.source_digest,
            report.source_set_digest,
        ));
    }
    assert_eq!(digests[0], digests[1]);
    assert_eq!(digests[0], digests[2]);

    // Recompressed, the same evidence: nothing about the entries moved.
    let dir = temp.path().join("plain");
    let before = verify(&dir).unwrap();
    let outcome =
        recompress_segment(&before.segments[0].path, SegmentCodec::default(), false).unwrap();
    assert!(
        matches!(outcome, RecompressOutcome::Rewritten { .. }),
        "{outcome:?}"
    );
    let after = verify(&dir).unwrap();
    assert_eq!(after.segments[0].compressed, Some(true));
    assert_eq!(after.torn_segments(), 0);
    assert_eq!(after.tally, before.tally);
    assert_eq!(after.entry_digest, before.entry_digest);
    assert_eq!(after.source_set_digest, before.source_set_digest);
    assert_eq!(after.frame_digest(), before.frame_digest());
}

fn document_entry(document: CapturedDocument) -> LedgerEntry {
    LedgerEntry::Document {
        source: source("a.jsonl"),
        generation: 1,
        start_offset: 6,
        document,
    }
}

#[test]
fn the_tally_counts_media_frames_by_status() {
    let base = EntryTally {
        entries: 1,
        documents: 1,
        ..EntryTally::default()
    };
    let expected = [
        EntryTally {
            document_bytes: 9,
            media_captured: 1,
            media_captured_bytes: 9,
            ..base
        },
        EntryTally {
            media_missing: 1,
            ..base
        },
        EntryTally {
            media_too_large: 1,
            ..base
        },
        EntryTally {
            media_unsupported: 1,
            ..base
        },
        EntryTally {
            media_changed_during_read: 1,
            ..base
        },
    ];
    let mut all = EntryDigest::new();
    for (status, expected) in STATUSES.into_iter().zip(expected) {
        let entry = document_entry(media(status, "video/mp4", 9));
        let tally = digest_of(std::slice::from_ref(&entry)).tally();
        assert_eq!(tally, expected, "{status:?}");
        assert_eq!(tally.media_frames(), 1, "{status:?}");
        all.update(&entry);
    }
    // An HTML document, captured or not, is no media frame.
    let mut missing = html(None);
    missing.status = DocumentCaptureStatus::Missing;
    missing.bytes.clear();
    for document in [html(None), html(Some("/archive/review.html")), missing] {
        let entry = document_entry(document);
        let tally = digest_of(std::slice::from_ref(&entry)).tally();
        assert_eq!(tally.documents, 1);
        assert_eq!(tally.media_frames(), 0);
        assert_eq!(tally.media_captured_bytes, 0);
        all.update(&entry);
    }
    assert_eq!(
        all.tally(),
        EntryTally {
            entries: 8,
            documents: 8,
            document_bytes: 9 + HTML_BYTES * 2,
            media_captured: 1,
            media_captured_bytes: 9,
            media_missing: 1,
            media_too_large: 1,
            media_unsupported: 1,
            media_changed_during_read: 1,
            ..EntryTally::default()
        }
    );
    assert_eq!(all.tally().media_frames(), 5);
}

/// The media claim is source content. It reaches the digests only through a
/// frame that carries one, so the HTML-only stream pinned above keeps its
/// values; a media frame changes its own source's digest and no other's.
#[test]
fn the_media_claim_feeds_the_source_digests() {
    let with = |document: CapturedDocument| {
        let mut entries = pinned_entries();
        entries.push(document_entry(document));
        digest_of(&entries)
    };
    let source_digest = |digest: &EntryDigest, path: &str| digest.per_source[path].finalize();
    let every = |digest: &EntryDigest| {
        [
            digest.finish(),
            digest.source_digest(),
            digest.source_set_digest(),
            source_digest(digest, "a.jsonl"),
        ]
    };
    // Every digest that sees source a differs; source b's and the metadata's do not.
    let differ = |left: &EntryDigest, right: &EntryDigest, what: &str| {
        for (left, right) in every(left).into_iter().zip(every(right)) {
            assert_ne!(left, right, "{what}");
        }
        assert_eq!(
            source_digest(left, "b.jsonl"),
            source_digest(right, "b.jsonl"),
            "{what}"
        );
        assert_eq!(left.meta_digest(), right.meta_digest(), "{what}");
    };

    let html_only = digest_of(&pinned_entries());
    let missing = with(media(DocumentCaptureStatus::Missing, "video/mp4", 77));
    differ(&missing, &html_only, "a media frame");

    // Two streams that differ only in the claim: its size, then its type.
    let longer = with(media(DocumentCaptureStatus::Missing, "video/mp4", 78));
    assert_eq!(missing.tally(), longer.tally());
    differ(&missing, &longer, "expected_len");
    let quicktime = with(media(DocumentCaptureStatus::Missing, "video/quicktime", 77));
    assert_eq!(missing.tally(), quicktime.tally());
    differ(&missing, &quicktime, "media_type");
    // A neighbouring split of type and size is not the same claim.
    let unsupported = |media_type: &str, expected_len: u64| {
        with(media(
            DocumentCaptureStatus::Unsupported,
            media_type,
            expected_len,
        ))
    };
    differ(
        &unsupported("video/mp4", u64::from_be_bytes(*b"0\0\0\0\0\0\0\x01")),
        &unsupported("video/mp40", 1),
        "the boundary between type and size",
    );

    // The same path, status and bytes with and without a claim.
    let mut bare = media(DocumentCaptureStatus::Captured, "video/mp4", 9);
    let claimed = with(bare.clone());
    bare.media = None;
    let unclaimed = with(bare);
    differ(&claimed, &unclaimed, "the claim itself");
    assert_eq!(claimed.tally().media_frames(), 1);
    assert_eq!(unclaimed.tally().media_frames(), 0);

    // The observation time stays run-dependent: only the entry digest sees it.
    let mut observed_later = media(DocumentCaptureStatus::Missing, "video/mp4", 77);
    observed_later.captured_at += 9_999;
    let later = with(observed_later);
    assert_ne!(later.finish(), missing.finish());
    assert_eq!(later.source_digest(), missing.source_digest());
    assert_eq!(later.source_set_digest(), missing.source_set_digest());

    // Through real ledgers: only the media frame's `expected_len` differs.
    let temp = TempDir::new().unwrap();
    let write = |name: &str, expected_len: Option<u64>| {
        let dir = temp.path().join(name);
        let mut ledger = Ledger::open(&dir).unwrap();
        ledger.append_meta(7, META).unwrap();
        for path in ["a.jsonl", "b.jsonl"] {
            let source = source(path);
            ledger.append_raw(&source, 1, [(0, record(0))]).unwrap();
            if path == "a.jsonl" {
                ledger.append_document(&source, 1, 0, html(None)).unwrap();
                if let Some(expected_len) = expected_len {
                    let document =
                        media(DocumentCaptureStatus::TooLarge, "video/mp4", expected_len);
                    ledger.append_document(&source, 1, 0, document).unwrap();
                }
            }
            ledger.complete_generation(&source, 1, 3, true).unwrap();
        }
        ledger.flush().unwrap();
        drop(ledger);
        let report = verify(&dir).unwrap();
        assert_eq!(report.torn_segments(), 0, "{name}");
        report
    };
    let html_ledger = write("html", None);
    let first = write("first", Some(70 * 1024 * 1024));
    let second = write("second", Some(70 * 1024 * 1024 + 1));
    assert_eq!(first.tally, second.tally);
    assert_eq!(first.tally.media_too_large, 1);
    assert_eq!(html_ledger.tally.media_frames(), 0);
    for other in [&second, &html_ledger] {
        assert_ne!(first.entry_digest, other.entry_digest);
        assert_ne!(first.source_digest, other.source_digest);
        assert_ne!(first.source_set_digest, other.source_set_digest);
        assert_eq!(first.meta_digest, other.meta_digest);
    }
}
