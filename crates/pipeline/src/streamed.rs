//! The canonical writer's half of the streamed fresh-cold build.
//!
//! A runtime's closed sources enter the shared canonical RAM projection while
//! its capture — and other runtimes' captures — continue. They land exactly as
//! [`crate::apply_prepared_runtime`] would write them: final canonical rows of
//! an activated generation, with every relational lifecycle fragment collected
//! into this runtime's own RAM fold. Nothing becomes public before
//! [`StreamedRuntime::admit`]: its sessions stay `Pending` (the read barrier)
//! until the fold has written each resolved tool row once and the single
//! stream-state commit flips them.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::time::Instant;

use rosetta_adapter_support::AgentAdapter;
use rosetta_core::IngestOp;
use rosetta_core::rows::{RuntimeRow, SessionRole, SessionRow, StreamState};
use rosetta_store::{
    ApplyStats, Batch, CheckpointUpdate, ColdReconciliation, SourceMode, SourceWrite, Store,
};

use crate::PipelineError;
use crate::checkpoint::PipeCheckpoint;
use crate::closed::{ClosedSource, StreamedSetup};
use crate::instrument::{Instrument, Metric};
use crate::replay::{RebuildReport, stamp_source_cursor};
use crate::stream::merge_stats;

struct StreamedCursor {
    id: i64,
    initial: String,
    applied: bool,
}

/// One runtime between its setup and its admission on the canonical writer.
pub struct StreamedRuntime {
    runtime_id: String,
    cursors: HashMap<String, StreamedCursor>,
    reconciliation: ColdReconciliation,
    ordinals: OrdinalPlan,
    session_health: BTreeMap<String, bool>,
    report: RebuildReport,
    apply_nanos: u64,
    sources_applied: u64,
}

impl StreamedRuntime {
    /// Write the runtime's setup — its runtime row, a stub for each session
    /// no metadata batch named, and every source's cursor at the fresh
    /// checkpoint — exactly like the setup batch of
    /// [`crate::apply_prepared_runtime`]. The caller has already published
    /// the runtime's metadata; stubs stay out of the metadata mirror until
    /// admission.
    pub fn open(
        adapter: &dyn AgentAdapter,
        runtime: &RuntimeRow,
        setup: StreamedSetup,
        store: &mut Store,
        instrument: &Instrument,
    ) -> Result<Self, PipelineError> {
        let mut report = RebuildReport {
            files: u64::try_from(setup.sources.len()).unwrap_or(u64::MAX),
            ..RebuildReport::default()
        };
        let mut ops = vec![IngestOp::Runtime(runtime.clone())];
        let mut stubbed = BTreeSet::new();
        for source in &setup.sources {
            if source.ledger_only
                || setup.metadata_sessions.contains(&source.session_id)
                || !stubbed.insert(source.session_id.clone())
            {
                continue;
            }
            report.sessions_stubbed += 1;
            ops.push(IngestOp::SessionMeta(SessionRow {
                id: source.session_id.clone(),
                native_session_id: source.native_session_id.clone(),
                runtime_id: runtime.id.clone(),
                project_id: None,
                cwd: None,
                parent_session_id: None,
                delegation: None,
                root_session_id: source.session_id.clone(),
                role: SessionRole::Main,
                title: None,
                summary: None,
                started_at: 0,
                updated_at: 0,
                activity: None,
                stream_state: StreamState::Pending,
            }));
        }
        let mut initial_by_path = HashMap::with_capacity(setup.sources.len());
        let mut checkpoints = Vec::with_capacity(setup.sources.len());
        for source in &setup.sources {
            let initial = PipeCheckpoint::fresh(
                &source.session_id,
                &source.stream_id,
                adapter.normalizer_version(),
            )
            .encode();
            initial_by_path.insert(source.relative_path.clone(), initial.clone());
            checkpoints.push(CheckpointUpdate {
                runtime_id: runtime.id.clone(),
                stream: source.stream_id.clone(),
                path: source.relative_path.clone(),
                session_id: source.projected_session_id(),
                expected: None,
                next: initial,
                updated_at: None,
            });
        }
        let stats = instrument.apply_unobserved(
            store,
            &Batch {
                ops,
                checkpoints,
                source: None,
            },
        )?;
        merge_stats(&mut report.stats, &stats);

        let mut cursors = HashMap::with_capacity(initial_by_path.len());
        for (id, path) in store.runtime_cursor_ids(&runtime.id)? {
            if let Some(initial) = initial_by_path.remove(&path) {
                cursors.insert(
                    path,
                    StreamedCursor {
                        id,
                        initial,
                        applied: false,
                    },
                );
            }
        }
        if let Some(path) = initial_by_path.keys().next() {
            return Err(PipelineError::Config(format!(
                "streamed cursor setup lost {path}"
            )));
        }
        Ok(Self {
            runtime_id: runtime.id.clone(),
            cursors,
            reconciliation: ColdReconciliation::default(),
            ordinals: OrdinalPlan::default(),
            session_health: BTreeMap::new(),
            report,
            apply_nanos: 0,
            sources_applied: 0,
        })
    }

    pub fn runtime_id(&self) -> &str {
        &self.runtime_id
    }

    /// Canonical writer time spent applying this runtime's streamed sources.
    pub fn apply_nanos(&self) -> u64 {
        self.apply_nanos
    }

    pub fn sources_applied(&self) -> u64 {
        self.sources_applied
    }

