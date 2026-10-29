//! Streamed apply for the fresh RAM cold build (`serve` on a first run).
//!
//! One producer thread per runtime captures through its private planner and
//! hands every source to the sole canonical writer as a closed source the
//! moment nothing can supersede it (`rosetta_pipeline::closed`). The writer —
//! this thread — writes those final rows into the shared canonical RAM
//! projection while capture continues, then admits each runtime when its
//! producer closes: one RAM fold writes each resolved tool row once, and one
//! stream-state commit makes the whole runtime public. Nothing about
//! visibility changed: the runtime's sessions are `Pending` (the read
//! barrier) until that commit. What changed is that no runtime is retained
//! whole in RAM and the canonical write overlaps capture instead of following
//! it.
//!
//! A byte budget bounds the closed sources in flight across all producers, so
//! a fast producer cannot pile gigabytes up behind the writer while it admits
//! another runtime.
//!
//! Every producer shares one capture pool (`rosetta_pipeline::capture`,
//! `--capture-workers N`, `0` for none): its workers read, normalize and
//! frame the runtimes' JSONL sources ahead, and each producer, now its
//! runtime's sequencer, commits them to its planner and ledger in path order.
//!
//! Metadata comes first, per runtime: a few planning threads skim runtimes
//! in registry order while the writer makes each plan's metadata public, also
//! in registry order, and starts that runtime's producer right away. A large
//! runtime early in the registry therefore captures while later runtimes are
//! still skimming, and no runtime waits for another's capture to start.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{Scope, ScopedJoinHandle};
use std::time::{Duration, Instant};

use rosetta_adapter_support::AgentAdapter;
use rosetta_capture::Ledger;
use rosetta_pipeline::{
    CapturePool, ClosedSource, Instrument, InstrumentSnapshot, MetaReport, Metric, PoolBudgets,
    PreparedAdmission, StreamReport, StreamedPreparer, StreamedRuntime, StreamedSetup,
    stream_closed_sources_into, stream_closed_sources_pooled, streamed_setup,
};
use rosetta_server::{BackfillState, DaemonHealth, Hub, ProgressBoard, RuntimeIdentity};
use rosetta_store::Store;
use serde_json::json;

use crate::cold_progress::ColdProgress;
use crate::engine::{absorb_effects, now_ms};
use crate::ledger_compression::thread_cpu_time;
use crate::serve::counter_source;
use crate::{
    AppliedColdRuntime, ColdCaptureRequest, Flags, PlannedColdRuntime, bench, identity_name,
    plan_parallel_cold_runtime, record_fresh_failures,
};

/// Closed sources in flight between all producers and the writer, measured
/// in framed payload bytes. One source larger than the budget still moves
/// alone.
const IN_FLIGHT_BYTES: u64 = 256 * 1024 * 1024;
/// Queued sources of one runtime share a private transaction up to these
/// bounds, so commits amortize while a shared-cache reader never waits
/// behind one very long write.
const TRANSACTION_BYTES: u64 = 16 * 1024 * 1024;
const TRANSACTION_SOURCES: usize = 256;
/// Runtimes whose metadata is planned at once. Each plan already reads its
/// sources on a bounded pool, and publication stays in registry order, so
/// this only needs to keep the small runtimes planning while a large one
/// does; more planners would add threads, not an earlier capture start.
const METADATA_PLANNERS: usize = 3;

enum StreamMessage {
    /// A runtime's metadata plan (or why there is none), by its position in
    /// registry order.
    Planned {
        index: usize,
        planned: Result<Box<PlannedColdRuntime>, String>,
    },
    Opened(Box<OpenedColdRuntime>),
    Sources {
        runtime_id: String,
        sources: Vec<ClosedSource>,
        bytes: u64,
    },
    Closed(Box<ClosedColdRuntime>),
    Failed(String),
}

/// A runtime whose producer is about to capture: the writer's setup input.
struct OpenedColdRuntime {
    adapter: &'static dyn AgentAdapter,
    runtime: rosetta_core::rows::RuntimeRow,
    setup: StreamedSetup,
}

