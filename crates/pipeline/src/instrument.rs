//! The profiler that ships in the binary. Cheap atomic counters per stage —
//! always on, so `rosettad ingest --profile` and the integration tests read
//! the same numbers real runs produce.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

use rosetta_adapter_support::{LineOutcome, LineStatus};
use rosetta_capture::{Ledger, LedgerWriteStats};
use rosetta_core::IngestOp;
use rosetta_store::{ApplyStats, Batch, BulkLoadStats, Store, StoreError};
use serde_json::{Value, json};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Metric {
    FilesWalked,
    FilesMatched,
    FilesSkimmed,
    SessionsDiscovered,
    RecordsFramed,
    RecordsOversized,
    LinesParsed,
    LinesUnknown,
    LinesHardError,
    OpsEmitted,
    BatchesApplied,
    BytesRead,
    CheckpointConflicts,
    WalkNanos,
    SkimNanos,
    SkimFallbackBytes,
    SkimFallbackNanos,
    ReadNanos,
    NormalizeNanos,
    ApplyNanos,
    ProjectionAssets,
    AssetFilesWritten,
    AssetFilesReused,
    AssetBytes,
    AssetWriteNanos,
    LedgerReadNanos,
    LedgerWriteNanos,
    LedgerFrames,
    LedgerFrameBytes,
    LedgerFlushes,
    LedgerSyncs,
    LedgerDirectorySyncs,
    LedgerSegmentsOpened,
    LedgerElectionReadNanos,
    LedgerReplayReadNanos,
    LedgerFrameNanos,
    LedgerBufferWriteNanos,
    LedgerFlushNanos,
    LedgerSyncNanos,
    LedgerDirectorySyncNanos,
    LedgerSegmentOpenNanos,
    SourceStageNanos,
    CanonicalApplyNanos,
    SqliteCommitNanos,
    MessageApplyAttempts,
    ToolResultApplyAttempts,
    UsageApplyAttempts,
    DriftApplyAttempts,
    OtherApplyAttempts,
    SessionTouchAttempts,
    SessionTouchRows,
    PendingGateChecks,
    PendingLookupSkips,
    PendingLookups,
    PendingRows,
    PendingQueues,
    OrdinalLookups,
    SessionRenumbers,
    SessionRenumberRows,
    BulkToolRowsReconciled,
    BulkToolFactsWritten,
    MessageLookupNanos,
    MessageWriteNanos,
    ToolResultLookupNanos,
    ToolResultWriteNanos,
    UsageLookupNanos,
    UsageWriteNanos,
    SessionTouchNanos,
    PendingGateNanos,
    PendingLookupNanos,
    PendingQueueNanos,
    DriftApplyNanos,
    OtherApplyNanos,
    OrdinalLookupNanos,
    SessionRenumberNanos,
    ToolReconcileNanos,
    ToolFactWriteNanos,
    BulkRenumberRows,
    BulkRenumberNanos,
    BulkBucketNanos,
    BulkFtsNanos,
    BulkIndexNanos,
    BulkValidateNanos,
    BulkPragmaNanos,
    BulkAssetSyncNanos,
    PublishNanos,
    SearchSyncNanos,
}

const METRIC_COUNT: usize = 87;

