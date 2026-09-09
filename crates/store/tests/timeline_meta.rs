use rosetta_core::rows::{
    PlanRow, ProjectRow, RuntimeRow, SessionRole, SessionRow, StreamState, TaskRow, TaskStatus,
    TokenSnapshot,
};
use rosetta_core::timeline::{
    ArtifactKind, ArtifactOp, CanonicalText, CorrelationDerivation, GroupDerivation, GroupKind,
    NativeToolRef, PlanOp, Provenance, ResourceRef, StatusBasis, TaskOp, TaskSnapshot,
    TaskSnapshotItem, TimelineBody, TimelineEntry, TimelineGroup, ToolAction, ToolCorrelation,
    ToolInteraction, ToolStatus, ToolStatusFact, WorkflowJournalKind, WorkflowJournalObservation,
    WorkflowRun, WorkflowRunStatus, WorkflowUsage, WorkflowWorker, WorkflowWorkerState,
};
use rosetta_core::{IngestOp, ids};
use rosetta_store::timeline_meta::{
    TimelineMetaArtifactSource, TimelineMetaPlanSource, read, revision_plan_document,
    timeline_plan_document,
};
use rosetta_store::{Batch, Store};

fn base_batch() -> Batch {
    Batch {
        ops: vec![
            IngestOp::Runtime(RuntimeRow {
                id: "rt1".into(),
                kind: "test".into(),
                data_root: "/tmp/test".into(),
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
                root_paths: vec!["/w/project".into()],
                git_repo: None,
            }),
            IngestOp::SessionMeta(SessionRow {
                id: "s1".into(),
                native_session_id: "native-s1".into(),
                runtime_id: "rt1".into(),
                project_id: Some("p1".into()),
                cwd: Some("/w/project".into()),
                parent_session_id: None,
                delegation: None,
                root_session_id: "s1".into(),
                role: SessionRole::Main,
                title: None,
                summary: None,
                started_at: 1,
                updated_at: 1,
                stream_state: StreamState::Streaming,
            }),
        ],
        ..Batch::default()
    }
}

fn workflow_child(id: &str, native_id: &str) -> SessionRow {
    SessionRow {
        id: id.into(),
        native_session_id: native_id.into(),
        runtime_id: "rt1".into(),
        project_id: Some("p1".into()),
        cwd: Some("/w/project".into()),
        parent_session_id: Some("s1".into()),
        delegation: None,
        root_session_id: "s1".into(),
        role: SessionRole::Subagent,
        title: Some("general-purpose".into()),
        summary: None,
        started_at: 1_250,
        updated_at: 1_500,
        stream_state: StreamState::Indexed,
    }
}

fn task_row(status: TaskStatus, at: i64) -> TaskRow {
    TaskRow {
        id: ids::task_id("s1", "native-task"),
        session_id: "s1".into(),
        status,
        content: "Stored task".into(),
        created_at: (status == TaskStatus::Pending).then_some(100),
        updated_at: Some(at),
        started_at: None,
        terminal_at: None,
    }
}

fn tool_entry(id: &str, ordinal: i64, at: i64, action: ToolAction) -> TimelineEntry {
    TimelineEntry {
        timeline_id: id.into(),
        session_id: "s1".into(),
        ordinal,
        group: TimelineGroup {
            id: format!("group-{id}"),
            kind: GroupKind::SystemCycle,
            derivation: GroupDerivation::Native,
        },
        response: None,
        block_index: None,
        native_id: Some(id.into()),
        parent_native_id: None,
        at: Some(at),
        model: None,
        effort: None,
        body: TimelineBody::ToolUse {
            tool: Box::new(ToolInteraction {
                correlation: ToolCorrelation {
                    key: format!("call-{id}"),
                    derivation: CorrelationDerivation::NativeId,
                },
                native: NativeToolRef {
                    id: Some(format!("call-{id}")),
                    namespace: None,
                    name: "test".into(),
                    provider: None,
                },
                status: ToolStatusFact {
                    value: ToolStatus::Succeeded,
                    basis: StatusBasis::NativeTerminalRecord,
                },
                action,
                failure: None,
                timing: None,
            }),
        },
        parent_tool_native_id: None,
        usage: TokenSnapshot::default(),
        provenance: Provenance {
            agent_version: None,
            src_cursor: None,
            src_offset: None,
        },
    }
}

fn plan_action(title: &str) -> ToolAction {
    ToolAction::Plan {
        operation: PlanOp::Update,
        plan_id: Some("primary".into()),
        title: Some(title.into()),
        summary: Some(CanonicalText::inline("Query, protocol, client, view.").unwrap()),
        step_count: Some(4),
    }
}

