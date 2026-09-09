//! Wire-stability goldens: the serialized JSON of every wire type, pinned.
//! The TypeScript bindings regenerate from these Rust types, but nothing
//! else fails when a serde rename silently breaks every deployed client —
//! this does. If this test fails, the wire changed: either revert the
//! accidental break, or bump `PROTOCOL_VERSION` deliberately and refresh
//! the golden with `UPDATE_WIRE_GOLDEN=1 cargo test -p rosetta-protocol`.

use rosetta_core::TaskStatus;
use rosetta_core::timeline::{
    ArtifactKind, AssetRef, CanonicalText, CollaborationChannel, CollaborationEvent,
    CollaborationEventKind, CollaborationStatus, CorrelationDerivation, GroupDerivation, GroupKind,
    NativeToolRef, Notice, ParticipantColor, Provenance, ResponseGroup, ShellOutcome, StatusBasis,
    TaskOp, TaskSnapshot, TaskSnapshotItem, TimelineBody, TimelineEntry, TimelineGroup, ToolAction,
    ToolCorrelation, ToolInteraction, ToolStatus, ToolStatusFact, WorkflowRunStatus,
};
use rosetta_protocol::{
    ApiError, BackfillState, BoostRequest, BoostResponse, CollaborationTimeline, CommandResponse,
    ContributionResponse, ContributionRow, DaemonStatus, DiscoveriesResponse, DiscoveryRow,
    EnableRequest, EnableResponse, ErrorCode, HeatmapDay, HeatmapResponse, HourTotal, HubStatus,
    IngestPhase, IngestProgress, ModelDay, Mutation, PROTOCOL_VERSION, ProjectBackfill, ProjectRow,
    ProjectsResponse, ResponseRuntime, RuntimeBackfill, RuntimeIngestStatus, RuntimeRow,
    RuntimesResponse, SearchHit, SearchReadiness, SearchRequest, SearchResponse, SearchState,
    ServerFrame, SessionResponse, SessionRole, SessionRow, SessionsResponse, Stamp, StatusResponse,
    StreamState, TimelineEntryResponse, TimelineMeta, TimelineMetaArtifact,
    TimelineMetaArtifactSource, TimelineMetaPlan, TimelineMetaPlanSource, TimelineMetaTask,
    TimelineMetaWorkflow, TimelinePage, TokenSnapshot, UsageResponse,
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
        cwd: Some("/w/rosetta".into()),
        parent_session_id: Some("s0".into()),
        delegation: None,
        root_session_id: "s0".into(),
        role: SessionRole::Subagent,
        title: Some("Named".into()),
        summary: Some("A summary".into()),
        started_at: 10_000,
        updated_at: 20_000,
        activity: Some(rosetta_protocol::SessionActivity { first_at: 11_000, last_at: 19_000 }),
        stream_state: StreamState::Streaming,
    }
}

fn tool_interaction() -> ToolInteraction {
    ToolInteraction {
        correlation: ToolCorrelation {
            key: "toolu_1".into(),
            derivation: CorrelationDerivation::NativeId,
        },
        native: NativeToolRef {
            id: Some("toolu_1".into()),
            namespace: None,
            name: "Bash".into(),
            provider: None,
        },
        status: ToolStatusFact {
            value: ToolStatus::Succeeded,
            basis: StatusBasis::NativeTerminalRecord,
        },
        action: ToolAction::Shell {
            command: CanonicalText::inline("one").unwrap(),
            source_truncated: true,
            cwd: None,
            timeout_ms: None,
            background: false,
            outcome: Some(ShellOutcome {
                exit_code: Some(0),
                signal: None,
                process_id: None,
            }),
        },
        failure: None,
        timing: None,
    }
}

fn task_action() -> ToolAction {
    ToolAction::Task {
        operation: TaskOp::Update,
        task_id: Some("T-1".into()),
        subject: Some("Ship the task board".into()),
        description: None,
        status: Some(TaskStatus::Completed),
        owner: Some("architect".into()),
        blocker_ids: vec![],
        snapshot: Some(TaskSnapshot {
            tasks: vec![TaskSnapshotItem {
                task_id: "T-1".into(),
                subject: Some("Ship the task board".into()),
                status: Some(TaskStatus::Completed),
                owner: Some("architect".into()),
                blocker_ids: vec![],
            }],
            total_count: 1,
        }),
    }
}

