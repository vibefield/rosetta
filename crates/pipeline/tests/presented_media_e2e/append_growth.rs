//! Growth of a transcript across passes and restarts: a send split across
//! two polls, a restart after a crash, and a repeated opener and result.

use super::*;

// 6. The warm path: the opener arrives in one poll and its result in the
//    next, across a restart, so the opener id comes back from the checkpoint.
#[test]
fn a_warm_send_split_across_polls_equals_a_cold_backfill_and_a_ledger_only_replay() {
    let mut f = Fixture::new();
    // The warm projection is this fixture's, held here so a poll can borrow
    // it while the transcript is written.
    let mut store = std::mem::replace(&mut f.store, Store::open_in_memory().unwrap());
    let relative = "projects/synthetic/native.jsonl";
    let session = f.session(NATIVE);
    let warm_ledger = f.temp.path().join("warm-ledger");
    let video = mp4(8192, 7);
    let clip = f.media("clip.mp4", &video);
    let files = [(clip.as_str(), 8192, MP4)];
    let instrument = Instrument::new();
    // One start of the daemon's runner, and the one poll it then makes.
    let poll = |store: &mut Store, write: &dyn Fn() -> u64| {
        let mut runner = f.runner(store, &warm_ledger, &[relative]);
        let offset = write();
        runner.tick(store, &instrument).unwrap();
        offset
    };
    let rows = |store: &Store| queries::timeline_page(store.conn(), &session, None, 100).unwrap();

    // The warm roster exists before the session does.
    poll(&mut store, &|| f.open_call(NATIVE, "send-1", &files));
    let opened = rows(&store);
    assert_eq!(opened.len(), 1);
    assert!(attachments(&store, &session).is_empty());
    assert!(media_frames(&warm_ledger).is_empty());
    let state = checkpoint(&store).adapter_state;
    assert_eq!(state.unwrap()["send_file_calls"], json!(["send-1"]));

    // The result arrives after a restart of the projection and the runner.
    drop(store);
    let mut store = Store::open(&f.db).unwrap();
    let sent = json!("sent");
    let offset = poll(&mut store, &|| {
        f.finish_call(NATIVE, "send-1", sent.clone(), &files)
    });
    let warm = rows(&store);
    assert_eq!(warm.len(), 1);
    assert_eq!(warm[0].timeline_id, opened[0].timeline_id, "the same row");
    assert_eq!(
        slots(&store, &session),
        [asset(Video, &video, "clip.mp4", MP4)]
    );
    let live = shown(&store, &f.db);
    assert_eq!(live.1, members(&[("mp4", &video)]));
    assert_eq!(
        frame_statuses(&warm_ledger),
        [(1, offset, Status::Captured)]
    );
    drop(store);

    // A cold backfill of the same root, with no ledger.
    let cold_db = f.temp.path().join("cold.sqlite");
    let mut cold = Store::open(&cold_db).unwrap();
    try_ingest(
        &ClaudeCodeAdapter,
        &f.runtime,
        &f.root,
        &mut cold,
        None,
        &options(),
    )
    .unwrap();
    assert_eq!(shown(&cold, &cold_db), live);
    assert_eq!(rows(&cold)[0].body, warm[0].body);

    // A replay of the warm ledger alone.
    f.remove_every_source();
    let (replay, replay_db) = f.rebuild("replay", &warm_ledger, RebuildOptions::default());
    assert_eq!(shown(&replay, &replay_db), live);
    let replayed = rows(&replay);
    assert_eq!(replayed[0].timeline_id, warm[0].timeline_id);
    assert_eq!(replayed[0].body, warm[0].body);
}

/// Copy a projection's database files over another's (members stay put).
fn copy_projection(from: &Path, to: &Path) {
    for suffix in ["", "-wal", "-shm"] {
        let with_suffix = |path: &Path| {
            let mut name = path.as_os_str().to_owned();
            name.push(suffix);
            PathBuf::from(name)
        };
        let _ = std::fs::remove_file(with_suffix(to));
        if with_suffix(from).exists() {
            std::fs::copy(with_suffix(from), with_suffix(to)).unwrap();
        }
    }
}

// 4. The crash window. Append growth reads the file first and asks the
//    ledger when the file cannot be captured, so a restart finds the capture
//    its interrupted batch ledgered.
#[test]
fn a_restart_after_a_crash_between_ledger_and_store_finds_the_capture_in_the_ledger() {
    let mut f = Fixture::new();
    let video = mp4(2048, 5);
    let clip = f.media("clip.mp4", &video);
    let files = [(clip.as_str(), 2048, MP4)];
    let session = f.session(NATIVE);
    let saved = f.temp.path().join("before-the-batch.sqlite");

    // The opener is committed; the projection is copied before the batch
    // that carries the result.
    f.open_call(NATIVE, "send-1", &files);
    f.ingest();
    f.reopen(|db| copy_projection(db, &saved));

    // The batch: ledger first, then the store.
    let offset = f.finish_call(NATIVE, "send-1", json!("sent"), &files);
    f.ingest();
    let committed = attachments(&f.store, &session);
    assert_eq!(
        slots(&f.store, &session),
        [asset(Video, &video, "clip.mp4", MP4)]
    );
    let live = shown(&f.store, &f.db);
    let captured = [(1, offset, Status::Captured, None)];
    assert_eq!(frame_rows(&f.ledger_dir), captured);

    // A hard kill after the ledger append and before the store commit: the
    // store is where it was, with no member; the ledger keeps the batch; and
    // the sent file vanishes before the restart.
    f.reopen(|db| {
        copy_projection(&saved, db);
        std::fs::remove_dir_all(assets::asset_root(db)).unwrap();
    });
    assert!(attachments(&f.store, &session).is_empty());
    std::fs::remove_file(&clip).unwrap();
    f.ingest();

    // The restart reads the record as append growth. The file is gone, and
    // the ledger holds its capture at this very record: the row is the one
    // the batch would have committed, the member is written from the frame,
    // and nothing new is ledgered.
    assert_eq!(attachments(&f.store, &session), committed);
    assert_eq!(shown(&f.store, &f.db), live);
    assert_eq!(live.1, members(&[("mp4", &video)]));
    assert_eq!(frame_rows(&f.ledger_dir), captured);

    // Live and replay agree.
    assert_eq!(f.rebuilt("rebuilt"), live);
}