/// Everything admission and the live handoff need once a producer's capture
/// has ended. Its sources were all sent before this message.
struct ClosedColdRuntime {
    adapter: &'static dyn AgentAdapter,
    runtime: rosetta_core::rows::RuntimeRow,
    identity: RuntimeIdentity,
    root: PathBuf,
    roster: Vec<rosetta_pipeline::SessionFile>,
    discovery_truncated: bool,
    meta: MetaReport,
    stream: StreamReport,
    ledger: Option<Ledger>,
    prepared: PreparedAdmission,
    profile: InstrumentSnapshot,
    wall_ms: f64,
    captured_at: Instant,
}

#[derive(Default)]
struct BudgetState {
    in_flight: u64,
    aborted: bool,
}

struct ByteBudget {
    limit: u64,
    state: Mutex<BudgetState>,
    released: Condvar,
}

impl ByteBudget {
    fn new(limit: u64) -> Self {
        Self {
            limit,
            state: Mutex::new(BudgetState::default()),
            released: Condvar::new(),
        }
    }

    /// Wait until `bytes` fit beside what the writer has not applied yet.
    fn acquire(&self, bytes: u64) -> Result<(), String> {
        let mut state = self.state.lock().expect("cold byte budget poisoned");
        while state.in_flight > 0 && state.in_flight.saturating_add(bytes) > self.limit {
            if state.aborted {
                break;
            }
            state = self
                .released
                .wait(state)
                .expect("cold byte budget poisoned");
        }
        if state.aborted {
            return Err("fresh cold writer stopped".to_string());
        }
        state.in_flight = state.in_flight.saturating_add(bytes);
        Ok(())
    }

    fn release(&self, bytes: u64) {
        let mut state = self.state.lock().expect("cold byte budget poisoned");
        state.in_flight = state.in_flight.saturating_sub(bytes);
        self.released.notify_all();
    }

    fn abort(&self) {
        let mut state = self.state.lock().expect("cold byte budget poisoned");
        state.aborted = true;
        self.released.notify_all();
    }
}

fn millis(nanos: u64) -> f64 {
    nanos as f64 / 1_000_000.0
}

/// This thread's CPU since `started`, in milliseconds, where the clock exists.
fn cpu_millis(started: Option<Duration>) -> Option<f64> {
    started
        .zip(thread_cpu_time())
        .map(|(before, after)| after.saturating_sub(before).as_secs_f64() * 1_000.0)
}

