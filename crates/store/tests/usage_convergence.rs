//! Cross-file usage ownership converges whatever order sessions' records
//! arrive in (ARCHITECTURE §4.1.3, counting rule 2; R1 decision D6). The
//! same per-session record sequences, interleaved every way, leave one
//! identical projection, one owner per identity, and every non-owner copy
//! exactly as a copy whose usage was dropped on arrival.

use std::collections::BTreeSet;

use rosetta_core::rows::{
    MessageKind, MessageRow, ProjectRow, RuntimeRow, SessionRole, SessionRow, SessionUsageRow,
    StreamState, TokenSnapshot,
};
use rosetta_core::{IngestOp, ModelUsageTotal, SessionUsageTotals};
use rosetta_store::{ApplyStats, Batch, CheckpointUpdate, SourceMode, SourceWrite, Store};
use rusqlite::types::Value;

const T: i64 = 1_788_264_000_000;
const OPUS: &str = "claude-opus-4-8";
const SONNET: &str = "claude-sonnet-4-5";
const HAIKU: &str = "claude-haiku-4-5";

const ORIGIN: &str = "origin";
const FORK: &str = "fork";
const SUB: &str = "sub";
const SESSIONS: [(&str, i64); 3] = [(ORIGIN, 10_000), (FORK, 20_000), (SUB, 30_000)];

const ID_A: &str = "msg_A:req_A";
const ID_B: &str = "msg_B:req_B";
const ID_C: &str = "msg_C:req_C";
const ID_D: &str = "msg_D:req_D";
const ID_E: &str = "msg_E:req_E";
const IDENTITIES: [&str; 5] = [ID_A, ID_B, ID_C, ID_D, ID_E];

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

fn session(id: &str, started_at: i64, stream_state: StreamState) -> SessionRow {
    SessionRow {
        id: id.into(),
        native_session_id: format!("native-{id}"),
        runtime_id: "rt1".into(),
        project_id: Some("p1".into()),
        cwd: Some("/w/rosetta".into()),
        parent_session_id: (id != ORIGIN).then(|| ORIGIN.into()),
        delegation: None,
        root_session_id: ORIGIN.into(),
        role: if id == ORIGIN {
            SessionRole::Main
        } else {
            SessionRole::Subagent
        },
        title: None,
        summary: None,
        started_at,
        updated_at: started_at,
        activity: None,
        stream_state,
    }
}

fn metadata(stream_state: StreamState) -> Vec<IngestOp> {
    let mut ops = vec![
        IngestOp::Runtime(runtime()),
        IngestOp::Project(ProjectRow {
            id: "p1".into(),
            name: "rosetta".into(),
            root_paths: vec!["/w/rosetta".into()],
            git_repo: None,
        }),
    ];
    for (id, started_at) in SESSIONS {
        ops.push(IngestOp::SessionMeta(session(id, started_at, stream_state)));
    }
    ops
}

fn io(input: i64, output: i64) -> TokenSnapshot {
    TokenSnapshot {
        input: Some(input),
        output: Some(output),
        ..TokenSnapshot::default()
    }
}

fn row(
    session_id: &str,
    name: &str,
    ordinal: i64,
    ts: Option<i64>,
    model: Option<&str>,
) -> IngestOp {
    let row = MessageRow {
        timeline_id: format!("{session_id}-{name}"),
        session_id: session_id.into(),
        ordinal,
        kind: MessageKind::Agent,
        subtype: None,
        uuid: None,
        parent_uuid: None,
        ts,
        model: model.map(Into::into),
        effort: None,
        content: Some(format!("reply {name}")),
        tool: None,
        attachments: Vec::new(),
        citations: Vec::new(),
        group: None,
        response: None,
        block_index: None,
        branch_tool_id: None,
        // Copied rows carry no inline usage: it travels as its own op.
        usage: Some(TokenSnapshot::default()),
        agent_version: None,
        src_cursor: None,
        src_offset: Some(ordinal),
    };
    IngestOp::project_message(row).pop().unwrap()
}

