//! Optional maintenance after canonical publication and initial live catch-up.
//! Only a writer's immutable segment snapshot is eligible. Compression never
//! owns a canonical connection or delays the live writer's durable commits.

use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use rosetta_capture::{RecompressOutcome, SegmentCodec, recompress_segment_controlled};
use serde_json::json;

const CPU_PERCENT: u32 = 10;
const CPU_QUANTUM: Duration = Duration::from_millis(5);
const CANCEL_POLL: Duration = Duration::from_millis(100);
const READINESS_POLL: Duration = Duration::from_secs(1);

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
                lower_priority();
                let mut budget = Budget::new(stopped.clone(), ready);
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

struct Budget<F> {
    stop: Arc<AtomicBool>,
    ready: F,
    next_readiness: Instant,
    window: Instant,
    work: Duration,
    clock: WorkClock,
}

impl<F: Fn() -> bool> Budget<F> {
    fn new(stop: Arc<AtomicBool>, ready: F) -> Self {
        Self {
            stop,
            ready,
            next_readiness: Instant::now(),
            window: Instant::now(),
            work: Duration::ZERO,
            clock: WorkClock::new(),
        }
    }

    fn checkpoint(&mut self) -> io::Result<()> {
        self.cancelled()?;
        self.work += self.clock.sample();
        if self.work >= CPU_QUANTUM {
            let delay = budget_delay(self.work, self.window.elapsed());
            self.wait(delay)?;
            self.reset_window();
        }
        if Instant::now() >= self.next_readiness {
            let mut waited = false;
            // Ready includes the current canonical epoch and search revision.
            // Pause again if search needs rebuilding or catches up with writes.
            while !(self.ready)() {
                self.wait(READINESS_POLL)?;
                waited = true;
            }
            self.next_readiness = Instant::now() + READINESS_POLL;
            if waited {
                // Idle time never becomes credit for a subsequent CPU burst.
                self.reset_window();
            }
        }
        Ok(())
    }

    fn reset_window(&mut self) {
        self.clock.sample();
        self.work = Duration::ZERO;
        self.window = Instant::now();
    }

    fn cancelled(&self) -> io::Result<()> {
        if self.stop.load(Ordering::Acquire) {
            Err(io::Error::other("background ledger compression cancelled"))
        } else {
            Ok(())
        }
    }

    fn wait(&self, delay: Duration) -> io::Result<()> {
        let until = Instant::now() + delay;
        loop {
            self.cancelled()?;
            let remaining = until.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(());
            }
            thread::park_timeout(remaining.min(CANCEL_POLL));
        }
    }
}

fn budget_delay(work: Duration, elapsed: Duration) -> Duration {
    (work * (100 / CPU_PERCENT)).saturating_sub(elapsed)
}

/// macOS measures this worker's CPU, excluding descheduling and IO wait.
/// Other hosts (or clock failure) conservatively charge elapsed active time.
struct WorkClock {
    cpu: Option<Duration>,
    wall: Instant,
}

impl WorkClock {
    fn new() -> Self {
        Self {
            cpu: thread_cpu_time(),
            wall: Instant::now(),
        }
    }

    fn sample(&mut self) -> Duration {
        let wall = Instant::now();
        let cpu = thread_cpu_time();
        let work = match (self.cpu, cpu) {
            (Some(before), Some(after)) => after.saturating_sub(before),
            _ => wall.duration_since(self.wall),
        };
        self.cpu = cpu;
        self.wall = wall;
        work
    }
}

#[cfg(target_os = "macos")]
fn thread_cpu_time() -> Option<Duration> {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: valid writable timespec; this clock identifies only this thread.
    let status = unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut time) };
    (status == 0).then(|| Duration::new(time.tv_sec as u64, time.tv_nsec as u32))
}

#[cfg(not(target_os = "macos"))]
fn thread_cpu_time() -> Option<Duration> {
    None
}

fn lower_priority() {
    #[cfg(target_os = "macos")]
    {
        // Utility is below interactive work. Strict background QoS can be
        // indefinitely starved on a busy host; measured CPU pacing supplies
        // the explicit ceiling while utility priority still makes progress.
        // SAFETY: affects the calling maintenance thread only; no pointers.
        let status =
            unsafe { libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_UTILITY, 0) };
        if status != 0 {
            eprintln!(
                "ledger compression: utility QoS unavailable ({status}); CPU pacing remains active"
            );
        }
    }
}

#[cfg(test)]
mod tests;