/// One runtime's producer: setup, streamed capture, then the closing report.
/// With a capture pool the producer is the runtime's sequencer: the pool's
/// workers read and normalize its sources, and it commits them in path order.
fn produce(
    planned: PlannedColdRuntime,
    sender: &Sender<StreamMessage>,
    budget: &ByteBudget,
    stage_profiles: bool,
    pool: Option<&CapturePool>,
) -> Result<(), String> {
    let PlannedColdRuntime {
        adapter,
        runtime,
        identity,
        root,
        files,
        discovery_truncated,
        meta,
        mut planner,
        mut ledger,
        instrument,
        opts,
        meta_profile,
        meta_wall_ms,
        capture_started,
    } = planned;
    let gone = |_| "fresh cold writer stopped".to_string();
    let setup = streamed_setup(&files, &mut planner).map_err(|error| error.to_string())?;
    sender
        .send(StreamMessage::Opened(Box::new(OpenedColdRuntime {
            adapter,
            runtime: runtime.clone(),
            setup,
        })))
        .map_err(gone)?;

    let stream_before = instrument.snapshot();
    let stream_started = Instant::now();
    // This thread's CPU over the walk: all of capture on the serial walk, the
    // sequencer's own share (planner, ledger hand-off, preparer, inline
    // groups) on the pooled one.
    let stream_cpu_started = thread_cpu_time();
    // Final ordinals, and the tool rows of every session proven
    // self-contained, are prepared here, off the canonical writer, as each
    // source is emitted — in path order on the pooled walk too, where this
    // thread is the runtime's sequencer.
    let mut preparer = StreamedPreparer::new(&files);
    let mut prepare_cpu = Duration::ZERO;
    let mut emit = |mut sources: Vec<ClosedSource>| {
        let prepare_started = stage_profiles.then(thread_cpu_time).flatten();
        preparer.prepare(&mut sources)?;
        if let Some(before) = prepare_started
            && let Some(after) = thread_cpu_time()
        {
            prepare_cpu += after.saturating_sub(before);
        }
        let bytes = sources.iter().map(ClosedSource::payload_bytes).sum();
        instrument
            .time(Metric::WriterBudgetWaitNanos, || budget.acquire(bytes))
            .map_err(rosetta_pipeline::PipelineError::Config)?;
        sender
            .send(StreamMessage::Sources {
                runtime_id: runtime.id.clone(),
                sources,
                bytes,
            })
            .map_err(|_| {
                rosetta_pipeline::PipelineError::Config("fresh cold writer stopped".into())
            })
    };
    let stream = match pool {
        Some(pool) => stream_closed_sources_pooled(
            pool,
            adapter,
            &runtime,
            &root,
            &mut planner,
            &files,
            &opts,
            &instrument,
            ledger.as_mut(),
            &mut emit,
        ),
        None => stream_closed_sources_into(
            adapter,
            &runtime,
            &root,
            &mut planner,
            &files,
            &opts,
            &instrument,
            ledger.as_mut(),
            &mut emit,
        ),
    }
    .map_err(|error| error.to_string())?;
    let stream_wall_ms = stream_started.elapsed().as_secs_f64() * 1_000.0;
    let stream_cpu_ms = cpu_millis(stream_cpu_started);
    let prepare_cpu_ms = stream_cpu_started
        .filter(|_| stage_profiles)
        .map(|_| prepare_cpu.as_secs_f64() * 1_000.0);
    let stream_profile = instrument.snapshot().delta_since(&stream_before);
    let prepared = preparer.finish().map_err(|error| error.to_string())?;
    let early = prepared.report();
    drop(planner);
    let closing_started = Instant::now();
    let wall_ms = capture_started.elapsed().as_secs_f64() * 1_000.0;
    if stage_profiles {
        // Emitted where capture ends, so the harness attributes the writer's
        // queued tail and the admission separately from capture.
        bench::emit(&json!({
            "event": "stage-profile",
            "command": "ingest",
            "stage": "parallel-metadata",
            "agent": adapter.kind(),
            "runtime": runtime.id,
            "wallMs": meta_wall_ms,
            "report": meta_profile.report(),
        }));
        bench::emit(&json!({
            "event": "stage-profile",
            "command": "ingest",
            "stage": "parallel-stream-prepare",
            "agent": adapter.kind(),
            "runtime": runtime.id,
            "wallMs": stream_wall_ms,
            "cpuMs": stream_cpu_ms,
            "captureWallMs": wall_ms,
            "report": stream_profile.report(),
            "resolvedSessions": early.sessions,
            "resolvedToolRows": early.tool_rows,
            "withheldSources": early.withheld_sources,
            "crossedSessions": early.crossed_sessions,
            "releasedSources": early.released_sources,
            "peakPendingSessions": early.peak_pending_sessions,
            "prepareMs": millis(early.nanos),
            "prepareCpuMs": prepare_cpu_ms,
        }));
        // Streamed capture closes each source inside the stream wall; no
        // serial plan preparation follows it any more. The closing work is
        // still reported, as the time it took inside capture.
        bench::emit(&json!({
            "event": "stage-profile",
            "command": "ingest",
            "stage": "cold-plan-preparation",
            "agent": adapter.kind(),
            "runtime": runtime.id,
            "wallMs": closing_started.elapsed().as_secs_f64() * 1_000.0,
            "closeSourcesMs": millis(stream_profile.get(Metric::ColdPrepareNanos)),
        }));
    }
    sender
        .send(StreamMessage::Closed(Box::new(ClosedColdRuntime {
            adapter,
            runtime,
            identity,
            root,
            roster: files,
            discovery_truncated,
            meta,
            stream,
            ledger,
            prepared,
            profile: instrument.snapshot(),
            wall_ms,
            captured_at: Instant::now(),
        })))
        .map_err(gone)
}

