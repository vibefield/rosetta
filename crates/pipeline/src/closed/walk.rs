//! The two walks behind the closed-source entry points.
//!
//! The **path walk** is the walk every runtime admitted whole has always had:
//! one roster in path order, each source committed at its turn, emitted
//! blocking. The **lane-merge walk** serves a root-tree runtime's plan: each
//! lane keeps its own order and its own emission frontier, and the sequencer
//! walks span `P` first, serially and whole (every emission sent), then
//! repeatedly
//!
//! 0. retries every held emission, in lane order;
//! 1. takes the ready lane with the lowest global index — a lane whose next
//!    item is sequencer-local (a document or snapshot group, a non-fresh
//!    cursor) is always ready, a pool lane when its head has output queued —
//!    and otherwise waits on the pool, where a writer-budget release wakes it;
//! 2. processes exactly one event of that lane, its counters going to that
//!    unit's own report;
//! 3. closes a finished unit from its own report, heals its session's
//!    siblings when due, and emits whatever the lane's frontier lets go.
//!
//! Without a pool (the serial driver) every item is sequencer-local, so the
//! loop walks the plan's global order with blocking emission.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::time::{Duration, Instant};

use rosetta_adapter_support::{AgentAdapter, StreamDriver};
use rosetta_capture::{Ledger, Tailer};
use rosetta_core::rows::{RuntimeRow, StreamState};
use rosetta_store::{Store, queries};

use super::emit::{ColdEmit, emit_blocking};
use super::frontier::Frontier;
use super::lane_profile::WalkProfile;
use super::walk_order::{ColdWalkOptions, LaneId, WalkPlan};
use super::{close_unit, lost_sources, record_outcome, virtual_parent};
use crate::capture::{
    CaptureClock, CapturePool, Consumed, Event, LaneClass, Pooled, UnitReport, WAIT_SLICE, consume,
    consume_event, register,
};
use crate::instrument::{Instrument, Metric};
use crate::meta::SessionFile;
use crate::stream::{FilePass, PassMode, StreamReport, ingest_file, rotating_audit_paths};
use crate::{IngestOptions, PipelineError};

mod lane;
use lane::{LaneState, Merge};

/// One unit's own payload, losses and restarts.
#[derive(Debug, Clone, Copy)]
struct Attribution {
    payload: u64,
    lost: bool,
    restarted: bool,
    /// Its capture steps (measurement only).
    capture: CaptureClock,
}

impl Attribution {
    fn of(report: &StreamReport) -> Self {
        Self {
            payload: report.payload_bytes,
            lost: lost_sources(report) > 0,
            restarted: report.generations_restarted > 0,
            capture: report.capture,
        }
    }
}

/// What every walk shares: the runtime, its planner and the report.
pub(super) struct Walker<'w> {
    adapter: &'w dyn AgentAdapter,
    runtime: &'w RuntimeRow,
    root: &'w Path,
    planner: &'w mut Store,
    opts: &'w IngestOptions,
    instrument: &'w Instrument,
    tailer: Tailer,
    /// Every transcript, in path order: the sibling-heal candidates.
    transcripts: Vec<&'w SessionFile>,
    audit_paths: Vec<String>,
    report: StreamReport,
    session_health: BTreeMap<String, bool>,
    file_health: BTreeMap<String, bool>,
    restarted_files: BTreeSet<String>,
}

