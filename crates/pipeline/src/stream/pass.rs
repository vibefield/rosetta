//! One pass over one JSONL source as a resumable state machine.
//!
//! [`IngestPass`] holds every local of a pass: the cursor's CAS expectation,
//! generations, the tail checkpoint, the ledger watermark, the adapter's
//! normalization context and the starter-budget arithmetic. [`IngestPass::begin`]
//! sets it up from the cursor row, [`IngestPass::step`] reads and normalizes
//! one tail batch into exactly what the pass hands the raw ledger and then the
//! planner ([`PassOutput`]), and [`IngestPass::committed`] advances it once the
//! planner accepted that batch. `step` touches neither the planner nor the
//! ledger, so everything upstream of the first ledger call can run away from
//! the thread that owns them; [`super::ingest_file`] is the serial driver.

use std::path::{Path, PathBuf};

use rosetta_adapter_support::{AgentAdapter, LineStatus, NormalizeCtx};
use rosetta_capture::{
    CommittedVerification, TailCheckpoint, TailConfig, TailItem, TailRead, Tailer, Transition,
};
use rosetta_core::IngestOp;
use rosetta_core::fold::CumulativeUsage;
use rosetta_core::rows::{DriftFact, RuntimeRow};
use rosetta_store::{Batch, CheckpointUpdate, SourceMode, SourceWrite, Store, StoreError, queries};

use super::{FilePass, PassMode, StreamReport, merge_stats};
use crate::checkpoint::PipeCheckpoint;
use crate::instrument::{Instrument, Metric};
use crate::meta::SessionFile;
use crate::{IngestOptions, PipelineError, SliceBudget};

/// What one [`IngestPass::step`] produced.
pub(crate) enum PassStep {
    /// One batch for the raw ledger, then the planner, in that order.
    Batch(PassOutput),
    /// The pass ended without another batch.
    Finished(FilePass, ReportDelta),
}

/// Exactly what a pass hands the raw ledger and the planner for one batch.
pub(crate) struct PassOutput {
    pub(crate) raw: RawOut,
    /// The ops, the CAS checkpoint update and the source write.
    pub(crate) batch: Batch,
    pub(crate) deltas: ReportDelta,
}

/// One batch's raw ledger work: its records not yet ledgered, then an
/// optional seal of the generation.
pub(crate) struct RawOut {
    pub(crate) generation: u64,
    pub(crate) records: Vec<(u64, Vec<u8>)>,
    /// `Some(generation_clean)` when this batch completes a raw-complete
    /// generation that is not already active.
    pub(crate) seal: Option<bool>,
}

/// The [`StreamReport`] counters one step moves, merged by the driver.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ReportDelta {
    pub(crate) files_missing: usize,
    pub(crate) retries_exhausted: u64,
    pub(crate) generations_restarted: u64,
    pub(crate) records: u64,
    pub(crate) payload_bytes: u64,
    pub(crate) parsed: u64,
    pub(crate) unknown: u64,
    pub(crate) hard_errors: u64,
    pub(crate) oversized: u64,
}

impl ReportDelta {
    pub(crate) fn merge_into(&self, report: &mut StreamReport) {
        report.files_missing += self.files_missing;
        report.retries_exhausted += self.retries_exhausted;
        report.generations_restarted += self.generations_restarted;
        report.records += self.records;
        report.payload_bytes += self.payload_bytes;
        report.parsed += self.parsed;
        report.unknown += self.unknown;
        report.hard_errors += self.hard_errors;
        report.oversized += self.oversized;
    }
}

/// What [`IngestPass::committed`] advances to once the planner accepts the
/// batch [`IngestPass::step`] produced.
struct Staged {
    next_blob: String,
    mode: SourceMode,
    tail: TailCheckpoint,
    budget_stop: bool,
    more_available: bool,
}

