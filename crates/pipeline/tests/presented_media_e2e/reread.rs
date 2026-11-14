//! Passes that re-read records the ledger already holds: the catch-up after
//! a rebuild, one that stops between batches, and a replacement generation.

use super::*;

// 2. A rebuilt projection's cursors carry no tail, so the next live pass
//    re-reads each transcript from byte zero in the same generation.
#[test]
fn a_catch_up_after_a_rebuild_keeps_the_capture_when_the_file_is_deleted_or_changed() {
    let mut f = Fixture::new();
    let video = mp4(4096, 2);
    let clip = f.media("clip.mp4", &video);
    let offset = f.send(NATIVE, "send-1", &[(&clip, 4096, MP4)]);
    f.ingest();
    let live = shown(&f.store, &f.db);
    assert_eq!(live.1, members(&[("mp4", &video)]));
    let captured = [(1, offset, Status::Captured)];
    assert_eq!(frame_statuses(&f.ledger_dir), captured);
    // Each runner below opens the ledger itself, as a restarted daemon does.
    f.ledger = Ledger::open(&f.temp.path().join("unused-ledger")).unwrap();

    for today in [None, Some(mp4(4096, 3))] {
        let name = match &today {
            // The sent file is gone,
            None => {
                std::fs::remove_file(&clip).unwrap();
                "deleted"
            }
            // or it holds other bytes of the recorded size.
            Some(bytes) => {
                std::fs::write(&clip, bytes).unwrap();
                "changed"
            }
        };
        let (mut rebuilt, rebuilt_db) = f.rebuild(name, &f.ledger_dir, RebuildOptions::default());
        assert_eq!(shown(&rebuilt, &rebuilt_db), live, "{name}: rebuilt");
        let rebuilt_cursor = checkpoint(&rebuilt);
        assert_eq!(rebuilt_cursor.source_generation, 1);
        assert!(
            rebuilt_cursor.tail.is_none(),
            "{name}: a rebuilt cursor has no tail"
        );

        // The start of the next serve: one catch-up, which re-reads the
        // transcript from byte zero in the same generation.
        let mut runner = f.runner(&mut rebuilt, &f.ledger_dir, &[]);
        runner.catch_up(&mut rebuilt, &Instrument::new()).unwrap();
        drop(runner);
        let caught_up = checkpoint(&rebuilt);
        assert_eq!(caught_up.source_generation, 1, "{name}: no new generation");
        assert!(
            caught_up.tail.is_some(),
            "{name}: the transcript was re-read"
        );
        assert_eq!(shown(&rebuilt, &rebuilt_db), live, "{name}: caught up");
        // The ledger gained no document frame.
        assert_eq!(frame_statuses(&f.ledger_dir), captured, "{name}");
        assert_eq!(media_frames(&f.ledger_dir)[0].document.bytes, video);
    }
}

