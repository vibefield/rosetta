//! One lane of a lane-merge walk, and the loop's emission and measurement
//! state.

use std::time::Instant;

use super::super::ClosedSource;
use super::super::emit::{ColdEmit, EmitOutcome, emit_blocking};
use super::super::frontier::Frontier;
use super::super::lane_profile::LaneProfile;
use super::super::walk_order::{ColdWalkOptions, LaneId, ProbeView, read_len};
use super::{elapsed_nanos, mid_source};
use crate::PipelineError;
use crate::capture::{Pooled, UnitReport};
use crate::meta::SessionFile;

/// One lane of a lane-merge walk.
pub(super) struct LaneState<'f, 'p> {
    pub(super) id: LaneId,
    pub(super) files: Vec<&'f SessionFile>,
    /// Each lane position's index in the plan's global order.
    pub(super) global_index: Vec<usize>,
    pub(super) next: usize,
    pub(super) pooled: Option<Pooled<'p>>,
    pub(super) frontier: Frontier,
    /// The head unit's own report, from its first event to its close.
    pub(super) current: Option<UnitReport>,
    /// The walk's report counters at the head unit's first event (the
    /// attribution mutation's baseline).
    pub(super) global_before: (u64, u64, u64),
    pub(super) held_since: Option<Instant>,
    pub(super) held_bytes: u64,
    /// When the lane last became servable: the walk began, span P ended, or
    /// it stopped holding an emission.
    pub(super) unheld_since: Instant,
    pub(super) ended: bool,
    pub(super) profile: LaneProfile,
}

impl<'f, 'p> LaneState<'f, 'p> {
    pub(super) fn new(
        id: LaneId,
        files: &[&'f SessionFile],
        pooled: Option<Pooled<'p>>,
        started: Instant,
    ) -> Self {
        let mut profile = LaneProfile::new(id);
        profile.sources = files.len() as u64;
        profile.bytes = files
            .iter()
            .map(|file| read_len(file))
            .fold(0, u64::saturating_add);
        Self {
            id,
            files: files.to_vec(),
            global_index: vec![0; files.len()],
            next: 0,
            pooled,
            frontier: Frontier::new(id, files),
            current: None,
            global_before: (0, 0, 0),
            held_since: None,
            held_bytes: 0,
            unheld_since: started,
            ended: false,
            profile,
        }
    }

    pub(super) fn exhausted(&self) -> bool {
        self.next == self.files.len()
    }

    /// The pool unit of the lane's next item; `None` when it is
    /// sequencer-local.
    pub(super) fn next_unit(&self) -> Option<usize> {
        self.pooled.as_ref()?.unit(self.next)
    }

    /// The sequencer takes an event its head queued at `since`.
    pub(super) fn served(&mut self, since: Option<Instant>) {
        if let Some(since) = since {
            let from = since.max(self.unheld_since);
            self.profile
                .service_delay
                .record(Instant::now().saturating_duration_since(from));
        }
    }

    pub(super) fn note_unemitted(&mut self) {
        let head = self
            .current
            .as_ref()
            .map_or(0, |unit| unit.deltas.payload_bytes);
        let unemitted = head
            .saturating_add(self.frontier.pending_bytes())
            .saturating_add(self.held_bytes);
        self.profile.peak_unemitted_head_bytes =
            self.profile.peak_unemitted_head_bytes.max(unemitted);
    }
}

/// The lane-merge loop's emission and measurement state.
pub(super) struct Merge<'m> {
    pub(super) walk: &'m ColdWalkOptions,
    pub(super) emit: &'m mut dyn ColdEmit,
    pub(super) can_hold: bool,
    pub(super) started: Instant,
    pub(super) events: u64,
    pub(super) last_served: Option<usize>,
    pub(super) lane_switches: u64,
    pub(super) big_lane_chain_nanos: u64,
    pub(super) all_lanes_held_nanos: u64,
}

