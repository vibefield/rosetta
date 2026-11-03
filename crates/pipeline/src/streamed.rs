//! The canonical writer's half of the streamed fresh-cold build.
//!
//! A runtime's closed sources enter the shared canonical RAM projection while
//! its capture — and other runtimes' captures — continue. They land exactly as
//! [`crate::apply_prepared_runtime`] would write them: final canonical rows of
//! an activated generation, with final ordinals and every tool lifecycle the
//! producer could resolve already ranked and resolved there
//! ([`crate::StreamedPreparer`]); every other relational lifecycle fragment is
//! collected into this runtime's own RAM fold. Nothing becomes public before
//! [`StreamedRuntime::admit`]: its sessions stay `Pending` (the read barrier)
//! until the fold has written each remaining tool row once and the single
//! stream-state commit flips them.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::time::Instant;

use rosetta_adapter_support::AgentAdapter;
use rosetta_core::IngestOp;
use rosetta_core::rows::{RuntimeRow, SessionRole, SessionRow, StreamState};
use rosetta_store::{
    ApplyStats, Batch, CheckpointUpdate, ColdReconciliation, ResolvedSession, Store, StreamedSource,
};

use crate::PipelineError;
use crate::checkpoint::PipeCheckpoint;
use crate::closed::{ClosedSource, StreamedSetup};
use crate::early::PreparedAdmission;
use crate::instrument::{Instrument, Metric};
use crate::live::PollEvidence;
use crate::replay::{RebuildReport, stamp_source_cursor};
use crate::stream::merge_stats;

mod item;
mod ordinals;
mod trees;

#[cfg(test)]
mod slices_tests;

