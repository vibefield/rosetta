//! `GET /api/activity` over a real file-backed projection: which top-level
//! sessions were active in a window, each tree's turns (one per message
//! group, a delegated thread's under its root), the gaps inside a turn longer
//! than the threshold — counted between activity instants, so a tool held
//! open over a wait is a gap — and the bounds an ask must keep.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

use rosetta_core::rows::{
    MessageKind, MessageRow, ProjectRow, RuntimeRow, SessionRole, SessionRow, StreamState,
};
use rosetta_core::timeline::{GroupDerivation, GroupKind, TimelineGroup, ToolTiming};
use rosetta_core::{CorrelationDerivation, IngestOp, ToolInteraction};
use rosetta_protocol::{ActivityResponse, ApiError, ErrorCode, TurnGap};
use rosetta_server::{AppState, Hub, HubConfig, router};
use rosetta_store::{Batch, Store};
use tempfile::TempDir;

const MIN: i64 = 60_000;
const HOUR: i64 = 60 * MIN;
/// The window's end: a day of activity before it.
const T: i64 = 100 * 24 * HOUR;

fn session(id: &str, parent: Option<&str>) -> SessionRow {
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
        title: None,
        summary: None,
        started_at: T - 2 * 24 * HOUR,
        updated_at: T - 2 * 24 * HOUR,
        activity: None,
        stream_state: StreamState::Streaming,
    }
}

