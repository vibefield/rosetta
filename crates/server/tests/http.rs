//! REST surface over a real file-backed projection: mirror-served lists,
//! exact-watermark timeline pagination (expiry on rewrite, never a silent
//! rebase), filtered search, and readiness on the status route.

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

use rosetta_core::rows::{
    MessageKind, MessageRow, ProjectRow, RuntimeRow, SessionRole, SessionRow, StreamState,
};
use rosetta_core::{
    CanonicalText, CorrelationDerivation, IngestOp, NativeToolRef, ProjectionAsset, StatusBasis,
    ToolAction, ToolCorrelation, ToolInteraction, ToolStatus, ToolStatusFact,
};
use rosetta_protocol::{SearchResponse, SessionsResponse, StatusResponse, TimelinePage};
use rosetta_search::{
    Fts5SidecarSearch, SearchBackend, SearchError, SearchOutput, SearchQuery, SearchReadiness,
    SearchState, synchronize_fts5_once,
};
use rosetta_server::{AppState, Hub, HubConfig, router};
use rosetta_store::{Batch, CheckpointUpdate, SourceMode, SourceWrite, Store, queries};
use tempfile::TempDir;

fn session(id: &str) -> SessionRow {
    SessionRow {
        id: id.into(),
        native_session_id: format!("native-{id}"),
        runtime_id: "rt1".into(),
        project_id: Some("p1".into()),
        cwd: Some("/w/rosetta".into()),
        parent_session_id: None,
        root_session_id: id.into(),
        role: SessionRole::Main,
        title: None,
        summary: None,
        started_at: 10_000,
        updated_at: 10_000,
        stream_state: StreamState::Streaming,
    }
}

fn message(timeline_id: &str, kind: MessageKind, content: &str) -> MessageRow {
    MessageRow {
        timeline_id: timeline_id.into(),
        session_id: "s1".into(),
        ordinal: 0,
        kind,
        subtype: None,
        uuid: None,
        parent_uuid: None,
        ts: Some(3_600_000),
        model: None,
        effort: None,
        content: Some(content.to_string()),
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
    _dir: TempDir,
    store: Store,
    cursor: i64,
    hub: Arc<Hub>,
    db: std::path::PathBuf,
    search: Fts5SidecarSearch,
    app: Router,
}

fn harness() -> Harness {
    let dir = TempDir::new().unwrap();
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
                IngestOp::SessionMeta(session("s1")),
                IngestOp::SessionMeta(session("s2")),
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
    let search = Fts5SidecarSearch::new(&db);
    let ack = synchronize_fts5_once(&search, &AtomicBool::new(false))
        .unwrap()
        .unwrap();
    store
        .acknowledge_search(&ack.projection_epoch, ack.revision)
        .unwrap();
    let app = router(AppState::with_search(
        hub.clone(),
        db.clone(),
        Arc::new(search.clone()),
    ));
    Harness {
        _dir: dir,
        store,
        cursor,
        hub,
        db,
        search,
        app,
    }
}

impl Harness {
    fn append(&mut self, rows: Vec<MessageRow>) {
        self.store
            .apply(&Batch {
                ops: rows
                    .into_iter()
                    .flat_map(IngestOp::project_message)
                    .collect(),
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
        let ack = synchronize_fts5_once(&self.search, &AtomicBool::new(false))
            .unwrap()
            .unwrap();
        self.store
            .acknowledge_search(&ack.projection_epoch, ack.revision)
            .unwrap();
    }

    async fn get(&self, uri: &str) -> (StatusCode, serde_json::Value) {
        let response = self
            .app
            .clone()
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let value = serde_json::from_slice(&bytes).unwrap();
        (status, value)
    }
}

#[tokio::test]
async fn content_addressed_images_are_served_without_exposing_filesystem_paths() {
    let mut h = harness();
    let asset = ProjectionAsset::new("image/png", b"\x89PNG\r\n\x1a\nbody".to_vec()).unwrap();
    let url = asset.url().unwrap();
    h.store
        .apply(&Batch {
            ops: vec![IngestOp::ProjectionAsset {
                asset: asset.clone(),
            }],
            ..Batch::default()
        })
        .unwrap();

    let response = h
        .app
        .clone()
        .oneshot(Request::builder().uri(url).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-type"], "image/png");
    assert_eq!(response.headers()["x-content-type-options"], "nosniff");
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(bytes.as_ref(), asset.bytes);

    let missing = h
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/assets/{}.png", "0".repeat(64)))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn status_reports_versions_and_detached_search() {
    let h = harness();
    let (status, body) = h.get("/api/status").await;
    assert_eq!(status, StatusCode::OK);
    let parsed: StatusResponse = serde_json::from_value(body).unwrap();
    assert!(parsed.canonical_ready);
    assert_eq!(parsed.schema_version, rosetta_store::schema::SCHEMA_VERSION);
    assert_eq!(
        serde_json::to_value(parsed.search.readiness).unwrap(),
        "ready"
    );
}

#[tokio::test]
async fn progressive_projection_lists_are_visible_before_canonical_or_search_ready() {
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("progressive.sqlite");
    let mut store = Store::open_progressive(&db).unwrap();
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
                }),
                IngestOp::Project(ProjectRow {
                    id: "p1".into(),
                    name: "project".into(),
                    root_paths: vec!["/project".into()],
                    git_repo: None,
                }),
                IngestOp::SessionMeta(session("s1")),
            ],
            checkpoints: vec![],
            source: None,
        })
        .unwrap();
    let hub = Arc::new(Hub::new(HubConfig::default(), 1));
    hub.warm(store.conn()).unwrap();
    let app = router(AppState::new(hub, db));

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let status: StatusResponse = serde_json::from_slice(&body).unwrap();
    assert!(!status.canonical_ready);
    assert_eq!(status.search.readiness, SearchReadiness::Building);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/sessions")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let sessions: SessionsResponse = serde_json::from_slice(&body).unwrap();
    assert_eq!(sessions.sessions.len(), 1);
    assert_eq!(sessions.sessions[0].id, "s1");
}