impl<'w> Walker<'w> {
    pub(super) fn new(
        adapter: &'w dyn AgentAdapter,
        runtime: &'w RuntimeRow,
        root: &'w Path,
        planner: &'w mut Store,
        ordered: &[&'w SessionFile],
        opts: &'w IngestOptions,
        instrument: &'w Instrument,
    ) -> Result<Self, PipelineError> {
        let tailer = Tailer::new(opts.tail.clone())?;
        let transcripts: Vec<&SessionFile> = ordered
            .iter()
            .copied()
            .filter(|file| file.driver == StreamDriver::JsonLines)
            .collect();
        let audit_paths =
            rotating_audit_paths(planner, &runtime.id, &transcripts, opts.audit_files_per_run)?;
        let session_health = ordered
            .iter()
            .map(|file| (file.session_id.clone(), true))
            .collect();
        Ok(Self {
            adapter,
            runtime,
            root,
            planner,
            opts,
            instrument,
            tailer,
            transcripts,
            audit_paths,
            report: StreamReport::default(),
            session_health,
            file_health: BTreeMap::new(),
            restarted_files: BTreeSet::new(),
        })
    }

    /// Register one span's pool sources (`None` when it has none).
    pub(super) fn register<'p>(
        &self,
        pool: &'p CapturePool,
        adapter: &'static dyn AgentAdapter,
        files: &[&SessionFile],
        plan: &WalkPlan<'_>,
        class: LaneClass,
        ledger: Option<&Ledger>,
    ) -> Result<Option<Pooled<'p>>, PipelineError> {
        register(
            pool,
            adapter,
            self.runtime,
            self.root,
            self.planner,
            files,
            &plan.path_rank,
            class,
            self.opts,
            self.instrument,
            ledger.map(Ledger::codec),
        )
    }

    fn pass_mode(&self, file: &SessionFile) -> PassMode {
        if self.audit_paths.contains(&file.relative_path) {
            PassMode::Audit
        } else {
            PassMode::Normal
        }
    }

    /// One transcript captured on this thread, into its own report.
    fn capture_inline(
        &mut self,
        file: &SessionFile,
        ledger: &mut Option<&mut Ledger>,
    ) -> Result<(FilePass, Attribution), PipelineError> {
        let mode = self.pass_mode(file);
        let mut local = StreamReport::default();
        let outcome = ingest_file(
            self.adapter,
            self.runtime,
            self.root,
            self.planner,
            file,
            &self.tailer,
            self.opts,
            self.instrument,
            &mut local,
            mode,
            ledger.as_deref_mut(),
        )?;
        self.report.merge_from(&local);
        Ok((outcome, Attribution::of(&local)))
    }

    /// A transcript's main pass ended: close it from its own report, and
    /// heal its session's other transcripts once its last one is in.
    #[allow(clippy::too_many_arguments)]
    fn transcript_done(
        &mut self,
        frontier: &mut Frontier,
        file: &SessionFile,
        outcome: FilePass,
        unit: Attribution,
        ledger: &mut Option<&mut Ledger>,
        open: &BTreeSet<i64>,
    ) -> Result<(), PipelineError> {
        self.report.files += 1;
        if unit.restarted {
            self.restarted_files.insert(file.relative_path.clone());
            frontier.restarted.insert(file.session_id.clone());
        }
        record_outcome(
            outcome,
            file,
            &mut self.report,
            &mut self.file_health,
            &mut self.session_health,
        );
        for mut closed in close_unit(
            self.adapter,
            self.runtime,
            self.planner,
            &[file],
            unit.payload,
            &self.file_health,
            unit.lost,
            open,
            self.instrument,
        )? {
            closed.capture = unit.capture;
            frontier.captured(closed)?;
        }
        // A generation restart swept and rebuilt that file's rows, but merge
        // ops a sibling contributed into them died with the old rows. Once the
        // session's last transcript is captured, force one replay of its other
        // transcripts so cross-file merges re-derive — the same heal
        // `stream_backfill_into` runs after its JSONL pass.
        if frontier.transcripts_left(&file.session_id) == 0
            && frontier.restarted.contains(&file.session_id)
        {
            self.heal(frontier, &file.session_id, ledger, open)?;
        }
        Ok(())
    }

    fn heal(
        &mut self,
        frontier: &mut Frontier,
        session_id: &str,
        ledger: &mut Option<&mut Ledger>,
        open: &BTreeSet<i64>,
    ) -> Result<(), PipelineError> {
        let siblings: Vec<&SessionFile> = self
            .transcripts
            .iter()
            .copied()
            .filter(|sibling| {
                sibling.session_id == session_id
                    && !self.restarted_files.contains(&sibling.relative_path)
            })
            .collect();
        for sibling in siblings {
            self.opts.check_cancelled()?;
            self.report.siblings_healed += 1;
            let generations_before = queries::cursor_state(
                self.planner.conn(),
                &self.runtime.id,
                &sibling.relative_path,
            )?
            .map(|cursor| cursor.highest_generation);
            let mut local = StreamReport::default();
            let outcome = ingest_file(
                self.adapter,
                self.runtime,
                self.root,
                self.planner,
                sibling,
                &self.tailer,
                self.opts,
                self.instrument,
                &mut local,
                PassMode::ForceHeal,
                ledger.as_deref_mut(),
            )?;
            self.report.merge_from(&local);
            let lost = lost_sources(&local) > 0;
            record_outcome(
                outcome,
                sibling,
                &mut self.report,
                &mut self.file_health,
                &mut self.session_health,
            );
            if lost
                && queries::cursor_state(
                    self.planner.conn(),
                    &self.runtime.id,
                    &sibling.relative_path,
                )?
                .map(|cursor| cursor.highest_generation)
                    == generations_before
            {
                // The sibling could not be read again at all, so nothing was
                // staged: the generation it was held with stays, held
                // Streaming like a live heal that lost its file.
                frontier.held_incomplete(&sibling.relative_path)?;
                continue;
            }
            for closed in close_unit(
                self.adapter,
                self.runtime,
                self.planner,
                &[sibling],
                local.payload_bytes,
                &self.file_health,
                lost,
                open,
                self.instrument,
            )? {
                frontier.healed(closed)?;
            }
        }
        frontier.restarted.remove(session_id);
        Ok(())
    }

    /// One physical document or database: its virtual members are contiguous
    /// in the span's order and read from one snapshot. `captured` runs once
    /// the group is captured and before it closes.
    fn group(
        &mut self,
        frontier: &mut Frontier,
        group: &[&SessionFile],
        ledger: &mut Option<&mut Ledger>,
        open: &BTreeSet<i64>,
        captured: &mut dyn FnMut() -> Result<(), PipelineError>,
    ) -> Result<(), PipelineError> {
        let file = group[0];
        let parent = virtual_parent(file)?;
        let mut local = StreamReport::default();
        if file.driver == StreamDriver::DocumentSnapshot {
            crate::document::stream_document_group(
                self.adapter,
                self.runtime,
                self.root,
                self.planner,
                parent,
                group,
                self.opts,
                self.instrument,
                ledger,
                &mut local,
                &mut self.file_health,
                &mut self.session_health,
            )?;
        } else {
            crate::snapshot::stream_snapshot_group(
                self.adapter,
                self.runtime,
                self.root,
                self.planner,
                parent,
                group,
                self.opts,
                self.instrument,
                ledger,
                &mut local,
                &mut self.file_health,
                &mut self.session_health,
            )?;
        }
        self.report.merge_from(&local);
        captured()?;
        for closed in close_unit(
            self.adapter,
            self.runtime,
            self.planner,
            group,
            local.payload_bytes,
            &self.file_health,
            lost_sources(&local) > 0,
            open,
            self.instrument,
        )? {
            frontier.captured(closed)?;
        }
        Ok(())
    }

    /// The walk's end: the audit rotation, the reading milestone and the
    /// planner's own readiness badges.
    fn finish(mut self) -> Result<StreamReport, PipelineError> {
        let mut last_completed_audit = None;
        for path in &self.audit_paths {
            if self.file_health.get(path).copied() != Some(true) {
                break;
            }
            last_completed_audit = Some(path.as_str());
            self.report.files_audited += 1;
        }
        if let Some(path) = last_completed_audit {
            self.planner.set_audit_path(&self.runtime.id, path)?;
        }
        if let Some(progress) = &self.opts.progress {
            progress.reading_finished();
        }
        // The planner's own readiness badges, kept for the report's
        // accounting; canonical readiness is the writer's admission flip.
        for (session_id, complete) in std::mem::take(&mut self.session_health) {
            let next = if complete {
                StreamState::Indexed
            } else {
                StreamState::Streaming
            };
            if self.planner.set_session_stream_state(&session_id, next)? && complete {
                self.report.sessions_indexed += 1;
            }
        }
        Ok(self.report)
    }

    /// The path walk: every source at its path position, `pooled` capturing
    /// the fresh JSONL ones, every emission blocking.
    pub(super) fn walk_path(
        mut self,
        ordered: &[&SessionFile],
        mut ledger: Option<&mut Ledger>,
        pooled: Option<Pooled<'_>>,
        emit: &mut dyn ColdEmit,
    ) -> Result<StreamReport, PipelineError> {
        let none = BTreeSet::new();
        let mut frontier = Frontier::new(LaneId::Path, ordered);
        let mut index = 0;
        while index < ordered.len() {
            self.opts.check_cancelled()?;
            let file = ordered[index];
            if file.driver == StreamDriver::JsonLines {
                let (outcome, unit) = match pooled
                    .as_ref()
                    .and_then(|pooled| Some((pooled, pooled.unit(index)?)))
                {
                    Some((pooled, unit)) => {
                        let mut local = StreamReport::default();
                        let outcome = consume(
                            pooled,
                            unit,
                            file,
                            self.planner,
                            ledger.as_deref_mut(),
                            &mut local,
                            self.instrument,
                            self.opts,
                        )?;
                        self.report.merge_from(&local);
                        (outcome, Attribution::of(&local))
                    }
                    None => self.capture_inline(file, &mut ledger)?,
                };
                self.transcript_done(&mut frontier, file, outcome, unit, &mut ledger, &none)?;
                index += 1;
            } else {
                let end = group_end(ordered, index)?;
                self.group(
                    &mut frontier,
                    &ordered[index..end],
                    &mut ledger,
                    &none,
                    &mut || Ok(()),
                )?;
                index = end;
            }
            let ready = frontier.take_ready();
            if !ready.is_empty() {
                emit_blocking(emit, LaneId::Path, ready)?;
            }
        }
        frontier.check_finished()?;
        self.finish()
    }

    /// The lane-merge walk of a recency `plan`: pooled (`pool` and the
    /// adapter its workers step) or serial.
    pub(super) fn walk_lanes(
        mut self,
        plan: &WalkPlan<'_>,
        mut ledger: Option<&mut Ledger>,
        pool: Option<(&CapturePool, &'static dyn AgentAdapter)>,
        walk: &ColdWalkOptions,
        emit: &mut dyn ColdEmit,
    ) -> Result<StreamReport, PipelineError> {
        let started = Instant::now();
        let can_hold = pool.is_some() && !emit.blocking();
        let mut lanes = Vec::with_capacity(plan.spans.len());
        let mut lane_index = HashMap::new();
        for (id, files) in &plan.spans {
            let class = if id.is_small_class() {
                LaneClass::Reserved
            } else {
                LaneClass::Shared
            };
            let pooled = match pool {
                Some((pool, adapter)) => {
                    self.register(pool, adapter, files, plan, class, ledger.as_deref())?
                }
                None => None,
            };
            lane_index.insert(*id, lanes.len());
            lanes.push(LaneState::new(*id, files, pooled, started));
        }
        for (global, (lane, position)) in plan.global.iter().enumerate() {
            lanes[lane_index[lane]].global_index[*position as usize] = global;
        }
        let cursors: HashMap<String, i64> = self
            .planner
            .runtime_cursor_ids(&self.runtime.id)?
            .into_iter()
            .map(|(id, path)| (path, id))
            .collect();
        let pool = pool.map(|(pool, _)| pool);
        let mut merge = Merge {
            walk,
            emit,
            can_hold,
            started,
            events: 0,
            last_served: None,
            lane_switches: 0,
            big_lane_chain_nanos: 0,
            all_lanes_held_nanos: 0,
        };
        let mut prefix_walking = prefix_open(&lanes).is_some();
        loop {
            self.opts.check_cancelled()?;
            let epoch = pool.map(CapturePool::output_epoch);
            // 0. Every held emission, in lane order.
            for lane in &mut lanes {
                if lane.held_since.is_some() {
                    merge.retry(lane)?;
                }
            }
            if lanes
                .iter()
                .all(|lane| lane.exhausted() && lane.held_since.is_none())
            {
                break;
            }
            // 1. The ready lane with the lowest global index, once span P
            //    has run whole, its emissions sent (a held P is waited for).
            let prefix = prefix_open(&lanes);
            if prefix_walking && prefix.is_none() {
                // No lane was servable while P ran: its end, like a sent
                // held emission, starts every lane's service delay.
                prefix_walking = false;
                let now = Instant::now();
                for lane in &mut lanes {
                    lane.unheld_since = lane.unheld_since.max(now);
                }
            }
            let mut candidates: Vec<usize> = (0..lanes.len())
                .filter(|&index| prefix.is_none_or(|prefix| prefix == index))
                .filter(|&index| !lanes[index].exhausted() && lanes[index].held_since.is_none())
                .collect();
            candidates.sort_by_key(|&index| lanes[index].global_index[lanes[index].next]);
            let local = candidates
                .iter()
                .position(|&index| lanes[index].next_unit().is_none());
            let heads: Vec<(usize, usize)> = candidates[..local.unwrap_or(candidates.len())]
                .iter()
                .map(|&index| {
                    let unit = lanes[index]
                        .next_unit()
                        .expect("lanes before the first local item are pool heads");
                    (index, unit)
                })
                .collect();
            let taken = match (pool, epoch) {
                (Some(pool), Some(epoch)) if !(heads.is_empty() && local.is_some()) => {
                    let wait = if local.is_some() {
                        Duration::ZERO
                    } else {
                        WAIT_SLICE
                    };
                    let refs: Vec<_> = heads
                        .iter()
                        .map(|(index, unit)| {
                            let pooled = lanes[*index]
                                .pooled
                                .as_ref()
                                .expect("a pool head's lane is pooled");
                            (&pooled.lane, *unit)
                        })
                        .collect();
                    let waited = Instant::now();
                    let ready = pool.next_ready_since(&refs, wait, self.opts, epoch)?;
                    let nanos = elapsed_nanos(waited);
                    self.instrument.add(Metric::SequencerWaitNanos, nanos);
                    if !wait.is_zero() {
                        merge.waited(&mut lanes, &heads, &candidates, nanos);
                    }
                    ready.map(|ready| (heads[ready.index], ready.event, ready.waiting_since))
                }
                _ => None,
            };
            // 2–3. One event of that lane, and whatever it finishes.
            let served = if let Some(((index, unit), event, since)) = taken {
                lanes[index].served(since);
                self.lane_event(
                    &mut lanes,
                    index,
                    unit,
                    event,
                    &mut ledger,
                    &cursors,
                    &mut merge,
                )?;
                index
            } else if let Some(position) = local {
                let index = candidates[position];
                self.lane_local(&mut lanes, index, &mut ledger, &cursors, &mut merge)?;
                index
            } else {
                continue;
            };
            if merge
                .last_served
                .replace(served)
                .is_some_and(|last| last != served)
            {
                merge.lane_switches += 1;
            }
            merge.probe(&lanes, false)?;
        }
        let mut profile = WalkProfile {
            lanes: Vec::with_capacity(lanes.len()),
            lane_switches: merge.lane_switches,
            big_lane_chain_nanos: merge.big_lane_chain_nanos,
            all_lanes_held_nanos: merge.all_lanes_held_nanos,
        };
        for lane in lanes {
            lane.frontier.check_finished()?;
            profile.lanes.push(lane.profile);
        }
        self.report.walk = Some(Box::new(profile));
        self.finish()
    }

    /// One pool event of lane `index`'s head unit.
    #[allow(clippy::too_many_arguments)]
    fn lane_event(
        &mut self,
        lanes: &mut [LaneState<'_, '_>],
        index: usize,
        unit: usize,
        event: Event,
        ledger: &mut Option<&mut Ledger>,
        cursors: &HashMap<String, i64>,
        merge: &mut Merge<'_>,
    ) -> Result<(), PipelineError> {
        let lane = &mut lanes[index];
        let file = lane.files[lane.next];
        if lane.current.is_none() {
            lane.current = Some(UnitReport::default());
            lane.global_before = snapshot(&self.report);
        }
        let consumed = consume_event(
            lane.pooled.as_ref().expect("a pool event's lane is pooled"),
            unit,
            event,
            file,
            self.planner,
            ledger.as_deref_mut(),
            lane.current.as_mut().expect("set above"),
            &mut self.report,
            self.instrument,
        )?;
        let Consumed::Finished(outcome) = consumed else {
            lane.note_unemitted();
            return Ok(());
        };
        let started = Instant::now();
        let unit_report = lane.current.take().expect("set above");
        let before = lane.global_before;
        let attribution = self.attribute(&unit_report, before);
        let open = open_heads(lanes, cursors);
        let lane = &mut lanes[index];
        self.transcript_done(
            &mut lane.frontier,
            file,
            outcome,
            attribution,
            ledger,
            &open,
        )?;
        lane.next += 1;
        lane.profile.units += 1;
        lane.profile.inline_nanos += elapsed_nanos(started);
        merge.emit(lane)
    }

    /// Lane `index`'s next item on this thread: a transcript the pool does
    /// not capture, or a physical group.
    fn lane_local(
        &mut self,
        lanes: &mut [LaneState<'_, '_>],
        index: usize,
        ledger: &mut Option<&mut Ledger>,
        cursors: &HashMap<String, i64>,
        merge: &mut Merge<'_>,
    ) -> Result<(), PipelineError> {
        let started = Instant::now();
        let open = open_heads(lanes, cursors);
        let mid_source = mid_source(lanes);
        let lane = &lanes[index];
        let file = lane.files[lane.next];
        if file.driver == StreamDriver::JsonLines {
            let (outcome, unit) = self.capture_inline(file, ledger)?;
            let lane = &mut lanes[index];
            self.transcript_done(&mut lane.frontier, file, outcome, unit, ledger, &open)?;
            lane.next += 1;
        } else {
            let end = group_end(&lane.files, lane.next)?;
            let group = lane.files[lane.next..end].to_vec();
            let prefix = lane.id == LaneId::Prefix;
            let lane = &mut lanes[index];
            self.group(&mut lane.frontier, &group, ledger, &open, &mut || {
                if prefix {
                    merge.probe_view(mid_source, true)
                } else {
                    Ok(())
                }
            })?;
            lane.next = end;
        }
        let lane = &mut lanes[index];
        lane.profile.units += 1;
        lane.profile.inline_nanos += elapsed_nanos(started);
        merge.emit(lane)
    }

    /// A finished pool unit's own payload, losses and restarts, folded into
    /// the walk's report.
    fn attribute(&mut self, unit: &UnitReport, _before: (u64, u64, u64)) -> Attribution {
        #[cfg(test)]
        if crate::capture::tests_switch::ATTRIBUTION_FROM_GLOBAL.get() {
            // v1's mutation: differences of the walk's report since the
            // unit's first event, which other lanes' events also moved.
            let now = snapshot(&self.report);
            return Attribution {
                payload: now.0 - _before.0,
                lost: now.1 > _before.1,
                restarted: now.2 > _before.2,
                capture: unit.capture,
            };
        }
        unit.deltas.merge_into(&mut self.report);
        self.report.capture.merge(&unit.capture);
        Attribution {
            payload: unit.deltas.payload_bytes,
            lost: (unit.deltas.files_missing as u64).saturating_add(unit.deltas.retries_exhausted)
                > 0,
            restarted: unit.deltas.generations_restarted > 0,
            capture: unit.capture,
        }
    }
}

