//! A sent file captured live and replayed from the ledger alone: the plain
//! case, the capture switch, and one result that carries inline media too.

use super::*;

// 1. Live capture, then a rebuild from the ledger alone.
#[test]
fn a_sent_video_and_image_are_captured_and_rebuilt_from_the_ledger_with_the_files_deleted() {
    let mut f = Fixture::new();
    let (video, png) = (mp4(4096, 1), decoded(PNG));
    let clip = f.media("screen recording.mp4", &video);
    let shot = f.media("shot.png", &png);
    let files: [Sent<'_>; 2] = [(&clip, 4096, MP4), (&shot, png.len() as u64, "image/png")];
    let offset = f.send(NATIVE, "send-1", &files);
    f.ingest();

    let session = f.session(NATIVE);
    let sent = [
        asset(Video, &video, "screen recording.mp4", MP4),
        asset(Image, &png, "shot.png", "image/png"),
    ];
    assert_eq!(slots(&f.store, &session), sent);
    assert!(attachments(&f.store, &session)[1].dimensions.is_some());
    let live = shown(&f.store, &f.db);
    assert_eq!(live.0.len(), 1, "one tool row");
    for (extension, bytes) in [("mp4", &video), ("png", &png)] {
        let url = format!("/api/assets/{}.{extension}", digest(bytes));
        assert!(live.0[0].contains(&url), "{url}");
    }
    // The members are the sent bytes; the ledger holds one frame per file at
    // the record that sent it.
    assert_eq!(live.1, members(&[("mp4", &video), ("png", &png)]));
    let captured = (1, offset, Status::Captured);
    assert_eq!(frame_statuses(&f.ledger_dir), [captured, captured]);
    let frames = media_frames(&f.ledger_dir);
    let paths = [&frames[0].document.path, &frames[1].document.path];
    assert_eq!(paths, [&clip, &shot]);
    assert_eq!(frames[0].document.bytes, video);
    let claim = MediaClaim {
        media_type: MP4.into(),
        expected_len: 4096,
    };
    assert_eq!(frames[0].document.media, Some(claim));
    assert_eq!(frames[0].document.recovered_from, None);
    let tally = verify_ledger(&f.ledger_dir).unwrap().tally;
    assert_eq!((tally.documents, tally.media_frames()), (2, 2));
    assert_eq!(tally.media_captured, 2);
    assert_eq!(tally.media_captured_bytes, 4096 + png.len() as u64);

    // A quiet pass captures nothing again.
    f.ingest();
    assert_eq!(media_frames(&f.ledger_dir).len(), 2);
    assert_eq!(shown(&f.store, &f.db), live);

    f.remove_every_source();
    assert_eq!(f.rebuilt("rebuilt"), live);
}

// 5. Raw records and no frame.
#[test]
fn a_ledger_written_with_the_switch_off_rebuilds_a_reference_while_the_file_is_present() {
    let mut f = Fixture::new();
    let video = mp4(1024, 6);
    let clip = f.media("clip.mp4", &video);
    f.send(NATIVE, "send-1", &[(&clip, 1024, MP4)]);
    f.ingest_with(&IngestOptions {
        observe_presented_media: false,
        ..options()
    });

    // A reference keeps what the transcript recorded.
    let sent = [reference(Video, "clip.mp4", MP4, 1024)];
    assert_eq!(slots(&f.store, &f.session(NATIVE)), sent);
    let live = shown(&f.store, &f.db);
    assert!(live.1.is_empty(), "no member");
    assert!(media_frames(&f.ledger_dir).is_empty(), "no frame");
    assert_eq!(
        verify_ledger(&f.ledger_dir).unwrap().tally.media_frames(),
        0
    );

    // The file is still there, and replay does not look.
    assert_eq!(std::fs::read(&clip).unwrap(), video);
    assert_eq!(f.rebuilt("rebuilt"), live);
}

// 7. One result record with an inline image block and three sent files.
#[test]
fn an_inline_image_and_sent_files_on_one_result_are_each_attached_once_in_one_order() {
    let mut f = Fixture::new();
    let (video, png, gif) = (mp4(4096, 8), decoded(PNG), decoded(GIF));
    let clip = f.media("clip.mp4", &video);
    let picture = f.media("picture.gif", &gif);
    let gone = f.temp.path().join("media/gone.mp4");
    let files = [
        (picture.as_str(), gif.len() as u64, "image/gif"),
        (gone.to_str().unwrap(), 512, MP4),
        (clip.as_str(), 4096, MP4),
    ];
    f.open_call(NATIVE, "send-1", &files);
    let content = json!([
        {"type":"text","text":"sent 3 files"},
        {"type":"image","source":{"type":"base64","media_type":"image/png","data":PNG}},
    ]);
    let offset = f.finish_call(NATIVE, "send-1", content, &files);
    f.ingest();
    let session = f.session(NATIVE);

    // The live fold orders a row's attachments by the evidence that first
    // attached each: the sent files' reference update, in native order, then
    // the result's own inline media.
    let mut inline = asset(Image, &png, "", "image/png");
    inline.2 = None;
    let sent = [
        asset(Image, &gif, "picture.gif", "image/gif"),
        reference(Video, "gone.mp4", MP4, 512),
        asset(Video, &video, "clip.mp4", MP4),
        inline,
    ];
    assert_eq!(slots(&f.store, &session), sent);
    // A sent file's slot is its native index, so every attachment is there
    // once.
    let attached = attachments(&f.store, &session);
    let ids: Vec<&str> = attached.iter().map(|a| a.id.as_str()).collect();
    let sent_slot = |index: usize| format!("{}:sent-{index}", digest(b"send-1"));
    assert_eq!(ids[..3], [sent_slot(0), sent_slot(1), sent_slot(2)]);
    assert!(!ids[..3].contains(&ids[3]));
    let live = shown(&f.store, &f.db);
    assert_eq!(live.0.len(), 1, "one tool row");
    assert_eq!(
        live.1,
        members(&[("png", &png), ("gif", &gif), ("mp4", &video)])
    );
    // One frame per sent file; the inline image needs none.
    let (captured, missing) = ((1, offset, Status::Captured), (1, offset, Status::Missing));
    assert_eq!(frame_statuses(&f.ledger_dir), [captured, missing, captured]);

    // Replayed from the ledger alone: in ledger order, whole and with a
    // flush after every op, then prepared in RAM and applied.
    f.remove_every_source();
    assert_eq!(f.rebuilt("rebuilt"), live);
    let cache = f.temp.path().join("election.json");
    let chunked = RebuildOptions {
        chunk_ops: 1,
        election_cache: Some(cache.clone()),
        ..RebuildOptions::default()
    };
    let (rebuilt, rebuilt_db) = f.rebuild("chunked", &f.ledger_dir, chunked);
    assert_eq!(shown(&rebuilt, &rebuilt_db), live);
    assert_eq!(attachments(&rebuilt, &session), attached);

    let (adapter, instrument) = (&ClaudeCodeAdapter, Instrument::new());
    let prepare = RebuildOptions {
        election_cache: Some(cache),
        ..RebuildOptions::default()
    };
    let prepared =
        prepare_runtime(adapter, &f.runtime, &f.ledger_dir, &instrument, prepare).unwrap();
    let prepared_db = f.temp.path().join("prepared.sqlite");
    let mut applied = Store::open_in_memory_for_rebuild_with_assets(&prepared_db).unwrap();
    apply_prepared_runtime(adapter, &f.runtime, prepared, &mut applied, &instrument).unwrap();
    applied.flush_bulk_reconciliation().unwrap();
    applied.finish_bulk_load().unwrap();
    assert_eq!(shown(&applied, &prepared_db), live);
    assert_eq!(attachments(&applied, &session), attached);
}
