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

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Condvar, Mutex};
use std::time::Instant;

use rosetta_adapter_support::AgentAdapter;
use rosetta_capture::Ledger;
use rosetta_pipeline::{
    ClosedSource, Instrument, InstrumentSnapshot, MetaReport, Metric, StreamReport,
    StreamedRuntime, StreamedSetup, stream_closed_sources_into, streamed_setup,
};
use rosetta_server::{BackfillState, DaemonHealth, Hub, ProgressBoard, RuntimeIdentity};
use rosetta_store::Store;
use serde_json::json;

use crate::engine::{absorb_effects, now_ms};
use crate::{
    AppliedColdRuntime, Flags, PlannedColdRuntime, bench, identity_name, record_backfill_failures,
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

enum StreamMessage {
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

/// One runtime's producer: setup, streamed capture, then the closing report.
fn produce(
    planned: PlannedColdRuntime,
    sender: &Sender<StreamMessage>,
    budget: &ByteBudget,
    stage_profiles: bool,
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
    let stream = stream_closed_sources_into(
        adapter,
        &runtime,
        &root,
        &mut planner,
        &files,
        &opts,
        &instrument,
        ledger.as_mut(),
        &mut |sources| {
            let bytes = sources.iter().map(ClosedSource::payload_bytes).sum();
            budget
                .acquire(bytes)
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
        },
    )
    .map_err(|error| error.to_string())?;
    let stream_wall_ms = stream_started.elapsed().as_secs_f64() * 1_000.0;
    let stream_profile = instrument.snapshot().delta_since(&stream_before);
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
            "captureWallMs": wall_ms,
            "report": stream_profile.report(),
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
            profile: instrument.snapshot(),
            wall_ms,
            captured_at: Instant::now(),
        })))
        .map_err(gone)
}

/// The serve-scoped state the writer publishes through.
pub(crate) struct StreamedCold<'a> {
    pub(crate) flags: &'a Flags,
    pub(crate) store: &'a mut Store,
    pub(crate) instrument: &'a Instrument,
    pub(crate) hub: &'a Hub,
    pub(crate) health: &'a DaemonHealth,
    pub(crate) board: &'a ProgressBoard,
    pub(crate) ownership: &'a BTreeMap<String, Option<String>>,
}

/// Capture every planned runtime on its own producer thread while this thread,
/// the sole canonical writer, applies their closed sources and admits each
/// runtime in completion order. Returns the admitted runtimes for the live
/// handoff; runtimes with recorded capture failures are left unadmitted and
/// their failures appended to `failures`.
pub(crate) fn build_streamed(
    planned: Vec<PlannedColdRuntime>,
    mut cx: StreamedCold<'_>,
    failures: &mut Vec<String>,
) -> Result<Vec<AppliedColdRuntime>, String> {
    let budget = ByteBudget::new(IN_FLIGHT_BYTES);
    let stage_profiles = cx.flags.json && cx.flags.profile;
    std::thread::scope(|scope| {
        let workers = planned.len();
        let (sender, receiver) = mpsc::channel();
        let mut handles = Vec::with_capacity(workers);
        for plan in planned {
            let sender = sender.clone();
            let budget = &budget;
            handles.push(scope.spawn(move || {
                if let Err(error) = produce(plan, &sender, budget, stage_profiles) {
                    let _ = sender.send(StreamMessage::Failed(error));
                }
            }));
        }
        drop(sender);

        let written = write(receiver, workers, &budget, &mut cx, failures);
        if written.is_err() {
            // Producers blocked on the budget must see the writer is gone;
            // the dropped receiver fails their next send.
            budget.abort();
        }
        for handle in handles {
            handle
                .join()
                .map_err(|_| "fresh cold capture worker panicked".to_string())?;
        }
        written
    })
}

fn write(
    receiver: Receiver<StreamMessage>,
    workers: usize,
    budget: &ByteBudget,
    cx: &mut StreamedCold<'_>,
    failures: &mut Vec<String>,
) -> Result<Vec<AppliedColdRuntime>, String> {
    let mut open: HashMap<String, StreamedRuntime> = HashMap::new();
    let mut outputs = Vec::with_capacity(workers);
    let mut pending: Option<StreamMessage> = None;
    let mut closed = 0_usize;
    let mut idle_nanos = 0_u64;
    let mut apply_calls = 0_u64;
    let writer_started = Instant::now();
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
                entry
                    .apply(cx.store, sources, cx.instrument)
                    .map_err(|error| format!("applying streamed {runtime_id} sources: {error}"))?;
                apply_calls += 1;
                budget.release(bytes);
            }
            StreamMessage::Closed(runtime) => {
                closed += 1;
                let runtime = *runtime;
                let streamed = open.remove(&runtime.runtime.id).ok_or_else(|| {
                    format!("streamed runtime {} closed unopened", runtime.runtime.id)
                })?;
                if let Some(output) = admit(runtime, streamed, cx, failures)? {
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
            "applyCalls": apply_calls,
        }));
    }
    Ok(outputs)
}

/// Admit one closed runtime: fold, canonical ordinals, the Pending flip, then
/// the same publication steps the retained path runs after an apply.
fn admit(
    runtime: ClosedColdRuntime,
    streamed: StreamedRuntime,
    cx: &mut StreamedCold<'_>,
    failures: &mut Vec<String>,
) -> Result<Option<AppliedColdRuntime>, String> {
    let name = identity_name(&runtime.identity);
    let mut runtime_failures = Vec::new();
    record_backfill_failures(
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
        bench::emit(&json!({
            "event": "stage-profile",
            "command": "serve",
            "stage": "streamed-apply",
            "agent": kind,
            "runtime": runtime_id,
            "wallMs": millis(streamed.apply_nanos()),
            "sources": streamed.sources_applied(),
            "lagMs": runtime.captured_at.elapsed().as_secs_f64() * 1_000.0,
        }));
    }

    let admission_started = Instant::now();
    let admission_before = cx.instrument.snapshot();
    let report = streamed
        .admit(cx.store, cx.instrument)
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

    absorb_effects(cx.store, cx.hub)?;
    let session_ids = cx
        .store
        .runtime_session_ids(&runtime_id)
        .map_err(|error| format!("reading {name} sessions after cold admission: {error}"))?;
    cx.hub
        .invalidate_sessions(session_ids.iter().map(String::as_str), now_ms());
    for session_id in &session_ids {
        cx.board.session_done(
            cx.ownership.get(session_id).and_then(Option::as_deref),
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
        ledger: runtime.ledger,
    }))
}