fn artifact_action(name: &str) -> ToolAction {
    ToolAction::Artifact {
        operation: ArtifactOp::Create,
        artifact_kind: ArtifactKind::Document,
        name: Some(name.into()),
        description: None,
        resources: vec![ResourceRef::File {
            path: "/w/project/report.md".into(),
            resolved: true,
        }],
        media: None,
    }
}

fn workflow_action(run_id: &str, attempt: u64) -> ToolAction {
    ToolAction::Workflow {
        name: Some("parallel-review".into()),
        path: None,
        arguments: None,
        objective: None,
        resume_id: (attempt > 1).then(|| run_id.to_owned()),
        background: true,
        task_id: Some(format!("task-{attempt}")),
        run_id: Some(run_id.into()),
        attempt: Some(attempt),
        callback_summary: None,
        callback_result: None,
        usage: None,
        run: None,
    }
}

fn workflow_run(run_id: &str) -> WorkflowRun {
    WorkflowRun {
        run_id: run_id.into(),
        task_id: Some("task-2".into()),
        name: Some("parallel-review".into()),
        summary: None,
        status: WorkflowRunStatus::Completed,
        started_at: Some(1_200),
        updated_at: Some(6_200),
        usage: WorkflowUsage {
            duration_ms: Some(5_000),
            tokens: Some(42_000),
            tool_calls: Some(12),
            agent_count: Some(3),
        },
        default_model: None,
        arguments: None,
        script_path: None,
        script: None,
        phases: vec![],
        workers: vec![WorkflowWorker {
            index: 1,
            key: Some("worker-1".into()),
            label: "reader".into(),
            phase_index: None,
            phase_title: None,
            state: WorkflowWorkerState::Done,
            cached: false,
            configured_model: None,
            configured_effort: None,
            observed_models: vec![],
            observed_efforts: vec![],
            child_session_id: Some("child-workflow".into()),
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
    }
}

#[test]
fn one_read_builds_complete_deduplicated_work_indexes_without_bodies() {
    let mut store = Store::open_in_memory().unwrap();
    store.apply(&base_batch()).unwrap();

    for (status, at) in [
        (TaskStatus::Pending, 100),
        (TaskStatus::InProgress, 200),
        (TaskStatus::Completed, 500),
    ] {
        store
            .apply(&Batch {
                ops: vec![IngestOp::Task(task_row(status, at))],
                ..Batch::default()
            })
            .unwrap();
    }
    store
        .apply(&Batch {
            ops: vec![
                IngestOp::Plan(PlanRow {
                    session_id: "s1".into(),
                    revision: 1,
                    content: "old".into(),
                    updated_at: 300,
                }),
                IngestOp::Plan(PlanRow {
                    session_id: "s1".into(),
                    revision: 2,
                    content: "[{\"step\":\"ship\"}]".into(),
                    updated_at: 600,
                }),
                IngestOp::Message(Box::new(tool_entry(
                    "task-message",
                    0,
                    700,
                    ToolAction::Task {
                        operation: TaskOp::Update,
                        task_id: Some("claude-task".into()),
                        subject: Some("Snapshot task".into()),
                        description: None,
                        status: Some(TaskStatus::InProgress),
                        owner: None,
                        blocker_ids: vec![],
                        snapshot: Some(TaskSnapshot {
                            tasks: vec![TaskSnapshotItem {
                                task_id: "claude-task".into(),
                                subject: Some("Snapshot task".into()),
                                status: Some(TaskStatus::InProgress),
                                owner: None,
                                blocker_ids: vec![],
                            }],
                            total_count: 1,
                        }),
                    },
                ))),
                IngestOp::Message(Box::new(tool_entry(
                    "plan-old",
                    1,
                    800,
                    plan_action("Old plan"),
                ))),
                IngestOp::Message(Box::new(tool_entry(
                    "plan-new",
                    2,
                    900,
                    plan_action("Current plan"),
                ))),
                IngestOp::Message(Box::new(tool_entry(
                    "artifact-old",
                    3,
                    1_000,
                    artifact_action("old-report.md"),
                ))),
                IngestOp::Message(Box::new(tool_entry(
                    "artifact-new",
                    4,
                    1_100,
                    artifact_action("report.md"),
                ))),
            ],
            ..Batch::default()
        })
        .unwrap();

    let meta = read(store.conn(), "s1").unwrap();
    assert_eq!(meta.tasks.len(), 2);
    let stored = meta
        .tasks
        .iter()
        .find(|task| task.subject == "Stored task")
        .unwrap();
    assert_eq!(stored.started_at, Some(200));
    assert_eq!(stored.terminal_at, Some(500));
    assert_eq!(stored.timeline_id, None);
    let snapshot = meta
        .tasks
        .iter()
        .find(|task| task.subject == "Snapshot task")
        .unwrap();
    assert_eq!(snapshot.started_at, Some(700));
    assert_eq!(snapshot.timeline_id.as_deref(), Some("task-message"));

    assert_eq!(meta.plans.len(), 2, "latest stored and latest named plan");
    assert!(meta.plans.iter().any(|plan| {
        plan.name == "plan-r2.json"
            && matches!(
                plan.source,
                TimelineMetaPlanSource::Revision { revision: 2 }
            )
    }));
    assert!(meta.plans.iter().any(|plan| {
        plan.name == "Current plan.md"
            && matches!(
                &plan.source,
                TimelineMetaPlanSource::Timeline { timeline_id } if timeline_id == "plan-new"
            )
    }));

    assert_eq!(meta.artifacts.len(), 1, "a repeated resource is one file");
    assert_eq!(meta.artifacts[0].name, "report.md");
    assert_eq!(
        meta.artifacts[0].created_at,
        Some(1_000),
        "later artifact activity must not rewrite its creation time"
    );
    assert!(matches!(
        &meta.artifacts[0].source,
        TimelineMetaArtifactSource::TimelineFile { timeline_id, target_index: 0, .. }
            if timeline_id == "artifact-new"
    ));

    let stored_plan = revision_plan_document(store.conn(), "s1", 2)
        .unwrap()
        .unwrap();
    assert_eq!(stored_plan.content, "[{\"step\":\"ship\"}]");
    let timeline_plan = timeline_plan_document(store.conn(), "s1", "plan-new")
        .unwrap()
        .unwrap();
    assert!(timeline_plan.content.contains("# Current plan"));
    assert!(timeline_plan.content.contains("Steps recorded: 4"));
}

#[test]
fn task_rename_preserves_the_source_observed_run_timing() {
    let mut store = Store::open_in_memory().unwrap();
    store.apply(&base_batch()).unwrap();
    store
        .apply(&Batch {
            ops: vec![
                IngestOp::Message(Box::new(tool_entry(
                    "task-started",
                    0,
                    200,
                    ToolAction::Task {
                        operation: TaskOp::Update,
                        task_id: Some("rename-me".into()),
                        subject: Some("First name".into()),
                        description: None,
                        status: Some(TaskStatus::InProgress),
                        owner: None,
                        blocker_ids: vec![],
                        snapshot: None,
                    },
                ))),
                IngestOp::Message(Box::new(tool_entry(
                    "task-completed",
                    1,
                    500,
                    ToolAction::Task {
                        operation: TaskOp::Update,
                        task_id: Some("rename-me".into()),
                        subject: Some("Clearer final name".into()),
                        description: None,
                        status: Some(TaskStatus::Completed),
                        owner: None,
                        blocker_ids: vec![],
                        snapshot: None,
                    },
                ))),
            ],
            ..Batch::default()
        })
        .unwrap();

    let meta = read(store.conn(), "s1").unwrap();
    assert_eq!(meta.tasks.len(), 1);
    assert_eq!(meta.tasks[0].subject, "Clearer final name");
    assert_eq!(meta.tasks[0].started_at, Some(200));
    assert_eq!(meta.tasks[0].terminal_at, Some(500));
    assert_eq!(meta.tasks[0].timeline_id.as_deref(), Some("task-completed"));
}

#[test]
fn task_snapshots_contribute_timing_and_do_not_hide_explicit_tasks() {
    let mut store = Store::open_in_memory().unwrap();
    store.apply(&base_batch()).unwrap();

    let snapshot = |id: &str, ordinal: i64, at: i64, status: TaskStatus| {
        IngestOp::Message(Box::new(tool_entry(
            id,
            ordinal,
            at,
            ToolAction::Task {
                operation: TaskOp::List,
                task_id: None,
                subject: None,
                description: None,
                status: None,
                owner: None,
                blocker_ids: vec![],
                snapshot: Some(TaskSnapshot {
                    tasks: vec![TaskSnapshotItem {
                        task_id: "board-task".into(),
                        subject: Some("Board task".into()),
                        status: Some(status),
                        owner: None,
                        blocker_ids: vec![],
                    }],
                    total_count: 1,
                }),
            },
        )))
    };

    store
        .apply(&Batch {
            ops: vec![
                IngestOp::Message(Box::new(tool_entry(
                    "explicit-task",
                    0,
                    100,
                    ToolAction::Task {
                        operation: TaskOp::Create,
                        task_id: Some("outside-board".into()),
                        subject: Some("Explicit task".into()),
                        description: None,
                        status: Some(TaskStatus::Pending),
                        owner: None,
                        blocker_ids: vec![],
                        snapshot: None,
                    },
                ))),
                snapshot("board-start", 1, 200, TaskStatus::InProgress),
                snapshot("board-finish", 2, 500, TaskStatus::Completed),
            ],
            ..Batch::default()
        })
        .unwrap();

    let meta = read(store.conn(), "s1").unwrap();
    assert_eq!(meta.tasks.len(), 2);
    let board = meta
        .tasks
        .iter()
        .find(|task| task.subject == "Board task")
        .unwrap();
    assert_eq!(board.started_at, Some(200));
    assert_eq!(board.terminal_at, Some(500));
    assert_eq!(board.timeline_id.as_deref(), Some("board-finish"));
    assert!(
        meta.tasks
            .iter()
            .any(|task| task.subject == "Explicit task")
    );
}

#[test]
fn workflows_are_one_light_row_per_run_and_land_on_the_latest_attempt() {
    let mut store = Store::open_in_memory().unwrap();
    store.apply(&base_batch()).unwrap();
    store
        .apply(&Batch {
            ops: vec![
                IngestOp::SessionMeta(workflow_child("child-workflow", "native-current")),
                IngestOp::SessionMeta(workflow_child("child-earlier", "native-earlier")),
                IngestOp::WorkflowRun {
                    session_id: "s1".into(),
                    run: Box::new(workflow_run("run-1")),
                },
                IngestOp::WorkflowJournal {
                    session_id: "s1".into(),
                    observation: WorkflowJournalObservation {
                        observation_id: "journal-earlier".into(),
                        run_id: "run-1".into(),
                        source_offset: Some(12),
                        kind: WorkflowJournalKind::Started,
                        worker_key: "earlier-worker".into(),
                        agent_id: Some("native-earlier".into()),
                        result_kind: None,
                        result_preview: None,
                    },
                },
                IngestOp::Message(Box::new(tool_entry(
                    "workflow-launch",
                    0,
                    1_200,
                    workflow_action("run-1", 1),
                ))),
                IngestOp::Message(Box::new(tool_entry(
                    "workflow-resume",
                    1,
                    2_000,
                    workflow_action("run-1", 2),
                ))),
            ],
            ..Batch::default()
        })
        .unwrap();

    let meta = read(store.conn(), "s1").unwrap();
    assert_eq!(meta.workflows.len(), 1, "a resume is not a second run");
    let workflow = &meta.workflows[0];
    assert_eq!(workflow.id, "run-1");
    assert_eq!(workflow.name, "parallel-review");
    assert_eq!(workflow.status, WorkflowRunStatus::Completed);
    assert_eq!(workflow.started_at, Some(1_200));
    assert_eq!(workflow.duration_ms, Some(5_000));
    assert_eq!(workflow.worker_count, Some(1));
    assert_eq!(
        workflow.child_session_ids,
        ["child-earlier", "child-workflow"],
        "superseded journal workers remain owned by the workflow"
    );
    assert_eq!(workflow.timeline_id.as_deref(), Some("workflow-resume"));
}

#[test]
fn a_source_proven_workflow_run_survives_a_missing_sidecar() {
    let mut store = Store::open_in_memory().unwrap();
    store.apply(&base_batch()).unwrap();
    store
        .apply(&Batch {
            ops: vec![IngestOp::Message(Box::new(tool_entry(
                "workflow-launch",
                0,
                1_200,
                workflow_action("run-without-sidecar", 1),
            )))],
            ..Batch::default()
        })
        .unwrap();

    let meta = read(store.conn(), "s1").unwrap();
    assert_eq!(meta.workflows.len(), 1);
    let workflow = &meta.workflows[0];
    assert_eq!(workflow.id, "run-without-sidecar");
    assert_eq!(workflow.name, "parallel-review");
    assert_eq!(workflow.status, WorkflowRunStatus::Unknown);
    assert_eq!(workflow.started_at, Some(1_200));
    assert_eq!(workflow.duration_ms, None);
    assert_eq!(workflow.worker_count, None);
    assert!(workflow.child_session_ids.is_empty());
    assert_eq!(workflow.timeline_id.as_deref(), Some("workflow-launch"));
}

#[test]
fn relevant_batch_coalesces_to_one_timeline_meta_session_touch() {
    let mut store = Store::open_in_memory().unwrap();
    store.apply(&base_batch()).unwrap();
    store.observe_effects();
    store
        .apply(&Batch {
            ops: vec![
                IngestOp::Message(Box::new(tool_entry("plan", 0, 100, plan_action("Plan")))),
                IngestOp::Message(Box::new(tool_entry(
                    "artifact",
                    1,
                    200,
                    artifact_action("report.md"),
                ))),
                IngestOp::WorkflowRun {
                    session_id: "s1".into(),
                    run: Box::new(workflow_run("run-1")),
                },
            ],
            ..Batch::default()
        })
        .unwrap();
    let effects = store.drain_effects();
    assert_eq!(
        effects
            .timeline_meta_sessions
            .into_iter()
            .collect::<Vec<_>>(),
        vec!["s1"]
    );
}
