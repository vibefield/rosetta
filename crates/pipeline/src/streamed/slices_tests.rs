//! T6 tests on the writer's item machinery: chunked items equal whole ones
//! through `begin_item`/`step` (test 1), resolved rows go out in slices
//! (test 2), an Abandon item is one transaction (test 3), flip batches keep
//! their caps (test 9), and budget bytes are released by remainder
//! (test 13).

use std::collections::BTreeMap;
use std::sync::Arc;

use rosetta_adapter_claude_code::ClaudeCodeAdapter;
use rosetta_adapter_support::{AgentAdapter, FileDiscipline, FileFormat, StreamDriver};
use rosetta_capture::SourceDef;
use rosetta_core::IngestOp;
use rosetta_core::rows::{RuntimeRow, SessionRole, SessionRow, StreamState};
use rosetta_core::{EvidenceOrder, MessageRow, SourceIdentity};
use rosetta_store::{Batch, Store};
use rusqlite::types::Value;
use serde_json::json;

use super::{FlipCaps, Step, StreamedRuntime, TxKind};
use crate::StreamedPreparer;
use crate::checkpoint::PipeCheckpoint;
use crate::closed::{ClosedSource, LaneId, SessionLink, StreamedSetup, TreeMap};
use crate::instrument::Instrument;
use crate::live::PollEvidence;
use crate::meta::SessionFile;

const RUNTIME: &str = "runtime-slices";
const MIB: u64 = 1024 * 1024;

fn runtime() -> RuntimeRow {
    RuntimeRow {
        id: RUNTIME.into(),
        kind: "claude-code".into(),
        data_root: "~/.claude".into(),
        enabled: true,
        account: None,
        config: None,
        skills: None,
        versions_seen: None,
        discovered_at: 0,
        label: None,
        provenance: Default::default(),
    }
}

fn session_meta(id: &str, parent: Option<&str>) -> IngestOp {
    IngestOp::SessionMeta(SessionRow {
        id: id.into(),
        native_session_id: id.into(),
        runtime_id: RUNTIME.into(),
        project_id: None,
        cwd: None,
        parent_session_id: parent.map(str::to_owned),
        delegation: None,
        root_session_id: parent.unwrap_or(id).into(),
        role: if parent.is_some() {
            SessionRole::Subagent
        } else {
            SessionRole::Main
        },
        title: None,
        summary: None,
        started_at: 1,
        updated_at: 1,
        activity: None,
        stream_state: StreamState::Pending,
    })
}

fn file(path: &str, session_id: &str) -> SessionFile {
    SessionFile {
        ledger_only: false,
        relative_path: path.into(),
        stream_id: "transcript".into(),
        file_format: FileFormat::Jsonl,
        discipline: FileDiscipline::AppendOnly,
        driver: StreamDriver::JsonLines,
        session_id: session_id.into(),
        native_session_id: session_id.into(),
        size: 1,
        modified_ms: 1,
        agent_version: None,
        source_missing: false,
        tree_parent: None,
        cut_len: None,
        cut_modified_ms: None,
    }
}

fn source_def(path: &str, session_id: &str) -> SourceDef {
    SourceDef {
        ledger_only: false,
        relative_path: path.into(),
        stream_id: "transcript".into(),
        session_id: session_id.into(),
        native_session_id: session_id.into(),
        agent_version: None,
    }
}

/// A closed source whose framed payload measured what its ops write.
fn closed(path: &str, session_id: &str, ops: Vec<IngestOp>) -> ClosedSource {
    let source = source_def(path, session_id);
    let mut checkpoint = PipeCheckpoint::fresh(
        session_id,
        &source.stream_id,
        ClaudeCodeAdapter.normalizer_version(),
    );
    checkpoint.source_generation = 1;
    let payload_bytes = ops.iter().map(IngestOp::approx_bytes).sum();
    ClosedSource {
        source,
        generation: 1,
        ops,
        evidence: PollEvidence::of(Some(&checkpoint)),
        checkpoint: checkpoint.encode(),
        activate: true,
        complete: true,
        payload_bytes,
        prepared: None,
        lane: LaneId::Small,
    }
}

