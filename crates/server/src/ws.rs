//! WebSocket subscriptions speaking `rosetta-protocol` frames. The protocol
//! is the hub's, made visible: `Hello` stamps the position, one snapshot
//! frame covers everything up to it, mutations stream from there
//! (at-least-once — clients key by `timeline_id`), and a `Resync` frame
//! announces the barrier before a replacement snapshot.
//!
//! The hub is synchronous; each connection runs one bridge thread that
//! blocks on the subscription and forwards items into an async channel. The
//! stream loops are generic over [`FrameSink`] so tests drive them without
//! sockets — the axum upgrade handlers are thin glue.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path as UrlPath, Query, State};
use axum::response::Response;

use rosetta_protocol::{PROTOCOL_VERSION, ServerFrame, Stamp};
use rosetta_store::hot::Replay;
use rosetta_store::queries;

use crate::http::{AppState, Failure, with_read};
use crate::hub::{Hub, HubItem, Subscription};

/// Messages in the initial timeline snapshot; older history pages via REST.
const SNAPSHOT_LIMIT: i64 = 200;
const BRIDGE_POLL: Duration = Duration::from_millis(200);

pub(crate) trait FrameSink: Send {
    /// Deliver one frame; `Err(())` means the client is gone and the stream
    /// loop must end.
    fn send(&mut self, frame: ServerFrame) -> impl Future<Output = Result<(), ()>> + Send;
}

struct WsSink(WebSocket);

impl FrameSink for WsSink {
    async fn send(&mut self, frame: ServerFrame) -> Result<(), ()> {
        let text = serde_json::to_string(&frame).map_err(|_| ())?;
        self.0.send(Message::text(text)).await.map_err(|_| ())
    }
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(0)
}

/// Forward hub items into async land from a dedicated bridge thread. The
/// thread exits when the receiver is dropped; the shared [`Subscription`]
/// stays available to the async side for resync calls.
fn bridge(sub: Arc<Subscription>) -> tokio::sync::mpsc::Receiver<HubItem> {
    let (tx, rx) = tokio::sync::mpsc::channel(64);
    std::thread::spawn(move || {
        loop {
            match sub.recv_timeout(BRIDGE_POLL) {
                Some(item) => {
                    if tx.blocking_send(item).is_err() {
                        break;
                    }
                }
                None => {
                    if tx.is_closed() {
                        break;
                    }
                }
            }
        }
    });
    rx
}

#[derive(serde::Deserialize)]
pub(crate) struct ResumeQuery {
    epoch: Option<u64>,
    seq: Option<u64>,
}

impl ResumeQuery {
    fn stamp(&self) -> Option<Stamp> {
        Some(Stamp {
            epoch: self.epoch?,
            seq: self.seq?,
        })
    }
}

pub(crate) async fn session(
    State(state): State<AppState>,
    UrlPath(id): UrlPath<String>,
    Query(resume): Query<ResumeQuery>,
    upgrade: WebSocketUpgrade,
) -> Response {
    upgrade.on_upgrade(move |socket| async move {
        let mut sink = WsSink(socket);
        run_session_stream(state, id, resume.stamp(), &mut sink).await;
    })
}

pub(crate) async fn meta(State(state): State<AppState>, upgrade: WebSocketUpgrade) -> Response {
    upgrade.on_upgrade(move |socket| async move {
        let mut sink = WsSink(socket);
        run_meta_stream(state.hub, &mut sink).await;
    })
}

async fn timeline_snapshot(
    db: Arc<PathBuf>,
    session_id: String,
    stamp: Stamp,
) -> Result<ServerFrame, Failure> {
    with_read(db, move |conn| {
        let messages = queries::timeline_page(conn, &session_id, None, SNAPSHOT_LIMIT)
            .map_err(|error| Failure::internal(format!("{error}")))?;
        Ok(ServerFrame::TimelineSnapshot { stamp, messages })
    })
    .await
}