/// One consented runtime a fresh cold start plans, publishes and captures.
pub(crate) struct ColdRuntimeRequest {
    /// Its log name, for the metadata line.
    pub(crate) name: String,
    pub(crate) capture: ColdCaptureRequest,
}

/// The serve-scoped state the writer publishes through.
pub(crate) struct StreamedCold<'a> {
    pub(crate) flags: &'a Flags,
    pub(crate) store: &'a mut Store,
    pub(crate) instrument: &'a Instrument,
    pub(crate) hub: &'a Hub,
    pub(crate) health: &'a Arc<DaemonHealth>,
    pub(crate) board: &'a Arc<ProgressBoard>,
    pub(crate) stop: &'a AtomicBool,
    /// The base every `milestone:` line reports from.
    pub(crate) started: Instant,
}

/// Plan every requested runtime's metadata a few at a time, and capture each
/// on its own producer thread from the moment its metadata is public, while
/// this thread, the sole canonical writer, publishes metadata in registry
/// order, applies closed sources, and admits each runtime in completion
/// order. Returns the admitted runtimes for the live handoff; runtimes with
/// recorded capture failures are left unadmitted and their failures appended
/// to `failures`. A runtime that cannot be planned fails the whole build —
/// the first such runtime in registry order, as a serial start would.
pub(crate) fn build_streamed(
    requests: Vec<ColdRuntimeRequest>,
    mut cx: StreamedCold<'_>,
    failures: &mut Vec<String>,
) -> Result<Vec<AppliedColdRuntime>, String> {
    let budget = ByteBudget::new(IN_FLIGHT_BYTES);
    let stage_profiles = cx.flags.json && cx.flags.profile;
    let names: Vec<String> = requests
        .iter()
        .map(|request| request.name.clone())
        .collect();
    let queue = Mutex::new(
        requests
            .into_iter()
            .map(|request| request.capture)
            .enumerate()
            .collect::<VecDeque<_>>(),
    );
    let abandoned = AtomicBool::new(false);
    // One capture pool for the whole build: every producer registers its
    // runtime as it starts, and the pool's workers serve them all.
    let pool = CapturePool::new(
        cx.flags
            .capture_workers
            .unwrap_or_else(CapturePool::default_workers),
        PoolBudgets {
            look_ahead_bytes: cx
                .flags
                .capture_look_ahead
                .unwrap_or(PoolBudgets::DEFAULT.look_ahead_bytes),
            ..PoolBudgets::DEFAULT
        },
    );
    let pooled = (pool.workers() > 0).then_some(&pool);
    let pool_started = Instant::now();
    let built = pool.scoped(|| {
        std::thread::scope(|scope| {
            let (sender, receiver) = mpsc::channel();
            let mut handles = Vec::with_capacity(names.len() + METADATA_PLANNERS);
            for _ in 0..METADATA_PLANNERS.min(names.len()) {
                let sender = sender.clone();
                let (queue, abandoned, health, stop) = (&queue, &abandoned, cx.health, cx.stop);
                handles.push(
                    scope.spawn(move || plan_queued(queue, abandoned, &sender, health, stop)),
                );
            }
            let mut producers = Producers {
                scope,
                sender: Some(sender),
                budget: &budget,
                stage_profiles,
                pool: pooled,
                handles: &mut handles,
            };
            let written = write(receiver, &names, &mut producers, &mut cx, failures);
            if written.is_err() {
                // Planners stop before their next runtime. Producers blocked
                // on the budget must see the writer is gone; the dropped
                // receiver fails every later send, and a closed pool fails
                // every sequencer waiting on its workers.
                abandoned.store(true, Ordering::SeqCst);
                budget.abort();
                pool.close();
            }
            drop(producers);
            for handle in handles {
                handle
                    .join()
                    .map_err(|_| "fresh cold capture worker panicked".to_string())?;
            }
            written
        })
    });
    if stage_profiles && let Some(pool) = pooled {
        let stats = pool.stats();
        bench::emit(&json!({
            "event": "stage-profile",
            "command": "serve",
            "stage": "capture-pool",
            "agent": null,
            "workers": pool.workers(),
            "lookAheadBytes": pool.budgets().look_ahead_bytes,
            "wallMs": pool_started.elapsed().as_secs_f64() * 1_000.0,
            "busyMs": millis(stats.busy_nanos),
            "idleMs": millis(stats.idle_nanos),
            "tailMs": millis(stats.tail_nanos),
            "parkedMs": millis(stats.parked_nanos),
            "steps": stats.steps,
            "runtimes": stats.runtimes,
            "peakLookAheadBytes": stats.peak_look_ahead_bytes,
            "peakHeadBytes": stats.peak_head_bytes,
        }));
    }
    built
}

