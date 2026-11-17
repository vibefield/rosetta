//! The cold build's memory trace: one sample a second of this process's
//! physical footprint and resident size, and of the host's swap use and
//! cumulative pageouts, swap-ins, swap-outs and compressor pages — the
//! evidence that tells a memory-bound build from a slow one.
//!
//! Under `--json --profile` every sample is a `memory-sample` event, for as
//! long as serve runs. Otherwise the sampler runs only while a cold build
//! does, prints a compact `memory:` line at most every 10 s, and stops at
//! canonical readiness, where `milestone: memory-peak` closes the trace. A
//! probe is a few system calls and two reads of SQLite's global memory
//! statistics; anything the platform cannot report is missing (`na`, or
//! absent from the event), never an error.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};

use crate::bench;

const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);
const LINE_INTERVAL: Duration = Duration::from_secs(10);
const MIB: u64 = 1024 * 1024;

/// One probe. Cumulative counters are the host's since boot.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Sample {
    pub(crate) phys_footprint: Option<u64>,
    pub(crate) resident: Option<u64>,
    /// The kernel's own high-water mark of the footprint, where it keeps one.
    pub(crate) lifetime_peak_footprint: Option<u64>,
    pub(crate) swap_used: Option<u64>,
    pub(crate) pageouts: Option<u64>,
    pub(crate) swapins: Option<u64>,
    pub(crate) swapouts: Option<u64>,
    pub(crate) compressor_pages: Option<u64>,
    pub(crate) page_size: Option<u64>,
    /// SQLite's process-wide allocator: what it holds now and its high-water
    /// mark (the canonical RAM generation's pages among them).
    pub(crate) sqlite_memory_used: Option<u64>,
    pub(crate) sqlite_memory_highwater: Option<u64>,
}

impl Sample {
    pub(crate) fn probe() -> Self {
        let mut sample = platform::probe();
        // SAFETY: both read SQLite's global memory statistics and take no
        // pointer; a reset flag of 0 leaves the high-water mark as it is.
        let (used, highwater) = unsafe {
            (
                rusqlite::ffi::sqlite3_memory_used(),
                rusqlite::ffi::sqlite3_memory_highwater(0),
            )
        };
        sample.sqlite_memory_used = u64::try_from(used).ok();
        sample.sqlite_memory_highwater = u64::try_from(highwater).ok();
        sample
    }

    fn compressor_bytes(&self) -> Option<u64> {
        self.compressor_pages
            .zip(self.page_size)
            .map(|(pages, size)| pages.saturating_mul(size))
    }

    /// The `memory-sample` event; fields the platform lacks are left out.
    pub(crate) fn event(&self, elapsed_s: f64) -> Value {
        let mut event = Map::new();
        event.insert("event".into(), json!("memory-sample"));
        event.insert("elapsedS".into(), json!(elapsed_s));
        for (key, value) in [
            ("physFootprintBytes", self.phys_footprint),
            ("residentBytes", self.resident),
            ("lifetimePeakFootprintBytes", self.lifetime_peak_footprint),
            ("swapUsedBytes", self.swap_used),
            ("pageouts", self.pageouts),
            ("swapins", self.swapins),
            ("swapouts", self.swapouts),
            ("compressorPages", self.compressor_pages),
            ("compressorBytes", self.compressor_bytes()),
            ("sqliteMemoryUsedBytes", self.sqlite_memory_used),
            ("sqliteMemoryHighwaterBytes", self.sqlite_memory_highwater),
        ] {
            if let Some(value) = value {
                event.insert(key.into(), json!(value));
            }
        }
        Value::Object(event)
    }

    /// The compact stdout line; cumulative counters as deltas since `base`.
    pub(crate) fn line(&self, base: &Sample, elapsed_s: f64) -> String {
        format!(
            "memory: elapsed_s={elapsed_s:.2} phys_footprint_mb={} resident_mb={} swap_used_mb={} compressor_mb={} pageouts={} swapouts={}",
            mib(self.phys_footprint),
            mib(self.resident),
            mib(self.swap_used),
            mib(self.compressor_bytes()),
            delta(self.pageouts, base.pageouts),
            delta(self.swapouts, base.swapouts),
        )
    }
}

fn mib(bytes: Option<u64>) -> String {
    bytes.map_or_else(|| "na".to_owned(), |bytes| (bytes / MIB).to_string())
}

fn delta(now: Option<u64>, base: Option<u64>) -> String {
    now.zip(base).map_or_else(
        || "na".to_owned(),
        |(now, base)| now.saturating_sub(base).to_string(),
    )
}

/// The highest values seen so far.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Peak {
    pub(crate) phys_footprint: Option<u64>,
    pub(crate) resident: Option<u64>,
    pub(crate) swap_used: Option<u64>,
}