/// One session subscription: hello, snapshot-or-replay, then live items
/// until the client disconnects.
pub(crate) async fn run_session_stream<S: FrameSink>(
    state: AppState,
    session_id: String,
    resume: Option<Stamp>,
    sink: &mut S,
) {
    let (sub, stamp) = state.hub.subscribe_session(&session_id, now_ms());
    let sub = Arc::new(sub);
    if sink
        .send(ServerFrame::Hello {
            protocol_version: PROTOCOL_VERSION,
            stamp: Some(stamp),
        })
        .await
        .is_err()
    {
        return;
    }

    // A resumable stamp replays the ring gap; anything else takes a full
    // snapshot. Replayed mutations carry no stamp — `Hello`'s stamp is the
    // client's position once the replay is drained.
    let mut replayed = false;
    if let Some(resume) = resume
        && let Replay::Mutations(items) = state.hub.replay_session(&session_id, resume, now_ms())
    {
        for mutation in items {
            let frame = ServerFrame::Mutation {
                stamp: None,
                mutation: (*mutation).clone(),
            };
            if sink.send(frame).await.is_err() {
                return;
            }
        }
        replayed = true;
    }
    if !replayed {
        match timeline_snapshot(state.db.clone(), session_id.clone(), stamp).await {
            Ok(frame) => {
                if sink.send(frame).await.is_err() {
                    return;
                }
            }
            Err(_) => {
                let _ = sink
                    .send(ServerFrame::Error {
                        error: rosetta_protocol::ApiError {
                            code: rosetta_protocol::ErrorCode::Internal,
                            message: "snapshot read failed".into(),
                        },
                    })
                    .await;
                return;
            }
        }
    }

    let mut items = bridge(sub.clone());
    while let Some(item) = items.recv().await {
        match item {
            HubItem::Mutation { stamp, mutation } => {
                let frame = ServerFrame::Mutation {
                    stamp,
                    mutation: (*mutation).clone(),
                };
                if sink.send(frame).await.is_err() {
                    return;
                }
            }
            HubItem::Resync => {
                if sink.send(ServerFrame::Resync).await.is_err() {
                    return;
                }
                let Some(stamp) = state.hub.resync(&sub, now_ms()) else {
                    return;
                };
                match timeline_snapshot(state.db.clone(), session_id.clone(), stamp).await {
                    Ok(frame) => {
                        if sink.send(frame).await.is_err() {
                            return;
                        }
                    }
                    Err(_) => return,
                }
            }
        }
    }
}

fn meta_snapshot(hub: &Hub) -> ServerFrame {
    let mut runtimes = hub.runtimes();
    runtimes.sort_by(|a, b| a.id.cmp(&b.id));
    let mut projects = hub.projects();
    projects.sort_by(|a, b| a.id.cmp(&b.id));
    let mut sessions = hub.sessions();
    sessions.sort_by(|a, b| {
        b.updated_at
            .cmp(&a.updated_at)
            .then_with(|| a.id.cmp(&b.id))
    });
    ServerFrame::MetaSnapshot {
        runtimes,
        projects,
        sessions,
    }
}