fn timeline_entry() -> TimelineEntry {
    TimelineEntry {
        timeline_id: "t1".into(),
        session_id: "s1".into(),
        ordinal: 7,
        group: TimelineGroup {
            id: "turn-1".into(),
            kind: GroupKind::HumanTurn,
            derivation: GroupDerivation::Native,
        },
        response: Some(ResponseGroup {
            id: "response-1".into(),
            derivation: GroupDerivation::Native,
        }),
        block_index: Some(0),
        native_id: Some("u1".into()),
        parent_native_id: Some("u0".into()),
        at: Some(3_600_000),
        model: Some("claude-opus-5".into()),
        effort: Some("high".into()),
        body: TimelineBody::ToolUse {
            tool: Box::new(tool_interaction()),
        },
        parent_tool_native_id: Some("toolu_0".into()),
        usage: TokenSnapshot {
            input: Some(10),
            output: Some(5),
            cache_read: Some(3),
            cache_write: Some(2),
            thinking: Some(1),
        },
        provenance: Provenance {
            agent_version: Some("2.1.240".into()),
            src_cursor: Some(4),
            src_offset: Some(1024),
        },
    }
}

fn command_output_entry() -> TimelineEntry {
    TimelineEntry {
        timeline_id: "t-command-output".into(),
        session_id: "s1".into(),
        ordinal: 8,
        group: TimelineGroup {
            id: "command-cycle".into(),
            kind: GroupKind::SystemCycle,
            derivation: GroupDerivation::Native,
        },
        response: None,
        block_index: None,
        native_id: Some("command-output-1".into()),
        parent_native_id: Some("command-1".into()),
        at: Some(3_700_000),
        model: None,
        effort: None,
        body: TimelineBody::Notice {
            notice: Notice::CommandOutput {
                output: CanonicalText::inline("Set model to Fable 5.1").unwrap(),
            },
        },
        parent_tool_native_id: None,
        usage: TokenSnapshot::default(),
        provenance: Provenance {
            agent_version: Some("2.1.240".into()),
            src_cursor: Some(4),
            src_offset: Some(2048),
        },
    }
}

fn collaboration_entry() -> TimelineEntry {
    TimelineEntry {
        timeline_id: "team-report-1".into(),
        session_id: "s1".into(),
        ordinal: 9,
        group: TimelineGroup {
            id: "team-delivery-1".into(),
            kind: GroupKind::SystemCycle,
            derivation: GroupDerivation::Native,
        },
        response: None,
        block_index: None,
        native_id: Some("team-native-1".into()),
        parent_native_id: Some("team-parent-1".into()),
        at: Some(3_800_000),
        model: None,
        effort: None,
        body: TimelineBody::Collaboration {
            event: Box::new(CollaborationEvent {
                kind: CollaborationEventKind::Report,
                channel: CollaborationChannel::Team,
                sender: Some("w2-docs".into()),
                recipient: Some("coordinator".into()),
                team: Some("refinement".into()),
                color: Some(ParticipantColor::Green),
                summary: Some("design docs updated".into()),
                body: Some(CanonicalText::inline("All design deltas landed.").unwrap()),
                task_id: Some("task-2".into()),
                delivery_id: Some("delivery-2".into()),
                status: Some(CollaborationStatus::Completed),
                authored_at: Some(3_799_000),
                source_session_id: Some("team-member-s2".into()),
                source_response_id: Some("team-response-2".into()),
                work_start_timeline_id: Some("team-work-start".into()),
                work_end_timeline_id: Some("team-work-end".into()),
            }),
        },
        parent_tool_native_id: None,
        usage: TokenSnapshot::default(),
        provenance: Provenance {
            agent_version: Some("2.1.240".into()),
            src_cursor: Some(4),
            src_offset: Some(3072),
        },
    }
}

fn search_state() -> SearchState {
    SearchState {
        readiness: SearchReadiness::CatchingUp,
        progress: None,
        projection_epoch: Some("epoch-1".into()),
        revision: Some(42),
    }
}

fn stamp() -> Stamp {
    Stamp { epoch: 9, seq: 3 }
}

