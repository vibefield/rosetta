use std::fs::OpenOptions;
use std::io::Write;

use tempfile::{NamedTempFile, TempDir};

use super::*;

fn tailer() -> Tailer {
    let mut config = TailConfig::json_lines();
    config.max_record_bytes = 16;
    config.max_batch_bytes = 32;
    config.max_records_per_batch = 8;
    Tailer::new(config).unwrap()
}

fn batch(read: TailRead) -> TailBatch {
    match read {
        TailRead::Batch(batch) => batch,
        other => panic!("expected tail batch, got {other:?}"),
    }
}

fn payloads(items: &[TailItem]) -> Vec<Vec<u8>> {
    items
        .iter()
        .map(|item| match item {
            TailItem::Record(record) => record.payload.clone(),
            TailItem::Oversized { .. } => panic!("unexpected oversized record"),
        })
        .collect()
}

#[test]
fn soft_cap_closes_at_the_first_boundary_past_it_and_admits_big_records() {
    let mut config = TailConfig::json_lines();
    config.soft_batch_bytes = Some(10);
    let tailer = Tailer::new(config).unwrap();
    let file = NamedTempFile::new().unwrap();
    // 4 + 4 crosses the 10-byte cap only on the third record; a record
    // larger than the cap still frames alone instead of going oversized.
    std::fs::write(file.path(), b"aaaa\nbbbb\ncccc\rdddd-oversoft\neeee\n").unwrap();

    let first = batch(tailer.read(file.path(), None, false).unwrap());
    assert_eq!(payloads(&first.items).len(), 3);
    assert!(first.more_available);

    let second = batch(
        tailer
            .read(file.path(), Some(&first.checkpoint), false)
            .unwrap(),
    );
    assert_eq!(payloads(&second.items), vec![b"eeee".to_vec()]);
    assert!(!second.more_available);

    assert!(
        Tailer::new(TailConfig {
            soft_batch_bytes: Some(0),
            ..TailConfig::json_lines()
        })
        .is_err(),
        "a zero soft cap is a configuration error, not an infinite loop"
    );
}

#[test]
fn partial_final_record_is_not_emitted_or_advanced() {
    let mut file = NamedTempFile::new().unwrap();
    file.write_all(b"{\"one\":1}\n{\"two\":").unwrap();
    file.flush().unwrap();

    let first = batch(tailer().read(file.path(), None, false).unwrap());
    assert_eq!(first.transition, Transition::Initial);
    assert_eq!(first.items.len(), 1);
    assert_eq!(first.checkpoint.committed_offset, 10);
    assert_eq!(first.checkpoint.incomplete_suffix_len, 7);
    assert!(first.needs_retry);

    let mut append = OpenOptions::new().append(true).open(file.path()).unwrap();
    append.write_all(b"2}\n").unwrap();
    append.flush().unwrap();
    let second = batch(
        tailer()
            .read(file.path(), Some(&first.checkpoint), false)
            .unwrap(),
    );
    assert_eq!(second.transition, Transition::Continued);
    assert_eq!(second.items.len(), 1);
    let TailItem::Record(record) = &second.items[0] else {
        panic!("expected ordinary record");
    };
    assert_eq!(record.payload, b"{\"two\":2}");
    assert_eq!(record.start_offset, 10);
    assert_eq!(record.end_offset, 20);
    assert_eq!(second.checkpoint.committed_offset, 20);
    assert!(!second.needs_retry);
}

#[test]
fn crlf_is_normalized_after_byte_cursors_are_calculated() {
    let mut file = NamedTempFile::new().unwrap();
    file.write_all(b"one\r\ntwo\r\n").unwrap();
    file.flush().unwrap();
    let result = batch(tailer().read(file.path(), None, false).unwrap());
    assert_eq!(payloads(&result.items), [b"one".to_vec(), b"two".to_vec()]);
    assert_eq!(result.checkpoint.committed_offset, 10);
}

#[test]
fn complete_oversized_record_is_quarantined_and_cursor_advances() {
    let mut file = NamedTempFile::new().unwrap();
    file.write_all(b"0123456789abcdefX\nok\n").unwrap();
    file.flush().unwrap();
    let result = batch(tailer().read(file.path(), None, false).unwrap());
    assert!(matches!(
        result.items[0],
        TailItem::Oversized {
            payload_len: 17,
            ..
        }
    ));
    assert!(matches!(result.items[1], TailItem::Record(_)));
    assert_eq!(result.checkpoint.committed_offset, 21);
}

