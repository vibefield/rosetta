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
        activity: None,
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
                    label: None,
                    provenance: Default::default(),
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

#[tokio::test]
async fn html_library_reads_exact_captured_bytes_only_through_the_owning_session() {
    use rosetta_core::html_artifacts::{
        HTML_MEDIA_TYPE, HtmlArtifactAvailability, HtmlArtifactContent, HtmlArtifactPublication,
        identity,
    };
    let mut h = harness();
    h.append(vec![message(
        "html-review",
        MessageKind::Agent,
        "Here is the design",
    )]);
    let bytes = b"<html><script>window.example = true</script><h1>Review</h1></html>".to_vec();
    let asset = ProjectionAsset::new(HTML_MEDIA_TYPE, bytes.clone()).unwrap();
    let reference = asset.reference();
    let id = identity(&["publication"]);
    h.store
        .apply(&Batch {
            ops: vec![
                IngestOp::ProjectionAsset { asset },
                IngestOp::HtmlArtifact(HtmlArtifactPublication {
                    id: id.clone(),
                    artifact_id: identity(&["artifact"]),
                    session_id: "s1".into(),
                    timeline_id: "html-review".into(),
                    title: "Design review".into(),
                    filename: "design.html".into(),
                    path: Some("/gone/design.html".into()),
                    url: None,
                    native_id: None,
                    version: None,
                    published_at: Some(5),
                    source_offset: 20,
                    description: Some("The checkout redesign".into()),
                    dropped_files: Some(2),
                }),
                IngestOp::HtmlArtifactContent(HtmlArtifactContent {
                    session_id: "s1".into(),
                    source_offset: 20,
                    path: Some("/gone/design.html".into()),
                    availability: HtmlArtifactAvailability::Captured,
                    captured_at: Some(50),
                    recovered_from: None,
                    asset: Some(reference.clone()),
                }),
            ],
            ..Default::default()
        })
        .unwrap();
    let (status, library) = h.get("/api/artifacts").await;
    assert_eq!(status, StatusCode::OK, "{library}");
    assert_eq!(library["entries"][0]["publication"]["id"], id);
    assert_eq!(
        library["entries"][0]["publication"]["timeline_id"],
        "html-review"
    );
    // RO-33 and RO-34a ride the publication onto the list entry.
    assert_eq!(
        library["entries"][0]["publication"]["description"],
        "The checkout redesign"
    );
    assert_eq!(library["entries"][0]["publication"]["dropped_files"], 2);
    let response = h
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/sessions/s1/artifacts/{id}/content"))
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
    assert_eq!(
        response.headers()["content-security-policy"],
        "sandbox; default-src 'none'"
    );
    assert_eq!(response.headers()["x-content-type-options"], "nosniff");
    assert_eq!(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .as_ref(),
        bytes
    );
    assert_eq!(
        h.get(&format!("/api/sessions/s2/artifacts/{id}/content"))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        h.get(&reference.url).await.0,
        StatusCode::NOT_FOUND,
        "no executable HTML on the daemon origin"
    );
    assert_eq!(
        h.get("/api/artifacts?cursor=broken").await.0,
        StatusCode::BAD_REQUEST
    );
    let mut repeated: HtmlArtifactPublication =
        serde_json::from_value(library["entries"][0]["publication"].clone()).unwrap();
    h.store
        .apply(&Batch {
            ops: vec![IngestOp::HtmlArtifact(repeated.clone())],
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        h.get("/api/artifacts").await.1["entries"][0]["availability"],
        "captured",
        "idempotent publication preserves its observation"
    );
    repeated.source_offset += 1;
    h.store
        .apply(&Batch {
            ops: vec![IngestOp::HtmlArtifact(repeated)],
            ..Default::default()
        })
        .unwrap();
    let (_, changed) = h.get("/api/artifacts").await;
    assert_eq!(
        changed["entries"][0]["availability"], "not_captured",
        "a new coordinate cannot inherit an old observation"
    );
    assert!(changed["entries"][0]["asset"].is_null());
    assert_eq!(
        h.get(&format!("/api/sessions/s1/artifacts/{id}/content"))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
}

/// One captured publication in `s1`, applied live: its id and its capture.
fn capture_publication(
    h: &mut Harness,
    name: &str,
    html: &str,
    published_at: i64,
) -> (String, rosetta_core::timeline::AssetRef) {
    use rosetta_core::html_artifacts::{
        HTML_MEDIA_TYPE, HtmlArtifactAvailability, HtmlArtifactContent, HtmlArtifactPublication,
        identity,
    };
    let asset = ProjectionAsset::new(HTML_MEDIA_TYPE, html.as_bytes().to_vec()).unwrap();
    let reference = asset.reference();
    let id = identity(&["publication", name]);
    let path = format!("/gone/{name}.html");
    let offset = 100 + published_at as u64;
    h.store
        .apply(&Batch {
            ops: vec![
                IngestOp::ProjectionAsset { asset },
                IngestOp::HtmlArtifact(HtmlArtifactPublication {
                    id: id.clone(),
                    artifact_id: identity(&["artifact", name]),
                    session_id: "s1".into(),
                    timeline_id: "html-review".into(),
                    title: name.into(),
                    filename: format!("{name}.html"),
                    path: Some(path.clone()),
                    url: None,
                    native_id: None,
                    version: None,
                    published_at: Some(published_at),
                    source_offset: offset,
                    description: None,
                    dropped_files: Some(0),
                }),
                IngestOp::HtmlArtifactContent(HtmlArtifactContent {
                    session_id: "s1".into(),
                    source_offset: offset,
                    path: Some(path),
                    availability: HtmlArtifactAvailability::Captured,
                    captured_at: Some(50),
                    recovered_from: None,
                    asset: Some(reference.clone()),
                }),
            ],
            ..Default::default()
        })
        .unwrap();
    (id, reference)
}

async fn post_json(
    app: &Router,
    uri: &str,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}

#[tokio::test]
async fn the_library_cursor_outlives_unrelated_ingest_and_expires_when_the_library_moves() {
    let mut h = harness();
    h.append(vec![message("html-review", MessageKind::Agent, "Designs")]);
    assert_eq!(h.get("/api/status").await.1["artifacts_revision"], 0);
    capture_publication(&mut h, "first", "<h1>First</h1>", 5);
    capture_publication(&mut h, "second", "<h1>Second</h1>", 6);
    // RO-35: one step per live transaction that moved the library.
    assert_eq!(h.get("/api/status").await.1["artifacts_revision"], 2);
    let (status, page) = h.get("/api/artifacts?limit=1").await;
    assert_eq!(status, StatusCode::OK, "{page}");
    let cursor = page["next_cursor"].as_str().unwrap().to_owned();
    let next = format!("/api/artifacts?limit=1&cursor={}", urlencode(&cursor));

    // New messages step the search revision, never the library's.
    h.append(vec![message("later", MessageKind::User, "unrelated")]);
    assert_eq!(h.get("/api/status").await.1["artifacts_revision"], 2);
    let (status, rest) = h.get(&next).await;
    assert_eq!(status, StatusCode::OK, "{rest}");
    assert_eq!(rest["entries"][0]["publication"]["title"], "first");

    // A new publication moves the library: the walk restarts from the head.
    capture_publication(&mut h, "third", "<h1>Third</h1>", 7);
    assert_eq!(h.get("/api/status").await.1["artifacts_revision"], 3);
    let (status, expired) = h.get(&next).await;
    assert_eq!(status, StatusCode::GONE, "{expired}");
    assert_eq!(expired["error"]["code"], "cursor_expired");
}

/// A router whose export door opens below `root`.
fn export_app(h: &Harness, root: &std::path::Path) -> Router {
    let root = rosetta_server::ExportRoot::new(root).unwrap();
    let state = AppState::with_search(h.hub.clone(), h.db.clone(), Arc::new(h.search.clone()));
    router(state.with_export_root(root))
}

/// The export body that names `path` as the directory.
fn export_to(path: &std::path::Path) -> serde_json::Value {
    serde_json::json!({ "into": path.to_str().unwrap() })
}

#[tokio::test]
async fn the_export_door_links_a_capture_by_digest_and_answers_it() {
    use rosetta_core::html_artifacts::SHELL_HOSTS;
    use rosetta_protocol::ExportEnvironment;
    let mut h = harness();
    h.append(vec![message("html-review", MessageKind::Agent, "Designs")]);
    let html = "<h1>Review π</h1><script src=\"app.js\"></script>";
    let (id, reference) = capture_publication(&mut h, "review", html, 5);
    let exports = TempDir::new().unwrap();
    let app = export_app(&h, exports.path());
    let uri = format!("/api/sessions/s1/artifacts/{id}/export");

    // The page lands as itself, linked from the asset store, and its
    // directory is made on the way.
    let into = exports.path().join("made-src/review");
    let (status, body) = post_json(&app, &uri, export_to(&into)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["method"], "link");
    assert_eq!(
        body["page"],
        serde_json::json!({
            "path": "index.html",
            "blake3": reference.digest,
            "bytes": reference.byte_len,
        })
    );
    assert_eq!(body["members"], serde_json::json!([]));
    assert_eq!(body["captured_at"], 50);
    assert!(body.get("shell").is_none(), "no shell unasked: {body}");
    let page = into.join("index.html");
    assert_eq!(
        std::fs::read(&page).unwrap(),
        html.as_bytes(),
        "the page's own bytes, never a shell"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let member = rosetta_store::assets::html_member_path(&h.db, &reference.digest);
        assert_eq!(
            std::fs::metadata(&page).unwrap().ino(),
            std::fs::metadata(member).unwrap().ino(),
            "one inode, linked from the asset store"
        );
        for dir in [exports.path().join("made-src"), into.clone()] {
            let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700, "{}", dir.display());
        }
    }

    // The environment rides every answer: the room's hosts by directive and
    // its theme variables, read from rosetta's own tables.
    assert_eq!(
        body["environment"],
        serde_json::to_value(ExportEnvironment::room()).unwrap()
    );
    assert_eq!(
        body["environment"]["hosts"]["script-src"],
        serde_json::json!(SHELL_HOSTS.script)
    );
    assert_eq!(
        body["environment"]["variables"]["--color-background-primary"],
        "#fff"
    );

    // The same digest at the same path answers the same and writes nothing.
    let (status, again) = post_json(&app, &uri, export_to(&into)).await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again, body);
    let names: Vec<String> = std::fs::read_dir(&into)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(names, ["index.html"], "the page and nothing else");

    // The capture's own bytes already there as a file of their own count as
    // placed: the answer is a copy of the capture's digest, and the file is
    // left as it stands, never swapped for a link.
    let copied = exports.path().join("copied");
    std::fs::create_dir(&copied).unwrap();
    let standing = copied.join("index.html");
    std::fs::write(&standing, html).unwrap();
    let (status, body) = post_json(&app, &uri, export_to(&copied)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["method"], "copy");
    assert_eq!(body["page"]["blake3"], reference.digest);
    assert_eq!(std::fs::read(&standing).unwrap(), html.as_bytes());
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let member = rosetta_store::assets::html_member_path(&h.db, &reference.digest);
        assert_ne!(
            std::fs::metadata(&standing).unwrap().ino(),
            std::fs::metadata(member).unwrap().ino(),
            "a file of its own"
        );
    }
}

