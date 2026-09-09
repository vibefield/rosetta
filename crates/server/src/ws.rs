//! WebSocket subscriptions speaking `rosetta-protocol` frames. The protocol
//! is the hub's, made visible: `Hello` stamps the position, one snapshot
//! frame covers everything up to it, mutations stream from there
//! (at-least-once — clients key by `timeline_id`), and a `Resync` frame
//! announces the barrier before a replacement snapshot.
//!
//! The synchronous hub wakes async subscribers directly through Notify. The
//! stream loops are generic over [`FrameSink`] so tests drive them without
//! sockets — the axum upgrade handlers are thin glue.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path as UrlPath, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use futures_util::stream::{SplitSink, SplitStream, StreamExt};

use rosetta_protocol::{PROTOCOL_VERSION, ServerFrame, Stamp};
use rosetta_store::hot::{Mutation, Replay};
use rosetta_store::{queries, timeline_meta};

use crate::http::{
    AppState, Failure, ProjectionReader, origin_allowed, read_timeline_page, with_read,
};
use crate::hub::{Hub, HubItem, Subscription};

/// Messages in the initial timeline snapshot; older history pages via REST.
const SNAPSHOT_LIMIT: i64 = 200;
/// Idle interval before the server pings. A vanished client fails the ping
/// (or a later send) instead of holding its subscription forever. Unit
/// tests compile the short interval so liveness is provable in real time.
const KEEPALIVE: Duration = if cfg!(test) {
    Duration::from_millis(150)
} else {
    Duration::from_secs(20)
};

pub(crate) trait FrameSink: Send {
    /// Deliver one frame; `Err(())` means the client is gone and the stream
    /// loop must end.
    fn send(&mut self, frame: ServerFrame) -> impl Future<Output = Result<(), ()>> + Send;

    /// Transport-level liveness probe on an idle stream. `Err(())` ends the
    /// loop like a failed send.
    fn ping(&mut self) -> impl Future<Output = Result<(), ()>> + Send;
}

struct WsSink(SplitSink<WebSocket, Message>);

impl FrameSink for WsSink {
    async fn send(&mut self, frame: ServerFrame) -> Result<(), ()> {
        use futures_util::SinkExt;
        let text = serde_json::to_string(&frame).map_err(|_| ())?;
        self.0.send(Message::text(text)).await.map_err(|_| ())
    }

    async fn ping(&mut self) -> Result<(), ()> {
        use futures_util::SinkExt;
        self.0
            .send(Message::Ping(axum::body::Bytes::new()))
            .await
            .map_err(|_| ())
    }
}

/// Consume the client half until it closes or errors. Reading is what lets
/// tungstenite answer client pings and observe Close frames — a push-only
/// socket never notices a polite disconnect.
async fn drain_until_close(mut receiver: SplitStream<WebSocket>) {
    while let Some(Ok(message)) = receiver.next().await {
        if matches!(message, Message::Close(_)) {
            break;
        }
    }
}

/// Anti cross-site-WebSocket-hijacking: browsers do not apply CORS to WS,
/// so this route layer enforces the same origin policy the REST layer uses,
/// BEFORE upgrade extraction. Native clients send no Origin and pass.
pub(crate) async fn require_local_origin(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    if let Some(origin) = request.headers().get(header::ORIGIN)
        && !origin_allowed(origin)
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    next.run(request).await
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(0)
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
        let (sender, receiver) = socket.split();
        let mut sink = WsSink(sender);
        // Either side ending tears the connection down: a Close/error on the
        // read half cancels the stream, dropping the subscription so the hub
        // prunes it.
        tokio::select! {
            _ = run_session_stream(state, id, resume.stamp(), &mut sink) => {}
            _ = drain_until_close(receiver) => {}
        }
    })
}

pub(crate) async fn meta(State(state): State<AppState>, upgrade: WebSocketUpgrade) -> Response {
    upgrade.on_upgrade(move |socket| async move {
        let (sender, receiver) = socket.split();
        let mut sink = WsSink(sender);
        tokio::select! {
            _ = run_meta_stream(state.hub, &mut sink) => {}
            _ = drain_until_close(receiver) => {}
        }
    })
}

