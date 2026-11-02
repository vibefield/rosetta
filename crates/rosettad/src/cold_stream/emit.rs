//! The producers' side of the writer's in-flight budgets.
//!
//! Closed sources in flight between the producers and the writer are bounded
//! in bytes, per budget class: lane `S` and span `P` of every root-tree
//! runtime share a small class, so a giant in a big lane can never fill the
//! budget lane `S` needs; every big lane and every runtime admitted whole
//! share the rest. The HEAD-compatible modes (`--experiment-cold-admission
//! runtime`, `--experiment-cold-lanes 1`) keep one budget for everything.
//!
//! Each class keeps HEAD's rule — one item larger than the class moves alone
//! once nothing else of the class is in flight — plus a **drain ticket**: the
//! first acquire that does not fit takes the class's ticket, and until it is
//! admitted every other acquire of the class waits or fails, even one that
//! would fit. An item that does not fit therefore waits for at most one drain
//! of the class, whatever the other lanes keep sending.
//!
//! A recency walk's emission never blocks its sequencer: [`ProducerEmit`]
//! prepares it, tries the budget, and holds it when the class is full; the
//! writer's release, and a drain ticket clearing, wake the sequencers (the
//! budget's `on_release` hook) to retry. The path walk and the serial driver
//! emit blocking, as HEAD did.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

use rosetta_pipeline::{
    BIG_SOURCE_BYTES, ClosedSource, ColdEmit, EmitOutcome, Instrument, LaneId, Metric,
    PipelineError, StreamedPreparer,
};

use super::StreamMessage;
use crate::maintenance_pace::thread_cpu_time;

/// Closed sources in flight between all producers and the writer, measured
/// in budget bytes, over every class.
pub(crate) const IN_FLIGHT_BYTES: u64 = 256 * 1024 * 1024;
/// Lane `S` and span `P` of every root-tree runtime.
pub(crate) const SMALL_IN_FLIGHT_BYTES: u64 = IN_FLIGHT_BYTES / 4;
/// Every big lane, and every runtime admitted whole.
pub(crate) const SHARED_IN_FLIGHT_BYTES: u64 = IN_FLIGHT_BYTES - SMALL_IN_FLIGHT_BYTES;

// The lane split is the census's large-source bound (`ingest_metrics`).
const _: () = assert!(BIG_SOURCE_BYTES == 64 * 1024 * 1024);

/// Which budget an emission is charged to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum BudgetClass {
    Small,
    Shared,
    /// The single budget of the HEAD-compatible modes.
    All,
}

/// Wakes whoever waits for a released budget outside the budget itself (the
/// capture pool's sequencers).
pub(crate) type ReleaseHook = Arc<dyn Fn() + Send + Sync>;

/// What one admission attempt did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Admit {
    Refused,
    Admitted,
    /// Admitted, and the class's drain ticket it held cleared with it.
    ClearedTicket,
}

#[derive(Default)]
struct BudgetState {
    in_flight: u64,
    aborted: bool,
    /// The acquire that failed first and drains the class before anyone else.
    ticket: Option<u64>,
}

pub(crate) struct ByteBudget {
    limit: u64,
    state: Mutex<BudgetState>,
    released: Condvar,
    on_release: Option<ReleaseHook>,
}

impl ByteBudget {
    pub(crate) fn new(limit: u64, on_release: Option<ReleaseHook>) -> Self {
        Self {
            limit,
            state: Mutex::new(BudgetState::default()),
            released: Condvar::new(),
            on_release,
        }
    }