/// The metadata subscription: hello, the mirror as one snapshot, then
/// precise diffs. Meta has no ring, so resync just re-reads the mirror.
pub(crate) async fn run_meta_stream<S: FrameSink>(hub: Arc<Hub>, sink: &mut S) {
    let sub = Arc::new(hub.subscribe_meta());
    if sink
        .send(ServerFrame::Hello {
            protocol_version: PROTOCOL_VERSION,
            stamp: None,
        })
        .await
        .is_err()
    {
        return;
    }
    if sink.send(meta_snapshot(&hub)).await.is_err() {
        return;
    }

    let mut items = bridge(sub.clone());
    while let Some(item) = items.recv().await {
        match item {
            HubItem::Mutation { stamp, mutation } => {
                let frame = ServerFrame::Mutation {
                    stamp,
                    mutation: (*mutation).clone(),
                };
                if sink.send(frame).await.is_err() {
                    return;
                }
            }
            HubItem::Resync => {
                if sink.send(ServerFrame::Resync).await.is_err() {
                    return;
                }
                hub.resync(&sub, now_ms());
                if sink.send(meta_snapshot(&hub)).await.is_err() {
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    use rosetta_core::IngestOp;
    use rosetta_core::rows::{
        MessageKind, MessageRow, ProjectRow, RuntimeRow, SessionRole, SessionRow, StreamState,
    };
    use rosetta_store::hot::Mutation;
    use rosetta_store::{Batch, CheckpointUpdate, SourceMode, SourceWrite, Store, queries};

    use crate::hub::HubConfig;

    #[derive(Clone, Default)]
    struct RecordingSink {
        frames: Arc<Mutex<Vec<ServerFrame>>>,
    }

    impl FrameSink for RecordingSink {
        async fn send(&mut self, frame: ServerFrame) -> Result<(), ()> {
            self.frames.lock().unwrap().push(frame);
            Ok(())
        }
    }

    async fn wait_for_frames(frames: &Arc<Mutex<Vec<ServerFrame>>>, count: usize) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if frames.lock().unwrap().len() >= count {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("frames arrived in time");
    }

    struct Harness {
        _dir: tempfile::TempDir,
        store: Store,
        cursor: i64,
        state: AppState,
    }

    fn harness() -> Harness {
        let dir = tempfile::TempDir::new().unwrap();
        let db = dir.path().join("projection.sqlite");
        let mut store = Store::open(&db).unwrap();
        store.observe_effects();
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
                        discovered_at: 1_000,
                    }),
                    IngestOp::Project(ProjectRow {
                        id: "p1".into(),
                        name: "rosetta".into(),
                        root_paths: vec!["/w/rosetta".into()],
                        git_repo: None,
                    }),
                    IngestOp::SessionMeta(SessionRow {
                        id: "s1".into(),
                        native_session_id: "native-s1".into(),
                        runtime_id: "rt1".into(),
                        project_id: Some("p1".into()),
                        parent_session_id: None,
                        root_session_id: "s1".into(),
                        role: SessionRole::Main,
                        title: None,
                        summary: None,
                        started_at: 10_000,
                        updated_at: 10_000,
                        stream_state: StreamState::Streaming,
                    }),
                ],
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
        let hub = Arc::new(Hub::new(HubConfig::default(), 1));
        hub.warm(store.conn()).unwrap();
        store.drain_effects();
        let state = AppState {
            hub,
            db: Arc::new(db),
        };
        Harness {
            _dir: dir,
            store,
            cursor,
            state,
        }
    }

    impl Harness {
        fn append(&mut self, timeline_id: &str) {
            self.store
                .apply(&Batch {
                    ops: vec![IngestOp::Message(Box::new(MessageRow {
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
                        branch_tool_id: None,
                        usage: None,
                        agent_version: None,
                        // Real pipeline rows always carry provenance; the
                        // activation sweep deletes by it.
                        src_cursor: Some(self.cursor),
                        src_offset: None,
                    }))],
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

        fn absorb(&mut self) {
            let effects = self.store.drain_effects();
            self.state
                .hub
                .absorb(self.store.conn(), effects, now_ms())
                .unwrap();
        }
    }

    #[tokio::test]
    async fn session_stream_snapshots_then_streams_and_resyncs() {
        let mut h = harness();
        h.append("m1");
        h.absorb();

        let sink = RecordingSink::default();
        let frames = sink.frames.clone();
        let state = h.state.clone();
        let task = tokio::spawn(async move {
            let mut sink = sink;
            run_session_stream(state, "s1".into(), None, &mut sink).await;
        });

        wait_for_frames(&frames, 2).await;
        {
            let seen = frames.lock().unwrap();
            assert!(matches!(seen[0], ServerFrame::Hello { stamp: Some(_), .. }));
            match &seen[1] {
                ServerFrame::TimelineSnapshot { messages, .. } => {
                    assert_eq!(messages.len(), 1, "snapshot covers the pre-subscribe row");
                }
                other => panic!("expected snapshot, got {other:?}"),
            }
        }

        h.append("m2");
        h.absorb();
        wait_for_frames(&frames, 3).await;
        {
            let seen = frames.lock().unwrap();
            match &seen[2] {
                ServerFrame::Mutation {
                    stamp: Some(stamp),
                    mutation: Mutation::Message(row),
                } => {
                    assert_eq!(row.timeline_id, "m2");
                    assert_eq!(stamp.seq, 1);
                }
                other => panic!("expected stamped live mutation, got {other:?}"),
            }
        }

        // A generation swap raises the barrier: Resync, then a replacement
        // snapshot of the renumbered session.
        h.store
            .apply(&Batch {
                ops: vec![],
                checkpoints: vec![],
                source: Some(SourceWrite {
                    cursor_id: h.cursor,
                    session_id: "s1".into(),
                    generation: 1,
                    first_sequence: 0,
                    mode: SourceMode::Activate,
                }),
            })
            .unwrap();
        h.absorb();
        wait_for_frames(&frames, 5).await;
        {
            let seen = frames.lock().unwrap();
            assert!(matches!(seen[3], ServerFrame::Resync));
            match &seen[4] {
                ServerFrame::TimelineSnapshot { messages, .. } => {
                    assert!(messages.is_empty(), "the empty activation swept the rows");
                }
                other => panic!("expected replacement snapshot, got {other:?}"),
            }
        }
        task.abort();
    }

    #[tokio::test]
    async fn resumable_stamps_replay_instead_of_snapshotting() {
        let mut h = harness();

        // First subscriber creates the ring, then observes one mutation.
        let sink = RecordingSink::default();
        let frames = sink.frames.clone();
        let state = h.state.clone();
        let task = tokio::spawn(async move {
            let mut sink = sink;
            run_session_stream(state, "s1".into(), None, &mut sink).await;
        });
        wait_for_frames(&frames, 2).await;
        h.append("m1");
        h.absorb();
        wait_for_frames(&frames, 3).await;
        let resume = {
            let seen = frames.lock().unwrap();
            match &seen[0] {
                ServerFrame::Hello { stamp, .. } => stamp.unwrap(),
                other => panic!("expected hello, got {other:?}"),
            }
        };
        task.abort();

        // More history lands while the client is away.
        h.append("m2");
        h.absorb();

        // The reconnect replays the gap — no snapshot frame.
        let sink = RecordingSink::default();
        let frames = sink.frames.clone();
        let state = h.state.clone();
        let task = tokio::spawn(async move {
            let mut sink = sink;
            run_session_stream(state, "s1".into(), Some(resume), &mut sink).await;
        });
        wait_for_frames(&frames, 3).await;
        {
            let seen = frames.lock().unwrap();
            assert!(matches!(seen[0], ServerFrame::Hello { .. }));
            let replayed: Vec<&str> = seen[1..]
                .iter()
                .map(|frame| match frame {
                    ServerFrame::Mutation {
                        mutation: Mutation::Message(row),
                        ..
                    } => row.timeline_id.as_str(),
                    other => panic!("expected replayed mutations, got {other:?}"),
                })
                .collect();
            assert_eq!(replayed, vec!["m1", "m2"]);
        }
        task.abort();
    }

    #[tokio::test]
    async fn meta_stream_serves_the_mirror_then_precise_diffs() {
        let mut h = harness();
        let sink = RecordingSink::default();
        let frames = sink.frames.clone();
        let hub = h.state.hub.clone();
        let task = tokio::spawn(async move {
            let mut sink = sink;
            run_meta_stream(hub, &mut sink).await;
        });
        wait_for_frames(&frames, 2).await;
        {
            let seen = frames.lock().unwrap();
            match &seen[1] {
                ServerFrame::MetaSnapshot {
                    runtimes,
                    projects,
                    sessions,
                } => {
                    assert_eq!(runtimes.len(), 1);
                    assert_eq!(projects.len(), 1);
                    assert_eq!(sessions.len(), 1);
                }
                other => panic!("expected meta snapshot, got {other:?}"),
            }
        }

        // A title change reaches meta subscribers as one precise diff.
        let mut titled = h.state.hub.session("s1").unwrap();
        titled.title = Some("Named".into());
        h.store
            .apply(&Batch {
                ops: vec![IngestOp::SessionMeta(titled)],
                checkpoints: vec![],
                source: None,
            })
            .unwrap();
        h.absorb();
        wait_for_frames(&frames, 3).await;
        {
            let seen = frames.lock().unwrap();
            match &seen[2] {
                ServerFrame::Mutation {
                    stamp: None,
                    mutation: Mutation::Session(row),
                } => assert_eq!(row.title.as_deref(), Some("Named")),
                other => panic!("expected session diff, got {other:?}"),
            }
        }
        task.abort();
    }
}
