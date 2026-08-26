//! rosetta-pipeline — the phase orchestrators (docs/ingest-engine.html).
//!
//! Three phases, James's framing: **discovery** (what's installed — the app
//! asks the user which to ingest), **meta ingest** (bounded skims so project
//! and session lists render before first paint), **stream** (backfill lanes
//! that parse transcripts and keep cursors durable). Phases take `&dyn` deps
//! so integration tests run whole phases against a temp-dir corpus and an
//! in-memory store.
//!
//! The pipeline owns id derivation: adapters report NATIVE facts, and every
//! rosetta id (runtime, project, session, timeline prefix inputs) is minted
//! here from `rosetta_core::ids`.

pub mod checkpoint;
pub mod discover;
pub mod instrument;
pub mod live;
pub mod meta;
mod project;
pub mod replay;
pub mod sched;
pub mod stream;

pub use checkpoint::PipeCheckpoint;
pub use discover::{Discovery, discover, expand_data_root};
pub use instrument::{Instrument, Metric};
pub use live::{LiveOptions, LiveReport, LiveRunner, stream_live};
pub use meta::{MetaReport, SessionFile, meta_ingest, meta_ingest_into};
pub use replay::{RebuildReport, rebuild_runtime};
pub use sched::{Lane, WorkItem, WorkQueue};
pub use stream::{StreamReport, stream_backfill, stream_backfill_into};

use rosetta_capture::{CaptureError, TailConfig};
use rosetta_store::StoreError;

#[derive(Debug)]
pub enum PipelineError {
    Capture(CaptureError),
    Store(StoreError),
    Config(String),
}

impl From<CaptureError> for PipelineError {
    fn from(error: CaptureError) -> Self {
        Self::Capture(error)
    }
}

impl From<StoreError> for PipelineError {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}

impl std::fmt::Display for PipelineError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Capture(error) => write!(formatter, "{error}"),
            Self::Store(error) => write!(formatter, "store error: {error:?}"),
            Self::Config(message) => write!(formatter, "pipeline config: {message}"),
        }
    }
}

impl std::error::Error for PipelineError {}

/// Bounds for one ingest run. Every limit is explicit so the profiler can
/// attribute cost to a knob.
#[derive(Debug, Clone)]
pub struct IngestOptions {
    /// Skim windows — the meta-phase promise is enforced by these bounds.
    pub skim_head_bytes: usize,
    pub skim_tail_bytes: usize,
    /// Walk cap per data root.
    pub max_files: usize,
    /// Framing bounds for the stream phase.
    pub tail: TailConfig,
    /// Transient-read retries per file before the lane gives up.
    pub max_retries: u32,
    /// Files per runtime that receive a rotating forced replay each run. This
    /// is the bounded audit lane for same-size rewrites outside the tail anchor.
    pub audit_files_per_run: usize,
}

impl Default for IngestOptions {
    fn default() -> Self {
        Self {
            skim_head_bytes: 64 * 1024,
            skim_tail_bytes: 16 * 1024,
            max_files: 100_000,
            tail: TailConfig::json_lines(),
            max_retries: 3,
            audit_files_per_run: 1,
        }
    }
}
