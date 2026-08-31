//! Append growth of a multi-source session must re-rank before it is read.
//! A budget-paused sibling's remainder (or live growth) appends rows at its
//! own file-local checkpoint ordinals; once another source's activation has
//! renumbered the session, those ordinals collide with the assigned range.
//! The Append arm now detects the sibling and renumbers — this is the proof.

use rosetta_core::IngestOp;
use rosetta_core::rows::{
    MessageKind, MessageRow, ProjectRow, RuntimeRow, SessionRole, SessionRow, StreamState,
};
use rosetta_store::{Batch, CheckpointUpdate, SourceMode, SourceWrite, Store, queries};

fn meta_batch() -> Batch {
    Batch {
        ops: vec![
            IngestOp::Runtime(RuntimeRow {
                id: "rt1".into(),
                kind: "grok".into(),
                data_root: "~/.grok".into(),
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
            IngestOp::SessionMeta(SessionRow {
                id: "s1".into(),
                native_session_id: "native-s1".into(),
                runtime_id: "rt1".into(),
                project_id: Some("p1".into()),
                cwd: Some("/w/rosetta".into()),
                parent_session_id: None,
                root_session_id: "s1".into(),
                role: SessionRole::Main,
                title: None,
                summary: None,
                started_at: 10_000,
                updated_at: 10_000,
                stream_state: StreamState::Streaming,
            }),
        ],
        ..Batch::default()
    }
}

fn message(timeline_id: &str, ordinal: i64, src_offset: i64, src_cursor: i64) -> MessageRow {
    MessageRow {
        timeline_id: timeline_id.into(),
        session_id: "s1".into(),
        ordinal,
        kind: MessageKind::User,
        subtype: None,
        uuid: None,
        parent_uuid: None,
        ts: Some(3_600_000),
        model: None,
        effort: None,
        content: Some(format!("content of {timeline_id}")),
        tool: None,
        attachments: Vec::new(),
        citations: Vec::new(),
        group: None,
        response: None,
        block_index: None,
        branch_tool_id: None,
        usage: None,
        agent_version: None,
        src_cursor: Some(src_cursor),
        src_offset: Some(src_offset),
    }
}

fn message_op(row: MessageRow) -> IngestOp {
    let mut ops = IngestOp::project_message(row);
    assert_eq!(ops.len(), 1);
    ops.pop().unwrap()
}

fn install_cursor(store: &mut Store, path: &str) -> i64 {
    store
        .apply(&Batch {
            checkpoints: vec![CheckpointUpdate {
                runtime_id: "rt1".into(),
                stream: "transcript".into(),
                path: path.into(),
                session_id: "s1".into(),
                expected: None,
                next: format!("ck-{path}"),
                updated_at: Some(1),
            }],
            ..Batch::default()
        })
        .unwrap();
    queries::cursor_state(store.conn(), "rt1", path)
        .unwrap()
        .unwrap()
        .id
}

fn source(cursor: i64, generation: u64, first_sequence: u64, mode: SourceMode) -> SourceWrite {
    SourceWrite {
        cursor_id: cursor,
        session_id: "s1".into(),
        generation,
        first_sequence,
        mode,
    }
}

fn timeline_order(store: &Store) -> Vec<(String, i64)> {
    queries::timeline_page(store.conn(), "s1", None, 100)
        .unwrap()
        .into_iter()
        .map(|entry| (entry.timeline_id.clone(), entry.ordinal))
        .collect()
}

#[test]
fn append_after_a_sibling_renumber_re_ranks_instead_of_colliding() {
    let mut store = Store::open_in_memory().unwrap();
    store.apply(&meta_batch()).unwrap();

    // Source M ("m.jsonl") activates first with two rows at its own file-
    // local ordinals — the budget-paused prefix of a sliced file.
    let cursor_m = install_cursor(&mut store, "m.jsonl");
    store
        .apply(&Batch {
            ops: vec![
                message_op(message("m-first", 0, 0, cursor_m)),
                message_op(message("m-second", 1, 100, cursor_m)),
            ],
            source: Some(source(cursor_m, 1, 0, SourceMode::Activate)),
            ..Batch::default()
        })
        .unwrap();

    // Sibling A ("a.jsonl") activates and renumbers the session: its path
    // sorts FIRST, so the final order is a-row, m-first, m-second.
    let cursor_a = install_cursor(&mut store, "a.jsonl");
    store
        .apply(&Batch {
            ops: vec![message_op(message("a-row", 0, 0, cursor_a))],
            source: Some(source(cursor_a, 1, 0, SourceMode::Activate)),
            ..Batch::default()
        })
        .unwrap();
    assert_eq!(
        timeline_order(&store),
        vec![
            ("a-row".into(), 0),
            ("m-first".into(), 1),
            ("m-second".into(), 2),
        ],
        "the sibling's activation owns the session order"
    );

    // M's remainder appends at its stale file-local ordinal 2 — exactly the
    // range the renumber assigned to m-second. The Append arm must re-rank
    // the session in the same transaction rather than leave the collision
    // readable.
    store
        .apply(&Batch {
            ops: vec![message_op(message("m-appended", 2, 200, cursor_m))],
            source: Some(source(cursor_m, 1, 2, SourceMode::Append)),
            ..Batch::default()
        })
        .unwrap();

    assert_eq!(
        timeline_order(&store),
        vec![
            ("a-row".into(), 0),
            ("m-first".into(), 1),
            ("m-second".into(), 2),
            ("m-appended".into(), 3),
        ],
        "the appended remainder lands after its own earlier rows, never on them"
    );
}

#[test]
fn single_source_appends_keep_their_ordinals_without_a_renumber() {
    let mut store = Store::open_in_memory().unwrap();
    store.apply(&meta_batch()).unwrap();

    let cursor = install_cursor(&mut store, "solo.jsonl");
    store
        .apply(&Batch {
            ops: vec![message_op(message("first", 0, 0, cursor))],
            source: Some(source(cursor, 1, 0, SourceMode::Activate)),
            ..Batch::default()
        })
        .unwrap();
    store
        .apply(&Batch {
            ops: vec![message_op(message("second", 1, 50, cursor))],
            source: Some(source(cursor, 1, 1, SourceMode::Append)),
            ..Batch::default()
        })
        .unwrap();

    assert_eq!(
        timeline_order(&store),
        vec![("first".into(), 0), ("second".into(), 1)],
        "the common single-file case appends without paying a re-rank"
    );
}
