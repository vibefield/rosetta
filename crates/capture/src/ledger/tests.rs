use tempfile::TempDir;

use super::*;

fn source(path: &str) -> SourceDef {
    SourceDef {
        ledger_only: false,
        relative_path: path.to_string(),
        stream_id: "transcript".to_string(),
        session_id: "s-canonical".to_string(),
        native_session_id: "s-native".to_string(),
        agent_version: Some("1.2.3".to_string()),
    }
}

#[test]
fn source_admission_is_backward_compatible_and_round_trips() {
    let legacy = br#"{"file_id":"0","relative_path":"a.jsonl","stream_id":"rollout","session_id":"s","native_session_id":"n","agent_version":"1"}"#;
    assert!(!decode_define(legacy).unwrap().1.ledger_only);

    let dir = TempDir::new().unwrap();
    let mut ledger = Ledger::open(dir.path()).unwrap();
    let mut internal = source("internal.jsonl");
    internal.ledger_only = true;
    ledger
        .append_raw(&internal, 1, [(0, b"private".as_slice())])
        .unwrap();
    ledger.flush().unwrap();
    drop(ledger);
    let scanned = scan(dir.path()).unwrap();
    assert_eq!(scanned.torn_segments, 0);
    assert!(matches!(&scanned.entries[0], LedgerEntry::Raw { source, .. } if source == &internal));
    assert_eq!(internal.projected_session_id(), None);
}

#[test]
fn round_trips_raw_and_meta_entries_in_order() {
    let dir = TempDir::new().unwrap();
    let mut ledger = Ledger::open(dir.path()).unwrap();
    ledger.append_meta(1, "[{\"op\":\"runtime\"}]").unwrap();
    ledger
        .append_raw(
            &source("a.jsonl"),
            1,
            [(0_u64, b"one".as_slice()), (4, b"two")],
        )
        .unwrap();
    ledger
        .append_raw(&source("b.jsonl"), 3, [(10_u64, b"three".as_slice())])
        .unwrap();
    ledger.checkpoint().unwrap();

    let scanned = scan(dir.path()).unwrap();
    assert_eq!(scanned.segments, 1);
    assert_eq!(scanned.torn_segments, 0);
    assert_eq!(scanned.entries.len(), 4);
    assert_eq!(
        scanned.entries[0],
        LedgerEntry::Meta {
            schema_version: 1,
            payload: "[{\"op\":\"runtime\"}]".to_string(),
        }
    );
    let LedgerEntry::Raw {
        source: from_scan,
        generation,
        start_offset,
        payload,
    } = &scanned.entries[2]
    else {
        panic!("expected a raw entry");
    };
    assert_eq!(from_scan, &source("a.jsonl"));
    assert_eq!((*generation, *start_offset), (1, 4));
    assert_eq!(payload, b"two");
}