// 3. A generation restart at the same offsets, with the sent file gone.
#[test]
fn a_replaced_transcript_carries_the_capture_into_its_new_generation() {
    let mut f = Fixture::new();
    let video = mp4(4096, 4);
    let clip = f.media("clip.mp4", &video);
    let offset = f.send(NATIVE, "send-1", &[(&clip, 4096, MP4)]);
    f.ingest();
    let session = f.session(NATIVE);
    let first = attachments(&f.store, &session);
    assert_eq!(
        slots(&f.store, &session),
        [asset(Video, &video, "clip.mp4", MP4)]
    );

    // The same bytes under a new inode: every record keeps its offset.
    std::fs::remove_file(&clip).unwrap();
    let transcript = f.transcript(NATIVE);
    let copy = transcript.with_extension("copy");
    std::fs::write(&copy, std::fs::read(&transcript).unwrap()).unwrap();
    std::fs::rename(&copy, &transcript).unwrap();
    f.ingest();

    assert_eq!(attachments(&f.store, &session), first, "still an asset");
    let active: i64 = f
        .store
        .conn()
        .query_row("SELECT active_generation FROM ingest_cursor", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(active, 2, "the replacement is the active generation");
    let generations = [(1, offset, Status::Captured), (2, offset, Status::Captured)];
    assert_eq!(frame_statuses(&f.ledger_dir), generations);
    let frames = media_frames(&f.ledger_dir);
    let (original, carried) = (&frames[0].document, &frames[1].document);
    assert_eq!(original.recovered_from, None);
    let recovered_from = carried.recovered_from.as_deref().unwrap();
    assert!(recovered_from.starts_with("ledger:"), "{recovered_from}");
    assert_eq!(recovered_from, format!("ledger:1:{offset}"));
    assert_eq!(carried.captured_at, original.captured_at);
    assert_eq!(carried.bytes, video);

    // A rebuild agrees, from the ledger alone.
    let live = shown(&f.store, &f.db);
    assert_eq!(live.1, members(&[("mp4", &video)]));
    f.remove_every_source();
    assert_eq!(f.rebuilt("replaced"), live);
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

// 2, interrupted: the daemon stops while the catch-up is part-way through a
//    transcript. The pass that resumes from the committed tail is still a
//    re-read of records the ledger holds.
#[test]
fn a_catch_up_that_stops_between_batches_keeps_the_capture_when_it_resumes() {
    let mut f = Fixture::new();
    let video = mp4(4096, 16);
    let clip = f.media("clip.mp4", &video);
    let files = [(clip.as_str(), 4096, MP4)];
    // Two records to a batch below: the prompt and the opener are the first
    // batch, the result that lists the file is in the second.
    f.prompt(NATIVE, "prompt-1", "record the screen");
    f.open_call(NATIVE, "send-1", &files);
    let offset = f.finish_call(NATIVE, "send-1", json!("sent"), &files);
    f.prompt(NATIVE, "prompt-2", "thanks");
    f.ingest();
    let session = f.session(NATIVE);
    let sent = attachments(&f.store, &session);
    assert_eq!(
        slots(&f.store, &session),
        [asset(Video, &video, "clip.mp4", MP4)]
    );
    let live = shown(&f.store, &f.db);
    let captured = [(1, offset, Status::Captured, None)];
    assert_eq!(frame_rows(&f.ledger_dir), captured);
    assert!(!checkpoint(&f.store).reframing);

    for today in [None, Some(mp4(4096, 17))] {
        let name = match &today {
            // The sent file is gone,
            None => {
                std::fs::remove_file(&clip).unwrap();
                "stopped-deleted"
            }
            // or it holds other bytes of the recorded size.
            Some(bytes) => {
                std::fs::write(&clip, bytes).unwrap();
                "stopped-changed"
            }
        };
        let (mut rebuilt, rebuilt_db) = f.rebuild(name, &f.ledger_dir, RebuildOptions::default());
        assert_eq!(shown(&rebuilt, &rebuilt_db), live, "{name}: rebuilt");
        let after_rebuild = checkpoint(&rebuilt);
        assert!(after_rebuild.tail.is_none() && !after_rebuild.reframing);

        // The catch-up, stopped once its first batch is committed.
        let cancel = Arc::new(AtomicBool::new(false));
        let mut stopping = options();
        stopping.tail.max_records_per_batch = 2;
        stopping.cancel = Some(cancel.clone());
        stopping.progress = Some(Arc::new(CancelAfterFirstBatch(cancel)));
        let (runtime, root, ledger) = (&f.runtime, &f.root, Some(&mut f.ledger));
        let stopped = try_ingest(
            &ClaudeCodeAdapter,
            runtime,
            root,
            &mut rebuilt,
            ledger,
            &stopping,
        );
        assert!(
            matches!(stopped, Err(PipelineError::Cancelled)),
            "{name}: {stopped:?}"
        );
        let interrupted = checkpoint(&rebuilt);
        let committed = interrupted.tail_checkpoint().unwrap().committed_offset;
        assert!(
            0 < committed && committed <= offset,
            "{name}: stopped before the result"
        );
        assert_eq!(interrupted.source_generation, 1);
        assert!(interrupted.reframing, "{name}: the rest is a re-read too");
        // The opener is behind the tail, so the resumed pass knows the call
        // from the checkpoint alone.
        let state = interrupted.adapter_state.unwrap();
        assert_eq!(state["send_file_calls"], json!(["send-1"]), "{name}");

        // The ordinary pass that resumes from that tail.
        f.catch_up(&mut rebuilt);
        assert_eq!(shown(&rebuilt, &rebuilt_db), live, "{name}: resumed");
        assert_eq!(attachments(&rebuilt, &session), sent, "{name}");
        assert_eq!(frame_rows(&f.ledger_dir), captured, "{name}: no new frame");
        assert_eq!(media_frames(&f.ledger_dir)[0].document.bytes, video);
        let finished = checkpoint(&rebuilt);
        assert!(finished.tail.is_some() && !finished.reframing, "{name}");

        // Live equals a rebuild of the ledger as it now stands.
        assert_eq!(f.rebuilt(&format!("{name}-again")), live, "{name}");
    }
}
