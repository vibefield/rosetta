//! One span's emission frontier: closed sources leave in the span's order.

use std::collections::{BTreeSet, HashMap};

use rosetta_adapter_support::StreamDriver;

use super::ClosedSource;
use super::walk_order::LaneId;
use crate::PipelineError;
use crate::meta::SessionFile;

struct Slot {
    session_id: String,
    transcript: bool,
    closed: Option<ClosedSource>,
}

/// Emits a span's closed sources in its order. A transcript waits while its
/// session still has an uncaptured transcript or a pending sibling heal;
/// everything behind a waiting slot waits with it, so the writer sees each
/// span's sources in the span's order — on the path walk, the canonical path
/// order. A session's transcripts never span two lanes (a lane holds whole
/// trees), so each lane's frontier settles its own sessions.
pub(crate) struct Frontier {
    span: LaneId,
    slots: Vec<Slot>,
    position: HashMap<String, usize>,
    next: usize,
    transcripts_left: HashMap<String, usize>,
    pub(crate) restarted: BTreeSet<String>,
    /// Payload bytes of closed sources not emitted yet.
    pending_bytes: u64,
}

impl Frontier {
    pub(crate) fn new(span: LaneId, ordered: &[&SessionFile]) -> Self {
        let mut transcripts_left: HashMap<String, usize> = HashMap::new();
        let mut slots = Vec::with_capacity(ordered.len());
        let mut position = HashMap::with_capacity(ordered.len());
        for (index, file) in ordered.iter().enumerate() {
            let transcript = file.driver == StreamDriver::JsonLines;
            if transcript {
                *transcripts_left.entry(file.session_id.clone()).or_default() += 1;
            }
            position.insert(file.relative_path.clone(), index);
            slots.push(Slot {
                session_id: file.session_id.clone(),
                transcript,
                closed: None,
            });
        }
        Self {
            span,
            slots,
            position,
            next: 0,
            transcripts_left,
            restarted: BTreeSet::new(),
            pending_bytes: 0,
        }
    }

    fn settled(&self, slot: &Slot) -> bool {
        !slot.transcript
            || (self
                .transcripts_left
                .get(&slot.session_id)
                .is_none_or(|left| *left == 0)
                && !self.restarted.contains(&slot.session_id))
    }

    /// Record a source closed by the main pass.
    pub(crate) fn captured(&mut self, mut closed: ClosedSource) -> Result<(), PipelineError> {
        closed.lane = self.span;
        let index = self.slot_index(closed.relative_path())?;
        let slot = &mut self.slots[index];
        if slot.closed.is_some() || index < self.next {
            return Err(PipelineError::Config(format!(
                "cold source {} closed twice",
                closed.relative_path()
            )));
        }
        if slot.transcript
            && let Some(left) = self.transcripts_left.get_mut(&slot.session_id)
        {
            *left = left.saturating_sub(1);
        }
        self.pending_bytes = self.pending_bytes.saturating_add(closed.payload_bytes());
        slot.closed = Some(closed);
        Ok(())
    }

    /// Replace a held transcript with the replacement generation its sibling
    /// heal produced. Held means never emitted: the writer sees one terminal
    /// generation per source.
    pub(crate) fn healed(&mut self, mut closed: ClosedSource) -> Result<(), PipelineError> {
        closed.lane = self.span;
        let index = self.slot_index(closed.relative_path())?;
        if index < self.next || self.slots[index].closed.is_none() {
            return Err(PipelineError::Config(format!(
                "healed cold source {} was not held for its session",
                closed.relative_path()
            )));
        }
        let bytes = closed.payload_bytes();
        if let Some(replaced) = self.slots[index].closed.replace(closed) {
            self.pending_bytes = self
                .pending_bytes
                .saturating_sub(replaced.payload_bytes())
                .saturating_add(bytes);
        }
        Ok(())
    }

    /// Keep a held transcript whose sibling heal could not read it again, but
    /// no longer as complete.
    pub(crate) fn held_incomplete(&mut self, path: &str) -> Result<(), PipelineError> {
        let index = self.slot_index(path)?;
        match self.slots[index].closed.as_mut() {
            Some(closed) if index >= self.next => {
                closed.complete = false;
                Ok(())
            }
            _ => Err(PipelineError::Config(format!(
                "healed cold source {path} was not held for its session"
            ))),
        }
    }

    fn slot_index(&self, path: &str) -> Result<usize, PipelineError> {
        self.position.get(path).copied().ok_or_else(|| {
            PipelineError::Config(format!("closed cold source {path} is not in the roster"))
        })
    }

    pub(crate) fn transcripts_left(&self, session_id: &str) -> usize {
        self.transcripts_left.get(session_id).copied().unwrap_or(0)
    }

    /// Payload bytes captured and closed but not emitted yet.
    pub(crate) fn pending_bytes(&self) -> u64 {
        self.pending_bytes
    }

    /// Every source that may leave now, in span order.
    pub(crate) fn take_ready(&mut self) -> Vec<ClosedSource> {
        let mut ready = Vec::new();
        while self.next < self.slots.len() {
            let slot = &self.slots[self.next];
            if slot.closed.is_none() || !self.settled(slot) {
                break;
            }
            let closed = self.slots[self.next].closed.take().expect("checked above");
            self.pending_bytes = self.pending_bytes.saturating_sub(closed.payload_bytes());
            ready.push(closed);
            self.next += 1;
        }
        ready
    }

    /// Every source has left.
    pub(crate) fn finished(&self) -> bool {
        self.next == self.slots.len()
    }

    /// Fail unless every source has left: one that never closed is a bug.
    pub(crate) fn check_finished(&self) -> Result<(), PipelineError> {
        if self.finished() {
            return Ok(());
        }
        let path = self
            .position
            .iter()
            .find(|(_, index)| **index == self.next)
            .map(|(path, _)| path.clone())
            .unwrap_or_default();
        Err(PipelineError::Config(match self.span {
            LaneId::Path => format!("cold source {path} never closed"),
            span => format!("cold source {path} in lane {span} never closed"),
        }))
    }
}
