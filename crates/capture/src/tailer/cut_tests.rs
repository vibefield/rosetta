//! [`Tailer::read_cut`]: a logical end of file for the reads of the content
//! it was taken over (initial, continued, forced replay), and none for a
//! rewrite's new content.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

use tempfile::TempDir;

use super::*;

fn tailer() -> Tailer {
    Tailer::new(TailConfig::json_lines()).unwrap()
}

fn batch(read: TailRead) -> TailBatch {
    match read {
        TailRead::Batch(batch) => batch,
        other => panic!("expected tail batch, got {other:?}"),
    }
}

fn record(index: usize) -> String {
    format!("{{\"record\":{index},\"text\":\"line {index}\"}}\n")
}

/// Records `from..=to`, one per line.
fn records(from: usize, to: usize) -> String {
    (from..=to).map(record).collect()
}

/// The offset one past record `index` of a file holding records `1..`.
fn end_of(index: usize) -> u64 {
    records(1, index).len() as u64
}

fn payload_indices(items: &[TailItem]) -> Vec<usize> {
    items
        .iter()
        .map(|item| match item {
            TailItem::Record(record) => {
                let value: serde_json::Value = serde_json::from_slice(&record.payload).unwrap();
                value["record"].as_u64().unwrap() as usize
            }
            TailItem::Oversized { .. } => panic!("unexpected oversized record"),
        })
        .collect()
}

fn append(path: &Path, text: &str) {
    let mut file = OpenOptions::new().append(true).open(path).unwrap();
    file.write_all(text.as_bytes()).unwrap();
    file.flush().unwrap();
}

#[test]
fn read_cut_stops_at_the_cut() {
    let root = TempDir::new().unwrap();
    let path = root.path().join("cut.jsonl");
    std::fs::write(&path, records(1, 10)).unwrap();
    let tailer = tailer();

    // A cut inside record 7: records 1–6 frame, and the part of record 7
    // before the cut is an unfinished suffix, as if the file ended there.
    let cut = end_of(6) + 5;
    let first = batch(tailer.read_cut(&path, None, false, Some(cut)).unwrap());
    assert_eq!(first.transition, Transition::Initial);
    assert_eq!(payload_indices(&first.items), (1..=6).collect::<Vec<_>>());
    assert_eq!(first.checkpoint.committed_offset, end_of(6));
    assert_eq!(first.checkpoint.observed_len, cut);
    assert_eq!(first.snapshot_len, cut);
    assert_eq!(first.checkpoint.incomplete_suffix_len, cut - end_of(6));
    assert!(first.cut_applied && !first.more_available && first.needs_retry);

    // Growth past the cut is no reason to retry or to read on: the same cut
    // continues to frame nothing more.
    append(&path, &records(11, 12));
    let again = batch(
        tailer
            .read_cut(&path, Some(&first.checkpoint), false, Some(cut))
            .unwrap(),
    );
    assert_eq!(again.transition, Transition::Continued);
    assert!(again.items.is_empty());
    assert_eq!(again.checkpoint, first.checkpoint);

    // The cut's checkpoint is ordinary append state: an uncut read lands the
    // rest exactly once.
    let rest = batch(tailer.read(&path, Some(&first.checkpoint), false).unwrap());
    assert_eq!(rest.transition, Transition::Continued);
    assert_eq!(payload_indices(&rest.items), (7..=12).collect::<Vec<_>>());
    assert!(!rest.cut_applied);

    // A cut on a record boundary leaves nothing unfinished.
    let boundary = batch(
        tailer
            .read_cut(&path, None, false, Some(end_of(6)))
            .unwrap(),
    );
    assert_eq!(
        payload_indices(&boundary.items),
        (1..=6).collect::<Vec<_>>()
    );
    assert_eq!(boundary.checkpoint.observed_len, end_of(6));
    assert_eq!(boundary.checkpoint.incomplete_suffix_len, 0);
    assert!(boundary.cut_applied && !boundary.needs_retry);

    // A cut at or past the end binds nothing.
    let whole = batch(
        tailer
            .read_cut(&path, None, false, Some(end_of(12)))
            .unwrap(),
    );
    assert_eq!(payload_indices(&whole.items), (1..=12).collect::<Vec<_>>());
    assert!(!whole.cut_applied);
    let unbounded = batch(tailer.read_cut(&path, None, false, None).unwrap());
    assert_eq!(unbounded.items, whole.items);
    assert!(!unbounded.cut_applied);
}