#[derive(Clone)]
struct BuildingSearch {
    state: SearchState,
    seen: Arc<Mutex<Vec<SearchQuery>>>,
}

impl SearchBackend for BuildingSearch {
    fn state(&self) -> SearchState {
        self.state.clone()
    }

    fn search(&self, query: SearchQuery) -> Result<SearchOutput, SearchError> {
        self.seen.lock().unwrap().push(query);
        Ok(SearchOutput::unavailable(self.state.clone()))
    }
}

#[tokio::test]
async fn detached_backend_never_masquerades_as_ready_empty_search() {
    let mut h = harness();
    let state = SearchState {
        readiness: SearchReadiness::Building,
        projection_epoch: Some("projection-7".into()),
        revision: Some(12),
    };
    let seen = Arc::new(Mutex::new(Vec::new()));
    let backend = Arc::new(BuildingSearch {
        state: state.clone(),
        seen: seen.clone(),
    });
    h.app = router(AppState::with_search(h.hub.clone(), h.db.clone(), backend));

    let (status, body) = h.get("/api/status").await;
    assert_eq!(status, StatusCode::OK);
    let status: StatusResponse = serde_json::from_value(body).unwrap();
    assert!(status.canonical_ready);
    assert_eq!(status.search, state);

    let (status, body) = h
        .get("/api/search?q=ionosphere&kind=tool_use&session=s1&limit=7")
        .await;
    assert_eq!(status, StatusCode::OK);
    let response: SearchResponse = serde_json::from_value(body).unwrap();
    assert_eq!(response.state, state);
    assert!(response.hits.is_empty());

    let queries = seen.lock().unwrap();
    assert_eq!(queries.len(), 1);
    assert_eq!(
        queries[0].text,
        rosetta_search::TextQuery::parse("ionosphere").unwrap()
    );
    assert_eq!(queries[0].kind.as_deref(), Some("tool_use"));
    assert_eq!(queries[0].session.as_deref(), Some("s1"));
    assert_eq!(queries[0].limit, 7);
}