fn message(value: serde_json::Value) -> IngestOp {
    let row: MessageRow = serde_json::from_value(value).unwrap();
    IngestOp::project_message(row).pop().unwrap()
}

fn text(session: &str, timeline: &str, offset: i64, len: usize) -> IngestOp {
    message(json!({
        "timeline_id": timeline, "session_id": session, "ordinal": 0, "kind": "user",
        "ts": 10 + offset, "content": "x".repeat(len), "attachments": [], "citations": [],
        "src_offset": offset
    }))
}

fn opener(session: &str, key: &str, offset: i64, len: usize) -> IngestOp {
    message(json!({
        "timeline_id": format!("{session}/{key}"), "session_id": session, "ordinal": 0,
        "kind": "tool_use", "ts": 10 + offset, "attachments": [], "citations": [],
        "src_offset": offset,
        "tool": {
            "correlation": {"key": key, "derivation": "native_id"},
            "native": {"id": key, "name": "Bash"},
            "status": {"value": "pending", "basis": "request_observed"},
            "action": {"type": "shell", "command": {"type": "inline", "text": "y".repeat(len)},
                "source_truncated": false, "background": false}
        }
    }))
}

fn update(session: &str, key: &str, offset: u64) -> IngestOp {
    IngestOp::ToolUpdate {
        session_id: session.into(),
        correlation_key: key.into(),
        evidence: EvidenceOrder {
            source_identity: SourceIdentity("x.jsonl".into()),
            generation: 1,
            record_offset: offset,
            mutation_index: 0,
        },
        patch: Box::new(
            serde_json::from_value(json!({
                "status": {"value": "succeeded", "basis": "native_terminal_record"}
            }))
            .unwrap(),
        ),
        at: Some(20),
    }
}

/// One runtime on a fresh cold store: its writer and preparer.
struct Harness {
    store: Store,
    writer: StreamedRuntime,
    preparer: StreamedPreparer,
    instrument: Instrument,
}

/// `roster` as `(path, session)`; `links` join sessions into trees, and
/// make the runtime per-tree when `per_tree`.
fn harness(roster: &[(&str, &str)], links: &[(&str, &str)], per_tree: bool) -> Harness {
    let instrument = Instrument::new();
    let mut store = Store::open_in_memory_for_rebuild().unwrap();
    store.use_precomputed_bulk_ordinals().unwrap();
    let mut sessions: Vec<&str> = roster.iter().map(|(_, session)| *session).collect();
    sessions.sort_unstable();
    sessions.dedup();
    let parent_of: BTreeMap<&str, &str> = links.iter().copied().collect();
    let mut ops = vec![IngestOp::Runtime(runtime())];
    for session in &sessions {
        ops.push(session_meta(session, parent_of.get(session).copied()));
    }
    // A metadata-only placeholder: no roster source, never readable.
    ops.push(session_meta("placeholder", None));
    store
        .apply(&Batch {
            ops,
            checkpoints: vec![],
            source: None,
        })
        .unwrap();
    let files: Vec<SessionFile> = roster
        .iter()
        .map(|(path, session)| file(path, session))
        .collect();
    let tree_links: Vec<SessionLink> = links
        .iter()
        .map(|(child, parent)| ((*child).into(), Some((*parent).into()), None))
        .collect();
    let trees = per_tree.then(|| Arc::new(TreeMap::build(&files, &tree_links)));
    let mut sources: Vec<SourceDef> = roster
        .iter()
        .map(|(path, session)| source_def(path, session))
        .collect();
    sources.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    let setup = StreamedSetup {
        sources,
        metadata_sessions: sessions
            .iter()
            .map(|session| (*session).to_owned())
            .collect(),
        admission: Default::default(),
        trees: trees.clone(),
    };
    let writer = StreamedRuntime::open(
        &ClaudeCodeAdapter,
        &runtime(),
        setup,
        &mut store,
        &instrument,
    )
    .unwrap();
    let preparer = match trees {
        Some(trees) => {
            let lanes = vec![Some(LaneId::Small); trees.trees().len()];
            StreamedPreparer::with_trees(&files, trees, lanes)
        }
        None => StreamedPreparer::new(&files),
    };
    Harness {
        store,
        writer,
        preparer,
        instrument,
    }
}

