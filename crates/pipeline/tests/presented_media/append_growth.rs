//! Append growth reads the file first, and falls back to the source's own
//! earlier capture when the file cannot be captured: a repeated opener and
//! result, and a later call that sends the same file again. A first
//! generation has nothing earlier and never looks.

use super::*;

/// `(generation, offset, status, recovered_from)` of every media frame.
fn frame_rows(ledger_dir: &Path) -> Vec<(u64, u64, Status, Option<String>)> {
    media_frames(ledger_dir)
        .into_iter()
        .map(|(generation, offset, frame)| (generation, offset, frame.status, frame.recovered_from))
        .collect()
}

/// A rebuild shows `live`'s rows, and the catch-up pass that follows it
/// (a re-frame from byte zero) changes no row and ledgers no frame.
fn assert_rebuild_and_catch_up_agree(f: &mut Fixture, name: &str, video: &[u8]) {
    let session = session_id(&f.runtime);
    let live = attachments(&f.store, &session);
    let details = tool_details(&f.store, &session);
    let frames = frame_rows(&f.ledger_dir);
    let member = BTreeMap::from([(format!("{}.mp4", digest(video)), video.to_vec())]);

    let (mut rebuilt, rebuilt_db) = f.rebuild(name, RebuildOptions::default());
    assert_eq!(attachments(&rebuilt, &session), live, "rebuild");
    assert_eq!(tool_details(&rebuilt, &session), details, "rebuild");
    assert_eq!(members(&rebuilt_db), member, "rebuild");

    ingest(&f.runtime, &f.root, &mut rebuilt, &mut f.ledger, &options());
    assert_eq!(attachments(&rebuilt, &session), live, "catch-up");
    assert_eq!(tool_details(&rebuilt, &session), details, "catch-up");
    assert_eq!(members(&rebuilt_db), member, "catch-up");
    assert_eq!(
        frame_rows(&f.ledger_dir),
        frames,
        "catch-up ledgers no frame"
    );
}

#[test]
fn a_repeated_opener_and_result_keep_the_capture_after_the_file_is_gone() {
    let mut f = Fixture::new();
    let video = mp4(4096, 11);
    let clip = f.media("clip.mp4", &video);
    f.open_call("send-1");
    let first = f.send("send-1", "result-1", &[(&clip, 4096, "video/mp4")]);
    f.ingest();
    let session = session_id(&f.runtime);
    let shown = attachments(&f.store, &session);
    assert_eq!(shown.len(), 1);
    assert_eq!(asset_digest(&shown[0]), Some(digest(&video).as_str()));

    // The file is gone, and the transcript appends the same opener and the
    // same result again: the same tool row, the same attachment slot, at a
    // later record.
    std::fs::remove_file(&clip).unwrap();
    f.open_call("send-1");
    let again = f.send("send-1", "result-1", &[(&clip, 4096, "video/mp4")]);
    assert!(again > first);
    f.ingest();

    let live = attachments(&f.store, &session);
    assert_eq!(live.len(), 1, "one slot: {live:?}");
    assert_eq!(live[0].id, shown[0].id);
    assert_eq!(
        asset_digest(&live[0]),
        Some(digest(&video).as_str()),
        "the repeat did not turn the capture back into a path reference"
    );
    assert_eq!(
        members(&f.db),
        BTreeMap::from([(format!("{}.mp4", digest(&video)), video.clone())])
    );

    // The capture was carried to the repeated record from the ledger.
    assert_eq!(
        frame_rows(&f.ledger_dir),
        [
            (1, first, Status::Captured, None),
            (
                1,
                again,
                Status::Captured,
                Some(format!("ledger:1:{first}"))
            ),
        ]
    );
    let frames = media_frames(&f.ledger_dir);
    assert_eq!(frames[1].2.bytes, video);
    assert_eq!(frames[1].2.captured_at, frames[0].2.captured_at);

    // The row does not depend on history: a rebuild shows it, and the
    // re-frame after the rebuild adds nothing.
    assert_rebuild_and_catch_up_agree(&mut f, "repeat", &video);
}

#[test]
fn a_second_call_that_sends_the_same_file_after_it_is_gone_shows_the_capture() {
    let mut f = Fixture::new();
    let video = mp4(4096, 12);
    let clip = f.media("clip.mp4", &video);
    f.open_call("send-1");
    let first = f.send("send-1", "result-1", &[(&clip, 4096, "video/mp4")]);
    f.ingest();
    let session = session_id(&f.runtime);

    // Another call, another tool row, the same path and size; no file.
    std::fs::remove_file(&clip).unwrap();
    f.open_call("send-2");
    let second = f.send("send-2", "result-2", &[(&clip, 4096, "video/mp4")]);
    f.ingest();

    let live = attachments(&f.store, &session);
    assert_eq!(live.len(), 2, "one slot per call: {live:?}");
    assert_ne!(live[0].id, live[1].id);
    assert_eq!(asset_digest(&live[0]), Some(digest(&video).as_str()));
    assert_eq!(
        asset_digest(&live[1]),
        Some(digest(&video).as_str()),
        "the second call shows the source's own earlier capture"
    );
    assert_eq!(
        frame_rows(&f.ledger_dir),
        [
            (1, first, Status::Captured, None),
            (
                1,
                second,
                Status::Captured,
                Some(format!("ledger:1:{first}"))
            ),
        ]
    );
    assert_rebuild_and_catch_up_agree(&mut f, "second-call", &video);
}