const ALL: [(Metric, &str); METRIC_COUNT] = [
    (Metric::FilesWalked, "files walked"),
    (Metric::FilesMatched, "files matched"),
    (Metric::FilesSkimmed, "files skimmed"),
    (Metric::SessionsDiscovered, "sessions discovered"),
    (Metric::RecordsFramed, "records framed"),
    (Metric::RecordsOversized, "records oversized"),
    (Metric::LinesParsed, "lines parsed"),
    (Metric::LinesUnknown, "lines unknown-type"),
    (Metric::LinesHardError, "lines hard-error"),
    (Metric::OpsEmitted, "ops emitted"),
    (Metric::BatchesApplied, "batches applied"),
    (Metric::BytesRead, "bytes read"),
    (Metric::CheckpointConflicts, "checkpoint conflicts"),
    (Metric::WalkNanos, "walk time"),
    (Metric::SkimNanos, "skim time"),
    (Metric::SkimFallbackBytes, "skim fallback bytes"),
    (Metric::SkimFallbackNanos, "skim fallback time"),
    (Metric::ReadNanos, "read time"),
    (Metric::NormalizeNanos, "normalize time"),
    (Metric::ApplyNanos, "apply time"),
    (Metric::ProjectionAssets, "projection assets"),
    (Metric::AssetFilesWritten, "asset files written"),
    (Metric::AssetFilesReused, "asset files reused"),
    (Metric::AssetBytes, "materialized asset bytes"),
    (Metric::AssetWriteNanos, "asset materialization time"),
    (Metric::LedgerReadNanos, "ledger read time"),
    (Metric::LedgerWriteNanos, "ledger write time"),
    (Metric::LedgerFrames, "ledger frames"),
    (Metric::LedgerFrameBytes, "ledger frame bytes"),
    (Metric::LedgerFlushes, "ledger flushes"),
    (Metric::LedgerSyncs, "ledger syncs"),
    (Metric::LedgerDirectorySyncs, "ledger directory syncs"),
    (Metric::LedgerSegmentsOpened, "ledger segments opened"),
    (Metric::LedgerElectionReadNanos, "ledger election read time"),
    (Metric::LedgerReplayReadNanos, "ledger replay read time"),
    (Metric::LedgerFrameNanos, "ledger framing time"),
    (Metric::LedgerBufferWriteNanos, "ledger buffered write time"),
    (Metric::LedgerFlushNanos, "ledger flush time"),
    (Metric::LedgerSyncNanos, "ledger sync time"),
    (
        Metric::LedgerDirectorySyncNanos,
        "ledger directory sync time",
    ),
    (Metric::LedgerSegmentOpenNanos, "ledger segment open time"),
    (Metric::SourceStageNanos, "source staging time"),
    (Metric::CanonicalApplyNanos, "canonical apply time"),
    (Metric::SqliteCommitNanos, "sqlite commit time"),
    (Metric::MessageApplyAttempts, "message apply attempts"),
    (
        Metric::ToolResultApplyAttempts,
        "tool-result apply attempts",
    ),
    (Metric::UsageApplyAttempts, "usage apply attempts"),
    (Metric::DriftApplyAttempts, "drift apply attempts"),
    (Metric::OtherApplyAttempts, "other apply attempts"),
    (Metric::SessionTouchAttempts, "session touch attempts"),
    (Metric::SessionTouchRows, "session touch rows"),
    (Metric::PendingGateChecks, "pending batch checks"),
    (Metric::PendingLookupSkips, "pending lookups skipped"),
    (Metric::PendingLookups, "pending lookups"),
    (Metric::PendingRows, "pending rows drained"),
    (Metric::PendingQueues, "pending rows queued"),
    (Metric::OrdinalLookups, "ordinal lookups"),
    (Metric::SessionRenumbers, "session renumbers"),
    (Metric::SessionRenumberRows, "session renumber rows"),
    (Metric::BulkToolRowsReconciled, "bulk tool rows reconciled"),
    (Metric::BulkToolFactsWritten, "bulk tool facts written"),
    (Metric::MessageLookupNanos, "message lookup time"),
    (Metric::MessageWriteNanos, "message write time"),
    (Metric::ToolResultLookupNanos, "tool-result lookup time"),
    (Metric::ToolResultWriteNanos, "tool-result write time"),
    (Metric::UsageLookupNanos, "usage lookup time"),
    (Metric::UsageWriteNanos, "usage write time"),
    (Metric::SessionTouchNanos, "session touch time"),
    (Metric::PendingGateNanos, "pending gate time"),
    (Metric::PendingLookupNanos, "pending lookup time"),
    (Metric::PendingQueueNanos, "pending queue time"),
    (Metric::DriftApplyNanos, "drift apply time"),
    (Metric::OtherApplyNanos, "other apply time"),
    (Metric::OrdinalLookupNanos, "ordinal lookup time"),
    (Metric::SessionRenumberNanos, "session renumber time"),
    (Metric::ToolReconcileNanos, "tool reconciliation time"),
    (Metric::ToolFactWriteNanos, "tool fact write time"),
    (Metric::BulkRenumberRows, "bulk renumber rows"),
    (Metric::BulkRenumberNanos, "bulk renumber time"),
    (Metric::BulkBucketNanos, "bulk bucket time"),
    (Metric::BulkFtsNanos, "bulk FTS time"),
    (Metric::BulkIndexNanos, "bulk index time"),
    (Metric::BulkValidateNanos, "bulk validate time"),
    (Metric::BulkPragmaNanos, "bulk pragma time"),
    (Metric::BulkAssetSyncNanos, "bulk asset sync time"),
    (Metric::PublishNanos, "publication time"),
    (Metric::SearchSyncNanos, "detached search sync time"),
];

