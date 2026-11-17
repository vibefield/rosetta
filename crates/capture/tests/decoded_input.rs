use rosetta_capture::input::{InputCandidate, InputEncoding, InputSource};
use rosetta_capture::{TailConfig, TailRead, Tailer, Transition};
use std::io::{Read, Write};
use tempfile::TempDir;

fn source() -> InputSource {
    InputSource::single(vec![
        InputCandidate {
            path: "source.jsonl".into(),
            encoding: InputEncoding::Plain,
        },
        InputCandidate {
            path: "source.jsonl.zst".into(),
            encoding: InputEncoding::Zstd,
        },
    ])
}
fn batch(read: TailRead) -> rosetta_capture::TailBatch {
    match read {
        TailRead::Batch(b) => b,
        other => panic!("{other:?}"),
    }
}

#[test]
fn decoded_offsets_and_generation_survive_encoding_changes() {
    let dir = TempDir::new().unwrap();
    let plan = source();
    let bytes = b"{\"a\":1}\r\n{\"b\":2}\nunfinished";
    std::fs::write(
        dir.path().join("source.jsonl.zst"),
        zstd::encode_all(&bytes[..], 3).unwrap(),
    )
    .unwrap();
    let tailer = Tailer::new(TailConfig::json_lines()).unwrap();
    let mut cold = plan.open(dir.path(), || false).unwrap().unwrap();
    let first = batch(tailer.read_input(&mut cold, None, false, None).unwrap());
    assert_eq!(first.checkpoint.committed_offset, 17);
    assert_eq!(first.checkpoint.incomplete_suffix_len, 10);
    std::fs::write(dir.path().join("source.jsonl"), bytes).unwrap();
    let mut hot = plan.open(dir.path(), || false).unwrap().unwrap();
    let resumed = batch(
        tailer
            .read_input(&mut hot, Some(&first.checkpoint), false, None)
            .unwrap(),
    );
    assert_eq!(resumed.transition, Transition::Continued);
    assert!(resumed.items.is_empty());
    assert_eq!(resumed.checkpoint.generation, first.checkpoint.generation);
    std::fs::OpenOptions::new()
        .append(true)
        .open(dir.path().join("source.jsonl"))
        .unwrap()
        .write_all(b"\n")
        .unwrap();
    let appended = batch(
        tailer
            .read_input(&mut hot, Some(&resumed.checkpoint), false, None)
            .unwrap(),
    );
    assert_eq!(appended.items.len(), 1);
    assert_eq!(appended.checkpoint.committed_offset, bytes.len() as u64 + 1);
}

#[test]
fn open_handle_survives_unlink_without_switching_sources_mid_pass() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("source.jsonl"), b"first\nsecond\n").unwrap();
    let mut opened = source().open(dir.path(), || false).unwrap().unwrap();
    std::fs::remove_file(dir.path().join("source.jsonl")).unwrap();
    std::fs::write(
        dir.path().join("source.jsonl.zst"),
        zstd::encode_all(&b"other\n"[..], 3).unwrap(),
    )
    .unwrap();
    let tailer = Tailer::new(TailConfig {
        max_records_per_batch: 1,
        ..TailConfig::json_lines()
    })
    .unwrap();
    let first = batch(tailer.read_input(&mut opened, None, false, None).unwrap());
    let second = batch(
        tailer
            .read_input(&mut opened, Some(&first.checkpoint), false, None)
            .unwrap(),
    );
    assert_eq!(second.transition, Transition::Continued);
    assert_eq!(second.checkpoint.committed_offset, 13);
}

#[test]
fn replacement_checks_the_entire_committed_prefix() {
    let dir = TempDir::new().unwrap();
    let mut bytes = b"original\n".to_vec();
    bytes.extend(vec![b'x'; 9000]);
    bytes.push(b'\n');
    std::fs::write(dir.path().join("source.jsonl"), &bytes).unwrap();
    let tailer = Tailer::new(TailConfig::json_lines()).unwrap();
    let mut opened = source().open(dir.path(), || false).unwrap().unwrap();
    let first = batch(tailer.read_input(&mut opened, None, false, None).unwrap());
    bytes[..8].copy_from_slice(b"replaced");
    std::fs::write(
        dir.path().join("source.jsonl.zst"),
        zstd::encode_all(&bytes[..], 3).unwrap(),
    )
    .unwrap();
    std::fs::remove_file(dir.path().join("source.jsonl")).unwrap();
    let mut changed = source().open(dir.path(), || false).unwrap().unwrap();
    let next = batch(
        tailer
            .read_input(&mut changed, Some(&first.checkpoint), false, None)
            .unwrap(),
    );
    assert!(next.transition.starts_new_generation());
    assert_eq!(next.checkpoint.generation, first.checkpoint.generation + 1);
}

#[test]
fn unknown_size_multiframe_and_checksum_failures() {
    let dir = TempDir::new().unwrap();
    // encode_all uses an unknown-size streaming encoder.
    let first = zstd::encode_all(&b"one\n"[..], 3).unwrap();
    let second = zstd::encode_all(&b"two\n"[..], 3).unwrap();
    let both = [first.clone(), second].concat();
    std::fs::write(dir.path().join("source.jsonl.zst"), &both).unwrap();
    let mut opened = source().open(dir.path(), || false).unwrap().unwrap();
    let mut decoded = String::new();
    opened.file.read_to_string(&mut decoded).unwrap();
    assert_eq!(decoded, "one\ntwo\n");
    std::fs::write(dir.path().join("source.jsonl.zst"), &both[..both.len() - 2]).unwrap();
    assert!(source().open(dir.path(), || false).is_err());
    std::fs::write(dir.path().join("source.jsonl.zst"), first).unwrap();
    assert!(source().open(dir.path(), || true).is_err());
}

#[test]
fn plain_precedence_never_hides_corrupt_preferred_content() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("source.jsonl"), b"plain\n").unwrap();
    std::fs::write(dir.path().join("source.jsonl.zst"), b"corrupt").unwrap();
    let before = source().stamp(dir.path()).unwrap();
    let mut opened = source().open(dir.path(), || false).unwrap().unwrap();
    let mut decoded = String::new();
    opened.file.read_to_string(&mut decoded).unwrap();
    assert_eq!(decoded, "plain\n");
    assert_eq!(before, source().stamp(dir.path()).unwrap());
    std::fs::remove_file(dir.path().join("source.jsonl")).unwrap();
    assert!(source().open(dir.path(), || false).is_err());
}

#[test]
fn a_chain_less_legacy_checkpoint_cannot_adopt_a_new_representation() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("source.jsonl"), b"one\ntwo\n").unwrap();
    let tailer = Tailer::new(TailConfig::json_lines()).unwrap();
    let mut input = source().open(dir.path(), || false).unwrap().unwrap();
    let mut previous = batch(tailer.read_input(&mut input, None, false, None).unwrap()).checkpoint;
    previous.committed_chain = None;
    std::fs::write(
        dir.path().join("source.jsonl.zst"),
        zstd::encode_all(&b"one\ntwo\n"[..], 3).unwrap(),
    )
    .unwrap();
    std::fs::remove_file(dir.path().join("source.jsonl")).unwrap();
    let mut input = source().open(dir.path(), || false).unwrap().unwrap();
    assert_eq!(
        tailer.verify_input(&mut input, &previous).unwrap(),
        rosetta_capture::CommittedVerification::IdentityChanged
    );
    let next = batch(
        tailer
            .read_input(&mut input, Some(&previous), false, None)
            .unwrap(),
    );
    assert!(next.transition.starts_new_generation());
}
