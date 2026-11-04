//! One worker step of one pool source: begin its pass on the first step,
//! step it once, finish the batch's raw-ledger frames, and hand the output
//! back with the step's instrument delta and its [`CaptureClock`] (moved
//! from `pool.rs` unchanged but for the clock, which is measurement only).

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::Instant;

use rosetta_capture::{FrameCompressor, PrebuiltFrames};
use rosetta_store::Batch;

use super::{Event, Output, Shared, UnitSpec, elapsed_nanos};
use crate::PipelineError;
use crate::capture::CaptureClock;
use crate::instrument::{Instrument, Metric};
use crate::stream::{FilePass, IngestPass, PassMode, PassOutput, PassStep, RawOut, ReportDelta};

/// One step of one source, on a worker, with its instrument delta.
pub(super) fn run_step(
    shared: &Shared,
    unit: usize,
    pass: Option<IngestPass>,
    instrument: &Instrument,
    compressor: &mut Option<FrameCompressor>,
) -> (Option<IngestPass>, Event) {
    let spec = &shared.units[unit];
    let before = instrument.snapshot();
    let started = Instant::now();
    let stepped = catch_unwind(AssertUnwindSafe(|| {
        step_source(shared, spec, pass, instrument, compressor)
    }));
    let nanos = elapsed_nanos(started);
    instrument.add(Metric::CaptureStepNanos, nanos);
    let profile = instrument.snapshot().delta_since(&before);
    match stepped {
        Ok(Ok((pass, Stepped::Batch(frames, batch, deltas, ends)))) => {
            let output = Output {
                frames,
                batch,
                clock: CaptureClock::step(started, nanos, deltas.max_record_bytes),
                deltas,
                profile,
                ends,
            };
            (pass, Event::Batch(Box::new(output)))
        }
        Ok(Ok((_, Stepped::Finished(outcome, deltas)))) => (
            None,
            Event::Finished {
                outcome,
                clock: CaptureClock::step(started, nanos, deltas.max_record_bytes),
                deltas,
                profile: Box::new(profile),
            },
        ),
        Ok(Err(error)) => (None, Event::Failed(error)),
        Err(panic) => {
            let message = panic
                .downcast_ref::<&str>()
                .copied()
                .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
                .unwrap_or("a non-text panic");
            let error = PipelineError::Config(format!(
                "capturing {} panicked: {message}",
                spec.file.relative_path
            ));
            (None, Event::Failed(error))
        }
    }
}

enum Stepped {
    Batch(Option<PrebuiltFrames>, Batch, ReportDelta, Option<FilePass>),
    Finished(FilePass, ReportDelta),
}

/// Begin the pass on its first step, step it once, finish the batch's
/// frames (compressed, for a zstd-frames ledger), and advance the pass as if
/// the sequencer had committed the batch: the only way that commit can fail
/// is a CAS conflict, which ends the source on both sides. A fresh cursor's
/// audit is inert (its generation is not active yet), so pool sources never
/// audit.
fn step_source(
    shared: &Shared,
    spec: &UnitSpec,
    pass: Option<IngestPass>,
    instrument: &Instrument,
    compressor: &mut Option<FrameCompressor>,
) -> Result<(Option<IngestPass>, Stepped), PipelineError> {
    let mut pass = match pass {
        Some(pass) => pass,
        None => IngestPass::begin(
            shared.adapter,
            &shared.runtime,
            &shared.root,
            &spec.file,
            &shared.opts,
            spec.cursor.clone(),
            PassMode::Normal,
        )?,
    };
    let PassOutput { raw, batch, deltas } = match pass.step(
        shared.adapter,
        &shared.runtime,
        &spec.file,
        &shared.tailer,
        &shared.opts,
        instrument,
    )? {
        PassStep::Batch(output) => output,
        PassStep::Finished(outcome, deltas) => {
            return Ok((None, Stepped::Finished(outcome, deltas)));
        }
    };
    let frames = if shared.frames {
        let normalizer_version = shared.adapter.normalizer_version();
        let mut frames = instrument.time(Metric::LedgerFrameNanos, || {
            frame(spec.key, raw, normalizer_version)
        })?;
        if let Some(level) = shared.compress {
            let compressor = compressor_at(compressor, level)?;
            instrument.time(Metric::LedgerCompressNanos, || frames.compress(compressor))?;
        }
        Some(frames)
    } else {
        None
    };
    let ends = pass.committed();
    let pass = ends.is_none().then_some(pass);
    Ok((pass, Stepped::Batch(frames, batch, deltas, ends)))
}

/// The worker's compressor, at `level`.
fn compressor_at(
    compressor: &mut Option<FrameCompressor>,
    level: i32,
) -> Result<&mut FrameCompressor, PipelineError> {
    if compressor
        .as_ref()
        .is_none_or(|compressor| compressor.level() != level)
    {
        *compressor = Some(FrameCompressor::new(level).map_err(|error| {
            PipelineError::Config(format!("opening a level-{level} zstd compressor: {error}"))
        })?);
    }
    Ok(compressor.as_mut().expect("set above"))
}

/// Exactly the frames the serial driver's `append_raw_owned` and
/// `complete_generation` write for one batch, under the source's roster key.
fn frame(key: u32, raw: RawOut, normalizer_version: u32) -> Result<PrebuiltFrames, PipelineError> {
    let mut frames = PrebuiltFrames::new(key, raw.generation);
    frames.push_records(raw.records)?;
    for (offset, document) in raw.documents {
        frames.push_document(offset, &document)?;
    }
    if let Some(clean) = raw.seal {
        frames.push_seal(normalizer_version, clean);
    }
    Ok(frames)
}