/// One committed step of an item.
#[derive(Debug, Clone, Copy)]
struct Observed {
    kind: TxKind,
    approx_bytes: u64,
    released: u64,
}

impl Harness {
    /// Prepare `source` and apply it as one item under `chunk_bytes`,
    /// checking after every commit that `watched` is still Pending.
    fn apply(&mut self, source: ClosedSource, chunk_bytes: u64, watched: &[&str]) -> Vec<Observed> {
        let mut sources = vec![source];
        self.preparer.prepare(&mut sources).unwrap();
        let bytes = sources[0].budget_bytes();
        let mut item = self.writer.begin_item(sources, bytes, chunk_bytes).unwrap();
        let mut observed = Vec::new();
        loop {
            let (step, released, _) = self
                .writer
                .step(&mut self.store, &mut item, &self.instrument)
                .unwrap();
            let last = item.last_step().unwrap();
            observed.push(Observed {
                kind: last.kind,
                approx_bytes: last.approx_bytes,
                released,
            });
            for session in watched {
                assert_eq!(
                    state(&self.store, session),
                    "pending",
                    "{session} visible between slices"
                );
            }
            if step == Step::Done {
                break;
            }
        }
        assert_eq!(
            observed.iter().map(|step| step.released).sum::<u64>(),
            bytes,
            "an item releases exactly what it acquired"
        );
        assert_eq!(self.writer.items_in_progress(), 0);
        observed
    }

    fn admit(mut self) -> Store {
        let prepared = std::mem::replace(&mut self.preparer, StreamedPreparer::new(&[]))
            .finish()
            .unwrap();
        self.writer
            .admit(&mut self.store, &self.instrument, &prepared)
            .unwrap();
        self.store
    }
}

fn state(store: &Store, session: &str) -> String {
    store
        .conn()
        .query_row(
            "SELECT stream_state FROM session WHERE id = ?1",
            [session],
            |row| row.get(0),
        )
        .unwrap()
}

/// Every canonical table with message ids mapped to timeline ids.
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
                    && !(table == "agent_runtime" && column == "discovered_at")
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
                let mut fields = Vec::new();
                for (index, column) in columns.iter().enumerate() {
                    let value: Value = row.get(index)?;
                    let text = match (&value, column.as_str()) {
                        (Value::Integer(id), "message_id") => messages
                            .get(id)
                            .cloned()
                            .unwrap_or_else(|| format!("<dangling {id}>")),
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
        if rows != &actual[table] {
            let missing: Vec<_> = rows
                .iter()
                .filter(|row| !actual[table].contains(row))
                .take(3)
                .collect();
            let extra: Vec<_> = actual[table]
                .iter()
                .filter(|row| !rows.contains(row))
                .take(3)
                .collect();
            panic!("{label}: {table} differs\nwhole only: {missing:#?}\nsliced only: {extra:#?}");
        }
    }
}

/// A three-session runtime: `s-a` over two sources (its facts withheld and
/// resolved), `s-b` crossed by `s-c`'s source (not withheld), and large
/// prose, tool rows and updates throughout.
fn corpus(seed: u64) -> Vec<(&'static str, &'static str, Vec<IngestOp>)> {
    let mut rng = Rng(seed | 1);
    let mut crossing = ops_of(&mut rng, "s-c", "c", 10);
    crossing.push(text("s-b", "s-b/from-c", 999, 40));
    let a1 = ops_of(&mut rng, "s-a", "a", 20);
    let a2 = ops_of(&mut rng, "s-a", "b", 20);
    let b = ops_of(&mut rng, "s-b", "d", 10);
    vec![
        ("a1.jsonl", "s-a", a1),
        ("a2.jsonl", "s-a", a2),
        // Emitted before s-b's own source, so s-b is crossed (never
        // withheld) and its ordinals renumbered at admission.
        ("c.jsonl", "s-c", crossing),
        ("b.jsonl", "s-b", b),
    ]
}

