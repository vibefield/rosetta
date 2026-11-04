//! The process-wide capture pool: `P` workers shared by every runtime of one
//! build, with look-ahead bounded in bytes.
//!
//! A task is one source's pass. A worker takes a runnable task, runs ONE
//! step of it, queues the output on the source, and hands the pass back.
//! Output waits on its source until the runtime's sequencer consumes it, in
//! path order, so the output of every source but the one it consumes next
//! (its head) is look-ahead. Three bounds apply, in bytes of queued output
//! (frame bytes plus framed payload, the proxy for its normalized ops): `G`
//! over all runtimes' look-ahead; a share of `G` per runtime, so one runtime
//! stuck behind a giant head cannot starve the others; and `H` over the
//! head's own queue. A look-ahead step still running counts against them by
//! the payload it may still read, so workers starting steps together cannot
//! overshoot them by a batch each. A task over its bound is PARKED: it keeps
//! its pass and workers take other work.
//!
//! Workers wait only for a runnable task, a step is one bounded tail batch,
//! and the head is runnable whenever its queue is empty — ahead of anything
//! else. A sequencer waiting on its head therefore always gets it: no wait
//! points back into the pool. And when a sequencer is blocked downstream
//! (the writer's budget, its ledger queue, an inline group), its look-ahead
//! fills, its tasks park, and the workers go elsewhere or idle.
//!
//! A pool runtime is one sequencer lane: a runtime admitted whole registers
//! one, a root-tree runtime one per lane that has pool units. A lane is
//! `Shared` (HEAD's rule: a share of what `G` leaves) or `Reserved` (a root-
//! tree runtime's small lane: a fixed slice of `G`, so big-lane overshoot
//! cannot starve it). Head grants are fair: among runnable heads the one
//! granted least recently goes first, so a lane's head never waits behind
//! another lane's head that keeps becoming runnable again.

use std::collections::{BTreeSet, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use rosetta_adapter_support::AgentAdapter;
use rosetta_capture::{PrebuiltFrames, Tailer};
use rosetta_core::rows::RuntimeRow;
use rosetta_store::Batch;
use rosetta_store::queries::CursorState;

use super::CaptureClock;
use crate::instrument::{Instrument, InstrumentSnapshot};
use crate::meta::SessionFile;
use crate::stream::{FilePass, IngestPass, ReportDelta};
use crate::{IngestOptions, PipelineError};

mod lanes;
use lanes::Bounds;
mod step;
use step::run_step;

const MIB: u64 = 1024 * 1024;
const POISONED: &str = "capture pool poisoned";
/// How long a waiting sequencer sleeps before it rechecks cancellation.
pub(crate) const WAIT_SLICE: Duration = Duration::from_millis(100);

/// The pool's look-ahead bounds, in bytes of queued output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PoolBudgets {
    /// `G`: output queued on every source a sequencer does not consume next.
    pub look_ahead_bytes: u64,
    /// The floor of one runtime's share of `G`: the share is
    /// `max(floor, G / runtimes with sources left to step)`.
    pub runtime_floor_bytes: u64,
    /// `H`: output queued on the source a sequencer consumes next. It binds
    /// only while that sequencer is blocked downstream.
    pub head_bytes: u64,
}

impl PoolBudgets {
    pub const DEFAULT: Self = Self {
        look_ahead_bytes: 512 * MIB,
        runtime_floor_bytes: 64 * MIB,
        head_bytes: 64 * MIB,
    };

    /// A `Reserved` lane's slice of `G`: 64 MiB at the default bounds.
    pub fn small_lane_bytes(&self) -> u64 {
        self.runtime_floor_bytes
            .min(self.look_ahead_bytes / 8)
            .max(1)
    }
}

/// How a lane's look-ahead is bounded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LaneClass {
    /// A share of what the reserved slices leave of `G`.
    Shared,
    /// A fixed slice of `G` ([`PoolBudgets::small_lane_bytes`]).
    Reserved,
}

