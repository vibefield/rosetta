//! The cold engine — the four-lane backfill phase as one named unit.
//!
//! `serve` reads as phases (bind, consent, skim, cold, live); this module
//! is the cold phase's machinery: the settled [`IngestPlan`], the
//! [`ProgressBoard`] fold, the [`ColdSchedule`]'s lanes, the pass loop with
//! its watcher-hint drain and mid-cold admissions, and the closing audit
//! sweep. Everything here is RAM orchestration over the store's durable
//! cursors and generations — the engine can crash and be rebuilt from a
//! skim, which is exactly what a restart does.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rosetta_adapter_support::AgentAdapter;
use rosetta_capture::{Ledger, NotifyEvents, SourceEvent, SourceEvents};
use rosetta_core::rows::{RuntimeRow, StreamState};
use rosetta_pipeline::{
    ColdLane, ColdSchedule, IngestOptions, IngestPlan, Instrument, InstrumentSnapshot, MetaReport,
    Metric, SessionFile, SliceBudget, StreamReport,
};
use rosetta_search::Fts5SearchWorker;
use rosetta_server::{
    BoostLedger, DaemonHealth, Hub, IngestCounterSource, IngestCounters, ProgressBoard,
};
use rosetta_store::{Store, queries};

use crate::record_backfill_failures;

/// Watcher hints drained per runtime per cold-loop iteration — a bounded
/// burst so a chatty agent cannot stall the pop.
const COLD_HINT_BURST: usize = 32;

pub(crate) struct BackfillRuntime {
    pub adapter: &'static dyn AgentAdapter,
    pub runtime: RuntimeRow,
    /// The log name — the kind, with the enrollment label for an enrolled
    /// runtime. Health entries key by `runtime.id`.
    pub name: String,
    pub root: PathBuf,
    pub discovery_truncated: bool,
    pub events: NotifyEvents,
    pub ledger: Option<Ledger>,
    pub meta: MetaReport,
    pub roster: Vec<SessionFile>,
    pub stream: StreamReport,
    pub stream_meter: StreamMeter,
}

#[derive(Clone)]
pub(crate) struct StreamMeter {
    instrument: Arc<Instrument>,
    state: Arc<Mutex<StreamMeterState>>,
}

struct StreamMeterState {
    baseline: InstrumentSnapshot,
    completed: IngestCounters,
    active: bool,
}

impl StreamMeter {
    pub fn new(instrument: &Arc<Instrument>) -> Self {
        Self {
            instrument: instrument.clone(),
            state: Arc::new(Mutex::new(StreamMeterState {
                baseline: instrument.snapshot(),
                completed: IngestCounters::default(),
                active: false,
            })),
        }
    }

    pub fn source(&self) -> IngestCounterSource {
        let meter = self.clone();
        Arc::new(move || meter.snapshot())
    }

    pub fn begin(&self) {
        let mut state = self.state.lock().expect("stream meter poisoned");
        state.baseline = self.instrument.snapshot();
        state.active = true;
    }

    /// Close the pass and return the physical bytes it read. The runtime
    /// meters ride this; the board's per-project meter deliberately does
    /// not — it uses the pass report's framed payload bytes instead, which
    /// never exceed the census figure the way physical reads do.
    pub fn finish(&self) -> u64 {
        let current = self.instrument.snapshot();
        let mut state = self.state.lock().expect("stream meter poisoned");
        let delta = current.delta_since(&state.baseline);
        let pass_bytes = delta.get(Metric::BytesRead);
        state.completed.bytes_read = state.completed.bytes_read.saturating_add(pass_bytes);
        state.completed.records = state
            .completed
            .records
            .saturating_add(delta.get(Metric::RecordsFramed));
        state.baseline = current;
        state.active = false;
        pass_bytes
    }

    pub fn snapshot(&self) -> IngestCounters {
        let state = self.state.lock().expect("stream meter poisoned");
        let mut counters = state.completed;
        if state.active {
            let delta = self.instrument.snapshot().delta_since(&state.baseline);
            counters.bytes_read = counters
                .bytes_read
                .saturating_add(delta.get(Metric::BytesRead));
            counters.records = counters
                .records
                .saturating_add(delta.get(Metric::RecordsFramed));
        }
        counters
    }
}

pub(crate) fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(0)
}

