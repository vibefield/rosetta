//! Wire-stability goldens: the serialized JSON of every wire type, pinned.
//! The TypeScript bindings regenerate from these Rust types, but nothing
//! else fails when a serde rename silently breaks every deployed client —
//! this does. If this test fails, the wire changed: either revert the
//! accidental break, or bump `PROTOCOL_VERSION` deliberately and refresh
//! the golden with `UPDATE_WIRE_GOLDEN=1 cargo test -p rosetta-protocol`.

use rosetta_protocol::{
    ApiError, BackfillState, BoostRequest, BoostResponse, ContributionResponse, ContributionRow,
    DaemonStatus, DiscoveriesResponse, DiscoveryRow, EnableRequest, EnableResponse, ErrorCode,
    HeatmapDay, HeatmapResponse, HourTotal, HubStatus, IngestPhase, IngestProgress, MessageKind,
    MessageRow, ModelDay, Mutation, PROTOCOL_VERSION, ProjectRow, ProjectsResponse,
    RuntimeBackfill, RuntimeIngestStatus, RuntimeRow, RuntimesResponse, SearchHit, SearchReadiness,
    SearchRequest, SearchResponse, SearchState, ServerFrame, SessionResponse, SessionRole,
    SessionRow, SessionsResponse, Stamp, StatusResponse, StreamState, TimelinePage, TokenSnapshot,
    ToolCall, UsageResponse,
};

const GOLDEN_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden/wire.json");

fn runtime_row() -> RuntimeRow {
    RuntimeRow {
        id: "rt1".into(),
        kind: "claude-code".into(),
        data_root: "~/.claude".into(),
        enabled: true,
        account: Some(serde_json::json!({"email": "user@example.com"})),
        config: Some(serde_json::json!({"theme": "dark"})),
        skills: Some(serde_json::json!(["review"])),
        versions_seen: Some(serde_json::json!(["2.1.240"])),
        discovered_at: 1_000,
    }
}

fn project_row() -> ProjectRow {
    ProjectRow {
        id: "p1".into(),
        name: "rosetta".into(),
        root_paths: vec!["/w/rosetta".into()],
        git_repo: Some("git@example.com:w/rosetta.git".into()),
    }
}

fn session_row() -> SessionRow {
    SessionRow {
        id: "s1".into(),
        native_session_id: "native-s1".into(),
        runtime_id: "rt1".into(),
        project_id: Some("p1".into()),
        parent_session_id: Some("s0".into()),
        root_session_id: "s0".into(),
        role: SessionRole::Subagent,
        title: Some("Named".into()),
        summary: Some("A summary".into()),
        started_at: 10_000,
        updated_at: 20_000,
        stream_state: StreamState::Streaming,
    }
}

fn message_row() -> MessageRow {
    MessageRow {
        timeline_id: "t1".into(),
        session_id: "s1".into(),
        ordinal: 7,
        kind: MessageKind::ToolUse,
        subtype: Some("sub".into()),
        uuid: Some("u1".into()),
        parent_uuid: Some("u0".into()),
        ts: Some(3_600_000),
        model: Some("claude-opus-5".into()),
        effort: Some("high".into()),
        content: Some("body".into()),
        tool: Some(ToolCall {
            tool_id: Some("toolu_1".into()),
            name: "Bash".into(),
            input: serde_json::json!({"command": "true"}),
            result: Some("ok".into()),
            payload: Some(serde_json::json!({"exit": 0})),
            is_error: Some(false),
        }),
        branch_tool_id: Some("toolu_0".into()),
        usage: Some(TokenSnapshot {
            input: Some(10),
            output: Some(5),
            cache_read: Some(3),
            cache_write: Some(2),
            thinking: Some(1),
        }),
        agent_version: Some("2.1.240".into()),
        src_cursor: Some(4),
        src_offset: Some(1024),
    }
}

fn search_state() -> SearchState {
    SearchState {
        readiness: SearchReadiness::CatchingUp,
        projection_epoch: Some("epoch-1".into()),
        revision: Some(42),
    }
}

fn stamp() -> Stamp {
    Stamp { epoch: 9, seq: 3 }
}

