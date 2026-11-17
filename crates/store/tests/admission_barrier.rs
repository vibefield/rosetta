//! The cold admission barrier on the read side: while a fresh build is
//! unpublished, a Pending session's rows are not public, so no family walk —
//! activity rollups, turn spans, Workflow child profiles, the metadata
//! mirror — may count them; with the barrier down every read is exactly the
//! published read it was before.

use std::collections::BTreeMap;

use rosetta_core::rows::{
    MessageKind, MessageRow, ProjectRow, RuntimeRow, SessionActivity, SessionRole, SessionRow,
    StreamState,
};
use rosetta_core::timeline::{
    CollaborationChannel, CollaborationEvent, CollaborationEventKind, GroupDerivation, GroupKind,
    ResponseGroup, TimelineBody, TimelineEntry, TimelineGroup, WorkflowRun, WorkflowRunStatus,
    WorkflowUsage, WorkflowWorker, WorkflowWorkerState,
};
use rosetta_core::{CorrelationDerivation, IngestOp, ToolAction, ToolInteraction};
use rosetta_store::hot::{MetaMirror, Mutation};
use rosetta_store::turns::{turn_spans, turn_spans_visible};
use rosetta_store::{Batch, Store, queries};

const ALL: [&str; 6] = [
    "root",
    "child",
    "grand",
    "held-root",
    "held-root-child",
    "missing",
];

fn session(id: &str, parent: Option<&str>, root: &str, state: StreamState) -> SessionRow {
    SessionRow {
        id: id.into(),
        native_session_id: format!("native-{id}"),
        runtime_id: "rt1".into(),
        project_id: Some("p1".into()),
        cwd: Some("/w/app".into()),
        parent_session_id: parent.map(Into::into),
        delegation: None,
        // Codex records the immediate parent as the root.
        root_session_id: root.into(),
        role: if parent.is_some() {
            SessionRole::Subagent
        } else {
            SessionRole::Main
        },
        title: None,
        summary: None,
        started_at: 1_000,
        updated_at: 1_000,
        activity: None,
        stream_state: state,
    }
}

