//! Optional maintenance after canonical publication and initial live catch-up.
//! Only a writer's immutable segment snapshot is eligible. Compression never
//! owns a canonical connection or delays the live writer's durable commits.

use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Instant;

use rosetta_capture::{RecompressOutcome, SegmentCodec, recompress_segment_controlled};
use serde_json::json;

use crate::maintenance_pace::{Pace, lower_priority, thread_cpu_time};

const CPU_PERCENT: u32 = 10;

pub(crate) struct BackgroundCompression {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl BackgroundCompression {
    pub(crate) fn start(
        segments: Vec<PathBuf>,
        ready: impl Fn() -> bool + Send + 'static,
    ) -> io::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let thread = thread::Builder::new()
            .name("rosetta-ledger-compression".into())
            .spawn(move || {
                lower_priority("ledger compression");
                let mut budget = Pace::new(stopped.clone(), CPU_PERCENT, ready);
                let mut started = None;
                let mut cpu_started = None;
                let mut rewritten = 0_u64;
                let mut bytes_before = 0_u64;
                let mut bytes_after = 0_u64;
                let mut announced = false;
                for path in segments {
                    if budget.checkpoint().is_err() {
                        break;
                    }
                    if !announced {
                        started = Some(Instant::now());
                        cpu_started = thread_cpu_time();
                        eprintln!("{}", json!({"event": "ledger-compression", "state": "started",
                            "cpuBudgetPercent": CPU_PERCENT}));
                        announced = true;
                    }
                    match recompress_segment_controlled(
                        &path, SegmentCodec::default(), false, &mut || budget.checkpoint(),
                    ) {
                        Ok(RecompressOutcome::Rewritten { before, after }) => {
                            rewritten += 1;
                            bytes_before += before;
                            bytes_after += after;
                        }
                        Ok(RecompressOutcome::Torn { .. }) => {
                            eprintln!("ledger compression: keeping torn segment {}", path.display());
                        }
                        Ok(RecompressOutcome::Unchanged { .. }) => {}
                        Err(_) if stopped.load(Ordering::Acquire) => break,
                        Err(error) => {
                            // Space/IO failures leave the original valid. A future
                            // daemon start rediscovers remaining plain segments.
                            eprintln!("ledger compression deferred: {error}");
                        }
                    }
                }
                if announced {
                    eprintln!("{}", json!({"event": "ledger-compression",
                        "state": if stopped.load(Ordering::Acquire) { "stopped" } else { "finished" },
                        "segmentsRewritten": rewritten, "bytesBefore": bytes_before,
                        "bytesAfter": bytes_after, "wallSeconds": started.map(|t| t.elapsed().as_secs_f64()),
                        "cpuSeconds": cpu_started.zip(thread_cpu_time()).map(|(before, after)| after.saturating_sub(before).as_secs_f64()),
                        "cpuBudgetPercent": CPU_PERCENT}));
                }
            })?;
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for BackgroundCompression {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread.thread().unpark();
            // Join before the caller can release the projection lock. Pacing
            // and readiness waits are interruptible, including mid-segment.
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests;
