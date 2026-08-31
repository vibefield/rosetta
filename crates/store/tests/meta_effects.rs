//! The metadata granularity of the hot-tier seam through real applies: which
//! identity keys the store hands the mirror. The load-bearing invariants —
//! observation costs nothing until it is asked for, the cold paths report
//! metadata even though their row mutation log is suppressed, staging that
//! projects nothing reports nothing, a rolled-back batch reports nothing, and
//! a drain takes everything accumulated and clears.

use rosetta_core::IngestOp;
use rosetta_core::rows::{
    MessageKind, MessageRow, ProjectRow, RuntimeRow, SessionRole, SessionRow, StreamState,
};
use rosetta_store::hot::MetaEffects;
use rosetta_store::{Batch, CheckpointUpdate, SourceMode, SourceWrite, Store, StoreError, queries};

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

/// The bounded metadata batch: the only place session identity, cwd, and
/// project ownership are settled.
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

fn message(timeline_id: &str) -> MessageRow {
    MessageRow {
        timeline_id: timeline_id.into(),
        session_id: "s1".into(),
        ordinal: 0,
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
        src_cursor: None,
        src_offset: None,
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
                next: "ck-v1".into(),
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

fn ids(keys: &std::collections::BTreeSet<String>) -> Vec<&str> {
    keys.iter().map(String::as_str).collect()
}

#[test]
fn meta_effects_are_off_by_default_and_cost_nothing() {
    let mut store = Store::open_in_memory().unwrap();
    store.apply(&meta_batch()).unwrap();
    assert_eq!(store.drain_meta_effects(), MetaEffects::default());
}

#[test]
fn drain_takes_everything_accumulated_since_the_last_one_and_clears() {
    let mut store = Store::open_in_memory().unwrap();
    store.observe_meta_effects();
    store.apply(&meta_batch()).unwrap();

    // A second committed batch before the first drain: one drain covers both.
    let mut renamed = session("s2");
    renamed.title = Some("Named".into());
    store
        .apply(&Batch {
            ops: vec![IngestOp::SessionMeta(renamed)],
            ..Batch::default()
        })
        .unwrap();

    let drained = store.drain_meta_effects();
    assert_eq!(ids(&drained.runtimes), ["rt1"]);
    assert_eq!(ids(&drained.projects), ["p1"]);
    assert_eq!(ids(&drained.sessions), ["s1", "s2"]);
    assert!(
        store.drain_meta_effects().is_empty(),
        "a drain clears what it returned"
    );
}

#[test]
fn readiness_badge_changes_land_and_no_op_writes_do_not() {
    let mut store = Store::open_in_memory().unwrap();
    store.observe_meta_effects();
    store.apply(&meta_batch()).unwrap();
    store.drain_meta_effects();

    assert!(
        store
            .set_session_stream_state("s1", StreamState::Indexed)
            .unwrap()
    );
    assert_eq!(ids(&store.drain_meta_effects().sessions), ["s1"]);

    // The badge is already Indexed: no row changed, so the mirror has no
    // work to be handed.
    assert!(
        !store
            .set_session_stream_state("s1", StreamState::Indexed)
            .unwrap()
    );
    assert!(store.drain_meta_effects().is_empty());
}

/// A cold generation projects canonical rows with the row-level mutation log
/// switched off — a full mutation for every cold row is waste nobody reads.
/// The metadata half must still report, or the mirror cannot follow a
/// backfill without re-reading the whole projection.
fn assert_cold_reports_metadata_without_row_effects(mut store: Store) {
    store.observe_effects();
    store.observe_meta_effects();
    store.apply(&meta_batch()).unwrap();
    let cold_meta = store.drain_meta_effects();
    assert_eq!(ids(&cold_meta.runtimes), ["rt1"]);
    assert_eq!(ids(&cold_meta.projects), ["p1"]);
    assert_eq!(ids(&cold_meta.sessions), ["s1", "s2"]);

    let cursor = install_cursor(&mut store, "a.jsonl");
    store.drain_effects();
    store.drain_meta_effects();

    let mut row = message("cold-1");
    row.src_cursor = Some(cursor);
    row.src_offset = Some(0);
    store
        .apply(&Batch {
            ops: vec![message_op(row)],
            source: Some(source(cursor, 1, 0, SourceMode::Activate)),
            ..Batch::default()
        })
        .unwrap();

    assert!(
        store.drain_effects().is_empty(),
        "the cold path suppresses row effects: no mutation log, no touches"
    );
    let meta = store.drain_meta_effects();
    assert_eq!(
        ids(&meta.sessions),
        ["s1"],
        "the projected session still reaches the mirror"
    );
    assert!(
        meta.projects.is_empty() && meta.runtimes.is_empty(),
        "a stream batch settles no ownership: {meta:?}"
    );
}

#[test]
fn private_cold_rebuild_reports_metadata_without_row_effects() {
    assert_cold_reports_metadata_without_row_effects(Store::open_in_memory_for_rebuild().unwrap());
}

#[test]
fn progressive_cold_load_reports_metadata_without_row_effects() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = Store::open_progressive(&dir.path().join("projection.sqlite")).unwrap();
    assert!(store.is_progressive());
    assert_cold_reports_metadata_without_row_effects(store);
}

#[test]
fn live_staging_reports_nothing_and_activation_reports_the_swept_session() {
    let mut store = Store::open_in_memory().unwrap();
    store.observe_meta_effects();
    store.apply(&meta_batch()).unwrap();
    let cursor = install_cursor(&mut store, "x.jsonl");
    store.drain_meta_effects();

    store
        .apply(&Batch {
            ops: vec![message_op(message("g1-a"))],
            source: Some(source(cursor, 1, 0, SourceMode::Stage)),
            ..Batch::default()
        })
        .unwrap();
    assert!(
        store.drain_meta_effects().is_empty(),
        "staged ops wait in process RAM; nothing was projected to report"
    );

    store
        .apply(&Batch {
            ops: vec![],
            source: Some(source(cursor, 1, 1, SourceMode::Activate)),
            ..Batch::default()
        })
        .unwrap();
    assert_eq!(
        ids(&store.drain_meta_effects().sessions),
        ["s1"],
        "the activation sweep rewrote this session's title and activity time"
    );
}

#[test]
fn a_rolled_back_batch_contributes_nothing() {
    let mut store = Store::open_in_memory().unwrap();
    store.observe_meta_effects();
    store.apply(&meta_batch()).unwrap();
    install_cursor(&mut store, "x.jsonl");
    store.drain_meta_effects();

    // A stale checkpoint CAS rolls the whole batch back, so the session it
    // carried must not reach the mirror's work list either.
    let result = store.apply(&Batch {
        ops: vec![IngestOp::SessionMeta(session("s-lost"))],
        checkpoints: vec![CheckpointUpdate {
            runtime_id: "rt1".into(),
            stream: "transcript".into(),
            path: "x.jsonl".into(),
            session_id: "s1".into(),
            expected: Some("ck-v0-stale".into()),
            next: "ck-v2".into(),
            updated_at: Some(2),
        }],
        ..Batch::default()
    });
    assert!(matches!(result, Err(StoreError::CheckpointConflict { .. })));
    assert!(queries::session(store.conn(), "s-lost").unwrap().is_none());
    assert!(
        store.drain_meta_effects().is_empty(),
        "effects merge on commit only"
    );
}