impl Peak {
    pub(crate) fn record(&mut self, sample: &Sample) {
        let footprint = sample
            .phys_footprint
            .into_iter()
            .chain(sample.lifetime_peak_footprint)
            .max();
        self.phys_footprint = self.phys_footprint.max(footprint);
        self.resident = self.resident.max(sample.resident);
        self.swap_used = self.swap_used.max(sample.swap_used);
    }

    /// `milestone: memory-peak`, with the host's swap-outs and pageouts
    /// since `base`.
    pub(crate) fn line(&self, now: &Sample, base: &Sample, elapsed_s: f64) -> String {
        format!(
            "milestone: memory-peak phys_footprint_mb={} resident_mb={} swap_used_mb={} swapouts={} pageouts={} elapsed_s={elapsed_s:.2}",
            mib(self.phys_footprint),
            mib(self.resident),
            mib(self.swap_used),
            delta(now.swapouts, base.swapouts),
            delta(now.pageouts, base.pageouts),
        )
    }
}

struct Shared {
    started: Instant,
    /// Every sample becomes an event, for as long as serve runs.
    events: bool,
    /// A cold build runs: compact lines print, and the sampler lives on.
    cold: AtomicBool,
    base: Sample,
    peak: Mutex<Peak>,
    stopped: Mutex<bool>,
    wake: Condvar,
}

/// The sampler serve runs, reachable from every path's canonical-ready.
static ACTIVE: Mutex<Option<Arc<Shared>>> = Mutex::new(None);

