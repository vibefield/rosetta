//! Roster-keyed file ids, prebuilt frames and the byte-bounded queue.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc::channel;
use std::time::Duration;

use tempfile::TempDir;

use super::*;
use crate::segment_index::INDEX_FILE;

fn source(path: &str) -> SourceDef {
    SourceDef {
        ledger_only: false,
        relative_path: path.to_string(),
        stream_id: "transcript".to_string(),
        session_id: format!("session:{path}"),
        native_session_id: format!("native:{path}"),
        agent_version: Some("1.2.3".to_string()),
    }
}

fn keys(paths: &[&str]) -> Arc<HashMap<SourceDef, u32>> {
    Arc::new(
        paths
            .iter()
            .enumerate()
            .map(|(key, path)| (source(path), key as u32))
            .collect(),
    )
}

fn records(from: u64, count: u64, len: usize) -> Vec<(u64, Vec<u8>)> {
    (from..from + count)
        .map(|index| (index * 1000, vec![b'a' + (index % 26) as u8; len]))
        .collect()
}

/// Every file of a ledger directory — segments and the segment index.
fn files(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            (
                path.file_name().unwrap().to_string_lossy().into_owned(),
                std::fs::read(&path).unwrap(),
            )
        })
        .collect()
}

/// `(file id, source)` of every define frame in a plain segment, in order.
fn defines(segment: &Path) -> Vec<(u32, SourceDef)> {
    let bytes = std::fs::read(segment).unwrap();
    assert_eq!(&bytes[..4], b"RLG1");
    let mut at = 4;
    let mut out = Vec::new();
    while at < bytes.len() {
        let body_len = u32::from_be_bytes(bytes[at + 1..at + 5].try_into().unwrap()) as usize;
        let body = &bytes[at + 5..at + 5 + body_len];
        if bytes[at] == TAG_DEFINE {
            out.push(decode_define(body).unwrap());
        }
        at += 5 + body_len + CHECKSUM_BYTES;
    }
    out
}

fn plain(dir: &Path, max_segment_bytes: u64) -> Ledger {
    Ledger::open(dir)
        .unwrap()
        .with_codec(SegmentCodec::Plain)
        .with_max_segment_bytes(max_segment_bytes)
}

/// One capture-shaped sequence: metadata, then batches of several sources
/// interleaved, each generation sealed after its last batch, a checkpoint
/// after every batch as the pipeline sends them.
fn legacy_sequence(ledger: &mut Ledger) {
    ledger.append_meta(3, "[{\"op\":\"runtime\"}]").unwrap();
    ledger.checkpoint().unwrap();
    for round in 0..6_u64 {
        for (path, generation) in [("c.jsonl", 1), ("a.jsonl", 1), ("b.jsonl", 2)] {
            let source = source(path);
            ledger
                .append_raw_owned(&source, generation, records(round * 3, 3, 300))
                .unwrap();
            if round == 5 {
                ledger
                    .complete_generation(&source, generation, 9, path != "b.jsonl")
                    .unwrap();
            }
            ledger.checkpoint().unwrap();
        }
    }
}

/// The same sequence, with `prebuilt` sources framed ahead of the ledger.
fn mixed_sequence(ledger: &mut Ledger, keys: &HashMap<SourceDef, u32>, prebuilt: &[&str]) {
    ledger.append_meta(3, "[{\"op\":\"runtime\"}]").unwrap();
    ledger.checkpoint().unwrap();
    for round in 0..6_u64 {
        for (path, generation) in [("c.jsonl", 1), ("a.jsonl", 1), ("b.jsonl", 2)] {
            let source = source(path);
            let batch = records(round * 3, 3, 300);
            let seal = (round == 5).then_some(path != "b.jsonl");
            if prebuilt.contains(&path) {
                let mut frames = PrebuiltFrames::new(keys[&source], generation);
                frames.push_records(batch).unwrap();
                if let Some(clean) = seal {
                    frames.push_seal(9, clean);
                }
                ledger.append_prebuilt(&source, frames).unwrap();
            } else {
                ledger.append_raw_owned(&source, generation, batch).unwrap();
                if let Some(clean) = seal {
                    ledger
                        .complete_generation(&source, generation, 9, clean)
                        .unwrap();
                }
            }
            ledger.checkpoint().unwrap();
        }
    }
}