// @atlas step live.absorb
/// Drain both effects seams into the hub, strictly after commit. Row
/// effects first — session mutations into the rings, their touched rows
/// into the mirror — then the metadata identity keys, the writer thread's
/// replacement for the old 500 ms whole-surface rewarm poll. On cold paths
/// the row seam carries only watched sessions (the scope armed from
/// [`Hub::watched_sessions`] between passes); on the live path it carries
/// everything. Returns items fanned out to subscribers.
pub(crate) fn absorb_effects(store: &mut Store, hub: &Hub) -> Result<usize, String> {
    let mut fanned = 0_usize;
    let rows = store.drain_effects();
    if !rows.is_empty() {
        let report = hub
            .absorb(store.conn(), rows, now_ms())
            .map_err(|error| format!("absorbing row effects: {error}"))?;
        fanned += usize::try_from(report.session_mutations + report.meta_events).unwrap_or(0);
    }
    let effects = store.drain_meta_effects();
    if !effects.is_empty() {
        fanned += hub
            .absorb_meta(store.conn(), effects)
            .map_err(|error| format!("absorbing metadata effects: {error}"))?;
    }
    Ok(fanned)
}

pub(crate) fn acknowledge_search(
    store: &mut Store,
    worker: &Fts5SearchWorker,
) -> Result<(), String> {
    for ack in worker.drain_acknowledgements() {
        store
            .acknowledge_search(&ack.projection_epoch, ack.revision)
            .map_err(|error| format!("acknowledging search revision: {error}"))?;
    }
    Ok(())
}

/// Everything the cold loop borrows from the serve phase per call.
pub(crate) struct ColdCx<'a> {
    pub control: Option<&'a crate::ingestion_control::ColdControl>,
    pub store: &'a mut Store,
    pub hub: &'a Hub,
    pub health: &'a DaemonHealth,
    pub search_worker: &'a Fts5SearchWorker,
    pub instrument: &'a Arc<Instrument>,
    pub boost: &'a BoostLedger,
    pub stop: &'a AtomicBool,
    /// Full options (audit budget intact) — meta reruns and the audit sweep.
    pub opts: &'a IngestOptions,
    /// Slice-budgeted options for live/starter/completion passes.
    pub sliced: &'a IngestOptions,
    /// Unbudgeted, audit-free options for boost passes.
    pub audit_free: &'a IngestOptions,
    pub backfills: &'a mut Vec<BackfillRuntime>,
}

pub(crate) struct ColdEngine {
    schedule: ColdSchedule,
    plan: IngestPlan,
    board: ProgressBoard,
    slice_budget: Option<SliceBudget>,
    started: Instant,
    starter_files: usize,
    starter_paused: u64,
    starter_drained_logged: bool,
}

impl ColdEngine {
    /// Settle the plan from the enrolled rosters and the ownership the
    /// skims committed, seed the board's denominators, and build the
    /// four-lane schedule over the same rosters.
    pub fn new(
        rosters: Vec<Vec<SessionFile>>,
        ownership: &BTreeMap<String, Option<String>>,
        slice_budget: Option<SliceBudget>,
        started: Instant,
    ) -> Self {
        let plan = IngestPlan::settle(&rosters, ownership);
        let board = ProgressBoard::new();
        for (_, session_id, row) in plan.sessions() {
            board.seed_session(row.project_id.as_deref(), session_id, row.bytes);
        }
        for (runtime, roster) in rosters.iter().enumerate() {
            for file in roster.iter().filter(|file| !file.ledger_only) {
                if file.driver != rosetta_adapter_support::StreamDriver::JsonLines {
                    board.mark_bytes_unknown(plan.source_project_of(runtime, file));
                }
            }
        }
        let starter_files = rosters.iter().map(Vec::len).sum();
        let schedule = ColdSchedule::new(rosters);
        // The milestone line exists for the sweep harness; without slicing
        // the starter lane is just whole-file backfill and the line stays
        // absent.
        let starter_drained_logged = slice_budget.is_none() || !schedule.starter_pending();
        Self {
            schedule,
            plan,
            board,
            slice_budget,
            started,
            starter_files,
            starter_paused: 0,
            starter_drained_logged,
        }
    }

    /// Push the board's current fold onto the status surface.
    pub fn publish_projects(&self, health: &DaemonHealth) {
        health.ingest_projects(self.board.snapshot());
    }

    /// Whole-file mode has no slices to reveal breadth with — restore the
    /// old bootstrap through the boost lane: the first canonical-list
    /// session that has queued sources completes before ordinary rotation.
    pub fn bootstrap_whole_file(&mut self, store: &Store) -> Result<(), String> {
        if self.slice_budget.is_some() {
            return Ok(());
        }
        let recent = queries::session_list(store.conn(), None, 1_000)
            .map_err(|error| format!("selecting first-readable session: {error}"))?;
        for session in recent {
            if self.schedule.promote([session.id]) > 0 {
                break;
            }
        }
        Ok(())
    }

