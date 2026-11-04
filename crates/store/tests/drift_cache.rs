//! The Store answers drift's two per-event lookups (the event's cursor path,
//! the aggregate's stored sample) from RAM. The aggregate it keeps must be
//! exactly what reading both from SQLite every time gives: across a resume
//! (a fresh cache over an existing projection), a rebuild numbering cursors
//! in another order, a source rewrite, a deleted cursor whose id SQLite
//! hands out again, and a rolled-back batch.

use rosetta_core::IngestOp;
use rosetta_core::rows::{
    DriftFact, RuntimeRow, SessionRole, SessionRow, SessionUsageRow, StreamState, TokenSnapshot,
};
use rosetta_store::{Batch, CheckpointUpdate, SourceMode, SourceWrite, Store, StoreError};
use rusqlite::types::Value;

const PATHS: [&str; 3] = ["a.jsonl", "b.jsonl", "c.jsonl"];

fn runtime() -> RuntimeRow {
    RuntimeRow {
        id: "rt1".into(),
        kind: "claude-code".into(),
        data_root: "/agent".into(),
        enabled: true,
        account: None,
        config: None,
        skills: None,
        versions_seen: None,
        discovered_at: 1_000,
        label: None,
        provenance: Default::default(),
    }
}

fn checkpoint(path: &str, expected: Option<&str>) -> CheckpointUpdate {
    CheckpointUpdate {
        runtime_id: "rt1".into(),
        stream: "transcript".into(),
        path: path.into(),
        session_id: None,
        expected: expected.map(Into::into),
        next: "fresh".into(),
        updated_at: None,
    }
}

fn add_cursors(store: &mut Store, paths: &[&str]) {
    for path in paths {
        store
            .apply(&Batch {
                checkpoints: vec![checkpoint(path, None)],
                ..Batch::default()
            })
            .unwrap();
    }
}

/// `store` with the runtime and cursors created in `cursor_order`.
fn seeded(mut store: Store, cursor_order: &[&str]) -> Store {
    store
        .apply(&Batch {
            ops: vec![IngestOp::Runtime(runtime())],
            ..Batch::default()
        })
        .unwrap();
    add_cursors(&mut store, cursor_order);
    store
}

fn cursor(store: &Store, path: &str) -> i64 {
    store
        .runtime_cursor_ids("rt1")
        .unwrap()
        .into_iter()
        .find(|(_, candidate)| candidate == path)
        .unwrap()
        .0
}

/// One drift event: `(kind, path, offset, at)`, a missing path meaning a
/// fact with no sample cursor.
type Event = (&'static str, Option<&'static str>, Option<i64>, i64);

fn fact(store: &Store, (kind, path, offset, at): Event) -> IngestOp {
    IngestOp::Drift(DriftFact {
        runtime_id: "rt1".into(),
        agent_version: "2.1.0".into(),
        stream: "transcript".into(),
        record_kind: kind.into(),
        error_code: "unknown_field".into(),
        shape_hash: "h1".into(),
        at: Some(at),
        sample_cursor: path.map(|path| cursor(store, path)),
        sample_offset: offset,
    })
}

fn apply_event(store: &mut Store, event: Event) -> u64 {
    let op = fact(store, event);
    store
        .apply(&Batch {
            ops: vec![op],
            ..Batch::default()
        })
        .unwrap()
        .profile
        .drift_sample_reads
}

/// Every aggregate, its sample cursor mapped to the cursor's path, and the
/// number of distinct events seen.
fn drift(store: &Store) -> (Vec<Vec<Value>>, i64) {
    let mut statement = store
        .conn()
        .prepare(
            "SELECT d.record_kind, d.count, d.first_seen, d.last_seen, c.path, d.sample_offset
             FROM drift_agg d LEFT JOIN ingest_cursor c ON c.id = d.sample_cursor
             ORDER BY d.record_kind",
        )
        .unwrap();
    let rows = statement
        .query_map([], |row| {
            (0..6)
                .map(|index| row.get::<_, Value>(index))
                .collect::<Result<Vec<_>, _>>()
        })
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let seen = store
        .conn()
        .query_row("SELECT count(*) FROM drift_seen", [], |row| row.get(0))
        .unwrap();
    (rows, seen)
}