/// The end of the physical group starting at `index`.
fn group_end(files: &[&SessionFile], index: usize) -> Result<usize, PipelineError> {
    let file = files[index];
    let parent = virtual_parent(file)?;
    let mut end = index + 1;
    while end < files.len()
        && files[end].driver == file.driver
        && virtual_parent(files[end])? == parent
    {
        end += 1;
    }
    Ok(end)
}

/// The cursors of every lane head mid-capture: what the planner may still
/// retain batches of.
fn open_heads(lanes: &[LaneState<'_, '_>], cursors: &HashMap<String, i64>) -> BTreeSet<i64> {
    lanes
        .iter()
        .filter(|lane| lane.current.is_some())
        .filter_map(|lane| cursors.get(&lane.files[lane.next].relative_path).copied())
        .collect()
}

/// Span `P` while it still has a source to walk or an emission to send.
fn prefix_open(lanes: &[LaneState<'_, '_>]) -> Option<usize> {
    lanes.iter().position(|lane| {
        lane.id == LaneId::Prefix && !(lane.exhausted() && lane.held_since.is_none())
    })
}

fn mid_source(lanes: &[LaneState<'_, '_>]) -> u32 {
    u32::try_from(lanes.iter().filter(|lane| lane.current.is_some()).count()).unwrap_or(u32::MAX)
}

/// The counters a unit's attribution compares under the test mutation.
fn snapshot(report: &StreamReport) -> (u64, u64, u64) {
    (
        report.payload_bytes,
        lost_sources(report),
        report.generations_restarted,
    )
}

fn elapsed_nanos(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
}
