//! T6 store tests: a closed source applied in bounded transactions — Stage
//! slices of its immediate ops, resolved-row slices of the session it
//! completes, then its terminal — writes exactly what applying it whole
//! does (test 1); a usage displacement split across slices converges like
//! the whole source (test 4); and one admission transaction equals the
//! three steps it replaces (test 7).

use std::collections::BTreeMap;

use rosetta_core::rows::StreamState;
use rosetta_core::{EvidenceOrder, IngestOp, MessageRow, SessionRow, SourceIdentity};
use rosetta_store::{
    Batch, CheckpointUpdate, ColdReconciliation, ResolvedSession, SourceMode, SourceWrite, Store,
    StreamedSource, WithheldFacts,
};
use rusqlite::types::Value;
use serde_json::json;

const PATH: &str = "a.jsonl";

fn session(id: &str, started_at: i64) -> SessionRow {
    serde_json::from_value(json!({
        "id": id, "native_session_id": id, "runtime_id": "rt", "root_session_id": id,
        "role": "main", "started_at": started_at, "updated_at": started_at,
        "stream_state": "pending"
    }))
    .unwrap()
}

/// A cold store holding the runtime, sessions `root` (started first) and
/// `fork`, and fresh cursors for `a.jsonl` and `b.jsonl`.
fn cold_store() -> Store {
    let mut store = Store::open_in_memory_for_rebuild().unwrap();
    store.use_precomputed_bulk_ordinals().unwrap();
    let ops = vec![
        IngestOp::Runtime(
            serde_json::from_value(json!({
                "id": "rt", "kind": "claude-code", "data_root": "/agent",
                "enabled": true, "discovered_at": 1
            }))
            .unwrap(),
        ),
        IngestOp::SessionMeta(session("root", 1)),
        IngestOp::SessionMeta(session("fork", 5)),
    ];
    store
        .apply(&Batch {
            ops,
            checkpoints: ["a.jsonl", "b.jsonl"]
                .into_iter()
                .map(|path| CheckpointUpdate {
                    runtime_id: "rt".into(),
                    stream: "transcript".into(),
                    path: path.into(),
                    session_id: Some("root".into()),
                    expected: None,
                    next: format!("fresh {path}"),
                    updated_at: None,
                })
                .collect(),
            source: None,
        })
        .unwrap();
    store
}

fn cursor(store: &Store, path: &str) -> i64 {
    store
        .runtime_cursor_ids("rt")
        .unwrap()
        .into_iter()
        .find(|(_, candidate)| candidate == path)
        .unwrap()
        .0
}

/// Seeded xorshift: the property cases are reproducible.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound.max(1)
    }
}

fn message(value: serde_json::Value) -> Vec<IngestOp> {
    let row: MessageRow = serde_json::from_value(value).unwrap();
    IngestOp::project_message(row)
}

fn sample(session_id: &str, key: &str, identity: Option<&str>, output: i64) -> IngestOp {
    IngestOp::SessionUsage(
        serde_json::from_value(json!({
            "session_id": session_id, "key": key, "ts": 40, "model": "model-x",
            "tokens": {"input": 2, "output": output}, "identity": identity
        }))
        .unwrap(),
    )
}

