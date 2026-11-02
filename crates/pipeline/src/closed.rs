//! Closed sources for the streamed fresh-cold build.
//!
//! The capture drivers run unchanged against a private planning store: the
//! cursor CAS chain, generations, raw-ledger order, and restart detection are
//! the same control plane [`crate::stream_backfill_into`] drives. What changes
//! is retention. Instead of holding every normalized batch until the runtime
//! closes, the producer walks the roster and hands each source to the writer
//! as a closed source (terminal generation, final checkpoint, validated
//! contiguous ops) as soon as nothing can supersede it. Only a JSONL
//! transcript whose session still has an uncaptured transcript is held back:
//! a later restart of that session would force-replay its siblings into
//! replacement generations, and the writer only ever sees one terminal
//! generation per source.
//!
//! A runtime admitted whole walks in source-path order — the order the sole
//! canonical writer applies its sources in, so every order-sensitive
//! canonical upsert resolves exactly as it would for the whole runtime at
//! once. A runtime admitted per root tree walks its newest trees first, in
//! lanes ([`walk_order`]), each lane emitting its own closed sources
//! ([`ColdEmit`]).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use rosetta_adapter_support::AgentAdapter;
use rosetta_capture::SourceDef;
use rosetta_core::IngestOp;
use rosetta_core::rows::RuntimeRow;
use rosetta_store::{Batch, SourceMode, Store, queries};

use crate::capture::{CapturePool, LaneClass};
use crate::checkpoint::PipeCheckpoint;
use crate::instrument::{Instrument, Metric};
use crate::live::PollEvidence;
use crate::meta::SessionFile;
use crate::stream::{FilePass, StreamReport, fresh_cursor};
use crate::{IngestOptions, PipelineError};

mod emit;
mod frontier;
mod lane_profile;
mod setup;
pub mod trees;
mod walk;
pub mod walk_order;
pub use emit::{BlockingEmit, ColdEmit, EmitOutcome};
pub use lane_profile::{LaneProfile, Samples, WalkProfile};
pub use setup::{StreamedSetup, streamed_setup, streamed_setup_for};
pub use trees::{SessionLink, Tree, TreeId, TreeMap};
pub use walk_order::{
    BIG_LANES, BIG_SOURCE_BYTES, ColdWalkOptions, LaneId, WalkOrder, WalkSource, WalkSummary,
    walk_summary,
};
#[doc(hidden)]
pub use walk_order::{ProbeCounters, ProbeView, WalkOverride, WalkProbe};

use walk_order::{path_ordered, walk_plan_for};

/// One source whose generation is terminal: exactly what
/// [`crate::prepare_captured_runtime`] computes per file, ready for the
/// canonical writer. Process-local and disposable; the raw ledger remains the
/// recovery source.
#[derive(Debug)]
pub struct ClosedSource {
    pub(crate) source: SourceDef,
    pub(crate) generation: u64,
    pub(crate) ops: Vec<IngestOp>,
    pub(crate) checkpoint: String,
    /// What the live stat poll reads from `checkpoint`, taken from the decode
    /// that validated it, so the writer can hand it on without decoding.
    pub(crate) evidence: PollEvidence,
    pub(crate) activate: bool,
    /// The pass's own verdict: a clean generation, fully active, with nothing
    /// of the file left unread. Anything less — an unclean generation, a
    /// record its writer has not finished, a file that vanished or kept
    /// changing — holds the session `Streaming`, as a live pass would.
    pub(crate) complete: bool,
    pub(crate) payload_bytes: u64,
    /// What [`crate::StreamedPreparer`] did to the source as it was emitted;
    /// the writer applies only prepared sources.
    pub(crate) prepared: Option<crate::early::Prepared>,
    /// The walk span that emitted it ([`LaneId::Path`] on the path walk).
    pub(crate) lane: LaneId,
}

impl ClosedSource {
    pub fn relative_path(&self) -> &str {
        &self.source.relative_path
    }

    /// The public session this source belongs to; `None` for a ledger-only
    /// source, which admits none.
    pub fn session_id(&self) -> Option<&str> {
        (!self.source.ledger_only).then_some(self.source.session_id.as_str())
    }

    /// The walk span that emitted it.
    pub fn lane(&self) -> LaneId {
        self.lane
    }

    /// The source's ops as captured (before the preparer withholds any):
    /// the integration harness's closure audit reads them.
    #[doc(hidden)]
    pub fn ops(&self) -> &[IngestOp] {
        &self.ops
    }

