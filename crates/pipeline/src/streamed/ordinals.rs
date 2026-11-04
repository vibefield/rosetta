//! Final presentation ordinals for a streamed runtime ([`OrdinalPlan`]).

use std::collections::{BTreeSet, HashMap};

use rosetta_core::IngestOp;

use crate::PipelineError;

/// Final presentation ordinals for a streamed runtime, assigned as each
/// closed source lands instead of over the retained runtime.
///
/// The whole-runtime precompute ([`crate::prepare_captured_runtime`]) orders
/// every session by `(source path, position, source sequence, timeline id)`,
/// where a timeline repeated across sources takes its final occurrence. Within
/// one closed source this plan applies exactly that key; sources arrive in
/// path order, so continuing each session's counter reproduces the
/// precompute. Only a timeline repeated in a later source of the same session
/// (it moves there, leaving a gap) or a source arriving out of path order
/// breaks the continuation; those sessions are canonicalized at admission by
/// `(source path, assigned ordinal)`, which is the precompute order again.
/// Everything here is RAM only: a 128-bit key per timeline identity.
#[derive(Debug, Default)]
pub(crate) struct OrdinalPlan {
    next: HashMap<u32, i64>,
    sessions: HashMap<String, u32>,
    names: Vec<String>,
    last_path: HashMap<u32, String>,
    seen: HashMap<u128, (u32, u32)>,
    sources: u32,
    renumber: BTreeSet<u32>,
}

fn timeline_key(timeline_id: &str) -> u128 {
    let digest = blake3::hash(timeline_id.as_bytes());
    let mut key = [0_u8; 16];
    key.copy_from_slice(&digest.as_bytes()[..16]);
    u128::from_le_bytes(key)
}

struct FinalMessage {
    key: u128,
    session: u32,
    position: i64,
    sequence: u64,
    timeline_id: String,
}

impl OrdinalPlan {
    fn intern(&mut self, session_id: &str) -> Result<u32, PipelineError> {
        if let Some(index) = self.sessions.get(session_id) {
            return Ok(*index);
        }
        let index = u32::try_from(self.names.len()).map_err(|_| {
            PipelineError::Config("streamed runtime has more sessions than u32".to_string())
        })?;
        self.sessions.insert(session_id.to_owned(), index);
        self.names.push(session_id.to_owned());
        Ok(index)
    }

    pub(crate) fn assign(&mut self, path: &str, ops: &mut [IngestOp]) -> Result<(), PipelineError> {
        let source = self.sources;
        self.sources = self.sources.checked_add(1).ok_or_else(|| {
            PipelineError::Config("streamed runtime has more sources than u32".to_string())
        })?;

        // Final occurrence of each timeline identity within this source.
        let mut keys = Vec::with_capacity(ops.len());
        let mut finals: Vec<FinalMessage> = Vec::new();
        let mut final_index: HashMap<u128, usize> = HashMap::new();
        for (index, op) in ops.iter().enumerate() {
            let IngestOp::Message(row) = op else {
                keys.push(None);
                continue;
            };
            let sequence = u64::try_from(index).map_err(|_| {
                PipelineError::Config("source operation sequence exceeds u64".to_string())
            })?;
            let key = timeline_key(&row.timeline_id);
            let session = self.intern(&row.session_id)?;
            let message = FinalMessage {
                key,
                session,
                position: row
                    .provenance
                    .src_offset
                    .unwrap_or_else(|| i64::try_from(sequence).unwrap_or(i64::MAX)),
                sequence,
                timeline_id: row.timeline_id.clone(),
            };
            match final_index.get(&key) {
                Some(&at) => {
                    if finals[at].session != session {
                        return Err(self.moved(&row.timeline_id, finals[at].session, session));
                    }
                    finals[at] = message;
                }
                None => {
                    final_index.insert(key, finals.len());
                    finals.push(message);
                }
            }
            keys.push(Some(key));
        }

        finals.sort_by(|left, right| {
            (
                left.session,
                left.position,
                left.sequence,
                &left.timeline_id,
            )
                .cmp(&(
                    right.session,
                    right.position,
                    right.sequence,
                    &right.timeline_id,
                ))
        });
        let mut assigned: HashMap<u128, i64> = HashMap::with_capacity(finals.len());
        for message in &finals {
            match self.last_path.get(&message.session) {
                Some(last) if last.as_str() == path => {}
                // An earlier path after a later one: the session's sources
                // arrived out of path order.
                Some(last) if last.as_str() > path => {
                    self.renumber.insert(message.session);
                }
                _ => {
                    self.last_path.insert(message.session, path.to_owned());
                }
            }
            match self.seen.insert(message.key, (message.session, source)) {
                Some((session, _)) if session != message.session => {
                    return Err(self.moved(&message.timeline_id, session, message.session));
                }
                Some((_, earlier)) if earlier != source => {
                    // The identity takes its final position in this later
                    // source; the ordinal it held earlier becomes a gap.
                    self.renumber.insert(message.session);
                }
                _ => {}
            }
            let next = self.next.entry(message.session).or_insert(0);
            assigned.insert(message.key, *next);
            *next = next.checked_add(1).ok_or_else(|| {
                PipelineError::Config("session has more messages than i64 ordinals".to_string())
            })?;
        }

        for (op, key) in ops.iter_mut().zip(keys) {
            if let (IngestOp::Message(row), Some(key)) = (op, key) {
                row.ordinal = *assigned
                    .get(&key)
                    .expect("every message identity was assigned");
            }
        }
        Ok(())
    }