#[test]
fn keys_in_first_definition_order_write_the_dense_ledger_byte_for_byte() {
    let dense = TempDir::new().unwrap();
    let keyed = TempDir::new().unwrap();
    let mut ledger = plain(dense.path(), 1 << 30);
    legacy_sequence(&mut ledger);
    drop(ledger);
    let mut ledger = plain(keyed.path(), 1 << 30);
    // c, a, b is the order the sequence first defines them.
    ledger
        .set_source_keys(keys(&["c.jsonl", "a.jsonl", "b.jsonl"]))
        .unwrap();
    legacy_sequence(&mut ledger);
    drop(ledger);
    assert_eq!(files(keyed.path()), files(dense.path()));
}

#[test]
fn keyed_ledgers_decode_like_dense_ones_across_rotations() {
    let dense = TempDir::new().unwrap();
    let keyed = TempDir::new().unwrap();
    let roster = keys(&["a.jsonl", "b.jsonl", "c.jsonl"]);
    let mut ledger = plain(dense.path(), 4096);
    legacy_sequence(&mut ledger);
    drop(ledger);
    let mut ledger = plain(keyed.path(), 4096);
    ledger.set_source_keys(Arc::clone(&roster)).unwrap();
    legacy_sequence(&mut ledger);
    drop(ledger);

    let expected = scan(dense.path()).unwrap();
    let actual = scan(keyed.path()).unwrap();
    assert!(actual.segments > 3, "{} segments", actual.segments);
    assert_eq!(actual.torn_segments, 0);
    assert_eq!(actual.segments, expected.segments);
    assert_eq!(actual.entries, expected.entries);
    assert_eq!(
        verify(keyed.path()).unwrap().entry_digest,
        verify(dense.path()).unwrap().entry_digest
    );
    // The index names sources, never ids.
    assert_eq!(
        std::fs::read(keyed.path().join(INDEX_FILE)).unwrap(),
        std::fs::read(dense.path().join(INDEX_FILE)).unwrap()
    );
    // Every segment redefines what it uses, always under the roster key.
    for (_, segment) in segment_files(keyed.path()).unwrap() {
        let defined = defines(&segment);
        assert!(!defined.is_empty());
        for (id, source) in defined {
            assert_eq!(id, roster[&source], "{}", segment.display());
        }
    }
}

#[test]
fn prebuilt_frames_land_where_the_ledgers_own_framing_would() {
    let roster = keys(&["a.jsonl", "b.jsonl", "c.jsonl"]);
    for max_segment_bytes in [4096, 1 << 30] {
        let reference = TempDir::new().unwrap();
        let mut ledger = plain(reference.path(), max_segment_bytes);
        ledger.set_source_keys(Arc::clone(&roster)).unwrap();
        legacy_sequence(&mut ledger);
        drop(ledger);
        let expected = files(reference.path());

        // Synchronous, every source prebuilt.
        let sync = TempDir::new().unwrap();
        let mut ledger = plain(sync.path(), max_segment_bytes);
        ledger.set_source_keys(Arc::clone(&roster)).unwrap();
        mixed_sequence(&mut ledger, &roster, &["a.jsonl", "b.jsonl", "c.jsonl"]);
        drop(ledger);
        assert_eq!(files(sync.path()), expected, "{max_segment_bytes}");

        // Asynchronous behind a one-command queue, prebuilt and legacy
        // commands interleaved, keys sent through the queue after metadata.
        for budget in [1, 64 * 1024 * 1024] {
            let queued = TempDir::new().unwrap();
            let mut ledger = plain(queued.path(), max_segment_bytes)
                .defer_sync_until_flush()
                .into_async_bounded(budget);
            ledger.set_source_keys(Arc::clone(&roster)).unwrap();
            mixed_sequence(&mut ledger, &roster, &["a.jsonl", "c.jsonl"]);
            ledger.flush().unwrap();
            drop(ledger);
            assert_eq!(
                files(queued.path()),
                expected,
                "{max_segment_bytes} {budget}"
            );
        }
    }
}