    /// The writer-budget unit of this source (R1 D22): its
    /// [`ClosedSource::payload_bytes`] plus the approx bytes of the facts
    /// withheld from earlier emissions that it carries to the writer.
    pub fn budget_bytes(&self) -> u64 {
        self.payload_bytes().saturating_add(
            self.prepared
                .as_ref()
                .map_or(0, |prepared| prepared.carried_bytes),
        )
    }

    /// A synthetic closed source for writer-scheduling tests: an activated,
    /// unprepared generation of `session_id` at `path` holding `ops`, whose
    /// framed payload measured `payload_bytes`. Never reaches a store.
    #[doc(hidden)]
    pub fn synthetic(
        path: &str,
        session_id: &str,
        ops: Vec<IngestOp>,
        payload_bytes: u64,
        lane: LaneId,
    ) -> Self {
        Self {
            source: SourceDef {
                ledger_only: false,
                relative_path: path.to_owned(),
                stream_id: "transcript".to_owned(),
                session_id: session_id.to_owned(),
                native_session_id: session_id.to_owned(),
                agent_version: None,
            },
            generation: 1,
            ops,
            checkpoint: String::new(),
            evidence: PollEvidence::of(None),
            activate: true,
            complete: true,
            payload_bytes,
            prepared: None,
            lane,
        }
    }

    /// Whether the source's terminal abandons its generation (a skipped
    /// source does not).
    pub fn abandons(&self) -> bool {
        !self.activate && !self.skipped()
    }

    /// Framed payload bytes this source consumed, assets included.
    pub fn payload_bytes(&self) -> u64 {
        self.payload_bytes.saturating_add(
            self.ops
                .iter()
                .filter_map(|op| match op {
                    IngestOp::ProjectionAsset { asset } => {
                        Some(asset.materialized_byte_len() as u64)
                    }
                    _ => None,
                })
                .sum::<u64>(),
        )
    }

    /// Nothing of the source could be read — it vanished, or kept changing
    /// under every retry — so it has no generation: the writer applies no
    /// batch for it and its cursor keeps the fresh checkpoint, which the live
    /// runner reads it from if it comes back.
    pub(crate) fn skipped(&self) -> bool {
        self.generation == 0
    }
}

pub(crate) fn source_def(file: &SessionFile) -> SourceDef {
    SourceDef {
        ledger_only: file.ledger_only,
        relative_path: file.relative_path.clone(),
        stream_id: file.stream_id.clone(),
        session_id: file.session_id.clone(),
        native_session_id: file.native_session_id.clone(),
        agent_version: file.agent_version.clone(),
    }
}