pub(crate) struct IngestPass {
    path: PathBuf,
    cursor_id: i64,
    /// The planner's exact stored checkpoint: the next batch's CAS expectation.
    prev_blob: String,
    active_generation: u64,
    highest_generation: u64,
    tail: Option<TailCheckpoint>,
    /// Records below this offset of the current generation are already in
    /// the raw ledger.
    ledgered_through: u64,
    ctx: NormalizeCtx,
    source_generation: u64,
    next_source_sequence: u64,
    generation_clean: bool,
    generation_raw_complete: bool,
    budget: Option<SliceBudget>,
    budget_tailer: Option<Tailer>,
    budget_bytes: u64,
    budget_records: u64,
    /// [`IngestOptions::stop_at_partial_record`].
    stop_at_partial_record: bool,
    retries: u32,
    force_replay: bool,
    staged: Option<Staged>,
}

impl IngestPass {
    /// Set a pass up from the file's cursor row. Pure: no planner, no ledger,
    /// no source IO.
    pub(crate) fn begin(
        adapter: &dyn AgentAdapter,
        runtime: &RuntimeRow,
        root: &Path,
        file: &SessionFile,
        opts: &IngestOptions,
        cursor: queries::CursorState,
        mode: PassMode,
    ) -> Result<Self, PipelineError> {
        if cursor.session_id != file.projected_session_id() {
            return Err(PipelineError::Config(format!(
                "source {} moved from projected session {:?} to {}",
                file.relative_path, cursor.session_id, file.session_id
            )));
        }
        let cursor_id = cursor.id;
        let prev_blob = cursor.checkpoint;
        let active_generation = cursor.active_generation;
        let highest_generation = cursor.highest_generation;

        // An expired blob (undecodable, foreign version, or a cursor row that
        // belongs to a different session id) means full replay from byte zero —
        // deterministic ids make the replay idempotent.
        let resumed = PipeCheckpoint::decode(&prev_blob)
            .filter(|checkpoint| {
                checkpoint.session_id == file.session_id
                    && checkpoint.stream_id == file.stream_id
                    && checkpoint.normalizer_version == adapter.normalizer_version()
                    // An unactivated generation may have process-local tool join
                    // state. After restart, replay it from byte zero instead of
                    // pretending SQLite was its reconciliation journal.
                    && checkpoint.source_generation == active_generation
            })
            .and_then(|checkpoint| {
                adapter
                    .restore_state(checkpoint.adapter_state.as_ref())
                    .ok()
                    .map(|state| (checkpoint, state))
            });
        let tail = resumed
            .as_ref()
            .and_then(|(checkpoint, _)| checkpoint.tail_checkpoint());
        // Ledger-first means everything at or below the resumed committed offset
        // is already durable in the raw ledger for this generation. An audit (or
        // sibling-heal) replay re-frames those records to verify the projection,
        // but appending them again would grow the append-only ledger without
        // bound — one full file copy per audit turn. Records below the watermark
        // stay out of the ledger; a generation restart resets it, because a
        // replacement generation's content is all new.
        let ledgered_through = tail
            .as_ref()
            .map_or(0, |checkpoint| checkpoint.committed_offset);

        let mut ctx = NormalizeCtx::new(runtime.id.as_str(), file.session_id.as_str());
        ctx.native_session_id = Some(file.native_session_id.clone());
        ctx.agent_version = file.agent_version.clone();
        ctx.state = match &resumed {
            Some((_, _)) => None,
            None => adapter.restore_state(None).map_err(PipelineError::Config)?,
        };
        ctx.src_cursor = Some(cursor_id);
        ctx.source_identity = Some(file.relative_path.clone());
        let mut source_generation = highest_generation
            .checked_add(1)
            .ok_or_else(|| PipelineError::Config("source generation overflowed".to_string()))?;
        let mut next_source_sequence = 0_u64;
        let mut generation_clean = true;
        if let Some((checkpoint, state)) = resumed {
            ctx.next_ordinal = checkpoint.next_ordinal;
            if let Some(last) = checkpoint.cumulative {
                ctx.cumulative = CumulativeUsage::resume(last);
            }
            ctx.state = state;
            if checkpoint.source_generation > 0 {
                source_generation = checkpoint.source_generation;
            }
            next_source_sequence = checkpoint.next_source_sequence;
            generation_clean = checkpoint.generation_clean;
        }

        // A starter budget applies only where pausing cannot regress what a
        // reader already sees: a fresh source's first generation (early Activate
        // turns the unread remainder into ordinary growth) or append growth of
        // the already-active generation. A replacement generation ignores it so
        // the previous complete content stays visible until the full swap.
        let budget = opts
            .starter_budget
            .filter(|_| active_generation == 0 || source_generation == active_generation);
        let budget_tailer = match budget {
            Some(slice) => Some(Tailer::new(TailConfig {
                // A soft per-batch byte cap sizes budgeted batches to the slice
                // itself — one transaction per slice in the common case — while
                // the ordinary record and hard-byte bounds stay untouched, so a
                // large valid record still frames instead of going oversized.
                soft_batch_bytes: Some(
                    usize::try_from(slice.max_bytes)
                        .unwrap_or(usize::MAX)
                        .max(1),
                ),
                ..opts.tail.clone()
            })?),
            None => None,
        };

        let force_replay = matches!(mode, PassMode::ForceHeal)
            && tail.is_some()
            && source_generation == active_generation;
        Ok(Self {
            path: root.join(&file.relative_path),
            cursor_id,
            prev_blob,
            active_generation,
            highest_generation,
            tail,
            ledgered_through,
            ctx,
            source_generation,
            next_source_sequence,
            generation_clean,
            generation_raw_complete: true,
            budget,
            budget_tailer,
            budget_bytes: 0,
            budget_records: 0,
            stop_at_partial_record: opts.stop_at_partial_record,
            retries: 0,
            force_replay,
            staged: None,
        })
    }