    fn log_starter_drained(&mut self) {
        if !self.starter_drained_logged && !self.schedule.starter_pending() {
            self.starter_drained_logged = true;
            println!(
                "milestone: starter-drained elapsed_s={:.2} files={} paused={}",
                self.started.elapsed().as_secs_f64(),
                self.starter_files,
                self.starter_paused,
            );
        }
    }

    /// The cold loop: live ▸ boost ▸ starter ▸ completion until the lanes
    /// drain or a stop lands. One budgeted pass per iteration; watcher hints
    /// and boosts jump ahead of the rotation between passes.
    pub fn run(&mut self, cx: &mut ColdCx<'_>) -> Result<(), String> {
        while self.schedule.pending() && !cx.stop.load(Ordering::SeqCst) {
            let resumed = crate::serve::control::disk_boundary(cx.control, cx.backfills)?;
            self.drain_hints(cx, resumed)?;
            self.schedule.promote(cx.boost.drain());
            // Check before the pass too: a promotion that empties the
            // starter lane must not charge the following (possibly long,
            // unbudgeted) boost pass to the starter-drained timing.
            self.log_starter_drained();
            let Some((lane, index, file)) = self.schedule.pop() else {
                break;
            };
            // One SQLite/document source may expose hundreds of virtual
            // session members. The scheduler picks one representative, then
            // removes every queued sibling so the snapshot driver sees the
            // whole physical source and copies it exactly once.
            let files = self.schedule.coalesce_source(index, file);
            let file = &files[0];
            // Arm the row-effects scope from the hub's open session
            // subscriptions: the pass about to run collects a mutation log
            // only for transcripts a reader is actually watching, so its
            // seal (or its appended rows) push to them instead of waiting
            // for the live phase. A subscriber arriving mid-pass is one
            // slice from the next re-arm.
            cx.store.observe_effects_watched(cx.hub.watched_sessions());
            let entry = &mut cx.backfills[index];
            cx.health
                .ingest_stream(&entry.runtime.id, entry.stream_meter.source());
            entry.stream_meter.begin();
            let pass_opts = match lane {
                ColdLane::Boost => cx.audit_free,
                ColdLane::Live | ColdLane::Starter | ColdLane::Completion => cx.sliced,
            };
            let report = rosetta_pipeline::stream_backfill_into(
                entry.adapter,
                &entry.runtime,
                &entry.root,
                cx.store,
                &files,
                pass_opts,
                cx.instrument,
                entry.ledger.as_mut(),
            )
            .map_err(|error| error.to_string())?;
            entry.stream_meter.finish();
            let paused = report.files_paused > 0;
            if paused {
                if lane == ColdLane::Starter {
                    self.starter_paused += 1;
                }
                // Only JSONL tasks can pause, and those never coalesce.
                debug_assert_eq!(files.len(), 1);
                self.schedule.requeue(index, file.clone());
            }
            // Fold the outcome onto the board: FRAMED PAYLOAD bytes to the
            // session's settled project (an unplanned session lands honestly
            // in the unassigned bucket) — physical reads amplify past any
            // census figure, payload never does — and completion when the
            // pass that just ran left nothing queued for the session.
            if let Some(projected) = files.iter().find(|file| !file.ledger_only) {
                let project = self.plan.source_project_of(index, projected);
                self.board.advance(project, report.payload_bytes);
            }
            entry.stream.merge_from(&report);
            // A sibling's pass may have badged this session Indexed while
            // other tasks for it are still queued (its own paused remainder
            // included). Readiness is the scheduler's to grant during cold:
            // hold Streaming until the session's last queued task is gone;
            // the audit pass below re-derives the final verdict either way.
            let mut sessions = BTreeMap::new();
            for member in files.iter().filter(|file| !file.ledger_only) {
                sessions
                    .entry(member.session_id.clone())
                    .or_insert_with(|| {
                        self.plan
                            .project_of(index, &member.session_id)
                            .and_then(|row| row.project_id.clone())
                    });
            }
            for (session_id, project_id) in sessions {
                if self.schedule.session_pending(&session_id) {
                    cx.store
                        .set_session_stream_state(&session_id, StreamState::Streaming)
                        .map_err(|error| format!("holding session readiness: {error}"))?;
                } else if !paused {
                    self.board.session_done(project_id.as_deref(), &session_id);
                }
            }
            self.publish_projects(cx.health);
            self.log_starter_drained();
            cx.search_worker.wake();
            acknowledge_search(cx.store, cx.search_worker)?;
            absorb_effects(cx.store, cx.hub)?;
        }
        Ok(())
    }