/// Validate one source's retained planner batches exactly like
/// [`crate::prepare_captured_runtime`]: a started, terminal generation with
/// contiguous operation sequences and a decodable final checkpoint of this
/// adapter's normalizer. Batches of a superseded generation are dropped. A
/// source its pass could not read at all (`lost`) closes skipped instead;
/// `complete` is that pass's verdict.
fn close_file(
    adapter: &dyn AgentAdapter,
    file: &SessionFile,
    cursor: queries::CursorState,
    batches: Vec<Batch>,
    payload_bytes: u64,
    complete: bool,
    lost: bool,
) -> Result<ClosedSource, PipelineError> {
    if cursor.highest_generation == 0 {
        if lost && batches.is_empty() {
            return Ok(ClosedSource {
                source: source_def(file),
                generation: 0,
                ops: Vec::new(),
                evidence: PollEvidence::of(PipeCheckpoint::decode(&cursor.checkpoint).as_ref()),
                checkpoint: cursor.checkpoint,
                activate: false,
                complete: false,
                payload_bytes,
                prepared: None,
                lane: LaneId::Path,
            });
        }
        return Err(PipelineError::Config(format!(
            "cold source {} never started a generation",
            file.relative_path
        )));
    }
    let generation = cursor.highest_generation;
    let mut ops: Vec<IngestOp> = Vec::new();
    let mut last_mode = None;
    for batch in batches {
        let source_write = batch
            .source
            .as_ref()
            .expect("closed-source batches are grouped by their source");
        if source_write.session_id != file.session_id {
            return Err(PipelineError::Config(format!(
                "cold source {} moved from session {} to {}",
                file.relative_path, file.session_id, source_write.session_id
            )));
        }
        if source_write.generation < generation {
            // A rewrite observed during capture superseded this incomplete
            // generation; the planner swept it, and so does the closed source.
            continue;
        }
        if source_write.generation > generation {
            return Err(PipelineError::Config(format!(
                "cold source {} retained generation {} above planner watermark {}",
                file.relative_path, source_write.generation, generation
            )));
        }
        let expected = u64::try_from(ops.len()).map_err(|_| {
            PipelineError::Config(format!(
                "cold source {} operation count exceeds u64",
                file.relative_path
            ))
        })?;
        if source_write.first_sequence != expected {
            return Err(PipelineError::Config(format!(
                "cold source {} has non-contiguous operation sequence {} after {}",
                file.relative_path, source_write.first_sequence, expected
            )));
        }
        last_mode = Some(source_write.mode);
        ops.extend(batch.ops);
    }
    let terminal = matches!(last_mode, Some(SourceMode::Activate | SourceMode::Abandon))
        || (cursor.active_generation == generation
            && matches!(last_mode, Some(SourceMode::Append)));
    if !terminal {
        return Err(PipelineError::Config(format!(
            "cold source {} stopped without a terminal generation",
            file.relative_path
        )));
    }
    let checkpoint = PipeCheckpoint::decode(&cursor.checkpoint).ok_or_else(|| {
        PipelineError::Config(format!(
            "cold source {} ended with an undecodable checkpoint",
            file.relative_path
        ))
    })?;
    if checkpoint.source_generation != generation {
        return Err(PipelineError::Config(format!(
            "cold source {} checkpoint generation {} differs from control watermark {}",
            file.relative_path, checkpoint.source_generation, generation
        )));
    }
    if checkpoint.normalizer_version != adapter.normalizer_version() {
        return Err(PipelineError::Config(format!(
            "cold source {} checkpoint normalizer {} differs from adapter {}",
            file.relative_path,
            checkpoint.normalizer_version,
            adapter.normalizer_version()
        )));
    }
    Ok(ClosedSource {
        source: source_def(file),
        generation,
        ops,
        evidence: PollEvidence::of(Some(&checkpoint)),
        checkpoint: cursor.checkpoint,
        activate: cursor.active_generation == generation,
        complete,
        payload_bytes,
        prepared: None,
        lane: LaneId::Path,
    })
}

/// Close every source of one capture unit from the batches the planner
/// retained for it, with each one's health as its pass reported it and
/// whether the unit's source could not be read at all — both from the unit's
/// own report, never another unit's. Each member's batches are drained by
/// its cursor; afterwards, anything still retained must belong to `open`,
/// the cursors of other lanes' sources mid-capture, or the planner broke an
/// invariant.
#[allow(clippy::too_many_arguments)]
fn close_unit(
    adapter: &dyn AgentAdapter,
    runtime: &RuntimeRow,
    planner: &mut Store,
    members: &[&SessionFile],
    payload_bytes: u64,
    file_health: &BTreeMap<String, bool>,
    lost: bool,
    open: &BTreeSet<i64>,
    instrument: &Instrument,
) -> Result<Vec<ClosedSource>, PipelineError> {
    let started = Instant::now();
    let count = u64::try_from(members.len().max(1)).unwrap_or(u64::MAX);
    // Every member's cursor, then one drain for all of them: a physical
    // group's members close in one pass over what the planner retained. A
    // lost cursor still fails at its member's turn, as when every batch was
    // drained up front.
    let mut cursors = Vec::with_capacity(members.len());
    for file in members {
        cursors.push(
            queries::cursor_state(planner.conn(), &runtime.id, &file.relative_path)?.ok_or_else(
                || {
                    PipelineError::Config(format!(
                        "cold planner lost cursor for {}",
                        file.relative_path
                    ))
                },
            ),
        );
    }
    let ids: Vec<i64> = cursors
        .iter()
        .filter_map(|cursor| cursor.as_ref().ok().map(|cursor| cursor.id))
        .collect();
    let mut drained = planner.drain_planned_batches_for_cursors(&ids)?.into_iter();
    let mut closed = Vec::with_capacity(members.len());
    for (file, cursor) in members.iter().zip(cursors) {
        let cursor = cursor?;
        let batches = drained.next().unwrap_or_default();
        closed.push(close_file(
            adapter,
            file,
            cursor,
            batches,
            payload_bytes.div_ceil(count),
            file_health
                .get(&file.relative_path)
                .copied()
                .unwrap_or(false),
            lost,
        )?);
    }
    if let Some(cursor_id) = planner
        .planned_batch_cursors()?
        .into_iter()
        .find(|cursor| !open.contains(cursor))
    {
        return Err(PipelineError::Config(format!(
            "cold planner retained unknown cursor {cursor_id}"
        )));
    }
    instrument.add(
        Metric::ColdPrepareNanos,
        u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX),
    );
    Ok(closed)
}