impl Merge<'_> {
    /// Emit whatever `lane`'s frontier lets go; hold it when the writer's
    /// budget for its class is full.
    pub(super) fn emit(&mut self, lane: &mut LaneState<'_, '_>) -> Result<(), PipelineError> {
        let ready = lane.frontier.take_ready();
        if !ready.is_empty() {
            let bytes = ready
                .iter()
                .map(ClosedSource::payload_bytes)
                .fold(0, u64::saturating_add);
            let started = Instant::now();
            let outcome = if self.can_hold {
                self.emit.emit(lane.id, ready)?
            } else {
                emit_blocking(self.emit, lane.id, ready)?;
                EmitOutcome::Sent
            };
            lane.profile.inline_nanos += elapsed_nanos(started);
            if outcome == EmitOutcome::Held {
                lane.held_since = Some(Instant::now());
                lane.held_bytes = bytes;
            }
        }
        lane.note_unemitted();
        self.check_end(lane);
        Ok(())
    }

    pub(super) fn retry(&mut self, lane: &mut LaneState<'_, '_>) -> Result<(), PipelineError> {
        let started = Instant::now();
        let outcome = self.emit.retry(lane.id)?;
        lane.profile.inline_nanos += elapsed_nanos(started);
        if outcome == EmitOutcome::Sent {
            let now = Instant::now();
            if let Some(since) = lane.held_since.take() {
                lane.profile
                    .held
                    .record(now.saturating_duration_since(since));
            }
            lane.held_bytes = 0;
            lane.unheld_since = now;
            self.check_end(lane);
        }
        Ok(())
    }

    pub(super) fn check_end(&self, lane: &mut LaneState<'_, '_>) {
        if !lane.ended && lane.exhausted() && lane.held_since.is_none() && lane.frontier.finished()
        {
            lane.ended = true;
            lane.profile.end_nanos = elapsed_nanos(self.started);
        }
    }

    /// The sequencer waited `nanos` on `heads` while `candidates` were live.
    pub(super) fn waited(
        &mut self,
        lanes: &mut [LaneState<'_, '_>],
        heads: &[(usize, usize)],
        candidates: &[usize],
        nanos: u64,
    ) {
        for (index, _) in heads {
            lanes[*index].profile.head_wait_nanos += nanos;
        }
        if !candidates.is_empty() && candidates.iter().all(|index| lanes[*index].id.is_big()) {
            self.big_lane_chain_nanos += nanos;
        }
        // Live: not yet walked to its end, or holding its last emission.
        let mut live = lanes
            .iter()
            .filter(|lane| !lane.exhausted() || lane.held_since.is_some())
            .peekable();
        if live.peek().is_some() && live.all(|lane| lane.held_since.is_some()) {
            self.all_lanes_held_nanos += nanos;
        }
    }

    pub(super) fn probe(
        &mut self,
        lanes: &[LaneState<'_, '_>],
        prefix_mid_group: bool,
    ) -> Result<(), PipelineError> {
        self.probe_view(mid_source(lanes), prefix_mid_group)
    }

    /// Show the probe this event; cancel the walk if it says so.
    pub(super) fn probe_view(
        &mut self,
        lanes_mid_source: u32,
        prefix_mid_group: bool,
    ) -> Result<(), PipelineError> {
        let Some(probe) = &self.walk.probe else {
            return Ok(());
        };
        self.events += 1;
        let counters = self.walk.probe_counters.as_deref();
        let view = ProbeView {
            event: self.events,
            lanes_mid_source,
            items_mid_sequence: counters.map_or(0, |counters| {
                counters
                    .items_mid_sequence
                    .load(std::sync::atomic::Ordering::SeqCst)
            }),
            flips_committed: counters.map_or(0, |counters| {
                counters
                    .flips_committed
                    .load(std::sync::atomic::Ordering::SeqCst)
            }),
            prefix_mid_group,
        };
        if probe(&view) {
            return Err(PipelineError::Cancelled);
        }
        Ok(())
    }
}