/// A real cross-volume export, opt-in: `ROSETTA_TEST_OTHER_VOLUME` names a
/// directory on another device than the temporary directory that holds the
/// asset store. The link is refused there, so the page is a verified copy.
#[tokio::test]
async fn an_export_to_another_volume_is_a_verified_copy() {
    let Some(other_volume) = std::env::var_os("ROSETTA_TEST_OTHER_VOLUME") else {
        return;
    };
    let mut h = harness();
    h.append(vec![message("html-review", MessageKind::Agent, "Designs")]);
    let html = "<h1>Review π</h1>";
    let (id, reference) = capture_publication(&mut h, "review", html, 5);
    let exports = TempDir::new_in(other_volume).unwrap();
    let app = export_app(&h, exports.path());
    let uri = format!("/api/sessions/s1/artifacts/{id}/export");

    let into = exports.path().join("review");
    let (status, body) = post_json(&app, &uri, export_to(&into)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["method"], "copy");
    assert_eq!(body["page"]["blake3"], reference.digest);
    assert_eq!(
        std::fs::read(into.join("index.html")).unwrap(),
        html.as_bytes()
    );
}

#[tokio::test]
async fn the_export_door_refuses_what_it_may_not_write() {
    let mut h = harness();
    h.append(vec![message("html-review", MessageKind::Agent, "Designs")]);
    let (id, _) = capture_publication(&mut h, "review", "<h1>Review</h1>", 5);
    let uri = format!("/api/sessions/s1/artifacts/{id}/export");

    // No root: the door is closed, honestly.
    let (status, body) = post_json(&h.app, &uri, serde_json::json!({"into": "/tmp/x"})).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(body["error"]["code"], "not_ready");

    let exports = TempDir::new().unwrap();
    let app = export_app(&h, exports.path());

    // Outside the root, relative, `..`, the root itself, empty, past the
    // 4,096-byte bound, or holding a NUL: refused.
    for refused in [
        serde_json::json!({"into": "/elsewhere/review"}),
        serde_json::json!({"into": "relative/review"}),
        export_to(&exports.path().join("a/../review")),
        export_to(exports.path()),
        serde_json::json!({"into": ""}),
        serde_json::json!({ "into": format!("/{}", "a".repeat(4096)) }),
        serde_json::json!({"into": "/tmp/a\u{0}b"}),
    ] {
        let (status, body) = post_json(&app, &uri, refused.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{refused} {body}");
        assert_eq!(body["error"]["code"], "bad_request");
    }

    // A body that is not JSON {into, shell?} answers the error envelope,
    // never the extractor's plain-text rejection: bad JSON, no `into`, a
    // shell rosetta does not write, no content type.
    for (content_type, raw) in [
        (Some("application/json"), "{"),
        (Some("application/json"), "{}"),
        (
            Some("application/json"),
            r#"{"into":"/tmp/x","shell":"dark"}"#,
        ),
        (None, r#"{"into":"/tmp/x"}"#),
    ] {
        let mut request = Request::builder().method("POST").uri(uri.as_str());
        if let Some(content_type) = content_type {
            request = request.header("content-type", content_type);
        }
        let response = app
            .clone()
            .oneshot(request.body(Body::from(raw)).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{raw}");
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["error"]["code"], "bad_request", "{raw}");
    }

    // Another page already where this one goes, or a file where its
    // directory belongs: 409, and neither is touched.
    let taken = exports.path().join("taken");
    std::fs::create_dir(&taken).unwrap();
    std::fs::write(taken.join("index.html"), b"mine").unwrap();
    let file = exports.path().join("file.html");
    std::fs::write(&file, b"mine").unwrap();
    for refused in [&taken, &file] {
        let (status, body) = post_json(&app, &uri, export_to(refused)).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["error"]["code"], "export_conflict");
    }
    assert_eq!(std::fs::read(taken.join("index.html")).unwrap(), b"mine");
    assert_eq!(std::fs::read(&file).unwrap(), b"mine");

    // A directory where the page goes is not a page.
    let odd = exports.path().join("odd");
    std::fs::create_dir_all(odd.join("index.html")).unwrap();
    let (status, body) = post_json(&app, &uri, export_to(&odd)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    // No component below the root may be a link, and neither may the page.
    #[cfg(unix)]
    {
        let elsewhere = TempDir::new().unwrap();
        let link = exports.path().join("link");
        std::os::unix::fs::symlink(elsewhere.path(), &link).unwrap();
        let planted = exports.path().join("planted");
        std::fs::create_dir(&planted).unwrap();
        let target = elsewhere.path().join("index.html");
        std::os::unix::fs::symlink(target, planted.join("index.html")).unwrap();
        for refused in [link.join("review"), link.clone(), planted.clone()] {
            let (status, body) = post_json(&app, &uri, export_to(&refused)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        }
        assert_eq!(std::fs::read_dir(elsewhere.path()).unwrap().count(), 0);
    }

    // Scoped to the owning session, like the content read.
    let other = exports.path().join("other");
    let foreign = format!("/api/sessions/s2/artifacts/{id}/export");
    let (status, _) = post_json(&app, &foreign, export_to(&other)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(!other.exists(), "a refused export makes no directory");
}

#[tokio::test]
async fn a_capture_that_fails_its_hash_or_is_gone_exports_nothing() {
    let mut h = harness();
    h.append(vec![message("html-review", MessageKind::Agent, "Designs")]);
    let (id, reference) = capture_publication(&mut h, "review", "<h1>Review</h1>", 5);
    let exports = TempDir::new().unwrap();
    let app = export_app(&h, exports.path());
    let uri = format!("/api/sessions/s1/artifacts/{id}/export");
    let member = rosetta_store::assets::html_member_path(&h.db, &reference.digest);

    // Tampered (a new file of the same length): 500, and not even the
    // directory is made, with the shell or without.
    let mut tampered = std::fs::read(&member).unwrap();
    tampered[0] ^= 1;
    std::fs::remove_file(&member).unwrap();
    std::fs::write(&member, tampered).unwrap();
    let untouched = exports.path().join("tampered");
    for request in [
        export_to(&untouched),
        serde_json::json!({ "into": untouched.to_str().unwrap(), "shell": "room" }),
    ] {
        let (status, body) = post_json(&app, &uri, request).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
        assert_eq!(body["error"]["code"], "internal");
        assert!(!untouched.exists());
    }

    // Gone from the asset store: 404, and still nothing is made.
    std::fs::remove_file(&member).unwrap();
    let (status, body) = post_json(&app, &uri, export_to(&untouched)).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert!(!untouched.exists());
}

#[tokio::test]
async fn the_room_shell_is_an_option_and_off_by_default() {
    use rosetta_core::html_artifacts::{SHELL_HOSTS, SHELL_VERSION, runnable_document};
    let mut h = harness();
    h.append(vec![message("html-review", MessageKind::Agent, "Designs")]);
    let html = "<h1>Review π</h1>";
    let (id, reference) = capture_publication(&mut h, "review", html, 5);
    let exports = TempDir::new().unwrap();
    let app = export_app(&h, exports.path());
    let uri = format!("/api/sessions/s1/artifacts/{id}/export");

    // Asked for, the room's shell is written around the page as a file of
    // its own, and the answer names it.
    let shelled = exports.path().join("shelled");
    let request = serde_json::json!({ "into": shelled.to_str().unwrap(), "shell": "room" });
    let (status, body) = post_json(&app, &uri, request.clone()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let document = runnable_document(html);
    assert_eq!(
        std::fs::read_to_string(shelled.join("index.html")).unwrap(),
        document
    );
    assert_eq!(body["method"], "copy");
    assert_eq!(
        body["page"],
        serde_json::json!({
            "path": "index.html",
            "blake3": blake3::hash(document.as_bytes()).to_hex().to_string(),
            "bytes": document.len(),
        })
    );
    assert_eq!(
        body["shell"],
        serde_json::json!({
            "version": SHELL_VERSION,
            "source_blake3": reference.digest,
            "hosts": SHELL_HOSTS.all(),
        })
    );
    assert_eq!(
        body["environment"]["hosts"]["font-src"],
        serde_json::json!(SHELL_HOSTS.font)
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let written = std::fs::metadata(shelled.join("index.html")).unwrap();
        let member = rosetta_store::assets::html_member_path(&h.db, &reference.digest);
        assert_ne!(written.ino(), std::fs::metadata(member).unwrap().ino());
        assert_eq!(written.permissions().mode() & 0o777, 0o600);
    }

    // The same request again answers the same; the capture as itself at
    // that path is another page, refused.
    let (status, again) = post_json(&app, &uri, request).await;
    assert_eq!((status, &again), (StatusCode::OK, &body));
    let (status, raw) = post_json(&app, &uri, export_to(&shelled)).await;
    assert_eq!(status, StatusCode::CONFLICT, "{raw}");
    assert_eq!(raw["error"]["code"], "export_conflict");

    // Unasked, the page is its own bytes.
    let plain = exports.path().join("plain");
    let (status, body) = post_json(&app, &uri, export_to(&plain)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.get("shell").is_none(), "{body}");
    assert_eq!(
        std::fs::read_to_string(plain.join("index.html")).unwrap(),
        html
    );
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

    /// Rows for any session, outside a source scope, then indexed and acked.
    fn append_unscoped(&mut self, rows: Vec<MessageRow>) {
        self.store
            .apply(&Batch {
                ops: rows
                    .into_iter()
                    .flat_map(IngestOp::project_message)
                    .collect(),
                checkpoints: vec![],
                source: None,
            })
            .unwrap();
        let ack = synchronize_fts5_once(&self.search, &AtomicBool::new(false))
            .unwrap()
            .unwrap();
        self.store
            .acknowledge_search(&ack.projection_epoch, ack.revision)
            .unwrap();
    }

    /// A delegated child of `s1`, known to the store and the hub mirror.
    fn add_child_of_s1(&mut self, id: &str) {
        let mut child = session(id);
        child.parent_session_id = Some("s1".into());
        child.root_session_id = "s1".into();
        child.role = SessionRole::Subagent;
        self.store
            .apply(&Batch {
                ops: vec![IngestOp::SessionMeta(child)],
                checkpoints: vec![],
                source: None,
            })
            .unwrap();
        self.hub.warm(self.store.conn()).unwrap();
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
                attachments: None,
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
        cost: None,
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
            attachments: None,
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
        attachments: Vec::new(),
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
        attachments: Vec::new(),
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
        attachments: Vec::new(),
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
    let (status, day) = h
        .get(&format!("/api/projects/{scope}/usage/day?day=1970-01-01"))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(day["models"][0]["tokens"], 18);
}

#[tokio::test]
async fn selected_day_usage_reconciles_hours_models_scope_and_timezone() {
    let mut h = harness();
    h.add_child_of_s1("child");
    let make = |id: &str, hour: i64, model: &str, tokens: TokenSnapshot| {
        let mut row = message(id, MessageKind::Agent, "hourly usage");
        row.ts = Some(hour * 3_600_000);
        row.model = Some(model.into());
        row.usage = Some(tokens);
        row
    };
    h.append(vec![
        make(
            "before",
            0,
            "model-a",
            TokenSnapshot {
                input: Some(100),
                ..Default::default()
            },
        ),
        make(
            "first",
            1,
            "model-a",
            TokenSnapshot {
                input: Some(1),
                output: Some(2),
                cache_read: Some(3),
                cache_write: Some(4),
                cache_write_1h: Some(5),
                thinking: Some(6),
                ..Default::default()
            },
        ),
        make(
            "last",
            24,
            "model-a",
            TokenSnapshot {
                input: Some(9),
                ..Default::default()
            },
        ),
        make(
            "after",
            25,
            "model-a",
            TokenSnapshot {
                input: Some(200),
                ..Default::default()
            },
        ),
    ]);
    let mut child = make(
        "delegated",
        24,
        "model-b",
        TokenSnapshot {
            input: Some(7),
            ..Default::default()
        },
    );
    child.session_id = "child".into();
    h.append_unscoped(vec![child]);
    let (status, day) = h
        .get("/api/projects/p1/usage/day?day=1970-01-01&offset_minutes=-60")
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        day,
        serde_json::json!({"day": "1970-01-01", "models": [
            {"hour": 0, "model": "model-a", "tokens": 21},
            {"hour": 23, "model": "model-a", "tokens": 9},
            {"hour": 23, "model": "model-b", "tokens": 7}
        ]})
    );
    let (_, heatmap) = h.get("/api/projects/p1/heatmap?offset_minutes=-60").await;
    let daily_total = heatmap["days"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["day"] == "1970-01-01")
        .unwrap()["tokens"]
        .as_i64()
        .unwrap();
    assert_eq!(
        daily_total,
        day["models"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["tokens"].as_i64().unwrap())
            .sum::<i64>()
    );
    let (_, other) = h.get("/api/projects/other/usage/day?day=1970-01-01").await;
    assert_eq!(other["models"], serde_json::json!([]));
    let (_, shifted) = h
        .get("/api/projects/p1/usage/day?day=1970-01-01&offset_minutes=330")
        .await;
    assert_eq!(shifted["models"][0]["hour"], 5);
    assert_eq!(shifted["models"][1]["hour"], 6);
    for query in [
        "day=2026-02-30",
        "day=2026-13-01",
        "day=nonsense",
        "day=1970-01-01&offset_minutes=841",
    ] {
        let (status, error) = h.get(&format!("/api/projects/p1/usage/day?{query}")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(error["error"]["code"], "bad_request");
    }
}

#[tokio::test]
async fn every_token_figure_carries_its_list_price_spend() {
    let mut h = harness();
    let mut priced = message("priced", MessageKind::Agent, "answer");
    priced.model = Some("claude-opus-4-8".into());
    priced.usage = Some(rosetta_core::rows::TokenSnapshot {
        input: Some(1_000),
        output: Some(100),
        ..Default::default()
    });
    let mut unknown = message("unknown", MessageKind::Agent, "local model");
    unknown.ordinal = 1;
    unknown.model = Some("big-pickle".into());
    unknown.usage = Some(rosetta_core::rows::TokenSnapshot {
        input: Some(40),
        output: Some(2),
        ..Default::default()
    });
    h.append(vec![priced, unknown]);

    // $5/MTok in, $25/MTok out: 1,000 × 5 + 100 × 25 = 7,500 micro-USD.
    let (status, heatmap) = h.get("/api/projects/p1/heatmap").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(heatmap["days"][0]["tokens"], 1_142);
    assert_eq!(heatmap["days"][0]["cost_micro_usd"], 7_500);
    assert_eq!(heatmap["days"][0]["unpriced_tokens"], 42);
    assert!(heatmap["days"][0]["billed_micro_usd"].is_null());

    let (_, contribution) = h.get("/api/projects/p1/contribution").await;
    assert_eq!(contribution["rows"][0]["cost_micro_usd"], 7_500);

    // The transcript prices each response at read time from the same facts.
    let (_, page) = h.get("/api/sessions/s1/timeline").await;
    let costs: Vec<&serde_json::Value> = page["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| &entry["cost"])
        .collect();
    assert!(costs.contains(
        &&serde_json::json!({"status": "priced", "micro_usd": 7_500, "long_context": false})
    ));
    assert!(costs.contains(&&serde_json::json!({"status": "unpriced", "reason": "unknown_model"})));
}

#[tokio::test]
async fn pricing_reports_the_compiled_catalog_and_the_projections() {
    let h = harness();
    let (status, pricing) = h.get("/api/pricing").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(pricing["basis"], "list_price_equivalent");
    assert_eq!(
        pricing["catalog_version"],
        rosetta_pricing::pricing_version()
    );
    assert!(rosetta_pricing::pricing_version().starts_with(rosetta_pricing::CATALOG_VERSION));
    // An opened projection is priced with the compiled catalog and calculator.
    assert_eq!(pricing["projection_version"], pricing["catalog_version"]);
    assert_eq!(pricing["sources"][0]["name"], "pydantic/genai-prices");
    assert!(pricing["models"].as_u64().unwrap() > 100);
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
                    label: None,
                    provenance: Default::default(),
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
            label: None,
            provenance: Default::default(),
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

    // Pricing's coverage report reads the same rollup, which the RAM build
    // has not folded yet: an empty unpriced list would read as full coverage.
    for endpoint in [
        "/api/projects/p1/heatmap",
        "/api/projects/p1/usage",
        "/api/projects/p1/usage/day?day=1970-01-01",
        "/api/projects/p1/contribution",
        "/api/pricing",
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
            label: None,
            provenance: Default::default(),
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

    for endpoint in ["/api/projects/p1/contribution", "/api/pricing"] {
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
        assert_eq!(response.status(), StatusCode::OK, "{endpoint}");
    }

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

/// Two runtimes of one kind: the mirror holds neither label nor provenance
/// (they are this start's enrollment, never a column), and the transport
/// decorates only the enrolled row on the way out.
#[tokio::test]
async fn runtimes_carry_enrolled_labels_and_provenance_from_the_serve_time_map() {
    let mut h = harness();
    h.store
        .apply(&Batch {
            ops: vec![IngestOp::Runtime(RuntimeRow {
                id: "rt-kim".into(),
                kind: "claude-code".into(),
                data_root: "/users/u1/agents/roots/claude-code/9a3d".into(),
                enabled: true,
                account: None,
                config: None,
                skills: None,
                versions_seen: None,
                discovered_at: 2_000,
                label: None,
                provenance: Default::default(),
            })],
            checkpoints: vec![],
            source: None,
        })
        .unwrap();
    h.hub.rewarm(h.store.conn()).unwrap();
    let enrolled = Arc::new(rosetta_server::EnrolledRuntimes::new([(
        "rt-kim".to_string(),
        Some("Kim".to_string()),
    )]));
    let app = router(
        AppState::with_search(h.hub.clone(), h.db.clone(), Arc::new(h.search.clone()))
            .with_enrolled(enrolled),
    );
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/runtimes")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let rows = body["runtimes"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    let kim = rows.iter().find(|row| row["id"] == "rt-kim").unwrap();
    assert_eq!(kim["kind"], "claude-code");
    assert_eq!(kim["label"], "Kim");
    assert_eq!(kim["provenance"], "enrolled");
    let native = rows.iter().find(|row| row["id"] == "rt1").unwrap();
    assert_eq!(native["kind"], "claude-code");
    assert!(native.get("label").is_none(), "{native}");
    assert!(native.get("provenance").is_none(), "{native}");
    // The projection itself never learned either fact.
    let stored = queries::runtime(h.store.conn(), "rt-kim").unwrap().unwrap();
    assert!(stored.label.is_none() && stored.provenance.is_discovered());
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
async fn search_session_scope_widens_roots_and_prefix_widens_terms() {
    let mut h = harness();
    h.add_child_of_s1("s1c");
    let mut in_child = message(
        "m-child",
        MessageKind::Agent,
        "ionosphere from a delegated child",
    );
    in_child.session_id = "s1c".into();
    let mut in_sibling = message("m-s2", MessageKind::Agent, "ionosphere in another root");
    in_sibling.session_id = "s2".into();
    h.append_unscoped(vec![
        message("m-root", MessageKind::User, "ionosphere at the root"),
        in_child,
        in_sibling,
    ]);

    // One named root reaches its delegated child and never the sibling root.
    let (status, body) = h.get("/api/search?q=ionosphere&sessions=s1&limit=10").await;
    assert_eq!(status, StatusCode::OK);
    let parsed: SearchResponse = serde_json::from_value(body).unwrap();
    let mut ids: Vec<&str> = parsed
        .hits
        .iter()
        .map(|hit| hit.timeline_id.as_str())
        .collect();
    ids.sort_unstable();
    assert_eq!(ids, ["m-child", "m-root"]);

    // Two roots are one scope; an unknown id is kept and simply matches nothing.
    let (_, body) = h
        .get("/api/search?q=ionosphere&sessions=s1,s2,not-yet-absorbed&limit=10")
        .await;
    let parsed: SearchResponse = serde_json::from_value(body).unwrap();
    assert_eq!(parsed.hits.len(), 3);

    // The two selectors are alternatives; blank or oversized sets are refused.
    let too_many = (0..=rosetta_search::MAX_SEARCH_SESSIONS)
        .map(|i| format!("x{i}"))
        .collect::<Vec<_>>()
        .join(",");
    for selector in [
        "sessions=s1&session=s1".to_string(),
        "sessions=".to_string(),
        "sessions=s1,,s2".to_string(),
        format!("sessions={too_many}"),
    ] {
        let (status, body) = h.get(&format!("/api/search?q=ionosphere&{selector}")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{selector}");
        assert_eq!(body["error"]["code"], "bad_request", "{selector}");
    }

    // Prefix mode widens a term of three or more characters; phrases and
    // shorter terms stay exact, and the default stays whole-token.
    let (_, body) = h.get("/api/search?q=ionosph&limit=10").await;
    let parsed: SearchResponse = serde_json::from_value(body).unwrap();
    assert!(parsed.hits.is_empty(), "prefix is opt-in");
    let (_, body) = h.get("/api/search?q=ionosph&prefix=true&limit=10").await;
    let parsed: SearchResponse = serde_json::from_value(body).unwrap();
    assert_eq!(parsed.hits.len(), 3);
    let (_, body) = h
        .get("/api/search?q=%22ionosph%20from%22&prefix=true&limit=10")
        .await;
    let parsed: SearchResponse = serde_json::from_value(body).unwrap();
    assert!(parsed.hits.is_empty(), "a phrase stays exact");
    let (_, body) = h.get("/api/search?q=io&prefix=true&limit=10").await;
    let parsed: SearchResponse = serde_json::from_value(body).unwrap();
    assert!(parsed.hits.is_empty(), "a short term stays exact");
    let (_, body) = h
        .get("/api/search?q=ionosph&prefix=true&sessions=s2&limit=10")
        .await;
    let parsed: SearchResponse = serde_json::from_value(body).unwrap();
    assert_eq!(parsed.hits.len(), 1);
    assert_eq!(parsed.hits[0].timeline_id, "m-s2");
}

#[tokio::test]
async fn sessions_order_by_recorded_activity_before_limiting() {
    let mut h = harness();
    let mut stale = message("old", MessageKind::User, "old conversation");
    stale.ts = Some(100);
    let mut recent = message("new", MessageKind::User, "recent conversation");
    recent.session_id = "s2".into();
    recent.ts = Some(200);
    h.append_unscoped(vec![stale, recent]);
    let mut refreshed = session("s1");
    refreshed.updated_at = 99_000_000;
    h.store
        .apply(&Batch {
            ops: vec![IngestOp::SessionMeta(refreshed)],
            ..Batch::default()
        })
        .unwrap();
    h.hub.warm(h.store.conn()).unwrap();
    let (status, body) = h.get("/api/sessions?limit=1").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["sessions"][0]["id"], "s2");
    assert_eq!(body["sessions"][0]["activity"]["last_at"], 200);
}

#[tokio::test]
async fn sessions_resolve_by_native_id_including_children() {
    let mut h = harness();
    h.add_child_of_s1("s1c");

    let (status, body) = h
        .get("/api/sessions?native_session_id=native-s1,native-s1c,native-missing")
        .await;
    assert_eq!(status, StatusCode::OK);
    let parsed: SessionsResponse = serde_json::from_value(body).unwrap();
    let mut ids: Vec<&str> = parsed
        .sessions
        .iter()
        .map(|session| session.id.as_str())
        .collect();
    ids.sort_unstable();
    assert_eq!(ids, ["s1", "s1c"]);
    let child = parsed
        .sessions
        .iter()
        .find(|session| session.id == "s1c")
        .unwrap();
    assert_eq!(child.root_session_id, "s1");

    for selector in ["native_session_id=", "native_session_id=native-s1,%20"] {
        let (status, body) = h.get(&format!("/api/sessions?{selector}")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{selector}");
        assert_eq!(body["error"]["code"], "bad_request", "{selector}");
    }

    // Without the selector the list stays root-only.
    let (_, body) = h.get("/api/sessions").await;
    let parsed: SessionsResponse = serde_json::from_value(body).unwrap();
    assert!(parsed.sessions.iter().all(|session| session.id != "s1c"));
}

#[tokio::test]
async fn search_filters_inside_the_ranked_query_and_rejects_bad_input() {
    let mut h = harness();
    let mut tool_row = message("m-tool", MessageKind::ToolUse, "ionosphere via tool");
    tool_row.ts = Some(7_200_000);
    tool_row.content = None;
    tool_row.tool = Some(ToolInteraction {
        attachments: Vec::new(),
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
        &rosetta_server::RuntimeIdentity::new("rt-native", "claude-code", None),
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
    // A second runtime of the SAME kind is its own status row, keyed by id
    // and carrying the enrollment file's label.
    health.update(
        &rosetta_server::RuntimeIdentity::new("rt-kim", "claude-code", Some("Kim".into())),
        rosetta_server::RuntimeTick::default(),
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
    assert_eq!(daemon["runtimes"].as_array().unwrap().len(), 2);
    assert_eq!(daemon["runtimes"][1]["kind"], "claude-code");
    assert_eq!(daemon["runtimes"][1]["runtime_id"], "rt-native");
    assert!(daemon["runtimes"][1]["label"].is_null());
    assert_eq!(daemon["runtimes"][1]["live_rows"], 7);
    assert_eq!(daemon["runtimes"][1]["last_activity_ms"], 5_000);
    assert_eq!(daemon["runtimes"][0]["runtime_id"], "rt-kim");
    assert_eq!(daemon["runtimes"][0]["kind"], "claude-code");
    assert_eq!(daemon["runtimes"][0]["label"], "Kim");
    assert_eq!(daemon["hub"]["subscribers"], 0);
    assert!(daemon["uptime_ms"].is_number());
}

#[path = "http/image_pipeline.rs"]
mod image_pipeline;
#[path = "http/image_renditions.rs"]
mod image_renditions;
#[path = "http/video_assets.rs"]
mod video_assets;