#[test]
fn a_first_generation_whose_file_is_missing_does_not_walk_the_ledger() {
    let mut f = Fixture::new();
    let video = mp4(4096, 13);
    let clip = f.media("clip.mp4", &video);
    f.open_call("send-1");
    let offset = f.send("send-1", "result-1", &[(&clip, 4096, "video/mp4")]);
    f.ingest();
    assert_eq!(
        frame_rows(&f.ledger_dir),
        [(1, offset, Status::Captured, None)]
    );
    std::fs::remove_file(&clip).unwrap();

    // A projection that has never seen this source reads it as a first
    // generation. This ledger happens to hold a capture at the very
    // coordinate, which is what makes a walk visible here: a pass that
    // looked would show it. A first generation does not look.
    let fresh_db = f.temp.path().join("fresh.sqlite");
    let mut fresh = Store::open(&fresh_db).unwrap();
    ingest(&f.runtime, &f.root, &mut fresh, &mut f.ledger, &options());
    let session = session_id(&f.runtime);
    let shown = attachments(&fresh, &session);
    assert_eq!(shown.len(), 1);
    assert!(
        matches!(shown[0].source, AttachmentSource::Reference { .. }),
        "{shown:?}"
    );
    assert!(members(&fresh_db).is_empty());
    assert_eq!(
        frame_rows(&f.ledger_dir),
        [
            (1, offset, Status::Captured, None),
            (1, offset, Status::Missing, None),
        ],
        "the file was observed, and nothing was carried"
    );

    // The generation is active now, so growth of it does fall back.
    f.open_call("send-2");
    let later = f.send("send-2", "result-2", &[(&clip, 4096, "video/mp4")]);
    ingest(&f.runtime, &f.root, &mut fresh, &mut f.ledger, &options());
    let shown = attachments(&fresh, &session);
    assert_eq!(shown.len(), 2);
    assert_eq!(asset_digest(&shown[1]), Some(digest(&video).as_str()));
    assert_eq!(
        frame_rows(&f.ledger_dir)[2],
        (
            1,
            later,
            Status::Captured,
            Some(format!("ledger:1:{offset}"))
        )
    );
}

#[test]
fn a_sliced_first_read_does_not_walk_the_ledger() {
    // A sliced pass activates its first slice early, so every later slice of
    // a file never read before is append growth of an active generation. It
    // is still the first read: nothing earlier can exist, and a walk per
    // slice would be pure cost on the writer thread.
    let mut f = Fixture::new();
    let video = mp4(4096, 17);
    let clip = f.media("clip.mp4", &video);
    f.open_call("send-1");
    let first = f.send("send-1", "result-1", &[(&clip, 4096, "video/mp4")]);
    f.open_call("send-2");
    let second = f.send("send-2", "result-2", &[(&clip, 4096, "video/mp4")]);
    let sliced = IngestOptions {
        starter_budget: Some(rosetta_pipeline::SliceBudget {
            max_bytes: 1,
            min_records: 3,
        }),
        ..options()
    };

    // Slice one: header, opener, result. The file is there and is captured.
    f.ingest_with(&sliced);
    assert_eq!(
        frame_rows(&f.ledger_dir),
        [(1, first, Status::Captured, None)]
    );
    std::fs::remove_file(&clip).unwrap();

    // The rest of the first read. The second send finds no file; a pass that
    // looked would carry the first capture to it.
    for _ in 0..4 {
        f.ingest_with(&sliced);
    }
    assert_eq!(
        frame_rows(&f.ledger_dir),
        [
            (1, first, Status::Captured, None),
            (1, second, Status::Missing, None),
        ],
        "the second send was observed, and nothing was carried"
    );
    let session = session_id(&f.runtime);
    let shown = attachments(&f.store, &session);
    assert_eq!(shown.len(), 2);
    assert_eq!(asset_digest(&shown[0]), Some(digest(&video).as_str()));
    assert!(
        matches!(shown[1].source, AttachmentSource::Reference { .. }),
        "{shown:?}"
    );
    let (rebuilt, _) = f.rebuild("sliced", RebuildOptions::default());
    assert_eq!(attachments(&rebuilt, &session), shown, "rebuild");
}