#[test]
fn truncate_and_rewrite_starts_a_new_generation() {
    let mut file = NamedTempFile::new().unwrap();
    file.write_all(b"first-record\n").unwrap();
    file.flush().unwrap();
    let first = batch(tailer().read(file.path(), None, false).unwrap());

    std::fs::write(file.path(), b"new\n").unwrap();
    let second = batch(
        tailer()
            .read(file.path(), Some(&first.checkpoint), false)
            .unwrap(),
    );
    assert_eq!(second.transition, Transition::Truncated);
    assert_eq!(
        second.checkpoint.generation,
        first.checkpoint.generation + 1
    );
    assert_eq!(second.items.len(), 1);
}

#[test]
fn same_size_prefix_rewrite_starts_a_new_generation() {
    let mut file = NamedTempFile::new().unwrap();
    file.write_all(b"old\n").unwrap();
    file.flush().unwrap();
    let first = batch(tailer().read(file.path(), None, false).unwrap());
    std::fs::write(file.path(), b"new\n").unwrap();

    let second = batch(
        tailer()
            .read(file.path(), Some(&first.checkpoint), false)
            .unwrap(),
    );
    assert_eq!(second.transition, Transition::PrefixMismatch);
    assert_eq!(
        second.checkpoint.generation,
        first.checkpoint.generation + 1
    );
}

#[test]
fn atomic_identity_replacement_starts_a_new_generation() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("stream.jsonl");
    std::fs::write(&path, b"old\n").unwrap();
    let first = batch(tailer().read(&path, None, false).unwrap());
    let replacement = directory.path().join("replacement");
    std::fs::write(&replacement, b"new\n").unwrap();
    std::fs::rename(replacement, &path).unwrap();

    let second = batch(
        tailer()
            .read(&path, Some(&first.checkpoint), false)
            .unwrap(),
    );
    assert_eq!(second.transition, Transition::IdentityChanged);
    assert_eq!(
        second.checkpoint.generation,
        first.checkpoint.generation + 1
    );
}

#[test]
fn forced_replay_reframes_from_zero_in_a_new_generation() {
    let mut file = NamedTempFile::new().unwrap();
    file.write_all(b"one\ntwo\n").unwrap();
    file.flush().unwrap();
    let first = batch(tailer().read(file.path(), None, false).unwrap());

    let replay = batch(
        tailer()
            .read(file.path(), Some(&first.checkpoint), true)
            .unwrap(),
    );
    assert_eq!(replay.transition, Transition::ForcedReplay);
    assert_eq!(
        replay.checkpoint.generation,
        first.checkpoint.generation + 1
    );
    assert_eq!(replay.items.len(), 2);
}

#[test]
fn batch_record_limit_preserves_next_record_boundary() {
    let mut config = TailConfig::json_lines();
    config.max_record_bytes = 8;
    config.max_batch_bytes = 8;
    config.max_records_per_batch = 1;
    let tailer = Tailer::new(config).unwrap();
    let mut file = NamedTempFile::new().unwrap();
    file.write_all(b"one\ntwo\n").unwrap();
    file.flush().unwrap();

    let first = batch(tailer.read(file.path(), None, false).unwrap());
    assert_eq!(first.items.len(), 1);
    assert_eq!(first.checkpoint.committed_offset, 4);
    assert!(first.needs_retry && first.more_available);
    let second = batch(
        tailer
            .read(file.path(), Some(&first.checkpoint), false)
            .unwrap(),
    );
    assert_eq!(second.items.len(), 1);
    assert_eq!(second.checkpoint.committed_offset, 8);
    assert!(!second.needs_retry && !second.more_available);
}

#[test]
fn bounded_read_reports_physical_bytes_and_fails_before_overrun() {
    let root = TempDir::new().unwrap();
    let path = root.path().join("stream.jsonl");
    std::fs::write(&path, b"one\ntwo\n").unwrap();
    let tailer = tailer();

    // 8 framed bytes + an 8-byte checkpoint anchor need exactly 16.
    assert!(matches!(
        tailer.read_bounded(&path, None, false, 15),
        Err(CaptureError::LimitExceeded(_))
    ));
    let first = batch(tailer.read_bounded(&path, None, false, 16).unwrap());
    assert_eq!(first.bytes_read, 16);

    let mut append = OpenOptions::new().append(true).open(&path).unwrap();
    append.write_all(b"three\n").unwrap();
    append.flush().unwrap();
    // 8 verify + 6 appended + a 14-byte anchor over the new prefix.
    let second = batch(
        tailer
            .read_bounded(&path, Some(&first.checkpoint), false, 28)
            .unwrap(),
    );
    assert_eq!(second.bytes_read, 28);
}

