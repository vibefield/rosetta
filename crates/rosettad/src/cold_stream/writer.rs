//! The sole canonical writer of a streamed fresh-cold build (R1 brief §4.6).
//!
//! Every turn the writer first drains the channel without blocking —
//! metadata plans are published in registry order, opened runtimes get
//! their setup, sources join their (runtime, lane) queue, and a runtime's
//! `Closed` is parked — and then runs exactly one transaction:
//!
//! 1. a flip batch, when one is due (`flips.rs`, D9);
//! 2. otherwise the next key's turn, round-robin over keys with work: a
//!    coalesced item of small sources, or the next slice of a chunked item.
//!
//! After every committed step the budget gets back that step's share of the
//! item's bytes (release by remainder), and a usage displacement on a tree
//! already flipped invalidates its session (D15). A parked `Closed` is
//! admitted once its runtime has nothing queued and no item between
//! slices: its ready trees flip first, then the remainder
//! (`admit.rs`). The writer blocks on the channel only when no key has work
//! and no tree waits for its cadence. The kill switch keeps HEAD's FIFO
//! writer (`queues.rs`): no chunks, no flips (its runtimes have no trees).

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, RecvTimeoutError, TryRecvError};
use std::time::{Duration, Instant};

use rosetta_pipeline::{ColdWalkOptions, Step, StreamedRuntime, TxKind};
use serde_json::{Value, json};

use super::emit::BudgetClass;
use super::flips::{CHUNK_BYTES, Flips};
use super::queues::{Active, Work, WriterQueues, block_for};
use super::slowest::{OpCounts, Slowest, StepRecord};
use super::{
    ClosedColdRuntime, OpenedColdRuntime, Producers, StreamMessage, StreamedCold, admit, millis,
    publish_metadata,
};
use crate::cold_progress::ColdProgress;
use crate::engine::{absorb_around, now_ms};
use crate::ingest_metrics::{ColdMilestones, UsageCopies};
use crate::maintenance_pace::thread_cpu_time;
use crate::serve::counter_source;
use crate::{AppliedColdRuntime, PlannedColdRuntime, bench};

#[cfg(test)]
thread_local! {
    /// Tests: fail the writer at its n-th apply step (1-based).
    pub(super) static FAIL_AT_STEP: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
    /// Tests: how often a parked `Closed` waited for its runtime's item
    /// between slices.
    pub(super) static PARKED_BEHIND_ITEM: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    /// Tests: items begun while another item of their runtime was between
    /// its slices (lanes interleaving on the writer).
    pub(super) static BEGUN_BESIDE_ITEM: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// The longest apply transaction and its bytes, over the capture window.
#[derive(Default)]
pub(super) struct ApplyRecord {
    pub(super) steps: u64,
    pub(super) longest_ns: u64,
    pub(super) max_bytes: u64,
    pub(super) busy_ns: u64,
    /// The slowest apply steps (under `--json --profile`).
    pub(super) slowest: Slowest<StepRecord>,
}

struct ParkedClose {
    runtime: Box<ClosedColdRuntime>,
    seq: u64,
}

/// Everything the writer keeps across turns.
struct Writer<'n> {
    names: &'n [String],
    milestones: ColdMilestones,
    plans: Vec<Option<Result<Box<PlannedColdRuntime>, String>>>,
    published: usize,
    ownership: BTreeMap<String, Option<String>>,
    open: HashMap<String, StreamedRuntime>,
    parked: Vec<ParkedClose>,
    /// Producers whose `Closed` arrived, and when the last one did: the end
    /// of the capture window the busy share is measured over.
    closes_seen: usize,
    capture_ended: Option<Instant>,
    outputs: Vec<AppliedColdRuntime>,
    closed: usize,
    queues: WriterQueues,
    flips: Flips,
    applies: ApplyRecord,
    apply_cpu: HashMap<String, Duration>,
    resolve_ns: u64,
    stage_profiles: bool,
    walk: ColdWalkOptions,
}