#[test]
fn sources_outside_the_roster_get_ids_above_every_key() {
    let dir = TempDir::new().unwrap();
    let roster = keys(&["a.jsonl", "b.jsonl"]);
    let mut ledger = plain(dir.path(), 4096);
    ledger.set_source_keys(Arc::clone(&roster)).unwrap();
    for round in 0..8 {
        for path in ["x.jsonl", "a.jsonl", "y.jsonl"] {
            ledger
                .append_raw_owned(&source(path), 1, records(round * 2, 2, 400))
                .unwrap();
            ledger.checkpoint().unwrap();
        }
    }
    drop(ledger);
    let segments = segment_files(dir.path()).unwrap();
    assert!(segments.len() > 2);
    for (_, segment) in &segments {
        let mut unkeyed = Vec::new();
        for (id, source) in defines(segment) {
            match roster.get(&source) {
                Some(key) => assert_eq!(id, *key),
                None => unkeyed.push(id),
            }
        }
        // Numbered above the keys, from the top of the keys, per segment.
        let expected: Vec<u32> = (2..2 + unkeyed.len() as u32).collect();
        assert_eq!(unkeyed, expected, "{}", segment.display());
    }
    let scanned = scan(dir.path()).unwrap();
    assert_eq!(scanned.torn_segments, 0);
    assert_eq!(scanned.entries.len(), 8 * 3 * 2);
}

#[test]
fn roster_keys_must_number_the_roster_before_the_segment_defines_anything() {
    let dir = TempDir::new().unwrap();
    let mut ledger = plain(dir.path(), 1 << 30);
    let duplicate: HashMap<SourceDef, u32> =
        [(source("a.jsonl"), 0), (source("b.jsonl"), 0)].into();
    let error = ledger.set_source_keys(Arc::new(duplicate)).unwrap_err();
    assert!(error.to_string().contains("names two sources"), "{error}");
    let sparse: HashMap<SourceDef, u32> = [(source("a.jsonl"), 1)].into();
    let error = ledger.set_source_keys(Arc::new(sparse)).unwrap_err();
    assert!(error.to_string().contains("outside 0..1"), "{error}");

    ledger.append_meta(1, "[]").unwrap();
    ledger.set_source_keys(keys(&["a.jsonl"])).unwrap();
    ledger
        .append_raw_owned(&source("a.jsonl"), 1, records(0, 1, 10))
        .unwrap();
    let error = ledger.set_source_keys(keys(&["b.jsonl"])).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("before the open segment defines"),
        "{error}"
    );
}

#[test]
fn prebuilt_frames_must_carry_the_sources_key() {
    let dir = TempDir::new().unwrap();
    let mut ledger = plain(dir.path(), 1 << 30);
    let frames = || {
        let mut frames = PrebuiltFrames::new(1, 1);
        frames.push_records(records(0, 2, 10)).unwrap();
        frames
    };
    let error = ledger
        .append_prebuilt(&source("b.jsonl"), frames())
        .unwrap_err();
    assert!(error.to_string().contains("roster key is None"), "{error}");
    ledger
        .set_source_keys(keys(&["a.jsonl", "b.jsonl"]))
        .unwrap();
    let error = ledger
        .append_prebuilt(&source("a.jsonl"), frames())
        .unwrap_err();
    assert!(
        error.to_string().contains("roster key is Some(0)"),
        "{error}"
    );
    ledger.flush().unwrap();
    assert!(scan(dir.path()).unwrap().entries.is_empty());

    ledger
        .append_prebuilt(&source("b.jsonl"), frames())
        .unwrap();
    ledger.flush().unwrap();
    assert_eq!(scan(dir.path()).unwrap().entries.len(), 2);
}

#[test]
fn a_failed_worker_never_leaves_its_owner_waiting_for_queue_room() {
    let dir = TempDir::new().unwrap();
    let (done, finished) = channel();
    let owner_dir = dir.path().to_path_buf();
    std::thread::spawn(move || {
        let mut ledger = plain(&owner_dir, 1 << 30)
            .defer_sync_until_flush()
            .into_async_bounded(1024 * 1024);
        ledger
            .append_raw_owned(&source("a.jsonl"), 1, records(0, 1, 10))
            .unwrap();
        // Accepted by the owner, refused by the worker: the open segment has
        // already defined a source.
        ledger.set_source_keys(keys(&["a.jsonl"])).unwrap();
        let mut refused = None;
        for round in 0..64 {
            if let Err(error) =
                ledger.append_raw_owned(&source("a.jsonl"), 1, records(round, 1, 512 * 1024))
            {
                refused = Some(error.to_string());
                break;
            }
        }
        let flushed = ledger.flush().unwrap_err().to_string();
        done.send((refused, flushed)).unwrap();
    });
    let (refused, flushed) = finished
        .recv_timeout(Duration::from_secs(60))
        .expect("the owner must never wait on a queue nobody drains");
    assert!(
        refused.is_some_and(|error| error.contains("worker stopped")),
        "the owner learns the worker stopped"
    );
    assert!(
        flushed.contains("before the open segment defines"),
        "{flushed}"
    );
}