fn row(session: &str, ordinal: i64, at: i64) -> MessageRow {
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
        content: Some(format!("{session} row {ordinal}")),
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

fn workflow_row(session: &str, ordinal: i64, at: i64) -> MessageRow {
    let mut row = row(session, ordinal, at);
    row.kind = MessageKind::ToolUse;
    row.content = None;
    let mut tool =
        ToolInteraction::unknown("call-workflow", CorrelationDerivation::NativeId, "Workflow");
    tool.action = ToolAction::Workflow {
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
    };
    row.tool = Some(tool);
    row.response = Some(ResponseGroup {
        id: "resp-1".into(),
        derivation: GroupDerivation::Native,
    });
    row.block_index = Some(0);
    row
}

fn workflow_run(child: &str) -> WorkflowRun {
    WorkflowRun {
        run_id: "wf".into(),
        task_id: None,
        name: Some("review".into()),
        summary: None,
        status: WorkflowRunStatus::Completed,
        started_at: Some(1_000),
        updated_at: Some(2_000),
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
    }
}

/// A depth-2 Codex family split across readiness — `root` and its
/// grandchild admitted, the child between them Pending — plus a Pending root
/// whose own child is admitted. The root's Workflow worker ran in the child.
fn split_family() -> Store {
    let mut store = Store::open_in_memory().unwrap();
    let mut ops = vec![
        IngestOp::Runtime(RuntimeRow {
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
        }),
        IngestOp::Project(ProjectRow {
            id: "p1".into(),
            name: "app".into(),
            root_paths: vec!["/w/app".into()],
            git_repo: None,
        }),
        IngestOp::SessionMeta(session("root", None, "root", StreamState::Indexed)),
        IngestOp::SessionMeta(session("child", Some("root"), "root", StreamState::Pending)),
        IngestOp::SessionMeta(session(
            "grand",
            Some("child"),
            "child",
            StreamState::Indexed,
        )),
        IngestOp::SessionMeta(session(
            "held-root",
            None,
            "held-root",
            StreamState::Pending,
        )),
        IngestOp::SessionMeta(session(
            "held-root-child",
            Some("held-root"),
            "held-root",
            StreamState::Indexed,
        )),
        IngestOp::WorkflowRun {
            session_id: "root".into(),
            run: Box::new(workflow_run("child")),
        },
    ];
    let mut child = row("child", 0, 5_000);
    child.model = Some("child-model".into());
    child.effort = Some("high".into());
    let rows = vec![
        row("root", 0, 10_000),
        workflow_row("root", 1, 11_000),
        child,
        row("grand", 0, 90_000),
        row("held-root", 0, 20_000),
        row("held-root-child", 0, 30_000),
    ];
    ops.extend(rows.into_iter().flat_map(IngestOp::project_message));
    store
        .apply(&Batch {
            ops,
            ..Batch::default()
        })
        .unwrap();
    store
}

fn bounds(first_at: i64, last_at: i64) -> Option<SessionActivity> {
    Some(SessionActivity { first_at, last_at })
}

fn observed_models(entry: &TimelineEntry) -> Vec<String> {
    let TimelineBody::ToolUse { tool } = &entry.body else {
        panic!("expected the workflow row");
    };
    let ToolAction::Workflow { run: Some(run), .. } = &tool.action else {
        panic!("the workflow run hydrates");
    };
    run.workers[0].observed_models.clone()
}

fn mirror_sessions(mirror: &MetaMirror) -> BTreeMap<String, SessionRow> {
    mirror
        .sessions()
        .map(|row| (row.id.clone(), row.clone()))
        .collect()
}

#[test]
fn a_split_family_never_leaks_into_the_admitted_root() {
    let store = split_family();
    let conn = store.conn();

    // Activity: the root counts itself only; the child between it and the
    // admitted grandchild is held, so the grandchild is not reached either.
    let root = queries::session_visible(conn, "root", true)
        .unwrap()
        .unwrap();
    assert_eq!(root.activity, bounds(10_000, 11_000));
    assert_eq!(
        queries::session(conn, "root").unwrap().unwrap().activity,
        bounds(5_000, 90_000)
    );
    // A Pending ancestor's activity stays None, admitted children or not.
    for held in ["child", "held-root"] {
        let row = queries::session_visible(conn, held, true).unwrap().unwrap();
        assert_eq!(row.activity, None, "{held}");
    }
    assert_eq!(
        queries::session_visible(conn, "held-root-child", true)
            .unwrap()
            .unwrap()
            .activity,
        bounds(30_000, 30_000)
    );

    // Turns: only the root's own turn is in its tree while the child is held.
    let hidden = turn_spans_visible(conn, "root", 0, 100_000, 60_000, true).unwrap();
    assert_eq!(
        hidden
            .iter()
            .map(|t| t.group_id.as_str())
            .collect::<Vec<_>>(),
        ["root-turn"]
    );
    let shown = turn_spans_visible(conn, "root", 0, 100_000, 60_000, false).unwrap();
    assert_eq!(
        shown,
        turn_spans(conn, "root", 0, 100_000, 60_000).unwrap(),
        "the barrier down is the published read"
    );
    assert_eq!(shown.len(), 3);
    assert!(
        turn_spans_visible(conn, "held-root", 0, 100_000, 60_000, true)
            .unwrap()
            .is_empty()
    );

    // Workflow profiles: the held child's model is not observed yet.
    let page = queries::timeline_page_visible(conn, "root", None, 10, true).unwrap();
    assert!(observed_models(&page[1]).is_empty());
    let entry = queries::timeline_entry_visible(conn, "root", "root-1", true)
        .unwrap()
        .unwrap();
    assert!(observed_models(&entry).is_empty());
    let published = queries::timeline_page(conn, "root", None, 10).unwrap();
    assert_eq!(observed_models(&published[1]), ["child-model"]);
    assert_eq!(
        queries::timeline_page_visible(conn, "root", None, 10, false).unwrap(),
        published
    );
    assert_eq!(
        queries::timeline_entry_visible(conn, "root", "root-1", false).unwrap(),
        queries::timeline_entry(conn, "root", "root-1").unwrap()
    );
}

#[test]
fn bulk_and_single_reads_agree_and_the_cheap_admission_read_matches_the_row() {
    let store = split_family();
    let conn = store.conn();
    let ids = ALL.iter().map(|id| id.to_string()).collect();
    for hide_pending in [true, false] {
        let named = queries::named_sessions_visible(conn, &ids, hide_pending).unwrap();
        let all: BTreeMap<_, _> = queries::all_sessions_visible(conn, hide_pending)
            .unwrap()
            .into_iter()
            .map(|row| (row.id.clone(), row))
            .collect();
        for (id, row) in named {
            let single = queries::session_visible(conn, id, hide_pending).unwrap();
            assert_eq!(row, single, "{id} hide={hide_pending}");
            assert_eq!(all.get(id).cloned(), single, "{id} hide={hide_pending}");
        }
    }
    assert_eq!(
        queries::all_sessions_visible(conn, false).unwrap(),
        queries::all_sessions(conn).unwrap()
    );
    assert_eq!(
        queries::named_sessions_visible(conn, &ids, false).unwrap(),
        queries::named_sessions(conn, &ids).unwrap()
    );
    for id in ALL {
        assert_eq!(
            queries::session_stream_state(conn, id).unwrap(),
            queries::session(conn, id)
                .unwrap()
                .map(|row| row.stream_state),
            "{id}"
        );
    }
    assert_eq!(
        queries::pending_session_ids(conn).unwrap(),
        ["child", "held-root"].map(String::from).into()
    );
}

#[test]
fn report_work_keeps_a_held_childs_profile_out_while_unpublished() {
    let mut store = split_family();
    // `root`'s workflow response, reported into the admitted grandchild.
    let mut report = queries::timeline_entry(store.conn(), "grand", "grand-0")
        .unwrap()
        .unwrap();
    report.timeline_id = "grand-report".into();
    report.ordinal = 1;
    report.body = TimelineBody::Collaboration {
        event: Box::new(CollaborationEvent {
            attachments: None,
            kind: CollaborationEventKind::Report,
            channel: CollaborationChannel::Team,
            sender: None,
            recipient: None,
            team: None,
            color: None,
            summary: Some("done".into()),
            body: None,
            task_id: Some("task-1".into()),
            delivery_id: None,
            status: None,
            authored_at: None,
            source_session_id: Some("root".into()),
            source_response_id: Some("resp-1".into()),
            work_start_timeline_id: Some("root-1".into()),
            work_end_timeline_id: Some("root-1".into()),
        }),
    };
    store
        .apply(&Batch {
            ops: vec![IngestOp::Message(Box::new(report))],
            ..Batch::default()
        })
        .unwrap();
    let conn = store.conn();
    let hidden = queries::report_work(conn, "grand", "grand-report", true)
        .unwrap()
        .expect("an admitted report over admitted work answers");
    assert!(observed_models(&hidden[0]).is_empty());
    let shown = queries::report_work(conn, "grand", "grand-report", false)
        .unwrap()
        .unwrap();
    assert_eq!(observed_models(&shown[0]), ["child-model"]);
    // A held report session answers nothing in either mode.
    for hide_pending in [true, false] {
        assert_eq!(
            queries::report_work(conn, "child", "child-0", hide_pending).unwrap(),
            None
        );
    }
}

fn session_ids(events: &[Mutation]) -> Vec<String> {
    let mut ids: Vec<_> = events
        .iter()
        .map(|event| match event {
            Mutation::Session(row) => row.id.clone(),
            other => panic!("only session rows change: {other:?}"),
        })
        .collect();
    ids.sort();
    ids
}

/// The fresh build's own path: it observes metadata effects only, so every
/// row landing in a held member reaches the mirror as that member's id, and
/// the mirror re-reads it and its ancestors — per row, and in one pass once a
/// refresh names many sessions.
#[test]
fn rows_landing_behind_the_barrier_move_no_mirror_row_until_their_tree_flips() {
    let mut store = split_family();
    // The skim stamps `updated_at` from the file's mtime, which its records
    // do not pass, so a landing row moves only the payload-derived activity.
    store
        .conn()
        .execute(
            "UPDATE session SET updated_at = 100000 WHERE id = 'child'",
            [],
        )
        .unwrap();
    let mut mirror = MetaMirror::default();
    mirror.set_hide_pending(true);
    mirror.warm(store.conn()).unwrap();
    let before = mirror_sessions(&mirror);

    store.observe_meta_effects();
    store
        .apply(&Batch {
            ops: IngestOp::project_message(row("child", 1, 99_000)),
            ..Batch::default()
        })
        .unwrap();
    let effects = store.drain_meta_effects();
    assert!(effects.sessions.contains("child"));
    // Past the bulk threshold the same refresh reads every row in one pass.
    let mut bulk = effects.clone();
    bulk.sessions
        .extend((0..80).map(|index| format!("absent-{index}")));
    for effects in [&effects, &bulk] {
        assert!(
            mirror
                .refresh_meta(store.conn(), effects)
                .unwrap()
                .is_empty(),
            "a held member's rows change nothing public"
        );
        assert_eq!(mirror_sessions(&mirror), before);
    }

    // The readiness commit admits the held child: it and the root above it
    // are re-read, now counting the child and the grandchild below it.
    store
        .set_session_stream_states(&[("child".into(), StreamState::Indexed)])
        .unwrap();
    let effects = store.drain_meta_effects();
    let events = mirror.refresh_meta(store.conn(), &effects).unwrap();
    assert_eq!(session_ids(&events), ["child", "root"]);
    let after = mirror_sessions(&mirror);
    assert_eq!(after["root"].activity, bounds(5_000, 99_000));
    assert_eq!(after["child"].activity, bounds(5_000, 99_000));
    assert_eq!(after["held-root"].activity, None);
    let mut warmed = MetaMirror::default();
    warmed.set_hide_pending(true);
    warmed.warm(store.conn()).unwrap();
    assert_eq!(after, mirror_sessions(&warmed));
}

#[test]
fn a_mirror_that_held_the_barrier_reveals_exactly_to_a_published_warm() {
    let store = split_family();
    let conn = store.conn();
    let mut held = MetaMirror::default();
    held.set_hide_pending(true);
    held.warm(conn).unwrap();
    let before = mirror_sessions(&held);
    assert_eq!(before["root"].activity, bounds(10_000, 11_000));
    assert_eq!(before["child"].activity, None);
    assert_eq!(before["held-root"].activity, None);

    let events = held.reveal_pending(conn).unwrap();
    assert!(!held.hides_pending());
    let mut published = MetaMirror::default();
    published.warm(conn).unwrap();
    assert_eq!(mirror_sessions(&held), mirror_sessions(&published));

    // Precisely the rows the barrier shaped changed: the held sessions and
    // the root above the held child. Nothing else is re-announced.
    assert_eq!(session_ids(&events), ["child", "held-root", "root"]);
    assert!(held.reveal_pending(conn).unwrap().is_empty(), "lifted once");
}