impl Default for PoolBudgets {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// What the pool's workers did over its lifetime.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PoolStats {
    pub runtimes: u64,
    pub steps: u64,
    /// Worker time inside steps.
    pub busy_nanos: u64,
    /// Worker time waiting while sources had steps left, all over their
    /// bounds: capture held back by its sequencers.
    pub parked_nanos: u64,
    /// Worker time waiting while the only steps left belonged to sources
    /// other workers were stepping: the tail of a few large sources.
    pub tail_nanos: u64,
    /// Worker time waiting with no registered source left to step: before
    /// the first runtime registers and after capture ends.
    pub idle_nanos: u64,
    pub peak_look_ahead_bytes: u64,
    pub peak_head_bytes: u64,
    /// The most look-ahead `Shared` lanes held at once.
    pub peak_shared_look_ahead_bytes: u64,
    /// The most look-ahead `Reserved` lanes held at once.
    pub peak_reserved_look_ahead_bytes: u64,
    /// The most output queued on every lane's head at once.
    pub peak_head_bytes_total: u64,
}

/// What workers read to step one runtime's sources.
pub(crate) struct Shared {
    pub(crate) adapter: &'static dyn AgentAdapter,
    pub(crate) runtime: RuntimeRow,
    pub(crate) root: PathBuf,
    pub(crate) opts: IngestOptions,
    pub(crate) tailer: Tailer,
    /// A raw ledger is attached, so steps finish each batch's frames.
    pub(crate) frames: bool,
    /// Its segments are zstd frames: steps also compress each batch's
    /// frames, at this level.
    pub(crate) compress: Option<i32>,
    /// The sources the pool captures, in path order.
    pub(crate) units: Vec<UnitSpec>,
}

pub(crate) struct UnitSpec {
    pub(crate) file: SessionFile,
    /// The fresh cursor the source's pass begins from.
    pub(crate) cursor: CursorState,
    /// The source's roster key: its ledger file id in every segment.
    pub(crate) key: u32,
}

/// What a step leaves on its source for the sequencer.
pub(crate) enum Event {
    /// One batch for the raw ledger, then the planner.
    Batch(Box<Output>),
    /// The pass ended without another batch.
    Finished {
        outcome: FilePass,
        deltas: ReportDelta,
        profile: Box<InstrumentSnapshot>,
        /// The step that found the end (measurement only).
        clock: CaptureClock,
    },
    /// The step failed or panicked; the source takes no more steps.
    Failed(PipelineError),
}

pub(crate) struct Output {
    /// The batch's finished frames when a ledger is attached — empty when
    /// the batch holds nothing new for it.
    pub(crate) frames: Option<PrebuiltFrames>,
    pub(crate) batch: Batch,
    pub(crate) deltas: ReportDelta,
    /// The worker instrument's counters for this step.
    pub(crate) profile: InstrumentSnapshot,
    /// The step's start, wall and largest record (measurement only).
    pub(crate) clock: CaptureClock,
    /// `Some` when the pass ends once the planner accepted this batch.
    pub(crate) ends: Option<FilePass>,
}

impl Event {
    /// Bytes held against the look-ahead bounds: compressed frames count
    /// what they hold.
    fn bytes(&self) -> u64 {
        match self {
            Self::Batch(output) => output
                .frames
                .as_ref()
                .map_or(0, PrebuiltFrames::held_bytes)
                .saturating_add(output.deltas.payload_bytes)
                .saturating_add(
                    output
                        .batch
                        .ops
                        .iter()
                        .filter_map(|op| match op {
                            rosetta_core::IngestOp::ProjectionAsset { asset } => {
                                Some(asset.materialized_byte_len() as u64)
                            }
                            _ => None,
                        })
                        .sum::<u64>(),
                ),
            Self::Finished { .. } | Self::Failed(_) => 0,
        }
    }
}

pub struct CapturePool {
    workers: usize,
    budgets: PoolBudgets,
    /// The test scheduler: seeded random picks among runnable tasks and
    /// random short pauses.
    chaos: bool,
    state: Mutex<State>,
    /// Workers wait here for a runnable task.
    work: Condvar,
    /// Sequencers wait here for their head's output.
    output: Condvar,
}