struct Rng(u64);

impl Rng {
    fn below(&mut self, bound: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % bound
    }
}

fn ops_of(rng: &mut Rng, session: &str, prefix: &str, base: u64) -> Vec<IngestOp> {
    let count = base + rng.below(base * 2);
    let mut ops = Vec::new();
    for index in 0..count {
        let offset = i64::try_from(index).unwrap() * 10;
        let key = format!("{prefix}{index}");
        match rng.below(3) {
            0 => {
                let len = 100 + rng.below(6_000) as usize;
                ops.push(text(session, &format!("{session}/{key}/text"), offset, len));
            }
            1 => {
                let len = 50 + rng.below(3_000) as usize;
                ops.push(opener(session, &key, offset, len));
                ops.push(update(session, &key, index * 10 + 1));
            }
            _ => ops.push(opener(session, &key, offset, 10)),
        }
    }
    ops
}

/// T6 test 1, through `StreamedRuntime::{begin_item, step}`: every source
/// applied in Stage and resolved-row slices under a small chunk bound
/// writes exactly what applying it whole does, every row keeps its cursor,
/// and no session is visible between slices. (Mutation: skipping
/// `stamp_source_cursor` in `begin_item` leaves Stage rows without a
/// cursor.)
#[test]
fn chunked_items_equal_whole_items() {
    let roster = [
        ("a1.jsonl", "s-a"),
        ("a2.jsonl", "s-a"),
        ("b.jsonl", "s-b"),
        ("c.jsonl", "s-c"),
    ];
    let mut sliced_steps = 0;
    for seed in 0..12_u64 {
        let mut whole = harness(&roster, &[], false);
        for (path, session, ops) in corpus(seed) {
            whole.apply(closed(path, session, ops), u64::MAX, &["s-a", "s-b", "s-c"]);
        }
        let expected = dump(&whole.admit());

        let chunk = 4 * 1024 + (seed % 4) * 7_000;
        let mut sliced = harness(&roster, &[], false);
        for (path, session, ops) in corpus(seed) {
            let steps = sliced.apply(closed(path, session, ops), chunk, &["s-a", "s-b", "s-c"]);
            sliced_steps += steps.len() - 1;
        }
        let store = sliced.admit();
        assert_same(&format!("seed {seed}"), &expected, &dump(&store));
        let unstamped: i64 = store
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM message WHERE src_cursor IS NULL",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(unstamped, 0);
    }
    assert!(sliced_steps > 50, "slices were exercised: {sliced_steps}");
}

/// The ops of a tool-heavy giant: `count` file-change openers, each with a
/// `len`-byte inline diff (a shell command would be compacted to a preview).
fn giant(session: &str, count: usize, len: usize) -> Vec<IngestOp> {
    (0..count)
        .map(|index| {
            let key = format!("g{index}");
            message(json!({
                "timeline_id": format!("{session}/{key}"), "session_id": session,
                "ordinal": 0, "kind": "tool_use", "ts": 10, "attachments": [],
                "citations": [], "src_offset": index,
                "tool": {
                    "correlation": {"key": key, "derivation": "native_id"},
                    "native": {"id": key, "name": "Edit"},
                    "status": {"value": "pending", "basis": "request_observed"},
                    "action": {"type": "file_change", "changes": [{
                        "operation": null,
                        "path": {"type": "file", "path": format!("/src/{index}.rs"), "resolved": true},
                        "destination": null,
                        "diff": {"type": "inline", "text": "+".repeat(len)},
                        "lines_added": 1, "lines_removed": 0
                    }]}
                }
            }))
        })
        .collect()
}

