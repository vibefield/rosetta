//! The pool's lane side: look-ahead bounds per lane class, and the
//! sequencer's wait for whichever of its lanes' heads is ready first.
//!
//! A look-ahead step that is still running counts against its lane's bounds
//! by what it may add: the payload it may still read, at most one batch. A
//! bound is otherwise checked only when a worker picks, so every worker
//! could start a step just under it and overshoot `G` together by a batch
//! each — on a big-tree lane, where every source is large, they do.

use std::time::{Duration, Instant};

use super::{
    CapturePool, Event, Lane, LaneClass, POISONED, PoolBudgets, Runtime, State, WAIT_SLICE,
};
use crate::{IngestOptions, PipelineError};

/// An event `next_ready` took, with since when its lane's head had output
/// waiting for the sequencer.
pub(crate) struct Ready {
    pub(crate) index: usize,
    pub(crate) event: Event,
    pub(crate) waiting_since: Option<Instant>,
}

impl CapturePool {
    /// The first of `heads` (in the given order: lowest global index first)
    /// whose head unit has a queued event; waits up to `wait` for one,
    /// rechecking cancellation. `Ok(None)` on timeout.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn next_ready(
        &self,
        heads: &[(&Lane<'_>, usize)],
        wait: Duration,
        opts: &IngestOptions,
    ) -> Result<Option<(usize, Event)>, PipelineError> {
        let epoch = self.output_epoch();
        Ok(self
            .next_ready_since(heads, wait, opts, epoch)?
            .map(|ready| (ready.index, ready.event)))
    }

    /// [`CapturePool::next_ready`] that also returns `Ok(None)` as soon as
    /// [`CapturePool::notify_output`] was called after the caller read
    /// `epoch`, and says how long the event's head had been waiting.
    pub(crate) fn next_ready_since(
        &self,
        heads: &[(&Lane<'_>, usize)],
        wait: Duration,
        opts: &IngestOptions,
        epoch: u64,
    ) -> Result<Option<Ready>, PipelineError> {
        let deadline = Instant::now() + wait;
        let mut state = self.lock();
        loop {
            if state.closed {
                return Err(PipelineError::Config("capture pool closed".to_string()));
            }
            for (index, (lane, unit)) in heads.iter().enumerate() {
                assert!(
                    std::ptr::eq(lane.pool, self),
                    "a lane of another capture pool"
                );
                if let Some((event, waiting_since)) = state.pop_head(lane.runtime, *unit) {
                    if state.idle_workers > 0 {
                        self.work.notify_one();
                    }
                    return Ok(Some(Ready {
                        index,
                        event,
                        waiting_since,
                    }));
                }
            }
            let now = Instant::now();
            if state.output_epoch != epoch || now >= deadline {
                return Ok(None);
            }
            opts.check_cancelled()?;
            state = self
                .output
                .wait_timeout(state, (deadline - now).min(WAIT_SLICE))
                .expect(POISONED)
                .0;
        }
    }

    /// The count [`CapturePool::notify_output`] bumps.
    pub(crate) fn output_epoch(&self) -> u64 {
        self.lock().output_epoch
    }

    /// Wake every sequencer waiting in [`CapturePool::next_ready`]: the
    /// writer released budget a held emission may fit in.
    pub fn notify_output(&self) {
        let mut state = self.lock();
        state.output_epoch = state.output_epoch.wrapping_add(1);
        self.output.notify_all();
    }
}

impl State {
    pub(super) fn note_look_ahead_peaks(&mut self) {
        let stats = &mut self.stats;
        stats.peak_look_ahead_bytes = stats.peak_look_ahead_bytes.max(self.look_ahead);
        stats.peak_shared_look_ahead_bytes = stats
            .peak_shared_look_ahead_bytes
            .max(self.shared_look_ahead);
        stats.peak_reserved_look_ahead_bytes = stats
            .peak_reserved_look_ahead_bytes
            .max(self.look_ahead - self.shared_look_ahead);
    }

    /// What each lane may queue ahead of its head right now.
    pub(super) fn bounds(&self, budgets: &PoolBudgets) -> Bounds {
        let slice = budgets.small_lane_bytes();
        let reserved_hold = self
            .registered()
            .filter(|(_, runtime)| runtime.class == LaneClass::Reserved)
            .map(|(_, runtime)| {
                if runtime.has_steps_left() {
                    slice
                } else {
                    runtime.look_ahead
                }
            })
            .fold(0_u64, u64::saturating_add);
        let shared_bytes = budgets
            .look_ahead_bytes
            .saturating_sub(reserved_hold)
            .max(1);
        let open_shared = self
            .registered()
            .filter(|(_, runtime)| runtime.class == LaneClass::Shared && runtime.has_steps_left())
            .count()
            .max(1) as u64;
        Bounds {
            slice,
            shared_bytes,
            share: budgets.runtime_floor_bytes.max(shared_bytes / open_shared),
            shared_look_ahead: self.shared_look_ahead + self.shared_running,
        }
    }
}

/// One moment's look-ahead bounds (see the module docs).
pub(super) struct Bounds {
    /// A `Reserved` lane's slice.
    pub(super) slice: u64,
    /// `G_shared`: what the reserved holds leave of `G`, at least one byte.
    pub(super) shared_bytes: u64,
    /// A `Shared` lane's share of `G_shared`.
    pub(super) share: u64,
    /// What `Shared` lanes hold ahead, with their running steps.
    shared_look_ahead: u64,
}

impl Bounds {
    /// Whether `runtime` may step a source that is not its head.
    pub(super) fn eligible(&self, runtime: &Runtime) -> bool {
        let ahead = runtime.look_ahead + runtime.running_reserved;
        match runtime.class {
            LaneClass::Reserved => ahead < self.slice,
            LaneClass::Shared => ahead < self.share && self.shared_look_ahead < self.shared_bytes,
        }
    }
}

impl Runtime {
    /// Count the look-ahead step just handed out for `unit` against this
    /// lane (and `shared_running`, the shared account, for a `Shared` lane):
    /// the payload it has left to read, at most one batch.
    pub(super) fn reserve(&mut self, unit: usize, shared_running: &mut u64) {
        let batch = self.shared.opts.tail.max_batch_bytes as u64;
        let source = &mut self.units[unit];
        source.reserved = source.unread.min(batch);
        self.running_reserved += source.reserved;
        if self.class == LaneClass::Shared {
            *shared_running += source.reserved;
        }
    }

    /// `unit`'s step returned: its reservation, if it held one, leaves both
    /// accounts.
    pub(super) fn release(&mut self, unit: usize, shared_running: &mut u64) {
        let reserved = std::mem::take(&mut self.units[unit].reserved);
        self.running_reserved -= reserved;
        if self.class == LaneClass::Shared {
            *shared_running -= reserved;
        }
    }
}
