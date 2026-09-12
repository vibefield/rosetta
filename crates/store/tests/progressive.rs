use rosetta_core::IngestOp;
use rosetta_core::rows::{
    MessageKind, MessageRow, RuntimeRow, SessionRole, SessionRow, StreamState,
};
use rosetta_store::queries;
use rosetta_store::{Batch, CheckpointUpdate, SourceMode, SourceWrite, Store};

fn runtime() -> RuntimeRow {
    RuntimeRow {
        id: "rt".into(),
        kind: "codex".into(),
        data_root: "/agent".into(),
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

fn session(state: StreamState) -> SessionRow {
    SessionRow {
        id: "s".into(),
        native_session_id: "native".into(),
        runtime_id: "rt".into(),
        project_id: None,
        cwd: None,
        parent_session_id: None,
        delegation: None,
        root_session_id: "s".into(),
        role: SessionRole::Main,
        title: Some("session".into()),
        summary: None,
        started_at: 1,
        updated_at: 2,
        activity: None,
        stream_state: state,
    }
}

fn message(text: &str) -> MessageRow {
    MessageRow {
        timeline_id: "m".into(),
        session_id: "s".into(),
        ordinal: 0,
        kind: MessageKind::Agent,
        subtype: None,
        uuid: None,
        parent_uuid: None,
        ts: Some(3_600_000),
        model: Some("model".into()),
        effort: None,
        content: Some(text.into()),
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

fn message_op(row: MessageRow) -> IngestOp {
    let mut ops = IngestOp::project_message(row);
    assert_eq!(ops.len(), 1);
    ops.pop().unwrap()
}

#[test]
fn interrupted_progressive_projection_is_public_resumable_and_not_ready() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("projection.sqlite");
    let mut store = Store::open_progressive(&path).unwrap();
    assert!(store.is_progressive());
    assert_eq!(
        store
            .conn()
            .query_row("PRAGMA wal_autocheckpoint", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        1_000
    );
    assert_eq!(
        store
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema WHERE name = 'message_bucket_key'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
    store
        .apply(&Batch {
            ops: vec![
                IngestOp::Runtime(runtime()),
                IngestOp::SessionMeta(session(StreamState::Indexed)),
                message_op(message("visible before finalization")),
            ],
            checkpoints: vec![],
            source: None,
        })
        .unwrap();
    assert_eq!(
        queries::timeline_page(store.conn(), "s", None, 10)
            .unwrap()
            .len(),
        1
    );
    assert!(
        !queries::projection_state(store.conn())
            .unwrap()
            .canonical_ready
    );
    drop(store);

    assert!(Store::open(&path).is_err());
    let mut resumed = Store::open_progressive(&path).unwrap();
    assert!(resumed.is_progressive());
    assert_eq!(
        queries::timeline_page(resumed.conn(), "s", None, 10)
            .unwrap()
            .len(),
        1
    );

    resumed.finish_progressive_load().unwrap();
    let state = queries::projection_state(resumed.conn()).unwrap();
    assert!(state.canonical_ready);
    assert_eq!(state.search_revision, 0);
    assert_eq!(
        resumed
            .conn()
            .query_row("PRAGMA wal_autocheckpoint", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        1_000
    );
    let (busy, log_frames, checkpointed): (i64, i64, i64) = resumed
        .conn()
        .query_row("PRAGMA wal_checkpoint(PASSIVE)", [], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .unwrap();
    assert_eq!(busy, 0);
    assert_eq!(log_frames, checkpointed);
    assert!(
        log_frames <= 2,
        "only the final readiness commit may remain after the cold WAL is folded: {log_frames} frames"
    );
    assert_eq!(
        resumed
            .conn()
            .query_row("SELECT COUNT(*) FROM usage_bucket", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        resumed
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema WHERE name = 'message_bucket_key'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        1
    );
    drop(resumed);
    assert!(!Store::open(&path).unwrap().is_progressive());
}

#[test]
fn live_mutation_after_progressive_gate_creates_one_coalesced_repair() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("projection.sqlite");
    let mut store = Store::open_progressive(&path).unwrap();
    store
        .apply(&Batch {
            ops: vec![
                IngestOp::Runtime(runtime()),
                IngestOp::SessionMeta(session(StreamState::Indexed)),
                message_op(message("first")),
            ],
            checkpoints: vec![],
            source: None,
        })
        .unwrap();
    store.finish_progressive_load().unwrap();
    store
        .apply(&Batch {
            ops: vec![message_op(message("second"))],
            checkpoints: vec![],
            source: None,
        })
        .unwrap();
    store
        .apply(&Batch {
            ops: vec![message_op(message("third"))],
            checkpoints: vec![],
            source: None,
        })
        .unwrap();
    let state = queries::projection_state(store.conn()).unwrap();
    assert_eq!(state.search_revision, 2);
    assert_eq!(
        store
            .conn()
            .query_row(
                "SELECT COUNT(*), MAX(revision) FROM search_dirty",
                [],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )
            .unwrap(),
        (1, 2)
    );
}

#[test]
fn live_noop_runtime_and_session_metadata_do_not_dirty_search() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("projection.sqlite");
    let mut store = Store::open_progressive(&path).unwrap();
    store
        .apply(&Batch {
            ops: vec![
                IngestOp::Runtime(runtime()),
                IngestOp::SessionMeta(session(StreamState::Indexed)),
            ],
            checkpoints: vec![],
            source: None,
        })
        .unwrap();
    store.finish_progressive_load().unwrap();

    let mut same_session = session(StreamState::Pending);
    same_session.title = Some("a newer title does not affect search".into());
    same_session.updated_at = 99;
    store
        .apply(&Batch {
            ops: vec![
                IngestOp::Runtime(runtime()),
                IngestOp::SessionMeta(same_session),
            ],
            checkpoints: vec![],
            source: None,
        })
        .unwrap();

    let state = queries::projection_state(store.conn()).unwrap();
    assert_eq!(state.search_revision, 0);
    assert_eq!(
        store
            .conn()
            .query_row("SELECT COUNT(*) FROM search_dirty", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        0
    );
}

#[test]
fn each_public_source_activation_canonicalizes_multi_file_session_order() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("projection.sqlite");
    let mut store = Store::open_progressive(&path).unwrap();
    store
        .apply(&Batch {
            ops: vec![
                IngestOp::Runtime(runtime()),
                IngestOp::SessionMeta(session(StreamState::Streaming)),
            ],
            checkpoints: ["z.jsonl", "a.jsonl"]
                .into_iter()
                .map(|path| CheckpointUpdate {
                    runtime_id: "rt".into(),
                    stream: "transcript".into(),
                    path: path.into(),
                    session_id: Some("s".into()),
                    expected: None,
                    next: format!("checkpoint-{path}"),
                    updated_at: Some(1),
                })
                .collect(),
            source: None,
        })
        .unwrap();
    let z = queries::cursor_state(store.conn(), "rt", "z.jsonl")
        .unwrap()
        .unwrap()
        .id;
    let a = queries::cursor_state(store.conn(), "rt", "a.jsonl")
        .unwrap()
        .unwrap()
        .id;

    let mut z_message = message("z source");
    z_message.timeline_id = "z-message".into();
    z_message.src_cursor = Some(z);
    z_message.src_offset = Some(10);
    store
        .apply(&Batch {
            ops: vec![message_op(z_message)],
            checkpoints: vec![],
            source: Some(SourceWrite {
                cursor_id: z,
                session_id: "s".into(),
                generation: 1,
                first_sequence: 0,
                mode: SourceMode::Activate,
            }),
        })
        .unwrap();

    let mut a_message = message("a source");
    a_message.timeline_id = "a-message".into();
    a_message.src_cursor = Some(a);
    a_message.src_offset = Some(20);
    store
        .apply(&Batch {
            ops: vec![message_op(a_message)],
            checkpoints: vec![],
            source: Some(SourceWrite {
                cursor_id: a,
                session_id: "s".into(),
                generation: 1,
                first_sequence: 0,
                mode: SourceMode::Activate,
            }),
        })
        .unwrap();

    let timeline = queries::timeline_page(store.conn(), "s", None, 10).unwrap();
    assert_eq!(
        timeline
            .iter()
            .map(|row| row.timeline_id.as_str())
            .collect::<Vec<_>>(),
        ["a-message", "z-message"]
    );
    assert_eq!(
        timeline.iter().map(|row| row.ordinal).collect::<Vec<_>>(),
        [0, 1]
    );
}

#[test]
fn path_ordered_public_sources_keep_final_ordinals_without_session_reranks() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("projection.sqlite");
    let mut store = Store::open_progressive(&path).unwrap();
    store
        .apply(&Batch {
            ops: vec![
                IngestOp::Runtime(runtime()),
                IngestOp::SessionMeta(session(StreamState::Streaming)),
            ],
            checkpoints: ["a.jsonl", "z.jsonl"]
                .into_iter()
                .map(|path| CheckpointUpdate {
                    runtime_id: "rt".into(),
                    stream: "transcript".into(),
                    path: path.into(),
                    session_id: Some("s".into()),
                    expected: None,
                    next: format!("checkpoint-{path}"),
                    updated_at: Some(1),
                })
                .collect(),
            source: None,
        })
        .unwrap();

    for (path, timeline_id) in [("a.jsonl", "a-message"), ("z.jsonl", "z-message")] {
        let cursor = queries::cursor_state(store.conn(), "rt", path)
            .unwrap()
            .unwrap()
            .id;
        let mut row = message(path);
        row.timeline_id = timeline_id.into();
        row.src_cursor = Some(cursor);
        row.src_offset = Some(1);
        let stats = store
            .apply(&Batch {
                ops: vec![message_op(row)],
                checkpoints: vec![],
                source: Some(SourceWrite {
                    cursor_id: cursor,
                    session_id: "s".into(),
                    generation: 1,
                    first_sequence: 0,
                    mode: SourceMode::Activate,
                }),
            })
            .unwrap();
        assert_eq!(stats.profile.session_renumbers, 0);
    }

    let timeline = queries::timeline_page(store.conn(), "s", None, 10).unwrap();
    assert_eq!(
        timeline
            .iter()
            .map(|row| (row.timeline_id.as_str(), row.ordinal))
            .collect::<Vec<_>>(),
        [("a-message", 0), ("z-message", 1)]
    );
}