pub(super) fn write(
    receiver: Receiver<StreamMessage>,
    names: &[String],
    producers: &mut Producers<'_, '_, '_>,
    cx: &mut StreamedCold<'_>,
    failures: &mut Vec<String>,
) -> Result<Vec<AppliedColdRuntime>, String> {
    let workers = names.len();
    let stage_profiles = cx.flags.json && cx.flags.profile;
    let walk = cx.flags.cold_walk.clone();
    let chunk_bytes = cx.flags.cold_chunk_bytes.unwrap_or(CHUNK_BYTES);
    let mut writer = Writer {
        names,
        // Measurement only: milestones and census from what the writer knows.
        milestones: ColdMilestones::new(cx.started, now_ms(), workers, stage_profiles),
        plans: std::iter::repeat_with(|| None).take(workers).collect(),
        published: 0,
        ownership: BTreeMap::new(),
        open: HashMap::new(),
        parked: Vec::new(),
        closes_seen: 0,
        capture_ended: None,
        outputs: Vec::with_capacity(workers),
        closed: 0,
        queues: WriterQueues::new(walk.admission_override, chunk_bytes),
        flips: Flips::default(),
        applies: ApplyRecord::default(),
        apply_cpu: HashMap::new(),
        resolve_ns: 0,
        stage_profiles,
        walk,
    };
    let mut idle_nanos = 0_u64;
    // The part of it before the last producer's `Closed`.
    let mut capture_idle_nanos = 0_u64;
    let writer_started = Instant::now();
    let writer_cpu_started = thread_cpu_time();
    while writer.closed < workers {
        // A non-blocking drain: everything that arrived joins its queue.
        loop {
            match receiver.try_recv() {
                Ok(message) => writer.handle(message, producers, cx)?,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if writer.parked.is_empty() && !writer.queues.has_work() {
                        return Err("fresh cold capture channel closed".to_string());
                    }
                    break;
                }
            }
        }
        if writer.admit_parked(producers, cx, failures)? {
            continue;
        }
        let now = Instant::now();
        let (due, soonest) = writer.flips.due(&writer.open, now);
        if let Some(runtime) = due.map(str::to_owned) {
            writer.flip(&runtime, cx)?;
            continue;
        }
        if writer.queues.has_work() {
            writer.turn(producers, cx)?;
            continue;
        }
        if writer.closed >= workers {
            break;
        }
        let waited = Instant::now();
        let message = match block_for(soonest) {
            Some(timeout) => match receiver.recv_timeout(timeout) {
                Ok(message) => Some(message),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => {
                    return Err("fresh cold capture channel closed".to_string());
                }
            },
            None => Some(
                receiver
                    .recv()
                    .map_err(|_| "fresh cold capture channel closed".to_string())?,
            ),
        };
        let waited = u64::try_from(waited.elapsed().as_nanos()).unwrap_or(u64::MAX);
        idle_nanos = idle_nanos.saturating_add(waited);
        if writer.capture_ended.is_none() {
            capture_idle_nanos = capture_idle_nanos.saturating_add(waited);
        }
        if let Some(message) = message {
            writer.handle(message, producers, cx)?;
        }
    }
    if stage_profiles {
        let wall = writer_started.elapsed();
        // Capture wall: until the last producer closed, not the last
        // admission (R1 §14 compares busy time with capture wall).
        let capture = writer
            .capture_ended
            .map_or(wall, |end| end.saturating_duration_since(writer_started));
        let usage = UsageCopies::of(cx.instrument);
        // Busy: apply steps, flips and resolving withheld facts as items
        // begin. Non-idle: the writer's wall less all its channel waits.
        // Both span the writer's whole life (the post-capture tail too), so
        // busy never exceeds non-idle, and both are shares of the capture
        // window. Within capture: the window less the waits inside it.
        let busy = writer
            .applies
            .busy_ns
            .saturating_add(writer.flips.stats.wall.total_nanos())
            .saturating_add(writer.resolve_ns);
        let capture_nanos = capture.as_nanos().max(1) as f64;
        let non_idle = wall.as_nanos().saturating_sub(u128::from(idle_nanos));
        let non_idle_within = capture
            .as_nanos()
            .saturating_sub(u128::from(capture_idle_nanos));
        let slowest: Vec<Value> = writer
            .applies
            .slowest
            .largest_first()
            .into_iter()
            .map(StepRecord::json)
            .collect();
        let mut event = json!({
            "event": "stage-profile",
            "command": "serve",
            "stage": "streamed-writer",
            "agent": null,
            "wallMs": wall.as_secs_f64() * 1_000.0,
            "idleMs": millis(idle_nanos),
            "cpuMs": super::cpu_millis(writer_cpu_started),
            "applyCalls": writer.applies.steps,
            "longestApplyTxMs": millis(writer.applies.longest_ns),
            "maxApplyTxBytes": writer.applies.max_bytes,
            "busyShareOfCapture": busy as f64 / capture_nanos,
            "nonIdleShareOfCapture": non_idle as f64 / capture_nanos,
            "nonIdleShareWithinCapture": non_idle_within as f64 / capture_nanos,
            "captureWallMs": capture.as_secs_f64() * 1_000.0,
            "captureIdleMs": millis(capture_idle_nanos),
            "resolveMs": millis(writer.resolve_ns),
            "slowestApplySteps": slowest,
            "lanes": writer.lane_profile(),
            "budgetResidualBytes": budget_residual(producers),
            "usageIdentitiesOwned": usage.owned,
            "usageCopiesDropped": usage.dropped,
            "usageCopiesDisplaced": usage.displaced,
            "usageIdentitiesUnclaimed": usage.unclaimed,
            "usageDisplacedVisible": usage.displaced_visible,
            "usageOwnerBytes": usage.owner_bytes,
            // W1 waits and reader prologue waits so far (the streaming
            // window); `shared-cache-locks` reports finalization after it.
            "sharedCacheLocks": cx.store.shared_cache_locks().map(|locks| locks.report()),
        });
        if let (Value::Object(event), Value::Object(flips)) =
            (&mut event, writer.flips.stats.profile())
        {
            event.extend(flips);
        }
        bench::emit(&event);
    }
    Ok(writer.outputs)
}