/// The running sampler thread; dropping it stops and joins the thread.
pub(crate) struct MemorySampler {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl MemorySampler {
    /// Sample while a `cold` build runs, or for the whole serve when
    /// `events` wants every sample; neither, and nothing runs. Serve's
    /// `started` is the base every elapsed time reports from.
    pub(crate) fn start(started: Instant, cold: bool, events: bool) -> Option<Self> {
        if !cold && !events {
            return None;
        }
        let base = Sample::probe();
        let mut peak = Peak::default();
        peak.record(&base);
        let shared = Arc::new(Shared {
            started,
            events,
            cold: AtomicBool::new(cold),
            base,
            peak: Mutex::new(peak),
            stopped: Mutex::new(false),
            wake: Condvar::new(),
        });
        let worker = shared.clone();
        let thread = std::thread::Builder::new()
            .name("rosettad-memory".into())
            .spawn(move || run(&worker))
            .map_err(|error| eprintln!("memory sampler unavailable: {error}"))
            .ok()?;
        *ACTIVE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(shared.clone());
        Some(Self {
            shared,
            thread: Some(thread),
        })
    }
}

impl Drop for MemorySampler {
    fn drop(&mut self) {
        let mut active = ACTIVE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if active
            .as_ref()
            .is_some_and(|shared| Arc::ptr_eq(shared, &self.shared))
        {
            *active = None;
        }
        drop(active);
        *self
            .shared
            .stopped
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = true;
        self.shared.wake.notify_all();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run(shared: &Shared) {
    let mut last_line = Instant::now();
    loop {
        let stopped = shared
            .stopped
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (stopped, _) = shared
            .wake
            .wait_timeout_while(stopped, SAMPLE_INTERVAL, |stopped| !*stopped)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if *stopped {
            return;
        }
        drop(stopped);
        let cold = shared.cold.load(Ordering::SeqCst);
        if !cold && !shared.events {
            return;
        }
        let sample = Sample::probe();
        shared
            .peak
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .record(&sample);
        let elapsed_s = shared.started.elapsed().as_secs_f64();
        if shared.events {
            bench::emit(&sample.event(elapsed_s));
        } else if last_line.elapsed() >= LINE_INTERVAL {
            last_line = Instant::now();
            println!("{}", sample.line(&shared.base, elapsed_s));
        }
    }
}

/// Canonical readiness ends the cold build's trace: one closing sample, then
/// `milestone: memory-peak`. Without a running sampler (it could not start)
/// the peak is the closing sample's, with the kernel's own high-water mark,
/// and the host counters since the start are `na`.
pub(crate) fn canonical_ready(started: Instant) {
    let now = Sample::probe();
    let shared = ACTIVE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    let (mut peak, base) = match &shared {
        Some(shared) => {
            shared.cold.store(false, Ordering::SeqCst);
            let peak = *shared
                .peak
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            (peak, shared.base)
        }
        None => (Peak::default(), Sample::default()),
    };
    // Without events the sampler sees the build is over and exits on its
    // next tick.
    peak.record(&now);
    println!(
        "{}",
        peak.line(&now, &base, started.elapsed().as_secs_f64())
    );
}

#[cfg(target_os = "macos")]
mod platform {
    use std::sync::OnceLock;

    use super::Sample;

    pub(super) fn probe() -> Sample {
        let mut sample = Sample::default();
        process(&mut sample);
        swap(&mut sample);
        host_vm(&mut sample);
        // SAFETY: sysconf reads a constant system value; no pointers.
        let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        sample.page_size = u64::try_from(page_size).ok().filter(|size| *size > 0);
        sample
    }

    /// This process's footprint and resident size (what Activity Monitor's
    /// "Memory" column and `ps -o rss` show).
    fn process(sample: &mut Sample) {
        // SAFETY: `rusage_info_v4` is a plain C struct of integers, for which
        // all-zero bytes are a valid value.
        let mut info: libc::rusage_info_v4 = unsafe { std::mem::zeroed() };
        // SAFETY: `info` is a writable `rusage_info_v4`, the struct the
        // RUSAGE_INFO_V4 flavor names, so the kernel writes at most its size;
        // the pid is this process's own.
        let status = unsafe {
            libc::proc_pid_rusage(
                libc::getpid(),
                libc::RUSAGE_INFO_V4,
                (&mut info as *mut libc::rusage_info_v4).cast(),
            )
        };
        if status == 0 {
            sample.phys_footprint = Some(info.ri_phys_footprint);
            sample.resident = Some(info.ri_resident_size);
            sample.lifetime_peak_footprint = Some(info.ri_lifetime_max_phys_footprint);
        }
    }

    /// The host's swap in use (`sysctl vm.swapusage`).
    fn swap(sample: &mut Sample) {
        // SAFETY: `xsw_usage` is a plain C struct of integers; all-zero bytes
        // are a valid value.
        let mut usage: libc::xsw_usage = unsafe { std::mem::zeroed() };
        let mut size = std::mem::size_of::<libc::xsw_usage>();
        // SAFETY: the name is NUL-terminated; `usage` is writable for `size`
        // bytes, which the kernel never exceeds and reports back; no new
        // value is set (null pointer, zero length).
        let status = unsafe {
            libc::sysctlbyname(
                c"vm.swapusage".as_ptr(),
                (&mut usage as *mut libc::xsw_usage).cast(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        if status == 0 && size == std::mem::size_of::<libc::xsw_usage>() {
            sample.swap_used = Some(usage.xsu_used);
        }
    }

    /// The host's cumulative paging counters and compressor occupancy
    /// (`vm_stat`), from `host_statistics64(HOST_VM_INFO64)`.
    fn host_vm(sample: &mut Sample) {
        // SAFETY: `vm_statistics64` is a plain C struct of integers; all-zero
        // bytes are a valid value.
        let mut vm: libc::vm_statistics64 = unsafe { std::mem::zeroed() };
        let mut count = libc::HOST_VM_INFO64_COUNT;
        // SAFETY: `host_port()` is this process's host name port; `vm` is
        // writable for `count` integer_t units, which the kernel never
        // exceeds and lowers to what it filled when its struct is smaller.
        let status = unsafe {
            libc::host_statistics64(
                host_port(),
                libc::HOST_VM_INFO64,
                (&mut vm as *mut libc::vm_statistics64).cast(),
                &mut count,
            )
        };
        if status == libc::KERN_SUCCESS {
            let (pageouts, swapins, swapouts, compressor) = (
                vm.pageouts,
                vm.swapins,
                vm.swapouts,
                vm.compressor_page_count,
            );
            sample.pageouts = Some(pageouts);
            sample.swapins = Some(swapins);
            sample.swapouts = Some(swapouts);
            sample.compressor_pages = Some(u64::from(compressor));
        }
    }

    /// Each `mach_host_self` call adds a reference to the host port's send
    /// right, so the port is taken once and held for the process's life.
    fn host_port() -> libc::mach_port_t {
        static HOST: OnceLock<libc::mach_port_t> = OnceLock::new();
        *HOST.get_or_init(|| {
            // SAFETY: no arguments; returns a send right to this host's name
            // port, taken once (above) and never released.
            #[allow(deprecated)]
            unsafe {
                libc::mach_host_self()
            }
        })
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    //! Best effort from Linux procfs; other systems report nothing.

    use super::Sample;

    const KIB: u64 = 1024;

    pub(super) fn probe() -> Sample {
        let mut sample = Sample::default();
        if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
            sample.resident = field(&status, "VmRSS:").map(|kib| kib * KIB);
        }
        if let Ok(meminfo) = std::fs::read_to_string("/proc/meminfo") {
            sample.swap_used = field(&meminfo, "SwapTotal:")
                .zip(field(&meminfo, "SwapFree:"))
                .map(|(total, free)| total.saturating_sub(free) * KIB);
        }
        if let Ok(vmstat) = std::fs::read_to_string("/proc/vmstat") {
            sample.swapins = field(&vmstat, "pswpin");
            sample.swapouts = field(&vmstat, "pswpout");
        }
        sample
    }

    /// The first number after `key` at the start of a line.
    fn field(text: &str, key: &str) -> Option<u64> {
        text.lines()
            .find_map(|line| line.strip_prefix(key))
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|value| value.parse().ok())
    }
}
