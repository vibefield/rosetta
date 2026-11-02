//! The sequencer's side of pooled capture: register a lane's pool sources,
//! then commit each one's worker output at its position in the lane, exactly
//! as the serial driver commits what its own steps produce.

use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

use rosetta_adapter_support::{AgentAdapter, StreamDriver};
use rosetta_capture::{Ledger, SegmentCodec, Tailer};
use rosetta_core::rows::RuntimeRow;
use rosetta_store::{Store, queries};

use super::pool::{CapturePool, Event, Lane, LaneClass, Output, Shared, UnitSpec, elapsed_nanos};
use crate::closed::source_def;
use crate::instrument::{Instrument, Metric};
use crate::meta::SessionFile;
use crate::stream::{FilePass, ReportDelta, StreamReport, apply_pass_batch};
use crate::{IngestOptions, PipelineError};

/// A lane registered with the pool.
pub(crate) struct Pooled<'p> {
    pub(crate) lane: Lane<'p>,
    /// Each lane position's pool source, if the pool captures it.
    units: Vec<Option<usize>>,
}

impl Pooled<'_> {
    pub(crate) fn unit(&self, position: usize) -> Option<usize> {
        self.units.get(position).copied().flatten()
    }
}

/// What one pool unit's own events reported, kept apart from every other
/// unit's so a lane interleaving cannot move payload, losses or restarts
/// from one source to another.
#[derive(Debug, Default)]
pub(crate) struct UnitReport {
    pub(crate) deltas: ReportDelta,
    /// The planner refused one of its batches (a checkpoint CAS conflict).
    pub(crate) conflicted: bool,
}

/// What one consumed event did to its unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Consumed {
    /// A batch was committed; the unit continues.
    Batch,
    /// The unit's pass ended, and the lane moved on to its next unit.
    Finished(FilePass),
}

/// Register one lane's sources with the pool, whose workers start on them at
/// once. The pool captures JSONL sources with a fresh cursor: a first
/// generation, read to EOF. Everything else — snapshot and document groups,
/// a resumed cursor, a starter budget's paused passes, the boundary sample
/// the audit tool takes from the first records the runtime's own instrument
/// sees — stays on the sequencer at its lane position. With a ledger
/// (`ledger_codec`), workers finish each batch's frames, compressed when its
/// segments are zstd frames, under each source's roster key: its path rank,
/// whatever its lane position. A lane with no pool source registers nothing.
#[allow(clippy::too_many_arguments)]
pub(crate) fn register<'p>(
    pool: &'p CapturePool,
    adapter: &'static dyn AgentAdapter,
    runtime: &RuntimeRow,
    root: &Path,
    planner: &Store,
    ordered_in_lane: &[&SessionFile],
    path_rank: &HashMap<&str, u32>,
    class: LaneClass,
    opts: &IngestOptions,
    instrument: &Instrument,
    ledger_codec: Option<SegmentCodec>,
) -> Result<Option<Pooled<'p>>, PipelineError> {
    let ordered = ordered_in_lane;
    let mut units = vec![None; ordered.len()];
    let mut specs = Vec::new();
    if opts.starter_budget.is_none() && !instrument.boundary_evidence_enabled() {
        for (position, file) in ordered.iter().enumerate() {
            if file.driver != StreamDriver::JsonLines {
                continue;
            }
            let Some(cursor) =
                queries::cursor_state(planner.conn(), &runtime.id, &file.relative_path)?
            else {
                continue;
            };
            if cursor.highest_generation != 0 || cursor.active_generation != 0 {
                continue;
            }
            let key = roster_key(path_rank, file, position)?;
            units[position] = Some(specs.len());
            specs.push(UnitSpec {
                file: (*file).clone(),
                cursor,
                key,
            });
        }
    }
    if specs.is_empty() {
        return Ok(None);
    }
    let lane = pool.register_lane(
        Shared {
            adapter,
            runtime: runtime.clone(),
            root: root.to_path_buf(),
            opts: opts.clone(),
            tailer: Tailer::new(opts.tail.clone())?,
            frames: ledger_codec.is_some(),
            compress: ledger_codec.and_then(SegmentCodec::batch_level),
            units: specs,
        },
        class,
    );
    Ok(Some(Pooled { lane, units }))
}

/// A source's ledger file id: its path rank.
fn roster_key(
    path_rank: &HashMap<&str, u32>,
    file: &SessionFile,
    _position: usize,
) -> Result<u32, PipelineError> {
    #[cfg(test)]
    if tests_switch::KEY_BY_LANE_POSITION.get() {
        return u32::try_from(_position)
            .map_err(|_| PipelineError::Config("lane position exceeds ledger ids".to_string()));
    }
    path_rank
        .get(file.relative_path.as_str())
        .copied()
        .ok_or_else(|| {
            PipelineError::Config(format!(
                "cold source {} has no path rank",
                file.relative_path
            ))
        })
}

/// Under the attribution mutation, every event's counters also go straight
/// to the walk's report, as v1's code merged them.
fn merge_globally(_deltas: &ReportDelta, _global: &mut StreamReport) {
    #[cfg(test)]
    if tests_switch::ATTRIBUTION_FROM_GLOBAL.get() {
        _deltas.merge_into(_global);
    }
}