fn index(metric: Metric) -> usize {
    ALL.iter()
        .position(|(candidate, _)| *candidate == metric)
        .expect("every metric is registered")
}

#[derive(Debug)]
pub struct Instrument {
    values: [AtomicU64; METRIC_COUNT],
    boundary_enabled: AtomicBool,
    boundary_complete: AtomicBool,
    boundary: Mutex<Option<BoundaryCapture>>,
}

#[derive(Debug, Default)]
struct BoundaryCapture {
    records_seen: u64,
    priority: u8,
    sample: Option<BoundarySample>,
    write_batch: Option<Value>,
}

#[derive(Debug)]
struct BoundarySample {
    stream: String,
    source_offset: Option<i64>,
    line_status: &'static str,
    timeline_id: Option<String>,
    parsed_record: Value,
    normalized_ops: Value,
}

impl Default for Instrument {
    fn default() -> Self {
        Self {
            values: std::array::from_fn(|_| AtomicU64::new(0)),
            boundary_enabled: AtomicBool::new(false),
            boundary_complete: AtomicBool::new(false),
            boundary: Mutex::new(None),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstrumentSnapshot {
    values: [u64; METRIC_COUNT],
}

impl Instrument {
    pub fn new() -> Self {
        Self::default()
    }

    /// Enable one bounded, content-bearing sample for the local JSONL audit
    /// tool. Ordinary production runs never take the mutex or serialize data.
    pub fn enable_boundary_evidence(&self) {
        *self
            .boundary
            .lock()
            .expect("boundary evidence mutex poisoned") = Some(BoundaryCapture::default());
        self.boundary_complete.store(false, Ordering::Relaxed);
        self.boundary_enabled.store(true, Ordering::Release);
    }

    /// Capture the same native bytes and canonical ops that crossed the real
    /// adapter boundary. Prefer the first message-producing record because it
    /// can be followed into the canonical message table; retain the first
    /// non-empty outcome as a fallback for non-message streams.
    pub fn observe_normalization(
        &self,
        stream: &str,
        source_offset: Option<i64>,
        payload: &[u8],
        outcome: &LineOutcome,
    ) {
        if !self.boundary_enabled.load(Ordering::Acquire)
            || self.boundary_complete.load(Ordering::Relaxed)
            || outcome.ops.is_empty()
        {
            return;
        }
        let timeline_id = outcome.ops.iter().find_map(|op| match op {
            IngestOp::Message(row) => Some(row.timeline_id.clone()),
            _ => None,
        });
        let priority = if timeline_id.is_some() { 2 } else { 1 };
        let mut guard = self
            .boundary
            .lock()
            .expect("boundary evidence mutex poisoned");
        let Some(capture) = guard.as_mut() else {
            return;
        };
        capture.records_seen = capture.records_seen.saturating_add(1);
        if capture.priority >= priority {
            return;
        }
        let parsed = serde_json::from_slice::<Value>(payload)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(payload).into_owned()));
        capture.priority = priority;
        capture.sample = Some(BoundarySample {
            stream: stream.to_owned(),
            source_offset,
            line_status: match &outcome.status {
                LineStatus::Parsed => "parsed",
                LineStatus::UnknownType => "unknown_type",
                LineStatus::HardError { .. } => "hard_error",
            },
            timeline_id,
            parsed_record: preview_value("parsed native JSON", &parsed, Some(payload.len())),
            normalized_ops: preview_value("Vec<IngestOp>", &outcome.ops, None),
        });
        capture.write_batch = None;
    }

    /// Capture the concrete batch after generic workspace enrichment and
    /// before Store::apply consumes it. The preview is intentionally bounded.
    pub fn observe_write_batch(&self, batch: &Batch) {
        if !self.boundary_enabled.load(Ordering::Acquire)
            || self.boundary_complete.load(Ordering::Relaxed)
        {
            return;
        }
        let mut guard = self
            .boundary
            .lock()
            .expect("boundary evidence mutex poisoned");
        let Some(capture) = guard.as_mut() else {
            return;
        };
        let Some(sample) = capture.sample.as_ref() else {
            return;
        };
        if let Some(timeline_id) = sample.timeline_id.as_deref()
            && !batch
                .ops
                .iter()
                .any(|op| matches!(op, IngestOp::Message(row) if row.timeline_id == timeline_id))
        {
            return;
        }
        capture.write_batch = Some(preview_batch(batch, sample.timeline_id.as_deref()));
        if capture.priority >= 2 || capture.records_seen >= 64 {
            self.boundary_complete.store(true, Ordering::Release);
        }
    }

    /// Return the bounded sample as JSON so the diagnostic CLI can pair it
    /// with pre/post-finalization SQLite snapshots without exposing a new
    /// production representation.
    pub fn boundary_evidence(&self) -> Option<Value> {
        if !self.boundary_enabled.load(Ordering::Acquire) {
            return None;
        }
        let guard = self
            .boundary
            .lock()
            .expect("boundary evidence mutex poisoned");
        let capture = guard.as_ref()?;
        let sample = capture.sample.as_ref()?;
        Some(json!({
            "stream": sample.stream,
            "sourceOffset": sample.source_offset,
            "lineStatus": sample.line_status,
            "sampleTimelineId": sample.timeline_id,
            "parsedRecord": sample.parsed_record,
            "normalizedOps": sample.normalized_ops,
            "writeBatch": capture.write_batch,
        }))
    }

    pub fn add(&self, metric: Metric, amount: u64) {
        self.values[index(metric)].fetch_add(amount, Ordering::Relaxed);
    }

    pub fn get(&self, metric: Metric) -> u64 {
        self.values[index(metric)].load(Ordering::Relaxed)
    }

    /// Time a closure into a `*Nanos` metric.
    pub fn time<T>(&self, metric: Metric, work: impl FnOnce() -> T) -> T {
        let started = Instant::now();
        let value = work();
        self.add(metric, elapsed_nanos(started));
        value
    }

    /// Time a durable ledger operation and drain its non-overlapping internal
    /// counters. Draining after errors preserves evidence from partial work.
    pub fn ledger_write<T, E>(
        &self,
        ledger: &mut Ledger,
        work: impl FnOnce(&mut Ledger) -> Result<T, E>,
    ) -> Result<T, E> {
        let started = Instant::now();
        let result = work(ledger);
        self.add(Metric::LedgerWriteNanos, elapsed_nanos(started));
        self.record_ledger_write(ledger.take_write_stats());
        result
    }

    fn record_ledger_write(&self, stats: LedgerWriteStats) {
        self.add(Metric::LedgerFrames, stats.frames);
        self.add(Metric::LedgerFrameBytes, stats.frame_bytes);
        self.add(Metric::LedgerFlushes, stats.flushes);
        self.add(Metric::LedgerSyncs, stats.syncs);
        self.add(Metric::LedgerDirectorySyncs, stats.directory_syncs);
        self.add(Metric::LedgerSegmentsOpened, stats.segments_opened);
        self.add(Metric::LedgerFrameNanos, stats.frame_nanos);
        self.add(Metric::LedgerBufferWriteNanos, stats.buffer_write_nanos);
        self.add(Metric::LedgerFlushNanos, stats.flush_nanos);
        self.add(Metric::LedgerSyncNanos, stats.sync_nanos);
        self.add(Metric::LedgerDirectorySyncNanos, stats.directory_sync_nanos);
        self.add(Metric::LedgerSegmentOpenNanos, stats.segment_open_nanos);
    }

    /// Apply one store batch while retaining both the end-to-end time and the
    /// store's non-overlapping SQLite subphase measurements.
    pub fn apply(&self, store: &mut Store, batch: &Batch) -> Result<ApplyStats, StoreError> {
        let result = self.time(Metric::ApplyNanos, || store.apply(batch));
        if let Ok(stats) = &result {
            self.record_apply_stats(stats);
        }
        result
    }

    /// Flush the private-cold relational fold at a runtime boundary. The same
    /// counters are used as ordinary batches, while the RAM fold has its own
    /// exclusive timer for the audit UI.
    pub fn flush_bulk_reconciliation(&self, store: &mut Store) -> Result<ApplyStats, StoreError> {
        let result = self.time(Metric::ApplyNanos, || store.flush_bulk_reconciliation());
        if let Ok(stats) = &result {
            self.record_apply_stats(stats);
        }
        result
    }

    fn record_apply_stats(&self, stats: &ApplyStats) {
        self.add(Metric::ProjectionAssets, stats.projection_assets);
        self.add(Metric::AssetFilesWritten, stats.asset_files_written);
        self.add(Metric::AssetFilesReused, stats.asset_files_reused);
        self.add(Metric::AssetBytes, stats.asset_bytes);
        self.add(Metric::AssetWriteNanos, stats.asset_write_nanos);
        self.add(Metric::SourceStageNanos, stats.source_stage_nanos);
        self.add(Metric::CanonicalApplyNanos, stats.canonical_apply_nanos);
        self.add(Metric::SqliteCommitNanos, stats.sqlite_commit_nanos);
        let profile = &stats.profile;
        self.add(Metric::MessageApplyAttempts, profile.message_attempts);
        self.add(
            Metric::ToolResultApplyAttempts,
            profile.tool_result_attempts,
        );
        self.add(Metric::UsageApplyAttempts, profile.usage_attempts);
        self.add(Metric::DriftApplyAttempts, profile.drift_attempts);
        self.add(Metric::OtherApplyAttempts, profile.other_attempts);
        self.add(Metric::SessionTouchAttempts, profile.session_touch_attempts);
        self.add(Metric::SessionTouchRows, profile.session_touch_rows);
        self.add(Metric::PendingGateChecks, profile.pending_gate_checks);
        self.add(Metric::PendingLookupSkips, profile.pending_lookup_skips);
        self.add(Metric::PendingLookups, profile.pending_lookups);
        self.add(Metric::PendingRows, profile.pending_rows);
        self.add(Metric::PendingQueues, profile.pending_queues);
        self.add(Metric::OrdinalLookups, profile.ordinal_lookups);
        self.add(Metric::SessionRenumbers, profile.session_renumbers);
        self.add(Metric::SessionRenumberRows, profile.session_renumber_rows);
        self.add(
            Metric::BulkToolRowsReconciled,
            profile.bulk_tool_rows_reconciled,
        );
        self.add(
            Metric::BulkToolFactsWritten,
            profile.bulk_tool_facts_written,
        );
        self.add(Metric::MessageLookupNanos, profile.message_lookup_nanos);
        self.add(Metric::MessageWriteNanos, profile.message_write_nanos);
        self.add(
            Metric::ToolResultLookupNanos,
            profile.tool_result_lookup_nanos,
        );
        self.add(
            Metric::ToolResultWriteNanos,
            profile.tool_result_write_nanos,
        );
        self.add(Metric::UsageLookupNanos, profile.usage_lookup_nanos);
        self.add(Metric::UsageWriteNanos, profile.usage_write_nanos);
        self.add(Metric::SessionTouchNanos, profile.session_touch_nanos);
        self.add(Metric::PendingGateNanos, profile.pending_gate_nanos);
        self.add(Metric::PendingLookupNanos, profile.pending_lookup_nanos);
        self.add(Metric::PendingQueueNanos, profile.pending_queue_nanos);
        self.add(Metric::DriftApplyNanos, profile.drift_apply_nanos);
        self.add(Metric::OtherApplyNanos, profile.other_apply_nanos);
        self.add(Metric::OrdinalLookupNanos, profile.ordinal_lookup_nanos);
        self.add(Metric::SessionRenumberNanos, profile.session_renumber_nanos);
        self.add(Metric::ToolReconcileNanos, profile.tool_reconcile_nanos);
        self.add(Metric::ToolFactWriteNanos, profile.tool_fact_write_nanos);
    }

    /// Finalize a private projection and record the independently useful
    /// bucket, FTS, index, validation, and journal-mode costs.
    pub fn finish_bulk_load(&self, store: &mut Store) -> Result<BulkLoadStats, StoreError> {
        let result = self.time(Metric::ApplyNanos, || store.finish_bulk_load());
        if let Ok(stats) = &result {
            self.record_bulk_stats(stats);
        }
        result
    }

    /// Finalize the public progressive cold path with the same canonical
    /// accounting as a private rebuild. Search is deliberately absent: its
    /// detached worker reports readiness independently.
    pub fn finish_progressive_load(&self, store: &mut Store) -> Result<BulkLoadStats, StoreError> {
        let result = self.time(Metric::ApplyNanos, || store.finish_progressive_load());
        if let Ok(stats) = &result {
            self.record_bulk_stats(stats);
        }
        result
    }

    fn record_bulk_stats(&self, stats: &BulkLoadStats) {
        self.add(Metric::BulkRenumberRows, stats.renumber_rows);
        self.add(Metric::BulkRenumberNanos, stats.renumber_nanos);
        self.add(Metric::BulkBucketNanos, stats.bucket_nanos);
        self.add(Metric::BulkFtsNanos, stats.fts_nanos);
        self.add(Metric::BulkIndexNanos, stats.index_nanos);
        self.add(Metric::SqliteCommitNanos, stats.commit_nanos);
        self.add(Metric::BulkValidateNanos, stats.validate_nanos);
        self.add(Metric::BulkPragmaNanos, stats.pragma_nanos);
        self.add(Metric::BulkAssetSyncNanos, stats.asset_sync_nanos);
    }

    /// Human-readable report: raw counters plus the derived rates the bench
    /// plan headlines (bytes/sec through read, records/sec through normalize,
    /// ops/sec through apply).
    pub fn report(&self) -> String {
        self.snapshot().report()
    }

    pub fn snapshot(&self) -> InstrumentSnapshot {
        InstrumentSnapshot {
            values: std::array::from_fn(|index| self.values[index].load(Ordering::Relaxed)),
        }
    }
}

const PREVIEW_BYTES: usize = 48 * 1024;
const PREVIEW_OPS: usize = 8;

fn truncate_utf8(text: &str, limit: usize) -> (&str, bool) {
    if text.len() <= limit {
        return (text, false);
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (&text[..end], true)
}

fn preview_value<T: serde::Serialize>(
    representation: &str,
    value: &T,
    original_bytes: Option<usize>,
) -> Value {
    let compact = serde_json::to_vec(value).unwrap_or_default();
    let pretty = serde_json::to_string_pretty(value)
        .unwrap_or_else(|error| format!("<could not serialize audit preview: {error}>"));
    let (preview, truncated) = truncate_utf8(&pretty, PREVIEW_BYTES);
    json!({
        "representation": representation,
        "bytes": original_bytes.unwrap_or(compact.len()),
        "previewBytes": preview.len(),
        "truncated": truncated,
        "data": if truncated {
            format!("{preview}\n… bounded audit preview truncated …")
        } else {
            preview.to_owned()
        },
    })
}

fn source_mode(mode: rosetta_store::SourceMode) -> &'static str {
    match mode {
        rosetta_store::SourceMode::Stage => "stage",
        rosetta_store::SourceMode::Append => "append",
        rosetta_store::SourceMode::Activate => "activate",
        rosetta_store::SourceMode::Abandon => "abandon",
    }
}

fn preview_batch(batch: &Batch, timeline_id: Option<&str>) -> Value {
    let selected_index = timeline_id.and_then(|timeline_id| {
        batch
            .ops
            .iter()
            .position(|op| matches!(op, IngestOp::Message(row) if row.timeline_id == timeline_id))
    });
    let mut sampled = Vec::new();
    if let Some(index) = selected_index {
        sampled.push(&batch.ops[index]);
    }
    for (index, op) in batch.ops.iter().enumerate() {
        if sampled.len() >= PREVIEW_OPS {
            break;
        }
        if Some(index) != selected_index {
            sampled.push(op);
        }
    }
    let source = batch.source.as_ref().map(|source| {
        json!({
            "cursorId": source.cursor_id,
            "sessionId": source.session_id,
            "generation": source.generation,
            "firstSequence": source.first_sequence,
            "mode": source_mode(source.mode),
        })
    });
    let checkpoints: Vec<Value> = batch
        .checkpoints
        .iter()
        .map(|checkpoint| {
            json!({
                "runtimeId": checkpoint.runtime_id,
                "stream": checkpoint.stream,
                "path": checkpoint.path,
                "sessionId": checkpoint.session_id,
                "expectedCheckpointBytes": checkpoint.expected.as_ref().map(String::len),
                "nextCheckpointBytes": checkpoint.next.len(),
                "updatedAt": checkpoint.updated_at,
            })
        })
        .collect();
    preview_value(
        "WriteBatch",
        &json!({
            "opCount": batch.ops.len(),
            "sampleOps": sampled,
            "opsOmitted": batch.ops.len().saturating_sub(sampled.len()),
            "checkpoints": checkpoints,
            "source": source,
        }),
        None,
    )
}

impl InstrumentSnapshot {
    pub fn get(&self, metric: Metric) -> u64 {
        self.values[index(metric)]
    }