#[tokio::test]
async fn lists_serve_from_the_mirror_and_missing_sessions_are_404() {
    let mut h = harness();
    let mut child = session("child");
    child.parent_session_id = Some("s1".into());
    child.root_session_id = "s1".into();
    child.role = SessionRole::Subagent;
    let mut grandchild = session("grandchild");
    grandchild.parent_session_id = Some("child".into());
    // A stale/self denormalized root must never override the authoritative
    // parent edge and make this descendant browseable as a root.
    grandchild.root_session_id = "grandchild".into();
    grandchild.role = SessionRole::Subagent;
    h.store
        .apply(&Batch {
            ops: vec![
                IngestOp::SessionMeta(child.clone()),
                IngestOp::SessionMeta(grandchild.clone()),
            ],
            checkpoints: vec![],
            source: None,
        })
        .unwrap();
    h.hub.rewarm(h.store.conn()).unwrap();
    let (status, body) = h.get("/api/sessions?limit=10").await;
    assert_eq!(status, StatusCode::OK);
    let parsed: SessionsResponse = serde_json::from_value(body).unwrap();
    assert_eq!(parsed.sessions.len(), 2);
    assert!(
        parsed
            .sessions
            .iter()
            .all(|session| session.role == SessionRole::Main)
    );

    let (status, _) = h.get("/api/sessions/s1").await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = h.get("/api/sessions/child").await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = h.get("/api/sessions/grandchild").await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = h.get("/api/sessions/ghost").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "not_found");

    let (status, body) = h.get("/api/runtimes").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["runtimes"][0]["id"], "rt1");
    let (status, body) = h.get("/api/projects").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["projects"][0]["id"], "p1");
}

