//! Every payload route honours per-session admission while a fresh build's
//! canonical RAM generation is unpublished: a Pending session's rows are not
//! public, an admitted sibling's are, whatever tree the other is in. The
//! session watched while its tree flips gets exactly one resync barrier; a
//! session born during the import is simply absent until it is ingested after
//! publication; and once the read router is published, every answer is the
//! one a daemon that never held the barrier gives.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use futures_util::StreamExt;
use http_body_util::BodyExt;
use tempfile::TempDir;
use tokio_tungstenite::tungstenite::Message;
use tower::ServiceExt;

use rosetta_core::rows::{
    MessageKind, MessageRow, PlanRow, ProjectRow, RuntimeRow, SessionRole, SessionRow, StreamState,
};
use rosetta_core::timeline::{
    GroupDerivation, GroupKind, ReadTarget, ResourceRef, ResponseGroup, TimelineBody,
    TimelineEntry, TimelineGroup, WorkflowRun, WorkflowRunStatus, WorkflowUsage, WorkflowWorker,
    WorkflowWorkerState,
};
use rosetta_core::{
    CanonicalText, CorrelationDerivation, IngestOp, NativeToolRef, StatusBasis, ToolAction,
    ToolCorrelation, ToolInteraction, ToolStatus, ToolStatusFact,
};
use rosetta_server::{AppState, Hub, HubConfig, ProjectionReader, RawCommandReader, router};
use rosetta_store::{Batch, CheckpointUpdate, Store, queries};

const HOUR: i64 = 3_600_000;
const WINDOW: &str = "since_ms=0&until_ms=86400000";

fn runtime() -> RuntimeRow {
    RuntimeRow {
        id: "rt1".into(),
        kind: "codex".into(),
        data_root: "~/.codex".into(),
        enabled: true,
        account: None,
        config: None,
        skills: None,
        versions_seen: None,
        discovered_at: 1,
        label: None,
        provenance: Default::default(),
    }
}

fn session(id: &str, parent: Option<&str>, state: StreamState) -> SessionRow {
    SessionRow {
        id: id.into(),
        native_session_id: format!("native-{id}"),
        runtime_id: "rt1".into(),
        project_id: Some("p1".into()),
        cwd: Some("/w/app".into()),
        parent_session_id: parent.map(Into::into),
        delegation: None,
        root_session_id: parent.unwrap_or(id).into(),
        role: if parent.is_some() {
            SessionRole::Subagent
        } else {
            SessionRole::Main
        },
        title: Some(format!("{id} title")),
        summary: None,
        started_at: 1_000,
        updated_at: 1_000,
        activity: None,
        stream_state: state,
    }
}

fn row(session: &str, name: &str, ordinal: i64, at: i64) -> MessageRow {
    MessageRow {
        timeline_id: format!("{session}-{name}"),
        session_id: session.into(),
        ordinal,
        kind: MessageKind::Agent,
        subtype: None,
        uuid: None,
        parent_uuid: None,
        ts: Some(at),
        model: None,
        effort: None,
        content: Some(format!("{session} says {name}")),
        tool: None,
        attachments: Vec::new(),
        citations: Vec::new(),
        group: Some(TimelineGroup {
            id: format!("{session}-turn"),
            kind: GroupKind::HumanTurn,
            derivation: GroupDerivation::Native,
        }),
        response: None,
        block_index: None,
        branch_tool_id: None,
        usage: None,
        agent_version: None,
        src_cursor: None,
        src_offset: None,
    }
}

fn tool(key: &str, name: &str, action: ToolAction) -> ToolInteraction {
    ToolInteraction {
        attachments: Vec::new(),
        correlation: ToolCorrelation {
            key: key.into(),
            derivation: CorrelationDerivation::NativeId,
        },
        native: NativeToolRef {
            id: Some(key.into()),
            namespace: None,
            name: name.into(),
            provider: None,
        },
        status: ToolStatusFact {
            value: ToolStatus::Succeeded,
            basis: StatusBasis::NativeTerminalRecord,
        },
        action,
        failure: None,
        timing: None,
    }
}