fn record_outcome(
    outcome: FilePass,
    file: &SessionFile,
    report: &mut StreamReport,
    file_health: &mut BTreeMap<String, bool>,
    session_health: &mut BTreeMap<String, bool>,
) {
    let complete = outcome == FilePass::Finished(true);
    file_health.insert(file.relative_path.clone(), complete);
    session_health
        .entry(file.session_id.clone())
        .and_modify(|healthy| *healthy &= complete);
    match outcome {
        FilePass::Paused => report.files_paused += 1,
        FilePass::Finished(true) => {}
        FilePass::Finished(false) => report.files_incomplete += 1,
    }
}

/// Sources a pass could not read: gone when opened, or changing under every
/// retry.
fn lost_sources(report: &StreamReport) -> u64 {
    (report.files_missing as u64).saturating_add(report.retries_exhausted)
}

fn virtual_parent(file: &SessionFile) -> Result<&str, PipelineError> {
    file.relative_path
        .split_once('#')
        .map(|(physical, _)| physical)
        .ok_or_else(|| {
            PipelineError::Config(format!(
                "snapshot source {} has no #session suffix",
                file.relative_path
            ))
        })
}

/// [`crate::stream_backfill_into`] for the streamed fresh-cold build: the
/// same capture drivers and planner control plane, walked by `trees`' plan
/// ([`walk_order`]), with each source handed to `emit` as a [`ClosedSource`]
/// once it can no longer be superseded. `emit` receives every roster source
/// exactly once: in path order for a runtime admitted whole (`trees` is
/// `None`), else in each lane's order, every emission blocking — this is the
/// serial driver, which walks the plan's global order. The planner must be a
/// cold-capture planner whose metadata batches were already taken by
/// [`streamed_setup`].
///
/// Every roster source's cursor is created before the walk, in one planner
/// transaction and in path order — the order the path walk would have
/// created them one at a time, so each cursor gets the rowid it always had,
/// whatever the walk. The ledger's file id for each source is its index in
/// that order (its roster key), in every segment.
///
/// Every pass of the walk runs as part of a private build
/// ([`IngestOptions::private_build`]): a source whose writer is mid-append,
/// or that vanishes or keeps changing after part of it was read, still ends at
/// a terminal generation, and an unclean one activates its parseable rows. A
/// source that cannot be read at all is emitted skipped.
///
/// `trees` is the runtime's [`StreamedSetup::trees`]: `Some` for a runtime
/// admitted per root tree, whose transcripts are then read only up to their
/// metadata cut ([`IngestOptions::cut_bounded`]).
#[allow(clippy::too_many_arguments)]
pub fn stream_closed_sources_into(
    adapter: &dyn AgentAdapter,
    runtime: &RuntimeRow,
    root: &Path,
    planner: &mut Store,
    files: &[SessionFile],
    opts: &IngestOptions,
    instrument: &Instrument,
    mut ledger: Option<&mut rosetta_capture::Ledger>,
    trees: Option<&TreeMap>,
    walk: &ColdWalkOptions,
    emit: &mut dyn ColdEmit,
) -> Result<StreamReport, PipelineError> {
    let opts = &closed_pass_options(opts, trees);
    let plan = walk_plan_for(adapter, files, trees, walk);
    let ordered = path_ordered(files);
    set_up_roster(
        adapter,
        runtime,
        planner,
        &ordered,
        instrument,
        ledger.as_deref_mut(),
    )?;
    let walker = walk::Walker::new(adapter, runtime, root, planner, &ordered, opts, instrument)?;
    match plan.order {
        WalkOrder::Path { .. } => walker.walk_path(&ordered, ledger, None, emit),
        WalkOrder::Recency => walker.walk_lanes(&plan, ledger, None, walk, emit),
    }
}