    fn lock(&self) -> MutexGuard<'_, BudgetState> {
        self.state.lock().expect("cold byte budget poisoned")
    }

    /// HEAD's predicate: `bytes` fit beside what is in flight, or nothing is.
    fn fits(&self, state: &BudgetState, bytes: u64) -> bool {
        state.in_flight == 0 || state.in_flight.saturating_add(bytes) <= self.limit
    }

    /// Admit `bytes` for `ticket` if they fit and no other acquire holds the
    /// class's ticket; otherwise take the ticket if it is free.
    fn admit(&self, state: &mut BudgetState, bytes: u64, ticket: u64) -> Admit {
        let free = state.ticket.is_none_or(|holder| holder == ticket);
        if free && self.fits(state, bytes) {
            state.in_flight = state.in_flight.saturating_add(bytes);
            if state.ticket.take().is_some() {
                // Acquires the ticket held back may fit now.
                self.released.notify_all();
                return Admit::ClearedTicket;
            }
            return Admit::Admitted;
        }
        if state.ticket.is_none() {
            state.ticket = Some(ticket);
        }
        Admit::Refused
    }

    /// Wake the sequencers holding emissions: room or a ticket just freed.
    /// Called with the budget's lock released, so the hook may take the
    /// capture pool's.
    fn wake(&self) {
        if let Some(hook) = &self.on_release {
            hook();
        }
    }

    /// Wait until `bytes` fit beside what the writer has not applied yet.
    pub(crate) fn acquire(&self, bytes: u64, ticket: u64) -> Result<(), String> {
        let mut state = self.lock();
        loop {
            if state.aborted {
                return Err("fresh cold writer stopped".to_string());
            }
            match self.admit(&mut state, bytes, ticket) {
                Admit::Refused => {}
                Admit::Admitted => return Ok(()),
                Admit::ClearedTicket => {
                    drop(state);
                    self.wake();
                    return Ok(());
                }
            }
            state = self
                .released
                .wait(state)
                .expect("cold byte budget poisoned");
        }
    }

    /// [`ByteBudget::acquire`] without waiting: `Ok(false)` when it would.
    pub(crate) fn try_acquire(&self, bytes: u64, ticket: u64) -> Result<bool, String> {
        let mut state = self.lock();
        if state.aborted {
            return Err("fresh cold writer stopped".to_string());
        }
        let admit = self.admit(&mut state, bytes, ticket);
        drop(state);
        if admit == Admit::ClearedTicket {
            // A held lane the ticket kept out may fit now: it must not wait
            // for the next release (a giant's apply away) to retry.
            self.wake();
        }
        Ok(admit != Admit::Refused)
    }

    /// The writer applied `bytes` of what it was handed.
    pub(crate) fn release(&self, bytes: u64) {
        {
            let mut state = self.lock();
            debug_assert!(
                state.in_flight >= bytes,
                "released {bytes} bytes of a budget holding {}",
                state.in_flight
            );
            state.in_flight = state.in_flight.saturating_sub(bytes);
            self.released.notify_all();
        }
        self.wake();
    }

    pub(crate) fn abort(&self) {
        let mut state = self.lock();
        state.aborted = true;
        self.released.notify_all();
    }

    /// Give up `ticket`'s claim on the class (its holder is gone).
    fn drop_ticket(&self, ticket: u64) {
        let mut state = self.lock();
        if state.ticket == Some(ticket) {
            state.ticket = None;
            self.released.notify_all();
            drop(state);
            self.wake();
        }
    }

    /// Bytes acquired and not released yet.
    pub(crate) fn in_flight(&self) -> u64 {
        self.lock().in_flight
    }
}

/// Tests: per-class limits for the split budgets a build constructs on this
/// thread, and counters of what its producers held and retried.
#[cfg(test)]
#[derive(Clone)]
pub(crate) struct TestBudgets {
    pub(crate) small: u64,
    pub(crate) shared: u64,
    pub(crate) counts: Arc<HoldCounts>,
}

/// Tests: emissions a producer held because their class was full, and held
/// emissions a retry then sent.
#[cfg(test)]
#[derive(Debug, Default)]
pub(crate) struct HoldCounts {
    pub(crate) held: AtomicU64,
    pub(crate) retried: AtomicU64,
}

#[cfg(test)]
thread_local! {
    pub(crate) static TEST_BUDGETS: std::cell::RefCell<Option<TestBudgets>> =
        const { std::cell::RefCell::new(None) };
}

/// Every producer's budgets, built once per build.
pub(crate) struct LaneBudgets {
    small: Option<ByteBudget>,
    /// The shared class, or the single budget of the HEAD-compatible modes.
    shared: ByteBudget,
    next_ticket: AtomicU64,
    #[cfg(test)]
    counts: Option<Arc<HoldCounts>>,
}

