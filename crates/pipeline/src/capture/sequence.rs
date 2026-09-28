//! The sequencer's side of pooled capture: register a runtime's pool
//! sources, then commit each one's worker output at its path position,
//! exactly as the serial driver commits what its own steps produce.

use std::path::Path;
use std::time::Instant;

use rosetta_adapter_support::{AgentAdapter, StreamDriver};
use rosetta_capture::{Ledger, SegmentCodec, Tailer};
use rosetta_core::rows::RuntimeRow;
use rosetta_store::{Store, queries};

use super::pool::{CapturePool, Event, Lane, Output, Shared, UnitSpec, elapsed_nanos};
use crate::closed::source_def;
use crate::instrument::{Instrument, Metric};
use crate::meta::SessionFile;
use crate::stream::{FilePass, StreamReport, apply_pass_batch};
use crate::{IngestOptions, PipelineError};

/// A runtime registered with the pool.
pub(crate) struct Pooled<'p> {
    lane: Lane<'p>,
    /// Each path-ordered roster position's pool source, if the pool
    /// captures it.
    units: Vec<Option<usize>>,
}

impl Pooled<'_> {
    pub(crate) fn unit(&self, position: usize) -> Option<usize> {
        self.units.get(position).copied().flatten()
    }
}

/// Register a runtime's path-ordered roster with the pool, whose workers
/// start on it at once. The pool captures JSONL sources with a fresh cursor:
/// a first generation, read to EOF. Everything else — snapshot and document
/// groups, a resumed cursor, a starter budget's paused passes, the boundary
/// sample the audit tool takes from the first records the runtime's own
/// instrument sees — stays on the sequencer at its path position. With a
/// ledger (`ledger_codec`), workers finish each batch's frames, compressed
/// when its segments are zstd frames.
#[allow(clippy::too_many_arguments)]
pub(crate) fn register<'p>(
    pool: &'p CapturePool,
    adapter: &'static dyn AgentAdapter,
    runtime: &RuntimeRow,
    root: &Path,
    planner: &Store,
    ordered: &[&SessionFile],
    opts: &IngestOptions,
    instrument: &Instrument,
    ledger_codec: Option<SegmentCodec>,
) -> Result<Pooled<'p>, PipelineError> {
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
            let key = u32::try_from(position).map_err(|_| {
                PipelineError::Config("cold roster has more sources than ledger ids".to_string())
            })?;
            units[position] = Some(specs.len());
            specs.push(UnitSpec {
                file: (*file).clone(),
                cursor,
                key,
            });
        }
    }
    let lane = pool.register(Shared {
        adapter,
        runtime: runtime.clone(),
        root: root.to_path_buf(),
        opts: opts.clone(),
        tailer: Tailer::new(opts.tail.clone())?,
        frames: ledger_codec.is_some(),
        compress: ledger_codec.and_then(SegmentCodec::batch_level),
        units: specs,
    });
    Ok(Pooled { lane, units })
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
