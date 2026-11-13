//! Capture at the record, the row it shows, and replay from the ledger alone
//! through every entry point: which frame a replay joins, and what a ledger
//! with no frame rebuilds (moved from `presented_media.rs`, which keeps the
//! shared fixture).

use super::*;

#[test]
fn sent_files_are_captured_shown_as_assets_and_replayed_by_every_entry_point() {
    let mut f = Fixture::new();
    let video = mp4(4096, 1);
    let clip = f.media("clip.mp4", &video);
    let shot = f.media("shot.png", &png());
    let gone = f.temp.path().join("media/gone.mp4");
    let gone = gone.to_str().unwrap();
    f.open_call("send-1");
    let offset = f.send(
        "send-1",
        "result-1",
        &[
            (&clip, 4096, "video/mp4"),
            (&shot, png().len() as u64, "image/png"),
            (gone, 512, "video/mp4"),
        ],
    );
    f.ingest();
    let session = session_id(&f.runtime);

    // The row: two captured assets and one honest path reference.
    let shown = attachments(&f.store, &session);
    assert_eq!(shown.len(), 3);
    assert_eq!(asset_digest(&shown[0]), Some(digest(&video).as_str()));
    assert_eq!(
        (shown[0].kind, shown[0].name.as_deref(), shown[0].byte_len),
        (AttachmentKind::Video, Some("clip.mp4"), Some(4096))
    );
    assert_eq!(shown[0].media_type.as_deref(), Some("video/mp4"));
    assert_eq!(asset_digest(&shown[1]), Some(digest(&png()).as_str()));
    assert_eq!(shown[1].kind, AttachmentKind::Image);
    assert!(shown[1].dimensions.is_some());
    assert!(matches!(
        shown[2].source,
        AttachmentSource::Reference { .. }
    ));
    assert_eq!(
        (
            shown[2].name.as_deref(),
            shown[2].media_type.as_deref(),
            shown[2].byte_len
        ),
        (Some("gone.mp4"), Some("video/mp4"), Some(512)),
        "a file that was not captured keeps what the transcript recorded"
    );
    let details = tool_details(&f.store, &session);
    assert_eq!(details.len(), 1);
    assert!(details[0].contains(&digest(&video)));

    // The members are the recorded bytes.
    let captured = members(&f.db);
    assert_eq!(
        captured,
        BTreeMap::from([
            (format!("{}.mp4", digest(&video)), video.clone()),
            (format!("{}.png", digest(&png())), png()),
        ])
    );

    // The ledger: one frame per sent file, at the record that sent it.
    let frames = media_frames(&f.ledger_dir);
    assert_eq!(
        frames
            .iter()
            .map(|(generation, at, frame)| (*generation, *at, frame.status, frame.path.as_str()))
            .collect::<Vec<_>>(),
        [
            (1, offset, Status::Captured, clip.as_str()),
            (1, offset, Status::Captured, shot.as_str()),
            (1, offset, Status::Missing, gone),
        ]
    );
    assert_eq!(frames[0].2.bytes, video);
    assert_eq!(
        frames[0].2.media,
        Some(MediaClaim {
            media_type: "video/mp4".into(),
            expected_len: 4096
        })
    );
    assert!(frames[2].2.bytes.is_empty());

    // Replay reads no filesystem: the files, the agent's data and the
    // projection's members are all gone.
    std::fs::remove_dir_all(f.temp.path().join("media")).unwrap();
    std::fs::remove_dir_all(&f.root).unwrap();
    std::fs::remove_dir_all(assets::asset_root(&f.db)).unwrap();

    // 1. The ledger-order rebuild, whole and with a flush after every op.
    let cache = f.temp.path().join("election.json");
    let (rebuilt, rebuilt_db) = f.rebuild("rebuilt", RebuildOptions::default());
    assert_eq!(tool_details(&rebuilt, &session), details);
    assert_eq!(attachments(&rebuilt, &session), shown);
    assert_eq!(members(&rebuilt_db), captured);
    let (chunked, chunked_db) = f.rebuild(
        "chunked",
        RebuildOptions {
            chunk_ops: 1,
            election_cache: Some(cache.clone()),
            ..RebuildOptions::default()
        },
    );
    assert_eq!(tool_details(&chunked, &session), details);
    assert_eq!(members(&chunked_db), captured);

    // 2. Prepared in RAM, then applied.
    let prepared = prepare_runtime(
        &Sender,
        &f.runtime,
        &f.ledger_dir,
        &Instrument::new(),
        RebuildOptions {
            election_cache: Some(cache),
            ..RebuildOptions::default()
        },
    )
    .unwrap();
    let prepared_db = f.temp.path().join("prepared.sqlite");
    let mut applied = Store::open_in_memory_for_rebuild_with_assets(&prepared_db).unwrap();
    apply_prepared_runtime(
        &Sender,
        &f.runtime,
        prepared,
        &mut applied,
        &Instrument::new(),
    )
    .unwrap();
    applied.flush_bulk_reconciliation().unwrap();
    applied.finish_bulk_load().unwrap();
    assert_eq!(tool_details(&applied, &session), details);
    assert_eq!(attachments(&applied, &session), shown);
    assert_eq!(members(&prepared_db), captured);

    // 3. An interrupted generation whose file is gone, closed from its
    //    staged frames.
    let staged_dir = f.temp.path().join("staged-ledger");
    let mut staged = copy_ledger(&f.ledger_dir, &staged_dir, false, &|_| Vec::new());
    let stopped_db = f.temp.path().join("stopped.sqlite");
    let mut stopped = Store::open(&stopped_db).unwrap();
    let instrument = Instrument::new();
    let report = rebuild_runtime_with_options(
        &Sender,
        &f.runtime,
        &staged_dir,
        &mut stopped,
        &instrument,
        RebuildOptions {
            allow_unsealed_sources: true,
            ..RebuildOptions::default()
        },
    )
    .unwrap();
    assert!(attachments(&stopped, &session).is_empty());
    let [(generation, source)] = report
        .unsealed_sources
        .into_values()
        .collect::<Vec<(u64, SourceDef)>>()
        .try_into()
        .unwrap();
    let closed = activate_stopped_source(
        &Sender,
        &f.runtime,
        &staged_dir,
        &mut stopped,
        &source,
        generation,
        &mut staged,
        &instrument,
        None,
    )
    .unwrap();
    assert!(closed.generation_clean);
    assert_eq!(tool_details(&stopped, &session), details);
    assert_eq!(attachments(&stopped, &session), shown);
    assert_eq!(members(&stopped_db), captured);
}