fn row(
    kind: &str,
    count: i64,
    first: i64,
    last: i64,
    path: Option<&str>,
    offset: Option<i64>,
) -> Vec<Value> {
    vec![
        Value::Text(kind.into()),
        Value::Integer(count),
        Value::Integer(first),
        Value::Integer(last),
        path.map_or(Value::Null, |path| Value::Text(path.into())),
        offset.map_or(Value::Null, Value::Integer),
    ]
}

/// Contested samples for two aggregates, cursor-less ones included.
fn events() -> Vec<Event> {
    vec![
        ("assistant", Some("b.jsonl"), Some(10), 300),
        ("assistant", Some("a.jsonl"), Some(50), 100),
        ("user", Some("c.jsonl"), None, 30),
        ("assistant", None, Some(1), 50),
        ("user", None, Some(9), 10),
        ("assistant", Some("c.jsonl"), None, 400),
        ("assistant", Some("a.jsonl"), Some(20), 200),
        ("user", None, Some(4), 20),
        ("assistant", Some("a.jsonl"), Some(20), 200),
    ]
}

fn expected() -> (Vec<Vec<Value>>, i64) {
    (
        vec![
            row("assistant", 5, 50, 400, Some("a.jsonl"), Some(20)),
            row("user", 3, 10, 30, Some("c.jsonl"), None),
        ],
        8,
    )
}

#[test]
fn an_event_that_cannot_lower_the_sample_reads_nothing() {
    let mut store = seeded(Store::open_in_memory().unwrap(), &PATHS);
    // The aggregate's first event reads (nothing is cached yet); every event
    // that sorts after the cached sample reads nothing.
    assert_eq!(
        apply_event(&mut store, ("assistant", Some("a.jsonl"), Some(5), 1)),
        1
    );
    for event in [
        ("assistant", Some("b.jsonl"), Some(1), 2),
        ("assistant", Some("a.jsonl"), Some(9), 3),
        ("assistant", Some("a.jsonl"), None, 4),
        ("assistant", None, Some(0), 5),
        ("assistant", Some("a.jsonl"), Some(5), 6),
    ] {
        assert_eq!(apply_event(&mut store, event), 0, "{event:?}");
    }
    // An event that sorts first reads the stored sample, then is cached.
    assert_eq!(
        apply_event(&mut store, ("assistant", Some("a.jsonl"), Some(2), 7)),
        1
    );
    assert_eq!(
        apply_event(&mut store, ("assistant", Some("a.jsonl"), Some(3), 8)),
        0
    );
    assert_eq!(
        drift(&store).0,
        vec![row("assistant", 8, 1, 8, Some("a.jsonl"), Some(2))]
    );
}

#[test]
fn a_resumed_store_and_a_rebuild_agree_with_one_pass_at_every_split() {
    let mut one_pass = seeded(Store::open_in_memory().unwrap(), &PATHS);
    for event in events() {
        apply_event(&mut one_pass, event);
    }
    assert_eq!(drift(&one_pass), expected());

    let dir = tempfile::tempdir().unwrap();
    let count = events().len();
    for split in 0..=count {
        // A resume: another cursor-id order, then a restart part way, whose
        // fresh cache must pick the stored samples up from the projection.
        let path = dir.path().join(format!("resume-{split}.sqlite"));
        // b.jsonl's cursor is created only when first needed, before or
        // after the restart.
        let mut store = seeded(Store::open(&path).unwrap(), &["c.jsonl", "a.jsonl"]);
        for event in &events()[..split] {
            ensure_cursor(&mut store, event.1);
            apply_event(&mut store, *event);
        }
        drop(store);
        let mut store = Store::open(&path).unwrap();
        for event in &events()[split..] {
            ensure_cursor(&mut store, event.1);
            apply_event(&mut store, *event);
        }
        assert_eq!(drift(&store), expected(), "resumed after {split} events");
    }

    // A rebuild numbers cursors in ledger order and replays in another.
    let mut rebuilt = seeded(
        Store::open_in_memory_for_rebuild().unwrap(),
        &["b.jsonl", "c.jsonl", "a.jsonl"],
    );
    for event in events().into_iter().rev() {
        apply_event(&mut rebuilt, event);
    }
    assert_eq!(drift(&rebuilt), expected());
    assert_ne!(cursor(&rebuilt, "a.jsonl"), cursor(&one_pass, "a.jsonl"));
}