    /// The rotating audit: one sequential read of the committed range against
    /// the checkpoint's content chain. Verified — the common case, a quiet
    /// file — costs no generation, no ledger append, and no projection write;
    /// a mismatch is real content change hiding under a quiet stat, and THAT
    /// escalates to the full replacement replay. A pre-chain checkpoint has
    /// its chain computed once and persisted, so the next audit verifies.
    pub(crate) fn audit(
        &mut self,
        runtime: &RuntimeRow,
        file: &SessionFile,
        tailer: &Tailer,
        store: &mut Store,
        instrument: &Instrument,
        report: &mut StreamReport,
    ) -> Result<(), PipelineError> {
        if self.source_generation != self.active_generation {
            return Ok(());
        }
        let Some(tail_checkpoint) = self.tail.as_mut() else {
            return Ok(());
        };
        match tailer.verify_committed(&self.path, tail_checkpoint)? {
            CommittedVerification::Verified => {
                instrument.add(Metric::BytesRead, tail_checkpoint.committed_offset);
            }
            CommittedVerification::Mismatch => {
                instrument.add(Metric::BytesRead, tail_checkpoint.committed_offset);
                self.force_replay = true;
            }
            CommittedVerification::Established(chain) => {
                instrument.add(Metric::BytesRead, tail_checkpoint.committed_offset);
                tail_checkpoint.committed_chain = Some(chain);
                if let Some(mut previous) = PipeCheckpoint::decode(&self.prev_blob) {
                    previous.tail = Some(tail_checkpoint.encode_hex());
                    let next_blob = previous.encode();
                    let applied = instrument.apply(
                        store,
                        &Batch {
                            ops: vec![],
                            checkpoints: vec![CheckpointUpdate {
                                runtime_id: runtime.id.clone(),
                                stream: file.stream_id.clone(),
                                path: file.relative_path.clone(),
                                session_id: file.projected_session_id(),
                                expected: Some(self.prev_blob.clone()),
                                next: next_blob.clone(),
                                updated_at: Some(file.modified_ms),
                            }],
                            source: None,
                        },
                    );
                    match applied {
                        Ok(stats) => {
                            merge_stats(&mut report.stats, &stats);
                            report.batches += 1;
                            self.prev_blob = next_blob;
                        }
                        Err(StoreError::CheckpointConflict { .. }) => report.conflicts += 1,
                        Err(error) => return Err(error.into()),
                    }
                }
            }
            // The ordinary read below reclassifies these and recovers.
            CommittedVerification::Missing
            | CommittedVerification::IdentityChanged
            | CommittedVerification::Truncated => {}
        }
        Ok(())
    }

