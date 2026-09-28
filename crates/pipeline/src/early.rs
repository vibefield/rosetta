//! Tool lifecycles resolved where a closed source is emitted.
//!
//! The streamed writer folds a runtime's relational tool facts at admission,
//! because in general an opener, its lifecycle updates and the usage of its
//! response may come from several of the runtime's sources. Where they cannot,
//! the admission fold restricted to that session is the fold of one closed
//! source, and it can run on the producer, before the source ever reaches the
//! writer: the writer then inserts the final tool rows with the source, and
//! admission folds only what is left.
//!
//! Nothing here assumes how an adapter behaves. A session is resolved at file
//! close only when this runtime's own inputs prove it holds all of its facts:
//! - the roster gives the session exactly one source, and
//! - no other source has addressed the session with a row, a tool fact, usage
//!   or a file edit, and the source addresses no other session.
//!
//! Sources are emitted in path order, so the second condition is checked for
//! every earlier source when a session is resolved and for every later source
//! as it is emitted; a later source that addresses an already resolved session
//! fails the build closed, the same way admission refuses a fold that meets a
//! stored tool row. Everything else — sessions with several sources, and every
//! source that addresses another session — keeps the admission fold exactly as
//! before.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use rosetta_core::{IngestOp, TimelineBody};
use rosetta_store::ResolvedSourceTools;

use crate::PipelineError;
use crate::closed::ClosedSource;
use crate::meta::SessionFile;

/// What the resolver did for one runtime.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct EarlyToolReport {
    /// Sources whose tool rows were resolved at file close, and those rows.
    pub sources: u64,
    pub tool_rows: u64,
    /// Sources with tool facts left to admission because the roster gives
    /// their session another source.
    pub shared_session: u64,
    /// Sources with tool facts left to admission because they address
    /// another session or another source addressed theirs.
    pub crossed: u64,
    /// Producer time spent proving and resolving.
    pub nanos: u64,
}

/// One runtime's resolver, fed every closed source in emission order.
#[derive(Debug)]
pub struct EarlyToolResolver {
    sources_per_session: HashMap<String, u32>,
    resolved: HashSet<String>,
    addressed_elsewhere: HashSet<String>,
    report: EarlyToolReport,
}

impl EarlyToolResolver {
    /// A resolver for the runtime whose capture roster is `files`.
    pub fn new(files: &[SessionFile]) -> Self {
        let mut sources_per_session: HashMap<String, u32> = HashMap::new();
        for file in files {
            *sources_per_session
                .entry(file.session_id.clone())
                .or_default() += 1;
        }
        Self {
            sources_per_session,
            resolved: HashSet::new(),
            addressed_elsewhere: HashSet::new(),
            report: EarlyToolReport::default(),
        }
    }

    pub fn report(&self) -> EarlyToolReport {
        self.report
    }

