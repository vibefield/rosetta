//! Hub end-to-end over a real store: subscribe → snapshot stamp → live
//! delivery, ring replay for reconnects, the lagging-subscriber resync
//! barrier, generation swaps forcing replacement snapshots, and
//! mirror-backed meta diffs. Spaghetti's observer semantics, pinned.

use std::time::Duration;

use rosetta_core::IngestOp;
use rosetta_core::rows::{
    MessageKind, MessageRow, ProjectRow, RuntimeRow, SessionRole, SessionRow, StreamState,
};
use rosetta_server::{Hub, HubConfig, HubItem};
use rosetta_store::hot::{HotConfig, Mutation, Replay, Stamp};
use rosetta_store::{Batch, CheckpointUpdate, SourceMode, SourceWrite, Store, queries};

const TICK: Duration = Duration::from_millis(200);

fn base_batch() -> Batch {
    Batch {
        ops: vec![
            IngestOp::Runtime(RuntimeRow {
                id: "rt1".into(),
                kind: "claude-code".into(),
                data_root: "~/.claude".into(),
                enabled: true,
                account: None,
                config: None,
                skills: None,
                versions_seen: None,
                discovered_at: 1_000,
            }),
            IngestOp::Project(ProjectRow {
                id: "p1".into(),
                name: "rosetta".into(),
                root_paths: vec!["/w/rosetta".into()],
                git_repo: None,
            }),
            IngestOp::SessionMeta(session("s1", None)),
        ],
        checkpoints: vec![],
        source: None,
    }
}

fn session(id: &str, title: Option<&str>) -> SessionRow {
    SessionRow {
        id: id.into(),
        native_session_id: format!("native-{id}"),
        runtime_id: "rt1".into(),
        project_id: Some("p1".into()),
        cwd: Some("/w/rosetta".into()),
        parent_session_id: None,
        root_session_id: id.into(),
        role: SessionRole::Main,
        title: title.map(str::to_owned),
        summary: None,
        started_at: 10_000,
        updated_at: 10_000,
        stream_state: StreamState::Streaming,
    }
}

fn message(timeline_id: &str) -> MessageRow {
    MessageRow {
        timeline_id: timeline_id.into(),
        session_id: "s1".into(),
        ordinal: 0,
        kind: MessageKind::User,
        subtype: None,
        uuid: None,
        parent_uuid: None,
        ts: Some(3_600_000),
        model: None,
        effort: None,
        content: Some(format!("content of {timeline_id}")),
        tool: None,
        attachments: Vec::new(),
        citations: Vec::new(),
        group: None,
        response: None,
        block_index: None,
        branch_tool_id: None,
        usage: None,
        agent_version: None,
        src_cursor: None,
        src_offset: None,
    }
}

struct Harness {
    store: Store,
    hub: Hub,
    cursor: i64,
}

fn harness(config: HubConfig) -> Harness {
    let mut store = Store::open_in_memory().unwrap();
    store.observe_effects();
    store.apply(&base_batch()).unwrap();
    store
        .apply(&Batch {
            ops: vec![],
            checkpoints: vec![CheckpointUpdate {
                runtime_id: "rt1".into(),
                stream: "transcript".into(),
                path: "p/x.jsonl".into(),
                session_id: "s1".into(),
                expected: None,
                next: "blob-v0".into(),
                updated_at: Some(1),
            }],
            source: None,
        })
        .unwrap();
    let cursor = queries::cursor_state(store.conn(), "rt1", "p/x.jsonl")
        .unwrap()
        .unwrap()
        .id;
    let hub = Hub::new(config, 1);
    hub.warm(store.conn()).unwrap();
    store.drain_effects();
    Harness { store, hub, cursor }
}

impl Harness {
    fn append(&mut self, timeline_id: &str) {
        self.store
            .apply(&Batch {
                ops: IngestOp::project_message(message(timeline_id)),
                checkpoints: vec![],
                source: Some(SourceWrite {
                    cursor_id: self.cursor,
                    session_id: "s1".into(),
                    generation: 0,
                    first_sequence: 0,
                    mode: SourceMode::Append,
                }),
            })
            .unwrap();
    }

    fn absorb(&mut self, now: i64) -> rosetta_server::AbsorbReport {
        let effects = self.store.drain_effects();
        self.hub.absorb(self.store.conn(), effects, now).unwrap()
    }
}