/// T6 test 2: a Claude subagent giant's transcript is withheld until its
/// meta document; the document's item carries the giant's facts, and the
/// writer resolves them and writes ~52 MiB of tool rows in ≥ 3 resolved-row
/// slices of at most 16 MiB plus one row before the terminal. The session is
/// Pending at every commit, the fold holds none of its openers, and the
/// result equals the unsliced apply. (Mutation: writing every resolved row
/// in the terminal, as v2 did, breaks the per-transaction bound.)
#[test]
fn resolved_rows_are_written_in_slices() {
    let roster = [
        ("p.jsonl", "s-p"),
        ("p/subagents/agent-x.jsonl", "agent-x"),
        ("p/subagents/agent-x.meta.json", "agent-x"),
    ];
    let links = [("agent-x", "s-p")];
    let chunk = 16 * MIB;
    let row_text = 30_000;
    let run = |chunk_bytes: u64| {
        let mut harness = harness(&roster, &links, true);
        harness.apply(
            closed("p.jsonl", "s-p", vec![text("s-p", "p/1", 0, 20)]),
            chunk_bytes,
            &["s-p", "agent-x"],
        );
        harness.apply(
            closed(
                "p/subagents/agent-x.jsonl",
                "agent-x",
                giant("agent-x", 1_800, row_text),
            ),
            chunk_bytes,
            &["s-p", "agent-x"],
        );
        let steps = harness.apply(
            closed("p/subagents/agent-x.meta.json", "agent-x", Vec::new()),
            chunk_bytes,
            &["s-p", "agent-x"],
        );
        // The fold holds none of the giant's openers: they were resolved.
        assert_eq!(harness.writer.reconciliation.tail_bytes(), 0);
        let largest_row = 32 * 1024;
        (harness.admit(), steps, largest_row)
    };
    let (sliced, steps, largest_row) = run(chunk);
    let resolved = steps
        .iter()
        .filter(|step| step.kind == TxKind::ResolvedRows)
        .count();
    assert!(resolved >= 3, "{steps:?}");
    assert_eq!(steps.last().unwrap().kind, TxKind::Terminal);
    for step in &steps {
        assert!(
            step.approx_bytes <= chunk + largest_row,
            "a {}-byte transaction: {steps:?}",
            step.approx_bytes
        );
    }
    let (whole, steps, _) = run(u64::MAX);
    assert_eq!(steps.len(), 1);
    assert_same("resolved slices", &dump(&whole), &dump(&sliced));
    let rows: i64 = sliced
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM message WHERE type = 'tool_use'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(rows, 1_800);
}

/// T6 test 3: a 40 MiB Abandon item applies in exactly one transaction,
/// drift only, exactly as the same source whole, and leaves no usage claim
/// behind. (Mutation: chunking Abandon items gives several transactions,
/// and a Stage slice's claim survives in the owner index.)
#[test]
fn an_abandon_item_is_never_chunked() {
    let roster = [("a.jsonl", "s-a")];
    let mut ops: Vec<IngestOp> = (0..1_400)
        .map(|index| text("s-a", &format!("a/{index}"), index, 30_000))
        .collect();
    ops.insert(
        700,
        IngestOp::SessionUsage(
            serde_json::from_value(json!({
                "session_id": "s-a", "key": "k", "ts": 40, "model": "m",
                "tokens": {"input": 1}, "identity": "abandoned-request"
            }))
            .unwrap(),
        ),
    );
    ops.push(IngestOp::Drift(
        serde_json::from_value(json!({
            "runtime_id": RUNTIME, "agent_version": "1", "stream": "transcript",
            "record_kind": "raw_record", "error_code": "oversized", "shape_hash": "",
            "at": null, "sample_cursor": null, "sample_offset": 7
        }))
        .unwrap(),
    ));
    let run = |chunk: u64| {
        let mut harness = harness(&roster, &[], true);
        let mut source = closed("a.jsonl", "s-a", ops.clone());
        source.activate = false;
        assert!(source.budget_bytes() > 40 * MIB);
        let steps = harness.apply(source, chunk, &["s-a"]);
        assert_eq!(harness.store.usage_owner("abandoned-request"), None);
        (harness.admit(), steps)
    };
    let (chunked, steps) = run(16 * MIB);
    assert_eq!(steps.len(), 1, "{steps:?}");
    assert_eq!(steps[0].kind, TxKind::Terminal);
    let (whole, _) = run(u64::MAX);
    assert_same("abandon", &dump(&whole), &dump(&chunked));
    let drift: i64 = chunked
        .conn()
        .query_row("SELECT COUNT(*) FROM drift_agg", [], |row| row.get(0))
        .unwrap();
    let messages: i64 = chunked
        .conn()
        .query_row("SELECT COUNT(*) FROM message", [], |row| row.get(0))
        .unwrap();
    assert_eq!((drift, messages), (1, 0), "drift only");
    let mut chunked = chunked;
    assert!(
        chunked.finish_bulk_load().is_err(),
        "an abandoned build never finalizes"
    );
}

