//! T6 test 12: a flip is published in R1 D10's order — row effects, then
//! the barrier, then metadata — so a watched session gets exactly one
//! resync barrier, no ring mutation of the flip survives past it, and
//! `/ws/meta` announces the new `stream_state` only after the barrier.

use std::cell::RefCell;

use rosetta_core::IngestOp;
use rosetta_core::rows::{
    MessageKind, MessageRow, ProjectRow, RuntimeRow, SessionRole, SessionRow, StreamState,
};
use rosetta_server::{HubConfig, HubItem, Subscription};
use rosetta_store::hot::Mutation;
use rosetta_store::{Batch, CheckpointUpdate, SourceMode, SourceWrite, queries};

use super::*;

/// What a subscriber saw, in the order the three steps delivered it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Seen {
    SessionMutation,
    Barrier,
    MetaStreamState(String),
    MetaOther,
}

/// The hub, recording after each step what each subscriber received.
struct Probe<'a> {
    hub: &'a Hub,
    session: &'a Subscription,
    meta: &'a Subscription,
    log: RefCell<Vec<Seen>>,
}

impl Probe<'_> {
    fn drain(&self) {
        let mut log = self.log.borrow_mut();
        while let Some(item) = self.session.try_recv() {
            log.push(match item {
                HubItem::Resync => Seen::Barrier,
                HubItem::Mutation { .. } => Seen::SessionMutation,
            });
        }
        while let Some(item) = self.meta.try_recv() {
            log.push(match item {
                HubItem::Mutation { mutation, .. } => match mutation.as_ref() {
                    Mutation::Session(row) if row.id == "s1" => {
                        Seen::MetaStreamState(row.stream_state.as_str().to_owned())
                    }
                    _ => Seen::MetaOther,
                },
                HubItem::Resync => Seen::MetaOther,
            });
        }
    }
}

impl Fanout for Probe<'_> {
    fn absorb_rows(
        &self,
        conn: &rusqlite::Connection,
        effects: rosetta_store::Effects,
        now: i64,
    ) -> Result<usize, String> {
        let fanned = self.hub.absorb_rows(conn, effects, now)?;
        self.drain();
        Ok(fanned)
    }

    fn invalidate(&self, sessions: &[String], now: i64) -> u64 {
        let raised = Fanout::invalidate(self.hub, sessions, now);
        self.drain();
        raised
    }

    fn absorb_meta(
        &self,
        conn: &rusqlite::Connection,
        effects: rosetta_store::MetaEffects,
    ) -> Result<usize, String> {
        let fanned = Fanout::absorb_meta(self.hub, conn, effects)?;
        self.drain();
        Ok(fanned)
    }
}

fn session_row() -> SessionRow {
    SessionRow {
        id: "s1".into(),
        native_session_id: "native-s1".into(),
        runtime_id: "rt1".into(),
        project_id: Some("p1".into()),
        cwd: Some("/w/p".into()),
        parent_session_id: None,
        delegation: None,
        root_session_id: "s1".into(),
        role: SessionRole::Main,
        title: Some("t".into()),
        summary: None,
        started_at: 10,
        updated_at: 10,
        activity: None,
        stream_state: StreamState::Pending,
    }
}

/// A Pending session with one source cursor, its metadata effects observed
/// as the fresh path observes them.
fn flipped_store() -> (Store, Hub) {
    let mut store = Store::open_in_memory().unwrap();
    store.observe_meta_effects();
    store
        .apply(&Batch {
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
                    discovered_at: 1,
                    label: None,
                    provenance: Default::default(),
                }),
                IngestOp::Project(ProjectRow {
                    id: "p1".into(),
                    name: "p".into(),
                    root_paths: vec!["/w/p".into()],
                    git_repo: None,
                }),
                IngestOp::SessionMeta(session_row()),
            ],
            checkpoints: vec![CheckpointUpdate {
                runtime_id: "rt1".into(),
                stream: "transcript".into(),
                path: "p/s1.jsonl".into(),
                session_id: Some("s1".into()),
                expected: None,
                next: "blob-v0".into(),
                updated_at: Some(1),
            }],
            source: None,
        })
        .unwrap();
    let hub = Hub::new(HubConfig::default(), 1);
    hub.warm(store.conn()).unwrap();
    store.drain_effects();
    store.drain_meta_effects();
    (store, hub)
}

/// Land one row of the session and flip it; `rows` arms row effects too
/// (a watched session), else metadata effects only (the fresh path).
fn land_rows_and_flip(store: &mut Store, rows: bool) {
    if rows {
        store.observe_effects();
    }
    let cursor = queries::cursor_state(store.conn(), "rt1", "p/s1.jsonl")
        .unwrap()
        .unwrap()
        .id;
    let row = MessageRow {
        timeline_id: "m1".into(),
        session_id: "s1".into(),
        ordinal: 0,
        kind: MessageKind::User,
        subtype: None,
        uuid: None,
        parent_uuid: None,
        ts: Some(20),
        model: None,
        effort: None,
        content: Some("hello".into()),
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
    };
    store
        .apply(&Batch {
            ops: IngestOp::project_message(row),
            checkpoints: vec![],
            source: Some(SourceWrite {
                cursor_id: cursor,
                session_id: "s1".into(),
                generation: 0,
                first_sequence: 0,
                mode: SourceMode::Append,
            }),
        })
        .unwrap();
    let changed = store
        .set_session_stream_states_changed(&[("s1".into(), StreamState::Indexed)])
        .unwrap();
    assert_eq!(changed, ["s1".to_string()]);
}

#[test]
fn a_flip_reaches_the_hub_rows_then_barrier_then_metadata() {
    // Ring order (row effects armed): the flip's mutations go into the ring
    // before the barrier voids it, so the resync stamp holds none of them.
    let (mut store, hub) = flipped_store();
    let (session, _) = hub.subscribe_session("s1", 0);
    let meta = hub.subscribe_meta();
    land_rows_and_flip(&mut store, true);
    let probe = Probe {
        hub: &hub,
        session: &session,
        meta: &meta,
        log: RefCell::new(Vec::new()),
    };
    absorb_around(&mut store, &probe, &["s1".to_string()], 5).unwrap();
    let barriers = probe
        .log
        .borrow()
        .iter()
        .filter(|seen| **seen == Seen::Barrier)
        .count();
    assert_eq!(barriers, 1, "exactly one barrier: {:?}", probe.log.borrow());
    let stamp = hub.resync(&session, 6).unwrap();
    assert_eq!(
        stamp.seq, 0,
        "no ring mutation of the flip after the barrier"
    );

    // Metadata order (the fresh path arms metadata effects only): the new
    // stream_state reaches /ws/meta after the session socket's barrier.
    let (mut store, hub) = flipped_store();
    let (session, _) = hub.subscribe_session("s1", 0);
    let meta = hub.subscribe_meta();
    land_rows_and_flip(&mut store, false);
    let probe = Probe {
        hub: &hub,
        session: &session,
        meta: &meta,
        log: RefCell::new(Vec::new()),
    };
    absorb_around(&mut store, &probe, &["s1".to_string()], 5).unwrap();
    let log = probe.log.borrow().clone();
    let barrier = log.iter().position(|seen| *seen == Seen::Barrier);
    let announced = log
        .iter()
        .position(|seen| *seen == Seen::MetaStreamState("indexed".into()));
    assert!(
        matches!((barrier, announced), (Some(barrier), Some(announced)) if barrier < announced),
        "{log:?}"
    );
    assert!(!log.contains(&Seen::SessionMutation), "{log:?}");
}