impl LaneBudgets {
    /// Per-class budgets; `split == false` keeps HEAD's single budget.
    pub(crate) fn new(split: bool, on_release: Option<ReleaseHook>) -> Self {
        #[cfg(test)]
        let test = TEST_BUDGETS.with(|test| test.borrow().clone());
        #[cfg(test)]
        let (small, shared) = test
            .as_ref()
            .map_or((SMALL_IN_FLIGHT_BYTES, SHARED_IN_FLIGHT_BYTES), |test| {
                (test.small, test.shared)
            });
        #[cfg(not(test))]
        let (small, shared) = (SMALL_IN_FLIGHT_BYTES, SHARED_IN_FLIGHT_BYTES);
        if split {
            Self {
                small: Some(ByteBudget::new(small, on_release.clone())),
                shared: ByteBudget::new(shared, on_release),
                next_ticket: AtomicU64::new(1),
                #[cfg(test)]
                counts: test.map(|test| test.counts),
            }
        } else {
            Self {
                small: None,
                shared: ByteBudget::new(IN_FLIGHT_BYTES, on_release),
                next_ticket: AtomicU64::new(1),
                #[cfg(test)]
                counts: None,
            }
        }
    }

    /// Tests: count one held emission, or one held emission a retry sent.
    #[cfg(test)]
    fn count(&self, retried: bool) {
        if let Some(counts) = &self.counts {
            let counter = if retried {
                &counts.retried
            } else {
                &counts.held
            };
            counter.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// The class `lane`'s emissions are charged to.
    pub(crate) fn class_of(&self, lane: LaneId) -> BudgetClass {
        match &self.small {
            None => BudgetClass::All,
            Some(_) if lane.is_small_class() => BudgetClass::Small,
            Some(_) => BudgetClass::Shared,
        }
    }

    pub(crate) fn budget(&self, class: BudgetClass) -> &ByteBudget {
        match (class, &self.small) {
            (BudgetClass::Small, Some(small)) => small,
            _ => &self.shared,
        }
    }

    pub(crate) fn release(&self, class: BudgetClass, bytes: u64) {
        self.budget(class).release(bytes);
    }

    /// Bytes `class` holds in flight (zero for a class this mode lacks).
    pub(crate) fn in_flight(&self, class: BudgetClass) -> u64 {
        match (class, &self.small) {
            (BudgetClass::Small, None) => 0,
            (BudgetClass::Shared | BudgetClass::All, Some(_)) | (_, None) => {
                self.shared.in_flight()
            }
            (BudgetClass::Small, Some(small)) => small.in_flight(),
        }
    }

    /// Bytes every class holds in flight.
    pub(crate) fn in_flight_total(&self) -> u64 {
        self.small
            .as_ref()
            .map_or(0, ByteBudget::in_flight)
            .saturating_add(self.shared.in_flight())
    }

    /// Abort every class: no producer stays blocked behind a dead writer.
    pub(crate) fn abort(&self) {
        if let Some(small) = &self.small {
            small.abort();
        }
        self.shared.abort();
    }

    /// A fresh drain-ticket id.
    pub(crate) fn ticket(&self) -> u64 {
        self.next_ticket.fetch_add(1, Ordering::Relaxed)
    }
}

/// An emission whose class was full.
struct Held {
    sources: Vec<ClosedSource>,
    bytes: u64,
    class: BudgetClass,
}

/// One runtime's emitter: its preparer, then the budget, then the writer.
pub(crate) struct ProducerEmit<'a> {
    runtime_id: String,
    preparer: StreamedPreparer,
    sender: &'a Sender<StreamMessage>,
    budgets: &'a LaneBudgets,
    instrument: &'a Instrument,
    blocking: bool,
    stage_profiles: bool,
    prepare_cpu: Duration,
    held: HashMap<LaneId, Held>,
    /// Each lane's drain ticket, kept across its retries.
    tickets: HashMap<LaneId, u64>,
}

impl<'a> ProducerEmit<'a> {
    /// `blocking` for the path walk and the serial driver: every emission
    /// waits for its budget, as HEAD's did.
    pub(crate) fn new(
        runtime_id: String,
        preparer: StreamedPreparer,
        sender: &'a Sender<StreamMessage>,
        budgets: &'a LaneBudgets,
        instrument: &'a Instrument,
        blocking: bool,
        stage_profiles: bool,
    ) -> Self {
        Self {
            runtime_id,
            preparer,
            sender,
            budgets,
            instrument,
            blocking,
            stage_profiles,
            prepare_cpu: Duration::ZERO,
            held: HashMap::new(),
            tickets: HashMap::new(),
        }
    }