pub(crate) async fn collaboration(
    State(state): State<AppState>,
    UrlPath(id): UrlPath<String>,
    upgrade: WebSocketUpgrade,
) -> Response {
    upgrade.on_upgrade(move |socket| async move {
        let (sender, receiver) = socket.split();
        let mut sink = WsSink(sender);
        tokio::select! {
            _ = run_collaboration_stream(state, id, &mut sink) => {}
            _ = drain_until_close(receiver) => {}
        }
    })
}

async fn collaboration_snapshot(state: &AppState, id: String) -> Result<ServerFrame, Failure> {
    let hide_pending = !state.projection.is_published();
    with_read(state.projection.clone(), move |conn| {
        let session =
            queries::session(conn, &id).map_err(|error| Failure::internal(error.to_string()))?;
        let hidden = hide_pending
            && session
                .is_some_and(|row| row.stream_state == rosetta_core::rows::StreamState::Pending);
        let (entries, response_runtimes) = if hidden {
            (vec![], vec![])
        } else {
            queries::collaboration_timeline(conn, &id)
                .map_err(|error| Failure::internal(error.to_string()))?
        };
        Ok(ServerFrame::CollaborationSnapshot {
            entries,
            response_runtimes,
        })
    })
    .await
}

fn runtime_frame(row: &rosetta_core::TimelineEntry) -> Option<ServerFrame> {
    if row.model.is_none() && row.effort.is_none() {
        return None;
    }
    Some(ServerFrame::ResponseRuntime {
        observation: rosetta_protocol::ResponseRuntime {
            response_id: row.response.as_ref()?.id.clone(),
            model: row.model.clone(),
            effort: row.effort.clone(),
        },
    })
}

/// A light, complete conversation projection. No private row or timeline-meta
/// body is serialized; the hub filters irrelevant tool updates before queuing.
pub(crate) async fn run_collaboration_stream<S: FrameSink>(
    state: AppState,
    id: String,
    sink: &mut S,
) {
    let sub = state.hub.subscribe_collaboration(&id);
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
    let Ok(frame) = collaboration_snapshot(&state, id.clone()).await else {
        return;
    };
    if sink.send(frame).await.is_err() {
        return;
    }
    loop {
        match next_or_ping(&sub, sink).await {
            ItemOrEnd::End => return,
            ItemOrEnd::Item(HubItem::Resync) => {
                if sink.send(ServerFrame::Resync).await.is_err() {
                    return;
                }
                state.hub.resync(&sub, now_ms());
                let Ok(frame) = collaboration_snapshot(&state, id.clone()).await else {
                    return;
                };
                if sink.send(frame).await.is_err() {
                    return;
                }
            }
            ItemOrEnd::Item(HubItem::Mutation { mutation, .. }) => {
                let observation = match &*mutation {
                    Mutation::Message(row) => {
                        if matches!(row.body, rosetta_core::TimelineBody::Collaboration { .. })
                            && sink
                                .send(ServerFrame::Mutation {
                                    stamp: None,
                                    mutation: (*mutation).clone(),
                                })
                                .await
                                .is_err()
                        {
                            return;
                        }
                        runtime_frame(row)
                    }
                    Mutation::Usage { timeline_id, .. } => {
                        let session_id = id.clone();
                        let timeline_id = timeline_id.clone();
                        let Ok(observation) = with_read(state.projection.clone(), move |conn| {
                            queries::response_runtime(conn, &session_id, &timeline_id)
                                .map_err(|error| Failure::internal(error.to_string()))
                        })
                        .await
                        else {
                            return;
                        };
                        observation.map(|observation| ServerFrame::ResponseRuntime { observation })
                    }
                    _ => None,
                };
                if let Some(frame) = observation
                    && sink.send(frame).await.is_err()
                {
                    return;
                }
            }
        }
    }
}

async fn timeline_snapshot(
    projection: Arc<ProjectionReader>,
    session_id: String,
    stamp: Stamp,
) -> Result<ServerFrame, Failure> {
    let hide_pending = !projection.is_published();
    with_read(projection, move |conn| {
        let session = queries::session(conn, &session_id)
            .map_err(|error| Failure::internal(format!("{error}")))?;
        let hidden = hide_pending
            && session
                .is_some_and(|row| row.stream_state == rosetta_core::rows::StreamState::Pending);
        let page = read_timeline_page(conn, &session_id, SNAPSHOT_LIMIT, None, hide_pending)?;
        let timeline_meta = if hidden {
            timeline_meta::TimelineMeta::default()
        } else {
            timeline_meta::read(conn, &session_id)
                .map_err(|error| Failure::internal(format!("{error}")))?
        };
        Ok(ServerFrame::TimelineSnapshot {
            stamp,
            entries: page.entries,
            timeline_meta,
            cursor: page.cursor,
            watermark: page.watermark,
        })
    })
    .await
}