/// One planning thread: skim queued runtimes, earliest in registry order
/// first, until none remain or the build is abandoned, and hand the writer
/// each plan or the reason there is none.
fn plan_queued(
    queue: &Mutex<VecDeque<(usize, ColdCaptureRequest)>>,
    abandoned: &AtomicBool,
    sender: &Sender<StreamMessage>,
    health: &DaemonHealth,
    stop: &AtomicBool,
) {
    while !abandoned.load(Ordering::SeqCst) {
        let Some((index, request)) = queue
            .lock()
            .expect("cold planning queue poisoned")
            .pop_front()
        else {
            return;
        };
        let planned = if stop.load(Ordering::SeqCst) {
            Err("stopped before cold metadata completed".to_string())
        } else {
            let kind = request.adapter.kind();
            health.ingest_skim(
                &request.runtime.id,
                counter_source(&request.instrument, request.instrument.snapshot()),
            );
            // Publication is in registry order, so every runtime must answer,
            // even one whose planning panics.
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                plan_parallel_cold_runtime(request)
            }))
            .unwrap_or_else(|_| Err(format!("{kind} cold metadata planning panicked")))
            .map(Box::new)
        };
        if sender
            .send(StreamMessage::Planned { index, planned })
            .is_err()
        {
            return;
        }
    }
}

/// Starts each runtime's producer on the capture scope once its metadata is
/// public. The writer's own sender leaves with the last producer, so the
/// channel closes once every worker is gone.
struct Producers<'h, 'scope, 'env> {
    scope: &'scope Scope<'scope, 'env>,
    sender: Option<Sender<StreamMessage>>,
    budget: &'scope ByteBudget,
    stage_profiles: bool,
    pool: Option<&'scope CapturePool>,
    handles: &'h mut Vec<ScopedJoinHandle<'scope, ()>>,
}

impl Producers<'_, '_, '_> {
    fn start(&mut self, plan: PlannedColdRuntime, last: bool) {
        let sender = if last {
            self.sender.take()
        } else {
            self.sender.clone()
        };
        let Some(sender) = sender else {
            return;
        };
        let (budget, stage_profiles, pool) = (self.budget, self.stage_profiles, self.pool);
        self.handles.push(self.scope.spawn(move || {
            if let Err(error) = produce(plan, &sender, budget, stage_profiles, pool) {
                let _ = sender.send(StreamMessage::Failed(error));
            }
        }));
    }
}