#[tokio::test]
async fn timeline_pages_bind_a_watermark_that_expires_on_rewrite() {
    let mut h = harness();
    h.append(
        (0..5)
            .map(|at| message(&format!("m{at}"), MessageKind::User, &format!("body {at}")))
            .collect(),
    );

    // Newest page: ordinals 3,4 oldest-first, with a continuation.
    let (status, body) = h.get("/api/sessions/s1/timeline?limit=2").await;
    assert_eq!(status, StatusCode::OK);
    let page: TimelinePage = serde_json::from_value(body).unwrap();
    assert_eq!(
        page.entries.iter().map(|m| m.ordinal).collect::<Vec<_>>(),
        vec![3, 4]
    );
    let cursor = page.cursor.expect("more history remains");

    // The continuation survives plain appends (the watermark ignores them).
    h.append(vec![message("m5", MessageKind::User, "body 5")]);
    let uri = format!(
        "/api/sessions/s1/timeline?limit=2&cursor={}",
        urlencode(&cursor)
    );
    let (status, body) = h.get(&uri).await;
    assert_eq!(status, StatusCode::OK);
    let page: TimelinePage = serde_json::from_value(body).unwrap();
    assert_eq!(
        page.entries.iter().map(|m| m.ordinal).collect::<Vec<_>>(),
        vec![1, 2]
    );

    // A generation swap rewrites the session: the cursor must EXPIRE.
    h.store
        .apply(&Batch {
            ops: IngestOp::project_message(message(
                "rewritten",
                MessageKind::User,
                "rewritten body",
            )),
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
    let (status, body) = h.get(&uri).await;
    assert_eq!(status, StatusCode::GONE);
    assert_eq!(body["error"]["code"], "cursor_expired");
}

#[tokio::test]
async fn search_filters_inside_the_ranked_query_and_rejects_bad_input() {
    let mut h = harness();
    let mut tool_row = message("m-tool", MessageKind::ToolUse, "ionosphere via tool");
    tool_row.ts = Some(7_200_000);
    tool_row.content = None;
    tool_row.tool = Some(ToolInteraction {
        correlation: ToolCorrelation {
            key: "tool-1".into(),
            derivation: CorrelationDerivation::NativeId,
        },
        native: NativeToolRef {
            id: Some("tool-1".into()),
            namespace: None,
            name: "Bash".into(),
            provider: None,
        },
        status: ToolStatusFact {
            value: ToolStatus::Failed,
            basis: StatusBasis::NativeTerminalRecord,
        },
        action: ToolAction::Shell {
            command: CanonicalText::inline("false # ionosphere").unwrap(),
            cwd: None,
            timeout_ms: None,
            background: false,
            outcome: None,
        },
        failure: None,
        timing: None,
    });
    h.append(vec![
        message("m-user", MessageKind::User, "ionosphere question"),
        tool_row,
    ]);

    let (status, body) = h.get("/api/search?q=ionosphere").await;
    assert_eq!(status, StatusCode::OK);
    let parsed: SearchResponse = serde_json::from_value(body).unwrap();
    assert_eq!(parsed.hits.len(), 2);

    let (_, body) = h.get("/api/search?q=ionosphere&agent=claude-code").await;
    let parsed: SearchResponse = serde_json::from_value(body).unwrap();
    assert_eq!(parsed.hits.len(), 2);

    let (_, body) = h.get("/api/search?q=ionosphere&agent=codex").await;
    let parsed: SearchResponse = serde_json::from_value(body).unwrap();
    assert!(parsed.hits.is_empty());

    let (_, body) = h.get("/api/search?q=ionosphere&kind=tool_use").await;
    let parsed: SearchResponse = serde_json::from_value(body).unwrap();
    assert_eq!(parsed.hits.len(), 1);
    assert_eq!(parsed.hits[0].timeline_id, "m-tool");

    let (_, body) = h.get("/api/search?q=ionosphere&is_error=true").await;
    let parsed: SearchResponse = serde_json::from_value(body).unwrap();
    assert_eq!(parsed.hits.len(), 1);
    assert_eq!(parsed.hits[0].timeline_id, "m-tool");

    let (_, body) = h.get("/api/search?q=ionosphere&is_error=false").await;
    let parsed: SearchResponse = serde_json::from_value(body).unwrap();
    assert!(parsed.hits.is_empty(), "unknown is not coerced to false");

    let (_, body) = h
        .get("/api/search?q=ionosphere&before_ts=3700000&session=s1")
        .await;
    let parsed: SearchResponse = serde_json::from_value(body).unwrap();
    assert_eq!(parsed.hits.len(), 1);
    assert_eq!(parsed.hits[0].timeline_id, "m-user");

    let mut undated = message("m-undated", MessageKind::Agent, "ionosphere undated");
    undated.ts = None;
    h.append(vec![undated]);
    let (_, body) = h
        .get("/api/search?q=ionosphere&before_ts=3700000&session=s1")
        .await;
    let parsed: SearchResponse = serde_json::from_value(body).unwrap();
    assert_eq!(parsed.hits.len(), 1, "a time bound excludes unknown time");
    assert_eq!(parsed.hits[0].timeline_id, "m-user");

    let (status, body) = h.get("/api/search?q=").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "bad_request");
    let (status, _) = h.get("/api/search?q=%22unbalanced").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = h.get("/api/search?q=ionosphere&kind=tool").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = h
        .get("/api/search?q=ionosphere&after_ts=2&before_ts=1")
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = h.get("/api/search?q=ionosphere&agent=%20").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    h.append(vec![
        message("m-left", MessageKind::Agent, "contractalpha only"),
        message("m-right", MessageKind::Agent, "contractbeta only"),
        message("m-phrase", MessageKind::Agent, "granite copper adjacent"),
        message(
            "m-separated",
            MessageKind::Agent,
            "granite durable copper separated",
        ),
    ]);
    let (status, body) = h
        .get("/api/search?q=contractalpha%20OR%20contractbeta")
        .await;
    assert_eq!(status, StatusCode::OK);
    let parsed: SearchResponse = serde_json::from_value(body).unwrap();
    assert!(
        parsed.hits.is_empty(),
        "OR is ordinary text, not an FTS5 operator"
    );

    let (status, body) = h.get("/api/search?q=%22granite%20copper%22").await;
    assert_eq!(status, StatusCode::OK);
    let parsed: SearchResponse = serde_json::from_value(body).unwrap();
    assert_eq!(parsed.hits.len(), 1);
    assert_eq!(parsed.hits[0].timeline_id, "m-phrase");
}

fn urlencode(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

#[tokio::test]
async fn cors_admits_local_app_origins_only() {
    let h = harness();
    let allowed = h
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/status")
                .header("origin", "http://localhost:5173")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        allowed
            .headers()
            .get("access-control-allow-origin")
            .and_then(|value| value.to_str().ok()),
        Some("http://localhost:5173")
    );

    // A foreign website gets no CORS grant: the daemon serves personal
    // transcript data and must never be readable by drive-by pages.
    let foreign = h
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/status")
                .header("origin", "https://evil.example")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        foreign
            .headers()
            .get("access-control-allow-origin")
            .is_none()
    );
}

