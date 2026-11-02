//! Streamed apply for the fresh RAM cold build (`serve` on a first run).
//!
//! One producer thread per runtime captures through its private planner and
//! hands every source to the sole canonical writer as a closed source the
//! moment nothing can supersede it (`rosetta_pipeline::closed`). The writer
//! — this thread (`writer.rs`) — writes those final rows into the shared
//! canonical RAM projection while capture continues, one bounded transaction
//! per turn, round-robin over every runtime's lanes. A runtime admitted per
//! root tree makes each tree readable as soon as its last source commits
//! (`flips.rs`): one transaction folds the tree's leftover facts and flips
//! its sessions out of `Pending`, last. Everything else is admitted when its
//! producer closes (`admit.rs`): one RAM fold writes each remaining tool row
//! once, and one stream-state commit makes it public. The read barrier is
//! unchanged — a session is `Pending` until its commit — and no runtime is
//! retained whole in RAM.
//!
//! Byte budgets bound the closed sources in flight across all producers, so
//! a fast producer cannot pile gigabytes up behind the writer while it admits
//! another runtime (`emit.rs`: per lane class, with a drain ticket each).
//!
//! A runtime admitted per root tree walks its newest trees first, in lanes
//! (`rosetta_pipeline::closed::walk_order`; the `walk:` line), and a lane
//! whose budget class is full holds its emission instead of stalling the
//! others.
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

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{Scope, ScopedJoinHandle};
use std::time::{Duration, Instant};

use rosetta_adapter_support::AgentAdapter;
use rosetta_capture::Ledger;
use rosetta_pipeline::{
    CapturePool, ClosedSource, ColdWalkOptions, Instrument, InstrumentSnapshot, LaneId, MetaReport,
    Metric, PoolBudgets, PreparedAdmission, StreamReport, StreamedPreparer, StreamedSetup,
    stream_closed_sources_into, stream_closed_sources_pooled, streamed_setup_for, walk_summary,
};
use rosetta_server::{DaemonHealth, Hub, ProgressBoard, RuntimeIdentity};
use rosetta_store::Store;
use serde_json::json;

use crate::engine::absorb_effects;
use crate::maintenance_pace::thread_cpu_time;
use crate::serve::counter_source;
use crate::{
    AppliedColdRuntime, ColdCaptureRequest, Flags, PlannedColdRuntime, bench,
    plan_parallel_cold_runtime,
};

