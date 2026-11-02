use super::*;

fn store() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE session (id TEXT PRIMARY KEY, started_at INTEGER NOT NULL);
         INSERT INTO session VALUES ('main', 100), ('sub', 200), ('sub2', 200);",
    )
    .unwrap();
    conn
}

fn stamp(model: Option<&str>, ts: Option<Ms>) -> RowStamp<&str> {
    RowStamp { model, ts }
}

fn own(claim: Claim) -> OwnerSlot {
    match claim {
        Claim::Own(slot) => slot,
        other => panic!("expected Own, got {other:?}"),
    }
}

/// Recompute the variable heap the index tracks incrementally.
fn recount(owners: &UsageOwners) -> usize {
    let keys: usize = owners.index.keys().map(|key| key.len()).sum();
    let sessions: usize = owners.sessions.iter().map(|id| 2 * id.len()).sum();
    let models: usize = owners.models.iter().map(|model| 2 * model.len()).sum();
    let touched: usize = owners
        .owners
        .iter()
        .map(|owner| match &owner.touched {
            TouchedSet::Empty => 0,
            TouchedSet::One(touched) => touched.heap_bytes(),
            TouchedSet::Many(all) => all
                .iter()
                .map(|touched| touched.heap_bytes() + size_of::<Touched>())
                .sum(),
        })
        .sum();
    keys + sessions + models + touched
}

#[test]
fn the_earliest_started_session_owns_a_response_whatever_the_arrival_order() {
    let conn = store();
    let mut owners = UsageOwners::default();
    // The copy lands first, then the origin displaces it.
    let copy = own(owners.claim(&conn, "m1:r1", "sub").unwrap());
    owners.touch_row(
        copy,
        "s-1",
        stamp(Some("m"), Some(5)),
        stamp(Some("m"), Some(5)),
    );
    assert_eq!(
        owners.claim(&conn, "m1:r1", "main").unwrap(),
        Claim::Displace {
            slot: copy,
            session_id: "sub".to_owned(),
            undo: vec![Undo::Row {
                timeline_id: "s-1".to_owned(),
                restore: None,
            }],
        }
    );
    assert_eq!(owners.owner_of("m1:r1"), Some("main"));
    // A later copy of an owned response is dropped; the owner's own
    // records still apply.
    assert_eq!(
        owners.claim(&conn, "m1:r1", "sub2").unwrap(),
        Claim::DropCopy
    );
    assert_eq!(
        owners.claim(&conn, "m1:r1", "main").unwrap(),
        Claim::Own(copy)
    );
    // Equal start times fall to the session id.
    let later = own(owners.claim(&conn, "m2:r2", "sub2").unwrap());
    assert!(matches!(
        owners.claim(&conn, "m2:r2", "sub").unwrap(),
        Claim::Displace { slot, .. } if slot == later
    ));
    // An unknown session (no metadata yet) never displaces a dated one.
    assert_eq!(
        owners.claim(&conn, "m2:r2", "ghost").unwrap(),
        Claim::DropCopy
    );
    assert_eq!(recount(&owners), owners.heap_bytes);
}

#[test]
fn a_displacement_undoes_every_target_the_owner_wrote_and_starts_a_new_set() {
    let conn = store();
    let mut owners = UsageOwners::default();
    let slot = own(owners.claim(&conn, "m1:r1", "sub").unwrap());
    // A response sample, then the fact moves onto its row (filling the
    // row's model), the sample is retracted, and two iteration samples land.
    owners.touch_sample(slot, "response:m1");
    owners.touch_row(
        slot,
        "s-1",
        stamp(None, Some(5)),
        stamp(Some("opus"), Some(5)),
    );
    owners.touch_sample(slot, "response:m1");
    owners.touch_sample(slot, "response:m1:iteration:1");
    owners.touch_sample(slot, "response:m1:iteration:2");
    assert_eq!(recount(&owners), owners.heap_bytes);
    let Claim::Displace {
        slot: displaced,
        session_id,
        undo,
    } = owners.claim(&conn, "m1:r1", "main").unwrap()
    else {
        panic!("the origin displaces the copy");
    };
    assert_eq!((displaced, session_id.as_str()), (slot, "sub"));
    assert_eq!(
        undo,
        vec![
            Undo::Sample {
                key: "response:m1".into()
            },
            Undo::Row {
                timeline_id: "s-1".into(),
                restore: Some((
                    RowStamp {
                        model: None,
                        ts: Some(5)
                    },
                    RowStamp {
                        model: Some("opus".into()),
                        ts: Some(5)
                    },
                )),
            },
            Undo::Sample {
                key: "response:m1:iteration:1".into()
            },
            Undo::Sample {
                key: "response:m1:iteration:2".into()
            },
        ]
    );
    // The new owner's set starts empty: a second displacement by an even
    // earlier session undoes only what the new owner wrote since.
    owners.touch_row(
        slot,
        "t-1",
        stamp(Some("opus"), Some(5)),
        stamp(Some("opus"), Some(5)),
    );
    conn.execute("INSERT INTO session VALUES ('origin', 50)", [])
        .unwrap();
    let Claim::Displace {
        session_id, undo, ..
    } = owners.claim(&conn, "m1:r1", "origin").unwrap()
    else {
        panic!("the earlier origin displaces");
    };
    assert_eq!(session_id, "main");
    assert_eq!(
        undo,
        vec![Undo::Row {
            timeline_id: "t-1".into(),
            restore: None
        }]
    );
    assert_eq!(recount(&owners), owners.heap_bytes);
}