    /// The live lane during backfill: drain a bounded burst of watcher
    /// hints per runtime. Known, not-yet-consumed sources jump to the live
    /// lane — budgeted, so they stay preemptible — while a claimed physical
    /// path no roster entry covers re-runs bounded meta. Once a source has
    /// satisfied the metadata cut, later hints do not recreate cold work;
    /// the live phase's stat poll catches that suffix after handoff. This is
    /// the cold phase's termination guarantee under continuous writers.
    fn drain_hints(&mut self, cx: &mut ColdCx<'_>, rescan: bool) -> Result<(), String> {
        for (index, entry) in cx.backfills.iter_mut().enumerate() {
            let mut needs_admission = rescan;
            let mut nominated = BTreeSet::new();
            let mut excluded = BTreeSet::new();
            for _ in 0..COLD_HINT_BURST {
                let Some(event) = entry.events.next_event(Duration::ZERO) else {
                    break;
                };
                match event {
                    SourceEvent::Changed(path) => {
                        let mapping = if cx.opts.exclude_cwds.is_empty() {
                            rosetta_pipeline::map_hint(entry.adapter, &entry.roster, &path)
                        } else {
                            rosetta_pipeline::map_changed_hint(
                                entry.adapter,
                                &entry.roster,
                                &entry.root,
                                &path,
                            )
                        };
                        // The metadata cut already settled membership for a
                        // known physical snapshot. Its WAL movement may
                        // reprioritize those known members, but re-enumerating
                        // the database after every write would make one
                        // active agent dominate the cold loop. New claimed
                        // paths (no known nomination) still admit at once;
                        // membership changes inside a known snapshot are
                        // recovered by the live runner's opening poll.
                        needs_admission |= mapping.needs_admission && mapping.nominated.is_empty();
                        nominated.extend(mapping.nominated);
                    }
                    SourceEvent::Rescan => {}
                }
            }
            if !needs_admission && nominated.is_empty() {
                continue;
            }
            if !needs_admission && !nominated.is_empty() && !cx.opts.exclude_cwds.is_empty() {
                let candidates: Vec<_> = entry
                    .roster
                    .iter()
                    .filter(|file| nominated.contains(&file.relative_path))
                    .cloned()
                    .collect();
                let check = entry
                    .meta
                    .admission_sources
                    .recheck(
                        entry.adapter,
                        &entry.runtime,
                        &entry.root,
                        cx.store,
                        &candidates,
                        cx.opts,
                        cx.instrument,
                    )
                    .map_err(|error| error.to_string())?;
                needs_admission = check.requires_admission;
                excluded.extend(check.excluded_sessions);
                if !needs_admission {
                    nominated = check
                        .files
                        .into_iter()
                        .map(|file| file.relative_path)
                        .collect();
                }
            }
            if needs_admission {
                let (meta, fresh) = rosetta_pipeline::meta_ingest_into(
                    entry.adapter,
                    &entry.runtime,
                    &entry.root,
                    cx.store,
                    cx.opts,
                    cx.instrument,
                    entry.ledger.as_mut(),
                )
                .map_err(|error| error.to_string())?;
                excluded.extend(meta.excluded_sessions);
                entry.meta.admission_sources = meta.admission_sources;
                for file in &fresh {
                    if !entry
                        .roster
                        .iter()
                        .any(|known| known.relative_path == file.relative_path)
                    {
                        self.schedule.admit_live(index, file.clone());
                        // Admit the file to the plan and board too: a session
                        // the rerun just settled reads its ownership off the
                        // row that rerun committed; a known session grows its
                        // census share.
                        let known_project = self
                            .plan
                            .project_of(index, &file.session_id)
                            .and_then(|row| row.project_id.clone());
                        let settled = queries::session(cx.store.conn(), &file.session_id)
                            .map_err(|error| format!("reading admitted session: {error}"))?
                            .and_then(|session| session.project_id);
                        let admission = self.plan.admit(index, file, settled.clone());
                        if admission.new_session {
                            self.board.seed_session(
                                settled.as_deref(),
                                &file.session_id,
                                admission.bytes_added,
                            );
                        } else if admission.bytes_added > 0 {
                            self.board
                                .extend_session(known_project.as_deref(), admission.bytes_added);
                        }
                    }
                }
                entry.roster = fresh;
            }
            if !excluded.is_empty() {
                self.schedule.retire_sessions(&excluded);
                for id in &excluded {
                    let project = self
                        .plan
                        .project_of(index, id)
                        .and_then(|row| row.project_id.as_deref());
                    self.board.session_done(project, id);
                }
                entry
                    .roster
                    .retain(|file| !excluded.contains(&file.session_id));
            }
            for file in &entry.roster {
                if nominated.contains(&file.relative_path) {
                    self.schedule.nominate_live(index, file.clone());
                }
            }
            if needs_admission || !excluded.is_empty() {
                self.publish_projects(cx.health);
            }
        }
        Ok(())
    }

