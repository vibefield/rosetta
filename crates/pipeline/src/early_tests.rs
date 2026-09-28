use rosetta_adapter_claude_code::ClaudeCodeAdapter;
use rosetta_adapter_support::{AgentAdapter, FileDiscipline, FileFormat, StreamDriver};
use rosetta_capture::SourceDef;
use rosetta_core::rows::MessageRow;
use rosetta_core::{EvidenceOrder, SourceIdentity};
use serde_json::json;

use super::*;
use crate::checkpoint::PipeCheckpoint;
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
    }
}

fn closed(path: &str, session_id: &str, ops: Vec<IngestOp>) -> ClosedSource {
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
        generation_clean: true,
        activate: true,
        payload_bytes: 1,
        prepared: None,
    }
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

/// `(withheld, resolved sessions, released facts)` per prepared source.
fn prepared(sources: &[ClosedSource]) -> Vec<(bool, usize, usize)> {
    sources
        .iter()
        .map(|source| {
            let prepared = source.prepared.as_ref().expect("prepared");
            (
                prepared.withheld,
                prepared.resolved.len(),
                prepared.released.len(),
            )
        })
        .collect()
}

#[test]
fn a_session_resolves_when_its_last_roster_source_is_emitted() {
    let roster = [
        file("a.jsonl", "s-a"),
        file("a.meta.json", "s-a"),
        file("b.jsonl", "s-b"),
    ];
    let mut preparer = StreamedPreparer::new(&roster);
    let mut first = vec![closed(
        "a.jsonl",
        "s-a",
        vec![opener("s-a", "k1"), reply("s-a", "r1"), update("s-a", "k1")],
    )];
    preparer.prepare(&mut first).unwrap();
    // The facts wait on the producer; the reply goes ahead with its ordinal.
    assert_eq!(prepared(&first), [(true, 0, 0)]);
    assert_eq!(first[0].ops.len(), 1);

    let mut later = vec![
        closed("a.meta.json", "s-a", Vec::new()),
        closed("b.jsonl", "s-b", vec![opener("s-b", "k2")]),
    ];
    preparer.prepare(&mut later).unwrap();
    // s-a completes with its sidecar, s-b with its only source.
    assert_eq!(prepared(&later), [(true, 1, 0), (true, 1, 0)]);
    let report = preparer.finish().unwrap().report();
    assert_eq!((report.sessions, report.tool_rows), (2, 2));
}

#[test]
fn a_crossed_session_gets_its_withheld_facts_back_unresolved() {
    let roster = [
        file("a.jsonl", "s-a"),
        file("a2.jsonl", "s-a"),
        file("b.jsonl", "s-b"),
        file("c.jsonl", "s-c"),
    ];
    let mut preparer = StreamedPreparer::new(&roster);
    let mut sources = vec![
        closed("a.jsonl", "s-a", vec![opener("s-a", "k1")]),
        // b.jsonl carries one of s-a's rows: neither session is proven
        // self-contained any more, and s-a's withheld facts return.
        closed(
            "b.jsonl",
            "s-b",
            vec![opener("s-b", "k2"), reply("s-a", "x")],
        ),
        closed("a2.jsonl", "s-a", vec![update("s-a", "k1")]),
        closed("c.jsonl", "s-c", vec![opener("s-c", "k3")]),
    ];
    preparer.prepare(&mut sources).unwrap();
    assert_eq!(
        prepared(&sources),
        [(true, 0, 0), (false, 0, 1), (false, 0, 0), (true, 1, 0)]
    );
    // Only the crossing source's and the later sources' facts stay in their
    // batches.
    assert_eq!(sources[1].ops.len(), 2);
    assert_eq!(sources[2].ops.len(), 1);
    let report = preparer.finish().unwrap().report();
    assert_eq!(
        (
            report.sessions,
            report.crossed_sessions,
            report.released_sources
        ),
        (1, 2, 1)
    );
}

#[test]
fn a_source_addressing_a_resolved_session_fails_closed() {
    let roster = [file("a.jsonl", "s-a"), file("b.jsonl", "s-b")];
    let mut preparer = StreamedPreparer::new(&roster);
    let mut first = vec![closed("a.jsonl", "s-a", vec![opener("s-a", "k1")])];
    preparer.prepare(&mut first).unwrap();
    assert_eq!(prepared(&first), [(true, 1, 0)]);

    let mut later = vec![closed("b.jsonl", "s-b", vec![update("s-a", "k1")])];
    let error = preparer.prepare(&mut later).unwrap_err();
    assert!(error.to_string().contains("already resolved"), "{error}");
}

#[test]
fn an_abandoned_generation_only_counts_towards_its_session() {
    let roster = [
        file("a.jsonl", "s-a"),
        file("a2.jsonl", "s-a"),
        file("b.jsonl", "s-b"),
    ];
    let mut preparer = StreamedPreparer::new(&roster);
    let mut abandoned = closed(
        "a2.jsonl",
        "s-a",
        vec![opener("s-a", "k9"), reply("s-b", "x")],
    );
    abandoned.activate = false;
    let mut sources = vec![
        closed("a.jsonl", "s-a", vec![opener("s-a", "k1")]),
        abandoned,
        closed("b.jsonl", "s-b", vec![opener("s-b", "k2")]),
    ];
    preparer.prepare(&mut sources).unwrap();
    // The abandoned source neither withholds nor crosses, and completes s-a.
    assert_eq!(
        prepared(&sources),
        [(true, 0, 0), (false, 1, 0), (true, 1, 0)]
    );
    assert_eq!(sources[1].ops.len(), 2);
}

#[test]
fn a_session_still_waiting_at_the_end_is_an_error() {
    let roster = [file("a.jsonl", "s-a"), file("a2.jsonl", "s-a")];
    let mut preparer = StreamedPreparer::new(&roster);
    let mut sources = vec![closed("a.jsonl", "s-a", vec![opener("s-a", "k1")])];
    preparer.prepare(&mut sources).unwrap();
    let error = preparer.finish().unwrap_err();
    assert!(error.to_string().contains("never emitted"), "{error}");
}

#[test]
fn without_early_tools_every_fact_stays_in_its_batch() {
    let roster = [file("a.jsonl", "s-a")];
    let mut preparer = StreamedPreparer::without_early_tools(&roster);
    let mut sources = vec![closed(
        "a.jsonl",
        "s-a",
        vec![opener("s-a", "k1"), update("s-a", "k1")],
    )];
    preparer.prepare(&mut sources).unwrap();
    assert_eq!(prepared(&sources), [(false, 0, 0)]);
    assert_eq!(sources[0].ops.len(), 2);
}