fn usage(
    session_id: &str,
    name: &str,
    ts: i64,
    model: Option<&str>,
    tokens: TokenSnapshot,
    identity: &str,
) -> IngestOp {
    IngestOp::Usage {
        session_id: session_id.into(),
        response: format!("{session_id}-{name}"),
        ts,
        model: model.map(Into::into),
        tokens,
        identity: Some(identity.into()),
    }
}

fn sample(
    session_id: &str,
    key: &str,
    model: &str,
    tokens: TokenSnapshot,
    identity: Option<&str>,
) -> IngestOp {
    IngestOp::SessionUsage(SessionUsageRow {
        session_id: session_id.into(),
        key: key.into(),
        ts: T,
        model: Some(model.into()),
        tokens,
        identity: identity.map(Into::into),
    })
}

/// One session's records in its own order. Copies carry the same
/// identities as the origin (scaled tokens, so a wrong owner shows), and
/// every usage time stays at or below the session's latest row time.
fn session_ops(session_id: &str, scale: i64) -> Vec<IngestOp> {
    let s = session_id;
    let mut ops = vec![
        // A: a response sample, then the fact moves onto the row and the
        // sample is retracted, plus two iteration samples.
        row(s, "a", 0, Some(T), Some(OPUS)),
        sample(s, "response:msg_A", OPUS, io(100 * scale, 5), Some(ID_A)),
        usage(s, "a", T, Some(OPUS), io(120 * scale, 7), ID_A),
        sample(
            s,
            "response:msg_A",
            OPUS,
            TokenSnapshot::default(),
            Some(ID_A),
        ),
        sample(
            s,
            "response:msg_A:iteration:1",
            HAIKU,
            io(30 * scale, 3),
            Some(ID_A),
        ),
        sample(
            s,
            "response:msg_A:iteration:2",
            OPUS,
            io(40 * scale, 4),
            Some(ID_A),
        ),
        // B: a row with no time; the usage fills it in, then a correction
        // of the same response replaces it on the same row.
        row(s, "b", 1, None, Some(OPUS)),
        usage(s, "b", T + 500, Some(OPUS), io(50 * scale, 5), ID_B),
        usage(s, "b", T + 700, Some(OPUS), io(55 * scale, 6), ID_B),
        // C: a model-less row; the usage fills the model in, and a
        // correction keeps it.
        row(s, "c", 2, Some(T + 1_000), None),
        usage(s, "c", T + 1_000, Some(SONNET), io(60 * scale, 6), ID_C),
        usage(s, "c", T + 1_000, Some(SONNET), io(65 * scale, 7), ID_C),
        // D: the usage fills the model, then a later record re-upserts the
        // row with another time and model while keeping its usage.
        row(s, "d", 3, Some(T + 1_500), None),
        usage(s, "d", T + 1_500, Some(OPUS), io(70 * scale, 7), ID_D),
        row(s, "d", 3, Some(T + 1_600), Some(SONNET)),
        // Usage no other session copies.
        usage(
            s,
            "z",
            T + 2_000,
            Some(OPUS),
            io(9 * scale, 1),
            &format!("own:{s}"),
        ),
        sample(s, &format!("plain:{s}"), OPUS, io(scale, 1), None),
    ];
    // The latest row: every usage time above stays at or below it.
    ops.insert(ops.len() - 2, row(s, "z", 4, Some(T + 2_000), Some(OPUS)));
    if s != ORIGIN {
        // E: a response only the two copies carry; the fork owns it.
        ops.push(row(s, "e", 5, Some(T + 1_800), Some(OPUS)));
        ops.push(usage(
            s,
            "e",
            T + 1_800,
            Some(OPUS),
            io(80 * scale, 8),
            ID_E,
        ));
    } else {
        // The root's cumulative tree totals, reconciled against every
        // member's recorded requests: a displacement moves that difference.
        ops.push(IngestOp::SessionUsageTotals(SessionUsageTotals {
            session_id: s.into(),
            ts: T + 2_000,
            models: vec![ModelUsageTotal {
                model: OPUS.into(),
                input: 5_000,
                output: 400,
                thinking: None,
                cache_read: 0,
                cache_write: 0,
                web_searches: 0,
                estimated_pico_usd: None,
            }],
        }));
    }
    ops
}