pub use item::{ApplyItem, Step, StepReport, TxKind, cut_slices};
use item::{Settled, ops_bytes, plan_stage, resolved_bytes, terminal_batch};
pub(crate) use ordinals::OrdinalPlan;
use trees::TreeLedger;
pub use trees::{FlipCaps, TreeAdmission};

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
    session_health: BTreeMap<String, bool>,
    /// The poll evidence of every final checkpoint committed so far, by path.
    evidence: HashMap<String, PollEvidence>,
    report: RebuildReport,
    apply_nanos: u64,
    sources_applied: u64,
    resolved_sessions: u64,
    resolved_tool_rows: u64,
    /// Writer time spent resolving withheld facts (R1 D22).
    resolve_nanos: u64,
    /// Items between their first and their terminal transaction.
    items_in_progress: u32,
    /// The trees of a runtime admitted per root tree.
    trees: Option<TreeLedger>,
    /// Measurement only: split each step's bytes by kind (the writer's
    /// `--json --profile`); off, a step scans none of its ops for it.
    measure_steps: bool,
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
        let cursors_len = cursors.len();
        // A runtime admitted per tree folds each tree's facts apart, so a
        // closed tree flips on its own; one admitted whole keeps one fold.
        let trees = setup
            .trees
            .as_deref()
            .map(|trees| TreeLedger::new(&setup.sources, trees));
        let reconciliation = trees
            .as_ref()
            .map_or_else(ColdReconciliation::default, |trees| {
                ColdReconciliation::partitioned(trees.unit_of_path())
            });
        Ok(Self {
            runtime_id: runtime.id.clone(),
            cursors,
            reconciliation,
            session_health: BTreeMap::new(),
            evidence: HashMap::with_capacity(cursors_len),
            report,
            apply_nanos: 0,
            sources_applied: 0,
            resolved_sessions: 0,
            resolved_tool_rows: 0,
            resolve_nanos: 0,
            items_in_progress: 0,
            trees,
            measure_steps: false,
        })
    }

    /// Measurement only: whether each step reports its asset bytes apart
    /// ([`StepReport::asset_bytes`]; zero when off). Changes no write.
    pub fn measure_steps(&mut self, on: bool) {
        self.measure_steps = on;
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

    /// Sessions whose tool rows the producer resolved, and those rows:
    /// written with the sources that completed them instead of at admission.
    pub fn resolved_before_admission(&self) -> (u64, u64) {
        (self.resolved_sessions, self.resolved_tool_rows)
    }

    /// The live stat poll's evidence for every final checkpoint this writer
    /// has committed — the seed that spares the runtime's startup catch-up
    /// from reading and decoding its cursor rows again.
    pub fn take_poll_evidence(&mut self) -> HashMap<String, PollEvidence> {
        std::mem::take(&mut self.evidence)
    }

    /// Writer time spent resolving the sessions sources completed.
    pub fn resolve_nanos(&self) -> u64 {
        self.resolve_nanos
    }

    /// Items between their first and their terminal transaction.
    pub fn items_in_progress(&self) -> u32 {
        self.items_in_progress
    }

    /// Apply closed sources, prepared in the order the producer emits them,
    /// in one private transaction: their rows with the final ordinals the
    /// preparer ranked, the tool rows of the sessions they completed, and
    /// every relational fact left to admission collected into this runtime's
    /// own fold as the transaction commits. The unchunked path:
    /// [`StreamedRuntime::begin_item`] then [`StreamedRuntime::step`] once.
    pub fn apply(
        &mut self,
        store: &mut Store,
        sources: Vec<ClosedSource>,
        instrument: &Instrument,
    ) -> Result<ApplyStats, PipelineError> {
        let bytes = sources
            .iter()
            .map(ClosedSource::budget_bytes)
            .fold(0, u64::saturating_add);
        let mut item = self.begin_item(sources, bytes, u64::MAX)?;
        let mut totals = ApplyStats::default();
        loop {
            let (step, _, stats) = self.step(store, &mut item, instrument)?;
            totals.merge_from(&stats);
            if step == Step::Done {
                return Ok(totals);
            }
        }
    }

    /// Begin one writer item holding `bytes` of the writer budget: validate
    /// its sources, stamp each op's cursor, resolve the sessions they
    /// complete from the facts the preparer withheld, bind resolved and
    /// released facts to their cursors, and plan its transactions. One
    /// source over `chunk_bytes` that does not abandon is planned in Stage
    /// and resolved-row slices; anything else is one terminal transaction.
    /// Writes nothing.
    pub fn begin_item(
        &mut self,
        sources: Vec<ClosedSource>,
        bytes: u64,
        chunk_bytes: u64,
    ) -> Result<ApplyItem, PipelineError> {
        let chunked = sources.len() == 1
            && bytes > chunk_bytes
            && (sources[0].activate || sources[0].skipped());
        let mut terminal = Vec::with_capacity(sources.len());
        let mut settled = Vec::with_capacity(sources.len());
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
            let cursor_id = cursor.id;
            let initial = cursor.initial.clone();
            let prepared = source.prepared.take().ok_or_else(|| {
                PipelineError::Config(format!(
                    "streamed source {} reached the writer unprepared",
                    source.source.relative_path
                ))
            })?;
            stamp_source_cursor(&mut source.ops, cursor_id);
            // What the preparer withheld belongs to the sources that
            // captured it: resolve it here, on the writer (R1 D22), and stamp
            // their cursors, as on every other row.
            let resolve_started = Instant::now();
            let mut resolved = Vec::with_capacity(prepared.to_resolve.len());
            for facts in prepared.to_resolve {
                let mut session = ResolvedSession::resolve(facts)?;
                session.bind_cursors(|path| self.cursors.get(path).map(|cursor| cursor.id))?;
                self.resolved_sessions += 1;
                self.resolved_tool_rows += session.tool_rows();
                resolved.push(session);
            }
            self.resolve_nanos = self
                .resolve_nanos
                .saturating_add(u64::try_from(resolve_started.elapsed().as_nanos()).unwrap_or(0));
            let mut released = prepared.released;
            for facts in &mut released {
                let cursor_id = self
                    .cursors
                    .get(facts.path())
                    .map(|cursor| cursor.id)
                    .ok_or_else(|| {
                        PipelineError::Config(format!(
                            "released facts of {} have no cursor in runtime {}",
                            facts.path(),
                            self.runtime_id
                        ))
                    })?;
                facts.bind_cursor(cursor_id);
            }
            // The pass's own verdict decides the badge: a source that is not
            // complete — unclean, stopped before a record its writer has not
            // finished, or cut short or skipped because its file could not be
            // read — holds its session Streaming, exactly as a live pass does.
            settled.push(Settled {
                path: source.source.relative_path.clone(),
                session_id: source.source.session_id.clone(),
                complete: source.complete,
                evidence: source.evidence,
                closed_trees: prepared.closed_trees,
            });
            let skipped = source.skipped();
            let batch = if skipped {
                // Nothing to write: its cursor keeps the fresh checkpoint.
                // Only the facts of a session it completed travel with it.
                Batch {
                    ops: Vec::new(),
                    checkpoints: Vec::new(),
                    source: None,
                }
            } else {
                terminal_batch(
                    &self.runtime_id,
                    &source.source,
                    cursor_id,
                    initial,
                    source.checkpoint,
                    source.generation,
                    source.activate,
                    source.ops,
                )
            };
            terminal.push(StreamedSource {
                batch,
                withheld: prepared.withheld && !skipped,
                resolved,
                released,
            });
        }
        let (stage, terminal_bytes) = if chunked {
            let resolved = resolved_bytes(&terminal);
            let ops = ops_bytes(&terminal[0].batch.ops);
            plan_stage(
                &mut terminal[0],
                chunk_bytes,
                resolved.saturating_add(ops.min(chunk_bytes)) > chunk_bytes,
            )
        } else {
            let ops = terminal
                .iter()
                .map(|source| ops_bytes(&source.batch.ops))
                .fold(0, u64::saturating_add);
            (VecDeque::new(), ops)
        };
        Ok(ApplyItem::new(
            terminal,
            settled,
            stage,
            terminal_bytes,
            if chunked { chunk_bytes } else { u64::MAX },
            bytes,
        ))
    }

    /// Run exactly one writer transaction of `item`: its next Stage slice,
    /// else its next resolved-row slice, else its terminal. Returns whether
    /// more follow, the budget bytes to release (by remainder: the terminal
    /// releases whatever the item still holds) and what it wrote.
    pub fn step(
        &mut self,
        store: &mut Store,
        item: &mut ApplyItem,
        instrument: &Instrument,
    ) -> Result<(Step, u64, ApplyStats), PipelineError> {
        // Measurement only, when asked: the asset bytes the transaction
        // writes, read before its clock starts.
        let assets = if self.measure_steps {
            item.next_asset_bytes()
        } else {
            0
        };
        let started = Instant::now();
        // Measurement only: its resolved bytes and sources.
        let (mut resolved, mut sources) = (0, 1);
        let (kind, approx, stats, release, step) = if let Some((slice, approx)) =
            item.stage.pop_front()
        {
            let stats = instrument.apply_streamed_slice(store, &mut self.reconciliation, slice)?;
            (
                TxKind::Stage,
                approx,
                stats,
                item.release_share(approx),
                Step::More,
            )
        } else if item.needs_resolved_slice() {
            let chunk_bytes = item.chunk_bytes;
            let session = item
                .terminal
                .iter_mut()
                .flat_map(|source| source.resolved.iter_mut())
                .find(|session| session.rows_left())
                .expect("a resolved-row slice has rows left");
            let before = session.rows_bytes();
            let (stats, _) = instrument.apply_resolved_slice(store, session, chunk_bytes)?;
            let approx = before.saturating_sub(session.rows_bytes());
            resolved = approx;
            (
                TxKind::ResolvedRows,
                approx,
                stats,
                item.release_share(approx),
                Step::More,
            )
        } else {
            resolved = resolved_bytes(&item.terminal);
            sources = u32::try_from(item.terminal.len()).unwrap_or(u32::MAX);
            let approx = item.terminal_bytes.saturating_add(resolved);
            let stats = self.apply_terminal(store, item, instrument)?;
            (
                TxKind::Terminal,
                approx,
                stats,
                item.release_rest(),
                Step::Done,
            )
        };
        let wall_nanos = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
        match (step, item.txs) {
            (Step::More, 0) => self.items_in_progress += 1,
            (Step::Done, txs) if txs > 0 => self.items_in_progress -= 1,
            _ => {}
        }
        item.txs += 1;
        item.last = Some(StepReport {
            kind,
            approx_bytes: approx,
            asset_bytes: assets,
            resolved_bytes: resolved,
            sources,
            wall_nanos,
        });
        if kind != TxKind::Terminal {
            merge_stats(&mut self.report.stats, &stats);
        }
        self.apply_nanos = self.apply_nanos.saturating_add(wall_nanos);
        Ok((step, release, stats))
    }

    /// The item's terminal transaction, then what its commit settles: each
    /// source's committed checkpoint evidence and badge, and the trees it
    /// closed, ready once all their sources have committed.
    fn apply_terminal(
        &mut self,
        store: &mut Store,
        item: &mut ApplyItem,
        instrument: &Instrument,
    ) -> Result<ApplyStats, PipelineError> {
        let batches = std::mem::take(&mut item.terminal);
        let count = u64::try_from(batches.len()).unwrap_or(u64::MAX);
        let cursors: Vec<i64> = batches
            .iter()
            .filter_map(|source| source.batch.source.as_ref().map(|write| write.cursor_id))
            .collect();
        let stats = instrument.apply_streamed_sources(store, &mut self.reconciliation, batches)?;
        // This item's terminal merged its own staged slices (I12). Another
        // lane's chunked item of this runtime may be between its slices
        // right now — the writer interleaves keys — so only this item's
        // cursors are checked; the runtime's close checks them all.
        if let Some(cursor) = cursors
            .iter()
            .find(|cursor| self.reconciliation.is_staged(**cursor))
        {
            return Err(PipelineError::Config(format!(
                "streamed runtime {} left cursor {cursor} between its slices",
                self.runtime_id
            )));
        }
        for settled in std::mem::take(&mut item.settled) {
            // Each checkpoint is now the committed value of its cursor.
            self.evidence.insert(settled.path.clone(), settled.evidence);
            self.session_health
                .entry(settled.session_id)
                .and_modify(|healthy| *healthy &= settled.complete)
                .or_insert(settled.complete);
            if let Some(trees) = &mut self.trees {
                trees.applied(&settled.path);
                for tree in settled.closed_trees {
                    trees.closed(tree);
                }
            }
        }
        if let Some(trees) = &mut self.trees {
            trees.promote();
        }
        instrument.add(Metric::BatchesApplied, count);
        merge_stats(&mut self.report.stats, &stats);
        self.sources_applied += count;
        Ok(stats)
    }

    /// Admit the complete runtime: resolve its collected relational fold in
    /// RAM and write each remaining tool row once, canonicalize any session a
    /// cross-source repeat left out of order, then flip every source-backed
    /// session out of `Pending` in one commit — the first moment any of its
    /// rows can be read. `prepared` is what the runtime's preparer handed
    /// over once its last source was emitted.
    pub fn admit(
        self,
        store: &mut Store,
        instrument: &Instrument,
        prepared: &PreparedAdmission,
    ) -> Result<RebuildReport, PipelineError> {
        self.admit_remainder(store, instrument, prepared)
            .map(|admitted| admitted.report)
    }

    /// [`StreamedRuntime::admit`] for a runtime that may have flipped trees
    /// already: everything not flipped — bound and tainted trees, trees
    /// closed but never flipped, and every session of a runtime admitted
    /// whole — is folded, renumbered and flipped in one writer transaction,
    /// flip last. Returns the sessions whose state that commit moved, and the
    /// sessions it changed under a reader instead: trees flipped earlier
    /// whose usage the remainder's fold displaced (R1 D15).
    pub fn admit_remainder(
        mut self,
        store: &mut Store,
        instrument: &Instrument,
        prepared: &PreparedAdmission,
    ) -> Result<RemainderAdmission, PipelineError> {
        if let Some((path, _)) = self.cursors.iter().find(|(_, cursor)| !cursor.applied) {
            return Err(PipelineError::Config(format!(
                "streamed runtime {} admitted before source {path} closed",
                self.runtime_id
            )));
        }
        if self.items_in_progress != 0 || self.reconciliation.staged_slices() != 0 {
            return Err(PipelineError::Config(format!(
                "streamed runtime {} admitted while an item is between its transactions",
                self.runtime_id
            )));
        }
        let (states, unflipped) = self.remainder();
        let mut renumber = prepared.sessions_to_renumber().to_vec();
        renumber.extend(unflipped);
        let mut fold = std::mem::take(&mut self.reconciliation);
        fold.absorb_units()?;
        let admitted = instrument.admit_cold_units(store, fold, &renumber, &states)?;
        let displaced = self.flipped_among(&admitted.stats.usage_displaced_sessions);
        merge_stats(&mut self.report.stats, &admitted.stats);
        Ok(RemainderAdmission {
            report: self.report,
            changed: admitted.changed,
            displaced,
        })
    }
}

/// What a runtime's close admitted ([`StreamedRuntime::admit_remainder`]).
#[derive(Debug)]
pub struct RemainderAdmission {
    pub report: RebuildReport,
    /// The sessions whose `stream_state` the commit moved.
    pub changed: Vec<String>,
    /// Sessions flipped earlier whose usage the commit displaced: readers
    /// may hold their rows (R1 D15).
    pub displaced: Vec<String>,
}
