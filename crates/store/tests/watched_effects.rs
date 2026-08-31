//! The row-effects scope through real applies: cold passes stream to the
//! sessions a reader holds open and stay silent for everything else. The
//! load-bearing invariants — a watched cold seal emits exactly one sweep
//! marker (never its provisional per-row mutations), watched appends stream
//! precise rows, an append renumber voids delivered ordinals with a marker,
//! a cold retry or abandon that sweeps projected rows tells its watchers,
//! the armed scope survives drains and widens on [`Store::observe_effects`],
//! and a private bulk rebuild emits nothing no matter what is armed.

use std::collections::BTreeSet;

use rosetta_core::IngestOp;
use rosetta_core::rows::{
    MessageKind, MessageRow, ProjectRow, RuntimeRow, SessionRole, SessionRow, StreamState,
};
use rosetta_store::hot::Mutation;
use rosetta_store::{Batch, CheckpointUpdate, SourceMode, SourceWrite, Store, queries};

fn session(id: &str) -> SessionRow {
    SessionRow {
        id: id.into(),
        native_session_id: format!("native-{id}"),
        runtime_id: "rt1".into(),
        project_id: Some("p1".into()),
        cwd: Some("/w/rosetta".into()),
        parent_session_id: None,
        root_session_id: id.into(),
        role: SessionRole::Main,
        title: None,
        summary: None,
        started_at: 10_000,
        updated_at: 10_000,
        stream_state: StreamState::Streaming,
    }
}

fn meta_batch() -> Batch {
    Batch {
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
        ..Batch::default()
    }
}

fn message(timeline_id: &str, session_id: &str, ordinal: i64, cursor: i64) -> MessageRow {
    MessageRow {
        timeline_id: timeline_id.into(),
        session_id: session_id.into(),
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
        src_cursor: Some(cursor),
        src_offset: Some(ordinal * 100),
    }
}

fn message_op(row: MessageRow) -> IngestOp {
    let mut ops = IngestOp::project_message(row);
    assert_eq!(ops.len(), 1);
    ops.pop().unwrap()
}

