//! The profiler that ships in the binary. Cheap atomic counters per stage —
//! always on, so `rosettad ingest --profile` and the integration tests read
//! the same numbers real runs produce.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

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
}

const METRIC_COUNT: usize = 18;

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
    }
}
