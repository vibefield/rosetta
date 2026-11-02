//! A drift aggregate's sample is the least (cursor path, offset) any of its
//! events offered, so it is the same whatever order sources land in and
//! whatever cursor ids a fresh build, a rebuild or a resume assigned (R1,
//! decision D6).

use rosetta_core::IngestOp;
use rosetta_core::rows::{DriftFact, RuntimeRow};
use rosetta_store::{Batch, CheckpointUpdate, Store};
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

/// A store whose cursors were created in `cursor_order` (a rebuild or a
/// resume numbers them differently from the fresh build).
fn store(cursor_order: &[&str]) -> Store {
    let mut store = Store::open_in_memory().unwrap();
    store
        .apply(&Batch {
            ops: vec![IngestOp::Runtime(runtime())],
            ..Batch::default()
        })
        .unwrap();
    for path in cursor_order {
        store
            .apply(&Batch {
                checkpoints: vec![CheckpointUpdate {
                    runtime_id: "rt1".into(),
                    stream: "transcript".into(),
                    path: (*path).into(),
                    session_id: None,
                    expected: None,
                    next: "fresh".into(),
                    updated_at: None,
                }],
                ..Batch::default()
            })
            .unwrap();
    }
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

/// One drift event: `(path, offset, at)`, a missing path meaning a fact
/// with no sample cursor.
type Event = (Option<&'static str>, Option<i64>, i64);

fn fact(store: &Store, kind: &str, (path, offset, at): Event) -> IngestOp {
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

/// Every aggregate with its sample cursor mapped to the cursor's path.
fn drift(store: &Store) -> Vec<Vec<Value>> {
    let mut statement = store
        .conn()
        .prepare(
            "SELECT d.record_kind, d.count, d.first_seen, d.last_seen, c.path, d.sample_offset
             FROM drift_agg d LEFT JOIN ingest_cursor c ON c.id = d.sample_cursor
             ORDER BY d.record_kind",
        )
        .unwrap();
    statement
        .query_map([], |row| {
            (0..6)
                .map(|index| row.get::<_, Value>(index))
                .collect::<Result<Vec<_>, _>>()
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

/// Each aggregate's events: cursor-backed samples across three files, a
/// cursor-less sample, and one aggregate no event gives a location.
fn events() -> Vec<(&'static str, Event)> {
    vec![
        ("assistant", (Some("b.jsonl"), Some(10), 300)),
        ("assistant", (Some("a.jsonl"), Some(50), 100)),
        ("assistant", (None, Some(1), 50)),
        ("assistant", (Some("c.jsonl"), None, 400)),
        ("assistant", (Some("a.jsonl"), Some(20), 200)),
        ("user", (None, Some(9), 10)),
        ("user", (None, Some(4), 20)),
        ("user", (Some("c.jsonl"), None, 30)),
        ("system", (None, None, 5)),
        ("system", (None, None, 6)),
    ]
}

fn project(cursor_order: &[&str], order: &[usize]) -> Vec<Vec<Value>> {
    let mut store = store(cursor_order);
    let events = events();
    for &index in order {
        let (kind, event) = events[index];
        let op = fact(&store, kind, event);
        store
            .apply(&Batch {
                ops: vec![op],
                ..Batch::default()
            })
            .unwrap();
    }
    drift(&store)
}

/// All permutations of `0..n` (Heap's algorithm, iterative).
fn permutations(n: usize) -> Vec<Vec<usize>> {
    let mut items: Vec<usize> = (0..n).collect();
    let mut counters = vec![0; n];
    let mut out = vec![items.clone()];
    let mut index = 0;
    while index < n {
        if counters[index] < index {
            if index % 2 == 0 {
                items.swap(0, index);
            } else {
                items.swap(counters[index], index);
            }
            out.push(items.clone());
            counters[index] += 1;
            index = 0;
        } else {
            counters[index] = 0;
            index += 1;
        }
    }
    out
}

#[test]
fn the_sample_is_the_least_path_and_offset_in_every_order() {
    let expected = vec![
        vec![
            Value::Text("assistant".into()),
            Value::Integer(5),
            Value::Integer(50),
            Value::Integer(400),
            Value::Text("a.jsonl".into()),
            Value::Integer(20),
        ],
        vec![
            Value::Text("system".into()),
            Value::Integer(2),
            Value::Integer(5),
            Value::Integer(6),
            Value::Null,
            Value::Null,
        ],
        vec![
            Value::Text("user".into()),
            Value::Integer(3),
            Value::Integer(10),
            Value::Integer(30),
            Value::Text("c.jsonl".into()),
            Value::Null,
        ],
    ];
    let count = events().len();
    // Every order of the five assistant events (the aggregate whose sample
    // is contested), the other aggregates' events in both directions.
    let tail: Vec<usize> = (5..count).collect();
    for order in permutations(5) {
        let mut forward = order.clone();
        forward.extend(&tail);
        assert_eq!(project(&PATHS, &forward), expected, "order {forward:?}");
        let mut backward = order;
        backward.extend(tail.iter().rev());
        assert_eq!(project(&PATHS, &backward), expected, "order {backward:?}");
    }
}

#[test]
fn a_fresh_build_and_a_rebuild_numbering_cursors_differently_agree() {
    let forward: Vec<usize> = (0..events().len()).collect();
    let reversed: Vec<usize> = forward.iter().rev().copied().collect();
    let fresh = project(&PATHS, &forward);
    // The rebuild created its cursors in another order and replays the
    // sources in another order too.
    let rebuilt = project(&["c.jsonl", "a.jsonl", "b.jsonl"], &reversed);
    assert_eq!(fresh, rebuilt);
    // The cursor ids really differ between the two projections.
    assert_ne!(
        cursor(&store(&PATHS), "a.jsonl"),
        cursor(&store(&["c.jsonl", "a.jsonl", "b.jsonl"]), "a.jsonl")
    );
}

#[test]
fn a_repeated_event_counts_once_and_moves_no_sample() {
    let mut store = store(&PATHS);
    for event in [
        (Some("b.jsonl"), Some(10), 300),
        (Some("b.jsonl"), Some(10), 300),
        (Some("a.jsonl"), Some(70), 100),
        (Some("a.jsonl"), Some(70), 100),
    ] {
        let op = fact(&store, "assistant", event);
        store
            .apply(&Batch {
                ops: vec![op],
                ..Batch::default()
            })
            .unwrap();
    }
    assert_eq!(
        drift(&store),
        vec![vec![
            Value::Text("assistant".into()),
            Value::Integer(2),
            Value::Integer(100),
            Value::Integer(300),
            Value::Text("a.jsonl".into()),
            Value::Integer(70),
        ]]
    );
}