struct State {
    /// Registered runtimes by registration order; `None` once withdrawn.
    runtimes: Vec<Option<Runtime>>,
    closed: bool,
    /// `G`'s account: output queued on sources that are not their head.
    look_ahead: u64,
    /// The part of `look_ahead` that `Shared` lanes hold.
    shared_look_ahead: u64,
    /// What `Shared` lanes' look-ahead steps still running may add.
    shared_running: u64,
    /// Output queued on every lane's head.
    head_bytes: u64,
    /// Head grants so far: the stamp of the latest.
    head_grants: u64,
    /// Bumped by [`CapturePool::notify_output`], so a waiter that read it
    /// before a release cannot miss the wake.
    output_epoch: u64,
    idle_workers: usize,
    rng: u64,
    stats: PoolStats,
}

struct Runtime {
    shared: Arc<Shared>,
    units: Vec<Source>,
    /// The source the sequencer consumes next.
    head: usize,
    /// Sources are dispatched in path order; this is the next one.
    next_dispatch: usize,
    /// Dispatched sources whose pass waits for a worker.
    ready: BTreeSet<usize>,
    running: usize,
    /// This runtime's share of `G`'s account.
    look_ahead: u64,
    /// What its look-ahead steps still running may add to it.
    running_reserved: u64,
    /// Source bytes not read yet — the scheduler's estimate of work left.
    remaining: u64,
    class: LaneClass,
    /// The pool's head-grant stamp when this lane's head was last granted.
    last_head_grant: u64,
    /// Since when the head's queue holds output the sequencer has not taken.
    ready_since: Option<Instant>,
}

#[derive(Default)]
struct Source {
    pass: Option<IngestPass>,
    /// No more steps: its last event is queued, or its sequencer moved on.
    finished: bool,
    /// The sequencer moved on; a step still running is discarded.
    abandoned: bool,
    queue: VecDeque<(u64, Event)>,
    queued: u64,
    unread: u64,
    /// What its running look-ahead step may add, counted against the bounds
    /// until the step returns.
    reserved: u64,
}

struct Task {
    runtime: usize,
    unit: usize,
    shared: Arc<Shared>,
    /// `None` on the source's first step: the worker begins the pass.
    pass: Option<IngestPass>,
}

/// What a worker with nothing runnable is waiting for.
enum Waiting {
    /// Steps are left, all over their look-ahead bounds.
    Parked,
    /// The only steps left are other workers' sources.
    Tail,
    /// No registered source has a step left.
    Idle,
}

/// Closes the pool however its owner's scope ends, so its workers exit.
struct CloseOnDrop<'p>(&'p CapturePool);

impl Drop for CloseOnDrop<'_> {
    fn drop(&mut self) {
        self.0.close();
    }
}

impl CapturePool {
    pub fn new(workers: usize, budgets: PoolBudgets) -> Self {
        Self::build(workers, budgets, None)
    }

    /// The pool size a fresh cold build uses unless told otherwise: one
    /// worker per core but one, which is left to the sole canonical writer
    /// (docs/ARCHITECTURE.md, fresh-serve checkpoint, for the sweep).
    pub fn default_workers() -> usize {
        std::thread::available_parallelism()
            .map_or(1, std::num::NonZeroUsize::get)
            .saturating_sub(1)
            .max(1)
    }

    /// The equivalence tests' scheduler: every bound one byte, so nearly
    /// every task parks, picks random among runnable tasks, and random short
    /// pauses between them. Tests only (the integration harness too); no
    /// flag reaches it.
    #[doc(hidden)]
    pub fn chaos(workers: usize, seed: u64) -> Self {
        let budgets = PoolBudgets {
            look_ahead_bytes: 1,
            runtime_floor_bytes: 1,
            head_bytes: 1,
        };
        Self::build(workers, budgets, Some(seed))
    }