#[test]
fn html_and_media_frames_interleave_in_one_source() {
    let mut f = Fixture::new();
    let video = mp4(2048, 3);
    let clip = f.media("clip.mp4", &video);
    let pages_on_disk = [1, 2].map(|n| {
        let page = f.temp.path().join(format!("review-{n}.html"));
        std::fs::write(&page, format!("<h1>Review {n}</h1>")).unwrap();
        page
    });
    let present = |n: usize, f: &Fixture| {
        let marker = format!(
            "\u{e200}visualize\u{e202}{}\u{e201}",
            json!({"path":pages_on_disk[n - 1],"title":format!("Review {n}")})
        );
        f.append(json!({"timestamp":format!("2026-10-01T00:00:{n:02}Z"),"type":"response_item","payload":{"type":"message","id":format!("reply-{n}"),"role":"assistant","phase":"final_answer","content":[{"type":"output_text","text":marker}]}}));
    };
    present(1, &f);
    f.open_call("send-1");
    f.send("send-1", "result-1", &[(&clip, 2048, "video/mp4")]);
    present(2, &f);
    f.open_call("send-2");
    f.send("send-2", "result-2", &[(&clip, 2048, "video/mp4")]);
    f.ingest();
    let session = session_id(&f.runtime);

    let pages = |store: &Store| {
        html_artifacts::list(store.conn(), &ArtifactListRequest::default(), false)
            .unwrap()
            .entries
    };
    let shown = attachments(&f.store, &session);
    assert_eq!(shown.len(), 2, "one slot per send");
    assert_ne!(shown[0].id, shown[1].id);
    for attachment in &shown {
        assert_eq!(asset_digest(attachment), Some(digest(&video).as_str()));
    }
    let listed = pages(&f.store);
    assert_eq!(listed.len(), 2);
    assert!(listed.iter().all(|page| page.asset.is_some()));
    let kinds: Vec<bool> = scan_ledger(&f.ledger_dir)
        .unwrap()
        .entries
        .iter()
        .filter_map(|entry| match entry {
            LedgerEntry::Document { document, .. } => Some(document.media.is_some()),
            _ => None,
        })
        .collect();
    assert_eq!(kinds, [false, true, false, true]);
    let captured = members(&f.db);
    assert_eq!(captured.len(), 3, "two pages and one video");

    std::fs::remove_dir_all(f.temp.path().join("media")).unwrap();
    for page in &pages_on_disk {
        std::fs::remove_file(page).unwrap();
    }
    let (rebuilt, rebuilt_db) = f.rebuild(
        "rebuilt",
        RebuildOptions {
            chunk_ops: 1,
            ..RebuildOptions::default()
        },
    );
    assert_eq!(attachments(&rebuilt, &session), shown);
    assert_eq!(
        tool_details(&rebuilt, &session),
        tool_details(&f.store, &session)
    );
    assert_eq!(pages(&rebuilt), listed);
    assert_eq!(members(&rebuilt_db), captured);
}