    /// Preserve the audit lane exactly once per runtime, after every
    /// ordinary file has reached its checkpoint. Passing the full roster is
    /// essential: rotating_audit_paths selects from the slice it receives.
    pub fn audit(&mut self, cx: &mut ColdCx<'_>, failures: &mut Vec<String>) -> Result<(), String> {
        if cx.stop.load(Ordering::SeqCst) {
            return Ok(());
        }
        for index in 0..cx.backfills.len() {
            crate::serve::control::disk_boundary(cx.control, cx.backfills)?;
            let entry = &mut cx.backfills[index];
            let check = entry
                .meta
                .admission_sources
                .recheck(
                    entry.adapter,
                    &entry.runtime,
                    &entry.root,
                    cx.store,
                    &entry.roster,
                    cx.opts,
                    cx.instrument,
                )
                .map_err(|error| error.to_string())?;
            entry.roster = if check.requires_admission {
                let (meta, roster) = rosetta_pipeline::meta_ingest_into(
                    entry.adapter,
                    &entry.runtime,
                    &entry.root,
                    cx.store,
                    cx.opts,
                    cx.instrument,
                    entry.ledger.as_mut(),
                )
                .map_err(|error| error.to_string())?;
                entry.meta.admission_sources = meta.admission_sources;
                roster
            } else {
                check.files
            };
            // Audit replays are cold passes too: keep the row-effects scope
            // current so a watched session's forced replay redelivers to its
            // subscribers (idempotent by deterministic ids) instead of going
            // dark.
            cx.store.observe_effects_watched(cx.hub.watched_sessions());
            cx.health
                .ingest_stream(&entry.runtime.id, entry.stream_meter.source());
            entry.stream_meter.begin();
            let report = rosetta_pipeline::stream_backfill_into(
                entry.adapter,
                &entry.runtime,
                &entry.root,
                cx.store,
                &entry.roster,
                cx.opts,
                cx.instrument,
                entry.ledger.as_mut(),
            )
            .map_err(|error| error.to_string())?;
            // Audit replays re-read finished files; the board's cold meters
            // are already settled, so the delta is not attributed.
            let _ = entry.stream_meter.finish();
            entry.stream.merge_from(&report);
            absorb_effects(cx.store, cx.hub)?;
            let stream_totals = entry.stream_meter.snapshot();
            cx.health.ingest_done(
                &entry.runtime.id,
                stream_totals.bytes_read,
                stream_totals.records,
            );
            println!(
                "{}: backfilled {} sessions ({} rows) — watching {}",
                entry.name,
                entry.meta.sessions,
                entry.stream.stats.messages_inserted,
                entry.root.display(),
            );
            record_backfill_failures(
                &entry.name,
                entry.discovery_truncated,
                &entry.meta,
                &entry.stream,
                failures,
            );
        }
        println!(
            "milestone: cold-drained elapsed_s={:.2}",
            self.started.elapsed().as_secs_f64(),
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interleaved_stream_meters_count_only_their_active_runtime() {
        let instrument = Arc::new(Instrument::new());
        let first = StreamMeter::new(&instrument);
        let second = StreamMeter::new(&instrument);
        first.begin();
        instrument.add(Metric::BytesRead, 10);
        instrument.add(Metric::RecordsFramed, 1);
        first.finish();
        second.begin();
        instrument.add(Metric::BytesRead, 20);
        instrument.add(Metric::RecordsFramed, 2);
        second.finish();
        assert_eq!(first.snapshot().bytes_read, 10);
        assert_eq!(first.snapshot().records, 1);
        assert_eq!(second.snapshot().bytes_read, 20);
        assert_eq!(second.snapshot().records, 2);
    }

    #[test]
    fn the_meter_reports_mid_pass_deltas_and_finish_returns_the_pass_bytes() {
        let instrument = Arc::new(Instrument::new());
        let meter = StreamMeter::new(&instrument);
        meter.begin();
        instrument.add(Metric::BytesRead, 7);
        assert_eq!(meter.snapshot().bytes_read, 7, "active passes read live");
        instrument.add(Metric::BytesRead, 5);
        assert_eq!(meter.finish(), 12, "finish returns this pass's delta");
        assert_eq!(meter.snapshot().bytes_read, 12);
    }
}
