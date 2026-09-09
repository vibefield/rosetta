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
    MessageKind, MessageRow, PlanRow, ProjectRow, RuntimeRow, SessionRole, SessionRow, StreamState,
};
use rosetta_core::timeline::{
    ArtifactKind, ArtifactOp, CollaborationChannel, CollaborationEvent, CollaborationEventKind,
    GroupDerivation, GroupKind, ReadTarget, ResourceRef, TimelineGroup,
};
use rosetta_core::{
    CanonicalText, CorrelationDerivation, IngestOp, NativeToolRef, ProjectionAsset, StatusBasis,
    TimelineBody, TimelineEntry, TokenSnapshot, ToolAction, ToolCorrelation, ToolInteraction,
    ToolStatus, ToolStatusFact,
};
use rosetta_protocol::{
    ApiError, CollaborationTimeline, CommandResponse, ErrorCode, SearchResponse, SessionsResponse,
    StatusResponse, TimelinePage,
};
use rosetta_search::{
    Fts5SidecarSearch, SearchBackend, SearchError, SearchOutput, SearchQuery, SearchReadiness,
    SearchState, synchronize_fts5_once,
};
use rosetta_server::{AppState, Hub, HubConfig, ProjectionReader, RawCommandReader, router};
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
        delegation: None,
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
async fn plan_body_is_loaded_only_after_its_scoped_revision_is_opened() {
    let mut h = harness();
    h.store
        .apply(&Batch {
            ops: vec![IngestOp::Plan(PlanRow {
                session_id: "s1".into(),
                revision: 7,
                content: "# Exact plan\n\nShip it.\n".into(),
                updated_at: 12_000,
            })],
            ..Batch::default()
        })
        .unwrap();

    let response = h
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/sessions/s1/plans/revisions/7")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.into_body().collect().await.unwrap().to_bytes(),
        "# Exact plan\n\nShip it.\n"
    );

    let missing = h
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/sessions/s2/plans/revisions/7")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn exact_timeline_row_is_scoped_and_does_not_require_paging() {
    let mut h = harness();
    h.append(vec![message(
        "old-task-anchor",
        MessageKind::Agent,
        "anchored row",
    )]);

    let (status, body) = h.get("/api/sessions/s1/timeline/old-task-anchor").await;
    assert_eq!(status, StatusCode::OK);
    let result: rosetta_protocol::TimelineEntryResponse = serde_json::from_value(body).unwrap();
    let entry = result.entry;
    assert!(!result.watermark.is_empty());
    assert_eq!(entry.timeline_id, "old-task-anchor");

    let (wrong_session, _) = h.get("/api/sessions/s2/timeline/old-task-anchor").await;
    assert_eq!(wrong_session, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn collaboration_history_returns_boundaries_without_private_rows() {
    let mut h = harness();
    let event = TimelineEntry {
        timeline_id: "team-message".into(),
        session_id: "s1".into(),
        ordinal: 2,
        group: TimelineGroup {
            id: "team-cycle".into(),
            kind: GroupKind::SystemCycle,
            derivation: GroupDerivation::Native,
        },
        response: None,
        block_index: None,
        native_id: Some("delivery-native".into()),
        parent_native_id: None,
        at: Some(3_600_200),
        model: None,
        effort: None,
        body: TimelineBody::Collaboration {
            event: Box::new(CollaborationEvent {
                kind: CollaborationEventKind::Message,
                channel: CollaborationChannel::Team,
                sender: Some("w1".into()),
                recipient: Some("coordinator".into()),
                team: Some("refinement".into()),
                color: None,
                summary: Some("ready".into()),
                body: Some(CanonicalText::inline("The implementation is ready.").unwrap()),
                task_id: None,
                delivery_id: Some("message-1".into()),
                status: None,
                authored_at: Some(3_600_100),
                source_session_id: None,
                source_response_id: None,
                work_start_timeline_id: None,
                work_end_timeline_id: None,
            }),
        },
        parent_tool_native_id: None,
        usage: TokenSnapshot::default(),
        provenance: rosetta_core::Provenance {
            agent_version: None,
            src_cursor: None,
            src_offset: None,
        },
    };
    let private = message(
        "private-work",
        MessageKind::Agent,
        "secret implementation detail",
    );
    let mut ops = IngestOp::project_message(private);
    ops.push(IngestOp::Message(Box::new(event)));
    h.store
        .apply(&Batch {
            ops,
            ..Batch::default()
        })
        .unwrap();

    let (status, body) = h.get("/api/sessions/s1/collaboration").await;
    assert_eq!(status, StatusCode::OK);
    let history: CollaborationTimeline = serde_json::from_value(body).unwrap();
    assert_eq!(history.entries.len(), 1);
    assert_eq!(history.entries[0].timeline_id, "team-message");
    assert!(
        !serde_json::to_string(&history)
            .unwrap()
            .contains("secret implementation detail")
    );
}

#[tokio::test]
async fn report_work_resolves_only_its_recorded_source_response_and_bounds() {
    let mut h = harness();
    h.append(vec![
        message("before", MessageKind::Agent, "outside the report"),
        message("start", MessageKind::Agent, "first action"),
        message("end", MessageKind::Agent, "last action"),
        message("after", MessageKind::Agent, "outside the report"),
    ]);
    let mut start = queries::timeline_entry(h.store.conn(), "s1", "start")
        .unwrap()
        .unwrap();
    start.response = Some(rosetta_core::timeline::ResponseGroup {
        id: "response-1".into(),
        derivation: GroupDerivation::Native,
    });
    start.block_index = Some(0);
    let mut report = start.clone();
    report.timeline_id = "report".into();
    report.session_id = "s2".into();
    report.response = None;
    report.block_index = None;
    report.provenance.src_cursor = None;
    report.body = TimelineBody::Collaboration {
        event: Box::new(CollaborationEvent {
            kind: CollaborationEventKind::Report,
            channel: CollaborationChannel::Team,
            sender: None,
            recipient: None,
            team: None,
            color: None,
            summary: Some("completed report".into()),
            body: None,
            task_id: Some("task-1".into()),
            delivery_id: None,
            status: None,
            authored_at: None,
            source_session_id: Some("s1".into()),
            source_response_id: Some("response-1".into()),
            work_start_timeline_id: Some("start".into()),
            work_end_timeline_id: Some("end".into()),
        }),
    };
    h.store
        .apply(&Batch {
            ops: vec![
                IngestOp::Message(Box::new(start)),
                IngestOp::Message(Box::new(report.clone())),
            ],
            ..Batch::default()
        })
        .unwrap();
    let (status, body) = h.get("/api/sessions/s2/timeline/report/work").await;
    assert_eq!(status, StatusCode::OK);
    let rows: Vec<TimelineEntry> = serde_json::from_value(body).unwrap();
    assert_eq!(
        rows.iter()
            .map(|r| r.timeline_id.as_str())
            .collect::<Vec<_>>(),
        vec!["start", "end"]
    );
    for route in [
        "/api/sessions/s1/timeline/report/work",
        "/api/sessions/s1/timeline/start/work",
    ] {
        assert_eq!(h.get(route).await.0, StatusCode::NOT_FOUND);
    }
    // A response mismatch, reversed range, or absent endpoint cannot be
    // guessed from nearby work. The report must retain all of its evidence.
    for (response, start, end) in [
        ("wrong", "start", "end"),
        ("response-1", "end", "start"),
        ("response-1", "missing", "end"),
    ] {
        let mut invalid = report.clone();
        let TimelineBody::Collaboration { event } = &mut invalid.body else {
            unreachable!()
        };
        event.source_response_id = Some(response.into());
        event.work_start_timeline_id = Some(start.into());
        event.work_end_timeline_id = Some(end.into());
        h.store
            .apply(&Batch {
                ops: vec![IngestOp::Message(Box::new(invalid))],
                ..Batch::default()
            })
            .unwrap();
        assert_eq!(
            h.get("/api/sessions/s2/timeline/report/work").await.0,
            StatusCode::NOT_FOUND
        );
    }
    h.store
        .apply(&Batch {
            ops: vec![IngestOp::Message(Box::new(report))],
            ..Batch::default()
        })
        .unwrap();
    let mut pending = session("s1");
    pending.stream_state = StreamState::Pending;
    h.store
        .apply(&Batch {
            ops: vec![IngestOp::SessionMeta(pending)],
            ..Batch::default()
        })
        .unwrap();
    assert_eq!(
        h.get("/api/sessions/s2/timeline/report/work").await.0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn electron_origin_receives_the_version_header_but_null_and_foreign_origins_do_not_get_cors()
{
    let h = harness();
    for origin in [
        "rosetta://app",
        "null",
        "rosetta://foreign",
        "https://evil.example",
    ] {
        let result = h
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/status")
                    .header("origin", origin)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            result.headers()["x-rosetta-protocol-version"],
            rosetta_protocol::PROTOCOL_VERSION.to_string()
        );
        assert_eq!(
            result
                .headers()
                .get("access-control-allow-origin")
                .is_some(),
            origin == "rosetta://app"
        );
        if origin == "rosetta://app" {
            assert_eq!(
                result.headers()["access-control-expose-headers"],
                "x-rosetta-protocol-version"
            );
        }
        for route in ["/ws/meta", "/ws/sessions/s1", "/ws/collaboration/s1"] {
            let result = h
                .app
                .clone()
                .oneshot(ws_upgrade_request(route, Some(origin)))
                .await
                .unwrap();
            assert_eq!(
                result.status(),
                if origin == "rosetta://app" {
                    StatusCode::UPGRADE_REQUIRED
                } else {
                    StatusCode::FORBIDDEN
                }
            );
        }
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

struct StubRawCommands {
    command: String,
    seen: Mutex<Vec<queries::CommandSource>>,
}

impl RawCommandReader for StubRawCommands {
    fn read_command(&self, source: &queries::CommandSource) -> Result<Option<String>, String> {
        self.seen.lock().unwrap().push(source.clone());
        Ok(Some(self.command.clone()))
    }
}

#[tokio::test]
async fn recorded_file_read_serves_only_the_addressed_canonical_target() {
    let mut h = harness();
    let path = h._dir.path().join("viewer-target.ts");
    let contents = b"export const answer = 42;\n";
    std::fs::write(&path, contents).unwrap();

    let mut row = message("read-target", MessageKind::ToolUse, "");
    row.content = None;
    row.tool = Some(ToolInteraction {
        correlation: ToolCorrelation {
            key: "tool-read".into(),
            derivation: CorrelationDerivation::NativeId,
        },
        native: NativeToolRef {
            id: Some("tool-read".into()),
            namespace: None,
            name: "Read".into(),
            provider: None,
        },
        status: ToolStatusFact {
            value: ToolStatus::Succeeded,
            basis: StatusBasis::NativeTerminalRecord,
        },
        action: ToolAction::FileRead {
            targets: vec![ReadTarget {
                resource: ResourceRef::File {
                    path: path.to_string_lossy().into_owned(),
                    resolved: true,
                },
                range: None,
                media_kind: None,
            }],
            outcome: None,
        },
        failure: None,
        timing: None,
    });
    h.append(vec![row]);

    let response = h
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/sessions/s1/timeline/read-target/read/0")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()["content-type"],
        "application/octet-stream"
    );
    assert_eq!(response.headers()["cache-control"], "no-store");
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(bytes.as_ref(), contents);

    let (status, _) = h.get("/api/sessions/s1/timeline/read-target/read/1").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn recorded_artifact_serves_each_addressed_file_resource() {
    let mut h = harness();
    let first = h._dir.path().join("artifact-first.txt");
    let second = h._dir.path().join("artifact-second.txt");
    std::fs::write(&first, b"first artifact\n").unwrap();
    std::fs::write(&second, b"second artifact\n").unwrap();

    let mut row = message("artifact-target", MessageKind::ToolUse, "");
    row.content = None;
    row.tool = Some(ToolInteraction {
        correlation: ToolCorrelation {
            key: "tool-artifact".into(),
            derivation: CorrelationDerivation::NativeId,
        },
        native: NativeToolRef {
            id: Some("tool-artifact".into()),
            namespace: None,
            name: "SendUserFile".into(),
            provider: None,
        },
        status: ToolStatusFact {
            value: ToolStatus::Succeeded,
            basis: StatusBasis::NativeTerminalRecord,
        },
        action: ToolAction::Artifact {
            operation: ArtifactOp::Send,
            artifact_kind: ArtifactKind::File,
            name: None,
            description: None,
            resources: vec![
                ResourceRef::File {
                    path: first.to_string_lossy().into_owned(),
                    resolved: true,
                },
                ResourceRef::File {
                    path: second.to_string_lossy().into_owned(),
                    resolved: true,
                },
            ],
            media: None,
        },
        failure: None,
        timing: None,
    });
    h.append(vec![row]);

    for (index, expected) in [b"first artifact\n".as_slice(), b"second artifact\n"]
        .into_iter()
        .enumerate()
    {
        let response = h
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/api/sessions/s1/timeline/artifact-target/read/{index}"
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(bytes.as_ref(), expected);
    }

    let (status, _) = h
        .get("/api/sessions/s1/timeline/artifact-target/read/2")
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn full_command_is_loaded_only_through_its_raw_source_anchor() {
    let mut h = harness();
    let full = "one\ntwo\nthree\nfour\nfive\nsix\nseven".to_string();
    let mut row = message("long-command", MessageKind::ToolUse, "");
    row.content = None;
    row.src_cursor = Some(h.cursor);
    row.src_offset = Some(42);
    row.tool = Some(ToolInteraction {
        correlation: ToolCorrelation {
            key: "tool-long".into(),
            derivation: CorrelationDerivation::NativeId,
        },
        native: NativeToolRef {
            id: Some("tool-long".into()),
            namespace: None,
            name: "Bash".into(),
            provider: None,
        },
        status: ToolStatusFact {
            value: ToolStatus::Succeeded,
            basis: StatusBasis::NativeTerminalRecord,
        },
        action: ToolAction::Shell {
            command: CanonicalText::inline(full.clone()).unwrap(),
            source_truncated: false,
            cwd: None,
            timeout_ms: None,
            background: false,
            outcome: None,
        },
        failure: None,
        timing: None,
    });
    h.append(vec![row]);
    let reader = Arc::new(StubRawCommands {
        command: full.clone(),
        seen: Mutex::new(Vec::new()),
    });
    h.app = router(
        AppState::with_search(h.hub.clone(), h.db.clone(), Arc::new(h.search.clone()))
            .with_raw_commands(reader.clone()),
    );

    let (status, body) = h
        .get("/api/sessions/s1/timeline/long-command/command")
        .await;
    assert_eq!(status, StatusCode::OK);
    let response: CommandResponse = serde_json::from_value(body).unwrap();
    assert_eq!(response.command, full);
    let seen = reader.seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].offset, 42);
    assert_eq!(seen[0].relative_path, "p/x.jsonl");
    assert_eq!(seen[0].correlation_key, "tool-long");

    let stored = queries::timeline_page(h.store.conn(), "s1", None, 10).unwrap();
    let TimelineBody::ToolUse { tool } = &stored[0].body else {
        panic!("expected tool row");
    };
    let ToolAction::Shell {
        command,
        source_truncated,
        ..
    } = &tool.action
    else {
        panic!("expected shell action");
    };
    assert_eq!(command.as_preview(), "one");
    assert!(source_truncated);
}

#[tokio::test]
async fn unassigned_project_scope_reports_projectless_session_figures() {
    let mut h = harness();
    let mut orphan = session("orphan");
    orphan.project_id = None;
    orphan.cwd = None;
    let mut orphan_message = message("orphan-message", MessageKind::Agent, "projectless");
    orphan_message.session_id = "orphan".into();
    orphan_message.usage = Some(rosetta_core::rows::TokenSnapshot {
        input: Some(11),
        output: Some(7),
        ..Default::default()
    });
    let mut ops = vec![IngestOp::SessionMeta(orphan)];
    ops.extend(IngestOp::project_message(orphan_message));
    h.store
        .apply(&Batch {
            ops,
            ..Batch::default()
        })
        .unwrap();

    let scope = "__rosetta_unassigned__";
    let (status, contribution) = h.get(&format!("/api/projects/{scope}/contribution")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(contribution["rows"][0]["session_id"], "orphan");
    assert_eq!(contribution["rows"][0]["messages"], 1);
    assert_eq!(contribution["rows"][0]["tokens"], 18);

    let (status, heatmap) = h.get(&format!("/api/projects/{scope}/heatmap")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(heatmap["days"][0]["tokens"], 18);

    let (status, usage) = h.get(&format!("/api/projects/{scope}/usage")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(usage["models"][0]["tokens"], 18);
    assert_eq!(usage["hours"][0]["tokens"], 18);
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

#[tokio::test]
async fn cold_ram_projection_serves_canonical_timeline_before_disk_publish() {
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("projection.sqlite");
    let uri = format!(
        "file:rosetta-http-test-{}?mode=memory&cache=shared",
        std::process::id()
    );
    let mut store = Store::open_shared_in_memory_for_rebuild_with_assets(&uri, &db).unwrap();
    let mut cold_session = session("s1");
    cold_session.stream_state = StreamState::Pending;
    let mut ops = vec![
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
            root_paths: vec!["/w/rosetta".into()],
            git_repo: None,
        }),
        IngestOp::SessionMeta(cold_session),
    ];
    ops.extend(IngestOp::project_message(message(
        "ram-message",
        MessageKind::Agent,
        "from RAM",
    )));
    store
        .apply(&Batch {
            ops,
            ..Batch::default()
        })
        .unwrap();
    let hub = Arc::new(Hub::new(HubConfig::default(), 1));
    hub.warm(store.conn()).unwrap();
    let projection = Arc::new(ProjectionReader::shared_memory(&uri).unwrap());
    let app = router(AppState::new(hub, db.clone()).with_projection(projection.clone()));

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
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/search?q=from")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let search: SearchResponse = serde_json::from_slice(&body).unwrap();
    assert_eq!(search.state.readiness, SearchReadiness::Building);
    assert!(search.hits.is_empty());

    for endpoint in [
        "/api/projects/p1/heatmap",
        "/api/projects/p1/usage",
        "/api/projects/p1/contribution",
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(endpoint)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let envelope: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let error: ApiError = serde_json::from_value(envelope["error"].clone()).unwrap();
        assert_eq!(error.code, ErrorCode::NotReady);
    }

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/sessions/s1/timeline")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let timeline: TimelinePage = serde_json::from_slice(&body).unwrap();
    assert!(
        timeline.entries.is_empty(),
        "source commits stay hidden while RAM reconciliation is pending"
    );

    store
        .set_session_stream_state("s1", StreamState::Indexed)
        .unwrap();
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/sessions/s1/timeline")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let timeline: TimelinePage = serde_json::from_slice(&body).unwrap();
    assert_eq!(timeline.entries.len(), 1);
    assert_eq!(timeline.entries[0].timeline_id, "ram-message");

    let mut disk = Store::open(&db).unwrap();
    let mut disk_ops = vec![
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
            root_paths: vec!["/w/rosetta".into()],
            git_repo: None,
        }),
        IngestOp::SessionMeta(session("s1")),
    ];
    disk_ops.extend(IngestOp::project_message(message(
        "disk-message",
        MessageKind::Agent,
        "from disk",
    )));
    disk.apply(&Batch {
        ops: disk_ops,
        ..Batch::default()
    })
    .unwrap();
    projection.publish_file(db);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/projects/p1/contribution")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/sessions/s1/timeline")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let timeline: TimelinePage = serde_json::from_slice(&body).unwrap();
    assert_eq!(timeline.entries.len(), 1);
    assert_eq!(timeline.entries[0].timeline_id, "disk-message");
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
        progress: None,
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
        .get("/api/search?q=ionosphere&kind=user&session=s1&limit=7")
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
    assert_eq!(queries[0].kind.as_deref(), Some("user"));
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
            source_truncated: false,
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

    // Coverage is prompts and replies (2026-09-09): the tool row that also says
    // "ionosphere" is canonical and rendered, never a hit.
    let (status, body) = h.get("/api/search?q=ionosphere").await;
    assert_eq!(status, StatusCode::OK);
    let parsed: SearchResponse = serde_json::from_value(body).unwrap();
    assert_eq!(parsed.hits.len(), 1);
    assert_eq!(parsed.hits[0].timeline_id, "m-user");
    assert_eq!(parsed.hits[0].kind, "user");

    let (_, body) = h.get("/api/search?q=ionosphere&agent=claude-code").await;
    let parsed: SearchResponse = serde_json::from_value(body).unwrap();
    assert_eq!(parsed.hits.len(), 1);

    let (_, body) = h.get("/api/search?q=ionosphere&agent=codex").await;
    let parsed: SearchResponse = serde_json::from_value(body).unwrap();
    assert!(parsed.hits.is_empty());

    // A selector that can never match is refused, never an empty success.
    for selector in [
        "kind=tool_use",
        "kind=thinking",
        "is_error=true",
        "is_error=false",
    ] {
        let (status, body) = h.get(&format!("/api/search?q=ionosphere&{selector}")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{selector}");
        assert_eq!(body["error"]["code"], "bad_request", "{selector}");
    }
    let (_, body) = h.get("/api/search?q=ionosphere&kind=user").await;
    let parsed: SearchResponse = serde_json::from_value(body).unwrap();
    assert_eq!(parsed.hits.len(), 1);
    assert_eq!(parsed.hits[0].timeline_id, "m-user");

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