fn install_cursor(store: &mut Store, path: &str, session_id: &str) -> i64 {
    store
        .apply(&Batch {
            checkpoints: vec![CheckpointUpdate {
                runtime_id: "rt1".into(),
                stream: "transcript".into(),
                path: path.into(),
                session_id: session_id.into(),
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

fn source(
    cursor: i64,
    session_id: &str,
    generation: u64,
    seq: u64,
    mode: SourceMode,
) -> SourceWrite {
    SourceWrite {
        cursor_id: cursor,
        session_id: session_id.into(),
        generation,
        first_sequence: seq,
        mode,
    }
}

fn watched(sessions: &[&str]) -> BTreeSet<String> {
    sessions.iter().map(|id| (*id).to_string()).collect()
}

/// The mutation log as (variant, session) pairs — enough shape to pin
/// exactly what a drain carried without restating whole rows.
fn shape(mutations: &[Mutation]) -> Vec<(&'static str, String)> {
    mutations
        .iter()
        .map(|mutation| match mutation {
            Mutation::Message(row) => ("message", row.session_id.clone()),
            Mutation::ToolUpdated { session_id, .. } => ("tool", session_id.clone()),
            Mutation::Usage { session_id, .. } => ("usage", session_id.clone()),
            Mutation::SessionSwept { session_id } => ("swept", session_id.clone()),
            Mutation::Session(row) => ("session", row.id.clone()),
            Mutation::Project(row) => ("project", row.id.clone()),
            Mutation::Runtime(row) => ("runtime", row.id.clone()),
        })
        .collect()
}

fn progressive_store(dir: &tempfile::TempDir) -> Store {
    let store = Store::open_progressive(&dir.path().join("projection.sqlite")).unwrap();
    assert!(store.is_progressive());
    store
}

#[test]
fn a_watched_cold_seal_emits_one_sweep_marker_and_unwatched_sessions_stay_silent() {
    let dir = tempfile::TempDir::new().unwrap();
    let mut store = progressive_store(&dir);
    store.observe_effects_watched(watched(&["s1"]));
    store.observe_meta_effects();
    store.apply(&meta_batch()).unwrap();
    let cursor_a = install_cursor(&mut store, "a.jsonl", "s1");
    let cursor_b = install_cursor(&mut store, "b.jsonl", "s2");
    store.drain_effects();
    store.drain_meta_effects();

    // The watched session's first generation seals: subscribers get exactly
    // the replacement-snapshot marker, never the provisional per-row log.
    store
        .apply(&Batch {
            ops: vec![
                message_op(message("a-1", "s1", 0, cursor_a)),
                message_op(message("a-2", "s1", 1, cursor_a)),
            ],
            source: Some(source(cursor_a, "s1", 1, 0, SourceMode::Activate)),
            ..Batch::default()
        })
        .unwrap();
    let drained = store.drain_effects();
    assert_eq!(shape(&drained.mutations), [("swept", "s1".to_string())]);
    assert_eq!(
        drained.touched_sessions,
        watched(&["s1"]),
        "the marker rides with its mirror touch"
    );

    // The unwatched session's seal collects nothing — its readers follow the
    // metadata channel, which must still name it.
    store
        .apply(&Batch {
            ops: vec![message_op(message("b-1", "s2", 0, cursor_b))],
            source: Some(source(cursor_b, "s2", 1, 0, SourceMode::Activate)),
            ..Batch::default()
        })
        .unwrap();
    assert!(
        store.drain_effects().is_empty(),
        "an unwatched seal pays no mutation log"
    );
    assert!(
        store.drain_meta_effects().sessions.contains("s2"),
        "unwatched metadata still reaches the mirror"
    );
}

#[test]
fn watched_appends_stream_precise_rows_without_a_marker() {
    let dir = tempfile::TempDir::new().unwrap();
    let mut store = progressive_store(&dir);
    store.observe_effects_watched(watched(&["s1"]));
    store.observe_meta_effects();
    store.apply(&meta_batch()).unwrap();
    let cursor = install_cursor(&mut store, "a.jsonl", "s1");
    store
        .apply(&Batch {
            ops: vec![message_op(message("a-1", "s1", 0, cursor))],
            source: Some(source(cursor, "s1", 1, 0, SourceMode::Activate)),
            ..Batch::default()
        })
        .unwrap();
    store.drain_effects();

    // The paused remainder (or live growth): final ordinals, so the rows
    // themselves stream — no replacement snapshot needed.
    store
        .apply(&Batch {
            ops: vec![message_op(message("a-2", "s1", 1, cursor))],
            source: Some(source(cursor, "s1", 1, 1, SourceMode::Append)),
            ..Batch::default()
        })
        .unwrap();
    let drained = store.drain_effects();
    assert_eq!(shape(&drained.mutations), [("message", "s1".to_string())]);
    match &drained.mutations[0] {
        Mutation::Message(row) => assert_eq!(row.timeline_id, "a-2"),
        other => panic!("expected the appended row, got {other:?}"),
    }
}

#[test]
fn an_append_renumber_voids_delivered_ordinals_with_a_sweep_marker() {
    // The append_renumber scenario, observed: source M activates, sibling A
    // renumbers the session, M's remainder appends at a colliding ordinal.
    let mut store = Store::open_in_memory().unwrap();
    store.observe_effects();
    store.apply(&meta_batch()).unwrap();
    let cursor_m = install_cursor(&mut store, "m.jsonl", "s1");
    let cursor_a = install_cursor(&mut store, "a.jsonl", "s1");
    store
        .apply(&Batch {
            ops: vec![
                message_op(message("m-first", "s1", 0, cursor_m)),
                message_op(message("m-second", "s1", 1, cursor_m)),
            ],
            source: Some(source(cursor_m, "s1", 1, 0, SourceMode::Activate)),
            ..Batch::default()
        })
        .unwrap();
    store
        .apply(&Batch {
            ops: vec![message_op(message("a-row", "s1", 0, cursor_a))],
            source: Some(source(cursor_a, "s1", 1, 0, SourceMode::Activate)),
            ..Batch::default()
        })
        .unwrap();
    store.drain_effects();

    store
        .apply(&Batch {
            ops: vec![message_op(message("m-appended", "s1", 2, cursor_m))],
            source: Some(source(cursor_m, "s1", 1, 2, SourceMode::Append)),
            ..Batch::default()
        })
        .unwrap();
    assert_eq!(
        shape(&store.drain_effects().mutations),
        [("message", "s1".to_string()), ("swept", "s1".to_string()),],
        "the re-rank voids the row's just-delivered ordinal, so the marker follows it"
    );
}

#[test]
fn a_cold_retry_sweep_and_an_abandon_resync_their_watchers() {
    let dir = tempfile::TempDir::new().unwrap();
    let mut store = progressive_store(&dir);
    store.observe_effects_watched(watched(&["s1"]));
    store.apply(&meta_batch()).unwrap();
    let cursor = install_cursor(&mut store, "a.jsonl", "s1");
    store.drain_effects();

    // Generation 1 stages rows directly into the public projection, then
    // dies unclean: the retrying generation 2 sweeps them first, and a
    // watcher must not keep rows the sweep removed.
    store
        .apply(&Batch {
            ops: vec![message_op(message("g1-a", "s1", 0, cursor))],
            source: Some(source(cursor, "s1", 1, 0, SourceMode::Stage)),
            ..Batch::default()
        })
        .unwrap();
    assert!(
        store.drain_effects().mutations.is_empty(),
        "staged cold rows carry provisional ordinals and never stream"
    );
    store
        .apply(&Batch {
            ops: vec![message_op(message("g2-a", "s1", 0, cursor))],
            source: Some(source(cursor, "s1", 2, 0, SourceMode::Stage)),
            ..Batch::default()
        })
        .unwrap();
    assert_eq!(
        shape(&store.drain_effects().mutations),
        [("swept", "s1".to_string())],
        "the retry's sweep of generation 1's projected rows resyncs the watcher"
    );

    // The retry itself ends unclean: the abandon sweeps its projected rows
    // and says so.
    store
        .apply(&Batch {
            ops: vec![],
            source: Some(source(cursor, "s1", 2, 1, SourceMode::Abandon)),
            ..Batch::default()
        })
        .unwrap();
    assert_eq!(
        shape(&store.drain_effects().mutations),
        [("swept", "s1".to_string())],
        "an abandon that removed projected rows resyncs the watcher too"
    );
}

#[test]
fn the_armed_scope_survives_drains_and_widens_on_observe_effects() {
    let dir = tempfile::TempDir::new().unwrap();
    let mut store = progressive_store(&dir);
    store.observe_effects_watched(watched(&["s1"]));
    store.apply(&meta_batch()).unwrap();
    let cursor = install_cursor(&mut store, "b.jsonl", "s2");
    store.drain_effects();

    store
        .apply(&Batch {
            ops: vec![message_op(message("b-1", "s2", 0, cursor))],
            source: Some(source(cursor, "s2", 1, 0, SourceMode::Activate)),
            ..Batch::default()
        })
        .unwrap();
    assert!(
        store.drain_effects().is_empty(),
        "the drain above must not have reset the watched scope to All"
    );

    // The live handoff widens the same accumulator in place.
    store.observe_effects();
    store
        .apply(&Batch {
            ops: vec![message_op(message("b-2", "s2", 1, cursor))],
            source: Some(source(cursor, "s2", 1, 1, SourceMode::Append)),
            ..Batch::default()
        })
        .unwrap();
    assert_eq!(
        shape(&store.drain_effects().mutations),
        [("message", "s2".to_string())]
    );
}

#[test]
fn a_private_bulk_rebuild_emits_no_row_effects_no_matter_what_is_armed() {
    let mut store = Store::open_in_memory_for_rebuild().unwrap();
    store.observe_effects();
    store.apply(&meta_batch()).unwrap();
    let cursor = install_cursor(&mut store, "a.jsonl", "s1");
    store.drain_effects();

    store
        .apply(&Batch {
            ops: vec![message_op(message("a-1", "s1", 0, cursor))],
            source: Some(source(cursor, "s1", 1, 0, SourceMode::Activate)),
            ..Batch::default()
        })
        .unwrap();
    assert!(
        store.drain_effects().mutations.is_empty(),
        "no reader can observe a private cold database — markers included"
    );
}