async fn timeline_meta_snapshot(
    projection: Arc<ProjectionReader>,
    session_id: String,
    stamp: Option<Stamp>,
) -> Result<ServerFrame, Failure> {
    let hide_pending = !projection.is_published();
    with_read(projection, move |conn| {
        let session = queries::session(conn, &session_id)
            .map_err(|error| Failure::internal(format!("{error}")))?;
        let hidden = hide_pending
            && session
                .is_some_and(|row| row.stream_state == rosetta_core::rows::StreamState::Pending);
        let timeline_meta = if hidden {
            timeline_meta::TimelineMeta::default()
        } else {
            timeline_meta::read(conn, &session_id)
                .map_err(|error| Failure::internal(format!("{error}")))?
        };
        Ok(ServerFrame::TimelineMetaSnapshot {
            stamp,
            timeline_meta,
        })
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
    let (sub, stamp, replay) = state.hub.subscribe_resuming(&session_id, resume, now_ms());
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
    // snapshot. Unstamped replay commits only at CaughtUp, after all data
    // and metadata were delivered. An interrupted replay is safely repeated.
    let mut replayed = false;
    if let Some(Replay::Mutations(items)) = replay {
        let mut refresh_timeline_meta = false;
        for mutation in items {
            if matches!(&*mutation, Mutation::TimelineMetaChanged { .. }) {
                refresh_timeline_meta = true;
                continue;
            }
            let frame = ServerFrame::Mutation {
                stamp: None,
                mutation: (*mutation).clone(),
            };
            if sink.send(frame).await.is_err() {
                return;
            }
        }
        if refresh_timeline_meta {
            match timeline_meta_snapshot(state.projection.clone(), session_id.clone(), None).await {
                Ok(frame) => {
                    if sink.send(frame).await.is_err() {
                        return;
                    }
                }
                Err(_) => return,
            }
        }
        if sink.send(ServerFrame::CaughtUp { stamp }).await.is_err() {
            return;
        }
        replayed = true;
    }
    if !replayed {
        match timeline_snapshot(state.projection.clone(), session_id.clone(), stamp).await {
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

    loop {
        let item = match next_or_ping(&sub, sink).await {
            ItemOrEnd::Item(item) => item,
            ItemOrEnd::End => return,
        };
        match item {
            HubItem::Mutation { stamp, mutation } => {
                if matches!(&*mutation, Mutation::TimelineMetaChanged { .. }) {
                    match timeline_meta_snapshot(
                        state.projection.clone(),
                        session_id.clone(),
                        stamp,
                    )
                    .await
                    {
                        Ok(frame) => {
                            if sink.send(frame).await.is_err() {
                                return;
                            }
                        }
                        Err(_) => return,
                    }
                    continue;
                }
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
                match timeline_snapshot(state.projection.clone(), session_id.clone(), stamp).await {
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

enum ItemOrEnd {
    Item(HubItem),
    End,
}

/// Wait for the next hub item, pinging on idle so a dead transport surfaces
/// within one keepalive interval instead of never.
async fn next_or_ping<S: FrameSink>(sub: &Subscription, sink: &mut S) -> ItemOrEnd {
    loop {
        match tokio::time::timeout(KEEPALIVE, sub.recv()).await {
            Ok(item) => return ItemOrEnd::Item(item),
            Err(_idle) => {
                if sink.ping().await.is_err() {
                    return ItemOrEnd::End;
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

    loop {
        let item = match next_or_ping(&sub, sink).await {
            ItemOrEnd::Item(item) => item,
            ItemOrEnd::End => return,
        };
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
        MessageKind, MessageRow, PlanRow, ProjectRow, RuntimeRow, SessionRole, SessionRow,
        StreamState,
    };
    use rosetta_store::hot::Mutation;
    use rosetta_store::{Batch, CheckpointUpdate, SourceMode, SourceWrite, Store, queries};

    use crate::hub::HubConfig;

    #[derive(Clone, Default)]
    struct RecordingSink {
        frames: Arc<Mutex<Vec<ServerFrame>>>,
        pings: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl FrameSink for RecordingSink {
        async fn send(&mut self, frame: ServerFrame) -> Result<(), ()> {
            self.frames.lock().unwrap().push(frame);
            Ok(())
        }

        async fn ping(&mut self) -> Result<(), ()> {
            self.pings.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
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
                        cwd: Some("/w/rosetta".into()),
                        parent_session_id: None,
                        delegation: None,
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
                    session_id: Some("s1".into()),
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
        let state = AppState::new(hub, db);
        Harness {
            _dir: dir,
            store,
            cursor,
            state,
        }
    }

    impl Harness {
        fn append(&mut self, timeline_id: &str) {
            let row = MessageRow {
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
                // Real pipeline rows always carry provenance; the
                // activation sweep deletes by it.
                src_cursor: Some(self.cursor),
                src_offset: None,
            };
            self.store
                .apply(&Batch {
                    ops: IngestOp::project_message(row),
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
                ServerFrame::TimelineSnapshot {
                    entries,
                    timeline_meta,
                    ..
                } => {
                    assert_eq!(entries.len(), 1, "snapshot covers the pre-subscribe row");
                    assert_eq!(timeline_meta, &timeline_meta::TimelineMeta::default());
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
                ServerFrame::TimelineSnapshot { entries, .. } => {
                    assert!(entries.is_empty(), "the empty activation swept the rows");
                }
                other => panic!("expected replacement snapshot, got {other:?}"),
            }
        }
        task.abort();
    }

    #[tokio::test]
    async fn relevant_commit_streams_one_replacement_timeline_meta_snapshot() {
        let mut h = harness();
        let sink = RecordingSink::default();
        let frames = sink.frames.clone();
        let state = h.state.clone();
        let task = tokio::spawn(async move {
            let mut sink = sink;
            run_session_stream(state, "s1".into(), None, &mut sink).await;
        });
        wait_for_frames(&frames, 2).await;

        h.store
            .apply(&Batch {
                ops: vec![IngestOp::Plan(PlanRow {
                    session_id: "s1".into(),
                    revision: 1,
                    content: "# Plan".into(),
                    updated_at: 20_000,
                })],
                ..Batch::default()
            })
            .unwrap();
        h.absorb();
        wait_for_frames(&frames, 3).await;

        let seen = frames.lock().unwrap();
        match &seen[2] {
            ServerFrame::TimelineMetaSnapshot {
                stamp: Some(_),
                timeline_meta,
            } => {
                assert_eq!(timeline_meta.plans.len(), 1);
                assert_eq!(timeline_meta.plans[0].name, "plan-r1.md");
            }
            other => panic!("expected timeline meta snapshot, got {other:?}"),
        }
        drop(seen);
        task.abort();
    }

    #[tokio::test]
    async fn replacement_timeline_meta_hides_progressive_pending_payload() {
        let dir = tempfile::TempDir::new().unwrap();
        let db = dir.path().join("projection.sqlite");
        let uri = format!(
            "file:rosetta-ws-pending-{}?mode=memory&cache=shared",
            std::process::id()
        );
        let mut store = Store::open_shared_in_memory_for_rebuild_with_assets(&uri, &db).unwrap();
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
                        cwd: Some("/w/rosetta".into()),
                        parent_session_id: None,
                        delegation: None,
                        root_session_id: "s1".into(),
                        role: SessionRole::Main,
                        title: None,
                        summary: None,
                        started_at: 10_000,
                        updated_at: 10_000,
                        stream_state: StreamState::Pending,
                    }),
                    IngestOp::Plan(PlanRow {
                        session_id: "s1".into(),
                        revision: 1,
                        content: "# Not public yet".into(),
                        updated_at: 20_000,
                    }),
                ],
                ..Batch::default()
            })
            .unwrap();
        let projection = Arc::new(ProjectionReader::shared_memory(&uri).unwrap());

        let frame =
            timeline_meta_snapshot(projection, "s1".into(), Some(Stamp { epoch: 1, seq: 2 })).await;
        let frame = match frame {
            Ok(frame) => frame,
            Err(_) => panic!("timeline meta snapshot should be readable"),
        };

        match frame {
            ServerFrame::TimelineMetaSnapshot {
                stamp,
                timeline_meta,
            } => {
                assert_eq!(stamp, Some(Stamp { epoch: 1, seq: 2 }));
                assert_eq!(timeline_meta, timeline_meta::TimelineMeta::default());
            }
            other => panic!("expected timeline meta snapshot, got {other:?}"),
        }
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
        wait_for_frames(&frames, 4).await;
        {
            let seen = frames.lock().unwrap();
            assert!(matches!(seen[0], ServerFrame::Hello { .. }));
            assert!(matches!(seen[3], ServerFrame::CaughtUp { .. }));
            let replayed: Vec<&str> = seen[1..3]
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
    async fn empty_replay_and_metadata_replay_end_with_an_explicit_checkpoint() {
        let mut h = harness();
        let (_sub, resume) = h.state.hub.subscribe_session("s1", now_ms());
        for with_metadata in [false, true] {
            if with_metadata {
                h.store
                    .apply(&Batch {
                        ops: vec![IngestOp::Plan(PlanRow {
                            session_id: "s1".into(),
                            revision: 1,
                            content: "# Plan".into(),
                            updated_at: 20_000,
                        })],
                        ..Batch::default()
                    })
                    .unwrap();
                h.absorb();
            }
            let sink = RecordingSink::default();
            let frames = sink.frames.clone();
            let state = h.state.clone();
            let task = tokio::spawn(async move {
                run_session_stream(state, "s1".into(), Some(resume), &mut sink.clone()).await;
            });
            wait_for_frames(&frames, if with_metadata { 3 } else { 2 }).await;
            let seen = frames.lock().unwrap();
            assert!(matches!(seen.last(), Some(ServerFrame::CaughtUp { .. })));
            if with_metadata {
                assert!(
                    matches!(&seen[1], ServerFrame::TimelineMetaSnapshot { stamp: None, timeline_meta } if timeline_meta.plans.len() == 1)
                );
            } else {
                assert_eq!(seen[1], ServerFrame::CaughtUp { stamp: resume });
            }
            task.abort();
        }
    }

    #[tokio::test]
    async fn snapshot_cursor_continues_above_the_window_in_the_same_generation() {
        let mut h = harness();
        for i in 0..205 {
            h.append(&format!("m{i}"));
        }
        h.absorb();
        let (_sub, stamp) = h.state.hub.subscribe_session("s1", now_ms());
        let frame = timeline_snapshot(h.state.projection.clone(), "s1".into(), stamp)
            .await
            .ok()
            .unwrap();
        let ServerFrame::TimelineSnapshot {
            entries,
            cursor,
            watermark,
            ..
        } = frame
        else {
            panic!("snapshot")
        };
        assert_eq!(entries.len(), 200);
        let page = with_read(h.state.projection.clone(), move |conn| {
            read_timeline_page(conn, "s1", 200, cursor.as_deref(), false)
        })
        .await
        .ok()
        .unwrap();
        assert_eq!(page.watermark, watermark);
        assert_eq!(page.entries.len(), 5);
        assert!(page.entries.last().unwrap().ordinal < entries[0].ordinal);
        assert!(page.cursor.is_none());
    }

    #[tokio::test]
    async fn collaboration_stream_sends_only_boundaries_and_runtime_observations() {
        use rosetta_core::timeline::{
            CollaborationChannel, CollaborationEvent, CollaborationEventKind, GroupDerivation,
            ResponseGroup, TimelineBody,
        };
        let mut h = harness();
        h.append("private");
        let mut private = queries::timeline_entry(h.store.conn(), "s1", "private")
            .unwrap()
            .unwrap();
        private.response = Some(ResponseGroup {
            id: "r1".into(),
            derivation: GroupDerivation::Native,
        });
        private.block_index = Some(0);
        private.model = Some("model-1".into());
        let mut boundary = private.clone();
        boundary.timeline_id = "boundary".into();
        boundary.ordinal = private.ordinal + 1;
        boundary.response = None;
        boundary.block_index = None;
        boundary.model = None;
        boundary.body = TimelineBody::Collaboration {
            event: Box::new(CollaborationEvent {
                kind: CollaborationEventKind::Message,
                channel: CollaborationChannel::Team,
                sender: None,
                recipient: None,
                team: None,
                color: None,
                summary: Some("delivered boundary".into()),
                body: None,
                task_id: None,
                delivery_id: None,
                status: None,
                authored_at: None,
                source_session_id: None,
                source_response_id: None,
                work_start_timeline_id: None,
                work_end_timeline_id: None,
            }),
        };
        h.store
            .apply(&Batch {
                ops: vec![
                    IngestOp::Message(Box::new(private.clone())),
                    IngestOp::Message(Box::new(boundary.clone())),
                ],
                ..Batch::default()
            })
            .unwrap();
        h.absorb();
        let sink = RecordingSink::default();
        let frames = sink.frames.clone();
        let state = h.state.clone();
        let task = tokio::spawn(async move {
            run_collaboration_stream(state, "s1".into(), &mut sink.clone()).await;
        });
        wait_for_frames(&frames, 2).await;
        {
            let seen = frames.lock().unwrap();
            let ServerFrame::CollaborationSnapshot {
                entries,
                response_runtimes,
            } = &seen[1]
            else {
                panic!("boundary snapshot")
            };
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].timeline_id, "boundary");
            assert_eq!(response_runtimes.len(), 1);
            assert_eq!(response_runtimes[0].model.as_deref(), Some("model-1"));
        }
        private.timeline_id = "private-live".into();
        private.model = Some("model-2".into());
        private.ordinal += 2;
        private.block_index = Some(1);
        boundary.timeline_id = "boundary-live".into();
        boundary.ordinal += 2;
        h.store
            .apply(&Batch {
                ops: vec![
                    IngestOp::Message(Box::new(private)),
                    IngestOp::Message(Box::new(boundary)),
                ],
                ..Batch::default()
            })
            .unwrap();
        h.absorb();
        wait_for_frames(&frames, 4).await;
        {
            let seen = frames.lock().unwrap();
            assert!(
                matches!(&seen[2], ServerFrame::ResponseRuntime { observation } if observation.model.as_deref() == Some("model-2"))
            );
            assert!(
                matches!(&seen[3], ServerFrame::Mutation { mutation: Mutation::Message(row), .. } if row.timeline_id == "boundary-live")
            );
            assert!(
                !serde_json::to_string(&*seen)
                    .unwrap()
                    .contains("content of private")
            );
        }
        h.state.hub.invalidate_sessions(["s1"], now_ms());
        wait_for_frames(&frames, 6).await;
        {
            let seen = frames.lock().unwrap();
            assert_eq!(seen[4], ServerFrame::Resync);
            assert!(
                matches!(&seen[5], ServerFrame::CollaborationSnapshot { entries, .. } if entries.len() == 2)
            );
        }
        h.store
            .apply(&Batch {
                ops: vec![IngestOp::Usage {
                    session_id: "s1".into(),
                    response: "private-live".into(),
                    ts: 3_600_100,
                    model: Some("model-3".into()),
                    tokens: rosetta_core::TokenSnapshot::default(),
                }],
                ..Batch::default()
            })
            .unwrap();
        h.absorb();
        wait_for_frames(&frames, 7).await;
        assert!(
            matches!(&frames.lock().unwrap()[6], ServerFrame::ResponseRuntime { observation }
            if observation.response_id == "r1" && observation.model.as_deref() == Some("model-3"))
        );
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

    #[tokio::test]
    async fn idle_streams_ping_and_stay_deliverable() {
        let mut h = harness();
        h.append("m1");
        h.absorb();

        let sink = RecordingSink::default();
        let frames = sink.frames.clone();
        let pings = sink.pings.clone();
        let state = h.state.clone();
        let task = tokio::spawn(async move {
            let mut sink = sink;
            run_session_stream(state, "s1".into(), None, &mut sink).await;
        });
        wait_for_frames(&frames, 2).await;

        // Idle past the (test-scaled) keepalive: pings flow.
        tokio::time::timeout(Duration::from_secs(5), async {
            while pings.load(std::sync::atomic::Ordering::SeqCst) == 0 {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("an idle stream must ping");

        // The stream is still live after pinging.
        h.append("m2");
        h.absorb();
        wait_for_frames(&frames, 3).await;
        task.abort();
    }
}