    /// The preparer, for admission, and this thread's CPU spent preparing.
    pub(crate) fn finish(mut self) -> (StreamedPreparer, Duration) {
        let preparer = std::mem::replace(&mut self.preparer, StreamedPreparer::new(&[]));
        (preparer, self.prepare_cpu)
    }

    fn ticket(&mut self, lane: LaneId) -> u64 {
        let budgets = self.budgets;
        *self.tickets.entry(lane).or_insert_with(|| budgets.ticket())
    }

    fn send(
        &self,
        lane: LaneId,
        class: BudgetClass,
        sources: Vec<ClosedSource>,
        bytes: u64,
    ) -> Result<EmitOutcome, PipelineError> {
        self.sender
            .send(StreamMessage::Sources {
                runtime_id: self.runtime_id.clone(),
                lane,
                class,
                sources,
                bytes,
            })
            .map_err(|_| PipelineError::Config("fresh cold writer stopped".into()))?;
        Ok(EmitOutcome::Sent)
    }
}

impl ColdEmit for ProducerEmit<'_> {
    fn emit(
        &mut self,
        lane: LaneId,
        mut sources: Vec<ClosedSource>,
    ) -> Result<EmitOutcome, PipelineError> {
        if self.held.contains_key(&lane) {
            return Err(PipelineError::Config(format!(
                "cold lane {lane} emitted while it holds an emission"
            )));
        }
        let prepare_started = self.stage_profiles.then(thread_cpu_time).flatten();
        self.preparer.prepare(&mut sources)?;
        if let Some(before) = prepare_started
            && let Some(after) = thread_cpu_time()
        {
            self.prepare_cpu += after.saturating_sub(before);
        }
        let bytes = sources
            .iter()
            .map(ClosedSource::budget_bytes)
            .fold(0, u64::saturating_add);
        let class = self.budgets.class_of(lane);
        let budget = self.budgets.budget(class);
        if self.blocking {
            let ticket = self.budgets.ticket();
            self.instrument
                .time(Metric::WriterBudgetWaitNanos, || {
                    budget.acquire(bytes, ticket)
                })
                .map_err(PipelineError::Config)?;
            return self.send(lane, class, sources, bytes);
        }
        let ticket = self.ticket(lane);
        if budget
            .try_acquire(bytes, ticket)
            .map_err(PipelineError::Config)?
        {
            return self.send(lane, class, sources, bytes);
        }
        #[cfg(test)]
        self.budgets.count(false);
        self.held.insert(
            lane,
            Held {
                sources,
                bytes,
                class,
            },
        );
        Ok(EmitOutcome::Held)
    }

    fn retry(&mut self, lane: LaneId) -> Result<EmitOutcome, PipelineError> {
        let ticket = self.ticket(lane);
        let Some(held) = self.held.get(&lane) else {
            return Err(PipelineError::Config(format!(
                "cold lane {lane} retried without a held emission"
            )));
        };
        if !self
            .budgets
            .budget(held.class)
            .try_acquire(held.bytes, ticket)
            .map_err(PipelineError::Config)?
        {
            return Ok(EmitOutcome::Held);
        }
        let held = self.held.remove(&lane).expect("checked above");
        #[cfg(test)]
        self.budgets.count(true);
        self.send(lane, held.class, held.sources, held.bytes)
    }

    fn held(&self, lane: LaneId) -> bool {
        self.held.contains_key(&lane)
    }

    fn blocking(&self) -> bool {
        self.blocking
    }
}

impl Drop for ProducerEmit<'_> {
    fn drop(&mut self) {
        // A walk that failed leaves no class waiting on its tickets.
        for (lane, ticket) in &self.tickets {
            let class = self.budgets.class_of(*lane);
            self.budgets.budget(class).drop_ticket(*ticket);
        }
    }
}

#[cfg(test)]
mod tests;