/// One session's payload: a modelled response, a shell call anchored to its
/// raw record, a recorded file read and a plan revision.
fn payload(session: &str, cursor: i64, at: i64, file: &std::path::Path) -> Vec<IngestOp> {
    let mut agent = row(session, "agent", 0, at);
    agent.model = Some(format!("model-{session}"));
    agent.response = Some(ResponseGroup {
        id: format!("{session}-response"),
        derivation: GroupDerivation::Native,
    });
    agent.block_index = Some(0);
    let mut shell = row(session, "shell", 1, at + 1_000);
    shell.kind = MessageKind::ToolUse;
    shell.content = None;
    shell.src_cursor = Some(cursor);
    shell.src_offset = Some(42);
    shell.tool = Some(tool(
        &format!("{session}-shell"),
        "Bash",
        ToolAction::Shell {
            command: CanonicalText::inline("echo one\necho two").unwrap(),
            source_truncated: false,
            cwd: None,
            timeout_ms: None,
            background: false,
            outcome: None,
        },
    ));
    let mut read = row(session, "read", 2, at + 2_000);
    read.kind = MessageKind::ToolUse;
    read.content = None;
    read.tool = Some(tool(
        &format!("{session}-read"),
        "Read",
        ToolAction::FileRead {
            targets: vec![ReadTarget {
                resource: ResourceRef::File {
                    path: file.to_string_lossy().into_owned(),
                    resolved: true,
                },
                range: None,
                media_kind: None,
            }],
            outcome: None,
        },
    ));
    let mut ops: Vec<IngestOp> = [agent, shell, read]
        .into_iter()
        .flat_map(IngestOp::project_message)
        .collect();
    ops.push(IngestOp::Plan(PlanRow {
        session_id: session.into(),
        revision: 1,
        content: format!("# {session} plan\n"),
        updated_at: at,
    }));
    ops
}

/// A Workflow invocation in `session` whose one worker ran in `child`: its
/// observed runtime profile is the child's rows, hydrated at read time.
fn workflow(session: &str, child: &str, ordinal: i64, at: i64) -> Vec<IngestOp> {
    let mut invocation = row(session, "workflow", ordinal, at);
    invocation.kind = MessageKind::ToolUse;
    invocation.content = None;
    invocation.tool = Some(tool(
        &format!("{session}-workflow"),
        "Workflow",
        ToolAction::Workflow {
            name: Some("review".into()),
            path: None,
            arguments: None,
            objective: None,
            resume_id: None,
            background: true,
            task_id: None,
            run_id: Some("wf".into()),
            attempt: Some(1),
            callback_summary: None,
            callback_result: None,
            usage: None,
            run: None,
        },
    ));
    let run = WorkflowRun {
        run_id: "wf".into(),
        task_id: None,
        name: Some("review".into()),
        summary: None,
        status: WorkflowRunStatus::Completed,
        started_at: Some(at),
        updated_at: Some(at),
        usage: WorkflowUsage {
            duration_ms: None,
            tokens: None,
            tool_calls: None,
            agent_count: Some(1),
        },
        default_model: None,
        arguments: None,
        script_path: None,
        script: None,
        phases: vec![],
        workers: vec![WorkflowWorker {
            index: 1,
            key: Some("worker".into()),
            label: "review".into(),
            phase_index: None,
            phase_title: None,
            state: WorkflowWorkerState::Done,
            cached: false,
            configured_model: None,
            configured_effort: None,
            observed_models: vec![],
            observed_efforts: vec![],
            child_session_id: Some(child.into()),
            prompt_preview: None,
            result_preview: None,
            last_tool_name: None,
            last_tool_summary: None,
            queued_at: None,
            started_at: None,
            last_progress_at: None,
            duration_ms: None,
            tokens: None,
            tool_calls: None,
            error: None,
        }],
        logs: vec![],
        result_kind: None,
        result: None,
        error: None,
        resources: vec![],
        attempts: vec![],
        attempt_total_count: 0,
        journal: vec![],
        journal_total_count: 0,
        journal_started_count: 0,
        journal_result_count: 0,
    };
    let mut ops = IngestOp::project_message(invocation);
    ops.push(IngestOp::WorkflowRun {
        session_id: session.into(),
        run: Box::new(run),
    });
    ops
}

/// The models a served Workflow row observed in its worker's child.
fn observed_models(entry: &serde_json::Value) -> Vec<String> {
    let entry: TimelineEntry = serde_json::from_value(entry.clone()).unwrap();
    let TimelineBody::ToolUse { tool } = entry.body else {
        panic!("expected the workflow row");
    };
    let ToolAction::Workflow { run: Some(run), .. } = tool.action else {
        panic!("the workflow run hydrates");
    };
    run.workers[0].observed_models.clone()
}

/// The served Workflow row among `entries`.
fn workflow_entry(entries: &serde_json::Value) -> &serde_json::Value {
    entries
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["timeline_id"] == "open-workflow")
        .expect("the workflow row is served")
}

/// The raw ledger behind the command route: whether the admitted record is
/// already on disk is the test's to say.
struct Commands {
    saved: AtomicBool,
    seen: Mutex<Vec<String>>,
}

impl RawCommandReader for Commands {
    fn read_command(&self, source: &queries::CommandSource) -> Result<Option<String>, String> {
        self.seen.lock().unwrap().push(source.session_id.clone());
        Ok(self
            .saved
            .load(Ordering::SeqCst)
            .then(|| format!("full command of {}", source.timeline_id)))
    }
}