    fn build(workers: usize, budgets: PoolBudgets, seed: Option<u64>) -> Self {
        Self {
            workers,
            budgets,
            chaos: seed.is_some(),
            state: Mutex::new(State {
                runtimes: Vec::new(),
                closed: false,
                look_ahead: 0,
                shared_look_ahead: 0,
                shared_running: 0,
                head_bytes: 0,
                head_grants: 0,
                output_epoch: 0,
                idle_workers: 0,
                // xorshift must not start at zero.
                rng: seed.unwrap_or(0) | 1,
                stats: PoolStats::default(),
            }),
            work: Condvar::new(),
            output: Condvar::new(),
        }
    }

    pub fn workers(&self) -> usize {
        self.workers
    }

    pub fn budgets(&self) -> PoolBudgets {
        self.budgets
    }

    pub fn stats(&self) -> PoolStats {
        self.lock().stats
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().expect(POISONED)
    }

    /// Run `body` with this pool's workers on scoped threads; the pool is
    /// closed when `body` returns or unwinds.
    pub fn scoped<T>(&self, body: impl FnOnce() -> T) -> T {
        std::thread::scope(|scope| {
            let _close = CloseOnDrop(self);
            for index in 0..self.workers {
                std::thread::Builder::new()
                    .name(format!("capture-{index}"))
                    .spawn_scoped(scope, || self.work())
                    .expect("spawning a capture worker");
            }
            body()
        })
    }

    /// Stop the workers after their current step and fail every sequencer
    /// still waiting. Idempotent.
    pub fn close(&self) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.closed = true;
        self.work.notify_all();
        self.output.notify_all();
    }

    /// One worker: run steps until the pool is closed. Spawn
    /// [`CapturePool::workers`] of these.
    pub fn work(&self) {
        // Each worker counts into its own instrument and ships each step's
        // delta with its output, so no two workers share a cache line.
        let instrument = Instrument::new();
        let mut compressor = None;
        let mut state = self.lock();
        loop {
            let task = loop {
                if state.closed {
                    return;
                }
                if let Some(task) = state.pick(&self.budgets, self.chaos) {
                    if state.idle_workers > 0 {
                        // More may be runnable; a woken worker passes it on.
                        self.work.notify_one();
                    }
                    break task;
                }
                let waiting = state.waiting();
                state.idle_workers += 1;
                let waited = Instant::now();
                state = self.work.wait(state).expect(POISONED);
                state.idle_workers -= 1;
                let nanos = elapsed_nanos(waited);
                match waiting {
                    Waiting::Parked => state.stats.parked_nanos += nanos,
                    Waiting::Tail => state.stats.tail_nanos += nanos,
                    Waiting::Idle => state.stats.idle_nanos += nanos,
                }
            };
            let pause = if self.chaos { state.random() % 4 } else { 0 };
            drop(state);
            if pause > 0 {
                std::thread::sleep(Duration::from_micros(pause * 50));
            }
            let started = Instant::now();
            let (pass, event) = run_step(
                &task.shared,
                task.unit,
                task.pass,
                &instrument,
                &mut compressor,
            );
            let nanos = elapsed_nanos(started);
            state = self.lock();
            state.stats.steps += 1;
            state.stats.busy_nanos += nanos;
            if state.returned(task.runtime, task.unit, pass, event) {
                self.output.notify_all();
            }
        }
    }

    /// Register one lane's sources: a runtime admitted whole is one
    /// `Shared` lane. Its sequencer consumes them through the returned lane,
    /// which withdraws them when dropped. What is left to read of each
    /// source is its [`SessionFile::read_len`]: a root-tree pass stops at
    /// the metadata cut.
    pub(crate) fn register_lane(&self, shared: Shared, class: LaneClass) -> Lane<'_> {
        let units = shared
            .units
            .iter()
            .map(|unit| Source {
                unread: unit.file.read_len(),
                ..Source::default()
            })
            .collect();
        let remaining = shared.units.iter().map(|unit| unit.file.read_len()).sum();
        let mut state = self.lock();
        let runtime = state.runtimes.len();
        state.runtimes.push(Some(Runtime {
            shared: Arc::new(shared),
            units,
            head: 0,
            next_dispatch: 0,
            ready: BTreeSet::new(),
            running: 0,
            look_ahead: 0,
            running_reserved: 0,
            remaining,
            class,
            last_head_grant: 0,
            ready_since: None,
        }));
        state.stats.runtimes += 1;
        if state.idle_workers > 0 {
            self.work.notify_one();
        }
        Lane {
            pool: self,
            runtime,
        }
    }
}