fn ws_upgrade_request(uri: &str, origin: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder()
        .uri(uri)
        .header("host", "127.0.0.1:4640")
        .header("connection", "upgrade")
        .header("upgrade", "websocket")
        .header("sec-websocket-version", "13")
        .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==");
    if let Some(origin) = origin {
        builder = builder.header("origin", origin);
    }
    builder.body(Body::empty()).unwrap()
}

#[tokio::test]
async fn websocket_upgrades_enforce_the_origin_policy() {
    let h = harness();
    // Browsers do not apply CORS to WebSocket, so the upgrade itself must
    // refuse foreign origins (cross-site WebSocket hijacking).
    let foreign = h
        .app
        .clone()
        .oneshot(ws_upgrade_request("/ws/meta", Some("https://evil.example")))
        .await
        .unwrap();
    assert_eq!(foreign.status(), StatusCode::FORBIDDEN);

    // Allowed and origin-less requests pass the guard and reach the upgrade
    // extractor. A synthetic oneshot request cannot carry a real connection
    // upgrade, so "past the guard" reads as 426 here; the real 101 is
    // exercised over an actual socket in the serve smoke.
    let local = h
        .app
        .clone()
        .oneshot(ws_upgrade_request(
            "/ws/sessions/s1",
            Some("http://127.0.0.1:5173"),
        ))
        .await
        .unwrap();
    assert_eq!(local.status(), StatusCode::UPGRADE_REQUIRED);

    // Native clients send no Origin at all and pass.
    let native = h
        .app
        .clone()
        .oneshot(ws_upgrade_request("/ws/meta", None))
        .await
        .unwrap();
    assert_eq!(native.status(), StatusCode::UPGRADE_REQUIRED);
}

#[tokio::test]
async fn status_reports_daemon_health_only_when_a_loop_feeds_it() {
    // The plain harness serves without an ingest loop: daemon is null.
    let h = harness();
    let (_, body) = h.get("/api/status").await;
    assert!(body["daemon"].is_null());

    // A serve-shaped state carries the health surface.
    let dir = tempfile::TempDir::new().unwrap();
    let db = dir.path().join("projection.sqlite");
    drop(rosetta_store::Store::open(&db).unwrap());
    let hub = std::sync::Arc::new(rosetta_server::Hub::new(
        rosetta_server::HubConfig::default(),
        1,
    ));
    let health = std::sync::Arc::new(rosetta_server::DaemonHealth::new(1_000));
    health.update(
        "claude-code",
        rosetta_server::RuntimeTick {
            ticks: 10,
            events: 2,
            polls: 4,
            rescans: 0,
            admissions: 1,
            live_rows: 7,
        },
        5_000,
    );
    let app = router(AppState::new(hub, db).with_daemon_health(health));
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let daemon = &body["daemon"];
    assert_eq!(daemon["runtimes"][0]["kind"], "claude-code");
    assert_eq!(daemon["runtimes"][0]["live_rows"], 7);
    assert_eq!(daemon["runtimes"][0]["last_activity_ms"], 5_000);
    assert_eq!(daemon["hub"]["subscribers"], 0);
    assert!(daemon["uptime_ms"].is_number());
}