struct Build {
    _dir: TempDir,
    db: PathBuf,
    store: Store,
    hub: Arc<Hub>,
    projection: Arc<ProjectionReader>,
    commands: Arc<Commands>,
    app: Router,
    addr: SocketAddr,
    server: tokio::task::JoinHandle<()>,
}

impl Drop for Build {
    fn drop(&mut self) {
        self.server.abort();
    }
}

/// A fresh build mid-import: metadata public for every session; `open`'s
/// tree is admitted (with one Pending child), `held`'s sibling tree is not.
async fn build(name: &str) -> Build {
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("projection.sqlite");
    let file = dir.path().join("recorded.txt");
    std::fs::write(&file, b"recorded bytes\n").unwrap();
    let uri = format!(
        "file:rosetta-admission-{name}-{}?mode=memory&cache=shared",
        std::process::id()
    );
    let mut store = Store::open_shared_in_memory_for_rebuild_with_assets(&uri, &db).unwrap();
    let checkpoint = |path: &str, session: &str| CheckpointUpdate {
        runtime_id: "rt1".into(),
        stream: "transcript".into(),
        path: path.into(),
        session_id: Some(session.into()),
        expected: None,
        next: "blob-v0".into(),
        updated_at: Some(1),
    };
    store
        .apply(&Batch {
            ops: vec![
                IngestOp::Runtime(runtime()),
                IngestOp::Project(ProjectRow {
                    id: "p1".into(),
                    name: "app".into(),
                    root_paths: vec!["/w/app".into()],
                    git_repo: None,
                }),
                IngestOp::SessionMeta(session("open", None, StreamState::Indexed)),
                IngestOp::SessionMeta(session("open-child", Some("open"), StreamState::Pending)),
                IngestOp::SessionMeta(session("held", None, StreamState::Pending)),
            ],
            checkpoints: vec![
                checkpoint("p/open.jsonl", "open"),
                checkpoint("p/held.jsonl", "held"),
            ],
            source: None,
        })
        .unwrap();
    let cursor = |path: &str| {
        queries::cursor_state(store.conn(), "rt1", path)
            .unwrap()
            .unwrap()
            .id
    };
    let (open_cursor, held_cursor) = (cursor("p/open.jsonl"), cursor("p/held.jsonl"));
    let mut ops = payload("open", open_cursor, HOUR, &file);
    ops.extend(payload("held", held_cursor, 2 * HOUR, &file));
    // The Pending child carries the runtime profile `open`'s Workflow worker
    // observed; the invocation sits inside `open`'s existing activity bounds.
    let mut child = row("open-child", "agent", 0, 5 * HOUR);
    child.model = Some("child-model".into());
    ops.extend(IngestOp::project_message(child));
    ops.extend(workflow("open", "open-child", 3, HOUR + 500));
    store
        .apply(&Batch {
            ops,
            ..Batch::default()
        })
        .unwrap();
    store.observe_meta_effects();

    let hub = Arc::new(Hub::new(HubConfig::default(), 1));
    hub.set_hide_pending(true);
    hub.warm(store.conn()).unwrap();
    let projection = Arc::new(ProjectionReader::shared_memory(&uri).unwrap());
    let commands = Arc::new(Commands {
        saved: AtomicBool::new(true),
        seen: Mutex::new(Vec::new()),
    });
    let app = router(
        AppState::new(hub.clone(), db.clone())
            .with_projection(projection.clone())
            .with_raw_commands(commands.clone()),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn({
        let app = app.clone();
        async move {
            axum::serve(listener, app).await.unwrap();
        }
    });
    Build {
        _dir: dir,
        db,
        store,
        hub,
        projection,
        commands,
        app,
        addr,
        server,
    }
}

async fn get(app: &Router, uri: &str) -> (StatusCode, Vec<u8>) {
    let response = app
        .clone()
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, bytes.to_vec())
}

async fn get_json(app: &Router, uri: &str) -> (StatusCode, serde_json::Value) {
    let (status, bytes) = get(app, uri).await;
    (status, serde_json::from_slice(&bytes).unwrap())
}

/// The error envelope's code and message.
fn error(body: &serde_json::Value) -> (&str, &str) {
    (
        body["error"]["code"].as_str().unwrap(),
        body["error"]["message"].as_str().unwrap(),
    )
}

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn ws(addr: SocketAddr, path: &str) -> Socket {
    tokio_tungstenite::connect_async(format!("ws://{addr}{path}"))
        .await
        .unwrap()
        .0
}

async fn frame(socket: &mut Socket) -> serde_json::Value {
    loop {
        let message = tokio::time::timeout(Duration::from_secs(10), socket.next())
            .await
            .expect("a frame arrived")
            .expect("the stream is open")
            .expect("the frame is readable");
        if let Message::Text(text) = message {
            return serde_json::from_str(text.as_str()).unwrap();
        }
    }
}