/// One generated terminal source of session `root`: prose (some large
/// enough to become assets), replies with response usage (identity-bearing
/// included), tool openers with lifecycle updates and file edits, usage
/// samples — some on `fork`, sharing identities with `root`'s, so owners are
/// displaced — and drift. Ordinals are final, as the preparer ranks them.
fn generated(seed: u64) -> Vec<IngestOp> {
    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let mut ops = Vec::new();
    let mut ordinal = 0_i64;
    let turns = 6 + rng.below(30);
    for turn in 0..turns {
        let offset = i64::try_from(turn).unwrap() * 10;
        match rng.below(6) {
            0 => {
                let len = if rng.below(8) == 0 {
                    40_000
                } else {
                    10 + rng.below(3_000)
                };
                ops.extend(message(json!({
                    "timeline_id": format!("u{turn}"), "session_id": "root",
                    "ordinal": ordinal, "kind": "user", "ts": 10 + turn,
                    "content": "x".repeat(len as usize), "attachments": [], "citations": [],
                    "src_offset": offset
                })));
                ordinal += 1;
            }
            1 => {
                ops.extend(message(json!({
                    "timeline_id": format!("r{turn}"), "session_id": "root",
                    "ordinal": ordinal, "kind": "agent", "ts": 10 + turn,
                    "content": format!("reply {turn}"), "attachments": [], "citations": [],
                    "src_offset": offset
                })));
                ordinal += 1;
                let identity = (rng.below(2) == 0).then(|| format!("resp-{}", turn % 4));
                ops.push(IngestOp::Usage {
                    session_id: "root".into(),
                    response: format!("r{turn}"),
                    ts: 11 + i64::try_from(turn).unwrap(),
                    model: Some("model-x".into()),
                    tokens: serde_json::from_value(json!({"input": 3, "output": turn})).unwrap(),
                    identity,
                });
            }
            2 => {
                let key = format!("k{turn}");
                let dated = rng.below(4) != 0;
                ops.extend(message(json!({
                    "timeline_id": format!("t{turn}"), "session_id": "root",
                    "ordinal": ordinal, "kind": "tool_use",
                    "ts": if dated { Some(10 + turn) } else { None },
                    "attachments": [], "citations": [], "src_offset": offset,
                    "tool": {
                        "correlation": {"key": key, "derivation": "native_id"},
                        "native": {"id": key, "name": "Bash"},
                        "status": {"value": "pending", "basis": "request_observed"},
                        "action": {"type": "shell",
                            "command": {"type": "inline", "text": "y".repeat(rng.below(2_000) as usize + 1)},
                            "source_truncated": false, "background": false}
                    }
                })));
                ordinal += 1;
                if rng.below(3) != 0 {
                    ops.push(IngestOp::ToolUpdate {
                        session_id: "root".into(),
                        correlation_key: key.clone(),
                        evidence: EvidenceOrder {
                            source_identity: SourceIdentity(PATH.into()),
                            generation: 1,
                            record_offset: turn * 10 + 5,
                            mutation_index: 0,
                        },
                        patch: Box::new(
                            serde_json::from_value(json!({
                                "status": {"value": "succeeded", "basis": "native_terminal_record"}
                            }))
                            .unwrap(),
                        ),
                        at: Some(20 + i64::try_from(turn).unwrap()),
                    });
                }
                if rng.below(3) == 0 {
                    ops.push(IngestOp::FileEdit(
                        serde_json::from_value(json!({
                            "session_id": "root", "message_timeline_id": format!("t{turn}"),
                            "path": format!("/src/f{turn}.rs"), "kind": "edit",
                            "lines_added": 2, "lines_removed": 1, "at": 25 + turn
                        }))
                        .unwrap(),
                    ));
                }
            }
            3 => {
                let target = if rng.below(2) == 0 { "fork" } else { "root" };
                let identity = format!("sample-{}", rng.below(3));
                ops.push(sample(target, &format!("s{turn}"), Some(&identity), 7));
            }
            4 => ops.push(IngestOp::Drift(
                serde_json::from_value(json!({
                    "runtime_id": "rt", "agent_version": "1", "stream": "transcript",
                    "record_kind": "raw_record", "error_code": format!("e{}", turn % 3),
                    "shape_hash": "", "at": null, "sample_cursor": null,
                    "sample_offset": offset
                }))
                .unwrap(),
            )),
            _ => ops.push(sample("root", &format!("plain{turn}"), None, 1)),
        }
    }
    ops
}

/// Stamp every row and drift sample with `cursor_id`, as the writer does.
fn stamp(ops: &mut [IngestOp], cursor_id: i64) {
    for op in ops {
        match op {
            IngestOp::Message(row) => row.provenance.src_cursor = Some(cursor_id),
            IngestOp::Drift(fact) if fact.sample_offset.is_some() => {
                fact.sample_cursor = Some(cursor_id);
            }
            _ => {}
        }
    }
}

fn write(cursor_id: i64, mode: SourceMode, first_sequence: u64) -> SourceWrite {
    SourceWrite {
        cursor_id,
        session_id: "root".into(),
        generation: 1,
        first_sequence,
        mode,
    }
}

fn terminal(cursor_id: i64, ops: Vec<IngestOp>, first_sequence: u64, mode: SourceMode) -> Batch {
    Batch {
        ops,
        checkpoints: vec![CheckpointUpdate {
            runtime_id: "rt".into(),
            stream: "transcript".into(),
            path: PATH.into(),
            session_id: Some("root".into()),
            expected: Some(format!("fresh {PATH}")),
            next: format!("final {PATH}"),
            updated_at: None,
        }],
        source: Some(write(cursor_id, mode, first_sequence)),
    }
}