fn message(session: &str, ordinal: i64, group: &str, kind: GroupKind, at: i64) -> MessageRow {
    MessageRow {
        timeline_id: format!("{session}-{ordinal}"),
        session_id: session.into(),
        ordinal,
        kind: MessageKind::Agent,
        subtype: None,
        uuid: None,
        parent_uuid: None,
        ts: Some(at),
        model: None,
        effort: None,
        content: Some(format!("row {ordinal}")),
        tool: None,
        attachments: Vec::new(),
        citations: Vec::new(),
        group: Some(TimelineGroup {
            id: group.into(),
            kind,
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

/// A tool row whose bounds are its request and its completion, nothing between.
fn tool(session: &str, ordinal: i64, group: &str, started: i64, completed: i64) -> MessageRow {
    let mut row = message(session, ordinal, group, GroupKind::HumanTurn, started);
    row.kind = MessageKind::ToolUse;
    row.content = None;
    let mut interaction = ToolInteraction::unknown(
        format!("call-{ordinal}"),
        CorrelationDerivation::NativeId,
        "Bash",
    );
    interaction.timing = Some(ToolTiming {
        started_at: Some(started),
        completed_at: Some(completed),
        duration_ms: None,
    });
    row.tool = Some(interaction);
    row
}

struct Harness {
    _dir: TempDir,
    app: Router,
}

fn harness(rows: Vec<MessageRow>) -> Harness {
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("projection.sqlite");
    let mut store = Store::open(&db).unwrap();
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
            discovered_at: 1_000,
            label: None,
            provenance: Default::default(),
        }),
        IngestOp::Project(ProjectRow {
            id: "p1".into(),
            name: "app".into(),
            root_paths: vec!["/w/app".into()],
            git_repo: None,
        }),
        IngestOp::SessionMeta(session("s1", None)),
        IngestOp::SessionMeta(session("c1", Some("s1"))),
        IngestOp::SessionMeta(session("s2", None)),
        IngestOp::SessionMeta(session("old", None)),
    ];
    ops.extend(rows.into_iter().flat_map(IngestOp::project_message));
    store
        .apply(&Batch {
            ops,
            checkpoints: vec![],
            source: None,
        })
        .unwrap();
    let hub = Arc::new(Hub::new(HubConfig::default(), 1));
    hub.warm(store.conn()).unwrap();
    Harness {
        _dir: dir,
        app: router(AppState::new(hub, db)),
    }
}

impl Harness {
    async fn get(&self, uri: &str) -> (StatusCode, serde_json::Value) {
        let response = self
            .app
            .clone()
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    async fn activity(&self, query: &str) -> ActivityResponse {
        let (status, body) = self.get(&format!("/api/activity?{query}")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        serde_json::from_value(body).unwrap()
    }

    async fn refused(&self, query: &str) -> ApiError {
        let (status, body) = self.get(&format!("/api/activity?{query}")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{query}: {body}");
        serde_json::from_value(body["error"].clone()).unwrap()
    }
}

/// s1: a human turn with a 40-minute silence, a delegated thread's own group,
/// then an agent cycle; s2: one short turn, recently; old: a day before.
fn day() -> Vec<MessageRow> {
    vec![
        message("s1", 0, "s1-t1", GroupKind::HumanTurn, T - 5 * HOUR),
        message(
            "s1",
            1,
            "s1-t1",
            GroupKind::HumanTurn,
            T - 5 * HOUR + 10 * MIN,
        ),
        message(
            "s1",
            2,
            "s1-t1",
            GroupKind::HumanTurn,
            T - 5 * HOUR + 50 * MIN,
        ),
        message("c1", 0, "c1-t1", GroupKind::ParentInvocation, T - 4 * HOUR),
        message(
            "c1",
            1,
            "c1-t1",
            GroupKind::ParentInvocation,
            T - 4 * HOUR + 5 * MIN,
        ),
        message("s1", 3, "s1-t2", GroupKind::AgentCycle, T - 3 * HOUR),
        message(
            "s1",
            4,
            "s1-t2",
            GroupKind::AgentCycle,
            T - 3 * HOUR + 2 * MIN,
        ),
        message("s2", 0, "s2-t1", GroupKind::HumanTurn, T - 30 * MIN),
        message("s2", 1, "s2-t1", GroupKind::HumanTurn, T - 20 * MIN),
        message("old", 0, "old-t1", GroupKind::HumanTurn, T - 30 * HOUR),
    ]
}

fn window() -> String {
    format!("since_ms={}&until_ms={T}", T - 24 * HOUR)
}

#[tokio::test]
async fn sessions_active_in_the_window_come_newest_first_with_their_trees_turns() {
    let h = harness(day());
    let answer = h.activity(&window()).await;
    assert_eq!((answer.since_ms, answer.until_ms), (T - 24 * HOUR, T));
    assert_eq!(answer.gap_ms, 15 * MIN);
    assert!(!answer.truncated);
    let ids: Vec<_> = answer
        .sessions
        .iter()
        .map(|s| s.session.id.as_str())
        .collect();
    // roots only, inside the window, the newest recorded activity first
    assert_eq!(ids, ["s2", "s1"]);
    let s1 = &answer.sessions[1];
    assert!(!s1.turns_withheld);
    let turns: Vec<_> = s1
        .turns
        .iter()
        .map(|t| (t.group_id.as_str(), t.kind, t.first_at, t.last_at))
        .collect();
    assert_eq!(
        turns,
        [
            (
                "s1-t1",
                GroupKind::HumanTurn,
                T - 5 * HOUR,
                T - 5 * HOUR + 50 * MIN
            ),
            // the delegated thread's group is its root's turn too
            (
                "c1-t1",
                GroupKind::ParentInvocation,
                T - 4 * HOUR,
                T - 4 * HOUR + 5 * MIN
            ),
            (
                "s1-t2",
                GroupKind::AgentCycle,
                T - 3 * HOUR,
                T - 3 * HOUR + 2 * MIN
            ),
        ]
    );
    // the 40-minute silence is a gap; the 10-minute pause is not
    assert_eq!(
        s1.turns[0].gaps,
        [TurnGap {
            from_ms: T - 5 * HOUR + 10 * MIN,
            to_ms: T - 5 * HOUR + 50 * MIN,
        }]
    );
    assert!(s1.turns[1].gaps.is_empty() && s1.turns[2].gaps.is_empty());
}

#[tokio::test]
async fn the_window_bounds_the_turns_and_a_narrowed_ask_answers_only_its_sessions() {
    let h = harness(day());
    // the last four hours: s1's first turn ended before them
    let answer = h
        .activity(&format!("since_ms={}&until_ms={T}", T - 4 * HOUR - MIN))
        .await;
    let s1 = answer
        .sessions
        .iter()
        .find(|s| s.session.id == "s1")
        .unwrap();
    let groups: Vec<_> = s1.turns.iter().map(|t| t.group_id.as_str()).collect();
    assert_eq!(groups, ["c1-t1", "s1-t2"]);
    // named: only s1, and a child id names no lane of its own
    let named = h.activity(&format!("{}&sessions=s1,c1", window())).await;
    let ids: Vec<_> = named
        .sessions
        .iter()
        .map(|s| s.session.id.as_str())
        .collect();
    assert_eq!(ids, ["s1"]);
    // a limit keeps the newest and says it cut
    let limited = h.activity(&format!("{}&limit=1", window())).await;
    assert_eq!(limited.sessions.len(), 1);
    assert_eq!(limited.sessions[0].session.id, "s2");
    assert!(limited.truncated);
    // a window that misses everything is an honest empty answer
    let empty = h
        .activity(&format!("since_ms={}&until_ms={}", T + HOUR, T + 2 * HOUR))
        .await;
    assert!(empty.sessions.is_empty());
}

#[tokio::test]
async fn a_long_silence_counts_from_instants_and_the_threshold_is_the_askers() {
    let mut rows = vec![
        message("s1", 0, "s1-t1", GroupKind::HumanTurn, T - 2 * HOUR),
        // a tool row held open 40 minutes: its bounds record nothing between
        tool("s1", 1, "s1-t1", T - 2 * HOUR + MIN, T - HOUR - 19 * MIN),
        message("s1", 2, "s1-t1", GroupKind::HumanTurn, T - HOUR - 18 * MIN),
    ];
    rows.extend(day().into_iter().filter(|row| row.session_id == "s2"));
    let h = harness(rows);
    let answer = h.activity(&window()).await;
    let s1 = answer
        .sessions
        .iter()
        .find(|s| s.session.id == "s1")
        .unwrap();
    assert_eq!(s1.turns.len(), 1);
    assert_eq!(
        s1.turns[0].gaps,
        [TurnGap {
            from_ms: T - 2 * HOUR + MIN,
            to_ms: T - HOUR - 19 * MIN,
        }]
    );
    // asked about silences over an hour, the same turn has none
    let coarse = h.activity(&format!("{}&gap_ms={}", window(), HOUR)).await;
    let s1 = coarse
        .sessions
        .iter()
        .find(|s| s.session.id == "s1")
        .unwrap();
    assert!(s1.turns[0].gaps.is_empty());
    assert_eq!(coarse.gap_ms, HOUR);
}

#[tokio::test]
async fn an_ask_keeps_its_bounds() {
    let h = harness(day());
    for query in [
        String::new(),
        format!("since_ms={T}&until_ms={T}"),
        format!("since_ms={T}&until_ms={}", T - 1),
        format!("since_ms={}&until_ms={T}", T - 7 * 24 * HOUR - 1),
        format!("{}&gap_ms=59999", window()),
        format!("{}&sessions=s1,,s2", window()),
    ] {
        assert_eq!(
            h.refused(&query).await.code,
            ErrorCode::BadRequest,
            "{query}"
        );
    }
    // a week is the widest window
    h.activity(&format!("since_ms={}&until_ms={T}", T - 7 * 24 * HOUR))
        .await;
}