#[test]
fn exact_full_batch_stops_before_scanning_a_delimiter_free_tail() {
    const RECORD_PAYLOAD_BYTES: usize = 65_536;
    const FRAMING_READ_AHEAD_BYTES: usize = 65_536;
    const CHECKPOINT_ANCHOR_BYTES: usize = 4_096;
    const PHYSICAL_READ_CEILING: u64 =
        (RECORD_PAYLOAD_BYTES + FRAMING_READ_AHEAD_BYTES + CHECKPOINT_ANCHOR_BYTES) as u64;

    let root = TempDir::new().unwrap();
    let path = root.path().join("stream.jsonl");
    let mut bytes = vec![b'a'; RECORD_PAYLOAD_BYTES];
    bytes.push(b'\n');
    // The next record has no delimiter and exceeds both the logical
    // record bound and the remaining physical reservation. Once the
    // accepted first record exactly fills the batch, reading further
    // would turn a bounded prefix into a false failure.
    bytes.extend(std::iter::repeat_n(b'b', FRAMING_READ_AHEAD_BYTES * 2));
    std::fs::write(&path, bytes).unwrap();

    let tailer = Tailer::new(TailConfig {
        delimiter: b'\n',
        normalize_crlf: true,
        max_record_bytes: RECORD_PAYLOAD_BYTES,
        max_batch_bytes: RECORD_PAYLOAD_BYTES,
        max_records_per_batch: 128,
        prefix_anchor_bytes: CHECKPOINT_ANCHOR_BYTES,
        soft_batch_bytes: None,
    })
    .unwrap();
    let result = batch(
        tailer
            .read_bounded(&path, None, false, PHYSICAL_READ_CEILING)
            .unwrap(),
    );
    assert_eq!(result.items.len(), 1);
    let TailItem::Record(record) = &result.items[0] else {
        panic!("expected accepted first record");
    };
    assert_eq!(record.payload.len(), RECORD_PAYLOAD_BYTES);
    assert_eq!(
        result.checkpoint.committed_offset,
        RECORD_PAYLOAD_BYTES as u64 + 1
    );
    assert!(result.needs_retry && result.more_available);
    assert_eq!(result.bytes_read, PHYSICAL_READ_CEILING);
}

#[test]
fn checkpoint_encoding_round_trips_and_rejects_corruption() {
    let mut file = NamedTempFile::new().unwrap();
    file.write_all(b"one\npartial").unwrap();
    file.flush().unwrap();
    let result = batch(tailer().read(file.path(), None, false).unwrap());
    let checkpoint = result.checkpoint;
    assert_eq!(
        TailCheckpoint::decode(&checkpoint.encode()).unwrap(),
        checkpoint
    );
    assert_eq!(
        TailCheckpoint::decode_hex(&checkpoint.encode_hex()).unwrap(),
        checkpoint
    );

    let mut corrupt = checkpoint.encode();
    corrupt.push(1);
    assert!(TailCheckpoint::decode(&corrupt).is_err());

    let mut wrong_version = checkpoint.encode();
    wrong_version[CHECKPOINT_MAGIC.len()] = 9;
    assert!(TailCheckpoint::decode(&wrong_version).is_err());

    assert!(TailCheckpoint::decode_hex("zz").is_err());
    assert!(TailCheckpoint::decode_hex("abc").is_err());
}

#[test]
fn missing_file_is_reported_without_advancing_state() {
    let directory = TempDir::new().unwrap();
    assert_eq!(
        tailer()
            .read(&directory.path().join("missing"), None, false)
            .unwrap(),
        TailRead::Missing
    );
}

#[test]
fn invalid_bounds_are_rejected_up_front() {
    let mut zero = TailConfig::json_lines();
    zero.max_record_bytes = 0;
    assert!(matches!(
        Tailer::new(zero),
        Err(CaptureError::InvalidConfig(_))
    ));
}