#[test]
fn replay_joins_the_first_captured_frame_at_a_coordinate_and_ignores_strays() {
    // Raw records and no frame: capture with the switch off.
    let mut f = Fixture::new();
    let path = f.temp.path().join("media/clip.mp4");
    let path = path.to_str().unwrap().to_owned();
    let opened = f.open_call("send-1");
    let sent = f.send("send-1", "result-1", &[(&path, 64, "video/mp4")]);
    f.ingest_with(&IngestOptions {
        observe_presented_media: false,
        ..options()
    });
    assert!(media_frames(&f.ledger_dir).is_empty());
    let session = session_id(&f.runtime);

    let a = mp4(64, 1);
    let b = mp4(64, 2);
    let captured = |bytes: &[u8]| frame(&path, Status::Captured, bytes, 64);
    let missing = || frame(&path, Status::Missing, &[], 64);
    type Case = (&'static str, Vec<CapturedDocument>, Option<Vec<u8>>);
    let cases: Vec<Case> = vec![
        ("no frame", vec![], None),
        ("missing", vec![missing()], None),
        (
            "missing then captured",
            vec![missing(), captured(&a)],
            Some(a.clone()),
        ),
        (
            "captured then missing",
            vec![captured(&a), missing()],
            Some(a.clone()),
        ),
        (
            "two captures",
            vec![captured(&a), captured(&b)],
            Some(a.clone()),
        ),
        (
            "a capture that is not a video, then one that is",
            vec![captured(&[b'x'; 64]), captured(&b)],
            Some(b.clone()),
        ),
        (
            "a capture of another claim",
            vec![frame(&path, Status::Captured, &mp4(65, 1), 65)],
            None,
        ),
        (
            "a capture of another path",
            vec![frame("/elsewhere/clip.mp4", Status::Captured, &a, 64)],
            None,
        ),
    ];
    for (index, (name, frames, expected)) in cases.into_iter().enumerate() {
        let dir = f.temp.path().join(format!("ledger-{index}"));
        drop(copy_ledger(&f.ledger_dir, &dir, true, &|offset| {
            if offset == sent {
                frames.clone()
            } else {
                Vec::new()
            }
        }));
        let (store, db) = rebuild(
            &f.runtime,
            &dir,
            f.temp.path(),
            &format!("case-{index}"),
            RebuildOptions::default(),
        );
        let shown = attachments(&store, &session);
        assert_eq!(shown.len(), 1, "{name}");
        match expected {
            Some(bytes) => {
                assert_eq!(
                    asset_digest(&shown[0]),
                    Some(digest(&bytes).as_str()),
                    "{name}"
                );
                assert_eq!(
                    members(&db),
                    BTreeMap::from([(format!("{}.mp4", digest(&bytes)), bytes)]),
                    "{name}"
                );
            }
            None => {
                assert!(
                    matches!(shown[0].source, AttachmentSource::Reference { .. }),
                    "{name}: {shown:?}"
                );
                assert!(members(&db).is_empty(), "{name}");
            }
        }
    }

    // A captured frame at a record that sent nothing has no slot.
    let dir = f.temp.path().join("ledger-stray");
    drop(copy_ledger(&f.ledger_dir, &dir, true, &|offset| {
        if offset == opened {
            vec![captured(&a)]
        } else {
            Vec::new()
        }
    }));
    let (store, db) = rebuild(
        &f.runtime,
        &dir,
        f.temp.path(),
        "stray",
        RebuildOptions::default(),
    );
    let shown = attachments(&store, &session);
    assert!(matches!(
        shown[0].source,
        AttachmentSource::Reference { .. }
    ));
    assert!(members(&db).is_empty());
}

#[test]
fn a_ledger_with_no_frame_rebuilds_a_reference_whatever_is_on_disk_today() {
    let mut f = Fixture::new();
    let video = mp4(1024, 5);
    let clip = f.media("clip.mp4", &video);
    f.open_call("send-1");
    f.send("send-1", "result-1", &[(&clip, 1024, "video/mp4")]);
    f.ingest_with(&IngestOptions {
        observe_presented_media: false,
        ..options()
    });
    let session = session_id(&f.runtime);

    // The switch: a reference, no member, no frame.
    let shown = attachments(&f.store, &session);
    assert_eq!(shown.len(), 1);
    assert!(matches!(
        shown[0].source,
        AttachmentSource::Reference { .. }
    ));
    assert_eq!(shown[0].byte_len, Some(1024));
    assert!(members(&f.db).is_empty());
    assert!(media_frames(&f.ledger_dir).is_empty());

    // The file is still there, and replay does not look.
    assert_eq!(std::fs::read(&clip).unwrap(), video);
    let (rebuilt, rebuilt_db) = f.rebuild("rebuilt", RebuildOptions::default());
    assert_eq!(attachments(&rebuilt, &session), shown);
    assert_eq!(
        tool_details(&rebuilt, &session),
        tool_details(&f.store, &session)
    );
    assert!(members(&rebuilt_db).is_empty());
}