#[test]
fn subscribe_stamps_then_live_mutations_flow_and_replay() {
    let mut h = harness(HubConfig::default());
    h.append("m1");
    h.absorb(0);

    // Subscribe: the stamp marks the position the DB snapshot covers. m1 was
    // projected before any reader interest, so it lives only in the DB — the
    // ring starts empty, exactly the lazy-rewarm contract.
    let (sub, stamp) = h.hub.subscribe_session("s1", 0);
    assert_eq!(stamp.seq, 0);
    let snapshot: i64 = h
        .store
        .conn()
        .query_row("SELECT COUNT(*) FROM message", [], |row| row.get(0))
        .unwrap();
    assert_eq!(snapshot, 1, "the DB snapshot carries m1");
    assert_eq!(sub.try_recv(), None, "nothing published since subscribing");

    // A live append flows: ring, subscriber queue, and replay all see it.
    h.append("m2");
    let report = h.absorb(1);
    assert_eq!(report.session_mutations, 1);
    match sub.recv_timeout(TICK) {
        Some(HubItem::Mutation { mutation, .. }) => match mutation.as_ref() {
            Mutation::Message(row) => assert_eq!(row.timeline_id, "m2"),
            other => panic!("expected message, got {other:?}"),
        },
        other => panic!("expected a mutation, got {other:?}"),
    }
    match h.hub.replay_session("s1", stamp, 2) {
        Replay::Mutations(items) => assert_eq!(items.len(), 1),
        other => panic!("expected replay, got {other:?}"),
    }
}

#[test]
fn lagging_subscriber_hits_the_barrier_and_resyncs() {
    let mut h = harness(HubConfig {
        subscriber_queue: 2,
        ..HubConfig::default()
    });
    let (sub, _stamp) = h.hub.subscribe_session("s1", 0);

    // Three undelivered mutations against a queue of two: the barrier trips.
    h.append("m1");
    h.append("m2");
    h.append("m3");
    let report = h.absorb(1);
    assert_eq!(report.resyncs, 1);
    assert_eq!(sub.recv_timeout(TICK), Some(HubItem::Resync));
    assert_eq!(sub.try_recv(), None, "the barrier leaves exactly one item");

    // Behind the barrier nothing is delivered.
    h.append("m4");
    h.absorb(2);
    assert_eq!(sub.try_recv(), None);

    // Resync: fresh stamp, replacement snapshot, live delivery resumes.
    let stamp = h.hub.resync(&sub, 3).expect("session topics re-stamp");
    let snapshot: i64 = h
        .store
        .conn()
        .query_row("SELECT COUNT(*) FROM message", [], |row| row.get(0))
        .unwrap();
    assert_eq!(snapshot, 4, "the replacement snapshot has everything");
    h.append("m5");
    h.absorb(4);
    match sub.recv_timeout(TICK) {
        Some(HubItem::Mutation { mutation, .. }) => match mutation.as_ref() {
            Mutation::Message(row) => assert_eq!(row.timeline_id, "m5"),
            other => panic!("expected m5, got {other:?}"),
        },
        other => panic!("expected a mutation, got {other:?}"),
    }
    match h.hub.replay_session("s1", stamp, 5) {
        Replay::Mutations(items) => assert_eq!(items.len(), 1, "m5 only"),
        other => panic!("expected replay from the resync stamp, got {other:?}"),
    }
}

#[test]
fn generation_swap_forces_a_replacement_snapshot() {
    let mut h = harness(HubConfig::default());
    let (sub, stamp) = h.hub.subscribe_session("s1", 0);
    h.append("m1");
    h.absorb(1);
    assert!(matches!(
        sub.recv_timeout(TICK),
        Some(HubItem::Mutation { .. })
    ));

    // The source rewrites: stage a replacement generation and activate it.
    h.store
        .apply(&Batch {
            ops: IngestOp::project_message(message("rewritten")),
            checkpoints: vec![],
            source: Some(SourceWrite {
                cursor_id: h.cursor,
                session_id: "s1".into(),
                generation: 1,
                first_sequence: 0,
                mode: SourceMode::Stage,
            }),
        })
        .unwrap();
    h.store
        .apply(&Batch {
            ops: vec![],
            checkpoints: vec![],
            source: Some(SourceWrite {
                cursor_id: h.cursor,
                session_id: "s1".into(),
                generation: 1,
                first_sequence: 1,
                mode: SourceMode::Activate,
            }),
        })
        .unwrap();
    let report = h.absorb(2);
    assert_eq!(report.resyncs, 1);
    assert_eq!(sub.recv_timeout(TICK), Some(HubItem::Resync));

    // The old identity is dead: replay expires, resync mints a new epoch.
    assert!(matches!(
        h.hub.replay_session("s1", stamp, 3),
        Replay::Expired(_)
    ));
    let fresh = h.hub.resync(&sub, 3).unwrap();
    assert_ne!(fresh.epoch, stamp.epoch);
}