/// Each budget class's bytes still in flight: zero once every queue is
/// empty (release by remainder conserves them).
fn budget_residual(producers: &Producers<'_, '_, '_>) -> Value {
    let budgets = producers.budgets;
    json!({
        "small": budgets.in_flight(BudgetClass::Small),
        "shared": budgets.in_flight(BudgetClass::Shared),
    })
}

impl Writer<'_> {
    fn handle(
        &mut self,
        message: StreamMessage,
        producers: &mut Producers<'_, '_, '_>,
        cx: &mut StreamedCold<'_>,
    ) -> Result<(), String> {
        match message {
            StreamMessage::Planned { index, planned } => {
                self.plans[index] = Some(planned);
                self.publish_planned(producers, cx)
            }
            StreamMessage::Opened(opened) => {
                let OpenedColdRuntime {
                    adapter,
                    runtime,
                    setup,
                } = *opened;
                self.milestones
                    .trees_known(&runtime.id, setup.trees().cloned());
                let mut streamed =
                    StreamedRuntime::open(adapter, &runtime, setup, cx.store, cx.instrument)
                        .map_err(|error| {
                            format!("opening streamed {} runtime: {error}", adapter.kind())
                        })?;
                // Measurement only: the slowest steps' byte split.
                streamed.measure_steps(self.stage_profiles);
                self.open.insert(runtime.id.clone(), streamed);
                Ok(())
            }
            StreamMessage::Sources {
                runtime_id,
                lane,
                class,
                sources,
                bytes,
            } => {
                if !self.open.contains_key(&runtime_id) {
                    return Err(format!(
                        "streamed sources arrived for unopened runtime {runtime_id}"
                    ));
                }
                self.queues
                    .push(&runtime_id, lane, class, sources, bytes, Instant::now());
                Ok(())
            }
            StreamMessage::Closed(runtime) => {
                self.closes_seen += 1;
                if self.closes_seen == self.names.len() {
                    self.capture_ended = Some(Instant::now());
                }
                let seq = self.queues.next_seq();
                self.parked.push(ParkedClose { runtime, seq });
                Ok(())
            }
            StreamMessage::Failed(error) => Err(error),
        }
    }

    /// Publish every plan that is next in registry order, whatever order
    /// plans finish in: runtimes share project rows, and the canonical row
    /// must not depend on thread timing.
    fn publish_planned(
        &mut self,
        producers: &mut Producers<'_, '_, '_>,
        cx: &mut StreamedCold<'_>,
    ) -> Result<(), String> {
        let workers = self.names.len();
        while let Some(planned) = self.plans.get_mut(self.published).and_then(Option::take) {
            let mut plan = *planned?;
            if cx.stop.load(Ordering::SeqCst) {
                return Err("stopped before cold metadata completed".into());
            }
            publish_metadata(&plan, &self.names[self.published], &mut self.ownership, cx)?;
            let public_at = cx.started.elapsed();
            self.published += 1;
            let last = self.published == workers;
            plan.opts.progress = Some(ColdProgress::observe(
                &plan.runtime.id,
                &plan.files,
                &self.ownership,
                cx.board.clone(),
                cx.health.clone(),
                true,
            ));
            if last {
                cx.health.ingest_metadata_ready(now_ms());
                println!(
                    "milestone: metadata-ready elapsed_s={:.2}",
                    cx.started.elapsed().as_secs_f64()
                );
            }
            let baseline = plan.instrument.snapshot();
            cx.health
                .ingest_stream(&plan.runtime.id, counter_source(&plan.instrument, baseline));
            let (id, kind) = (&plan.runtime.id, plan.adapter.kind());
            let census = self
                .milestones
                .metadata_public(public_at, id, kind, &plan.files);
            producers.start(plan, last);
            self.milestones.census(census, cx.store);
        }
        Ok(())
    }

    /// One transaction of the next key with work.
    fn turn(
        &mut self,
        producers: &mut Producers<'_, '_, '_>,
        cx: &mut StreamedCold<'_>,
    ) -> Result<(), String> {
        let Some((key, work)) = self.queues.take_turn(Instant::now()) else {
            return Ok(());
        };
        let runtime_id = key.0.clone();
        let streamed = self
            .open
            .get_mut(&runtime_id)
            .ok_or_else(|| format!("streamed sources arrived for unopened runtime {runtime_id}"))?;
        let mut active = match work {
            Work::Active(active) => active,
            Work::Fresh {
                sources,
                bytes,
                class,
                chunk_bytes,
            } => {
                #[cfg(test)]
                if streamed.items_in_progress() > 0 {
                    BEGUN_BESIDE_ITEM.with(|count| count.set(count.get() + 1));
                }
                let completes = self.milestones.completes_first_session(&sources);
                // Withheld facts are resolved here, as the item begins.
                let resolve_before = streamed.resolve_nanos();
                let item = streamed
                    .begin_item(sources, bytes, chunk_bytes)
                    .map_err(|error| format!("applying streamed {runtime_id} sources: {error}"))?;
                self.resolve_ns = self
                    .resolve_ns
                    .saturating_add(streamed.resolve_nanos().saturating_sub(resolve_before));
                Active {
                    item,
                    class,
                    completes,
                }
            }
        };
        #[cfg(test)]
        if FAIL_AT_STEP.with(|fail| fail.get()) == Some(self.applies.steps + 1) {
            return Err(format!(
                "injected writer failure at step {}",
                self.applies.steps + 1
            ));
        }
        let cpu_before = thread_cpu_time();
        let step_started = Instant::now();
        let (step, release, stats) = streamed
            .step(cx.store, &mut active.item, cx.instrument)
            .map_err(|error| format!("applying streamed {runtime_id} sources: {error}"))?;
        producers.budgets.release(active.class, release);
        if let (Some(before), Some(after)) = (cpu_before, thread_cpu_time()) {
            *self.apply_cpu.entry(runtime_id.clone()).or_default() += after.saturating_sub(before);
        }
        let report = active.item.last_step().expect("a step reports itself");
        self.applies.steps += 1;
        self.applies.busy_ns = self.applies.busy_ns.saturating_add(report.wall_nanos);
        self.applies.longest_ns = self.applies.longest_ns.max(report.wall_nanos);
        self.applies.max_bytes = self.applies.max_bytes.max(report.approx_bytes);
        if self.stage_profiles {
            self.applies
                .slowest
                .offer(report.wall_nanos, || StepRecord {
                    elapsed_s: step_started
                        .saturating_duration_since(cx.started)
                        .as_secs_f64(),
                    runtime: runtime_id.clone(),
                    lane: key.1,
                    report,
                    ops: OpCounts::of(&stats),
                    lock_held_ns: stats.profile.session_lock_held_nanos,
                    lock_wait_ns: stats.profile.session_lock_wait_nanos,
                });
        }
        let lane = self.queues.stats_mut(&key);
        lane.txs += 1;
        match report.kind {
            TxKind::Stage => lane.stage_txs += 1,
            TxKind::ResolvedRows => lane.resolved_slice_txs += 1,
            TxKind::Terminal => {}
        }
        // A displacement on a tree already flipped changed rows a reader
        // may hold: its session resyncs (D15).
        let displaced = streamed.flipped_among(&stats.usage_displaced_sessions);
        if !displaced.is_empty() {
            absorb_around(cx.store, cx.hub, &displaced, now_ms())?;
        }
        self.publish_progress();
        match step {
            Step::More => self.queues.put_back(&key, active),
            Step::Done => {
                if active.completes {
                    self.milestones.first_session_complete();
                }
            }
        }
        Ok(())
    }

    /// The probe's view of the writer: items between their slices.
    fn publish_progress(&self) {
        if let Some(counters) = &self.walk.probe_counters {
            let mid: u32 = self
                .open
                .values()
                .map(StreamedRuntime::items_in_progress)
                .sum();
            counters.items_mid_sequence.store(mid, Ordering::SeqCst);
        }
    }

    fn flip(&mut self, runtime_id: &str, cx: &mut StreamedCold<'_>) -> Result<(), String> {
        let streamed = self
            .open
            .get_mut(runtime_id)
            .ok_or_else(|| format!("flipping unopened runtime {runtime_id}"))?;
        self.flips.flip(
            runtime_id,
            streamed,
            cx,
            &self.ownership,
            &mut self.milestones,
            self.walk.probe_counters.as_deref(),
            self.stage_profiles,
        )?;
        Ok(())
    }

    /// Admit the first parked runtime whose queues are empty and that has
    /// no item between slices (and, under the kill switch, nothing older
    /// still queued). Returns whether one was admitted.
    fn admit_parked(
        &mut self,
        producers: &mut Producers<'_, '_, '_>,
        cx: &mut StreamedCold<'_>,
        failures: &mut Vec<String>,
    ) -> Result<bool, String> {
        #[cfg(test)]
        for parked in &self.parked {
            let id = &parked.runtime.runtime.id;
            if self
                .open
                .get(id)
                .is_some_and(|streamed| streamed.items_in_progress() > 0)
            {
                PARKED_BEHIND_ITEM.with(|count| count.set(count.get() + 1));
            }
        }
        let Some(position) = self.parked.iter().position(|parked| {
            let id = &parked.runtime.runtime.id;
            self.queues.runtime_idle(id)
                && self
                    .open
                    .get(id)
                    .is_none_or(|streamed| streamed.items_in_progress() == 0)
                && !self.queues.older_than(parked.seq)
        }) else {
            return Ok(false);
        };
        let parked = self.parked.remove(position);
        self.closed += 1;
        let runtime = *parked.runtime;
        let id = runtime.runtime.id.clone();
        let mut streamed = self
            .open
            .remove(&id)
            .ok_or_else(|| format!("streamed runtime {id} closed unopened"))?;
        // Ready trees flip first; the remainder follows.
        self.flips.drain(
            &id,
            &mut streamed,
            cx,
            &self.ownership,
            &mut self.milestones,
            self.walk.probe_counters.as_deref(),
            self.stage_profiles,
        )?;
        let applied_cpu = self.apply_cpu.remove(&id).unwrap_or_default();
        if let Some(output) = admit::admit(
            runtime,
            streamed,
            applied_cpu,
            &self.ownership,
            &mut self.milestones,
            cx,
            failures,
        )? {
            self.outputs.push(output);
        }
        if self.closed == self.names.len() {
            // Every producer has closed and every item has committed: each
            // class got back exactly what it lent (release by remainder).
            debug_assert_eq!(
                producers.budgets.in_flight_total(),
                0,
                "a budget class holds bytes after the last runtime closed"
            );
        }
        Ok(true)
    }

    /// Per (runtime, lane): transactions, slices and queue waits.
    fn lane_profile(&self) -> Value {
        Value::Array(
            self.queues
                .stats()
                .map(|((runtime, lane), stats)| {
                    let mut wait = stats.queue_wait.clone();
                    json!({
                        "runtime": runtime,
                        "lane": lane.to_string(),
                        "txs": stats.txs,
                        "stageTxs": stats.stage_txs,
                        "resolvedSliceTxs": stats.resolved_slice_txs,
                        "queueWaitP50Ms": millis(wait.percentile_nanos(50)),
                        "queueWaitP99Ms": millis(wait.percentile_nanos(99)),
                        "queueWaitMaxMs": millis(wait.max_nanos()),
                    })
                })
                .collect(),
        )
    }
}