/// No frame arrives within a short quiet window.
async fn quiet(socket: &mut Socket) {
    match tokio::time::timeout(Duration::from_millis(300), socket.next()).await {
        Err(_) => {}
        Ok(Some(Ok(Message::Ping(_) | Message::Pong(_)))) => {}
        Ok(other) => panic!("expected no frame, got {other:?}"),
    }
}

fn entry_ids(entries: &serde_json::Value) -> Vec<String> {
    entries
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["timeline_id"].as_str().unwrap().to_owned())
        .collect()
}

/// Flip `ids` out of Pending the way the writer admits a tree: the readiness
/// commit, then the watched sessions' barrier, then the mirror's diffs.
fn admit(build: &mut Build, ids: &[&str]) {
    let states: Vec<_> = ids
        .iter()
        .map(|id| (id.to_string(), StreamState::Indexed))
        .collect();
    build.store.set_session_stream_states(&states).unwrap();
    build.hub.invalidate_sessions(ids.iter().copied(), 1);
    let effects = build.store.drain_meta_effects();
    build.hub.absorb_meta(build.store.conn(), effects).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_payload_route_hides_pending_and_serves_admitted_sessions_during_a_build() {
    let b = build("matrix").await;
    let app = &b.app;

    // Metadata is public for every session; payload-derived activity is not.
    let (status, listed) = get_json(app, "/api/sessions").await;
    assert_eq!(status, StatusCode::OK);
    let ids: Vec<_> = listed["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&"open") && ids.contains(&"held"));
    let (_, held) = get_json(app, "/api/sessions/held").await;
    assert_eq!(held["session"]["stream_state"], "pending");
    assert!(held["session"]["activity"].is_null());
    let (_, open) = get_json(app, "/api/sessions/open").await;
    // The Pending child's later row never reaches its admitted root.
    assert_eq!(open["session"]["activity"]["first_at"], HOUR);
    assert_eq!(open["session"]["activity"]["last_at"], HOUR + 2_000);
    let (_, child) = get_json(app, "/api/sessions/open-child").await;
    assert!(child["session"]["activity"].is_null());

    // Timeline, collaboration and exact rows.
    let (status, page) = get_json(app, "/api/sessions/held/timeline").await;
    assert_eq!(status, StatusCode::OK);
    assert!(page["entries"].as_array().unwrap().is_empty());
    assert!(page["watermark"].is_string());
    let (_, page) = get_json(app, "/api/sessions/open/timeline").await;
    assert_eq!(
        entry_ids(&page["entries"]),
        ["open-agent", "open-shell", "open-read", "open-workflow"]
    );
    // The admitted root's Workflow row hydrates, but its worker's Pending
    // child has observed nothing public yet — on the page and the exact row.
    assert!(observed_models(workflow_entry(&page["entries"])).is_empty());
    let (status, exact) = get_json(app, "/api/sessions/open/timeline/open-workflow").await;
    assert_eq!(status, StatusCode::OK);
    assert!(observed_models(&exact["entry"]).is_empty());
    let (_, collaboration) = get_json(app, "/api/sessions/held/collaboration").await;
    assert!(collaboration["entries"].as_array().unwrap().is_empty());
    assert!(
        collaboration["response_runtimes"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let (_, collaboration) = get_json(app, "/api/sessions/open/collaboration").await;
    assert_eq!(collaboration["response_runtimes"][0]["model"], "model-open");
    for (uri, admitted) in [
        ("/api/sessions/held/timeline/held-agent", false),
        ("/api/sessions/open/timeline/open-agent", true),
        ("/api/sessions/held/timeline/held-read/read/0", false),
        ("/api/sessions/open/timeline/open-read/read/0", true),
        ("/api/sessions/held/plans/revisions/1", false),
        ("/api/sessions/open/plans/revisions/1", true),
        ("/api/sessions/held/timeline/held-agent/plan", false),
        ("/api/sessions/held/timeline/held-agent/work", false),
        ("/api/sessions/held/artifacts/any/content", false),
    ] {
        let (status, _) = get(app, uri).await;
        let expected = if admitted {
            StatusCode::OK
        } else {
            StatusCode::NOT_FOUND
        };
        assert_eq!(status, expected, "{uri}");
    }

    // Full commands: per-session admission, never a publication gate.
    let (status, body) = get_json(app, "/api/sessions/held/timeline/held-shell/command").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        error(&body),
        ("not_ready", "this conversation is still importing")
    );
    let (status, body) = get_json(app, "/api/sessions/open/timeline/open-shell/command").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["command"], "full command of open-shell");
    assert_eq!(*b.commands.seen.lock().unwrap(), ["open"]);
    b.commands.saved.store(false, Ordering::SeqCst);
    let (status, body) = get_json(app, "/api/sessions/open/timeline/open-shell/command").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(error(&body), ("not_ready", "still saving · retry"));

    // Activity: the admitted root's turns are its own; a Pending root has no
    // activity bounds, so it is not selected at all.
    let (status, activity) = get_json(app, &format!("/api/activity?{WINDOW}")).await;
    assert_eq!(status, StatusCode::OK);
    let sessions = activity["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1, "{activity}");
    assert_eq!(sessions[0]["session"]["id"], "open");
    assert_eq!(sessions[0]["turns_withheld"], false);
    let turns: Vec<_> = sessions[0]["turns"]
        .as_array()
        .unwrap()
        .iter()
        .map(|turn| turn["group_id"].as_str().unwrap())
        .collect();
    assert_eq!(turns, ["open-turn"]);

    // Aggregates stay not_ready and search stays Building until publication.
    for uri in [
        "/api/projects/p1/heatmap",
        "/api/projects/p1/usage",
        "/api/projects/p1/usage/day?day=1970-01-01",
        "/api/projects/p1/contribution",
        "/api/pricing",
    ] {
        let (status, body) = get_json(app, uri).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{uri}");
        assert_eq!(error(&body).0, "not_ready", "{uri}");
    }
    let (_, search) = get_json(app, "/api/search?q=says").await;
    assert_eq!(search["state"]["readiness"], "building");
    assert!(search["hits"].as_array().unwrap().is_empty());
    let (_, status) = get_json(app, "/api/status").await;
    assert_eq!(status["canonical_ready"], false);
    assert_eq!(get(app, "/api/artifacts").await.0, StatusCode::OK);

    // The same matrix on the sockets.
    let mut held = ws(b.addr, "/ws/sessions/held").await;
    assert_eq!(frame(&mut held).await["frame"], "hello");
    let snapshot = frame(&mut held).await;
    assert_eq!(snapshot["frame"], "timeline_snapshot");
    assert!(snapshot["entries"].as_array().unwrap().is_empty());
    let mut open = ws(b.addr, "/ws/sessions/open").await;
    frame(&mut open).await;
    let snapshot = frame(&mut open).await;
    assert_eq!(entry_ids(&snapshot["entries"]).len(), 4);
    assert!(observed_models(workflow_entry(&snapshot["entries"])).is_empty());
    let mut collaboration = ws(b.addr, "/ws/collaboration/held").await;
    frame(&mut collaboration).await;
    let snapshot = frame(&mut collaboration).await;
    assert_eq!(snapshot["frame"], "collaboration_snapshot");
    assert!(snapshot["response_runtimes"].as_array().unwrap().is_empty());
    let mut meta = ws(b.addr, "/ws/meta").await;
    frame(&mut meta).await;
    let snapshot = frame(&mut meta).await;
    let held_row = snapshot["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == "held")
        .unwrap();
    assert!(held_row["activity"].is_null());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_tree_that_flips_mid_subscription_resyncs_its_watchers_exactly_once() {
    let mut b = build("flip").await;
    let mut held = ws(b.addr, "/ws/sessions/held").await;
    let hello = frame(&mut held).await;
    let stamp = hello["stamp"].clone();
    assert!(
        frame(&mut held).await["entries"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let mut sibling = ws(b.addr, "/ws/sessions/open-child").await;
    frame(&mut sibling).await;
    assert!(
        frame(&mut sibling).await["entries"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let mut open = ws(b.addr, "/ws/sessions/open").await;
    frame(&mut open).await;
    frame(&mut open).await;
    let mut meta = ws(b.addr, "/ws/meta").await;
    frame(&mut meta).await;
    frame(&mut meta).await;
    let mut collaboration = ws(b.addr, "/ws/collaboration/held").await;
    frame(&mut collaboration).await;
    assert!(
        frame(&mut collaboration).await["response_runtimes"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    admit(&mut b, &["held"]);

    // The watched transcript: one barrier, then the admitted snapshot.
    assert_eq!(frame(&mut held).await["frame"], "resync");
    let snapshot = frame(&mut held).await;
    assert_eq!(snapshot["frame"], "timeline_snapshot");
    assert_eq!(
        entry_ids(&snapshot["entries"]),
        ["held-agent", "held-shell", "held-read"]
    );
    quiet(&mut held).await;
    // Its collaboration socket takes the same single barrier.
    assert_eq!(frame(&mut collaboration).await["frame"], "resync");
    let snapshot = frame(&mut collaboration).await;
    assert_eq!(snapshot["frame"], "collaboration_snapshot");
    assert_eq!(snapshot["response_runtimes"][0]["model"], "model-held");
    quiet(&mut collaboration).await;
    // The sibling tree and the already-admitted tree hear nothing.
    quiet(&mut sibling).await;
    quiet(&mut open).await;
    // /ws/meta announces the new readiness with the activity it unlocked.
    let mutation = frame(&mut meta).await;
    assert_eq!(mutation["frame"], "mutation");
    assert_eq!(mutation["mutation"]["type"], "session");
    assert_eq!(mutation["mutation"]["id"], "held");
    assert_eq!(mutation["mutation"]["stream_state"], "indexed");
    assert_eq!(mutation["mutation"]["activity"]["first_at"], 2 * HOUR);
    quiet(&mut meta).await;

    // A reconnect from before the flip cannot replay into a stale view.
    let path = format!(
        "/ws/sessions/held?epoch={}&seq={}",
        stamp["epoch"], stamp["seq"]
    );
    let mut again = ws(b.addr, &path).await;
    assert_eq!(frame(&mut again).await["frame"], "hello");
    let snapshot = frame(&mut again).await;
    assert_eq!(snapshot["frame"], "timeline_snapshot");
    assert_eq!(entry_ids(&snapshot["entries"]).len(), 3);

    // REST agrees at once, the full command included.
    let (_, page) = get_json(&b.app, "/api/sessions/held/timeline").await;
    assert_eq!(entry_ids(&page["entries"]).len(), 3);
    let (status, _) = get_json(&b.app, "/api/sessions/held/timeline/held-shell/command").await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_ring_gap_for_a_pending_session_is_never_replayed() {
    let mut b = build("replay").await;
    let mut held = ws(b.addr, "/ws/sessions/held").await;
    let stamp = frame(&mut held).await["stamp"].clone();
    frame(&mut held).await;
    drop(held);

    // A row mutation reaches the Pending session's ring (a path the cold
    // writer does not take today; the route must not depend on that).
    b.store.observe_effects();
    b.store
        .apply(&Batch {
            ops: IngestOp::project_message(row("held", "late", 3, 3 * HOUR)),
            ..Batch::default()
        })
        .unwrap();
    let effects = b.store.drain_effects();
    assert!(!effects.mutations.is_empty());
    b.hub.absorb(b.store.conn(), effects, 1).unwrap();

    let path = format!(
        "/ws/sessions/held?epoch={}&seq={}",
        stamp["epoch"], stamp["seq"]
    );
    let mut again = ws(b.addr, &path).await;
    assert_eq!(frame(&mut again).await["frame"], "hello");
    let next = frame(&mut again).await;
    assert_eq!(next["frame"], "timeline_snapshot", "{next}");
    assert!(next["entries"].as_array().unwrap().is_empty());
    quiet(&mut again).await;
    // The mirror absorbed that drain behind the barrier too.
    let (_, row) = get_json(&b.app, "/api/sessions/held").await;
    assert!(row["session"]["activity"].is_null());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_session_born_during_the_import_is_absent_until_ingested_after_publication() {
    let mut b = build("born").await;
    let app = b.app.clone();
    for uri in [
        "/api/sessions/born",
        "/api/sessions/born/timeline",
        "/api/sessions/born/collaboration",
        "/api/sessions/born/timeline/born-agent",
        "/api/sessions/born/timeline/born-shell/command",
        "/api/sessions/born/plans/revisions/1",
    ] {
        assert_eq!(get(&app, uri).await.0, StatusCode::NOT_FOUND, "{uri}");
    }
    let (_, activity) = get_json(&app, &format!("/api/activity?{WINDOW}&sessions=born")).await;
    assert!(activity["sessions"].as_array().unwrap().is_empty());
    let mut socket = ws(b.addr, "/ws/sessions/born").await;
    assert_eq!(frame(&mut socket).await["frame"], "hello");
    assert_eq!(frame(&mut socket).await["frame"], "error");

    // Publication: the validated durable copy replaces the RAM read target,
    // then the barrier lifts, then the live runner ingests the newborn.
    admit(&mut b, &["held"]);
    publish(&mut b);
    let mut disk = Store::open(&b.db).unwrap();
    disk.observe_meta_effects();
    let mut ops = vec![IngestOp::SessionMeta(session(
        "born",
        None,
        StreamState::Indexed,
    ))];
    ops.extend(IngestOp::project_message(row("born", "agent", 0, 6 * HOUR)));
    disk.apply(&Batch {
        ops,
        ..Batch::default()
    })
    .unwrap();
    let effects = disk.drain_meta_effects();
    b.hub.absorb_meta(disk.conn(), effects).unwrap();
    let (status, page) = get_json(&app, "/api/sessions/born/timeline").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(entry_ids(&page["entries"]), ["born-agent"]);
    let (_, row) = get_json(&app, "/api/sessions/born").await;
    assert_eq!(row["session"]["activity"]["first_at"], 6 * HOUR);
}

/// Install the RAM generation as the durable projection and switch the read
/// router to it, as `publish_fresh_ram_projection` does, then lift the
/// mirror's barrier.
fn publish(b: &mut Build) {
    b.store.finish_bulk_load().unwrap();
    b.store
        .conn()
        .execute("VACUUM INTO ?1", [b.db.to_string_lossy()])
        .unwrap();
    b.projection.publish_file(b.db.clone());
    let disk = Store::open(&b.db).unwrap();
    b.hub.reveal_pending(disk.conn()).unwrap();
    assert!(!b.hub.hides_pending());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn after_publication_every_answer_is_the_never_hidden_daemons() {
    let mut b = build("published").await;
    admit(&mut b, &["held"]);
    publish(&mut b);
    // `open-child` is still Pending in the published file: the barrier is
    // down, so its rows count exactly as they always did after publication.
    let reference_hub = Arc::new(Hub::new(HubConfig::default(), 1));
    reference_hub
        .warm(Store::open(&b.db).unwrap().conn())
        .unwrap();
    let reference =
        router(AppState::new(reference_hub, b.db.clone()).with_raw_commands(b.commands.clone()));
    let (_, open) = get_json(&b.app, "/api/sessions/open").await;
    assert_eq!(open["session"]["activity"]["last_at"], 5 * HOUR);
    // So does the profile the Workflow worker observed in it.
    let (_, page) = get_json(&b.app, "/api/sessions/open/timeline").await;
    assert_eq!(
        observed_models(workflow_entry(&page["entries"])),
        ["child-model"]
    );
    for uri in [
        "/api/sessions",
        "/api/sessions/open",
        "/api/sessions/open-child",
        "/api/sessions/held",
        "/api/sessions/open/timeline",
        "/api/sessions/open-child/timeline",
        "/api/sessions/held/timeline",
        "/api/sessions/held/collaboration",
        "/api/sessions/held/timeline/held-agent",
        "/api/sessions/open-child/timeline/open-child-agent",
        "/api/sessions/open/timeline/open-workflow",
        "/api/sessions/held/timeline/held-shell/command",
        "/api/sessions/held/plans/revisions/1",
        "/api/sessions/held/timeline/held-read/read/0",
        &format!("/api/activity?{WINDOW}"),
        "/api/projects/p1/contribution",
    ] {
        let ours = get(&b.app, uri).await;
        let theirs = get(&reference, uri).await;
        assert_eq!(ours.0, theirs.0, "{uri}");
        assert_eq!(
            String::from_utf8_lossy(&ours.1),
            String::from_utf8_lossy(&theirs.1),
            "{uri}"
        );
    }
    // A record still missing after publication is final, not "still saving".
    b.commands.saved.store(false, Ordering::SeqCst);
    let (status, body) = get_json(&b.app, "/api/sessions/held/timeline/held-shell/command").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(error(&body).0, "not_found");
}

/// One captured page `name` in `session`, at `at`: its publication id and the
/// ops that write it, its capture and its content member.
fn captured_page(session: &str, name: &str, at: i64) -> (String, Vec<IngestOp>) {
    use rosetta_core::ProjectionAsset;
    use rosetta_core::html_artifacts::{
        HTML_MEDIA_TYPE, HtmlArtifactAvailability, HtmlArtifactContent, HtmlArtifactPublication,
        identity,
    };
    let asset =
        ProjectionAsset::new(HTML_MEDIA_TYPE, format!("<h1>{name}</h1>").into_bytes()).unwrap();
    let id = identity(&["publication", name]);
    let path = format!("/w/app/{name}.html");
    let offset = at as u64;
    let ops = vec![
        IngestOp::ProjectionAsset {
            asset: asset.clone(),
        },
        IngestOp::HtmlArtifact(HtmlArtifactPublication {
            id: id.clone(),
            artifact_id: identity(&["artifact", name]),
            session_id: session.into(),
            timeline_id: format!("{name}-page"),
            title: name.into(),
            filename: format!("{name}.html"),
            path: Some(path.clone()),
            url: None,
            native_id: None,
            version: None,
            published_at: Some(at),
            source_offset: offset,
            description: None,
            dropped_files: Some(0),
        }),
        IngestOp::HtmlArtifactContent(HtmlArtifactContent {
            session_id: session.into(),
            source_offset: offset,
            path: Some(path),
            availability: HtmlArtifactAvailability::Captured,
            captured_at: Some(at),
            recovered_from: None,
            asset: Some(asset.reference()),
        }),
    ];
    (id, ops)
}

/// The library revisions a meta subscriber was told of since the last call.
fn announced(meta: &rosetta_server::Subscription) -> Vec<u64> {
    let mut revisions = Vec::new();
    while let Some(item) = meta.try_recv() {
        if let rosetta_server::HubItem::Mutation { mutation, .. } = item
            && let rosetta_store::hot::Mutation::ArtifactsChanged { revision } = mutation.as_ref()
        {
            revisions.push(*revision);
        }
    }
    revisions
}

fn titles(page: &serde_json::Value) -> Vec<String> {
    page["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["publication"]["title"].as_str().unwrap().to_owned())
        .collect()
}

/// The next page's URI: `next_cursor`, percent-encoded as a query value.
fn next_page(page: &serde_json::Value) -> String {
    let cursor: String = page["next_cursor"]
        .as_str()
        .expect("a next cursor")
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() {
                char::from(byte).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect();
    format!("/api/artifacts?limit=1&cursor={cursor}")
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
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_library_cursor_signal_and_export_follow_admission_and_the_barriers_lift() {
    let mut b = build("library").await;
    let meta = b.hub.subscribe_meta();
    // Two pages in the admitted `open`, one in Pending `held`, one in the
    // Pending child `open-child`, which publication leaves Pending.
    let mut ops = Vec::new();
    for (session, name, at) in [
        ("open", "open-a", HOUR),
        ("open", "open-b", HOUR + 1_000),
        ("held", "held-a", 2 * HOUR),
        ("open-child", "child-a", 3 * HOUR),
    ] {
        ops.extend(captured_page(session, name, at).1);
    }
    let held_id = captured_page("held", "held-a", 2 * HOUR).0;
    b.store
        .apply(&Batch {
            ops,
            ..Batch::default()
        })
        .unwrap();
    let effects = b.store.drain_meta_effects();
    b.hub.absorb_meta(b.store.conn(), effects).unwrap();
    // A cold write moves the library, and the mirror says so (RO-35).
    assert_eq!(announced(&meta), [1]);

    // Behind the barrier only the admitted session's pages are listed.
    let (status, first) = get_json(&b.app, "/api/artifacts?limit=1").await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(titles(&first), ["open-b"]);
    let walk = next_page(&first);
    let (status, rest) = get_json(&b.app, &walk).await;
    assert_eq!(status, StatusCode::OK, "{rest}");
    assert_eq!(titles(&rest), ["open-a"]);

    // The export door answers a held session as the content read does: not
    // found, and nothing is written.
    let exports = TempDir::new().unwrap();
    let door = router(
        AppState::new(b.hub.clone(), b.db.clone())
            .with_projection(b.projection.clone())
            .with_export_root(rosetta_server::ExportRoot::new(exports.path()).unwrap()),
    );
    let export = format!("/api/sessions/held/artifacts/{held_id}/export");
    let target = exports.path().join("held");
    let into = serde_json::json!({ "into": target.to_str().unwrap() });
    let (status, body) = post_json(&door, &export, into.clone()).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert!(!target.exists(), "a held session's export writes nothing");

    // Admitting `held` shows its page: the flip steps the library, the
    // mirror announces it, and the walk begun before it expires.
    admit(&mut b, &["held"]);
    assert_eq!(announced(&meta), [2]);
    let (status, expired) = get_json(&b.app, &walk).await;
    assert_eq!(status, StatusCode::GONE, "{expired}");
    assert_eq!(error(&expired).0, "cursor_expired");
    let (_, library) = get_json(&b.app, "/api/artifacts").await;
    assert_eq!(titles(&library), ["held-a", "open-b", "open-a"]);
    let (status, body) = post_json(&door, &export, into).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let digest = blake3::hash(b"<h1>held-a</h1>").to_hex().to_string();
    assert_eq!(body["page"]["blake3"], digest.as_str());
    assert_eq!(body["method"], "link");
    assert_eq!(
        std::fs::read(target.join("index.html")).unwrap(),
        b"<h1>held-a</h1>"
    );
    // A late meta subscriber learns where the library stands.
    let mut socket = ws(b.addr, "/ws/meta").await;
    assert_eq!(frame(&mut socket).await["frame"], "hello");
    let snapshot = frame(&mut socket).await;
    assert_eq!(snapshot["frame"], "meta_snapshot");
    assert_eq!(snapshot["artifacts_revision"], 2);

    // Publication lifts the barrier: `open-child` is still Pending, so its
    // page appears with no revision step. The lift announces the library once
    // at the same revision, and a cursor read behind the barrier expires.
    let (_, hidden) = get_json(&b.app, "/api/artifacts?limit=1").await;
    let behind = next_page(&hidden);
    publish(&mut b);
    assert_eq!(announced(&meta), [2]);
    let (status, expired) = get_json(&b.app, &behind).await;
    assert_eq!(status, StatusCode::GONE, "{expired}");
    assert_eq!(error(&expired).0, "cursor_expired");
    let (_, library) = get_json(&b.app, "/api/artifacts").await;
    assert_eq!(titles(&library), ["child-a", "held-a", "open-b", "open-a"]);
    // A cursor read after publication walks on.
    let (_, head) = get_json(&b.app, "/api/artifacts?limit=1").await;
    assert_eq!(titles(&head), ["child-a"]);
    let (status, rest) = get_json(&b.app, &next_page(&head)).await;
    assert_eq!(status, StatusCode::OK, "{rest}");
    assert_eq!(titles(&rest), ["held-a"]);
}