fn per_session() -> Vec<Vec<IngestOp>> {
    vec![
        session_ops(ORIGIN, 1),
        session_ops(FORK, 2),
        session_ops(SUB, 3),
    ]
}

/// splitmix64: a seeded interleaving source with no dependency.
struct Mix(u64);

impl Mix {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

/// One random interleaving that keeps every session's own order.
fn interleave(lists: &[Vec<IngestOp>], seed: u64) -> Vec<IngestOp> {
    let mut next = vec![0_usize; lists.len()];
    let mut mix = Mix(seed);
    let mut out = Vec::new();
    loop {
        let open: Vec<usize> = (0..lists.len())
            .filter(|&list| next[list] < lists[list].len())
            .collect();
        if open.is_empty() {
            return out;
        }
        let list = open[(mix.next() % open.len() as u64) as usize];
        out.push(lists[list][next[list]].clone());
        next[list] += 1;
    }
}

/// Every whole-session order: each session's records as one block.
fn block_orders(lists: &[Vec<IngestOp>]) -> Vec<Vec<IngestOp>> {
    let orders = [
        [0, 1, 2],
        [0, 2, 1],
        [1, 0, 2],
        [1, 2, 0],
        [2, 0, 1],
        [2, 1, 0],
    ];
    orders
        .iter()
        .map(|order| {
            order
                .iter()
                .flat_map(|&list| lists[list].iter().cloned())
                .collect()
        })
        .collect()
}

fn round_robin(lists: &[Vec<IngestOp>]) -> Vec<IngestOp> {
    let longest = lists.iter().map(Vec::len).max().unwrap_or(0);
    (0..longest)
        .flat_map(|index| {
            lists
                .iter()
                .filter_map(move |list| list.get(index).cloned())
        })
        .collect()
}

/// Apply every record as its own live batch, in order.
fn live(order: &[IngestOp], stream_state: StreamState) -> (Store, ApplyStats) {
    let mut store = Store::open_in_memory().unwrap();
    store
        .apply(&Batch {
            ops: metadata(stream_state),
            ..Batch::default()
        })
        .unwrap();
    let mut stats = ApplyStats::default();
    for op in order {
        let applied = store
            .apply(&Batch {
                ops: vec![op.clone()],
                ..Batch::default()
            })
            .unwrap();
        stats.merge_from(&applied);
    }
    (store, stats)
}

fn rows(store: &Store, sql: &str) -> Vec<Vec<Value>> {
    let mut statement = store.conn().prepare(sql).unwrap();
    let columns = statement.column_count();
    let mut rows: Vec<Vec<Value>> = statement
        .query_map([], |row| {
            (0..columns)
                .map(|index| row.get::<_, Value>(index))
                .collect()
        })
        .unwrap()
        .map(Result::unwrap)
        .collect();
    rows.sort_by(|a, b| format!("{a:?}").cmp(&format!("{b:?}")));
    rows
}

#[derive(Debug, PartialEq)]
struct Dump {
    messages: Vec<Vec<Value>>,
    samples: Vec<Vec<Value>>,
    buckets: Vec<Vec<Value>>,
    totals: Vec<Vec<Value>>,
    updated_at: Vec<Vec<Value>>,
    owners: Vec<Option<String>>,
}

fn dump(store: &Store) -> Dump {
    Dump {
        messages: rows(
            store,
            "SELECT timeline_id, session_id, ordinal, ts, model, in_tokens, out_tokens,
                    cache_r_tokens, cache_w_tokens, think_tokens, cache_w1h_tokens,
                    web_searches, service_tier, route, billed_pusd, usage_scope, requests
             FROM message",
        ),
        samples: rows(store, "SELECT * FROM session_usage"),
        buckets: rows(store, "SELECT * FROM usage_bucket"),
        totals: rows(store, "SELECT * FROM session_usage_total"),
        updated_at: rows(store, "SELECT id, updated_at FROM session"),
        owners: IDENTITIES
            .iter()
            .map(|identity| store.usage_owner(identity).map(str::to_owned))
            .collect(),
    }
}

/// Whether a record survives when every copy's usage is dropped on arrival:
/// everything but identity-bearing usage of a session that does not own it.
fn kept_on_arrival(op: &IngestOp) -> bool {
    let owns = |identity: &str, session: &str| match identity {
        ID_E => session == FORK,
        id if id.starts_with("own:") => true,
        _ => session == ORIGIN,
    };
    match op {
        IngestOp::Usage {
            identity: Some(identity),
            session_id,
            ..
        }
        | IngestOp::SessionUsage(SessionUsageRow {
            identity: Some(identity),
            session_id,
            ..
        }) => owns(identity, session_id),
        _ => true,
    }
}

/// What a copy shows when its usage is dropped on arrival: the same records
/// with every copy's identity-bearing usage left out.
fn dropped_on_arrival() -> Dump {
    let order: Vec<IngestOp> = per_session()
        .into_iter()
        .flatten()
        .filter(kept_on_arrival)
        .collect();
    let (store, _) = live(&order, StreamState::Streaming);
    let expected = dump(&store);
    assert_eq!(
        expected.owners,
        vec![
            Some(ORIGIN.into()),
            Some(ORIGIN.into()),
            Some(ORIGIN.into()),
            Some(ORIGIN.into()),
            Some(FORK.into()),
        ]
    );
    expected
}

#[test]
fn every_interleaving_of_the_same_records_converges_on_one_projection() {
    let lists = per_session();
    let mut orders = block_orders(&lists);
    orders.push(round_robin(&lists));
    orders.extend((0..200).map(|seed| interleave(&lists, seed)));
    let expected = dropped_on_arrival();
    let mut displaced_somewhere = false;
    for (index, order) in orders.iter().enumerate() {
        let (store, stats) = live(order, StreamState::Streaming);
        assert_eq!(dump(&store), expected, "order {index}");
        // Every claim has exactly one outcome, whatever the order.
        let claims = order
            .iter()
            .filter(|op| {
                matches!(
                    op,
                    IngestOp::Usage {
                        identity: Some(_),
                        ..
                    } | IngestOp::SessionUsage(SessionUsageRow {
                        identity: Some(_),
                        ..
                    })
                )
            })
            .count() as u64;
        assert_eq!(
            stats.usage_identities_owned
                + stats.usage_copies_dropped
                + stats.usage_copies_displaced,
            claims,
            "order {index}"
        );
        displaced_somewhere |= stats.usage_copies_displaced > 0;
    }
    assert!(displaced_somewhere, "some order lands a copy first");
}

#[test]
fn the_converged_copies_hold_no_usage_and_their_own_stamps() {
    let expected = dropped_on_arrival();
    let message = |timeline: &str| -> &Vec<Value> {
        expected
            .messages
            .iter()
            .find(|row| row[0] == Value::Text(timeline.into()))
            .unwrap()
    };
    for copy in [FORK, SUB] {
        // Columns: ts, model, then the usage columns.
        let b = message(&format!("{copy}-b"));
        assert_eq!(b[3], Value::Null, "a NULL-time copy stays undated");
        let c = message(&format!("{copy}-c"));
        assert_eq!(c[4], Value::Null, "a model-less copy stays model-less");
        let d = message(&format!("{copy}-d"));
        assert_eq!(
            (&d[3], &d[4]),
            (&Value::Integer(T + 1_600), &Value::Text(SONNET.into())),
            "the re-upserted copy keeps the re-upsert's stamp"
        );
        for name in ["a", "b", "c", "d"] {
            let row = message(&format!("{copy}-{name}"));
            assert!(
                row[5..].iter().all(|value| *value == Value::Null),
                "{row:?}"
            );
        }
    }
    // Only the origin's samples survive, the retracted one blank.
    let keys: BTreeSet<String> = expected
        .samples
        .iter()
        .filter_map(|row| match (&row[0], &row[1]) {
            (Value::Text(session), Value::Text(key)) if !key.starts_with("plain:") => {
                Some(format!("{session}/{key}"))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        keys,
        [
            "origin/response:msg_A",
            "origin/response:msg_A:iteration:1",
            "origin/response:msg_A:iteration:2",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    );
    // The root's totals exceed its tree's recorded requests, so a reported
    // difference row exists and every order must reconcile it alike.
    assert!(
        expected
            .buckets
            .iter()
            .any(|row| row[0] == Value::Text(ORIGIN.into())
                && row[3] == Value::Text("reported".into())),
        "{:?}",
        expected.buckets
    );
    let origin_e = expected
        .messages
        .iter()
        .find(|row| row[0] == Value::Text(format!("{FORK}-e")))
        .unwrap();
    assert_eq!(origin_e[5], Value::Integer(160), "the fork owns E");
}

#[test]
fn a_copy_landing_first_reports_the_sessions_it_displaced() {
    let lists = per_session();
    let copy_first: Vec<IngestOp> = [2, 1, 0]
        .iter()
        .flat_map(|&list| lists[list].iter().cloned())
        .collect();
    let (_, stats) = live(&copy_first, StreamState::Streaming);
    // sub owned A–E first; the fork displaced it, then the origin the fork.
    assert_eq!(
        stats.usage_displaced_sessions,
        [FORK, SUB].into_iter().map(str::to_owned).collect()
    );
    assert_eq!(stats.usage_copies_displaced, 9);
    // Every displaced session was readable, so each displacement counts as
    // visible, and so does each undone row (A–D twice, E once).
    assert_eq!(stats.usage_displaced_visible, 9);
    assert_eq!(stats.usage_displaced_visible_rows, 9);
    assert!(stats.usage_owner_bytes > 0);
    // Pending sessions are not readable yet: nothing visible changed.
    let (_, pending) = live(&copy_first, StreamState::Pending);
    assert_eq!(
        pending.usage_displaced_sessions,
        stats.usage_displaced_sessions
    );
    assert_eq!(
        (
            pending.usage_displaced_visible,
            pending.usage_displaced_visible_rows
        ),
        (0, 0)
    );
    // Origin first: nothing is ever displaced.
    let (_, origin_first) = live(
        &lists.iter().flatten().cloned().collect::<Vec<_>>(),
        StreamState::Streaming,
    );
    assert!(origin_first.usage_displaced_sessions.is_empty());
}

#[test]
fn a_message_re_upsert_between_the_claim_and_the_displacement_survives() {
    // The copy's usage fills the row's model, a later record re-upserts the
    // row with its own stamp, and only then does the origin land. The
    // re-upsert's stamp is what a dropped copy shows, so the displacement
    // nulls the usage and leaves the stamp alone.
    let copy = vec![
        row(FORK, "d", 0, Some(T), None),
        usage(FORK, "d", T, Some(OPUS), io(70, 7), ID_D),
        row(FORK, "d", 0, Some(T + 100), Some(SONNET)),
    ];
    let origin = vec![
        row(ORIGIN, "d", 0, Some(T + 100), Some(OPUS)),
        usage(ORIGIN, "d", T + 100, Some(OPUS), io(70, 7), ID_D),
    ];
    let stamp = |store: &Store| -> (Option<i64>, Option<String>, Option<i64>) {
        store
            .conn()
            .query_row(
                "SELECT ts, model, in_tokens FROM message WHERE timeline_id = 'fork-d'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap()
    };
    let copy_first: Vec<_> = copy.iter().chain(&origin).cloned().collect();
    let (displaced, stats) = live(&copy_first, StreamState::Streaming);
    assert_eq!(stats.usage_copies_displaced, 1);
    assert_eq!(
        stamp(&displaced),
        (Some(T + 100), Some(SONNET.into()), None)
    );
    let origin_first: Vec<_> = origin.iter().chain(&copy).cloned().collect();
    let (dropped, stats) = live(&origin_first, StreamState::Streaming);
    assert_eq!(stats.usage_copies_dropped, 1);
    assert_eq!(dump(&displaced), dump(&dropped));
    // Without the re-upsert the prior stamp comes back: model-less.
    let copy_first: Vec<_> = copy[..2].iter().chain(&origin).cloned().collect();
    let (restored, _) = live(&copy_first, StreamState::Streaming);
    assert_eq!(stamp(&restored), (Some(T), None, None));
}

#[test]
fn only_session_updated_at_keeps_a_displaced_usage_time_above_every_row() {
    // The documented residual: `updated_at` only rises, so a displaced copy
    // whose usage time exceeds every row time keeps that time.
    let late = T + 60_000;
    let copy = vec![
        row(FORK, "a", 0, Some(T), Some(OPUS)),
        usage(FORK, "a", late, Some(OPUS), io(10, 1), ID_A),
    ];
    let origin = vec![
        row(ORIGIN, "a", 0, Some(T), Some(OPUS)),
        usage(ORIGIN, "a", T, Some(OPUS), io(10, 1), ID_A),
    ];
    let (displaced, _) = live(
        &copy.iter().chain(&origin).cloned().collect::<Vec<_>>(),
        StreamState::Streaming,
    );
    let (dropped, _) = live(
        &origin.iter().chain(&copy).cloned().collect::<Vec<_>>(),
        StreamState::Streaming,
    );
    let mut a = dump(&displaced);
    let mut b = dump(&dropped);
    let updated = |dump: &Dump, session: &str| {
        dump.updated_at
            .iter()
            .find(|row| row[0] == Value::Text(session.into()))
            .unwrap()[1]
            .clone()
    };
    assert_eq!(updated(&a, FORK), Value::Integer(late));
    assert_eq!(updated(&b, FORK), Value::Integer(T));
    a.updated_at.clear();
    b.updated_at.clear();
    assert_eq!(a, b, "everything else converges");
}

// --- the cold path: closed sources, per-source folds -----------------------

fn cold_store() -> Store {
    let mut store = Store::open_in_memory_for_rebuild().unwrap();
    store
        .apply(&Batch {
            ops: metadata(StreamState::Pending),
            checkpoints: SESSIONS
                .iter()
                .map(|(session, _)| CheckpointUpdate {
                    runtime_id: "rt1".into(),
                    stream: "transcript".into(),
                    path: format!("{session}.jsonl"),
                    session_id: Some((*session).into()),
                    expected: None,
                    next: "fresh".into(),
                    updated_at: None,
                })
                .collect(),
            source: None,
        })
        .unwrap();
    store
}

fn closed(store: &Store, session: &str, mut ops: Vec<IngestOp>) -> Batch {
    let path = format!("{session}.jsonl");
    let cursor_id = store
        .runtime_cursor_ids("rt1")
        .unwrap()
        .into_iter()
        .find(|(_, candidate)| *candidate == path)
        .unwrap()
        .0;
    for op in &mut ops {
        if let IngestOp::Message(row) = op {
            row.provenance.src_cursor = Some(cursor_id);
        }
    }
    Batch {
        ops,
        checkpoints: vec![CheckpointUpdate {
            runtime_id: "rt1".into(),
            stream: "transcript".into(),
            path,
            session_id: Some(session.into()),
            expected: Some("fresh".into()),
            next: "final".into(),
            updated_at: None,
        }],
        source: Some(SourceWrite {
            cursor_id,
            session_id: session.into(),
            generation: 1,
            first_sequence: 0,
            mode: SourceMode::Activate,
        }),
    }
}

#[test]
fn closed_sources_folded_one_by_one_converge_in_every_source_order() {
    // A cold build applies each closed source, its samples claimed at once,
    // and folds its row usage when its tree is admitted: here right after
    // the source, in every source order.
    let lists = per_session();
    let names = [ORIGIN, FORK, SUB];
    let mut dumps = Vec::new();
    for order in [
        [0, 1, 2],
        [0, 2, 1],
        [1, 0, 2],
        [1, 2, 0],
        [2, 0, 1],
        [2, 1, 0],
    ] {
        let mut store = cold_store();
        for list in order {
            let batch = closed(&store, names[list], lists[list].clone());
            store.apply(&batch).unwrap();
            store.flush_bulk_reconciliation().unwrap();
        }
        store.finish_bulk_load().unwrap();
        dumps.push((order, dump(&store)));
    }
    let (_, reference) = &dumps[0];
    for (order, dump) in &dumps {
        assert_eq!(dump, reference, "source order {order:?}");
    }
    assert_eq!(
        reference.owners,
        vec![
            Some(ORIGIN.into()),
            Some(ORIGIN.into()),
            Some(ORIGIN.into()),
            Some(ORIGIN.into()),
            Some(FORK.into()),
        ]
    );
}

#[test]
fn a_cold_build_equals_dropping_every_copy_on_arrival_however_its_folds_fall() {
    // HEAD's runtime admission folds every source's row usage once at the
    // end; per-tree admission folds after each tree. Either way, in every
    // source order, the build equals one that never saw the copies' usage.
    let lists = per_session();
    let names = [ORIGIN, FORK, SUB];
    let build = |order: [usize; 3], keep: fn(&IngestOp) -> bool, fold_each: bool| {
        let mut store = cold_store();
        for list in order {
            let ops = lists[list].iter().filter(|op| keep(op)).cloned().collect();
            let batch = closed(&store, names[list], ops);
            store.apply(&batch).unwrap();
            if fold_each {
                store.flush_bulk_reconciliation().unwrap();
            }
        }
        store.flush_bulk_reconciliation().unwrap();
        store.finish_bulk_load().unwrap();
        dump(&store)
    };
    let reference = build([0, 1, 2], kept_on_arrival, false);
    for order in [
        [0, 1, 2],
        [0, 2, 1],
        [1, 0, 2],
        [1, 2, 0],
        [2, 0, 1],
        [2, 1, 0],
    ] {
        for fold_each in [false, true] {
            assert_eq!(
                build(order, |_| true, fold_each),
                reference,
                "source order {order:?}, fold after each source: {fold_each}"
            );
        }
    }
}

#[test]
fn a_displaced_owner_that_wrote_nothing_changes_nothing_visible() {
    // The copy's record names a row it never had, so it claims the identity
    // and writes nothing; the origin's displacement then has nothing to undo
    // and nothing for a reader to resync.
    let order = vec![
        usage(FORK, "missing", T, Some(OPUS), io(70, 7), ID_A),
        row(ORIGIN, "a", 0, Some(T), Some(OPUS)),
        usage(ORIGIN, "a", T, Some(OPUS), io(70, 7), ID_A),
    ];
    let (store, stats) = live(&order, StreamState::Streaming);
    assert_eq!(stats.usage_copies_displaced, 1);
    assert_eq!(
        (
            stats.usage_displaced_visible,
            stats.usage_displaced_visible_rows
        ),
        (0, 0)
    );
    assert!(stats.usage_displaced_sessions.is_empty());
    assert_eq!(store.usage_owner(ID_A), Some(ORIGIN));
}