    fn moved(&self, timeline_id: &str, from: u32, to: u32) -> PipelineError {
        PipelineError::Config(format!(
            "timeline {timeline_id} moved between sessions {} and {} during cold planning",
            self.names[from as usize], self.names[to as usize]
        ))
    }

    pub(crate) fn sessions_to_renumber(&self) -> Vec<String> {
        self.renumber
            .iter()
            .map(|session| self.names[*session as usize].clone())
            .collect()
    }

    /// Take `sessions` out of the renumber set: those of a tree that flips
    /// on its own, whose flip renumbers them. Returns the ones it held.
    pub(crate) fn take_renumber(&mut self, sessions: &[String]) -> Vec<String> {
        let mut taken = Vec::new();
        for session in sessions {
            if let Some(index) = self.sessions.get(session)
                && self.renumber.remove(index)
            {
                taken.push(session.clone());
            }
        }
        taken
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use rosetta_adapter_claude_code::ClaudeCodeAdapter;
    use rosetta_capture::SourceDef;
    use rosetta_core::rows::{MessageKind, MessageRow};

    use super::*;
    use crate::checkpoint::PipeCheckpoint;
    use crate::closed::{ClosedSource, StreamedSetup};
    use crate::instrument::{Instrument, Metric};
    use crate::live::PollEvidence;
    use crate::replay::precomputed_ordinals;
    use crate::streamed::StreamedRuntime;
    use rosetta_adapter_support::AgentAdapter;
    use rosetta_core::rows::{RuntimeRow, SessionRole, SessionRow, StreamState};
    use rosetta_store::{Batch, Store};

    const RUNTIME: &str = "runtime-ordinals";

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

    fn session(id: &str) -> IngestOp {
        IngestOp::SessionMeta(SessionRow {
            id: id.into(),
            native_session_id: id.into(),
            runtime_id: RUNTIME.into(),
            project_id: None,
            cwd: None,
            parent_session_id: None,
            delegation: None,
            root_session_id: id.into(),
            role: SessionRole::Main,
            title: None,
            summary: None,
            started_at: 1,
            updated_at: 1,
            activity: None,
            stream_state: StreamState::Pending,
        })
    }

    fn message(timeline_id: &str, session_id: &str, src_offset: Option<i64>) -> IngestOp {
        IngestOp::project_message(MessageRow {
            timeline_id: timeline_id.into(),
            session_id: session_id.into(),
            ordinal: 0,
            kind: MessageKind::User,
            subtype: None,
            uuid: None,
            parent_uuid: None,
            ts: Some(10),
            model: None,
            effort: None,
            content: Some(format!("text of {timeline_id}")),
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
            src_offset,
        })
        .pop()
        .expect("a user message projects one row")
    }

    fn source(path: &str, session_id: &str) -> SourceDef {
        SourceDef {
            ledger_only: false,
            relative_path: path.into(),
            stream_id: "transcript".into(),
            session_id: session_id.into(),
            native_session_id: session_id.into(),
            agent_version: None,
        }
    }

    fn closed(source: SourceDef, ops: Vec<IngestOp>) -> ClosedSource {
        let mut checkpoint = PipeCheckpoint::fresh(
            &source.session_id,
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
            lane: crate::closed::LaneId::Path,
            capture: crate::capture::CaptureClock::default(),
        }
    }

    fn roster(sources: &[(SourceDef, Vec<IngestOp>)]) -> Vec<crate::meta::SessionFile> {
        sources
            .iter()
            .map(|(source, _)| crate::meta::SessionFile {
                ledger_only: false,
                relative_path: source.relative_path.clone(),
                stream_id: source.stream_id.clone(),
                file_format: rosetta_adapter_support::FileFormat::Jsonl,
                discipline: rosetta_adapter_support::FileDiscipline::AppendOnly,
                driver: rosetta_adapter_support::StreamDriver::JsonLines,
                session_id: source.session_id.clone(),
                native_session_id: source.native_session_id.clone(),
                size: 1,
                modified_ms: 1,
                agent_version: None,
                source_missing: false,
                tree_parent: None,
                cut_len: None,
                cut_modified_ms: None,
            })
            .collect()
    }

    /// Apply `sources` through the streamed writer in `order` and read every
    /// stored ordinal back, with the number of sessions it had to re-rank.
    fn streamed_ordinals(
        sources: &[(SourceDef, Vec<IngestOp>)],
        order: &[usize],
    ) -> Result<(BTreeMap<String, i64>, u64), PipelineError> {
        let instrument = Instrument::new();
        let mut store = Store::open_in_memory_for_rebuild().unwrap();
        store.use_precomputed_bulk_ordinals().unwrap();
        store
            .apply(&Batch {
                ops: vec![IngestOp::Runtime(runtime()), session("s-a"), session("s-b")],
                checkpoints: vec![],
                source: None,
            })
            .unwrap();
        let mut setup_sources = sources
            .iter()
            .map(|(source, _)| source.clone())
            .collect::<Vec<_>>();
        setup_sources.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
        let setup = StreamedSetup {
            sources: setup_sources,
            metadata_sessions: ["s-a".to_string(), "s-b".to_string()].into(),
            admission: Default::default(),
            trees: None,
        };
        let mut writer = StreamedRuntime::open(
            &ClaudeCodeAdapter,
            &runtime(),
            setup,
            &mut store,
            &instrument,
        )?;
        let mut preparer = crate::StreamedPreparer::new(&roster(sources));
        for index in order {
            let (source, ops) = &sources[*index];
            let mut emitted = vec![closed(source.clone(), ops.clone())];
            preparer.prepare(&mut emitted)?;
            writer.apply(&mut store, emitted, &instrument)?;
        }
        let prepared = preparer.finish()?;
        writer.admit(&mut store, &instrument, &prepared)?;
        let ordinals = store
            .conn()
            .prepare("SELECT timeline_id, ordinal FROM message")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        Ok((ordinals, instrument.get(Metric::SessionRenumbers)))
    }

    fn repeated_corpus() -> Vec<(SourceDef, Vec<IngestOp>)> {
        vec![
            (
                source("a.jsonl", "s-a"),
                vec![
                    message("m1", "s-a", Some(0)),
                    message("m2", "s-a", Some(10)),
                    message("m3", "s-a", Some(20)),
                    // A repeat inside one source takes its final position.
                    message("m2", "s-a", Some(30)),
                    // One source may carry another session's rows.
                    message("x1", "s-b", Some(5)),
                ],
            ),
            (
                source("b.jsonl", "s-a"),
                vec![
                    // A repeat in a later source moves the row here.
                    message("m3", "s-a", Some(0)),
                    // Equal offsets fall back to the source sequence.
                    message("m4", "s-a", Some(10)),
                    message("m5", "s-a", Some(10)),
                    // No offset ranks at the source sequence.
                    message("m6", "s-a", None),
                ],
            ),
            (
                source("c.jsonl", "s-b"),
                vec![
                    message("x2", "s-b", None),
                    message("x3", "s-b", Some(1)),
                    message("x1", "s-b", Some(2)),
                ],
            ),
        ]
    }

    #[test]
    fn streamed_ordinals_equal_the_whole_runtime_precompute() {
        let corpus = repeated_corpus();
        let expected = precomputed_ordinals(corpus.clone()).unwrap();
        let (actual, renumbered) = streamed_ordinals(&corpus, &[0, 1, 2]).unwrap();
        assert_eq!(actual, expected);
        // Both sessions saw a cross-source repeat; nothing else was re-ranked.
        assert_eq!(renumbered, 2);
    }

    #[test]
    fn in_order_sources_without_repeats_need_no_admission_renumber() {
        let corpus = vec![
            (
                source("a.jsonl", "s-a"),
                vec![message("m1", "s-a", Some(0)), message("m2", "s-a", Some(9))],
            ),
            (
                source("b.jsonl", "s-a"),
                vec![message("m3", "s-a", Some(0)), message("m4", "s-a", None)],
            ),
            (
                source("c.jsonl", "s-b"),
                vec![message("x1", "s-b", Some(3))],
            ),
        ];
        let expected = precomputed_ordinals(corpus.clone()).unwrap();
        let (actual, renumbered) = streamed_ordinals(&corpus, &[0, 1, 2]).unwrap();
        assert_eq!(actual, expected);
        assert_eq!(renumbered, 0);
    }

    #[test]
    fn a_source_arriving_out_of_path_order_is_re_ranked_at_admission() {
        let corpus = vec![
            (
                source("a.jsonl", "s-a"),
                vec![message("m1", "s-a", Some(0)), message("m2", "s-a", Some(9))],
            ),
            (
                source("b.jsonl", "s-a"),
                vec![message("m3", "s-a", Some(0)), message("m4", "s-a", Some(1))],
            ),
        ];
        let expected = precomputed_ordinals(corpus.clone()).unwrap();
        let (actual, renumbered) = streamed_ordinals(&corpus, &[1, 0]).unwrap();
        assert_eq!(actual, expected);
        assert_eq!(renumbered, 1);
    }

    #[test]
    fn a_timeline_moving_between_sessions_fails_like_the_precompute() {
        let corpus = vec![
            (
                source("a.jsonl", "s-a"),
                vec![message("m1", "s-a", Some(0))],
            ),
            (
                source("b.jsonl", "s-b"),
                vec![message("m1", "s-b", Some(0))],
            ),
        ];
        assert!(precomputed_ordinals(corpus.clone()).is_err());
        let error = streamed_ordinals(&corpus, &[0, 1]).unwrap_err();
        assert!(
            error.to_string().contains("moved between sessions"),
            "{error}"
        );
    }
}