fn exemplars() -> serde_json::Value {
    let status = StatusResponse {
        protocol_version: PROTOCOL_VERSION,
        schema_version: 5,
        canonical_ready: true,
        search: search_state(),
        daemon: Some(DaemonStatus {
            started_at: 1_000,
            uptime_ms: 60_000,
            runtimes: vec![RuntimeIngestStatus {
                kind: "claude-code".into(),
                ticks: 100,
                events: 5,
                polls: 20,
                rescans: 1,
                admissions: 2,
                live_rows: 12,
                last_activity_ms: Some(50_000),
            }],
            hub: HubStatus {
                subscribers: 2,
                rings: 1,
                session_mutations: 12,
                meta_events: 4,
                resyncs: 1,
                rings_evicted: 0,
                overflow_rewarms: 0,
            },
            ingest: IngestProgress {
                phase: IngestPhase::Backfilling,
                runtimes: vec![RuntimeBackfill {
                    kind: "claude-code".into(),
                    state: BackfillState::Streaming,
                    files_total: 1_209,
                    bytes_total: 2_147_483_648,
                    files_skimmed: 1_209,
                    bytes_streamed: 1_073_741_824,
                    sessions_total: 5_912,
                    records: 214_032,
                }],
            },
        }),
    };
    let mutations = vec![
        Mutation::Message(Box::new(message_row())),
        Mutation::ToolResult {
            session_id: "s1".into(),
            timeline_id: "t1".into(),
            result: Some("ok".into()),
            payload: Some(serde_json::json!({"exit": 0})),
            is_error: Some(false),
            at: Some(3_700_000),
        },
        Mutation::Usage {
            session_id: "s1".into(),
            timeline_id: "t1".into(),
            ts: Some(3_600_000),
            model: Some("claude-opus-5".into()),
            tokens: TokenSnapshot {
                input: Some(10),
                output: Some(5),
                cache_read: None,
                cache_write: None,
                thinking: None,
            },
        },
        Mutation::SessionSwept {
            session_id: "s1".into(),
        },
        Mutation::Session(Box::new(session_row())),
        Mutation::Project(Box::new(project_row())),
        Mutation::Runtime(Box::new(runtime_row())),
    ];
    let frames = vec![
        ServerFrame::Hello {
            protocol_version: PROTOCOL_VERSION,
            stamp: Some(stamp()),
        },
        ServerFrame::TimelineSnapshot {
            stamp: stamp(),
            messages: vec![message_row()],
        },
        ServerFrame::MetaSnapshot {
            runtimes: vec![runtime_row()],
            projects: vec![project_row()],
            sessions: vec![session_row()],
        },
        ServerFrame::Mutation {
            stamp: Some(stamp()),
            mutation: Mutation::SessionSwept {
                session_id: "s1".into(),
            },
        },
        ServerFrame::Resync,
        ServerFrame::Error {
            error: ApiError {
                code: ErrorCode::Internal,
                message: "boom".into(),
            },
        },
    ];
    serde_json::json!({
        "protocol_version": PROTOCOL_VERSION,
        "status": status,
        "runtimes": RuntimesResponse { runtimes: vec![runtime_row()] },
        "projects": ProjectsResponse { projects: vec![project_row()] },
        "sessions": SessionsResponse { sessions: vec![session_row()] },
        "session": SessionResponse { session: session_row() },
        "timeline_page": TimelinePage {
            messages: vec![message_row()],
            cursor: Some("{\"session_id\":\"s1\",\"before\":7,\"watermark\":\"1.2.3\"}".into()),
        },
        "search_request": SearchRequest {
            q: "needle".into(),
            limit: Some(50),
            session: Some("s1".into()),
            runtime: Some("rt1".into()),
            agent: Some("claude-code".into()),
            project: Some("p1".into()),
            kind: Some("tool_use".into()),
            is_error: Some(false),
            after_ts: Some(1),
            before_ts: Some(2),
        },
        "search_response": SearchResponse {
            state: search_state(),
            hits: vec![SearchHit {
                timeline_id: "t1".into(),
                session_id: "s1".into(),
                snippet: "[needle] in a haystack".into(),
                score: -1.5,
            }],
        },
        "search_readiness": [
            SearchReadiness::Ready,
            SearchReadiness::Building,
            SearchReadiness::CatchingUp,
            SearchReadiness::Stale,
            SearchReadiness::Failed,
        ],
        "error_codes": [
            ErrorCode::NotFound,
            ErrorCode::BadRequest,
            ErrorCode::CursorExpired,
            ErrorCode::Internal,
        ],
        "heatmap": HeatmapResponse {
            days: vec![HeatmapDay { day: "2026-08-26".into(), tokens: 123 }],
        },
        "usage": UsageResponse {
            models: vec![
                ModelDay { day: "2026-08-26".into(), model: "claude-opus-5".into(), tokens: 100 },
                ModelDay { day: "2026-08-26".into(), model: "unknown".into(), tokens: 23 },
            ],
            hours: vec![HourTotal { hour: 15, tokens: 123 }],
        },
        "ingest_phases": [
            IngestPhase::Discovering,
            IngestPhase::AwaitingConsent,
            IngestPhase::Backfilling,
            IngestPhase::Live,
        ],
        "discoveries": DiscoveriesResponse {
            discoveries: vec![DiscoveryRow {
                kind: "claude-code".into(),
                data_root: "~/.claude".into(),
                installed: true,
                files: 1_209,
                bytes: 2_147_483_648,
                newest_modified_ms: 1_000,
                enabled: false,
            }],
        },
        "enable_request": EnableRequest { kinds: vec!["claude-code".into()] },
        "enable_response": EnableResponse { enabled: vec!["claude-code".into()] },
        "boost_request": BoostRequest { session_id: "s1".into() },
        "boost_response": BoostResponse { queued: true },
        "contribution": ContributionResponse {
            rows: vec![ContributionRow {
                session_id: "s1".into(),
                project_id: Some("p1".into()),
                tokens: 5,
                commits: 1,
                edits: 2,
                loc: 30,
                messages: 41,
            }],
        },
        "mutations": mutations,
        "frames": frames,
    })
}

#[test]
fn the_wire_shape_is_pinned() {
    let rendered = serde_json::to_string_pretty(&exemplars()).expect("wire types serialize");
    if std::env::var_os("UPDATE_WIRE_GOLDEN").is_some() {
        std::fs::create_dir_all(std::path::Path::new(GOLDEN_PATH).parent().unwrap()).unwrap();
        std::fs::write(GOLDEN_PATH, rendered + "\n").unwrap();
        return;
    }
    let golden = std::fs::read_to_string(GOLDEN_PATH).expect(
        "tests/golden/wire.json missing — generate it with \
         UPDATE_WIRE_GOLDEN=1 cargo test -p rosetta-protocol",
    );
    assert_eq!(
        golden.trim_end(),
        rendered,
        "\nTHE WIRE CHANGED. Every deployed client parses the old shape.\n\
         Either revert the accidental serde change, or — for a deliberate\n\
         contract change — bump PROTOCOL_VERSION, regenerate the TypeScript\n\
         bindings, and refresh this golden with\n\
         UPDATE_WIRE_GOLDEN=1 cargo test -p rosetta-protocol\n"
    );
}