#[test]
fn a_record_larger_than_a_batch_is_a_batch_of_its_own() {
    // The default captures whatever one raw ledger frame holds, while
    // batches of ordinary records keep their 32 MiB cap.
    let default = TailConfig::json_lines();
    assert_eq!(default.max_record_bytes, crate::MAX_RAW_RECORD_BYTES);
    assert_eq!(default.max_batch_bytes, 32 * 1024 * 1024);

    let mut config = TailConfig::json_lines();
    config.max_record_bytes = 16;
    config.max_batch_bytes = 8;
    let tailer = Tailer::new(config).unwrap();
    let mut file = NamedTempFile::new().unwrap();
    file.write_all(b"one\ntwo\n0123456789ab\nsix\n").unwrap();
    file.flush().unwrap();

    // The big record waits for a batch of its own, then closes it.
    let first = batch(tailer.read(file.path(), None, false).unwrap());
    assert_eq!(payloads(&first.items), [b"one".to_vec(), b"two".to_vec()]);
    assert!(first.more_available);
    let second = batch(
        tailer
            .read(file.path(), Some(&first.checkpoint), false)
            .unwrap(),
    );
    assert_eq!(payloads(&second.items), [b"0123456789ab".to_vec()]);
    assert!(second.more_available);
    let third = batch(
        tailer
            .read(file.path(), Some(&second.checkpoint), false)
            .unwrap(),
    );
    assert_eq!(payloads(&third.items), [b"six".to_vec()]);
    assert!(!third.more_available);
    assert_eq!(third.checkpoint.committed_offset, 25);
}

#[test]
fn the_committed_chain_resumes_across_batches_and_verifies() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("chain.jsonl");
    let content = b"first record here\nsecond one\nthird, longer record\n";
    std::fs::write(&path, content).unwrap();

    // Two bounded reads: the chain must fold across the boundary to the
    // same value one straight fold produces.
    let tailer = Tailer::new(TailConfig {
        max_record_bytes: 24,
        max_batch_bytes: 24,
        ..TailConfig::json_lines()
    })
    .unwrap();
    let TailRead::Batch(first) = tailer.read(&path, None, false).unwrap() else {
        panic!("expected a batch");
    };
    assert!(
        first.more_available,
        "the tiny batch bound must split reads"
    );
    let mut checkpoint = first.checkpoint;
    loop {
        let TailRead::Batch(batch) = tailer.read(&path, Some(&checkpoint), false).unwrap() else {
            panic!("expected a batch");
        };
        let done = !batch.more_available;
        checkpoint = batch.checkpoint;
        if done {
            break;
        }
    }
    assert_eq!(
        checkpoint.committed_chain,
        Some(chain_bytes(CONTENT_CHAIN_BASIS, content)),
        "the chained fold equals one straight fold over the whole file"
    );

    // Verification answers off that chain without a replay.
    assert_eq!(
        tailer.verify_committed(&path, &checkpoint).unwrap(),
        CommittedVerification::Verified
    );
    let mut rewritten = content.to_vec();
    rewritten[3] = b'z'; // same length, out of any anchor window
    std::fs::write(&path, rewritten).unwrap();
    assert_eq!(
        tailer.verify_committed(&path, &checkpoint).unwrap(),
        CommittedVerification::Mismatch
    );
}

#[test]
fn version_one_checkpoints_still_decode_and_establish_later() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("old.jsonl");
    std::fs::write(&path, b"one record\n").unwrap();
    let tailer = Tailer::new(TailConfig::json_lines()).unwrap();
    let TailRead::Batch(batch) = tailer.read(&path, None, false).unwrap() else {
        panic!("expected a batch");
    };

    // Reconstruct the version-1 wire form: strip the chain suffix and
    // stamp the old version byte.
    let mut v1 = batch.checkpoint.encode();
    match batch.checkpoint.committed_chain {
        Some(_) => v1.truncate(v1.len() - 9),
        None => v1.truncate(v1.len() - 1),
    }
    v1[CHECKPOINT_MAGIC.len()] = 1;
    let decoded = TailCheckpoint::decode(&v1).unwrap();
    assert_eq!(decoded.committed_offset, batch.checkpoint.committed_offset);
    assert_eq!(
        decoded.committed_chain, None,
        "pre-chain blobs carry no chain"
    );

    // The audit's first look at a pre-chain checkpoint establishes it.
    match tailer.verify_committed(&path, &decoded).unwrap() {
        CommittedVerification::Established(chain) => assert_eq!(
            Some(chain),
            batch.checkpoint.committed_chain,
            "the established value is the maintained one"
        ),
        other => panic!("expected Established, got {other:?}"),
    }
}
