//! A fresh cold build of one root, serial and on a capture pool.

use super::*;

/// A fresh cold build of the fixture's root into its own projection and
/// ledger under `dir`: the closed-source walk, serial or on a capture pool.
fn cold_build(f: &Fixture, dir: &Path, pool: Option<&CapturePool>) -> Store {
    std::fs::create_dir_all(dir).unwrap();
    let (instrument, opts, walk) = (Instrument::new(), options(), ColdWalkOptions::default());
    let adapter: &'static dyn AgentAdapter = &ClaudeCodeAdapter;
    let (runtime, root) = (&f.runtime, f.root.as_path());
    let mut ledger = Ledger::open(&dir.join("ledger")).unwrap();
    let mut store =
        Store::open_in_memory_for_rebuild_with_assets(&dir.join("projection.sqlite")).unwrap();
    let mut planner = Store::open_in_memory_for_cold_planning().unwrap();
    let journal = Some(&mut ledger);
    let (_, files) = meta_ingest_into(
        adapter,
        runtime,
        root,
        &mut planner,
        &opts,
        &instrument,
        journal,
    )
    .unwrap();
    store.use_precomputed_bulk_ordinals().unwrap();
    for batch in planner.planned_metadata_batches().unwrap() {
        store.apply(&batch).unwrap();
    }
    let setup = streamed_setup(&files, &mut planner).unwrap();
    let mut writer =
        StreamedRuntime::open(adapter, runtime, setup, &mut store, &instrument).unwrap();
    let mut preparer = StreamedPreparer::new(&files);
    let mut emit = BlockingEmit(|_, mut sources| {
        preparer.prepare(&mut sources)?;
        writer.apply(&mut store, sources, &instrument)?;
        Ok(())
    });
    let (planner, journal) = (&mut planner, Some(&mut ledger));
    let report = match pool {
        None => stream_closed_sources_into(
            adapter,
            runtime,
            root,
            planner,
            &files,
            &opts,
            &instrument,
            journal,
            None,
            &walk,
            &mut emit,
        ),
        Some(pool) => pool.scoped(|| {
            stream_closed_sources_pooled(
                pool,
                adapter,
                runtime,
                root,
                planner,
                &files,
                &opts,
                &instrument,
                journal,
                None,
                &walk,
                &mut emit,
            )
        }),
    };
    assert_eq!(report.unwrap().hard_errors, 0);
    let prepared = preparer.finish().unwrap();
    writer.admit(&mut store, &instrument, &prepared).unwrap();
    store.finish_bulk_load().unwrap();
    ledger.flush().unwrap();
    store
}

// 8. A serial and a pooled cold build of one root.
#[test]
fn a_pooled_cold_build_captures_what_the_serial_build_does() {
    let f = Fixture::new();
    let (shared, gif) = (mp4(4096, 9), decoded(GIF));
    let shared_clip = f.media("shared.mp4", &shared);
    let picture = f.media("picture.gif", &gif);
    let gone = f.temp.path().join("media/gone.mp4");
    // A file whose size is not the size its transcript recorded.
    let grown = f.media("grown.mp4", &mp4(600, 10));
    let mut captured = members(&[("mp4", &shared), ("gif", &gif)]);
    for index in 0..8_u8 {
        let native = format!("session-{index}");
        let own = mp4(1024 + usize::from(index) * 64, 20 + index);
        let own_clip = f.media(&format!("own-{index}.mp4"), &own);
        captured.extend(members(&[("mp4", &own)]));
        // Enough prose that the sources differ in size and batch shape.
        for prompt in 0..usize::from(index) * 3 {
            let text = format!("prompt {prompt} {}", "x".repeat(200));
            f.prompt(&native, &format!("prompt-{index}-{prompt}"), &text);
        }
        let own_len = own.len() as u64;
        let own_files: [Sent<'_>; 2] = [(&own_clip, own_len, MP4), (&shared_clip, 4096, MP4)];
        f.send(&native, "send-own", &own_files);
        let rest: [Sent<'_>; 3] = [
            (&picture, gif.len() as u64, "image/gif"),
            (gone.to_str().unwrap(), 512, MP4),
            (&grown, 512, MP4),
        ];
        f.send(&native, "send-rest", &rest);
    }

    let serial_dir = f.temp.path().join("serial");
    let serial = cold_build(&f, &serial_dir, None);
    let built = shown(&serial, &serial_dir.join("projection.sqlite"));
    assert_eq!(built.0.len(), 16, "two tool rows per session");
    assert_eq!(built.1, captured);
    let verified = verify_ledger(&serial_dir.join("ledger")).unwrap();
    let tally = verified.tally;
    assert_eq!((tally.media_frames(), tally.media_captured), (40, 24));
    assert_eq!(
        (tally.media_missing, tally.media_changed_during_read),
        (8, 8)
    );
    for index in 0..8 {
        let sent = slots(&serial, &f.session(&format!("session-{index}")));
        let captured: Vec<bool> = sent.iter().map(|slot| slot.1.is_some()).collect();
        assert_eq!(
            captured,
            [true, true, true, false, false],
            "session-{index}"
        );
    }

    // Workers of a real pool, and of the equivalence tests' scheduler, where
    // nearly every step parks.
    let pools = [
        ("pooled", CapturePool::new(3, PoolBudgets::DEFAULT)),
        ("chaos-1", CapturePool::chaos(4, 1)),
        ("chaos-2", CapturePool::chaos(4, 0x5eed)),
    ];
    for (name, pool) in &pools {
        let dir = f.temp.path().join(name);
        let pooled = cold_build(&f, &dir, Some(pool));
        assert_eq!(
            shown(&pooled, &dir.join("projection.sqlite")),
            built,
            "{name}"
        );
        let pooled = verify_ledger(&dir.join("ledger")).unwrap();
        assert_eq!(
            pooled.source_set_digest, verified.source_set_digest,
            "{name}"
        );
        assert_eq!(pooled.tally, verified.tally, "{name}");
    }
}
