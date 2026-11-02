//! T6 tests 5 and 6: resolution decided on the producer and done on the
//! writer, and root-tree closure under R1 D21.

use rosetta_adapter_claude_code::ClaudeCodeAdapter;
use rosetta_adapter_support::{AgentAdapter, FileDiscipline, FileFormat, StreamDriver};
use rosetta_capture::SourceDef;
use rosetta_core::rows::{MessageRow, PlanRow, SessionUsageRow, TokenSnapshot};
use rosetta_core::{EvidenceOrder, SourceIdentity};
use rosetta_store::ResolvedSession;
use serde_json::json;

use super::*;
use crate::checkpoint::PipeCheckpoint;
use crate::closed::SessionLink;
use crate::live::PollEvidence;

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

fn closed_in(lane: LaneId, path: &str, session_id: &str, ops: Vec<IngestOp>) -> ClosedSource {
    let source = SourceDef {
        ledger_only: false,
        relative_path: path.into(),
        stream_id: "transcript".into(),
        session_id: session_id.into(),
        native_session_id: session_id.into(),
        agent_version: None,
    };
    let mut checkpoint = PipeCheckpoint::fresh(
        session_id,
        &source.stream_id,
        ClaudeCodeAdapter.normalizer_version(),
    );
    checkpoint.source_generation = 1;
    ClosedSource {
        source,
        generation: 1,
        ops,
        evidence: PollEvidence::of(Some(&checkpoint)),
        checkpoint: checkpoint.encode(),
        activate: true,
        complete: true,
        payload_bytes: 1,
        prepared: None,
        lane,
    }
}

fn closed(path: &str, session_id: &str, ops: Vec<IngestOp>) -> ClosedSource {
    closed_in(LaneId::Small, path, session_id, ops)
}

fn opener(session_id: &str, key: &str) -> IngestOp {
    let row: MessageRow = serde_json::from_value(json!({
        "timeline_id": format!("{session_id}/{key}"), "session_id": session_id,
        "ordinal": 0, "kind": "tool_use", "ts": 10, "attachments": [], "citations": [],
        "tool": {
            "correlation": {"key": key, "derivation": "native_id"},
            "native": {"id": key, "name": "Bash"},
            "status": {"value": "pending", "basis": "request_observed"},
            "action": {"type": "shell", "command": {"type": "inline", "text": "true"},
                "source_truncated": false, "background": false}
        }
    }))
    .unwrap();
    IngestOp::project_message(row).pop().unwrap()
}

fn reply(session_id: &str, timeline: &str) -> IngestOp {
    let row: MessageRow = serde_json::from_value(json!({
        "timeline_id": timeline, "session_id": session_id, "ordinal": 0, "kind": "agent",
        "ts": 12, "content": "done", "attachments": [], "citations": []
    }))
    .unwrap();
    IngestOp::project_message(row).pop().unwrap()
}