mod admit;
mod emit;
mod flips;
mod queues;
mod walk_report;
mod writer;
use emit::{BudgetClass, LaneBudgets, ProducerEmit};
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
        /// The walk span that emitted them (the writer queues by it), and
        /// the budget they hold.
        lane: LaneId,
        class: BudgetClass,
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
    budgets: &LaneBudgets,
    stage_profiles: bool,
    pool: Option<&CapturePool>,
    walk: &ColdWalkOptions,
) -> Result<(), String> {
    let PlannedColdRuntime {
        adapter,
        runtime,
        identity,
        root,
        files,
        discovery_truncated,
        admission,
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
    // A truncated walk cannot prove a tree whole: it is admitted per runtime.
    let truncated = discovery_truncated || meta.walk_truncated;
    let setup = streamed_setup_for(&files, &mut planner, admission, truncated)
        .map_err(|error| error.to_string())?;
    // A runtime admitted per tree reads each transcript only to its cut.
    let trees = setup.trees().cloned();
    let plan = walk_summary(adapter, &files, trees.as_deref(), walk);
    walk_report::announce(
        &plan,
        &runtime.id,
        adapter.kind(),
        setup.fell_back(),
        stage_profiles,
    );
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
    // source is emitted — in each lane's order on a recency walk, and in
    // path order otherwise, where this thread is the runtime's sequencer.
    // The path walk and the serial driver emit blocking, as HEAD did.
    // A recency walk closes root trees as it goes (R1 D21); any other
    // runtime is admitted whole.
    let preparer = match &trees {
        Some(trees) if !plan.is_path() => {
            StreamedPreparer::with_trees(&files, Arc::clone(trees), plan.lane_of_tree.clone())
        }
        _ => StreamedPreparer::new(&files),
    };
    let preparer = if stage_profiles {
        preparer.measuring_big_sources(&files)
    } else {
        preparer
    };
    let mut emit = ProducerEmit::new(
        runtime.id.clone(),
        preparer,
        sender,
        budgets,
        &instrument,
        pool.is_none() || plan.is_path(),
        stage_profiles,
    );
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
            trees.as_deref(),
            walk,
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
            trees.as_deref(),
            walk,
            &mut emit,
        ),
    }
    .map_err(|error| error.to_string())?;
    let (preparer, prepare_cpu) = emit.finish();
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
        let (lane_switches, big_lane_chain_ms) = walk_report::scalars(stream.walk.as_deref());
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
            "peakPendingWithheldBytes": early.peak_pending_withheld_bytes,
            "treesClosed": early.trees_closed,
            "treesBound": early.trees_bound,
            "treesTainted": early.trees_tainted,
            "factlessResolved": early.factless_resolved,
            "lateSamples": early.late_samples,
            "crossTreeCrossings": early.cross_tree_crossings,
            "bigSourceSplits": prepared
                .big_source_splits()
                .iter()
                .map(|split| json!({
                    "readLen": split.read_len,
                    "immediateBytes": split.immediate,
                    "relationalBytes": split.relational,
                    "assetBytes": split.asset,
                }))
                .collect::<Vec<_>>(),
            "prepareMs": millis(early.nanos),
            "prepareCpuMs": prepare_cpu_ms,
            "lanes": walk_report::lanes(stream.walk.as_deref()),
            "laneSwitches": lane_switches,
            "bigLaneChainMs": big_lane_chain_ms,
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
    let pool = Arc::new(CapturePool::new(
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
    ));
    // Per-class budgets, unless a HEAD-compatible mode keeps one; a release
    // wakes every sequencer holding an emission.
    let walk = &cx.flags.cold_walk;
    let wake = Arc::clone(&pool);
    let budgets = LaneBudgets::new(
        !walk.admission_override && !walk.single_lane,
        Some(Arc::new(move || wake.notify_output())),
    );
    let walk = walk.clone();
    let pooled = (pool.workers() > 0).then_some(&*pool);
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
                budgets: &budgets,
                stage_profiles,
                pool: pooled,
                walk: &walk,
                handles: &mut handles,
            };
            let written = writer::write(receiver, &names, &mut producers, &mut cx, failures);
            if written.is_err() {
                abort_build(&abandoned, &budgets, &pool);
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
            "peakSharedLookAheadBytes": stats.peak_shared_look_ahead_bytes,
            "peakReservedLookAheadBytes": stats.peak_reserved_look_ahead_bytes,
            "peakHeadBytesTotal": stats.peak_head_bytes_total,
        }));
    }
    built
}

/// The writer failed: planners stop before their next runtime, and no
/// producer stays blocked behind it — every budget class aborts (a producer
/// may wait in a blocking acquire of either), the dropped receiver fails
/// every later send, and a closed pool fails every sequencer waiting on its
/// workers.
fn abort_build(abandoned: &AtomicBool, budgets: &LaneBudgets, pool: &CapturePool) {
    abandoned.store(true, Ordering::SeqCst);
    budgets.abort();
    pool.close();
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
    budgets: &'scope LaneBudgets,
    stage_profiles: bool,
    pool: Option<&'scope CapturePool>,
    walk: &'scope ColdWalkOptions,
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
        let (budgets, stage_profiles, pool, walk) =
            (self.budgets, self.stage_profiles, self.pool, self.walk);
        self.handles.push(self.scope.spawn(move || {
            if let Err(error) = produce(plan, &sender, budgets, stage_profiles, pool, walk) {
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

/// After publication: the RAM generation's shared-cache lock evidence —
/// the writer's session-lock waits and the readers' prologue waits, split
/// into the streaming and finalization windows, with reader lock retries
/// and refusals and writer lock-order violations (all expected 0).
/// Measurement only.
pub(crate) fn emit_shared_cache_locks(projection: &rosetta_server::ProjectionReader) {
    if let Some(locks) = projection.shared_cache_locks() {
        bench::emit(&json!({
            "event": "stage-profile",
            "command": "serve",
            "stage": "shared-cache-locks",
            "agent": null,
            "locks": locks.report(),
        }));
    }
}

#[cfg(test)]
#[path = "cold_stream_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "cold_stream/writer_tests.rs"]
mod writer_tests;