/// T6 test 13 (b–d): three chunked items whose bytes do not divide evenly by
/// their slices release exactly what they acquired, so a 300 MiB item on
/// the shared class still acquires afterwards under HEAD's rule; every
/// step reports its bytes and wall time. (Mutation: releasing per-slice
/// estimates leaves a residue, and the 300 MiB acquire never fits.)
#[test]
fn release_by_remainder_conserves_the_budget() {
    let roster = [("a.jsonl", "s-a"), ("b.jsonl", "s-b"), ("c.jsonl", "s-c")];
    let mut harness = harness(&roster, &[], false);
    let limit = 192 * MIB;
    let mut in_flight = 0_u64;
    for (index, (path, session)) in roster.iter().enumerate() {
        let ops: Vec<IngestOp> = (0..300)
            .map(|row| {
                text(
                    session,
                    &format!("{session}/{row}"),
                    row,
                    1_000 + 37 * index,
                )
            })
            .collect();
        let mut source = closed(path, session, ops);
        // Budget bytes no slice count divides.
        source.payload_bytes = 17 * MIB + 7 + index as u64 * 13 * MIB;
        let bytes = source.budget_bytes();
        in_flight += bytes;
        let steps = harness.apply(source, 64 * 1024, &[session]);
        assert!(steps.len() > 3, "{steps:?}");
        assert!(steps.iter().all(|step| step.approx_bytes > 0));
        in_flight -= steps.iter().map(|step| step.released).sum::<u64>();
    }
    assert_eq!(in_flight, 0, "no residue");
    let giant = 300 * MIB;
    assert!(
        in_flight == 0 || in_flight + giant <= limit,
        "the 300 MiB item acquires"
    );
    harness.admit();
}

/// T6 test 9: a flip batch takes at most 64 trees, 512 sessions and 16 MiB
/// of tails; a tree over a cap flips alone. Each batch reports its trees and
/// changed sessions; a placeholder without a roster source never flips.
#[test]
fn flip_batches_keep_their_caps() {
    let mut roster: Vec<(String, String)> = Vec::new();
    let mut links: Vec<(String, String)> = Vec::new();
    // One family of 513 sessions, closing first, then 65 single-session trees.
    roster.push(("a/root.jsonl".into(), "a-root".into()));
    for child in 0..512 {
        let session = format!("a-child-{child:03}");
        roster.push((format!("a/{session}.jsonl"), session.clone()));
        links.push((session, "a-root".into()));
    }
    for tree in 0..65 {
        let session = format!("t-{tree:02}");
        roster.push((format!("{session}.jsonl"), session));
    }
    let roster_refs: Vec<(&str, &str)> = roster
        .iter()
        .map(|(path, session)| (path.as_str(), session.as_str()))
        .collect();
    let link_refs: Vec<(&str, &str)> = links
        .iter()
        .map(|(child, parent)| (child.as_str(), parent.as_str()))
        .collect();
    let mut harness = harness(&roster_refs, &link_refs, true);
    for (path, session) in &roster {
        harness.apply(
            closed(
                path,
                session,
                vec![text(session, &format!("{session}/m"), 0, 10)],
            ),
            u64::MAX,
            &[],
        );
    }
    assert_eq!(harness.writer.ready_trees(), 66);
    let caps = FlipCaps {
        trees: 64,
        sessions: 512,
        tail_bytes: 16 * MIB,
    };
    let mut batches = Vec::new();
    loop {
        let admitted = harness
            .writer
            .admit_trees(&mut harness.store, &harness.instrument, caps)
            .unwrap();
        if admitted.trees.is_empty() {
            break;
        }
        batches.push((admitted.trees.len(), admitted.changed.len()));
    }
    // The 513-session family alone; then 64 trees; then the last one.
    assert_eq!(batches, [(1, 513), (64, 64), (1, 1)]);
    assert_eq!(state(&harness.store, "t-00"), "indexed");
    assert_eq!(state(&harness.store, "placeholder"), "pending");
    let store = harness.admit();
    assert_eq!(
        state(&store, "placeholder"),
        "pending",
        "a placeholder never flips"
    );
}