#[test]
fn a_row_prior_is_kept_only_while_the_usage_changed_the_row() {
    let conn = store();
    let mut owners = UsageOwners::default();
    let slot = own(owners.claim(&conn, "m1:r1", "sub").unwrap());
    // Usage that changes nothing keeps no prior.
    owners.touch_row(
        slot,
        "s-1",
        stamp(Some("m"), Some(5)),
        stamp(Some("m"), Some(5)),
    );
    assert!(matches!(
        &owners.owners[0].touched,
        TouchedSet::One(Touched::Row { restore: None, .. })
    ));
    // A correction that fills the time remembers the row's prior stamp; a
    // further correction keeps it while the row still shows what the owner
    // wrote.
    owners.touch_row(
        slot,
        "s-1",
        stamp(Some("m"), Some(5)),
        stamp(Some("n"), Some(5)),
    );
    owners.touch_row(
        slot,
        "s-1",
        stamp(Some("n"), Some(5)),
        stamp(Some("o"), Some(5)),
    );
    assert_eq!(recount(&owners), owners.heap_bytes);
    // A message upsert rewrote the stamp in between: the rewritten stamp is
    // what a dropped copy shows, so it becomes the prior.
    owners.touch_row(
        slot,
        "s-1",
        stamp(Some("x"), Some(9)),
        stamp(Some("o"), Some(9)),
    );
    assert_eq!(recount(&owners), owners.heap_bytes);
    let Claim::Displace { undo, .. } = owners.claim(&conn, "m1:r1", "main").unwrap() else {
        panic!("the origin displaces the copy");
    };
    assert_eq!(
        undo,
        vec![Undo::Row {
            timeline_id: "s-1".into(),
            restore: Some((
                RowStamp {
                    model: Some("x".into()),
                    ts: Some(9)
                },
                RowStamp {
                    model: Some("o".into()),
                    ts: Some(9)
                },
            )),
        }]
    );
    // Writing the prior back drops the record again.
    let mut owners = UsageOwners::default();
    let slot = own(owners.claim(&conn, "m1:r1", "sub").unwrap());
    owners.touch_row(slot, "s-1", stamp(None, None), stamp(Some("m"), Some(5)));
    owners.touch_row(slot, "s-1", stamp(Some("m"), Some(5)), stamp(None, None));
    assert!(matches!(
        &owners.owners[0].touched,
        TouchedSet::One(Touched::Row { restore: None, .. })
    ));
    assert_eq!(recount(&owners), owners.heap_bytes);
    // Model names are interned once, however many priors name them.
    for index in 0..8 {
        let slot = own(owners.claim(&conn, &format!("m{index}:r"), "sub").unwrap());
        owners.touch_row(
            slot,
            &format!("row-{index}"),
            stamp(Some("opus"), None),
            stamp(Some("opus"), Some(index)),
        );
    }
    assert_eq!(owners.models.len(), 2);
    assert_eq!(recount(&owners), owners.heap_bytes);
    assert!(owners.approx_bytes() > owners.heap_bytes as u64);
}

#[test]
fn an_owner_whose_session_row_lands_later_ranks_by_it() {
    let conn = store();
    let mut owners = UsageOwners::default();
    // The early session's record arrives before its metadata row.
    own(owners.claim(&conn, "m1:r1", "early").unwrap());
    conn.execute("INSERT INTO session VALUES ('early', 1)", [])
        .unwrap();
    // A later-started session's copy no longer outranks it.
    assert_eq!(
        owners.claim(&conn, "m1:r1", "main").unwrap(),
        Claim::DropCopy
    );
    assert_eq!(owners.owner_of("m1:r1"), Some("early"));
}

#[test]
fn the_common_owner_stays_as_small_as_a_single_target() {
    // One identity, one touched row: the inline slot, no vector. The
    // per-owner record is what the cold-build peak pays per identity.
    assert!(size_of::<Owner>() <= 40, "{}", size_of::<Owner>());
    assert!(size_of::<Touched>() <= 24, "{}", size_of::<Touched>());
}