fn update(session_id: &str, key: &str) -> IngestOp {
    IngestOp::ToolUpdate {
        session_id: session_id.into(),
        correlation_key: key.into(),
        evidence: EvidenceOrder {
            source_identity: SourceIdentity("x.jsonl".into()),
            generation: 1,
            record_offset: 1,
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

fn usage(session_id: &str, response: &str) -> IngestOp {
    IngestOp::Usage {
        session_id: session_id.into(),
        response: response.into(),
        ts: 13,
        model: None,
        tokens: TokenSnapshot {
            input: Some(1),
            ..TokenSnapshot::default()
        },
        identity: None,
    }
}

fn sample(session_id: &str, key: &str) -> IngestOp {
    IngestOp::SessionUsage(SessionUsageRow {
        session_id: session_id.into(),
        key: key.into(),
        ts: 30,
        model: None,
        tokens: TokenSnapshot {
            input: Some(3),
            ..TokenSnapshot::default()
        },
        identity: None,
    })
}

fn plan_op(session_id: &str) -> IngestOp {
    IngestOp::Plan(PlanRow {
        session_id: session_id.into(),
        revision: 1,
        content: "plan".into(),
        updated_at: 5,
    })
}

/// The bytes `withhold` takes out of `ops`.
fn withheld_bytes(path: &str, ops: Vec<IngestOp>) -> u64 {
    WithheldFacts::withhold(path, 1, ops).1.approx_bytes()
}

fn prepared(source: &ClosedSource) -> &Prepared {
    source.prepared.as_ref().expect("prepared")
}

/// A root-tree preparer over `roster`, its trees linked by `links`, every
/// tree in `lane` except those `lanes` names.
fn tree_preparer(
    roster: &[SessionFile],
    links: &[SessionLink],
    lanes: &[(&str, LaneId)],
) -> (StreamedPreparer, Arc<TreeMap>) {
    let trees = Arc::new(TreeMap::build(roster, links));
    let mut lane_of_tree = vec![Some(LaneId::Small); trees.trees().len()];
    for (session, lane) in lanes {
        lane_of_tree[trees.tree_of(session).unwrap() as usize] = Some(*lane);
    }
    (
        StreamedPreparer::with_trees(roster, Arc::clone(&trees), lane_of_tree),
        trees,
    )
}

fn link(id: &str, parent: &str) -> SessionLink {
    (id.into(), Some(parent.into()), None)
}

// ── T6 test 5: preparer resolution ──────────────────────────────────────

/// (a) The completing source hands the writer the session's withheld
/// facts; resolving them there yields exactly what resolving them on the
/// producer did at HEAD.
#[test]
fn a_completed_session_hands_its_facts_to_the_writer() {
    let roster = [file("a.jsonl", "s-a"), file("a.meta.json", "s-a")];
    let mut preparer = StreamedPreparer::new(&roster);
    let ops = vec![opener("s-a", "k1"), reply("s-a", "r1"), update("s-a", "k1")];
    let mut sources = vec![
        closed("a.jsonl", "s-a", ops.clone()),
        closed("a.meta.json", "s-a", Vec::new()),
    ];
    preparer.prepare(&mut sources).unwrap();
    assert!(prepared(&sources[0]).to_resolve.is_empty());
    let handed = std::mem::take(&mut sources[1].prepared.as_mut().unwrap().to_resolve);
    assert_eq!(handed.len(), 1);
    assert_eq!(handed[0].len(), 1, "one withheld source");
    let on_writer = ResolvedSession::resolve(handed.into_iter().next().unwrap()).unwrap();
    // HEAD resolved the same facts on the producer.
    let (_, facts) = WithheldFacts::withhold("a.jsonl", 1, ops);
    let at_head = ResolvedSession::resolve(vec![facts]).unwrap();
    assert_eq!(on_writer.tool_rows(), at_head.tool_rows());
    assert_eq!(on_writer.rows_bytes(), at_head.rows_bytes());
    assert_eq!(on_writer.tool_rows(), 1);
    assert_eq!(preparer.finish().unwrap().report().sessions, 1);
}

/// (b) The completing source's budget carries the earlier sources'
/// withheld bytes, and not its own (already in its payload bytes).
#[test]
fn the_completing_source_carries_the_earlier_withheld_bytes() {
    let roster = [file("a1.jsonl", "s-a"), file("a2.jsonl", "s-a")];
    let first_ops = vec![opener("s-a", "k1"), update("s-a", "k1")];
    let second_ops = vec![opener("s-a", "k2"), reply("s-a", "r2")];
    let earlier = withheld_bytes("a1.jsonl", first_ops.clone());
    let own = withheld_bytes("a2.jsonl", second_ops.clone());
    assert!(earlier > 0 && own > 0);
    let mut preparer = StreamedPreparer::new(&roster);
    let mut sources = vec![
        closed("a1.jsonl", "s-a", first_ops),
        closed("a2.jsonl", "s-a", second_ops),
    ];
    preparer.prepare(&mut sources).unwrap();
    assert_eq!(prepared(&sources[0]).carried_bytes, 0);
    assert_eq!(prepared(&sources[1]).carried_bytes, earlier);
    assert_eq!(
        sources[1].budget_bytes(),
        sources[1].payload_bytes() + earlier
    );
    let report = preparer.finish().unwrap().report();
    assert_eq!(report.peak_pending_withheld_bytes, earlier + own);
}

/// (c) A crossed session releases its pending facts, unresolved, as at
/// HEAD, and the crossing source carries their bytes.
#[test]
fn a_crossed_session_releases_its_pending_facts() {
    let roster = [
        file("a.jsonl", "s-a"),
        file("a2.jsonl", "s-a"),
        file("b.jsonl", "s-b"),
    ];
    let first = vec![opener("s-a", "k1")];
    let pending = withheld_bytes("a.jsonl", first.clone());
    let mut preparer = StreamedPreparer::new(&roster);
    let mut sources = vec![
        closed("a.jsonl", "s-a", first),
        closed(
            "b.jsonl",
            "s-b",
            vec![opener("s-b", "k2"), reply("s-a", "x")],
        ),
        closed("a2.jsonl", "s-a", vec![update("s-a", "k1")]),
    ];
    preparer.prepare(&mut sources).unwrap();
    let crossing = prepared(&sources[1]);
    assert_eq!(crossing.released.len(), 1);
    assert!(crossing.to_resolve.is_empty());
    assert_eq!(crossing.carried_bytes, pending);
    assert!(prepared(&sources[2]).to_resolve.is_empty());
}

/// (d) An abandoned generation's facts are dropped with its batch, and it
/// taints its tree.
#[test]
fn an_abandoned_member_drops_its_facts_and_taints_its_tree() {
    let roster = [file("a1.jsonl", "s-a"), file("a2.jsonl", "s-a")];
    let (mut preparer, trees) = tree_preparer(&roster, &[], &[]);
    let mut abandoned = closed("a2.jsonl", "s-a", vec![opener("s-a", "k9")]);
    abandoned.activate = false;
    let mut sources = vec![
        closed("a1.jsonl", "s-a", vec![reply("s-a", "r")]),
        abandoned,
    ];
    preparer.prepare(&mut sources).unwrap();
    let tree = trees.tree_of("s-a").unwrap();
    assert_eq!(prepared(&sources[1]).tainted, Some(tree));
    assert!(!prepared(&sources[1]).withheld, "nothing withheld from it");
    assert_eq!(
        sources[1].ops.len(),
        1,
        "its batch keeps its ops (drift only)"
    );
    assert!(
        sources
            .iter()
            .all(|source| prepared(source).closed_trees.is_empty())
    );
    let report = preparer.finish().unwrap().report();
    assert_eq!((report.trees_tainted, report.trees_closed), (1, 0));
}

// ── T6 test 6: closure ──────────────────────────────────────────────────

/// A tree closes on its last member — a guardian (ledger-only) and a Codex
/// `#` member included — with only its public members to flip.
#[test]
fn a_tree_closes_on_its_last_member() {
    let mut guardian = file("g.jsonl", "s-g");
    guardian.ledger_only = true;
    guardian.tree_parent = Some("s-p".into());
    let mut member = file("state_5.sqlite#s-c", "s-c");
    member.driver = StreamDriver::SqliteSnapshot;
    let roster = [file("p.jsonl", "s-p"), member, guardian];
    let (mut preparer, trees) = tree_preparer(&roster, &[link("s-c", "s-p")], &[]);
    let mut guardian_source = closed("g.jsonl", "s-g", vec![sample("s-p", "guardian:1")]);
    guardian_source.source.ledger_only = true;
    let mut sources = vec![
        closed("p.jsonl", "s-p", vec![reply("s-p", "r1")]),
        closed("state_5.sqlite#s-c", "s-c", vec![reply("s-c", "r2")]),
        guardian_source,
    ];
    preparer.prepare(&mut sources).unwrap();
    assert!(prepared(&sources[0]).closed_trees.is_empty());
    assert!(prepared(&sources[1]).closed_trees.is_empty());
    assert_eq!(
        prepared(&sources[2]).closed_trees,
        [ClosedTree {
            tree: trees.tree_of("s-p").unwrap(),
            sessions: vec!["s-c".into(), "s-p".into()],
            renumber: Vec::new(),
        }]
    );
    let report = preparer.finish().unwrap().report();
    // The guardian's own-tree sample neither binds nor counts as late.
    assert_eq!(
        (
            report.trees_closed,
            report.cross_tree_crossings,
            report.late_samples
        ),
        (1, 0, 0)
    );
}

/// A closed tree carries the renumber subset of its sessions, which the
/// runtime's admission renumber then no longer holds.
#[test]
fn a_closed_tree_carries_its_renumber_subset() {
    let roster = [
        file("a1.jsonl", "s-a"),
        file("a2.jsonl", "s-a"),
        file("b1.jsonl", "s-b"),
        file("b2.jsonl", "s-b"),
        file("c.jsonl", "s-c"),
    ];
    let (mut preparer, trees) = tree_preparer(&roster, &[], &[]);
    let mut sources = vec![
        closed("a1.jsonl", "s-a", vec![reply("s-a", "m1")]),
        // A repeat in a later source of the same session: renumber.
        closed(
            "a2.jsonl",
            "s-a",
            vec![reply("s-a", "m1"), reply("s-a", "m2")],
        ),
        closed("b1.jsonl", "s-b", vec![reply("s-b", "n1")]),
        // s-b's tree binds to s-c's (same lane, still open): its renumber
        // waits for the close.
        closed("b2.jsonl", "s-b", vec![reply("s-b", "n1"), plan_op("s-c")]),
        closed("c.jsonl", "s-c", vec![reply("s-c", "o1")]),
    ];
    preparer.prepare(&mut sources).unwrap();
    assert_eq!(
        prepared(&sources[1]).closed_trees,
        [ClosedTree {
            tree: trees.tree_of("s-a").unwrap(),
            sessions: vec!["s-a".into()],
            renumber: vec!["s-a".into()],
        }]
    );
    assert!(
        sources[2..]
            .iter()
            .all(|source| prepared(source).closed_trees.is_empty())
    );
    let admission = preparer.finish().unwrap();
    assert_eq!(admission.sessions_to_renumber(), ["s-b".to_string()]);
    assert_eq!(admission.report().trees_bound, 2);
}

/// In RootTree mode a session with no facts resolves at its last source; a
/// later source naming it fails closed.
#[test]
fn a_factless_session_resolves_and_a_later_naming_fails_closed() {
    let roster = [file("a.jsonl", "s-a"), file("b.jsonl", "s-b")];
    let (mut preparer, _) = tree_preparer(&roster, &[link("s-b", "s-a")], &[]);
    let mut first = vec![closed("a.jsonl", "s-a", vec![reply("s-a", "r")])];
    preparer.prepare(&mut first).unwrap();
    assert_eq!(preparer.report().factless_resolved, 1);
    let mut later = vec![closed("b.jsonl", "s-b", vec![reply("s-a", "late")])];
    let error = preparer.prepare(&mut later).unwrap_err();
    assert!(error.to_string().contains("already resolved"), "{error}");

    // Runtime mode keeps HEAD's tolerance.
    let mut whole = StreamedPreparer::new(&roster);
    let mut sources = vec![
        closed("a.jsonl", "s-a", vec![reply("s-a", "r")]),
        closed("b.jsonl", "s-b", vec![reply("s-a", "late")]),
    ];
    whole.prepare(&mut sources).unwrap();
}

/// An abandoned member taints its tree; a skipped one (nothing could be
/// read) does not; an intra-tree target still closes.
#[test]
fn skipped_and_intra_tree_members_still_close() {
    let roster = [
        file("p.jsonl", "s-p"),
        file("c.jsonl", "s-c"),
        file("x.jsonl", "s-x"),
    ];
    let (mut preparer, trees) = tree_preparer(&roster, &[link("s-c", "s-p")], &[]);
    let mut skipped = closed("x.jsonl", "s-x", Vec::new());
    skipped.generation = 0;
    skipped.activate = false;
    skipped.complete = false;
    let mut sources = vec![
        // The child writes its parent's tool update before the parent
        // completes: same tree, so it crosses for resolution only.
        closed("c.jsonl", "s-c", vec![update("s-p", "k1")]),
        closed("p.jsonl", "s-p", vec![opener("s-p", "k1")]),
        skipped,
    ];
    preparer.prepare(&mut sources).unwrap();
    assert!(prepared(&sources[0]).closed_trees.is_empty());
    assert_eq!(
        prepared(&sources[1]).closed_trees[0].tree,
        trees.tree_of("s-p").unwrap()
    );
    assert_eq!(prepared(&sources[2]).tainted, None);
    assert_eq!(
        prepared(&sources[2]).closed_trees[0].tree,
        trees.tree_of("s-x").unwrap()
    );
    assert_eq!(preparer.finish().unwrap().report().cross_tree_crossings, 0);
}

/// A Plan (or Task) op naming a session of a closed tree in the same lane
/// fails closed.
#[test]
fn a_plan_naming_a_closed_tree_fails_closed() {
    let roster = [file("a.jsonl", "s-a"), file("b.jsonl", "s-b")];
    let (mut preparer, _) = tree_preparer(&roster, &[], &[]);
    let mut first = vec![closed("a.jsonl", "s-a", vec![reply("s-a", "r")])];
    preparer.prepare(&mut first).unwrap();
    assert_eq!(prepared(&first[0]).closed_trees.len(), 1);
    let mut later = vec![closed("b.jsonl", "s-b", vec![plan_op("s-a")])];
    let error = preparer.prepare(&mut later).unwrap_err().to_string();
    assert!(
        error.contains("addresses session s-a of closed tree s-a"),
        "{error}"
    );
}

/// A late usage sample naming another tree's session binds both trees (same
/// lane, target open) and counts in `late_samples`.
#[test]
fn a_late_sample_binds_and_counts() {
    let roster = [
        file("a.jsonl", "s-a"),
        file("b.jsonl", "s-b"),
        file("a2.jsonl", "s-a"),
    ];
    let (mut preparer, _) = tree_preparer(&roster, &[], &[]);
    let mut sources = vec![
        closed("a.jsonl", "s-a", vec![reply("s-a", "r")]),
        closed("b.jsonl", "s-b", vec![sample("s-a", "late:1")]),
        closed("a2.jsonl", "s-a", vec![reply("s-a", "r2")]),
    ];
    preparer.prepare(&mut sources).unwrap();
    assert!(
        sources
            .iter()
            .all(|source| prepared(source).closed_trees.is_empty())
    );
    let report = preparer.finish().unwrap().report();
    assert_eq!(
        (
            report.late_samples,
            report.cross_tree_crossings,
            report.trees_bound
        ),
        (1, 1, 2)
    );
}

/// D21: a same-lane forward reference — here a response `Usage` op naming
/// another tree's session, still open — binds both trees, so neither flips
/// early. (Mutation: `named_session` returning `None` for `Usage` closes
/// both trees.)
#[test]
fn a_same_lane_forward_reference_binds_both_trees() {
    let roster = [file("b.jsonl", "s-b"), file("a.jsonl", "s-a")];
    let (mut preparer, _) = tree_preparer(&roster, &[], &[]);
    let mut sources = vec![
        closed(
            "b.jsonl",
            "s-b",
            vec![reply("s-b", "r"), usage("s-a", "a/r")],
        ),
        closed("a.jsonl", "s-a", vec![reply("s-a", "a/r")]),
    ];
    preparer.prepare(&mut sources).unwrap();
    assert!(
        sources
            .iter()
            .all(|source| prepared(source).closed_trees.is_empty()),
        "a bound tree never closes early"
    );
    let report = preparer.finish().unwrap().report();
    assert_eq!((report.trees_bound, report.cross_tree_crossings), (2, 1));
}

/// D21: a same-lane backward reference (the target tree already closed)
/// fails closed.
#[test]
fn a_same_lane_backward_reference_fails_closed() {
    let roster = [file("a.jsonl", "s-a"), file("b.jsonl", "s-b")];
    let (mut preparer, _) = tree_preparer(&roster, &[], &[]);
    let mut first = vec![closed("a.jsonl", "s-a", vec![reply("s-a", "a/r")])];
    preparer.prepare(&mut first).unwrap();
    let mut later = vec![closed("b.jsonl", "s-b", vec![usage("s-a", "a/r")])];
    let error = preparer.prepare(&mut later).unwrap_err().to_string();
    assert!(error.contains("of closed tree s-a"), "{error}");
}

/// D21: a reference from lane S to a tree in lane B fails closed whether
/// that tree is still open or already closed — two emission orders, one
/// error. (Mutation: binding cross-lane references while the target is open
/// makes the two orders disagree.)
#[test]
fn a_cross_lane_reference_fails_closed_in_every_order() {
    let roster = [file("big.jsonl", "s-big"), file("small.jsonl", "s-small")];
    let lanes = [("s-big", LaneId::Big(0))];
    let reference = || {
        closed(
            "small.jsonl",
            "s-small",
            vec![reply("s-small", "r"), plan_op("s-big")],
        )
    };
    let giant = || {
        closed_in(
            LaneId::Big(0),
            "big.jsonl",
            "s-big",
            vec![reply("s-big", "g")],
        )
    };

    // The big tree still open.
    let (mut open_first, _) = tree_preparer(&roster, &[], &lanes);
    let open = open_first
        .prepare(&mut [reference()])
        .unwrap_err()
        .to_string();

    // The big tree already closed.
    let (mut closed_first, _) = tree_preparer(&roster, &[], &lanes);
    closed_first.prepare(&mut [giant()]).unwrap();
    let closed = closed_first
        .prepare(&mut [reference()])
        .unwrap_err()
        .to_string();

    let expected = "cold source small.jsonl addresses session s-big of tree s-big in lane B0";
    assert_eq!(open, closed);
    assert!(open.contains(expected), "{open}");
}

/// D21: a source of span P naming another tree fails closed too.
#[test]
fn a_prefix_member_naming_another_tree_fails_closed() {
    let roster = [file("state.sqlite#s-a", "s-a"), file("b.jsonl", "s-b")];
    let (mut preparer, _) = tree_preparer(&roster, &[], &[("s-a", LaneId::Prefix)]);
    let mut member = vec![closed_in(
        LaneId::Prefix,
        "state.sqlite#s-a",
        "s-a",
        vec![plan_op("s-b")],
    )];
    let error = preparer.prepare(&mut member).unwrap_err().to_string();
    assert!(error.contains("of tree s-b in lane S"), "{error}");
}