fn timeline_meta() -> TimelineMeta {
    TimelineMeta {
        tasks: vec![TimelineMetaTask {
            id: "task-1".into(),
            subject: "Ship the work index".into(),
            status: Some(TaskStatus::Completed),
            started_at: Some(3_600_000),
            terminal_at: Some(3_610_000),
            timeline_id: Some("task-message-1".into()),
        }],
        workflows: vec![TimelineMetaWorkflow {
            id: "workflow-run-1".into(),
            name: "Review in parallel".into(),
            status: WorkflowRunStatus::Completed,
            started_at: Some(3_601_000),
            duration_ms: Some(8_000),
            worker_count: Some(3),
            child_session_ids: vec!["workflow-child-1".into()],
            timeline_id: Some("workflow-message-1".into()),
        }],
        plans: vec![
            TimelineMetaPlan {
                id: "revision:2".into(),
                name: "plan-r2.json".into(),
                updated_at: Some(3_605_000),
                byte_len: 128,
                source: TimelineMetaPlanSource::Revision { revision: 2 },
            },
            TimelineMetaPlan {
                id: "timeline:primary".into(),
                name: "Implementation plan.md".into(),
                updated_at: Some(3_606_000),
                byte_len: 96,
                source: TimelineMetaPlanSource::Timeline {
                    timeline_id: "plan-message-1".into(),
                },
            },
        ],
        artifacts: vec![
            TimelineMetaArtifact {
                id: "file:/w/rosetta/report.md".into(),
                name: "report.md".into(),
                artifact_kind: ArtifactKind::Document,
                created_at: Some(3_608_000),
                byte_len: None,
                media_type: Some("text/markdown".into()),
                source: TimelineMetaArtifactSource::TimelineFile {
                    timeline_id: "artifact-message-1".into(),
                    target_index: 0,
                    path: "/w/rosetta/report.md".into(),
                },
            },
            TimelineMetaArtifact {
                id: "asset:digest".into(),
                name: "preview.png".into(),
                artifact_kind: ArtifactKind::Image,
                created_at: Some(3_609_000),
                byte_len: Some(42),
                media_type: Some("image/png".into()),
                source: TimelineMetaArtifactSource::Asset {
                    asset: AssetRef {
                        url: "/api/assets/digest.png".into(),
                        digest: "digest".into(),
                        media_type: "image/png".into(),
                        byte_len: 42,
                    },
                },
            },
        ],
    }
}

fn exemplars() -> serde_json::Value {
    let status = StatusResponse {
        protocol_version: PROTOCOL_VERSION,
        schema_version: 16,
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
                projects: vec![
                    ProjectBackfill {
                        project_id: Some("p1".into()),
                        sessions_total: 214,
                        sessions_done: 33,
                        bytes_total: 536_870_912,
                        bytes_streamed: 88_080_384,
                    },
                    ProjectBackfill {
                        project_id: None,
                        sessions_total: 12,
                        sessions_done: 12,
                        bytes_total: 1_048_576,
                        bytes_streamed: 1_048_576,
                    },
                ],
            },
        }),
    };
    let mutations = vec![
        Mutation::Message(Box::new(timeline_entry())),
        Mutation::Message(Box::new(command_output_entry())),
        Mutation::Message(Box::new(collaboration_entry())),
        Mutation::ToolUpdated {
            session_id: "s1".into(),
            timeline_id: "t1".into(),
            tool: Box::new(tool_interaction()),
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
        Mutation::TimelineMetaChanged {
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
            entries: vec![timeline_entry(), collaboration_entry()],
            timeline_meta: timeline_meta(),
            cursor: None,
            watermark: "1.2.3".into(),
        },
        ServerFrame::TimelineMetaSnapshot {
            stamp: Some(stamp()),
            timeline_meta: timeline_meta(),
        },
        ServerFrame::CaughtUp { stamp: stamp() },
        ServerFrame::CollaborationSnapshot {
            entries: vec![collaboration_entry()],
            response_runtimes: vec![],
        },
        ServerFrame::ResponseRuntime {
            observation: ResponseRuntime {
                response_id: "r1".into(),
                model: Some("model".into()),
                effort: None,
            },
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
            watermark: "1.2.3".into(),
            entries: vec![timeline_entry(), collaboration_entry()],
            cursor: Some("{\"session_id\":\"s1\",\"before\":7,\"watermark\":\"1.2.3\"}".into()),
        },
        "collaboration_timeline": CollaborationTimeline {
            entries: vec![collaboration_entry()],
            response_runtimes: vec![ResponseRuntime {
                response_id: "response-team-1".into(),
                model: Some("claude-opus-4-1".into()),
                effort: Some("high".into()),
            }],
        },
        "timeline_entry": TimelineEntryResponse { entry: timeline_entry(), watermark: "1.2.3".into() },
        "task_action": task_action(),
        "command": CommandResponse { command: "printf 'full source'".into() },
        "search_request": SearchRequest {
            q: "needle".into(),
            limit: Some(50),
            session: Some("s1".into()),
            sessions: Some("s1,s1c".into()),
            prefix: Some(true),
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
                kind: "user".into(),
                at: Some(1),
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
            ErrorCode::IncompatibleProtocol,
            ErrorCode::NotFound,
            ErrorCode::BadRequest,
            ErrorCode::NotReady,
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
            IngestPhase::MetadataReady,
            IngestPhase::Restoring,
            IngestPhase::Finalizing,
            IngestPhase::Publishing,
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

#[test]
fn typescript_protocol_version_matches() {
    let expected = format!(
        "// Shared wire version; checked against Rust in the wire tests.\nexport const PROTOCOL_VERSION = {PROTOCOL_VERSION};\n"
    );
    assert_eq!(
        include_str!("../../../clients/ts/protocol-version.mjs"),
        expected
    );
}
