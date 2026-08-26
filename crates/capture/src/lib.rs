//! rosetta-capture — the source IO layer (docs/ingest-engine.html).
//!
//! Three concerns live here, all agent-agnostic: the append-delimited
//! [`tailer`] (ported from spaghetti with its adversarial guarantees), the
//! data-root [`discovery`] walk, and bounded head/tail reads for the meta
//! phase. The notify watcher and raw ledger are deliberately later arrivals —
//! backfill works without them.

pub mod discovery;
pub mod ledger;
pub mod sqlite;
pub mod tailer;
pub mod watch;

pub use discovery::{
    HeadTail, ScannedFile, WalkReport, first_record, head_tail, walk, walk_filtered,
};
pub use ledger::{Ledger, LedgerEntry, LedgerScan, SourceDef, scan as scan_ledger};
pub use sqlite::{
    DbSnapshot, DbStamp, SnapshotOutcome, db_stamp, is_db_sidecar, snapshot as snapshot_db,
};
pub use tailer::{
    RawRecord, TailBatch, TailCheckpoint, TailConfig, TailItem, TailRead, Tailer, Transition,
};
pub use watch::{NotifyEvents, ScriptedEvents, SourceEvent, SourceEvents};

#[derive(Debug)]
pub enum CaptureError {
    InvalidConfig(String),
    InvalidCheckpoint(String),
    LimitExceeded(String),
    Io {
        operation: &'static str,
        path: String,
        source: std::io::Error,
    },
}

impl std::fmt::Display for CaptureError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidConfig(message) => write!(formatter, "invalid capture config: {message}"),
            Self::InvalidCheckpoint(message) => {
                write!(formatter, "invalid tail checkpoint: {message}")
            }
            Self::LimitExceeded(message) => write!(formatter, "capture limit exceeded: {message}"),
            Self::Io {
                operation,
                path,
                source,
            } => write!(
                formatter,
                "capture IO failed while {operation} {path}: {source}"
            ),
        }
    }
}

impl std::error::Error for CaptureError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

pub(crate) fn io_error(
    operation: &'static str,
    path: &std::path::Path,
    source: std::io::Error,
) -> CaptureError {
    CaptureError::Io {
        operation,
        path: path.to_string_lossy().into_owned(),
        source,
    }
}