#[test]
fn semantic_meta_head_skips_identical_warm_snapshots() {
    let dir = TempDir::new().unwrap();
    let mut ledger = Ledger::open(dir.path()).unwrap();
    assert!(
        ledger
            .append_meta_checkpoint_if_changed(2, "[{\"run\":1}]", "stable-a")
            .unwrap()
    );
    assert!(
        !ledger
            .append_meta_checkpoint_if_changed(2, "[{\"run\":2}]", "stable-a")
            .unwrap()
    );
    assert!(
        ledger
            .append_meta_checkpoint_if_changed(2, "[{\"run\":3}]", "stable-b")
            .unwrap()
    );

    let meta = scan(dir.path())
        .unwrap()
        .entries
        .into_iter()
        .filter_map(|entry| match entry {
            LedgerEntry::Meta { payload, .. } => Some(payload),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(meta, ["[{\"run\":1}]", "[{\"run\":3}]"]);
}

#[test]
fn write_stats_split_frames_flush_and_sync_without_changing_the_ledger() {
    let dir = TempDir::new().unwrap();
    let mut ledger = Ledger::open(dir.path()).unwrap();
    ledger
        .append_raw(
            &source("a.jsonl"),
            1,
            [(0_u64, b"one".as_slice()), (4, b"two")],
        )
        .unwrap();
    ledger.checkpoint().unwrap();

    let stats = ledger.take_write_stats();
    assert_eq!(stats.frames, 3, "one define plus two raw frames");
    assert!(stats.frame_bytes > 6);
    assert_eq!(stats.flushes, 1);
    assert_eq!(stats.syncs, 1, "default checkpoints stay live-durable");
    assert_eq!(stats.directory_syncs, 1);
    assert_eq!(stats.segments_opened, 1);
    assert_eq!(ledger.take_write_stats(), LedgerWriteStats::default());

    let scanned = scan(dir.path()).unwrap();
    assert_eq!(scanned.entries.len(), 2);
    assert_eq!(scanned.torn_segments, 0);
}

#[test]
fn asynchronous_owned_writer_preserves_order_and_joins_at_flush() {
    let dir = TempDir::new().unwrap();
    let source = source("async.jsonl");
    let mut ledger = Ledger::open(dir.path())
        .unwrap()
        .defer_sync_until_flush()
        .into_async_bounded(2);
    ledger.append_meta(2, "[]").unwrap();
    ledger.checkpoint().unwrap();
    ledger
        .append_raw_owned(&source, 1, vec![(0, b"one".to_vec()), (4, b"two".to_vec())])
        .unwrap();
    ledger.complete_generation(&source, 1, 7, true).unwrap();
    ledger.checkpoint().unwrap();
    ledger.flush().unwrap();

    let stats = ledger.take_write_stats();
    assert_eq!(stats.syncs, 1);
    let scanned = scan(dir.path()).unwrap();
    assert_eq!(scanned.torn_segments, 0);
    assert!(matches!(scanned.entries[0], LedgerEntry::Meta { .. }));
    assert!(matches!(
        scanned.entries.last(),
        Some(LedgerEntry::GenerationComplete { generation: 1, .. })
    ));
    let payloads = scanned
        .entries
        .iter()
        .filter_map(|entry| match entry {
            LedgerEntry::Raw { payload, .. } => Some(payload.as_slice()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(payloads, [b"one".as_slice(), b"two".as_slice()]);
}

#[test]
fn owned_batch_frames_match_individual_records_across_sizes_and_rotations() {
    let records = vec![
        (0, Vec::new()),
        (1, b"first".to_vec()),
        (7, vec![b'a'; 1024 * 1024 - 48]),
        (1_100_000, vec![b'b'; 1024 * 1024 + 1]),
        (2_200_000, b"last\r\n\0".to_vec()),
        (2_200_010, Vec::new()),
    ];
    for codec in [SegmentCodec::Plain, SegmentCodec::default()] {
        let borrowed_dir = TempDir::new().unwrap();
        let owned_dir = TempDir::new().unwrap();
        let mut borrowed = Ledger::open(borrowed_dir.path())
            .unwrap()
            .with_codec(codec)
            .with_max_segment_bytes(512 * 1024);
        let mut owned = Ledger::open(owned_dir.path())
            .unwrap()
            .with_codec(codec)
            .with_max_segment_bytes(512 * 1024);
        for generation in [1, 2] {
            let source = source(&format!("source-{generation}.jsonl"));
            borrowed
                .append_raw(&source, generation, records.iter().map(|(o, p)| (*o, p)))
                .unwrap();
            owned
                .append_raw_owned(&source, generation, records.clone())
                .unwrap();
            for ledger in [&mut borrowed, &mut owned] {
                ledger
                    .complete_generation(&source, generation, 7, true)
                    .unwrap();
                ledger.checkpoint().unwrap();
            }
        }
        assert!(owned.take_write_stats().frames < borrowed.take_write_stats().frames);
        let expected = scan(borrowed_dir.path()).unwrap();
        let actual = scan(owned_dir.path()).unwrap();
        assert_eq!(actual.torn_segments, 0);
        assert_eq!(actual.segments, expected.segments);
        assert_eq!(actual.entries, expected.entries);
    }
}

#[test]
fn a_torn_or_corrupt_batch_exposes_none_of_its_records() {
    let dir = TempDir::new().unwrap();
    let source = source("batch.jsonl");
    let mut ledger = Ledger::open(dir.path())
        .unwrap()
        .with_codec(SegmentCodec::Plain);
    ledger
        .append_raw(&source, 1, [(0, b"durable prefix")])
        .unwrap();
    ledger.checkpoint().unwrap();
    let segment = segment_files(dir.path()).unwrap().pop().unwrap().1;
    let prefix_len = std::fs::metadata(&segment).unwrap().len() as usize;
    ledger
        .append_raw_owned(
            &source,
            1,
            vec![(15, b"one".to_vec()), (19, b"two".to_vec())],
        )
        .unwrap();
    ledger.checkpoint().unwrap();
    drop(ledger);
    let bytes = std::fs::read(&segment).unwrap();
    assert_eq!(bytes[prefix_len], TAG_RAW_BATCH);
    for cut in [prefix_len + 1, prefix_len + 24, bytes.len() - 1] {
        std::fs::write(&segment, &bytes[..cut]).unwrap();
        let result = scan(dir.path()).unwrap();
        assert_eq!(result.torn_segments, 1);
        assert!(
            matches!(result.entries.as_slice(), [LedgerEntry::Raw { payload, .. }]
            if payload == b"durable prefix")
        );
    }
    let mut corrupt = bytes;
    corrupt[prefix_len + 5 + 16 + 12] ^= 0x80;
    std::fs::write(&segment, corrupt).unwrap();
    let result = scan(dir.path()).unwrap();
    assert_eq!(result.torn_segments, 1);
    assert!(
        matches!(result.entries.as_slice(), [LedgerEntry::Raw { payload, .. }]
        if payload == b"durable prefix")
    );
}

#[test]
fn private_async_checkpoints_coalesce_between_metadata_and_source_seals() {
    let dir = TempDir::new().unwrap();
    let source = source("coalesced.jsonl");
    let mut ledger = Ledger::open(dir.path())
        .unwrap()
        .defer_sync_until_flush()
        .into_async_bounded(1);
    ledger.append_meta(2, "[]").unwrap();
    ledger.checkpoint().unwrap();
    for generation in [1, 2] {
        for offset in 0..8 {
            ledger
                .append_raw(&source, generation, [(offset, b"record")])
                .unwrap();
            ledger.checkpoint().unwrap();
        }
        if generation == 1 {
            ledger.complete_generation(&source, 1, 7, true).unwrap();
            ledger.checkpoint().unwrap();
        }
    }
    ledger.flush().unwrap();
    let stats = ledger.take_write_stats();
    assert_eq!(
        stats.flushes, 3,
        "metadata, sealed source, final durability barrier"
    );
    assert_eq!(stats.syncs, 1, "all private bytes sync before publication");
    let result = scan(dir.path()).unwrap();
    assert_eq!(result.torn_segments, 0);
    assert_eq!(result.entries.len(), 18);
    assert!(matches!(
        result.entries.first(),
        Some(LedgerEntry::Meta { .. })
    ));
    assert_eq!(
        result
            .entries
            .iter()
            .filter(|e| matches!(e, LedgerEntry::GenerationComplete { .. }))
            .count(),
        1,
        "the unsealed second generation remains raw-only recovery evidence"
    );
}

#[test]
fn async_checkpoint_coalescing_still_rotates_long_unsealed_sources() {
    let dir = TempDir::new().unwrap();
    let source = source("long.jsonl");
    let mut ledger = Ledger::open(dir.path())
        .unwrap()
        .with_max_segment_bytes(1024)
        .defer_sync_until_flush()
        .into_async_bounded(1);
    for offset in 0..32 {
        ledger
            .append_raw_owned(&source, 1, vec![(offset, vec![b'x'; 128])])
            .unwrap();
        ledger.checkpoint().unwrap();
    }
    ledger.flush().unwrap();
    let stats = ledger.take_write_stats();
    let result = scan(dir.path()).unwrap();
    assert_eq!(result.torn_segments, 0);
    assert_eq!(result.entries.len(), 32);
    assert!(
        result.segments > 1,
        "rotation must not wait for a source seal"
    );
    assert_eq!(stats.syncs, result.segments as u64);
    assert!(stats.flushes < 32);
}

#[test]
fn async_writers_without_deferred_sync_keep_every_durable_checkpoint() {
    let dir = TempDir::new().unwrap();
    let source = source("durable.jsonl");
    let mut ledger = Ledger::open(dir.path()).unwrap().into_async_bounded(1);
    for offset in 0..8 {
        ledger
            .append_raw(&source, 1, [(offset, b"record")])
            .unwrap();
        ledger.checkpoint().unwrap();
    }
    ledger.flush().unwrap();
    let stats = ledger.take_write_stats();
    assert_eq!(stats.flushes, 9);
    assert_eq!(stats.syncs, 9);
    assert_eq!(scan(dir.path()).unwrap().entries.len(), 8);
}

#[test]
fn deferred_checkpoints_sync_every_rotated_segment_at_the_final_barrier() {
    let dir = TempDir::new().unwrap();
    let mut ledger = Ledger::open(dir.path())
        .unwrap()
        .with_max_segment_bytes(64)
        .defer_sync_until_flush();
    for index in 0..8_u64 {
        ledger
            .append_raw(
                &source("a.jsonl"),
                1,
                [(index * 8, format!("record-{index}").into_bytes())],
            )
            .unwrap();
        ledger.checkpoint().unwrap();
    }

    let checkpoints = ledger.take_write_stats();
    assert_eq!(checkpoints.flushes, 8);
    assert_eq!(checkpoints.syncs, 0);
    assert_eq!(checkpoints.directory_syncs, 0);
    assert_eq!(checkpoints.segments_opened, 8);

    ledger.flush().unwrap();
    let barrier = ledger.take_write_stats();
    assert_eq!(barrier.syncs, 8);
    assert_eq!(barrier.directory_syncs, 1);
    assert_eq!(barrier.flushes, 0, "every segment already rotated");

    let scanned = scan(dir.path()).unwrap();
    assert_eq!(scanned.segments, 8);
    assert_eq!(scanned.torn_segments, 0);
    assert_eq!(
        scanned
            .entries
            .iter()
            .filter(|entry| matches!(entry, LedgerEntry::Raw { .. }))
            .count(),
        8
    );
}

#[test]
fn first_barrier_syncs_new_directory_tree_child_first() {
    let temp = TempDir::new().unwrap();
    let dir = temp.path().join("ledger").join("runtime");
    let mut ledger = Ledger::open(&dir).unwrap();
    ledger
        .append_raw(&source("a.jsonl"), 1, [(0_u64, b"one".as_slice())])
        .unwrap();
    ledger.checkpoint().unwrap();

    let stats = ledger.take_write_stats();
    assert_eq!(stats.syncs, 1);
    assert_eq!(
        stats.directory_syncs, 3,
        "runtime, ledger root, and the pre-existing parent need barriers"
    );
}

#[test]
fn generation_seals_carry_normalizer_outcome_and_source_identity() {
    let dir = TempDir::new().unwrap();
    let mut ledger = Ledger::open(dir.path()).unwrap();
    let source = source("a.jsonl");
    ledger
        .append_raw(&source, 7, [(0_u64, b"record".as_slice())])
        .unwrap();
    ledger.complete_generation(&source, 7, 4, false).unwrap();
    ledger.flush().unwrap();

    let scanned = scan(dir.path()).unwrap();
    assert!(matches!(
        &scanned.entries[1],
        LedgerEntry::GenerationComplete {
            source: sealed,
            generation: 7,
            normalizer_version: 4,
            normalized_clean: false,
        } if sealed == &source
    ));
}

#[test]
fn same_path_with_new_source_metadata_gets_a_fresh_definition() {
    let dir = TempDir::new().unwrap();
    let mut ledger = Ledger::open(dir.path()).unwrap();
    let old = source("a.jsonl");
    let mut new = old.clone();
    new.agent_version = Some("2.0.0".to_string());
    ledger
        .append_raw(&old, 1, [(0_u64, b"old".as_slice())])
        .unwrap();
    ledger
        .append_raw(&new, 1, [(4_u64, b"new".as_slice())])
        .unwrap();
    ledger.flush().unwrap();

    let scanned = scan(dir.path()).unwrap();
    let sources: Vec<_> = scanned
        .entries
        .iter()
        .filter_map(|entry| match entry {
            LedgerEntry::Raw { source, .. } => Some(source.agent_version.as_deref()),
            _ => None,
        })
        .collect();
    assert_eq!(sources, [Some("1.2.3"), Some("2.0.0")]);
}

#[test]
fn reopening_continues_in_a_new_segment_and_redefines_sources() {
    let dir = TempDir::new().unwrap();
    let mut ledger = Ledger::open(dir.path()).unwrap();
    ledger
        .append_raw(&source("a.jsonl"), 1, [(0_u64, b"first".as_slice())])
        .unwrap();
    ledger.flush().unwrap();
    drop(ledger);

    let mut ledger = Ledger::open(dir.path()).unwrap();
    ledger
        .append_raw(&source("a.jsonl"), 1, [(6_u64, b"second".as_slice())])
        .unwrap();
    ledger.flush().unwrap();

    let scanned = scan(dir.path()).unwrap();
    assert_eq!(scanned.segments, 2);
    assert_eq!(scanned.torn_segments, 0);
    let raw: Vec<&[u8]> = scanned
        .entries
        .iter()
        .filter_map(|entry| match entry {
            LedgerEntry::Raw { payload, .. } => Some(payload.as_slice()),
            LedgerEntry::Meta { .. } | LedgerEntry::GenerationComplete { .. } => None,
        })
        .collect();
    assert_eq!(raw, [b"first".as_slice(), b"second"]);
}

#[test]
fn rotation_by_size_keeps_segments_self_contained() {
    let dir = TempDir::new().unwrap();
    let mut ledger = Ledger::open(dir.path()).unwrap().with_max_segment_bytes(64);
    for index in 0..8_u64 {
        ledger
            .append_raw(
                &source("a.jsonl"),
                1,
                [(index * 8, format!("record-{index}").into_bytes())],
            )
            .unwrap();
        ledger.flush().unwrap();
    }

    let scanned = scan(dir.path()).unwrap();
    assert!(scanned.segments > 1, "64-byte cap must force rotation");
    assert_eq!(scanned.torn_segments, 0);
    let raw_count = scanned
        .entries
        .iter()
        .filter(|entry| matches!(entry, LedgerEntry::Raw { .. }))
        .count();
    assert_eq!(raw_count, 8, "rotation must not lose or split records");
}

#[test]
fn a_torn_tail_is_harmless_and_prior_entries_survive() {
    let dir = TempDir::new().unwrap();
    let mut ledger = Ledger::open(dir.path())
        .unwrap()
        .with_codec(SegmentCodec::Plain);
    ledger
        .append_raw(&source("a.jsonl"), 1, [(0_u64, b"intact".as_slice())])
        .unwrap();
    ledger.flush().unwrap();
    drop(ledger);

    // Simulate a crash mid-append: garbage after the durable point.
    let segment = dir.path().join(segment_name(0));
    let mut file = OpenOptions::new().append(true).open(&segment).unwrap();
    file.write_all(&[TAG_RAW, 0, 0, 1]).unwrap(); // truncated header

    let scanned = scan(dir.path()).unwrap();
    assert_eq!(scanned.torn_segments, 1);
    assert_eq!(scanned.entries.len(), 1); // the intact record (defines resolve, they don't surface)

    // A checksum flip inside a frame also just stops the segment.
    let mut bytes = std::fs::read(&segment).unwrap();
    let flip_at = MAGIC_LEN + 7;
    bytes[flip_at] ^= 0xFF;
    std::fs::write(&segment, &bytes).unwrap();
    let scanned = scan(dir.path()).unwrap();
    assert_eq!(scanned.torn_segments, 1);
    assert!(scanned.entries.is_empty());
}

#[test]
fn define_encoding_survives_hostile_strings() {
    let dir = TempDir::new().unwrap();
    let mut ledger = Ledger::open(dir.path()).unwrap();
    let hostile = SourceDef {
        ledger_only: false,
        relative_path: "we\"ird\\path\n.jsonl".to_string(),
        stream_id: "transcript".to_string(),
        session_id: "s".to_string(),
        native_session_id: "n".to_string(),
        agent_version: None,
    };
    ledger
        .append_raw(&hostile, 1, [(0_u64, b"x".as_slice())])
        .unwrap();
    ledger.flush().unwrap();

    let scanned = scan(dir.path()).unwrap();
    let LedgerEntry::Raw { source, .. } = &scanned.entries[0] else {
        panic!("expected raw");
    };
    assert_eq!(source, &hostile);
}
