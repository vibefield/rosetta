//! Parallel per-file capture for the streamed fresh-cold build.
//!
//! One [`CapturePool`] of workers serves every runtime of a build. Workers
//! step the per-file [`crate::stream::IngestPass`] state machines one tail
//! batch at a time — read, normalize, checkpoint, and the batch's finished
//! raw-ledger frames — and queue the output on its source. Each runtime's
//! producer becomes its SEQUENCER: the only code that touches its planner and
//! ledger, consuming that output strictly in source-path order, so the
//! planner's CAS chain, the ledger's bytes and the closed sources the writer
//! sees are the serial walk's for any worker count and any schedule.
//! Snapshot and document groups, sibling heals and any non-fresh cursor stay
//! on the sequencer at their path position.

mod pool;
mod sequence;

pub use pool::{CapturePool, PoolBudgets, PoolStats};
pub(crate) use sequence::{Pooled, consume, register};

#[cfg(test)]
mod tests;