    /// Read and normalize one tail batch. A retryable read is retried here,
    /// so one step is one batch or the end of the pass.
    pub(crate) fn step(
        &mut self,
        adapter: &dyn AgentAdapter,
        runtime: &RuntimeRow,
        file: &SessionFile,
        tailer: &Tailer,
        opts: &IngestOptions,
        instrument: &Instrument,
    ) -> Result<PassStep, PipelineError> {
        debug_assert!(
            self.staged.is_none(),
            "a pass steps again only after its batch committed"
        );
        let mut deltas = ReportDelta::default();
        let mut batch = loop {
            opts.check_cancelled()?;
            // Chosen per read: a mid-pass rewrite that disables the budget
            // must also stop paying its batch shape for the replacement replay.
            let read_tailer = match (&self.budget, &self.budget_tailer) {
                (Some(_), Some(budgeted)) => budgeted,
                _ => tailer,
            };
            let read = instrument.time(Metric::ReadNanos, || {
                read_tailer.read(&self.path, self.tail.as_ref(), self.force_replay)
            })?;
            match read {
                TailRead::Missing => {
                    deltas.files_missing += 1;
                    return Ok(PassStep::Finished(FilePass::Finished(false), deltas));
                }
                TailRead::Retry => {
                    self.retries += 1;
                    if self.retries > opts.max_retries {
                        deltas.retries_exhausted += 1;
                        return Ok(PassStep::Finished(FilePass::Finished(false), deltas));
                    }
                }
                TailRead::Batch(batch) => break batch,
            }
        };
        self.force_replay = false;
        instrument.add(Metric::BytesRead, batch.bytes_read);

        if batch.transition.starts_new_generation()
            && !matches!(batch.transition, Transition::Initial)
        {
            if self.active_generation > 0 {
                // A rewrite mid-pass is a replacement generation: pausing it
                // early would hide previously complete content, so the
                // budget stops applying.
                self.budget = None;
            }
            // The replacement generation starts its own budget arithmetic;
            // carrying the abandoned generation's counts could pause it
            // below the documented record floor.
            self.budget_bytes = 0;
            self.budget_records = 0;
            deltas.generations_restarted += 1;
            self.source_generation = self
                .source_generation
                .max(self.active_generation)
                .max(self.highest_generation)
                .checked_add(1)
                .ok_or_else(|| PipelineError::Config("source generation overflowed".to_string()))?;
            self.highest_generation = self.source_generation;
            self.next_source_sequence = 0;
            self.ledgered_through = 0;
            self.generation_clean = true;
            self.generation_raw_complete = true;
            self.ctx.next_ordinal = 0;
            self.ctx.cumulative = CumulativeUsage::new();
            self.ctx.state = adapter.restore_state(None).map_err(PipelineError::Config)?;
        }

        let mut ops: Vec<IngestOp> = Vec::with_capacity(batch.items.len());
        for item in &batch.items {
            opts.check_cancelled()?;
            match item {
                TailItem::Record(record) => {
                    deltas.records += 1;
                    deltas.payload_bytes += record.payload.len() as u64;
                    self.budget_records += 1;
                    // The budget counts FRAMED PAYLOAD, mirroring the soft
                    // batch cap — physical amplification (anchor and
                    // continuity re-reads) must not spend the slice.
                    self.budget_bytes = self
                        .budget_bytes
                        .saturating_add(record.payload.len() as u64);
                    instrument.add(Metric::RecordsFramed, 1);
                    self.ctx.src_offset = Some(record.start_offset as i64);
                    self.ctx.source_generation = self.source_generation;
                    let outcome = instrument.time(Metric::NormalizeNanos, || {
                        adapter
                            .ingest_line(&mut self.ctx, &file.stream_id, &record.payload)
                            .into_projection()
                    });
                    match &outcome.status {
                        LineStatus::Parsed => {
                            deltas.parsed += 1;
                            instrument.add(Metric::LinesParsed, 1);
                        }
                        LineStatus::UnknownType => {
                            deltas.unknown += 1;
                            instrument.add(Metric::LinesUnknown, 1);
                        }
                        LineStatus::HardError { .. } => {
                            self.generation_clean = false;
                            deltas.hard_errors += 1;
                            instrument.add(Metric::LinesHardError, 1);
                        }
                    }
                    instrument.observe_normalization(
                        &file.stream_id,
                        self.ctx.src_offset,
                        &record.payload,
                        &outcome,
                    );
                    ops.extend(outcome.ops);
                }
                TailItem::Oversized { start_offset, .. } => {
                    self.generation_clean = false;
                    self.generation_raw_complete = false;
                    deltas.oversized += 1;
                    instrument.add(Metric::RecordsOversized, 1);
                    ops.push(IngestOp::Drift(DriftFact {
                        runtime_id: runtime.id.clone(),
                        agent_version: file.agent_version.clone().unwrap_or_default(),
                        stream: file.stream_id.clone(),
                        record_kind: "raw_record".to_string(),
                        error_code: "oversized".to_string(),
                        shape_hash: String::new(),
                        at: None,
                        sample_cursor: Some(self.cursor_id),
                        sample_offset: Some(*start_offset as i64),
                    }));
                }
            }
        }

        instrument.add(Metric::PayloadBytes, deltas.payload_bytes);
        if let Some(progress) = &opts.progress {
            progress.source_bytes(file, deltas.payload_bytes);
        }
        let last_reading = self.ctx.cumulative.last_reading().clone();
        let adapter_state = adapter
            .checkpoint_state(self.ctx.state.as_deref())
            .map_err(PipelineError::Config)?;
        let first_sequence = self.next_source_sequence;
        self.next_source_sequence = self
            .next_source_sequence
            .checked_add(ops.len() as u64)
            .ok_or_else(|| PipelineError::Config("source sequence overflowed".to_string()))?;
        let complete = !batch.more_available && batch.checkpoint.incomplete_suffix_len == 0;
        // The partial-record stop: all that is left is a record its writer
        // has not finished, and this pass must end its generation anyway
        // ([`IngestOptions::stop_at_partial_record`]). Every rule below reads
        // it as the end of the file — the generation ends at its last
        // complete record exactly as if the file ended there — while the
        // checkpoint keeps the unterminated suffix unconsumed, so the finished
        // record later arrives as ordinary append growth. A replacement
        // generation ends here too: unlike a budget pause it has read every
        // complete record the file holds, and the pass that sets this option
        // has published no generation of the source for it to hide.
        let partial_stop = self.stop_at_partial_record && !batch.more_available && !complete;
        let ended = complete || partial_stop;
        // The slice boundary: enough clean bytes and complete records are
        // framed that this pass may stop. A fresh generation stops by
        // ACTIVATING and sealing its prefix, so the unread remainder becomes
        // ordinary append growth — indistinguishable from the file having
        // ended here and then grown.
        let budget_stop = !complete
            && self.generation_clean
            && self.generation_raw_complete
            && self.budget.is_some_and(|slice| {
                self.budget_bytes >= slice.max_bytes
                    && self.budget_records >= u64::from(slice.min_records)
            });
        let source_mode = if self.source_generation == self.active_generation {
            SourceMode::Append
        } else if (ended || budget_stop) && self.generation_clean {
            SourceMode::Activate
        } else if ended && self.active_generation == 0 && self.generation_raw_complete {
            // The live-growth rule, converged (product decision 2026-08-31):
            // a FIRST generation that reaches EOF unclean still activates its
            // parseable rows and the session holds Streaming — exactly what
            // the sliced path and live growth already produce. A dirty
            // REPLACEMENT still abandons so previously clean content
            // survives, and a raw-incomplete (oversized) generation still
            // abandons because an unsealable generation must never activate.
            SourceMode::Activate
        } else if ended {
            SourceMode::Abandon
        } else {
            SourceMode::Stage
        };
        let next_blob = PipeCheckpoint {
            v: crate::checkpoint::PIPE_CHECKPOINT_VERSION,
            session_id: file.session_id.clone(),
            stream_id: file.stream_id.clone(),
            normalizer_version: adapter.normalizer_version(),
            source_generation: self.source_generation,
            next_source_sequence: self.next_source_sequence,
            generation_clean: self.generation_clean,
            next_ordinal: self.ctx.next_ordinal,
            tail: Some(batch.checkpoint.encode_hex()),
            cumulative: (!last_reading.is_empty()).then_some(last_reading),
            adapter_state,
            snapshot_stamp: None,
            snapshot_revision: None,
        }
        .encode();

        // Nothing new and nothing to move the cursor to: don't write an
        // empty transaction on every re-scan of a quiet file.
        if ops.is_empty() && next_blob == self.prev_blob && !batch.more_available {
            return Ok(PassStep::Finished(self.finished(), deltas));
        }

        // Oversized records are quarantined by the tailer and carry no
        // payload, so only ordinary records reach the ledger.
        let records: Vec<(u64, Vec<u8>)> = batch
            .items
            .iter_mut()
            .filter_map(|item| match item {
                TailItem::Record(record) if record.start_offset >= self.ledgered_through => {
                    Some((record.start_offset, std::mem::take(&mut record.payload)))
                }
                TailItem::Record(_) | TailItem::Oversized { .. } => None,
            })
            .collect();
        let seal = (self.source_generation != self.active_generation
            && (ended || budget_stop)
            && self.generation_raw_complete)
            .then_some(self.generation_clean);
        let write = Batch {
            ops,
            checkpoints: vec![CheckpointUpdate {
                runtime_id: runtime.id.clone(),
                stream: file.stream_id.clone(),
                path: file.relative_path.clone(),
                session_id: file.projected_session_id(),
                expected: Some(self.prev_blob.clone()),
                next: next_blob.clone(),
                updated_at: Some(file.modified_ms),
            }],
            source: Some(SourceWrite {
                cursor_id: self.cursor_id,
                session_id: file.session_id.clone(),
                generation: self.source_generation,
                first_sequence,
                mode: source_mode,
            }),
        };
        self.staged = Some(Staged {
            next_blob,
            mode: source_mode,
            tail: batch.checkpoint,
            budget_stop,
            more_available: batch.more_available,
        });
        Ok(PassStep::Batch(PassOutput {
            raw: RawOut {
                generation: self.source_generation,
                records,
                seal,
            },
            batch: write,
            deltas,
        }))
    }

