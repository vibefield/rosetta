//! How a walk hands closed sources to the canonical writer.
//!
//! A lane of a recency walk must not stall the others while the writer's
//! budget for its class is full: its emission is prepared, then **held**, and
//! the lane pauses its own consumption until a retry sends it. The path walk,
//! the serial driver and every runtime admitted whole emit blocking, exactly
//! as before lanes existed.

use super::ClosedSource;
use super::walk_order::LaneId;
use crate::PipelineError;

/// What became of one emission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmitOutcome {
    /// Handed to the writer.
    Sent,
    /// Prepared and kept until [`ColdEmit::retry`] sends it.
    Held,
}

/// A walk's emitter: one held emission per lane at most.
pub trait ColdEmit {
    /// Prepare `sources` (in emission order) and hand them to the writer for
    /// `lane`; `Held` keeps them prepared until `retry` sends them. Emitting
    /// into a lane that holds one is a bug (error).
    fn emit(
        &mut self,
        lane: LaneId,
        sources: Vec<ClosedSource>,
    ) -> Result<EmitOutcome, PipelineError>;

    /// Try to send `lane`'s held emission again.
    fn retry(&mut self, lane: LaneId) -> Result<EmitOutcome, PipelineError>;

    /// Whether `lane` holds an emission.
    fn held(&self, lane: LaneId) -> bool;

    /// The serial driver and runtime admission: every emission is `Sent`.
    fn blocking(&self) -> bool;
}

/// A blocking emitter over a closure: every emission is sent before `emit`
/// returns.
pub struct BlockingEmit<F: FnMut(LaneId, Vec<ClosedSource>) -> Result<(), PipelineError>>(pub F);

impl<F> ColdEmit for BlockingEmit<F>
where
    F: FnMut(LaneId, Vec<ClosedSource>) -> Result<(), PipelineError>,
{
    fn emit(
        &mut self,
        lane: LaneId,
        sources: Vec<ClosedSource>,
    ) -> Result<EmitOutcome, PipelineError> {
        (self.0)(lane, sources)?;
        Ok(EmitOutcome::Sent)
    }

    fn retry(&mut self, lane: LaneId) -> Result<EmitOutcome, PipelineError> {
        Err(PipelineError::Config(format!(
            "blocking cold emitter holds nothing for lane {lane}"
        )))
    }

    fn held(&self, _lane: LaneId) -> bool {
        false
    }

    fn blocking(&self) -> bool {
        true
    }
}

/// Emit on a walk that cannot hold: a held emission is a broken emitter.
pub(crate) fn emit_blocking(
    emit: &mut dyn ColdEmit,
    lane: LaneId,
    sources: Vec<ClosedSource>,
) -> Result<(), PipelineError> {
    match emit.emit(lane, sources)? {
        EmitOutcome::Sent => Ok(()),
        EmitOutcome::Held => Err(PipelineError::Config(format!(
            "cold emitter held lane {lane} on a walk that cannot hold"
        ))),
    }
}