fn ensure_cursor(store: &mut Store, path: Option<&str>) {
    let Some(path) = path else {
        return;
    };
    let exists = store
        .runtime_cursor_ids("rt1")
        .unwrap()
        .iter()
        .any(|(_, candidate)| candidate == path);
    if !exists {
        add_cursors(store, &[path]);
    }
}

/// A ledger-only source (no canonical session) whose generations carry
/// drift and a usage sample, so a replacement really sweeps the cursor.
#[test]
fn a_rewritten_source_keeps_every_drift_event_and_the_least_sample() {
    let mut store = seeded(Store::open_in_memory().unwrap(), &PATHS);
    store
        .apply(&Batch {
            ops: vec![IngestOp::SessionMeta(SessionRow {
                id: "parent".into(),
                native_session_id: "parent-thread".into(),
                runtime_id: "rt1".into(),
                project_id: None,
                cwd: None,
                parent_session_id: None,
                delegation: None,
                root_session_id: "parent".into(),
                role: SessionRole::Main,
                title: None,
                summary: None,
                started_at: 1_000,
                updated_at: 1_000,
                activity: None,
                stream_state: StreamState::Streaming,
            })],
            ..Batch::default()
        })
        .unwrap();
    let guardian = cursor(&store, "b.jsonl");
    let source = |generation, mode| SourceWrite {
        cursor_id: guardian,
        session_id: "guardian".into(),
        generation,
        first_sequence: 0,
        mode,
    };
    let sample = |key: &str| {
        IngestOp::SessionUsage(SessionUsageRow {
            identity: None,
            session_id: "parent".into(),
            key: key.into(),
            ts: 3_600_000,
            model: None,
            tokens: TokenSnapshot {
                input: Some(1),
                ..Default::default()
            },
        })
    };
    // A sample from another file comes first, so the rewrite's own events
    // contest it.
    apply_event(&mut store, ("assistant", Some("c.jsonl"), Some(3), 10));
    let generation = |store: &mut Store, number, mode, events: &[Event]| {
        let mut ops: Vec<IngestOp> = events.iter().map(|event| fact(store, *event)).collect();
        ops.push(sample(&format!("review:{number}")));
        store
            .apply(&Batch {
                source: Some(source(number, SourceMode::Stage)),
                ..Batch::default()
            })
            .unwrap();
        store
            .apply(&Batch {
                source: Some(source(number, mode)),
                ops,
                checkpoints: vec![],
            })
            .unwrap()
            .source_sweeps
    };
    let samples = |store: &Store| -> Vec<String> {
        store
            .conn()
            .prepare("SELECT sample_key FROM session_usage ORDER BY 1")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    generation(
        &mut store,
        1,
        SourceMode::Activate,
        &[("assistant", Some("b.jsonl"), Some(40), 20)],
    );
    assert_eq!(samples(&store), ["review:1"]);
    // The rewrite repeats an event (counted once), adds one that sorts
    // first and one that does not.
    let sweeps = generation(
        &mut store,
        2,
        SourceMode::Activate,
        &[
            ("assistant", Some("b.jsonl"), Some(40), 20),
            ("assistant", Some("b.jsonl"), Some(7), 30),
            ("assistant", Some("b.jsonl"), Some(90), 40),
        ],
    );
    assert_eq!((sweeps, samples(&store)), (1, vec!["review:2".to_string()]));
    assert_eq!(
        drift(&store),
        (
            vec![row("assistant", 4, 10, 40, Some("b.jsonl"), Some(7))],
            4
        )
    );
    // An unclean third generation still publishes its drift.
    generation(
        &mut store,
        3,
        SourceMode::Abandon,
        &[("assistant", Some("b.jsonl"), Some(2), 50)],
    );
    // The same events landing straight in a store numbering cursors the
    // other way, the latest first.
    let mut reference = seeded(
        Store::open_in_memory().unwrap(),
        &["c.jsonl", "b.jsonl", "a.jsonl"],
    );
    for event in [
        ("assistant", Some("b.jsonl"), Some(2), 50),
        ("assistant", Some("b.jsonl"), Some(90), 40),
        ("assistant", Some("b.jsonl"), Some(7), 30),
        ("assistant", Some("b.jsonl"), Some(40), 20),
        ("assistant", Some("c.jsonl"), Some(3), 10),
    ] {
        apply_event(&mut reference, event);
    }
    assert_eq!(drift(&store), drift(&reference));
    assert_eq!(
        drift(&store),
        (
            vec![row("assistant", 5, 10, 50, Some("b.jsonl"), Some(2))],
            5
        )
    );
}

#[test]
fn a_deleted_cursor_whose_id_is_handed_out_again_resolves_to_its_new_path() {
    let mut store = seeded(Store::open_in_memory().unwrap(), &PATHS);
    apply_event(&mut store, ("assistant", Some("a.jsonl"), Some(20), 1));
    // c.jsonl's path is now cached, and c.jsonl samples nothing.
    apply_event(&mut store, ("assistant", Some("c.jsonl"), Some(7), 2));
    let reused = cursor(&store, "c.jsonl");
    store
        .conn()
        .execute("DELETE FROM ingest_cursor WHERE id = ?1", [reused])
        .unwrap();
    add_cursors(&mut store, &["0.jsonl"]);
    assert_eq!(
        cursor(&store, "0.jsonl"),
        reused,
        "SQLite hands the id out again"
    );
    // The same offset and time under the new path is a new event, and its
    // path sorts first.
    apply_event(&mut store, ("assistant", Some("0.jsonl"), Some(7), 2));
    assert_eq!(
        drift(&store),
        (vec![row("assistant", 3, 1, 2, Some("0.jsonl"), Some(7))], 3)
    );
    // A live projection refuses to delete a cursor that holds a sample, so
    // a cached sample never loses its path there.
    assert!(
        store
            .conn()
            .execute("DELETE FROM ingest_cursor WHERE id = ?1", [reused])
            .is_err()
    );
}

#[test]
fn a_deleted_sample_cursor_in_a_rebuild_is_read_again_once_its_id_returns() {
    // A private rebuild enforces no foreign keys, so even a sampled cursor
    // can go; its id coming back must drop the cached sample with it.
    let mut store = seeded(
        Store::open_in_memory_for_rebuild().unwrap(),
        &["b.jsonl", "a.jsonl"],
    );
    apply_event(&mut store, ("assistant", Some("a.jsonl"), Some(5), 1));
    let reused = cursor(&store, "a.jsonl");
    store
        .conn()
        .execute("DELETE FROM ingest_cursor WHERE id = ?1", [reused])
        .unwrap();
    add_cursors(&mut store, &["z.jsonl"]);
    assert_eq!(cursor(&store, "z.jsonl"), reused);
    // The stored sample now reads as z.jsonl, which b.jsonl sorts before.
    apply_event(&mut store, ("assistant", Some("b.jsonl"), Some(9), 2));
    assert_eq!(
        drift(&store),
        (vec![row("assistant", 2, 1, 2, Some("b.jsonl"), Some(9))], 2)
    );
}

#[test]
fn a_rolled_back_batch_leaves_no_cached_sample_behind() {
    let mut store = seeded(Store::open_in_memory().unwrap(), &PATHS);
    apply_event(&mut store, ("assistant", Some("b.jsonl"), Some(10), 1));
    // This batch's drift sorts first, but its checkpoint conflicts, so
    // none of it commits.
    let op = fact(&store, ("assistant", Some("a.jsonl"), Some(5), 2));
    let error = store
        .apply(&Batch {
            ops: vec![op],
            checkpoints: vec![checkpoint("b.jsonl", Some("stale"))],
            source: None,
        })
        .unwrap_err();
    assert!(
        matches!(error, StoreError::CheckpointConflict { .. }),
        "{error}"
    );
    assert_eq!(
        drift(&store),
        (
            vec![row("assistant", 1, 1, 1, Some("b.jsonl"), Some(10))],
            1
        )
    );
    // Sorting after the rolled-back event but before the stored one, this
    // event is the sample.
    apply_event(&mut store, ("assistant", Some("a.jsonl"), Some(50), 3));
    assert_eq!(
        drift(&store),
        (
            vec![row("assistant", 2, 1, 3, Some("a.jsonl"), Some(50))],
            2
        )
    );
    // The rolled-back event, delivered again, counts once and wins.
    apply_event(&mut store, ("assistant", Some("a.jsonl"), Some(5), 2));
    apply_event(&mut store, ("assistant", Some("a.jsonl"), Some(5), 2));
    assert_eq!(
        drift(&store),
        (vec![row("assistant", 3, 1, 3, Some("a.jsonl"), Some(5))], 3)
    );
}

/// The cold build's closed-source transaction keeps its cache only once it
/// commits, like [`Store::apply`]: a source failing after another's drift
/// sorted first must leave that sample uncached.
#[test]
fn a_failed_closed_source_transaction_leaves_no_cached_sample_behind() {
    let mut store = seeded(Store::open_in_memory_for_rebuild().unwrap(), &PATHS);
    apply_event(&mut store, ("assistant", Some("b.jsonl"), Some(10), 1));
    // Ledger-only sources: their cursors name no session.
    let closed = |store: &Store, path, generation, events: &[Event]| Batch {
        ops: events.iter().map(|event| fact(store, *event)).collect(),
        checkpoints: vec![],
        source: Some(SourceWrite {
            cursor_id: cursor(store, path),
            session_id: "ledger-only".into(),
            generation,
            first_sequence: 0,
            mode: SourceMode::Activate,
        }),
    };
    let lowers = [("assistant", Some("a.jsonl"), Some(5), 2)];
    // c.jsonl's generation 0 is already active, so the second source fails
    // inside the transaction, after the first one's drift applied.
    let error = store
        .apply_closed_bulk_sources(vec![
            closed(&store, "a.jsonl", 1, &lowers),
            closed(&store, "c.jsonl", 0, &[]),
        ])
        .unwrap_err();
    assert!(matches!(error, StoreError::SourceState { .. }), "{error}");
    assert_eq!(
        drift(&store),
        (
            vec![row("assistant", 1, 1, 1, Some("b.jsonl"), Some(10))],
            1
        )
    );
    // Sorting after the rolled-back sample but before the stored one.
    apply_event(&mut store, ("assistant", Some("a.jsonl"), Some(50), 3));
    assert_eq!(
        drift(&store),
        (
            vec![row("assistant", 2, 1, 3, Some("a.jsonl"), Some(50))],
            2
        )
    );
    // The whole runtime retried cleanly lands the rolled-back sample.
    store
        .apply_closed_bulk_sources(vec![closed(&store, "a.jsonl", 1, &lowers)])
        .unwrap();
    assert_eq!(
        drift(&store),
        (vec![row("assistant", 3, 1, 3, Some("a.jsonl"), Some(5))], 3)
    );
}