    /// Advance past the batch [`IngestPass::step`] produced, once the planner
    /// accepted it. `Some` ends the pass.
    pub(crate) fn committed(&mut self) -> Option<FilePass> {
        let staged = self
            .staged
            .take()
            .expect("committed follows a step that produced a batch");
        self.prev_blob = staged.next_blob;
        if staged.mode == SourceMode::Activate {
            self.active_generation = self.source_generation;
        }
        self.highest_generation = self.highest_generation.max(self.source_generation);
        self.tail = Some(staged.tail);
        if staged.budget_stop {
            // The prefix is active and its checkpoint durable; the rest of
            // this file is a later completion pass's work.
            return Some(FilePass::Paused);
        }
        if !staged.more_available {
            // An incomplete trailing record stays unconsumed until the file
            // grows; stepping again would re-frame the same bytes forever.
            return Some(self.finished());
        }
        None
    }

    fn finished(&self) -> FilePass {
        let active_clean =
            self.generation_clean && self.active_generation == self.source_generation;
        let suffix = self
            .tail
            .as_ref()
            .map(|checkpoint| checkpoint.incomplete_suffix_len);
        if self.stop_at_partial_record && active_clean && suffix.is_some_and(|len| len > 0) {
            // Stopped before a record its writer has not finished: the clean
            // prefix is active, and the record is append growth for a later
            // pass. Deliberate, like a budget pause — never a failure.
            return FilePass::Paused;
        }
        FilePass::Finished(active_clean && suffix == Some(0))
    }
}
