//! The profiler that ships in the binary. Cheap atomic counters per stage —
//! always on, so `rosettad ingest --profile` and the integration tests read
//! the same numbers real runs produce.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use rosetta_capture::{Ledger, LedgerWriteStats};
use rosetta_store::{ApplyStats, Batch, BulkLoadStats, Store, StoreError};

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
    ReadNanos,
    NormalizeNanos,
    ApplyNanos,
    LedgerReadNanos,
    LedgerWriteNanos,
    LedgerFrames,
    LedgerFrameBytes,
    LedgerFlushes,
    LedgerSyncs,
    LedgerSegmentsOpened,
    LedgerElectionReadNanos,
    LedgerReplayReadNanos,
    LedgerFrameNanos,
    LedgerBufferWriteNanos,
    LedgerFlushNanos,
    LedgerSyncNanos,
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
    BulkRenumberRows,
    BulkRenumberNanos,
    BulkBucketNanos,
    BulkFtsNanos,
    BulkIndexNanos,
    BulkValidateNanos,
    BulkPragmaNanos,
    PublishNanos,
}

const METRIC_COUNT: usize = 72;

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
    (Metric::ReadNanos, "read time"),
    (Metric::NormalizeNanos, "normalize time"),
    (Metric::ApplyNanos, "apply time"),
    (Metric::LedgerReadNanos, "ledger read time"),
    (Metric::LedgerWriteNanos, "ledger write time"),
    (Metric::LedgerFrames, "ledger frames"),
    (Metric::LedgerFrameBytes, "ledger frame bytes"),
    (Metric::LedgerFlushes, "ledger flushes"),
    (Metric::LedgerSyncs, "ledger syncs"),
    (Metric::LedgerSegmentsOpened, "ledger segments opened"),
    (Metric::LedgerElectionReadNanos, "ledger election read time"),
    (Metric::LedgerReplayReadNanos, "ledger replay read time"),
    (Metric::LedgerFrameNanos, "ledger framing time"),
    (Metric::LedgerBufferWriteNanos, "ledger buffered write time"),
    (Metric::LedgerFlushNanos, "ledger flush time"),
    (Metric::LedgerSyncNanos, "ledger sync time"),
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
    (Metric::BulkRenumberRows, "bulk renumber rows"),
    (Metric::BulkRenumberNanos, "bulk renumber time"),
    (Metric::BulkBucketNanos, "bulk bucket time"),
    (Metric::BulkFtsNanos, "bulk FTS time"),
    (Metric::BulkIndexNanos, "bulk index time"),
    (Metric::BulkValidateNanos, "bulk validate time"),
    (Metric::BulkPragmaNanos, "bulk pragma time"),
    (Metric::PublishNanos, "publication time"),
];

fn index(metric: Metric) -> usize {
    ALL.iter()
        .position(|(candidate, _)| *candidate == metric)
        .expect("every metric is registered")
}

#[derive(Debug)]
pub struct Instrument {
    values: [AtomicU64; METRIC_COUNT],
}

impl Default for Instrument {
    fn default() -> Self {
        Self {
            values: std::array::from_fn(|_| AtomicU64::new(0)),
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
        self.add(Metric::LedgerSegmentsOpened, stats.segments_opened);
        self.add(Metric::LedgerFrameNanos, stats.frame_nanos);
        self.add(Metric::LedgerBufferWriteNanos, stats.buffer_write_nanos);
        self.add(Metric::LedgerFlushNanos, stats.flush_nanos);
        self.add(Metric::LedgerSyncNanos, stats.sync_nanos);
        self.add(Metric::LedgerSegmentOpenNanos, stats.segment_open_nanos);
    }

    /// Apply one store batch while retaining both the end-to-end time and the
    /// store's non-overlapping SQLite subphase measurements.
    pub fn apply(&self, store: &mut Store, batch: &Batch) -> Result<ApplyStats, StoreError> {
        let result = self.time(Metric::ApplyNanos, || store.apply(batch));
        if let Ok(stats) = &result {
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
        }
        result
    }

    /// Finalize a private projection and record the independently useful
    /// bucket, FTS, index, validation, and journal-mode costs.
    pub fn finish_bulk_load(&self, store: &mut Store) -> Result<BulkLoadStats, StoreError> {
        let result = self.time(Metric::ApplyNanos, || store.finish_bulk_load());
        if let Ok(stats) = &result {
            self.add(Metric::BulkRenumberRows, stats.renumber_rows);
            self.add(Metric::BulkRenumberNanos, stats.renumber_nanos);
            self.add(Metric::BulkBucketNanos, stats.bucket_nanos);
            self.add(Metric::BulkFtsNanos, stats.fts_nanos);
            self.add(Metric::BulkIndexNanos, stats.index_nanos);
            self.add(Metric::SqliteCommitNanos, stats.commit_nanos);
            self.add(Metric::BulkValidateNanos, stats.validate_nanos);
            self.add(Metric::BulkPragmaNanos, stats.pragma_nanos);
        }
        result
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
}
