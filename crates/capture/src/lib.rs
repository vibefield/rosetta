//! rosetta-capture — the source IO layer (docs/ingest-engine.html).
//!
//! Agent-agnostic source IO lives here: append-delimited [`tailer`] reads,
//! data-root [`discovery`], bounded meta reads, consistent SQLite snapshots,
//! filesystem notification hints, and the append-only raw ledger.

pub mod discovery;
pub mod document;
pub mod ledger;
mod recompress;
pub mod segment;
pub mod segment_index;
pub mod sqlite;
pub mod tailer;
pub mod watch;

pub use discovery::{
    HeadTail, RecordScan, ScannedFile, WalkReport, first_record, head_tail, scan_records, walk,
    walk_classified, walk_filtered, walk_pruned,
};
pub use document::{DocumentOutcome, DocumentSnapshot, snapshot as snapshot_document};
pub use ledger::{
    Ledger, LedgerEntry, LedgerScan, LedgerVisit, LedgerWriteStats, SourceDef, scan as scan_ledger,
    visit as visit_ledger, visit_segments as visit_ledger_segments,
};
pub use recompress::{RecompressOutcome, recompress_segment, recompress_segment_controlled};
pub use segment::{SegmentCodec, sniff_segment};
pub use segment_index::{
    IndexedSource, SegmentFilter, SegmentIndexLine, candidate_segments, rebuild_segment_index,
    segment_filter,
};
pub use sqlite::{
    DbSnapshot, DbStamp, SnapshotOutcome, SqliteFileStatus, db_stamp, is_db_sidecar,
    snapshot as snapshot_db, sqlite_file_status,
};
pub use tailer::{
    CommittedVerification, RawRecord, TailBatch, TailCheckpoint, TailConfig, TailItem, TailRead,
    Tailer, Transition,
};
pub use watch::{NotifyEvents, ScriptedEvents, SourceEvent, SourceEvents, SourceWake};

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
