//! Wire-stability goldens: the serialized JSON of every wire type, pinned.
//! The TypeScript bindings regenerate from these Rust types, but nothing
//! else fails when a serde rename silently breaks every deployed client —
//! this does. If this test fails, the wire changed: either revert the
//! accidental break, or bump `PROTOCOL_VERSION` deliberately and refresh
//! the golden with `UPDATE_WIRE_GOLDEN=1 cargo test -p rosetta-protocol`.

use rosetta_core::TaskStatus;
use rosetta_core::html_artifacts::{
    HTML_MEDIA_TYPE, HtmlArtifactAvailability, HtmlArtifactPublication,
};
use rosetta_core::timeline::{
    ArtifactKind, CanonicalText, CollaborationChannel, CollaborationEvent, CollaborationEventKind,
    CollaborationStatus, CorrelationDerivation, GroupDerivation, GroupKind, NativeToolRef, Notice,
    ParticipantColor, Provenance, ResponseGroup, ShellOutcome, StatusBasis, TaskOp, TaskSnapshot,
    TaskSnapshotItem, TimelineBody, TimelineEntry, TimelineGroup, ToolAction, ToolCorrelation,
    ToolInteraction, ToolStatus, ToolStatusFact, WorkflowRunStatus,
};
use rosetta_protocol::{
    ActivityResponse, ActivitySession, DayUsageResponse, ModelHour, PricingResponse, PricingSource,
    SessionActivity, Spend, TurnGap, TurnSpan, UnpricedModel, UsageCost,
};
use rosetta_protocol::{
    ApiError, BackfillState, BoostRequest, BoostResponse, CollaborationTimeline, CommandResponse,
    ContributionResponse, ContributionRow, DaemonStatus, DiscoveriesResponse, DiscoveryRow,
    EnableRequest, EnableResponse, ErrorCode, HeatmapDay, HeatmapResponse, HourTotal, HubStatus,
    IngestPhase, IngestProgress, ModelDay, Mutation, PROTOCOL_VERSION, ProjectBackfill, ProjectRow,
    ProjectsResponse, ResponseRuntime, RuntimeBackfill, RuntimeIngestStatus, RuntimeProvenance,
    RuntimeRow, RuntimesResponse, SearchHit, SearchReadiness, SearchRequest, SearchResponse,
    SearchState, ServerFrame, SessionResponse, SessionRole, SessionRow, SessionsResponse, Stamp,
    StatusResponse, StreamState, TimelineEntryResponse, TimelineMeta, TimelineMetaArtifact,
    TimelineMetaArtifactSource, TimelineMetaPlan, TimelineMetaPlanSource, TimelineMetaTask,
    TimelineMetaWorkflow, TimelinePage, TokenSnapshot, UsageResponse,
};
use rosetta_protocol::{
    ArtifactEntry, ArtifactExportRequest, ArtifactExportResponse, ArtifactListRequest,
    ArtifactPage, ExportEnvironment, ExportMethod, ExportShell, ExportedObject, ExportedShell,
};

const GOLDEN_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden/wire.json");

fn html_artifact() -> ArtifactEntry {
    ArtifactEntry {
        publication: HtmlArtifactPublication {
            id: "a".repeat(64),
            artifact_id: "b".repeat(64),
            session_id: "s1".into(),
            timeline_id: "artifact-message-1".into(),
            title: "Checkout design review".into(),
            filename: "checkout.html".into(),
            path: Some("/tmp/checkout.html".into()),
            url: Some("https://claude.ai/code/artifact/review".into()),
            native_id: Some("review".into()),
            version: Some("2".into()),
            published_at: Some(3_608_000),
            source_offset: 4096,
            description: Some("Two checkout flows to compare".into()),
            dropped_files: Some(2),
        },
        availability: HtmlArtifactAvailability::Captured,
        captured_at: Some(3_700_000),
        recovered_from: Some("/archive/checkout.html".into()),
        asset: Some(
            rosetta_core::ProjectionAsset::new(HTML_MEDIA_TYPE, b"<h1>Review</h1>".to_vec())
                .unwrap()
                .reference(),
        ),
        agent: "claude-code".into(),
        session_title: Some("Review checkout".into()),
        project_id: Some("p1".into()),
        project_name: Some("Rosetta".into()),
        presentations: 2,
    }
}

#[cfg(feature = "ts")]
#[test]
fn export_artifact_wire_bindings() {
    use ts_rs::TS;
    let config = ts_rs::Config::from_env();
    ArtifactPage::export_all(&config).unwrap();
    ArtifactListRequest::export_all(&config).unwrap();
}

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
        label: None,
        provenance: Default::default(),
    }
}

