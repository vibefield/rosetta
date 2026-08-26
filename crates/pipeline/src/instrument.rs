//! The profiler that ships in the binary. Cheap atomic counters per stage —
//! always on, so `rosettad ingest --profile` and the integration tests read
//! the same numbers real runs produce.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

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
    SourceStageNanos,
    CanonicalApplyNanos,
    SqliteCommitNanos,
    BulkBucketNanos,
    BulkFtsNanos,
    BulkIndexNanos,
    BulkValidateNanos,
    BulkPragmaNanos,
    PublishNanos,
}

const METRIC_COUNT: usize = 29;

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
    (Metric::SourceStageNanos, "source staging time"),
    (Metric::CanonicalApplyNanos, "canonical apply time"),
    (Metric::SqliteCommitNanos, "sqlite commit time"),
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

#[derive(Debug, Default)]
pub struct Instrument {
    values: [AtomicU64; METRIC_COUNT],
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
        self.add(metric, started.elapsed().as_nanos() as u64);
        value
    }

    /// Apply one store batch while retaining both the end-to-end time and the
    /// store's non-overlapping SQLite subphase measurements.
    pub fn apply(&self, store: &mut Store, batch: &Batch) -> Result<ApplyStats, StoreError> {
        let result = self.time(Metric::ApplyNanos, || store.apply(batch));
        if let Ok(stats) = &result {
            self.add(Metric::SourceStageNanos, stats.source_stage_nanos);
            self.add(Metric::CanonicalApplyNanos, stats.canonical_apply_nanos);
            self.add(Metric::SqliteCommitNanos, stats.sqlite_commit_nanos);
        }
        result
    }

    /// Finalize a private projection and record the independently useful
    /// bucket, FTS, index, validation, and journal-mode costs.
    pub fn finish_bulk_load(&self, store: &mut Store) -> Result<BulkLoadStats, StoreError> {
        let result = self.time(Metric::ApplyNanos, || store.finish_bulk_load());
        if let Ok(stats) = &result {
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
        let mut out = String::new();
        for (metric, name) in ALL {
            let value = self.get(metric);
            if name.ends_with("time") {
                out.push_str(&format!("{name:<22} {:>12.1} ms\n", value as f64 / 1e6));
            } else {
                out.push_str(&format!("{name:<22} {value:>12}\n"));
            }
        }
        out.push_str(&format!(
            "read throughput        {:>12.1} MiB/s\n",
            rate(self.get(Metric::BytesRead), self.get(Metric::ReadNanos)) / (1024.0 * 1024.0)
        ));
        out.push_str(&format!(
            "normalize throughput   {:>12.0} records/s\n",
            rate(
                self.get(Metric::RecordsFramed),
                self.get(Metric::NormalizeNanos)
            )
        ));
        out.push_str(&format!(
            "apply throughput       {:>12.0} ops/s\n",
            rate(self.get(Metric::OpsEmitted), self.get(Metric::ApplyNanos))
        ));
        out
    }
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
        assert!(report.contains("bulk FTS time"));
        assert!(report.contains("publication time"));
    }
}