/// [`stream_closed_sources_into`] with its sources captured on a shared
/// [`CapturePool`]: this thread becomes the runtime's sequencer. Workers
/// read, normalize and frame the JSONL sources ahead of it. On the path walk
/// it commits their output to the planner and the ledger strictly in path
/// order, so the ledger bytes, the planner and every emitted source are those
/// of the serial walk, whatever the pool's size or schedule. On a recency
/// walk each lane is its own pool runtime, and the sequencer merges the lanes
/// one event at a time, whichever lane's head is ready first: the planner and
/// each source's ledger content are the serial walk's, while the interleaving
/// of different sources' ledger frames follows the schedule.
#[allow(clippy::too_many_arguments)]
pub fn stream_closed_sources_pooled(
    pool: &CapturePool,
    adapter: &'static dyn AgentAdapter,
    runtime: &RuntimeRow,
    root: &Path,
    planner: &mut Store,
    files: &[SessionFile],
    opts: &IngestOptions,
    instrument: &Instrument,
    mut ledger: Option<&mut rosetta_capture::Ledger>,
    trees: Option<&TreeMap>,
    walk: &ColdWalkOptions,
    emit: &mut dyn ColdEmit,
) -> Result<StreamReport, PipelineError> {
    let opts = &closed_pass_options(opts, trees);
    let plan = walk_plan_for(adapter, files, trees, walk);
    let ordered = path_ordered(files);
    set_up_roster(
        adapter,
        runtime,
        planner,
        &ordered,
        instrument,
        ledger.as_deref_mut(),
    )?;
    let walker = walk::Walker::new(adapter, runtime, root, planner, &ordered, opts, instrument)?;
    match plan.order {
        WalkOrder::Path { .. } => {
            let pooled = walker.register(
                pool,
                adapter,
                &ordered,
                &plan,
                LaneClass::Shared,
                ledger.as_deref(),
            )?;
            walker.walk_path(&ordered, ledger, pooled, emit)
        }
        WalkOrder::Recency => walker.walk_lanes(&plan, ledger, Some((pool, adapter)), walk, emit),
    }
}

/// The options every closed-source pass runs under: the walk hands the writer
/// only terminal generations ([`close_file`]) of a build nothing of which is
/// published yet, so a pass never leaves one staged. A runtime admitted per
/// root tree (`trees` is `Some` only for one that asked for
/// [`rosetta_adapter_support::ColdAdmission::RootTree`] and did not fall
/// back) also reads each transcript only up to its metadata cut, so a tree
/// is complete as of the metadata it was listed with. Runtime admission —
/// declared, fallen back to, or forced by the kill switch, which declares
/// it for every adapter — reads to the end, as before.
fn closed_pass_options(opts: &IngestOptions, trees: Option<&TreeMap>) -> IngestOptions {
    IngestOptions {
        private_build: true,
        cut_bounded: trees.is_some(),
        ..opts.clone()
    }
}

/// Every roster source's cursor, created up front, and its roster key
/// installed on the ledger.
fn set_up_roster(
    adapter: &dyn AgentAdapter,
    runtime: &RuntimeRow,
    planner: &mut Store,
    ordered: &[&SessionFile],
    instrument: &Instrument,
    ledger: Option<&mut rosetta_capture::Ledger>,
) -> Result<(), PipelineError> {
    create_roster_cursors(adapter, runtime, planner, ordered, instrument)?;
    if let Some(ledger) = ledger {
        ledger.set_source_keys(Arc::new(roster_keys(ordered)?))?;
    }
    Ok(())
}

/// Each roster source's ledger file id: its index in path order.
fn roster_keys(ordered: &[&SessionFile]) -> Result<HashMap<SourceDef, u32>, PipelineError> {
    ordered
        .iter()
        .enumerate()
        .map(|(index, file)| {
            let key = u32::try_from(index).map_err(|_| {
                PipelineError::Config("cold roster has more sources than ledger ids".to_string())
            })?;
            Ok((source_def(file), key))
        })
        .collect()
}

/// Create the fresh cursor of every roster source that has none, in one
/// planner transaction, in path order. Cursor ids are rowids allocated in
/// insertion order, so each id equals the one creation at the source's turn
/// in the walk gives it; the batch names no source, so closing a unit
/// ignores it.
fn create_roster_cursors(
    adapter: &dyn AgentAdapter,
    runtime: &RuntimeRow,
    planner: &mut Store,
    ordered: &[&SessionFile],
    instrument: &Instrument,
) -> Result<(), PipelineError> {
    let mut checkpoints = Vec::with_capacity(ordered.len());
    for file in ordered {
        if queries::cursor_state(planner.conn(), &runtime.id, &file.relative_path)?.is_none() {
            checkpoints.push(fresh_cursor(adapter, runtime, file));
        }
    }
    if !checkpoints.is_empty() {
        instrument.apply_owned(
            planner,
            Batch {
                ops: vec![],
                checkpoints,
                source: None,
            },
        )?;
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests;