/// Commit pool source `unit` at its path position, as
/// [`crate::stream::ingest_file`] commits its own steps: each batch's report
/// counters, the raw ledger first (the same frames, finished by a worker),
/// then the planner under the cursor's CAS. The first error in path order
/// wins: a step's failure surfaces here, at its source's turn.
#[allow(clippy::too_many_arguments)]
pub(crate) fn consume(
    pooled: &Pooled<'_>,
    unit: usize,
    file: &SessionFile,
    planner: &mut Store,
    mut ledger: Option<&mut Ledger>,
    report: &mut StreamReport,
    instrument: &Instrument,
    opts: &IngestOptions,
) -> Result<FilePass, PipelineError> {
    let source = source_def(file);
    let outcome = loop {
        let waited = Instant::now();
        let event = pooled.lane.next(unit, opts);
        instrument.add(Metric::SequencerWaitNanos, elapsed_nanos(waited));
        let output = match event? {
            Event::Batch(output) => output,
            Event::Finished {
                outcome,
                deltas,
                profile,
            } => {
                instrument.merge_snapshot(&profile);
                deltas.merge_into(report);
                break outcome;
            }
            Event::Failed(error) => return Err(error),
        };
        let Output {
            frames,
            batch,
            deltas,
            profile,
            ends,
        } = *output;
        instrument.merge_snapshot(&profile);
        deltas.merge_into(report);

        // Ledger-first: the raw bytes are durable before the projection tx
        // may reference them.
        if let Some(ledger) = ledger.as_deref_mut()
            && let Some(frames) = frames.filter(|frames| !frames.is_empty())
        {
            instrument.ledger_write(ledger, |ledger| -> Result<(), PipelineError> {
                ledger.append_prebuilt(&source, frames)?;
                ledger.checkpoint()?;
                Ok(())
            })?;
        }

        if !apply_pass_batch(planner, batch, report, instrument)? {
            break FilePass::Finished(false);
        }
        if let Some(outcome) = ends {
            break outcome;
        }
    };
    pooled.lane.advance(unit);
    Ok(outcome)
}

/// Commit one event of pool source `unit` — taken from the pool by the
/// lane-merge loop — exactly as [`consume`] commits each of its events: the
/// raw ledger first, then the planner under the cursor's CAS. The event's
/// counters go to the unit's own report; only the planner's batch, stats and
/// conflict counts go to `global`. A finished pass moves the lane on.
#[allow(clippy::too_many_arguments)]
pub(crate) fn consume_event(
    pooled: &Pooled<'_>,
    unit: usize,
    event: Event,
    file: &SessionFile,
    planner: &mut Store,
    ledger: Option<&mut Ledger>,
    unit_report: &mut UnitReport,
    global: &mut StreamReport,
    instrument: &Instrument,
) -> Result<Consumed, PipelineError> {
    let finished = |outcome| {
        pooled.lane.advance(unit);
        Ok(Consumed::Finished(outcome))
    };
    let output = match event {
        Event::Batch(output) => output,
        Event::Finished {
            outcome,
            deltas,
            profile,
        } => {
            instrument.merge_snapshot(&profile);
            add_delta(&mut unit_report.deltas, &deltas);
            merge_globally(&deltas, global);
            return finished(outcome);
        }
        Event::Failed(error) => return Err(error),
    };
    let Output {
        frames,
        batch,
        deltas,
        profile,
        ends,
    } = *output;
    instrument.merge_snapshot(&profile);
    add_delta(&mut unit_report.deltas, &deltas);
    merge_globally(&deltas, global);
    if let Some(ledger) = ledger
        && let Some(frames) = frames.filter(|frames| !frames.is_empty())
    {
        let source = source_def(file);
        instrument.ledger_write(ledger, |ledger| -> Result<(), PipelineError> {
            ledger.append_prebuilt(&source, frames)?;
            ledger.checkpoint()?;
            Ok(())
        })?;
    }
    if !apply_pass_batch(planner, batch, global, instrument)? {
        unit_report.conflicted = true;
        return finished(FilePass::Finished(false));
    }
    match ends {
        Some(outcome) => finished(outcome),
        None => Ok(Consumed::Batch),
    }
}

/// Fold one event's counters into its unit's.
pub(crate) fn add_delta(total: &mut ReportDelta, part: &ReportDelta) {
    total.files_missing += part.files_missing;
    total.retries_exhausted += part.retries_exhausted;
    total.generations_restarted += part.generations_restarted;
    total.records += part.records;
    total.payload_bytes += part.payload_bytes;
    total.parsed += part.parsed;
    total.unknown += part.unknown;
    total.hard_errors += part.hard_errors;
    total.oversized += part.oversized;
    total.quarantined += part.quarantined;
}

/// Test-only mutations, per thread (the sequencer's), so a test that flips
/// one cannot disturb another running beside it.
#[cfg(test)]
pub(crate) mod tests_switch {
    use std::cell::Cell;

    thread_local! {
        /// Key pool units by lane position instead of path rank (I6's
        /// mutation).
        pub(crate) static KEY_BY_LANE_POSITION: Cell<bool> = const { Cell::new(false) };
        /// Attribute a unit's payload, losses and restarts from differences
        /// of the walk's whole report (v1's code, the per-unit test's
        /// mutation).
        pub(crate) static ATTRIBUTION_FROM_GLOBAL: Cell<bool> = const { Cell::new(false) };
    }
}
