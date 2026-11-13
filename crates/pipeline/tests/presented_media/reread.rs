//! A pass that re-reads records the ledger already holds asks the ledger
//! before the filesystem: a re-frame after a rebuild, one that stopped
//! between batches, and a replacement generation (the first test moved from
//! `presented_media.rs`, which keeps the shared fixture).

use super::*;

#[test]
fn a_pass_that_re_reads_a_source_asks_the_ledger_before_the_filesystem() {
    let mut f = Fixture::new();
    let video = mp4(4096, 7);
    let clip = f.media("clip.mp4", &video);
    // A long record a later rewrite drops, so the rewrite is shorter.
    f.append(json!({"timestamp":"2026-10-01T00:00:01Z","type":"response_item","payload":{"type":"message","id":"filler","role":"assistant","content":[{"type":"output_text","text":"filler ".repeat(400)}]}}));
    f.open_call("send-1");
    let offset = f.send("send-1", "result-1", &[(&clip, 4096, "video/mp4")]);
    f.ingest();
    let session = session_id(&f.runtime);
    let shown = attachments(&f.store, &session);
    assert_eq!(asset_digest(&shown[0]), Some(digest(&video).as_str()));
    let details = tool_details(&f.store, &session);
    assert_eq!(media_frames(&f.ledger_dir).len(), 1);

    // A rebuilt projection's cursors carry no tail, so the next pass
    // re-reads each transcript from byte zero in the same generation.
    for today in [None, Some(mp4(4096, 8))] {
        match &today {
            // The file is gone,
            None => std::fs::remove_file(&clip).unwrap(),
            // or it has the recorded size and other bytes.
            Some(bytes) => std::fs::write(&clip, bytes).unwrap(),
        }
        let name = if today.is_some() {
            "changed"
        } else {
            "deleted"
        };
        let (mut rebuilt, rebuilt_db) = f.rebuild(name, RebuildOptions::default());
        assert_eq!(tool_details(&rebuilt, &session), details);
        ingest(&f.runtime, &f.root, &mut rebuilt, &mut f.ledger, &options());
        assert_eq!(tool_details(&rebuilt, &session), details, "{name}");
        assert_eq!(attachments(&rebuilt, &session), shown, "{name}");
        assert_eq!(
            members(&rebuilt_db),
            BTreeMap::from([(format!("{}.mp4", digest(&video)), video.clone())]),
            "{name}"
        );
        let frames = media_frames(&f.ledger_dir);
        assert_eq!(frames.len(), 1, "{name}: the capture is not ledgered twice");
        assert_eq!((frames[0].0, frames[0].1), (1, offset));
    }

    // A replacement generation: the transcript is rewritten shorter, the
    // record that sent the file moves, and the file is gone.
    std::fs::remove_file(&clip).unwrap();
    std::fs::write(&f.transcript, format!("{}\n", header(f.temp.path()))).unwrap();
    f.open_call("send-1");
    let moved = f.send("send-1", "result-1", &[(&clip, 4096, "video/mp4")]);
    assert_ne!(moved, offset);
    f.ingest();
    let replaced = attachments(&f.store, &session);
    assert_eq!(replaced.len(), 1);
    assert_eq!(
        asset_digest(&replaced[0]),
        Some(digest(&video).as_str()),
        "the capture is carried into the new generation"
    );
    let frames = media_frames(&f.ledger_dir);
    let [first, carried] = &frames[..] else {
        panic!("{frames:?}")
    };
    assert_eq!((first.0, first.1), (1, offset));
    assert_eq!(
        (carried.0, carried.1, carried.2.status),
        (2, moved, Status::Captured)
    );
    assert_eq!(carried.2.recovered_from, Some(format!("ledger:1:{offset}")));
    assert_eq!(carried.2.captured_at, first.2.captured_at);
    assert_eq!(carried.2.bytes, video);

    // The replacement replays from the ledger like any other generation.
    let (rebuilt, rebuilt_db) = f.rebuild("replaced", RebuildOptions::default());
    assert_eq!(attachments(&rebuilt, &session), replaced);
    assert_eq!(
        tool_details(&rebuilt, &session),
        tool_details(&f.store, &session)
    );
    assert_eq!(members(&rebuilt_db).len(), 1);
}

/// Sets a pass's cancel flag as soon as the pass has framed one batch, so
/// that batch is ledgered and committed and the next one is never read.
#[derive(Debug)]
struct CancelAfterFirstBatch(Arc<AtomicBool>);

impl ColdProgressObserver for CancelAfterFirstBatch {
    fn source_bytes(&self, _file: &SessionFile, _bytes: u64) {
        self.0.store(true, Ordering::Relaxed);
    }

    fn reading_finished(&self) {}
}