#[test]
fn read_cut_reads_a_rewrite_to_its_end() {
    let root = TempDir::new().unwrap();
    let path = root.path().join("rewrite.jsonl");
    std::fs::write(&path, records(1, 4)).unwrap();
    let tailer = tailer();
    let cut = end_of(4);
    let first = batch(tailer.read_cut(&path, None, false, Some(cut)).unwrap());
    assert_eq!(first.checkpoint.committed_offset, cut);

    // Atomic replacement by a longer file: a new generation from byte zero,
    // read to its own end — the old cut says nothing about new content.
    let replacement = root.path().join("replacement.jsonl");
    std::fs::write(&replacement, records(21, 30)).unwrap();
    std::fs::rename(&replacement, &path).unwrap();
    let replaced = batch(
        tailer
            .read_cut(&path, Some(&first.checkpoint), false, Some(cut))
            .unwrap(),
    );
    assert_eq!(replaced.transition, Transition::IdentityChanged);
    assert_eq!(
        payload_indices(&replaced.items),
        (21..=30).collect::<Vec<_>>()
    );
    assert_eq!(
        replaced.checkpoint.observed_len,
        std::fs::metadata(&path).unwrap().len()
    );
    assert!(!replaced.cut_applied);

    // An in-place rewrite of the committed prefix, grown past the cut: the
    // same, through the anchor.
    let second = batch(tailer.read_cut(&path, None, false, Some(cut)).unwrap());
    std::fs::write(&path, records(41, 50)).unwrap();
    let rewritten = batch(
        tailer
            .read_cut(&path, Some(&second.checkpoint), false, Some(cut))
            .unwrap(),
    );
    assert_eq!(rewritten.transition, Transition::PrefixMismatch);
    assert_eq!(
        payload_indices(&rewritten.items),
        (41..=50).collect::<Vec<_>>()
    );
    assert!(!rewritten.cut_applied);
}

#[test]
fn forced_replay_keeps_the_cut() {
    let root = TempDir::new().unwrap();
    let path = root.path().join("replay.jsonl");
    std::fs::write(&path, records(1, 5)).unwrap();
    let tailer = tailer();
    let cut = end_of(5);
    let first = batch(tailer.read_cut(&path, None, false, Some(cut)).unwrap());
    append(&path, &records(6, 9));

    let replay = batch(
        tailer
            .read_cut(&path, Some(&first.checkpoint), true, Some(cut))
            .unwrap(),
    );
    assert_eq!(replay.transition, Transition::ForcedReplay);
    assert_eq!(
        replay.checkpoint.generation,
        first.checkpoint.generation + 1
    );
    assert_eq!(payload_indices(&replay.items), (1..=5).collect::<Vec<_>>());
    assert_eq!(replay.checkpoint.observed_len, cut);
    assert!(replay.cut_applied);
}

/// A cut below where a continuation resumes never rewinds it: the read
/// frames nothing and its checkpoint stays consistent.
#[test]
fn a_cut_below_the_resume_point_frames_nothing() {
    let root = TempDir::new().unwrap();
    let path = root.path().join("resume.jsonl");
    std::fs::write(&path, records(1, 6)).unwrap();
    let tailer = tailer();
    let first = batch(tailer.read(&path, None, false).unwrap());
    append(&path, &records(7, 8));

    let below = batch(
        tailer
            .read_cut(&path, Some(&first.checkpoint), false, Some(end_of(2)))
            .unwrap(),
    );
    assert_eq!(below.transition, Transition::Continued);
    assert!(below.items.is_empty());
    assert_eq!(below.checkpoint.committed_offset, end_of(6));
    assert_eq!(below.checkpoint.observed_len, end_of(6));
    assert!(below.cut_applied);
}