/// Two trees bound by a same-lane reference fold as one at the close: a
/// source of `s-b`'s tree carries the lifecycle update of `s-a`'s opener,
/// emitted before `s-a`'s tree closes, so both trees bind and their facts —
/// collected into different partitions — must still meet in one fold,
/// exactly as a runtime admitted whole folds them. The same holds for a
/// root-tree runtime that fell back to the path walk: its fold is still
/// partitioned, and no closure rule stops a source crossing trees there.
/// (Mutation: finishing each partition's fold on its own leaves the opener
/// `unknown`.)
#[test]
fn bound_trees_fold_their_partitions_together() {
    let roster = [("a.jsonl", "s-a"), ("b.jsonl", "s-b")];
    let sources = || {
        vec![
            (
                "b.jsonl",
                "s-b",
                vec![
                    text("s-b", "s-b/1", 0, 20),
                    update("s-a", "k", 5),
                    IngestOp::Usage {
                        session_id: "s-a".into(),
                        response: "s-a/k".into(),
                        ts: 30,
                        model: Some("model-x".into()),
                        tokens: serde_json::from_value(json!({"input": 3, "output": 4})).unwrap(),
                        identity: None,
                    },
                ],
            ),
            ("a.jsonl", "s-a", vec![opener("s-a", "k", 0, 10)]),
        ]
    };
    let run = |per_tree: bool, closure: bool| {
        let mut harness = harness(&roster, &[], per_tree);
        if !closure {
            // A path-walk fallback: partitioned fold, runtime preparer.
            let files: Vec<SessionFile> = roster
                .iter()
                .map(|(path, session)| file(path, session))
                .collect();
            harness.preparer = StreamedPreparer::new(&files);
        }
        for (path, session, ops) in sources() {
            harness.apply(closed(path, session, ops), u64::MAX, &["s-a", "s-b"]);
        }
        if per_tree && closure {
            assert_eq!(harness.preparer.report().trees_bound, 2);
            assert_eq!(harness.writer.ready_trees(), 0, "bound trees never flip");
        }
        harness.admit()
    };
    let whole = run(false, false);
    let status: String = whole
        .conn()
        .query_row(
            "SELECT tool_status FROM message WHERE type = 'tool_use'",
            [],
            |row| row.get(0),
        )
        .unwrap_or_default();
    assert_eq!(status, "succeeded", "the update reached its opener");
    assert_same("bound trees", &dump(&whole), &dump(&run(true, true)));
    assert_same("path fallback", &dump(&whole), &dump(&run(true, false)));
}

fn reply(session: &str, timeline: &str, identity: &str) -> Vec<IngestOp> {
    vec![
        message(json!({
            "timeline_id": timeline, "session_id": session, "ordinal": 0, "kind": "agent",
            "ts": 10, "content": "reply", "attachments": [], "citations": [], "src_offset": 0
        })),
        IngestOp::Usage {
            session_id: session.into(),
            response: timeline.into(),
            ts: 11,
            model: Some("model-x".into()),
            tokens: serde_json::from_value(json!({"input": 3, "output": 4})).unwrap(),
            identity: Some(identity.into()),
        },
    ]
}