/// Make one planned runtime's metadata public: only source-free canonical
/// identity rows cross this seam, and its sessions join the progress
/// board's ownership. Payload follows as closed sources, public only when the
/// runtime is admitted.
fn publish_metadata(
    plan: &PlannedColdRuntime,
    name: &str,
    ownership: &mut BTreeMap<String, Option<String>>,
    cx: &mut StreamedCold<'_>,
) -> Result<(), String> {
    for batch in plan
        .planner
        .planned_metadata_batches()
        .map_err(|error| format!("reading {name} metadata plan: {error}"))?
    {
        cx.instrument
            .apply(cx.store, &batch)
            .map_err(|error| format!("publishing {name} metadata: {error}"))?;
    }
    absorb_effects(cx.store, cx.hub)?;
    cx.health.ingest_skim_done(
        &plan.runtime.id,
        plan.meta.sessions as u64,
        plan.meta.files_skimmed as u64,
    );
    println!(
        "{name}: metadata public — {} sessions, {} projects, {} streamable files",
        plan.meta.sessions,
        plan.meta.projects,
        plan.files.len(),
    );
    ownership.extend(
        cx.store
            .runtime_session_owners(&plan.runtime.id)
            .map_err(|error| format!("reading {name} session ownership: {error}"))?,
    );
    Ok(())
}

fn write(
    receiver: Receiver<StreamMessage>,
    names: &[String],
    producers: &mut Producers<'_, '_, '_>,
    cx: &mut StreamedCold<'_>,
    failures: &mut Vec<String>,
) -> Result<Vec<AppliedColdRuntime>, String> {
    let workers = names.len();
    let mut plans: Vec<Option<Result<Box<PlannedColdRuntime>, String>>> =
        std::iter::repeat_with(|| None).take(workers).collect();
    let mut published = 0_usize;
    let mut ownership = BTreeMap::new();
    let mut open: HashMap<String, StreamedRuntime> = HashMap::new();
    let mut outputs = Vec::with_capacity(workers);
    let mut pending: Option<StreamMessage> = None;
    let mut closed = 0_usize;
    let mut idle_nanos = 0_u64;
    let mut apply_calls = 0_u64;
    let writer_started = Instant::now();
    // Writer-thread CPU, whole window and per runtime's streamed applies.
    let writer_cpu_started = thread_cpu_time();
    let mut apply_cpu: HashMap<String, Duration> = HashMap::new();
    while closed < workers {
        let message = match pending.take() {
            Some(message) => message,
            None => {
                let waited = Instant::now();
                let message = receiver
                    .recv()
                    .map_err(|_| "fresh cold capture channel closed".to_string())?;
                idle_nanos = idle_nanos
                    .saturating_add(u64::try_from(waited.elapsed().as_nanos()).unwrap_or(u64::MAX));
                message
            }
        };
        match message {
            StreamMessage::Planned { index, planned } => {
                plans[index] = Some(planned);
                // Publish in registry order, whatever order plans finish in:
                // runtimes share project rows, and the canonical row must not
                // depend on thread timing.
                while let Some(planned) = plans.get_mut(published).and_then(Option::take) {
                    let mut plan = *planned?;
                    if cx.stop.load(Ordering::SeqCst) {
                        return Err("stopped before cold metadata completed".into());
                    }
                    publish_metadata(&plan, &names[published], &mut ownership, cx)?;
                    published += 1;
                    let last = published == workers;
                    if last {
                        cx.health.ingest_metadata_ready();
                        println!(
                            "milestone: metadata-ready elapsed_s={:.2}",
                            cx.started.elapsed().as_secs_f64()
                        );
                    }
                    plan.opts.progress = Some(ColdProgress::observe(
                        &plan.runtime.id,
                        &plan.files,
                        &ownership,
                        cx.board.clone(),
                        cx.health.clone(),
                        true,
                    ));
                    let baseline = plan.instrument.snapshot();
                    cx.health.ingest_stream(
                        &plan.runtime.id,
                        counter_source(&plan.instrument, baseline),
                    );
                    producers.start(plan, last);
                }
            }
            StreamMessage::Opened(opened) => {
                let OpenedColdRuntime {
                    adapter,
                    runtime,
                    setup,
                } = *opened;
                let streamed =
                    StreamedRuntime::open(adapter, &runtime, setup, cx.store, cx.instrument)
                        .map_err(|error| {
                            format!("opening streamed {} runtime: {error}", adapter.kind())
                        })?;
                open.insert(runtime.id.clone(), streamed);
            }
            StreamMessage::Sources {
                runtime_id,
                mut sources,
                mut bytes,
            } => {
                // Queued sources of the same runtime share one transaction.
                while bytes < TRANSACTION_BYTES && sources.len() < TRANSACTION_SOURCES {
                    match receiver.try_recv() {
                        Ok(StreamMessage::Sources {
                            runtime_id: next,
                            sources: more,
                            bytes: more_bytes,
                        }) if next == runtime_id => {
                            sources.extend(more);
                            bytes = bytes.saturating_add(more_bytes);
                        }
                        Ok(other) => {
                            pending = Some(other);
                            break;
                        }
                        Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
                    }
                }
                let entry = open.get_mut(&runtime_id).ok_or_else(|| {
                    format!("streamed sources arrived for unopened runtime {runtime_id}")
                })?;
                let cpu_before = thread_cpu_time();
                entry
                    .apply(cx.store, sources, cx.instrument)
                    .map_err(|error| format!("applying streamed {runtime_id} sources: {error}"))?;
                if let (Some(before), Some(after)) = (cpu_before, thread_cpu_time()) {
                    *apply_cpu.entry(runtime_id).or_default() += after.saturating_sub(before);
                }
                apply_calls += 1;
                producers.budget.release(bytes);
            }
            StreamMessage::Closed(runtime) => {
                closed += 1;
                let runtime = *runtime;
                let streamed = open.remove(&runtime.runtime.id).ok_or_else(|| {
                    format!("streamed runtime {} closed unopened", runtime.runtime.id)
                })?;
                let applied_cpu = apply_cpu.remove(&runtime.runtime.id).unwrap_or_default();
                if let Some(output) =
                    admit(runtime, streamed, applied_cpu, &ownership, cx, failures)?
                {
                    outputs.push(output);
                }
            }
            StreamMessage::Failed(error) => return Err(error),
        }
    }
    if cx.flags.json && cx.flags.profile {
        bench::emit(&json!({
            "event": "stage-profile",
            "command": "serve",
            "stage": "streamed-writer",
            "agent": null,
            "wallMs": writer_started.elapsed().as_secs_f64() * 1_000.0,
            "idleMs": millis(idle_nanos),
            "cpuMs": cpu_millis(writer_cpu_started),
            "applyCalls": apply_calls,
        }));
    }
    Ok(outputs)
}