#[test]
fn meta_subscribers_get_precise_diffs_and_mirror_serves_lists() {
    let mut h = harness(HubConfig::default());
    assert_eq!(h.hub.sessions().len(), 1);
    assert_eq!(h.hub.projects().len(), 1);
    assert_eq!(h.hub.runtimes().len(), 1);
    let sub = h.hub.subscribe_meta();

    h.store
        .apply(&Batch {
            ops: vec![IngestOp::SessionMeta(session("s1", Some("Named")))],
            checkpoints: vec![],
            source: None,
        })
        .unwrap();
    let report = h.absorb(1);
    assert_eq!(report.meta_events, 1);
    match sub.recv_timeout(TICK) {
        Some(HubItem::Mutation { mutation, .. }) => match mutation.as_ref() {
            Mutation::Session(row) => assert_eq!(row.title.as_deref(), Some("Named")),
            other => panic!("expected session diff, got {other:?}"),
        },
        other => panic!("expected a mutation, got {other:?}"),
    }
    assert_eq!(
        h.hub.sessions()[0].title.as_deref(),
        Some("Named"),
        "list reads serve from the mirror, not the DB"
    );

    // The identical upsert is a no-op row change: no event at all.
    h.store
        .apply(&Batch {
            ops: vec![IngestOp::SessionMeta(session("s1", Some("Named")))],
            checkpoints: vec![],
            source: None,
        })
        .unwrap();
    let report = h.absorb(2);
    assert_eq!(report.meta_events, 0);
    assert_eq!(sub.try_recv(), None);
}

#[test]
fn idle_rings_evict_but_watched_rings_survive() {
    let mut h = harness(HubConfig {
        hot: HotConfig {
            idle_evict_ms: 1_000,
            ..HotConfig::default()
        },
        ..HubConfig::default()
    });
    // s1 is watched by a live subscriber; "idle" only ever saw a reconnect
    // probe (reader interest with no subscription).
    let (_sub, watched_stamp) = h.hub.subscribe_session("s1", 0);
    let idle_stamp = match h.hub.replay_session("idle", Stamp { epoch: 0, seq: 0 }, 0) {
        Replay::Expired(stamp) => stamp,
        other => panic!("expected expiry for an unknown session, got {other:?}"),
    };

    let report = h.absorb(10_000);
    assert_eq!(report.rings_evicted, 1, "only the unwatched ring drops");
    assert_eq!(
        h.hub.replay_session("s1", watched_stamp, 10_001),
        Replay::Mutations(vec![]),
        "the watched ring's identity survives idling"
    );
    match h.hub.replay_session("idle", idle_stamp, 10_001) {
        Replay::Expired(rewarmed) => assert_ne!(rewarmed.epoch, idle_stamp.epoch),
        other => panic!("expected the evicted ring to expire, got {other:?}"),
    }
}

#[test]
fn watched_sessions_names_exactly_the_open_session_subscriptions() {
    let h = harness(HubConfig::default());
    assert!(
        h.hub.watched_sessions().is_empty(),
        "nothing is watched before anyone subscribes"
    );

    let (open, _stamp) = h.hub.subscribe_session("s1", 0);
    let (closed, _stamp) = h.hub.subscribe_session("s2", 0);
    let _meta = h.hub.subscribe_meta();
    let watched = h.hub.watched_sessions();
    assert_eq!(
        watched.iter().map(String::as_str).collect::<Vec<_>>(),
        ["s1", "s2"],
        "session topics are watched; the meta topic is not a session"
    );

    // A dropped subscriber leaves the watch set: the cold loop must stop
    // paying for a transcript nobody holds open anymore.
    drop(closed);
    let watched = h.hub.watched_sessions();
    assert_eq!(
        watched.iter().map(String::as_str).collect::<Vec<_>>(),
        ["s1"],
        "a closed subscription no longer arms its session"
    );
    drop(open);
}