/// How one source is applied: whole, or cut at `cuts` (op indices, each
/// slice before the last a Stage slice) with resolved-row slices of at most
/// each of `resolved_slices` bytes before the terminal.
struct Plan {
    withheld: bool,
    cuts: Vec<usize>,
    resolved_slices: Vec<u64>,
}

/// Apply `ops` as `plan` says into a fresh store, fold, and dump.
fn apply(ops: &[IngestOp], plan: &Plan) -> (BTreeMap<String, Vec<String>>, Store) {
    let mut store = cold_store();
    let cursor_id = cursor(&store, PATH);
    let mut ops = ops.to_vec();
    stamp(&mut ops, cursor_id);
    let (kept, mut resolved) = if plan.withheld {
        let (kept, facts) = WithheldFacts::withhold(PATH, 1, ops);
        let mut session = ResolvedSession::resolve(vec![facts]).unwrap();
        session.bind_cursors(|_| Some(cursor_id)).unwrap();
        (kept, vec![session])
    } else {
        (ops, Vec::new())
    };
    let mut state = ColdReconciliation::default();
    let mut slices = Vec::new();
    let mut rest = kept;
    let mut taken = 0;
    for cut in &plan.cuts {
        let cut = (*cut).clamp(taken, taken + rest.len());
        let tail = rest.split_off(cut - taken);
        slices.push((taken, std::mem::replace(&mut rest, tail)));
        taken = cut;
    }
    for (first, slice) in slices {
        store
            .apply_streamed_slice(
                &mut state,
                StreamedSource {
                    batch: Batch {
                        ops: slice,
                        checkpoints: Vec::new(),
                        source: Some(write(cursor_id, SourceMode::Stage, first as u64)),
                    },
                    withheld: plan.withheld,
                    resolved: Vec::new(),
                    released: Vec::new(),
                },
            )
            .unwrap();
        // A Stage slice moves no checkpoint and no readiness.
        assert_eq!(stream_state(&store, "root"), "pending");
    }
    for max in &plan.resolved_slices {
        if let Some(session) = resolved.first_mut()
            && session.rows_left()
        {
            store.apply_resolved_slice(session, *max).unwrap();
        }
    }
    let first = taken;
    store
        .apply_streamed_sources(
            &mut state,
            vec![StreamedSource {
                batch: terminal(cursor_id, rest, first as u64, SourceMode::Activate),
                withheld: plan.withheld,
                resolved,
                released: Vec::new(),
            }],
        )
        .unwrap();
    assert_eq!(
        state.staged_slices(),
        0,
        "every slice merged at its terminal"
    );
    store.fold_cold_reconciliation(state).unwrap();
    (dump(&store), store)
}

fn stream_state(store: &Store, session: &str) -> String {
    store
        .conn()
        .query_row(
            "SELECT stream_state FROM session WHERE id = ?1",
            [session],
            |row| row.get(0),
        )
        .unwrap()
}

/// Every canonical table, surrogate message ids mapped to timeline ids
/// (slices insert rows in another order than the whole source).
fn dump(store: &Store) -> BTreeMap<String, Vec<String>> {
    let conn = store.conn();
    let messages: BTreeMap<i64, String> = conn
        .prepare("SELECT id, timeline_id FROM message")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let tables: Vec<String> = conn
        .prepare(
            "SELECT name FROM sqlite_master
             WHERE type = 'table' AND name NOT LIKE 'sqlite_%' AND name != 'source_audit'
             ORDER BY name",
        )
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let mut out = BTreeMap::new();
    for table in tables {
        let columns: Vec<String> = conn
            .prepare(&format!("PRAGMA table_info(\"{table}\")"))
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
            .into_iter()
            .filter(|column| {
                !(table == "message" && column == "id")
                    && !(table == "projection_state" && column == "projection_epoch")
            })
            .collect();
        let select = columns
            .iter()
            .map(|column| format!("\"{column}\""))
            .collect::<Vec<_>>()
            .join(",");
        let mut rows: Vec<String> = conn
            .prepare(&format!("SELECT {select} FROM \"{table}\""))
            .unwrap()
            .query_map([], |row| {
                let mut fields = Vec::with_capacity(columns.len());
                for (index, column) in columns.iter().enumerate() {
                    let value: Value = row.get(index)?;
                    let text = match (&value, column.as_str()) {
                        (Value::Integer(id), "message_id") => messages
                            .get(id)
                            .cloned()
                            .unwrap_or_else(|| format!("<dangling message {id}>")),
                        _ => format!("{value:?}"),
                    };
                    fields.push(format!("{column}={text}"));
                }
                Ok(fields.join(" | "))
            })
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        rows.sort();
        out.insert(table, rows);
    }
    out
}