/// Admit one closed runtime: fold, canonical ordinals, the Pending flip, then
/// the same publication steps the retained path runs after an apply.
/// `applied_cpu` is the writer CPU its streamed sources took.
fn admit(
    runtime: ClosedColdRuntime,
    mut streamed: StreamedRuntime,
    applied_cpu: Duration,
    ownership: &BTreeMap<String, Option<String>>,
    cx: &mut StreamedCold<'_>,
    failures: &mut Vec<String>,
) -> Result<Option<AppliedColdRuntime>, String> {
    let name = identity_name(&runtime.identity);
    let mut runtime_failures = Vec::new();
    let tolerated = record_fresh_failures(
        &name,
        runtime.discovery_truncated,
        &runtime.meta,
        &runtime.stream,
        &mut runtime_failures,
    );
    if !runtime_failures.is_empty() {
        // Streamed rows stay behind the Pending barrier; the build fails
        // before anything is published.
        failures.extend(runtime_failures);
        return Ok(None);
    }
    if let Some(line) = tolerated {
        println!("{line}");
    }
    let runtime_id = runtime.runtime.id.clone();
    let kind = runtime.adapter.kind();
    cx.health
        .ingest_runtime_stage(&runtime_id, BackfillState::Applying);
    cx.instrument.merge_snapshot(&runtime.profile);
    let stage_profiles = cx.flags.json && cx.flags.profile;
    if cx.flags.json {
        bench::emit(&bench::meta_event(kind, &runtime.meta));
        bench::emit(&json!({
            "event": "stream-prepared",
            "agent": kind,
            "runtime": runtime_id,
            "records": runtime.stream.records,
            "parsed": runtime.stream.parsed,
            "unknown": runtime.stream.unknown,
            "hardErrors": runtime.stream.hard_errors,
            "oversized": runtime.stream.oversized,
            "quarantined": runtime.stream.quarantined,
            "filesMissing": runtime.stream.files_missing,
            "retriesExhausted": runtime.stream.retries_exhausted,
            "conflicts": runtime.stream.conflicts,
            "filesIncomplete": runtime.stream.files_incomplete,
            "captureWallMs": runtime.wall_ms,
        }));
    } else {
        println!(
            "{name}: streamed {} records and {} sessions in {:.2}s",
            runtime.stream.records,
            runtime.meta.sessions,
            runtime.wall_ms / 1_000.0,
        );
    }
    if stage_profiles {
        let (resolved_sessions, resolved_tool_rows) = streamed.resolved_before_admission();
        bench::emit(&json!({
            "event": "stage-profile",
            "command": "serve",
            "stage": "streamed-apply",
            "agent": kind,
            "runtime": runtime_id,
            "wallMs": millis(streamed.apply_nanos()),
            "sources": streamed.sources_applied(),
            "resolvedSessions": resolved_sessions,
            "resolvedToolRows": resolved_tool_rows,
            "cpuMs": applied_cpu.as_secs_f64() * 1_000.0,
            "lagMs": runtime.captured_at.elapsed().as_secs_f64() * 1_000.0,
        }));
    }

    let admission_started = Instant::now();
    let admission_cpu_started = thread_cpu_time();
    let admission_before = cx.instrument.snapshot();
    // Admission writes no cursor, so these stay the committed checkpoints
    // through finalization and publication.
    let poll_evidence = streamed.take_poll_evidence();
    let report = streamed
        .admit(cx.store, cx.instrument, &runtime.prepared)
        .map_err(|error| format!("admitting {name}: {error}"))?;
    if stage_profiles {
        bench::emit_command_stage_profile(
            "ingest",
            "apply-prepared",
            Some(kind),
            admission_started,
            &admission_before,
            cx.instrument,
        );
    }
    if cx.flags.json {
        bench::emit(&json!({
            "event": "canonical-applied",
            "agent": kind,
            "runtime": runtime_id,
            "files": report.files,
            "messagesInserted": report.stats.messages_inserted,
            "messagesUpdated": report.stats.messages_updated,
            "toolResultsMerged": report.stats.tool_results_merged,
        }));
    }

    let admission_cpu_ms = cpu_millis(admission_cpu_started);
    let absorb_started = Instant::now();
    let absorb_cpu_started = thread_cpu_time();
    absorb_effects(cx.store, cx.hub)?;
    if stage_profiles {
        bench::emit(&json!({
            "event": "stage-profile",
            "command": "serve",
            "stage": "admission-absorb",
            "agent": kind,
            "runtime": runtime_id,
            "wallMs": absorb_started.elapsed().as_secs_f64() * 1_000.0,
            "cpuMs": cpu_millis(absorb_cpu_started),
            "admissionCpuMs": admission_cpu_ms,
        }));
    }
    let session_ids = cx
        .store
        .runtime_session_ids(&runtime_id)
        .map_err(|error| format!("reading {name} sessions after cold admission: {error}"))?;
    cx.hub
        .invalidate_sessions(session_ids.iter().map(String::as_str), now_ms());
    for session_id in &session_ids {
        cx.board.session_done(
            ownership.get(session_id).and_then(Option::as_deref),
            session_id,
        );
    }
    cx.health.ingest_done(
        &runtime_id,
        runtime.stream.payload_bytes,
        runtime.stream.records,
    );
    println!(
        "{name}: canonical RAM public — {} records, {} sessions",
        runtime.stream.records,
        session_ids.len()
    );
    Ok(Some(AppliedColdRuntime {
        adapter: runtime.adapter,
        runtime: runtime.runtime,
        identity: runtime.identity,
        root: runtime.root,
        roster: runtime.roster,
        admission_sources: runtime.meta.admission_sources,
        ledger: runtime.ledger,
        poll_evidence,
    }))
}

#[cfg(test)]
#[path = "cold_stream_tests.rs"]
mod tests;
