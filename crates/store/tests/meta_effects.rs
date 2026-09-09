//! The metadata granularity of the hot-tier seam through real applies: which
//! identity keys the store hands the mirror. The load-bearing invariants —
//! observation costs nothing until it is asked for, the cold paths report
//! metadata even though their row mutation log is suppressed, staging that
//! projects nothing reports nothing, a rolled-back batch reports nothing, and
//! a drain takes everything accumulated and clears.
//!
//! Then what the mirror makes of those keys: one refresh absorbs, drops, and
//! stays silent in a single pass, and a read that fails partway leaves the
//! mirror where the caller's retry needs it — a drain is consumed once, so
//! diffs lost to a half-applied refresh are lost to subscribers for good.

use rosetta_core::IngestOp;
use rosetta_core::rows::{
    MessageKind, MessageRow, ProjectRow, RuntimeRow, SessionRole, SessionRow, StreamState,
};
use rosetta_store::hot::{Effects, MetaEffects, MetaMirror, Mutation};
use rosetta_store::{Batch, CheckpointUpdate, SourceMode, SourceWrite, Store, StoreError, queries};

fn session(id: &str) -> SessionRow {
    SessionRow {
        id: id.into(),
        native_session_id: format!("native-{id}"),
        runtime_id: "rt1".into(),
        project_id: Some("p1".into()),
        cwd: Some("/w/rosetta".into()),
        parent_session_id: None,
        delegation: None,
        root_session_id: id.into(),
        role: SessionRole::Main,
        title: None,
        summary: None,
        started_at: 10_000,
        updated_at: 10_000,
        activity: None,
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
                session_id: Some("s1".into()),
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

fn keys(runtimes: &[&str], projects: &[&str], sessions: &[&str]) -> MetaEffects {
    let owned = |ids: &[&str]| ids.iter().map(|id| (*id).to_string()).collect();
    MetaEffects {
        runtimes: owned(runtimes),
        projects: owned(projects),
        sessions: owned(sessions),
    }
}

/// The mirror's whole resident surface in a stable order — the thing a
/// refresh and a whole-surface warm must agree on.
fn resident(mirror: &MetaMirror) -> (Vec<RuntimeRow>, Vec<ProjectRow>, Vec<SessionRow>) {
    let mut runtimes: Vec<_> = mirror.runtimes().cloned().collect();
    runtimes.sort_by(|left, right| left.id.cmp(&right.id));
    let mut projects: Vec<_> = mirror.projects().cloned().collect();
    projects.sort_by(|left, right| left.id.cmp(&right.id));
    let mut sessions: Vec<_> = mirror.sessions().cloned().collect();
    sessions.sort_by(|left, right| left.id.cmp(&right.id));
    (runtimes, projects, sessions)
}

fn warmed(store: &Store) -> MetaMirror {
    let mut mirror = MetaMirror::default();
    mirror.warm(store.conn()).unwrap();
    mirror
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
/// scoped to watched sessions — and nothing is watched here, which is the
/// backfill's common case: a full mutation for every cold row is waste
/// nobody reads. The metadata half must still report, or the mirror cannot
/// follow a backfill without re-reading the whole projection.
fn assert_cold_reports_metadata_without_row_effects(mut store: Store) {
    store.observe_effects_watched(std::collections::BTreeSet::new());
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
            session_id: Some("s1".into()),
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

/// One refresh mixing every outcome a fetch can produce: a row that changed,
/// rows that did not, a row the projection no longer holds, and a name it
/// never held. The absent names sort last, so the rows that do resolve are
/// read before the absences are reached — and the pass still leaves exactly
/// the surface a whole-projection warm would have built.
#[test]
fn one_refresh_absorbs_drops_and_stays_silent_across_a_mixed_batch() {
    let mut store = Store::open_in_memory().unwrap();
    store.apply(&meta_batch()).unwrap();

    let mut mirror = MetaMirror::default();
    let all = keys(&["rt1"], &["p1"], &["s1", "s2"]);
    assert_eq!(
        mirror.refresh_meta(store.conn(), &all).unwrap().len(),
        4,
        "a cold mirror absorbs every named row"
    );

    let mut retitled = session("s1");
    retitled.title = Some("Named".into());
    store
        .apply(&Batch {
            ops: vec![IngestOp::SessionMeta(retitled)],
            ..Batch::default()
        })
        .unwrap();
    // No store path deletes identity rows today; this stands in for a
    // projection swapped underneath a live mirror.
    store
        .conn()
        .execute("DELETE FROM session WHERE id = 's2'", [])
        .unwrap();

    let mixed = keys(&["rt1"], &["p1"], &["s1", "s2", "s3-never-held"]);
    match mirror
        .refresh_meta(store.conn(), &mixed)
        .unwrap()
        .as_slice()
    {
        [Mutation::Session(row)] => {
            assert_eq!(row.id, "s1");
            assert_eq!(row.title.as_deref(), Some("Named"));
        }
        other => panic!("expected exactly the changed session, got {other:?}"),
    }
    assert_eq!(
        resident(&mirror),
        resident(&warmed(&store)),
        "the dropped row left and the unchanged ones stayed put"
    );
    assert!(
        mirror
            .refresh_meta(store.conn(), &mixed)
            .unwrap()
            .is_empty(),
        "nothing resident is stale"
    );
}

/// A read that fails partway through: `started_at` holds text the row mapper
/// cannot decode — the stand-in for any transient read failure — and s2 is
/// reached only after the runtime, the project, and s1 have already been read
/// successfully. Absorbing while reading would make those three resident and
/// then discard their diffs with the error, and the drain that named them is
/// spent: the retry would find them unchanged and publish nothing, so no
/// subscriber would ever hear about them. The fetch must therefore mutate
/// nothing until it has all of the rows.
#[test]
fn a_failed_fetch_leaves_the_mirror_untouched_and_the_retry_converges() {
    let mut store = Store::open_in_memory().unwrap();
    store.apply(&meta_batch()).unwrap();
    store
        .conn()
        .execute(
            "UPDATE session SET started_at = 'undecodable' WHERE id = 's2'",
            [],
        )
        .unwrap();

    let mut mirror = MetaMirror::default();
    let drained = keys(&["rt1"], &["p1"], &["s1", "s2"]);
    assert!(
        mirror.refresh_meta(store.conn(), &drained).is_err(),
        "the undecodable row fails the fetch"
    );
    assert_eq!(
        resident(&mirror),
        resident(&MetaMirror::default()),
        "rows read before the failure never reached the mirror"
    );

    store
        .conn()
        .execute("UPDATE session SET started_at = 10000 WHERE id = 's2'", [])
        .unwrap();
    assert_eq!(
        mirror.refresh_meta(store.conn(), &drained).unwrap().len(),
        4,
        "retrying the same effects still carries every diff"
    );
    assert_eq!(resident(&mirror), resident(&warmed(&store)));
}

/// The same property at the mirror's other two entry points, which are the
/// ones already carrying live traffic: the timed whole-surface rewarm and the
/// row-granularity refresh behind `Hub::absorb`. Neither may leave a row
/// resident that its failed pass could not publish.
#[test]
fn a_failed_warm_or_refresh_absorbs_nothing_either() {
    let mut store = Store::open_in_memory().unwrap();
    store.apply(&meta_batch()).unwrap();
    store
        .conn()
        .execute(
            "UPDATE session SET started_at = 'undecodable' WHERE id = 's2'",
            [],
        )
        .unwrap();

    let effects = Effects {
        touched_runtimes: ["rt1".to_string()].into_iter().collect(),
        touched_projects: ["p1".to_string()].into_iter().collect(),
        touched_sessions: ["s1".to_string(), "s2".to_string()].into_iter().collect(),
        ..Effects::default()
    };
    let mut warming = MetaMirror::default();
    let mut refreshing = MetaMirror::default();
    assert!(warming.warm(store.conn()).is_err());
    assert!(refreshing.refresh(store.conn(), &effects).is_err());
    assert_eq!(
        resident(&warming),
        resident(&MetaMirror::default()),
        "the runtimes and projects a warm reads first never landed"
    );
    assert_eq!(
        resident(&refreshing),
        resident(&MetaMirror::default()),
        "nor the rows a refresh read before the undecodable one"
    );

    store
        .conn()
        .execute("UPDATE session SET started_at = 10000 WHERE id = 's2'", [])
        .unwrap();
    assert_eq!(warming.warm(store.conn()).unwrap().len(), 4);
    assert_eq!(refreshing.refresh(store.conn(), &effects).unwrap().len(), 4);
    assert_eq!(resident(&warming), resident(&refreshing));
}