    pub fn delta_since(&self, earlier: &Self) -> Self {
        Self {
            values: std::array::from_fn(|index| {
                self.values[index].saturating_sub(earlier.values[index])
            }),
        }
    }

    pub fn report(&self) -> String {
        let mut out = String::new();
        for (metric, name) in ALL {
            let value = self.get(metric);
            if name.ends_with("time") {
                out.push_str(&format!("{name:<30} {:>12.1} ms\n", value as f64 / 1e6));
            } else {
                out.push_str(&format!("{name:<30} {value:>12}\n"));
            }
        }
        out.push_str(&format!(
            "read throughput                {:>12.1} MiB/s\n",
            rate(self.get(Metric::BytesRead), self.get(Metric::ReadNanos)) / (1024.0 * 1024.0)
        ));
        out.push_str(&format!(
            "normalize throughput           {:>12.0} records/s\n",
            rate(
                self.get(Metric::RecordsFramed),
                self.get(Metric::NormalizeNanos)
            )
        ));
        out.push_str(&format!(
            "apply throughput               {:>12.0} ops/s\n",
            rate(self.get(Metric::OpsEmitted), self.get(Metric::ApplyNanos))
        ));
        out
    }
}

fn elapsed_nanos(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

fn rate(amount: u64, nanos: u64) -> f64 {
    if nanos == 0 {
        return 0.0;
    }
    amount as f64 / (nanos as f64 / 1e9)
}

#[cfg(test)]
mod tests {
    use rosetta_core::rows::{MessageKind, MessageRow};
    use rosetta_store::{CheckpointUpdate, SourceMode, SourceWrite};

    use super::*;

    #[test]
    fn counters_accumulate_and_report_renders() {
        let instrument = Instrument::new();
        instrument.add(Metric::RecordsFramed, 5);
        instrument.add(Metric::RecordsFramed, 2);
        let value = instrument.time(Metric::ReadNanos, || 42);
        assert_eq!(value, 42);
        assert_eq!(instrument.get(Metric::RecordsFramed), 7);

        let report = instrument.report();
        assert!(report.contains("records framed"));
        assert!(report.contains("7"));
        assert!(report.contains("records/s"));
        assert!(report.contains("source staging time"));
        assert!(report.contains("canonical apply time"));
        assert!(report.contains("message lookup time"));
        assert!(report.contains("pending batch checks"));
        assert!(report.contains("pending lookup time"));
        assert!(report.contains("ledger sync time"));
        assert!(report.contains("ledger directory sync time"));
        assert!(report.contains("bulk renumber time"));
        assert!(report.contains("bulk FTS time"));
        assert!(report.contains("publication time"));
    }

    #[test]
    fn snapshots_report_only_the_delta_between_adapters() {
        let instrument = Instrument::new();
        instrument.add(Metric::OpsEmitted, 5);
        let before = instrument.snapshot();
        instrument.add(Metric::OpsEmitted, 7);
        instrument.add(Metric::MessageApplyAttempts, 3);
        let delta = instrument.snapshot().delta_since(&before);

        assert_eq!(delta.get(Metric::OpsEmitted), 7);
        assert_eq!(delta.get(Metric::MessageApplyAttempts), 3);
        assert_eq!(delta.get(Metric::RecordsFramed), 0);
    }

    #[test]
    fn boundary_evidence_follows_one_native_record_into_its_real_write_batch() {
        let instrument = Instrument::new();
        instrument.enable_boundary_evidence();
        let mut messages = IngestOp::project_message(MessageRow {
            timeline_id: "timeline-1".to_string(),
            session_id: "session-1".to_string(),
            ordinal: 7,
            kind: MessageKind::Agent,
            subtype: None,
            uuid: Some("native-1".to_string()),
            parent_uuid: None,
            ts: Some(42),
            model: Some("model-1".to_string()),
            effort: None,
            content: Some("canonical content".to_string()),
            tool: None,
            attachments: Vec::new(),
            citations: Vec::new(),
            group: None,
            response: None,
            block_index: None,
            branch_tool_id: None,
            usage: None,
            agent_version: Some("1.0".to_string()),
            src_cursor: Some(9),
            src_offset: Some(12),
        });
        let message = messages.pop().expect("one canonical message");
        let outcome = LineOutcome::parsed(vec![message.clone()]);
        instrument.observe_normalization(
            "transcript",
            Some(12),
            br#"{"type":"assistant","message":"native content"}"#,
            &outcome,
        );
        instrument.observe_write_batch(&Batch {
            ops: vec![message],
            checkpoints: vec![CheckpointUpdate {
                runtime_id: "runtime-1".to_string(),
                stream: "transcript".to_string(),
                path: "session.jsonl".to_string(),
                session_id: "session-1".to_string(),
                expected: Some("old".to_string()),
                next: "new".to_string(),
                updated_at: Some(42),
            }],
            source: Some(SourceWrite {
                cursor_id: 9,
                session_id: "session-1".to_string(),
                generation: 1,
                first_sequence: 0,
                mode: SourceMode::Activate,
            }),
        });

        let evidence = instrument.boundary_evidence().unwrap();
        assert_eq!(evidence["sampleTimelineId"], "timeline-1");
        assert!(
            evidence["parsedRecord"]["data"]
                .as_str()
                .unwrap()
                .contains("native content")
        );
        assert!(
            evidence["normalizedOps"]["data"]
                .as_str()
                .unwrap()
                .contains("canonical content")
        );
        assert!(
            evidence["writeBatch"]["data"]
                .as_str()
                .unwrap()
                .contains("activate")
        );
    }
}