impl State {
    fn random(&mut self) -> u64 {
        let mut value = self.rng;
        value ^= value << 13;
        value ^= value >> 7;
        value ^= value << 17;
        self.rng = value;
        value
    }

    fn registered(&self) -> impl Iterator<Item = (usize, &Runtime)> {
        self.runtimes
            .iter()
            .enumerate()
            .filter_map(|(index, runtime)| Some((index, runtime.as_ref()?)))
    }

    /// Why a worker that found nothing runnable waits.
    fn waiting(&self) -> Waiting {
        let mut tail = false;
        for (_, runtime) in self.registered() {
            if !runtime.ready.is_empty() || runtime.next_dispatch < runtime.units.len() {
                return Waiting::Parked;
            }
            tail |= runtime.running > 0;
        }
        if tail { Waiting::Tail } else { Waiting::Idle }
    }

    fn pick(&mut self, budgets: &PoolBudgets, chaos: bool) -> Option<Task> {
        let bounds = self.bounds(budgets);
        let (runtime, unit) = if chaos {
            let mut runnable = Vec::new();
            for (index, runtime) in self.registered() {
                if let Some(unit) = runtime.runnable_head(budgets.head_bytes) {
                    runnable.push((index, unit));
                }
                if bounds.eligible(runtime) {
                    runnable.extend(
                        runtime
                            .ready
                            .iter()
                            .filter(|unit| **unit != runtime.head)
                            .map(|unit| (index, *unit)),
                    );
                    if let Some(unit) = runtime.next_look_ahead() {
                        runnable.push((index, unit));
                    }
                }
            }
            if runnable.is_empty() {
                return None;
            }
            let choice = (self.random() % runnable.len() as u64) as usize;
            runnable[choice]
        } else {
            self.by_priority(budgets.head_bytes, &bounds)?
        };
        Some(self.take(runtime, unit))
    }

    fn by_priority(&self, head_bytes: u64, bounds: &Bounds) -> Option<(usize, usize)> {
        // 1. A runnable head: the lane granted a head least recently, then
        //    the one with the most left.
        let head = self
            .registered()
            .filter_map(|(index, runtime)| {
                Some((index, runtime.runnable_head(head_bytes)?, runtime))
            })
            .min_by(|a, b| {
                a.2.last_head_grant
                    .cmp(&b.2.last_head_grant)
                    .then(b.2.remaining.cmp(&a.2.remaining))
                    .then(a.0.cmp(&b.0))
            });
        if let Some((index, unit, _)) = head {
            return Some((index, unit));
        }
        let candidates: Vec<(usize, usize, &Runtime)> = self
            .registered()
            .filter(|(_, runtime)| bounds.eligible(runtime))
            .filter_map(|(index, runtime)| Some((index, runtime.look_ahead_candidate()?, runtime)))
            .collect();
        // 2. A runtime nothing is stepping, the one with the least left
        //    first, so every runtime makes progress from the start.
        let idle = candidates
            .iter()
            .filter(|(_, _, runtime)| runtime.running == 0)
            .min_by(|a, b| a.2.remaining.cmp(&b.2.remaining).then(a.0.cmp(&b.0)));
        if let Some((index, unit, _)) = idle {
            return Some((*index, *unit));
        }
        // 3. The runtime with the most left per worker already on it.
        candidates
            .iter()
            .max_by(|a, b| {
                let left = u128::from(a.2.remaining) * (b.2.running as u128 + 1);
                let right = u128::from(b.2.remaining) * (a.2.running as u128 + 1);
                left.cmp(&right).then(b.0.cmp(&a.0))
            })
            .map(|(index, unit, _)| (*index, *unit))
    }