    /// Apply closed sources, in the path order the producer emits them, in
    /// one private transaction. Messages receive their final presentation
    /// ordinals here (see [`OrdinalPlan`]); deferred relational facts are
    /// collected into this runtime's own fold as the transaction commits.
    pub fn apply(
        &mut self,
        store: &mut Store,
        sources: Vec<ClosedSource>,
        instrument: &Instrument,
    ) -> Result<ApplyStats, PipelineError> {
        let started = Instant::now();
        let mut batches = Vec::with_capacity(sources.len());
        for mut source in sources {
            let cursor = self
                .cursors
                .get_mut(&source.source.relative_path)
                .ok_or_else(|| {
                    PipelineError::Config(format!(
                        "streamed source {} has no cursor in runtime {}",
                        source.source.relative_path, self.runtime_id
                    ))
                })?;
            if cursor.applied {
                return Err(PipelineError::Config(format!(
                    "streamed source {} was applied twice",
                    source.source.relative_path
                )));
            }
            cursor.applied = true;
            self.ordinals
                .assign(&source.source.relative_path, &mut source.ops)?;
            stamp_source_cursor(&mut source.ops, cursor.id);
            let clean = source.activate && source.generation_clean;
            self.session_health
                .entry(source.source.session_id.clone())
                .and_modify(|healthy| *healthy &= clean)
                .or_insert(clean);
            batches.push(Batch {
                ops: source.ops,
                checkpoints: vec![CheckpointUpdate {
                    runtime_id: self.runtime_id.clone(),
                    session_id: source.source.projected_session_id(),
                    stream: source.source.stream_id,
                    path: source.source.relative_path,
                    expected: Some(cursor.initial.clone()),
                    next: source.checkpoint,
                    updated_at: None,
                }],
                source: Some(SourceWrite {
                    cursor_id: cursor.id,
                    session_id: source.source.session_id,
                    generation: source.generation,
                    first_sequence: 0,
                    mode: if source.activate {
                        SourceMode::Activate
                    } else {
                        SourceMode::Abandon
                    },
                }),
            });
        }
        let count = u64::try_from(batches.len()).unwrap_or(u64::MAX);
        let stats = instrument.apply_streamed_sources(store, &mut self.reconciliation, batches)?;
        instrument.add(Metric::BatchesApplied, count);
        merge_stats(&mut self.report.stats, &stats);
        self.sources_applied += count;
        self.apply_nanos = self
            .apply_nanos
            .saturating_add(u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX));
        Ok(stats)
    }

    /// Admit the complete runtime: resolve its collected relational fold in
    /// RAM and write each resolved tool row once, canonicalize any session a
    /// cross-source repeat left out of order, then flip every source-backed
    /// session out of `Pending` in one commit — the first moment any of its
    /// rows can be read.
    pub fn admit(
        mut self,
        store: &mut Store,
        instrument: &Instrument,
    ) -> Result<RebuildReport, PipelineError> {
        if let Some((path, _)) = self.cursors.iter().find(|(_, cursor)| !cursor.applied) {
            return Err(PipelineError::Config(format!(
                "streamed runtime {} admitted before source {path} closed",
                self.runtime_id
            )));
        }
        let reconciled =
            instrument.fold_cold_reconciliation(store, std::mem::take(&mut self.reconciliation))?;
        merge_stats(&mut self.report.stats, &reconciled);

        let reorder = self.ordinals.sessions_to_renumber();
        if !reorder.is_empty() {
            let started = Instant::now();
            let rows = store.renumber_sessions_by_source_order(&reorder)?;
            instrument.add(
                Metric::SessionRenumbers,
                u64::try_from(reorder.len()).unwrap_or(u64::MAX),
            );
            instrument.add(Metric::SessionRenumberRows, rows);
            instrument.add(
                Metric::SessionRenumberNanos,
                u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX),
            );
        }

        let stream_states = self
            .session_health
            .into_iter()
            .map(|(session_id, clean)| {
                (
                    session_id,
                    if clean {
                        StreamState::Indexed
                    } else {
                        StreamState::Streaming
                    },
                )
            })
            .collect::<Vec<_>>();
        store.set_session_stream_states(&stream_states)?;
        Ok(self.report)
    }
}

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
#[derive(Default)]
struct OrdinalPlan {
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

    fn assign(&mut self, path: &str, ops: &mut [IngestOp]) -> Result<(), PipelineError> {
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

    fn sessions_to_renumber(&self) -> Vec<String> {
        self.renumber
            .iter()
            .map(|session| self.names[*session as usize].clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use rosetta_adapter_claude_code::ClaudeCodeAdapter;
    use rosetta_capture::SourceDef;
    use rosetta_core::rows::{MessageKind, MessageRow};

    use super::*;
    use crate::replay::precomputed_ordinals;

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
            checkpoint: checkpoint.encode(),
            generation_clean: true,
            activate: true,
            payload_bytes: 1,
        }
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
        };
        let mut writer = StreamedRuntime::open(
            &ClaudeCodeAdapter,
            &runtime(),
            setup,
            &mut store,
            &instrument,
        )?;
        for index in order {
            let (source, ops) = &sources[*index];
            writer.apply(
                &mut store,
                vec![closed(source.clone(), ops.clone())],
                &instrument,
            )?;
        }
        writer.admit(&mut store, &instrument)?;
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