    /// Resolve every source of `sources` whose session is proven to be
    /// wholly inside it; leave every other source as it is. `sources` must
    /// be the runtime's next emission, in emission order.
    pub fn resolve(&mut self, sources: &mut [ClosedSource]) -> Result<(), PipelineError> {
        let started = Instant::now();
        let result = sources
            .iter_mut()
            .try_for_each(|source| self.resolve_one(source));
        self.report.nanos = self
            .report
            .nanos
            .saturating_add(u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX));
        result
    }

    fn resolve_one(&mut self, source: &mut ClosedSource) -> Result<(), PipelineError> {
        // An abandoned generation projects nothing but drift, and its facts
        // never reach a fold.
        if !source.activate {
            return Ok(());
        }
        let own = &source.source.session_id;
        let mut tool_facts = false;
        let mut crosses = false;
        for op in &source.ops {
            let (target, tool_fact) = match op {
                IngestOp::Message(row) => (
                    &row.session_id,
                    matches!(row.body, TimelineBody::ToolUse { .. }),
                ),
                IngestOp::ToolUpdate { session_id, .. } => (session_id, true),
                IngestOp::Usage { session_id, .. } => (session_id, false),
                IngestOp::FileEdit(row) => (&row.session_id, false),
                _ => continue,
            };
            if source.source.ledger_only || target != own {
                if self.resolved.contains(target) {
                    return Err(PipelineError::Config(format!(
                        "cold source {} addresses session {target}, whose tool rows were \
                         resolved at file close",
                        source.source.relative_path
                    )));
                }
                if !self.addressed_elsewhere.contains(target) {
                    self.addressed_elsewhere.insert(target.clone());
                }
                crosses = true;
            } else {
                tool_facts |= tool_fact;
            }
        }
        if !tool_facts {
            return Ok(());
        }
        if crosses || self.addressed_elsewhere.contains(own) {
            self.report.crossed += 1;
            return Ok(());
        }
        if self.sources_per_session.get(own) != Some(&1) {
            self.report.shared_session += 1;
            return Ok(());
        }

        let ops = std::mem::take(&mut source.ops);
        let (ops, resolved) =
            ResolvedSourceTools::resolve(&source.source.relative_path, source.generation, ops)?;
        self.report.sources += 1;
        self.report.tool_rows += resolved.tool_rows();
        self.resolved.insert(own.clone());
        source.ops = ops;
        source.resolved = Some(resolved);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
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
            resolved: None,
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

    fn resolved(sources: &[ClosedSource]) -> Vec<bool> {
        sources
            .iter()
            .map(|source| source.resolved.is_some())
            .collect()
    }

    #[test]
    fn only_a_session_with_one_roster_source_resolves_at_file_close() {
        let roster = [
            file("a.jsonl", "s-a"),
            file("b.jsonl", "s-a"),
            file("c.jsonl", "s-b"),
        ];
        let mut resolver = EarlyToolResolver::new(&roster);
        let mut sources = vec![
            closed(
                "a.jsonl",
                "s-a",
                vec![opener("s-a", "k1"), update("s-a", "k1")],
            ),
            closed("b.jsonl", "s-a", vec![reply("s-a", "r1")]),
            closed(
                "c.jsonl",
                "s-b",
                vec![opener("s-b", "k2"), update("s-b", "k2")],
            ),
        ];
        resolver.resolve(&mut sources).unwrap();
        assert_eq!(resolved(&sources), [false, false, true]);
        // The resolved source's update was consumed; its row stays.
        assert_eq!(sources[2].ops.len(), 1);
        assert_eq!(sources[0].ops.len(), 2);
        let report = resolver.report();
        assert_eq!(
            (report.sources, report.tool_rows, report.shared_session),
            (1, 1, 1)
        );
    }

    #[test]
    fn a_source_addressing_another_session_leaves_both_to_admission() {
        let roster = [file("a.jsonl", "s-a"), file("b.jsonl", "s-b")];
        let mut resolver = EarlyToolResolver::new(&roster);
        let mut sources = vec![
            // a.jsonl carries one of s-b's rows: neither session is proven
            // to live in one source any more.
            closed(
                "a.jsonl",
                "s-a",
                vec![opener("s-a", "k1"), reply("s-b", "x")],
            ),
            closed("b.jsonl", "s-b", vec![opener("s-b", "k2")]),
        ];
        resolver.resolve(&mut sources).unwrap();
        assert_eq!(resolved(&sources), [false, false]);
        assert_eq!(resolver.report().crossed, 2);
    }

    #[test]
    fn a_later_source_addressing_a_resolved_session_fails_closed() {
        let roster = [file("a.jsonl", "s-a"), file("b.jsonl", "s-b")];
        let mut resolver = EarlyToolResolver::new(&roster);
        let mut first = vec![closed("a.jsonl", "s-a", vec![opener("s-a", "k1")])];
        resolver.resolve(&mut first).unwrap();
        assert_eq!(resolved(&first), [true]);

        let mut later = vec![closed("b.jsonl", "s-b", vec![update("s-a", "k1")])];
        let error = resolver.resolve(&mut later).unwrap_err();
        assert!(
            error.to_string().contains("resolved at file close"),
            "{error}"
        );
    }

    #[test]
    fn an_abandoned_generation_neither_resolves_nor_addresses() {
        let roster = [file("a.jsonl", "s-a"), file("b.jsonl", "s-b")];
        let mut resolver = EarlyToolResolver::new(&roster);
        let mut abandoned = closed(
            "a.jsonl",
            "s-a",
            vec![opener("s-a", "k1"), reply("s-b", "x")],
        );
        abandoned.activate = false;
        let mut sources = vec![
            abandoned,
            closed("b.jsonl", "s-b", vec![opener("s-b", "k2")]),
        ];
        resolver.resolve(&mut sources).unwrap();
        assert_eq!(resolved(&sources), [false, true]);
    }
}