fn assert_same(
    label: &str,
    expected: &BTreeMap<String, Vec<String>>,
    actual: &BTreeMap<String, Vec<String>>,
) {
    for (table, rows) in expected {
        let other = &actual[table];
        if rows != other {
            let missing: Vec<_> = rows
                .iter()
                .filter(|row| !other.contains(row))
                .take(3)
                .collect();
            let extra: Vec<_> = other
                .iter()
                .filter(|row| !rows.contains(row))
                .take(3)
                .collect();
            panic!(
                "{label}: table {table} differs\nwhole only: {missing:#?}\nsliced only: {extra:#?}"
            );
        }
    }
}

/// T6 test 1: for random cut points into 1–8 Stage slices, plus 1–4
/// resolved-row slices, the sliced apply equals the whole source's on every
/// canonical table — drift, `session.updated_at`, the cursor row and the
/// folded tool rows included — withheld or not.
#[test]
fn sliced_sources_equal_the_whole_source() {
    let mut cases = 0;
    for seed in 0..100_u64 {
        let ops = generated(seed);
        let mut rng = Rng(seed.wrapping_add(17) | 1);
        for withheld in [false, true] {
            let whole = Plan {
                withheld,
                cuts: Vec::new(),
                resolved_slices: Vec::new(),
            };
            let (expected, _) = apply(&ops, &whole);
            let kept = if withheld {
                WithheldFacts::withhold(PATH, 1, ops.clone()).0.len()
            } else {
                ops.len()
            };
            let stages = 1 + rng.below(8) as usize;
            let mut cuts: Vec<usize> = (0..stages)
                .map(|_| rng.below(kept as u64 + 1) as usize)
                .collect();
            cuts.sort_unstable();
            let resolved_slices = (0..1 + rng.below(4))
                .map(|_| 1 + rng.below(4_000))
                .collect();
            let sliced = Plan {
                withheld,
                cuts,
                resolved_slices,
            };
            let (actual, store) = apply(&ops, &sliced);
            assert_same(
                &format!("seed {seed} withheld {withheld}"),
                &expected,
                &actual,
            );
            let unstamped: i64 = store
                .conn()
                .query_row(
                    "SELECT COUNT(*) FROM message WHERE src_cursor IS NULL",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(unstamped, 0, "every row carries its provenance");
            cases += 1;
        }
    }
    assert_eq!(cases, 200);
}

/// T6 test 4 (T1 delta): a usage displacement whose prior owner was written
/// in slice 1 and displaced in slice 3 ends exactly as the same source
/// applied whole: `fork`'s sample is claimed first and displaced by the
/// earlier-started `root`'s copy two slices later.
#[test]
fn a_displacement_across_slices_equals_the_whole_source() {
    let mut ops = vec![sample("fork", "fork-copy", Some("shared-request"), 9)];
    for turn in 0..4 {
        ops.extend(message(json!({
            "timeline_id": format!("filler{turn}"), "session_id": "root",
            "ordinal": turn, "kind": "agent", "ts": 30 + turn,
            "content": "filler", "attachments": [], "citations": [], "src_offset": turn * 10
        })));
    }
    ops.push(sample("root", "root-copy", Some("shared-request"), 9));
    for withheld in [false, true] {
        let (expected, whole) = apply(
            &ops,
            &Plan {
                withheld,
                cuts: Vec::new(),
                resolved_slices: Vec::new(),
            },
        );
        let (actual, sliced) = apply(
            &ops,
            &Plan {
                withheld,
                cuts: vec![1, 3],
                resolved_slices: Vec::new(),
            },
        );
        assert_same(&format!("withheld {withheld}"), &expected, &actual);
        for store in [&whole, &sliced] {
            assert_eq!(store.usage_owner("shared-request"), Some("root"));
        }
        let fork_samples: i64 = sliced
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM session_usage WHERE session_id = 'fork'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(fork_samples, 0, "the displaced copy is gone");
    }
}

fn two_sources(store: &mut Store, state: &mut ColdReconciliation) {
    // b.jsonl ranks after a.jsonl, but its rows took the lower ordinals:
    // the renumber puts them in path order.
    for (path, ordinals) in [("b.jsonl", [0, 1]), ("a.jsonl", [2, 3])] {
        let cursor_id = cursor(store, path);
        let mut ops = Vec::new();
        for ordinal in ordinals {
            ops.extend(message(json!({
                "timeline_id": format!("{path}/{ordinal}"), "session_id": "root",
                "ordinal": ordinal, "kind": "agent", "ts": 30 + ordinal,
                "content": "row", "attachments": [], "citations": [],
                "src_offset": ordinal * 10
            })));
        }
        let key = format!("call-{path}");
        ops.extend(message(json!({
            "timeline_id": format!("{path}/tool"), "session_id": "root",
            "ordinal": ordinals[1] + 10, "kind": "tool_use", "ts": 50,
            "attachments": [], "citations": [], "src_offset": 99,
            "tool": {
                "correlation": {"key": key, "derivation": "native_id"},
                "native": {"id": key, "name": "Bash"},
                "status": {"value": "pending", "basis": "request_observed"},
                "action": {"type": "shell", "command": {"type": "inline", "text": "true"},
                    "source_truncated": false, "background": false}
            }
        })));
        stamp(&mut ops, cursor_id);
        let mut batch = terminal(cursor_id, ops, 0, SourceMode::Activate);
        batch.checkpoints[0].path = path.into();
        batch.checkpoints[0].expected = Some(format!("fresh {path}"));
        store
            .apply_streamed_sources(state, vec![batch.into()])
            .unwrap();
    }
}

/// T6 test 7: `admit_cold_units` — guard, fold, renumber and flip in one
/// transaction — leaves the store exactly as the three separate steps, and
/// reports as changed exactly the sessions whose state moved. (Mutation:
/// returning every requested id fails on the session already Indexed.)
#[test]
fn one_admission_transaction_equals_the_separate_steps() {
    let states = vec![
        ("root".to_string(), StreamState::Indexed),
        ("fork".to_string(), StreamState::Indexed),
    ];
    let renumber = vec!["root".to_string()];

    let mut separate = cold_store();
    separate
        .set_session_stream_states(&[("fork".into(), StreamState::Indexed)])
        .unwrap();
    let mut state = ColdReconciliation::default();
    two_sources(&mut separate, &mut state);
    separate.fold_cold_reconciliation(state).unwrap();
    separate
        .renumber_sessions_by_source_order(&renumber)
        .unwrap();
    separate.set_session_stream_states(&states).unwrap();

    let mut admitted = cold_store();
    admitted
        .set_session_stream_states(&[("fork".into(), StreamState::Indexed)])
        .unwrap();
    let mut state = ColdReconciliation::default();
    two_sources(&mut admitted, &mut state);
    let admission = admitted
        .admit_cold_units(state, &renumber, &states)
        .unwrap();

    assert_same("admission", &dump(&separate), &dump(&admitted));
    assert_eq!(admission.changed, ["root".to_string()]);
    assert!(admission.renumber_rows > 0, "the renumber moved rows");
    let ordinals: Vec<(String, i64)> = admitted
        .conn()
        .prepare(
            "SELECT timeline_id, ordinal FROM message WHERE type != 'tool_use' ORDER BY ordinal",
        )
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(ordinals[0].0, "a.jsonl/2", "path order after the renumber");
}

/// A Stage slice of another generation than the one staged is refused, and
/// so is a Stage call carrying a checkpoint.
#[test]
fn a_slice_must_follow_its_staged_generation() {
    let mut store = cold_store();
    let cursor_id = cursor(&store, PATH);
    let mut state = ColdReconciliation::default();
    let mut ops = generated(3);
    stamp(&mut ops, cursor_id);
    let slice = |generation: u64, ops: Vec<IngestOp>| StreamedSource {
        batch: Batch {
            ops,
            checkpoints: Vec::new(),
            source: Some(SourceWrite {
                generation,
                ..write(cursor_id, SourceMode::Stage, 0)
            }),
        },
        withheld: false,
        resolved: Vec::new(),
        released: Vec::new(),
    };
    store
        .apply_streamed_slice(&mut state, slice(1, ops[..2].to_vec()))
        .unwrap();
    let error = store
        .apply_streamed_slice(&mut state, slice(2, ops[2..3].to_vec()))
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("disagrees with its staged generation"),
        "{error}"
    );
    let mut with_checkpoint = slice(1, Vec::new());
    with_checkpoint.batch.checkpoints =
        terminal(cursor_id, Vec::new(), 0, SourceMode::Stage).checkpoints;
    assert!(
        store
            .apply_streamed_slice(&mut state, with_checkpoint)
            .is_err()
    );
}
