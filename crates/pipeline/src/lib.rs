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
mod document;
pub mod instrument;
pub mod live;
pub mod meta;
pub mod plan;
mod project;
pub mod replay;
pub mod sched;
mod snapshot;
pub mod stream;

pub use checkpoint::PipeCheckpoint;
pub use discover::{Discovery, discover, expand_data_root};
pub use instrument::{Instrument, InstrumentSnapshot, Metric};
pub use live::{HintMapping, LiveOptions, LiveReport, LiveRunner, map_hint, stream_live};
pub use meta::{MetaReport, SessionFile, meta_ingest, meta_ingest_into};
pub use plan::{IngestPlan, SessionPlan};
pub use replay::{RebuildReport, rebuild_runtime};
pub use sched::{ColdLane, ColdSchedule, order_runtime_roster};
pub use stream::{StreamReport, stream_backfill, stream_backfill_into};

/// Version of the canonical-op envelope stored in ledger meta frames. Replay
/// routes explicitly by this value instead of assuming today's row JSON.
/// Meta payload v2 makes the adapter-selected primary cwd explicit on the
/// session and removes streamed workspace observations from the projection.
pub const LEDGER_META_SCHEMA_VERSION: u32 = 2;

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

/// Byte-governed bound for one starter pass over a fresh JSONL source. The
/// pass stops at the first batch boundary at or past `max_bytes`, activates
/// the clean prefix, and seals its generation — the unread remainder becomes
/// ordinary append growth for a later completion pass. Bytes govern because
/// they bound the slice's wall time (and boost preemption); the record floor
/// guarantees minimum content when single records are huge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SliceBudget {
    /// Soft byte target; the stop check runs at batch boundaries, so one
    /// budgeted batch of overshoot is expected.
    pub max_bytes: u64,
    /// Never pause before this many complete records. Also the record
    /// granularity of budgeted batches — and therefore of the stop check.
    pub min_records: u32,
}

impl SliceBudget {
    /// Starting default until the ingest-bench sweep picks the sweet spot.
    pub const STARTER: Self = Self {
        max_bytes: 256 * 1024,
        min_records: 24,
    };
}

/// Bounds for one ingest run. Every limit is explicit so the profiler can
/// attribute cost to a knob.
#[derive(Debug, Clone)]
pub struct IngestOptions {
    /// Skim windows — the meta-phase promise is enforced by these bounds.
    pub skim_head_bytes: usize,
    pub skim_tail_bytes: usize,
    /// Optional complete-record scan after the head, used only when an
    /// adapter says a useful metadata fact is still missing.
    pub skim_fallback_bytes: usize,
    pub skim_fallback_records: usize,
    /// Walk cap per data root.
    pub max_files: usize,
    /// Framing bounds for the stream phase.
    pub tail: TailConfig,
    /// Transient-read retries per file before the lane gives up.
    pub max_retries: u32,
    /// Files per runtime that receive a rotating forced replay each run. This
    /// is the bounded audit lane for same-size rewrites outside the tail anchor.
    pub audit_files_per_run: usize,
    /// `Some` bounds each JSONL pass to a starter slice (fresh first
    /// generations early-activate at the boundary; active generations simply
    /// pause). The daemon keeps the budget on for BOTH its starter and
    /// completion lanes so an app boost is never stuck behind more than one
    /// slice of anything. `None` — the default, and the setting for finite
    /// ingest, rebuild, audits, and boost passes — reads every source to
    /// EOF. Snapshot-driver sources (SQLite/document) always stream whole:
    /// their revision identity is a whole-content hash.
    pub starter_budget: Option<SliceBudget>,
}

impl Default for IngestOptions {
    fn default() -> Self {
        Self {
            skim_head_bytes: 64 * 1024,
            skim_tail_bytes: 16 * 1024,
            skim_fallback_bytes: 128 * 1024,
            skim_fallback_records: 256,
            max_files: 100_000,
            tail: TailConfig::json_lines(),
            max_retries: 3,
            audit_files_per_run: 1,
            starter_budget: None,
        }
    }
}