/// A supervisor-enrolled runtime of the same kind (2026-09-12): an absolute
/// identity root, and the two serve-time fields only such a row carries.
fn enrolled_runtime_row() -> RuntimeRow {
    RuntimeRow {
        id: "rt-kim".into(),
        kind: "claude-code".into(),
        data_root: "/users/u1/agents/roots/claude-code/9a3d".into(),
        enabled: true,
        account: None,
        config: None,
        skills: None,
        versions_seen: None,
        discovered_at: 2_000,
        label: Some("Kim".into()),
        provenance: RuntimeProvenance::Enrolled,
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
        activity: Some(rosetta_protocol::SessionActivity {
            first_at: 11_000,
            last_at: 19_000,
        }),
        stream_state: StreamState::Streaming,
    }
}

fn tool_interaction() -> ToolInteraction {
    ToolInteraction {
        attachments: vec![rosetta_core::timeline::AttachmentRef::new(
            "tool-image-1",
            rosetta_core::timeline::AttachmentKind::Image,
            rosetta_core::timeline::AttachmentSource::Asset {
                asset: {
                    let mut asset = rosetta_core::ProjectionAsset::new(
                        "image/webp",
                        b"golden tool image".to_vec(),
                    )
                    .unwrap();
                    asset.image_variants = rosetta_core::assets::ImageVariant::DERIVED
                        .into_iter()
                        .map(|variant| rosetta_core::assets::ImageVariantBytes {
                            variant,
                            bytes: format!("golden tool {}", variant.name()).into_bytes(),
                        })
                        .collect();
                    asset.reference()
                },
            },
        )],
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
            cache_write_1h: Some(4),
            web_searches: Some(1),
            service_tier: Some(rosetta_core::ServiceTier::Standard),
            route: Some("openrouter".into()),
            billed_pico_usd: Some(1_250_000),
            scope: rosetta_core::UsageScope::Request,
            requests: None,
        },
        cost: Some(UsageCost::Unpriced {
            reason: "unknown_model".into(),
        }),
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
        cost: None,
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
                attachments: None,
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
        cost: None,
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
                id: "a".repeat(64),
                name: "Checkout design review".into(),
                artifact_kind: ArtifactKind::Document,
                created_at: Some(3_608_000),
                byte_len: Some(15),
                media_type: Some(HTML_MEDIA_TYPE.into()),
                source: TimelineMetaArtifactSource::HtmlPublication {
                    publication_id: "a".repeat(64),
                    timeline_id: "artifact-message-1".into(),
                    filename: "checkout.html".into(),
                    availability: HtmlArtifactAvailability::Captured,
                    url: Some("https://claude.ai/code/artifact/review".into()),
                },
            },
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
                name: "preview.webp".into(),
                artifact_kind: ArtifactKind::Image,
                created_at: Some(3_609_000),
                byte_len: Some(42),
                media_type: Some("image/webp".into()),
                source: TimelineMetaArtifactSource::Asset {
                    asset: {
                        let mut asset = rosetta_core::ProjectionAsset::new(
                            "image/webp",
                            b"golden full image".to_vec(),
                        )
                        .unwrap();
                        asset.image_variants = rosetta_core::assets::ImageVariant::DERIVED
                            .into_iter()
                            .map(|variant| rosetta_core::assets::ImageVariantBytes {
                                variant,
                                bytes: format!("golden {}", variant.name()).into_bytes(),
                            })
                            .collect();
                        asset.reference()
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
            runtimes: vec![
                RuntimeIngestStatus {
                    kind: "claude-code".into(),
                    runtime_id: "rt1".into(),
                    label: None,
                    ticks: 100,
                    events: 5,
                    polls: 20,
                    rescans: 1,
                    admissions: 2,
                    live_rows: 12,
                    last_activity_ms: Some(50_000),
                },
                RuntimeIngestStatus {
                    kind: "claude-code".into(),
                    runtime_id: "rt-kim".into(),
                    label: Some("Kim".into()),
                    ticks: 3,
                    events: 0,
                    polls: 1,
                    rescans: 0,
                    admissions: 0,
                    live_rows: 0,
                    last_activity_ms: None,
                },
            ],
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
                run: Some(rosetta_protocol::Ingestion {
                    id: "ing_example".into(),
                    index_id: "idx_example".into(),
                    revision: 7,
                    state: rosetta_protocol::IngestionState::Paused,
                    phase: Some(rosetta_protocol::IngestionPhase::Capturing),
                    created_at: 1_000,
                    updated_at: 2_000,
                    progress_epoch: "ing_example:1".into(),
                    runtime_ids: vec!["rt1".into()],
                    allowed_actions: vec![rosetta_protocol::IngestionAction::Resume],
                    error: None,
                }),
                phase: IngestPhase::Backfilling,
                metadata_ready_at: None,
                runtimes: vec![
                    RuntimeBackfill {
                        kind: "claude-code".into(),
                        runtime_id: "rt1".into(),
                        label: None,
                        state: BackfillState::Streaming,
                        files_total: 1_209,
                        bytes_total: 2_147_483_648,
                        files_skimmed: 1_209,
                        bytes_streamed: 1_073_741_824,
                        sessions_total: 5_912,
                        records: 214_032,
                        admission_course: Some(rosetta_protocol::AdmissionCourse::PerTree),
                    },
                    RuntimeBackfill {
                        kind: "claude-code".into(),
                        runtime_id: "rt-kim".into(),
                        label: Some("Kim".into()),
                        state: BackfillState::Queued,
                        files_total: 4,
                        bytes_total: 65_536,
                        files_skimmed: 0,
                        bytes_streamed: 0,
                        sessions_total: 0,
                        records: 0,
                        admission_course: None,
                    },
                ],
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
        artifacts_revision: Some(7),
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
            tokens: Box::new(TokenSnapshot {
                input: Some(10),
                output: Some(5),
                ..TokenSnapshot::default()
            }),
            cost: Some(UsageCost::Priced {
                micro_usd: 175,
                long_context: false,
            }),
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
        Mutation::ArtifactsChanged { revision: 7 },
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
            artifacts_revision: Some(7),
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
                details: None,
                code: ErrorCode::Internal,
                message: "boom".into(),
            },
        },
    ];
    serde_json::json!({
        "protocol_version": PROTOCOL_VERSION,
        "status": status,
        "runtimes": RuntimesResponse { runtimes: vec![runtime_row(), enrolled_runtime_row()] },
        "projects": ProjectsResponse { projects: vec![project_row()] },
        "sessions": SessionsResponse { sessions: vec![session_row()] },
        "session": SessionResponse { session: session_row() },
        "artifact_request": ArtifactListRequest {
            q: Some("checkout".into()), agent: Some("claude-code".into()), project: Some("p1".into()),
            artifact_id: None, cursor: None, limit: Some(50),
        },
        "artifact_page": ArtifactPage { entries: vec![html_artifact()], next_cursor: Some("opaque".into()), canonical_ready: true },
        "artifact_export_request": ArtifactExportRequest { into: "/exports/s1/review".into(), shell: None },
        "artifact_export_request_room": ArtifactExportRequest {
            into: "/exports/s1/review-room".into(),
            shell: Some(ExportShell::Room),
        },
        "artifact_export_response": ArtifactExportResponse {
            page: ExportedObject { path: "index.html".into(), blake3: "c".repeat(64), bytes: 15 },
            members: Vec::new(),
            captured_at: Some(3_700_000),
            method: ExportMethod::Link,
            environment: ExportEnvironment::room(),
            shell: None,
        },
        "artifact_export_response_room": ArtifactExportResponse {
            page: ExportedObject { path: "index.html".into(), blake3: "d".repeat(64), bytes: 1_234 },
            members: Vec::new(),
            captured_at: None,
            method: ExportMethod::Copy,
            environment: ExportEnvironment::room(),
            shell: Some(ExportedShell {
                version: rosetta_core::html_artifacts::SHELL_VERSION.into(),
                source_blake3: "c".repeat(64),
                hosts: rosetta_core::html_artifacts::SHELL_HOSTS.all().into_iter().map(str::to_owned).collect(),
            }),
        },
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
            ErrorCode::ExportConflict,
            ErrorCode::Internal,
        ],
        "heatmap": HeatmapResponse {
            days: vec![HeatmapDay {
                day: "2026-08-26".into(),
                tokens: 123,
                spend: Spend {
                    cost_micro_usd: Some(4_210),
                    billed_micro_usd: None,
                    unpriced_tokens: 23,

                    unrecorded_output: 0,
                    reported_cost_adjustment_micro_usd: 0,
                },
            }],
        },
        "usage": UsageResponse {
            models: vec![
                ModelDay {
                    day: "2026-08-26".into(),
                    model: "claude-opus-5".into(),
                    tokens: 100,
                    spend: Spend {
                        cost_micro_usd: Some(4_210),
                        billed_micro_usd: None,
                        unpriced_tokens: 0,

                        unrecorded_output: 0,
                    reported_cost_adjustment_micro_usd: 0,
                    },
                },
                ModelDay {
                    day: "2026-08-26".into(),
                    model: "unknown".into(),
                    tokens: 23,
                    spend: Spend {
                        cost_micro_usd: None,
                        billed_micro_usd: None,
                        unpriced_tokens: 23,

                        unrecorded_output: 0,
                    reported_cost_adjustment_micro_usd: 0,
                    },
                },
            ],
            hours: vec![HourTotal {
                hour: 15,
                tokens: 123,
                spend: Spend::default(),
            }],
        },
        "day_usage": DayUsageResponse {
            day: "2026-09-30".into(),
            models: vec![ModelHour { hour: 16, model: "claude-opus-5-5".into(), tokens: 123 }],
        },
        "activity": ActivityResponse {
            since_ms: 1_791_460_800_000,
            until_ms: 1_791_547_200_000,
            gap_ms: 900_000,
            sessions: vec![ActivitySession {
                // a top-level session: the route answers roots only
                session: SessionRow {
                    parent_session_id: None,
                    root_session_id: "s1".into(),
                    role: SessionRole::Main,
                    activity: Some(SessionActivity {
                        first_at: 1_791_536_400_000,
                        last_at: 1_791_545_400_000,
                    }),
                    ..session_row()
                },
                turns: vec![
                    TurnSpan {
                        group_id: "g1".into(),
                        kind: GroupKind::HumanTurn,
                        first_at: 1_791_536_400_000,
                        last_at: 1_791_540_000_000,
                        gaps: vec![TurnGap {
                            from_ms: 1_791_537_000_000,
                            to_ms: 1_791_539_400_000,
                        }],
                    },
                    TurnSpan {
                        group_id: "g2".into(),
                        kind: GroupKind::AgentCycle,
                        first_at: 1_791_544_000_000,
                        last_at: 1_791_545_400_000,
                        gaps: vec![],
                    },
                ],
                turns_withheld: false,
            }],
            truncated: false,
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
                sessions: Some(1_204),
                bytes: 2_147_483_648,
                newest_modified_ms: 1_000,
                enabled: false,
            }, DiscoveryRow {
                kind: "hermes".into(),
                data_root: "~/.hermes".into(),
                installed: true,
                files: 3,
                sessions: None,
                bytes: 4_096,
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
                spend: Spend {
                    cost_micro_usd: Some(88),
                    billed_micro_usd: Some(12),
                    unpriced_tokens: 0,

                    unrecorded_output: 0,
                    reported_cost_adjustment_micro_usd: 0,
                },
                commits: 1,
                edits: 2,
                loc: 30,
                messages: 41,
            }],
        },
        "pricing": PricingResponse {
            basis: "list_price_equivalent".into(),
            catalog_version: "genai-prices@02cb80a15dce+overlay@39ab6e6b1409+gen1".into(),
            projection_version: Some("genai-prices@02cb80a15dce+overlay@39ab6e6b1409+gen1".into()),
            models: 400,
            sources: vec![PricingSource {
                name: "pydantic/genai-prices".into(),
                url: "https://github.com/pydantic/genai-prices".into(),
                revision: "02cb80a15dcea25ca66c76321f6cfdcc635d81fb".into(),
                sha256: "71d66c12".into(),
                license: "MIT".into(),
            }],
            unpriced: vec![UnpricedModel {
                model: "claude-opus-5-5".into(),
                reason: "unknown_model".into(),
                tokens: 3_488_371_239,
                first_hour_ms: 1_790_096_400_000,
                last_hour_ms: 1_790_190_000_000,
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

#[test]
fn the_library_fields_of_protocol_30_are_additive_and_unknown_from_an_older_daemon() {
    let mut legacy_status = serde_json::to_value(StatusResponse {
        protocol_version: 29,
        schema_version: 29,
        canonical_ready: true,
        search: search_state(),
        daemon: None,
        artifacts_revision: None,
    })
    .unwrap();
    legacy_status
        .as_object_mut()
        .unwrap()
        .remove("artifacts_revision");
    let status: StatusResponse = serde_json::from_value(legacy_status).unwrap();
    assert_eq!(
        status.artifacts_revision, None,
        "an older daemon names no revision"
    );

    let mut legacy_entry = serde_json::to_value(html_artifact()).unwrap();
    let publication = legacy_entry["publication"].as_object_mut().unwrap();
    publication.remove("description");
    publication.remove("dropped_files");
    let entry: ArtifactEntry = serde_json::from_value(legacy_entry).unwrap();
    assert_eq!(
        (
            entry.publication.description,
            entry.publication.dropped_files
        ),
        (None, None),
        "a schema 29 publication's request words are unknown, never empty"
    );
}

/// An export asks for a shell only to get one: absent is the page's own
/// bytes, and a shell rosetta does not write is refused, never ignored.
#[test]
fn an_export_request_names_its_shell_only_to_ask_for_one() {
    let raw: ArtifactExportRequest = serde_json::from_str(r#"{"into":"/exports/a"}"#).unwrap();
    assert_eq!(raw.shell, None);
    assert_eq!(
        serde_json::to_value(&raw).unwrap(),
        serde_json::json!({"into": "/exports/a"})
    );
    let room: ArtifactExportRequest =
        serde_json::from_str(r#"{"into":"/exports/a","shell":"room"}"#).unwrap();
    assert_eq!(room.shell, Some(ExportShell::Room));
    let dark = serde_json::from_str::<ArtifactExportRequest>(r#"{"into":"/a","shell":"dark"}"#);
    assert!(dark.is_err(), "rosetta writes no other shell");
}

#[test]
fn the_first_pass_stamp_is_additive_and_preserves_null_and_zero() {
    let legacy = serde_json::json!({
        "phase": "backfilling", "runtimes": [], "projects": [],
    });
    let mut ingest: IngestProgress = serde_json::from_value(legacy.clone()).unwrap();
    assert_eq!(ingest.metadata_ready_at, None);
    for stamp in [None, Some(0), Some(1_791_216_000_000)] {
        ingest.metadata_ready_at = stamp;
        let mut expected = legacy.clone();
        expected["metadata_ready_at"] = serde_json::json!(stamp);
        expected["run"] = serde_json::Value::Null;
        let wire = serde_json::to_value(&ingest).unwrap();
        assert_eq!(wire, expected);
        assert_eq!(
            serde_json::from_value::<IngestProgress>(wire).unwrap(),
            ingest
        );
    }
}

#[test]
fn discovery_session_estimates_are_additive_and_distinguish_unknown_from_zero() {
    let legacy = serde_json::json!({
        "kind": "claude-code", "data_root": "~/.claude", "installed": true,
        "files": 10, "bytes": 1_000, "newest_modified_ms": 5, "enabled": false,
    });
    let mut row: DiscoveryRow = serde_json::from_value(legacy.clone()).unwrap();
    assert_eq!(row.sessions, None, "an older daemon has no estimate");

    for sessions in [None, Some(0), Some(1_204)] {
        row.sessions = sessions;
        let mut expected = legacy.clone();
        expected["sessions"] = serde_json::json!(sessions);
        let wire = serde_json::to_value(&row).unwrap();
        assert_eq!(wire, expected, "only the additive field changes");
        assert_eq!(serde_json::from_value::<DiscoveryRow>(wire).unwrap(), row);
    }
}

#[test]
fn the_admission_course_is_additive_and_unknown_from_an_older_daemon() {
    let legacy = serde_json::json!({
        "kind": "codex", "runtime_id": "rt1", "label": null, "state": "streaming",
        "files_total": 3, "bytes_total": 30, "files_skimmed": 3, "bytes_streamed": 10,
        "sessions_total": 2, "records": 7,
    });
    let mut row: RuntimeBackfill = serde_json::from_value(legacy.clone()).unwrap();
    assert_eq!(
        row.admission_course, None,
        "an older daemon names no course"
    );

    for (course, word) in [
        (None, serde_json::Value::Null),
        (
            Some(rosetta_protocol::AdmissionCourse::PerTree),
            "per_tree".into(),
        ),
        (
            Some(rosetta_protocol::AdmissionCourse::AtRuntimeClose),
            "at_runtime_close".into(),
        ),
        (
            Some(rosetta_protocol::AdmissionCourse::AtPublication),
            "at_publication".into(),
        ),
    ] {
        row.admission_course = course;
        let mut expected = legacy.clone();
        expected["admission_course"] = word;
        let wire = serde_json::to_value(&row).unwrap();
        assert_eq!(wire, expected, "only the additive field changes");
        assert_eq!(
            serde_json::from_value::<RuntimeBackfill>(wire).unwrap(),
            row
        );
    }
}