/// R1 D15 at the close: a tree flipped early whose usage the remainder's
/// fold displaces — its older original arrives in a bound tree — is
/// reported, so the writer resyncs its readers. (Mutation: reporting only
/// the sessions whose state moved leaves the flipped copy stale.)
#[test]
fn the_remainder_reports_usage_it_displaced_on_a_flipped_tree() {
    let roster = [
        ("z-fork.jsonl", "z-fork"),
        ("m.jsonl", "m-other"),
        ("a-orig.jsonl", "a-orig"),
    ];
    let mut harness = harness(&roster, &[], true);
    let watched = ["a-orig", "m-other"];
    harness.apply(
        closed("z-fork.jsonl", "z-fork", reply("z-fork", "z/r", "req-1")),
        u64::MAX,
        &watched,
    );
    let caps = FlipCaps {
        trees: 1,
        sessions: usize::MAX,
        tail_bytes: u64::MAX,
    };
    let flipped = harness
        .writer
        .admit_trees(&mut harness.store, &harness.instrument, caps)
        .unwrap();
    assert_eq!(flipped.changed, ["z-fork".to_string()]);
    assert_eq!(harness.store.usage_owner("req-1"), Some("z-fork"));
    // `m-other` names `a-orig` before its tree closes: both trees bind.
    harness.apply(
        closed(
            "m.jsonl",
            "m-other",
            vec![
                text("m-other", "m/1", 0, 10),
                text("a-orig", "a/from-m", 1, 10),
            ],
        ),
        u64::MAX,
        &watched,
    );
    harness.apply(
        closed("a-orig.jsonl", "a-orig", reply("a-orig", "a/r", "req-1")),
        u64::MAX,
        &watched,
    );
    assert_eq!(harness.writer.ready_trees(), 0);
    let Harness {
        mut store,
        writer,
        preparer,
        instrument,
    } = harness;
    let prepared = preparer.finish().unwrap();
    let admitted = writer
        .admit_remainder(&mut store, &instrument, &prepared)
        .unwrap();
    assert_eq!(store.usage_owner("req-1"), Some("a-orig"));
    assert!(admitted.changed.contains(&"a-orig".to_string()));
    assert_eq!(admitted.displaced, ["z-fork".to_string()]);
}

/// R1 I2 at the tree level: a tree the preparer closed becomes ready only
/// once every one of its sources has committed, whatever order the writer
/// applies them in — a member hoisted into span `P` reaches the writer on
/// its own queue and may commit after the tree's closing source. (Mutation:
/// promoting a closed tree as soon as its closing source commits flips it
/// before its other source's rows.)
#[test]
fn a_closed_tree_waits_for_every_source_to_commit() {
    let roster = [("p.jsonl", "s-p"), ("p/x.jsonl", "s-x")];
    let mut harness = harness(&roster, &[("s-x", "s-p")], true);
    let mut sources = vec![
        closed("p.jsonl", "s-p", vec![text("s-p", "p/1", 0, 10)]),
        closed("p/x.jsonl", "s-x", vec![text("s-x", "x/1", 0, 10)]),
    ];
    harness.preparer.prepare(&mut sources).unwrap();
    assert_eq!(harness.preparer.report().trees_closed, 1);
    let first = sources.remove(0);
    for source in [sources.remove(0), first] {
        let bytes = source.budget_bytes();
        let mut item = harness
            .writer
            .begin_item(vec![source], bytes, u64::MAX)
            .unwrap();
        let (step, _, _) = harness
            .writer
            .step(&mut harness.store, &mut item, &harness.instrument)
            .unwrap();
        assert_eq!(step, Step::Done);
        if harness.writer.sources_applied() == 1 {
            assert_eq!(
                harness.writer.ready_trees(),
                0,
                "the closing source committed before the tree's other source"
            );
        }
    }
    assert_eq!(harness.writer.ready_trees(), 1);
    let caps = FlipCaps {
        trees: 1,
        sessions: usize::MAX,
        tail_bytes: u64::MAX,
    };
    let admitted = harness
        .writer
        .admit_trees(&mut harness.store, &harness.instrument, caps)
        .unwrap();
    assert_eq!(admitted.changed, ["s-p".to_string(), "s-x".to_string()]);
    harness.admit();
}

#[path = "slices_interleave_tests.rs"]
mod interleave;