// 4, what is left of it: the file is read before the ledger is asked, so a
//    file that holds other bytes of the recorded size at the restart is
//    captured as found.
#[test]
fn documented_limit_a_file_replaced_before_a_crashed_batch_is_reread_shows_its_new_bytes_live() {
    let mut f = Fixture::new();
    let (video, replaced) = (mp4(2048, 18), mp4(2048, 19));
    let clip = f.media("clip.mp4", &video);
    let files = [(clip.as_str(), 2048, MP4)];
    let session = f.session(NATIVE);
    let saved = f.temp.path().join("before-the-batch.sqlite");
    f.open_call(NATIVE, "send-1", &files);
    f.ingest();
    f.reopen(|db| copy_projection(db, &saved));
    let offset = f.finish_call(NATIVE, "send-1", json!("sent"), &files);
    f.ingest();
    let committed = shown(&f.store, &f.db);
    assert_eq!(committed.1, members(&[("mp4", &video)]));

    // The same crash, and the path holds another video of the same size.
    f.reopen(|db| {
        copy_projection(&saved, db);
        std::fs::remove_dir_all(assets::asset_root(db)).unwrap();
    });
    std::fs::write(&clip, &replaced).unwrap();
    f.ingest();

    // Live shows today's bytes, in a second captured frame at the record.
    assert_eq!(
        slots(&f.store, &session),
        [asset(Video, &replaced, "clip.mp4", MP4)]
    );
    let both = [
        (1, offset, Status::Captured, None),
        (1, offset, Status::Captured, None),
    ];
    assert_eq!(frame_rows(&f.ledger_dir), both);
    let frames = media_frames(&f.ledger_dir);
    assert_eq!(frames[0].document.bytes, video);
    assert_eq!(frames[1].document.bytes, replaced);

    // The next replay of the source shows the first capture: the first
    // captured frame at a coordinate wins.
    assert_eq!(f.rebuilt("rebuilt"), committed);
}

// A record Claude Code wrote twice: the same opener and the same result
// again, after the sent file is gone.
#[test]
fn a_repeated_opener_and_result_keep_the_capture_after_the_file_is_gone() {
    let mut f = Fixture::new();
    let video = mp4(4096, 15);
    let clip = f.media("clip.mp4", &video);
    let files = [(clip.as_str(), 4096, MP4)];
    let first = f.send(NATIVE, "send-1", &files);
    f.ingest();
    let session = f.session(NATIVE);
    let sent = attachments(&f.store, &session);
    assert_eq!(
        slots(&f.store, &session),
        [asset(Video, &video, "clip.mp4", MP4)]
    );
    assert_eq!(
        frame_rows(&f.ledger_dir),
        [(1, first, Status::Captured, None)]
    );

    // The repeated opener alone clears the row's attachments: the adapter
    // states the call again, and the fold takes the later statement.
    std::fs::remove_file(&clip).unwrap();
    f.open_call(NATIVE, "send-1", &files);
    f.ingest();
    assert!(attachments(&f.store, &session).is_empty());
    assert_eq!(frame_rows(&f.ledger_dir).len(), 1);

    // The repeated result names the same slot at a later record. Its file is
    // gone, and the source's own earlier capture stands in.
    let again = f.finish_call(NATIVE, "send-1", json!("sent"), &files);
    assert!(again > first);
    f.ingest();
    assert_eq!(
        attachments(&f.store, &session),
        sent,
        "the same slot, still the member"
    );
    let live = shown(&f.store, &f.db);
    assert_eq!(live.0.len(), 1, "one tool row");
    assert_eq!(live.1, members(&[("mp4", &video)]));
    let carried = Some(format!("ledger:1:{first}"));
    let frames = [
        (1, first, Status::Captured, None),
        (1, again, Status::Captured, carried),
    ];
    assert_eq!(frame_rows(&f.ledger_dir), frames);
    let ledgered = media_frames(&f.ledger_dir);
    assert_eq!(ledgered[1].document.bytes, video);
    assert_eq!(
        ledgered[1].document.captured_at,
        ledgered[0].document.captured_at
    );

    // The row does not depend on history: a rebuild shows it, and the
    // catch-up after the rebuild changes no row and ledgers no frame.
    let (mut rebuilt, rebuilt_db) = f.rebuild("repeat", &f.ledger_dir, RebuildOptions::default());
    assert_eq!(shown(&rebuilt, &rebuilt_db), live, "rebuilt");
    assert_eq!(attachments(&rebuilt, &session), sent);
    f.catch_up(&mut rebuilt);
    assert_eq!(shown(&rebuilt, &rebuilt_db), live, "caught up");
    assert_eq!(frame_rows(&f.ledger_dir), frames, "no new frame");
}