    fn take(&mut self, runtime: usize, unit: usize) -> Task {
        let entry = self.runtimes[runtime]
            .as_mut()
            .expect("a picked runtime is registered");
        if unit == entry.head {
            self.head_grants += 1;
            entry.last_head_grant = self.head_grants;
        }
        let pass = if unit == entry.next_dispatch {
            entry.next_dispatch += 1;
            None
        } else {
            entry.ready.remove(&unit);
            Some(
                entry.units[unit]
                    .pass
                    .take()
                    .expect("a parked source keeps its pass"),
            )
        };
        entry.running += 1;
        if unit != entry.head {
            entry.reserve(unit, &mut self.shared_running);
        }
        Task {
            runtime,
            unit,
            shared: Arc::clone(&entry.shared),
            pass,
        }
    }

    /// Take a worker's step back: queue its event on the source, and park
    /// the pass or close the source. `true` when the source is its runtime's
    /// head, whose sequencer may be waiting for it.
    fn returned(
        &mut self,
        runtime: usize,
        unit: usize,
        pass: Option<IngestPass>,
        event: Event,
    ) -> bool {
        // A withdrawn runtime's work is dropped.
        let Some(entry) = self.runtimes.get_mut(runtime).and_then(Option::as_mut) else {
            return false;
        };
        entry.running -= 1;
        entry.release(unit, &mut self.shared_running);
        let source = &mut entry.units[unit];
        if source.abandoned {
            return false;
        }
        if let Event::Batch(output) = &event {
            let read = output.deltas.payload_bytes.min(source.unread);
            source.unread -= read;
            entry.remaining = entry.remaining.saturating_sub(read);
        }
        let bytes = event.bytes();
        source.queue.push_back((bytes, event));
        source.queued += bytes;
        let queued = source.queued;
        match pass {
            Some(pass) => {
                source.pass = Some(pass);
                entry.ready.insert(unit);
            }
            None => {
                source.finished = true;
                entry.remaining = entry.remaining.saturating_sub(source.unread);
                source.unread = 0;
            }
        }
        if unit == entry.head {
            if source.queue.len() == 1 {
                entry.ready_since = Some(Instant::now());
            }
            self.head_bytes += bytes;
            self.stats.peak_head_bytes = self.stats.peak_head_bytes.max(queued);
            self.stats.peak_head_bytes_total =
                self.stats.peak_head_bytes_total.max(self.head_bytes);
            return true;
        }
        entry.look_ahead += bytes;
        self.look_ahead += bytes;
        if entry.class == LaneClass::Shared {
            self.shared_look_ahead += bytes;
        }
        self.note_look_ahead_peaks();
        false
    }

    /// Pop the head's next event of lane `runtime`, if one is queued.
    fn pop_head(&mut self, runtime: usize, unit: usize) -> Option<(Event, Option<Instant>)> {
        let entry = self.runtimes[runtime]
            .as_mut()
            .expect("a lane's runtime stays registered");
        assert_eq!(
            entry.head, unit,
            "a lane's sources are consumed in its order"
        );
        let source = &mut entry.units[unit];
        let (bytes, event) = source.queue.pop_front()?;
        source.queued -= bytes;
        self.head_bytes -= bytes;
        let since = entry.ready_since.take();
        if !source.queue.is_empty() {
            // The rest waits from now: the sequencer has just served the lane.
            entry.ready_since = Some(Instant::now());
        }
        Some((event, since))
    }
}

impl Runtime {
    fn has_steps_left(&self) -> bool {
        self.next_dispatch < self.units.len() || !self.ready.is_empty() || self.running > 0
    }

    /// The head, when a worker may step it: never started, or parked with
    /// its queue under `H`.
    fn runnable_head(&self, head_bytes: u64) -> Option<usize> {
        let source = self.units.get(self.head)?;
        (source.queued < head_bytes
            && (self.head == self.next_dispatch || self.ready.contains(&self.head)))
        .then_some(self.head)
    }