/// The transcript's stored checkpoint.
fn checkpoint(f: &Fixture, store: &Store) -> PipeCheckpoint {
    let path = f.transcript.strip_prefix(&f.root).unwrap();
    let cursor = queries::cursor_state(store.conn(), &f.runtime.id, path.to_str().unwrap())
        .unwrap()
        .unwrap();
    PipeCheckpoint::decode(&cursor.checkpoint).unwrap()
}

#[test]
fn a_re_frame_that_stops_between_batches_still_asks_the_ledger_when_it_resumes() {
    let mut f = Fixture::new();
    let video = mp4(4096, 21);
    let clip = f.media("clip.mp4", &video);
    // Two records to a batch below: the header and the opener are the first
    // batch, the record that sent the file is in the second.
    f.open_call("send-1");
    let offset = f.send("send-1", "result-1", &[(&clip, 4096, "video/mp4")]);
    f.append(json!({"timestamp":"2026-10-01T00:00:09Z","type":"response_item","payload":{"type":"message","id":"after","role":"assistant","content":[{"type":"output_text","text":"sent"}]}}));
    f.ingest();
    let session = session_id(&f.runtime);
    let shown = attachments(&f.store, &session);
    assert_eq!(asset_digest(&shown[0]), Some(digest(&video).as_str()));
    let details = tool_details(&f.store, &session);
    let captured = BTreeMap::from([(format!("{}.mp4", digest(&video)), video.clone())]);
    assert!(!checkpoint(&f, &f.store).reframing);

    for today in [None, Some(mp4(4096, 22))] {
        match &today {
            // The file is gone,
            None => std::fs::remove_file(&clip).unwrap(),
            // or it has the recorded size and other bytes.
            Some(bytes) => std::fs::write(&clip, bytes).unwrap(),
        }
        let name = if today.is_some() {
            "stopped-changed"
        } else {
            "stopped-deleted"
        };
        let (mut rebuilt, rebuilt_db) = f.rebuild(name, RebuildOptions::default());
        assert_eq!(tool_details(&rebuilt, &session), details, "{name}");
        let after_rebuild = checkpoint(&f, &rebuilt);
        assert!(after_rebuild.tail.is_none() && !after_rebuild.reframing);

        // The catch-up re-frame, stopped once its first batch is committed.
        let cancel = Arc::new(AtomicBool::new(false));
        let mut stopping = options();
        stopping.tail.max_records_per_batch = 2;
        stopping.cancel = Some(cancel.clone());
        stopping.progress = Some(Arc::new(CancelAfterFirstBatch(cancel)));
        let instrument = Instrument::new();
        let (_, files) = meta_ingest_into(
            &Sender,
            &f.runtime,
            &f.root,
            &mut rebuilt,
            &stopping,
            &instrument,
            Some(&mut f.ledger),
        )
        .unwrap();
        let stopped = stream_backfill_into(
            &Sender,
            &f.runtime,
            &f.root,
            &mut rebuilt,
            &files,
            &stopping,
            &instrument,
            Some(&mut f.ledger),
        );
        assert!(stopped.is_err(), "{name}: the pass was cancelled");
        f.ledger.flush().unwrap();
        let interrupted = checkpoint(&f, &rebuilt);
        let committed = interrupted.tail_checkpoint().unwrap().committed_offset;
        assert!(
            0 < committed && committed <= offset,
            "{name}: stopped before the record that sent the file"
        );
        assert_eq!(interrupted.source_generation, 1);
        assert!(
            interrupted.reframing,
            "{name}: the rest is still a re-frame"
        );

        // The ordinary pass that resumes from that tail.
        ingest(&f.runtime, &f.root, &mut rebuilt, &mut f.ledger, &options());
        assert_eq!(tool_details(&rebuilt, &session), details, "{name}");
        assert_eq!(attachments(&rebuilt, &session), shown, "{name}");
        assert_eq!(members(&rebuilt_db), captured, "{name}");
        let frames = media_frames(&f.ledger_dir);
        assert_eq!(frames.len(), 1, "{name}: no new media frame");
        assert_eq!((frames[0].0, frames[0].1), (1, offset));
        let finished = checkpoint(&f, &rebuilt);
        assert!(
            !finished.reframing,
            "{name}: cleared at the end of the file"
        );
        assert!(finished.tail.is_some());

        // Live equals a rebuild of the ledger as it now stands.
        let (again, again_db) = f.rebuild(&format!("{name}-again"), RebuildOptions::default());
        assert_eq!(tool_details(&again, &session), details, "{name}");
        assert_eq!(attachments(&again, &session), shown, "{name}");
        assert_eq!(members(&again_db), captured, "{name}");
    }
}
