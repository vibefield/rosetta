//! Pacing for the daemon's optional background maintenance (ledger
//! compression, image renditions): a CPU ceiling measured as this thread's
//! own CPU time, a readiness gate, macOS utility QoS, and waits a stop
//! request interrupts within [`CANCEL_POLL`].

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

const CPU_QUANTUM: Duration = Duration::from_millis(5);
pub(crate) const CANCEL_POLL: Duration = Duration::from_millis(100);
const READINESS_POLL: Duration = Duration::from_secs(1);

/// The pacer one maintenance thread checkpoints between (and within) units
/// of work. `ready` is the gate, re-checked about once a second; `|| true`
/// leaves the work ungated.
pub(crate) struct Pace<F> {
    stop: Arc<AtomicBool>,
    cpu_percent: u32,
    ready: F,
    next_readiness: Instant,
    window: Instant,
    pub(crate) work: Duration,
    clock: WorkClock,
}

impl<F: Fn() -> bool> Pace<F> {
    /// `cpu_percent` of one core, in 1..=100.
    pub(crate) fn new(stop: Arc<AtomicBool>, cpu_percent: u32, ready: F) -> Self {
        assert!((1..=100).contains(&cpu_percent), "a CPU share of one core");
        Self {
            stop,
            cpu_percent,
            ready,
            next_readiness: Instant::now(),
            window: Instant::now(),
            work: Duration::ZERO,
            clock: WorkClock::new(),
        }
    }

    /// Charge the CPU used since the last checkpoint, sleep off any excess
    /// over the budget, and wait while the gate is closed. Errs once stop is
    /// requested.
    pub(crate) fn checkpoint(&mut self) -> io::Result<()> {
        self.cancelled()?;
        self.work += self.clock.sample();
        if self.work >= CPU_QUANTUM {
            let delay = budget_delay(self.work, self.window.elapsed(), self.cpu_percent);
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

    pub(crate) fn reset_window(&mut self) {
        self.clock.sample();
        self.work = Duration::ZERO;
        self.window = Instant::now();
    }

    pub(crate) fn cancelled(&self) -> io::Result<()> {
        if self.stop.load(Ordering::Acquire) {
            Err(io::Error::other("background maintenance cancelled"))
        } else {
            Ok(())
        }
    }

    pub(crate) fn wait(&self, delay: Duration) -> io::Result<()> {
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

/// The sleep that brings `work` CPU within `elapsed` down to `cpu_percent`.
pub(crate) fn budget_delay(work: Duration, elapsed: Duration, cpu_percent: u32) -> Duration {
    (work * 100 / cpu_percent).saturating_sub(elapsed)
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
pub(crate) fn thread_cpu_time() -> Option<Duration> {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: valid writable timespec; this clock identifies only this thread.
    let status = unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut time) };
    (status == 0).then(|| Duration::new(time.tv_sec as u64, time.tv_nsec as u32))
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn thread_cpu_time() -> Option<Duration> {
    None
}

/// Utility QoS for the calling maintenance thread; `work` names it in the
/// one stderr line printed when the class is unavailable.
pub(crate) fn lower_priority(work: &str) {
    #[cfg(target_os = "macos")]
    {
        // Utility is below interactive work. Strict background QoS can be
        // indefinitely starved on a busy host; measured CPU pacing supplies
        // the explicit ceiling while utility priority still makes progress.
        // SAFETY: affects the calling maintenance thread only; no pointers.
        let status =
            unsafe { libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_UTILITY, 0) };
        if status != 0 {
            eprintln!("{work}: utility QoS unavailable ({status}); CPU pacing remains active");
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = work;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::sync::mpsc;

    #[test]
    fn budget_targets_its_share_without_banking_idle_time() {
        // Ledger compression: 10% of one core.
        assert_eq!(
            budget_delay(Duration::from_millis(5), Duration::from_millis(5), 10),
            Duration::from_millis(45),
        );
        assert_eq!(
            budget_delay(Duration::from_millis(5), Duration::from_millis(80), 10),
            Duration::ZERO,
        );
        // Image renditions: 50% of one core.
        assert_eq!(
            budget_delay(Duration::from_millis(40), Duration::from_millis(40), 50),
            Duration::from_millis(40),
        );
        assert_eq!(
            budget_delay(Duration::from_millis(40), Duration::from_millis(90), 50),
            Duration::ZERO,
        );
        let mut pace = Pace::new(Arc::new(AtomicBool::new(false)), 10, || true);
        pace.work = Duration::from_secs(1);
        pace.reset_window();
        assert_eq!(pace.work, Duration::ZERO);
    }

    #[test]
    fn stop_interrupts_cpu_budget_wait() {
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let calls = Arc::new(AtomicUsize::new(0));
        let worker_calls = calls.clone();
        let (started_tx, started_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            let pace = Pace::new(worker_stop, 10, || true);
            started_tx.send(()).unwrap();
            assert!(pace.wait(Duration::from_secs(60)).is_err());
            worker_calls.fetch_add(1, Ordering::Release);
        });
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        stop.store(true, Ordering::Release);
        worker.thread().unpark();
        worker.join().unwrap();
        assert_eq!(calls.load(Ordering::Acquire), 1);
    }

    #[test]
    fn a_closed_gate_holds_the_checkpoint_until_stop() {
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let (gated_tx, gated_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            let mut pace = Pace::new(worker_stop, 50, move || {
                let _ = gated_tx.send(());
                false
            });
            pace.checkpoint()
        });
        gated_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let started = Instant::now();
        stop.store(true, Ordering::Release);
        worker.thread().unpark();
        assert!(worker.join().unwrap().is_err());
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