    /// The next look-ahead task: the earliest parked source, else the next
    /// source in path order.
    fn look_ahead_candidate(&self) -> Option<usize> {
        self.ready
            .iter()
            .copied()
            .find(|unit| *unit != self.head)
            .or_else(|| self.next_look_ahead())
    }

    /// The next undispatched source, unless it is the head.
    fn next_look_ahead(&self) -> Option<usize> {
        (self.next_dispatch > self.head && self.next_dispatch < self.units.len())
            .then_some(self.next_dispatch)
    }
}

/// A registered runtime: its sequencer's end of the pool. Dropping it
/// withdraws the runtime, discarding its queued and parked work.
pub(crate) struct Lane<'p> {
    pool: &'p CapturePool,
    runtime: usize,
}

impl Lane<'_> {
    /// The next event of `unit`, the head, waiting for a worker to produce it.
    pub(crate) fn next(&self, unit: usize, opts: &IngestOptions) -> Result<Event, PipelineError> {
        let mut state = self.pool.lock();
        loop {
            let current = &mut *state;
            if current.closed {
                return Err(PipelineError::Config("capture pool closed".to_string()));
            }
            if let Some((event, _)) = current.pop_head(self.runtime, unit) {
                if current.idle_workers > 0 {
                    self.pool.work.notify_one();
                }
                return Ok(event);
            }
            opts.check_cancelled()?;
            state = self
                .pool
                .output
                .wait_timeout(state, WAIT_SLICE)
                .expect(POISONED)
                .0;
        }
    }

    /// `unit`, the head, is committed: the next source becomes the head, so
    /// its queued output moves from `G`'s account to `H`'s. Output a CAS
    /// conflict left on `unit` is discarded with its pass.
    pub(crate) fn advance(&self, unit: usize) {
        let mut guard = self.pool.lock();
        let state = &mut *guard;
        let entry = state.runtimes[self.runtime]
            .as_mut()
            .expect("a lane's runtime stays registered");
        assert_eq!(
            entry.head, unit,
            "a lane's sources are consumed in its order"
        );
        let source = &mut entry.units[unit];
        source.queue.clear();
        state.head_bytes -= source.queued;
        source.queued = 0;
        if !source.finished {
            source.finished = true;
            source.abandoned = true;
            if source.pass.take().is_some() {
                entry.ready.remove(&unit);
            }
            entry.remaining = entry.remaining.saturating_sub(source.unread);
            source.unread = 0;
        }
        entry.head += 1;
        entry.ready_since = None;
        if let Some(next) = entry.units.get(entry.head) {
            entry.look_ahead -= next.queued;
            state.look_ahead -= next.queued;
            if entry.class == LaneClass::Shared {
                state.shared_look_ahead -= next.queued;
            }
            state.head_bytes += next.queued;
            if !next.queue.is_empty() {
                entry.ready_since = Some(Instant::now());
            }
            state.stats.peak_head_bytes = state.stats.peak_head_bytes.max(next.queued);
            state.stats.peak_head_bytes_total =
                state.stats.peak_head_bytes_total.max(state.head_bytes);
        }
        if state.idle_workers > 0 {
            self.pool.work.notify_one();
        }
    }
}

impl Drop for Lane<'_> {
    fn drop(&mut self) {
        let withdrawn = {
            let mut state = self
                .pool
                .state
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let withdrawn = state.runtimes[self.runtime].take();
            if let Some(runtime) = &withdrawn {
                state.look_ahead -= runtime.look_ahead;
                if runtime.class == LaneClass::Shared {
                    state.shared_look_ahead -= runtime.look_ahead;
                    state.shared_running -= runtime.running_reserved;
                }
                state.head_bytes -= runtime
                    .units
                    .get(runtime.head)
                    .map_or(0, |source| source.queued);
            }
            // Freed look-ahead may unpark other runtimes' work.
            self.pool.work.notify_all();
            withdrawn
        };
        drop(withdrawn);
    }
}

pub(super) fn elapsed_nanos(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod cut_tests;
#[cfg(test)]
mod tests;
