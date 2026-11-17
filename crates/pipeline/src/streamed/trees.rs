//! Per-tree flips of a runtime admitted per root tree (R1 D8, D9).
//!
//! The preparer closes a tree at its last roster source ([`ClosedTree`]).
//! The writer's [`TreeLedger`] makes it *ready* only once every source of
//! the tree has committed its terminal transaction — the closing one and any
//! other lane's (a tree's members hoisted into span `P` reach the writer on
//! their own queue) — so a flip never precedes a row of its tree.
//! [`StreamedRuntime::admit_trees`] then admits a batch of ready trees in one
//! writer transaction ([`rosetta_store::Store::admit_cold_units`]): the
//! guard, their partitioned tails folded, their renumber subset, and their
//! readiness flip last. A flipped tree is never Pending again.

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Instant;

use rosetta_capture::SourceDef;
use rosetta_core::rows::StreamState;
use rosetta_store::{ApplyStats, ColdReconciliation, Store};

use super::StreamedRuntime;
use crate::PipelineError;
use crate::closed::{TreeId, TreeMap};
use crate::early::ClosedTree;
use crate::instrument::Instrument;

/// The caps of one flip batch (D9). A single tree over a cap flips alone:
/// a tree is atomic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlipCaps {
    pub trees: usize,
    pub sessions: usize,
    pub tail_bytes: u64,
}

/// What one flip batch admitted.
#[derive(Debug, Default)]
pub struct TreeAdmission {
    pub trees: Vec<TreeId>,
    /// The sessions whose `stream_state` the flip moved.
    pub changed: Vec<String>,
    pub stats: ApplyStats,
    pub wall_ns: u64,
    pub lock_wait_ns: u64,
    /// From the transaction's W1 to its commit (measurement only).
    pub lock_held_ns: u64,
    /// When the batch's work began, after its trees were chosen, and when
    /// its transaction committed (measurement only).
    pub started_at: Option<Instant>,
    pub committed_at: Option<Instant>,
    /// Approx bytes of the folded tails.
    pub tail_bytes: u64,
    pub renumber_rows: u64,
}

/// The writer's view of a per-tree runtime's trees.
#[derive(Debug)]
pub(crate) struct TreeLedger {
    /// Each roster source's tree, by path.
    index: HashMap<String, TreeId>,
    /// Roster sources per tree whose terminal has not committed.
    unapplied: HashMap<TreeId, u32>,
    /// Closed trees with a source still to commit, in closing order.
    waiting: Vec<ClosedTree>,
    ready: VecDeque<ClosedTree>,
    flipped: HashSet<TreeId>,
    flipped_sessions: HashSet<String>,
}

impl TreeLedger {
    pub(crate) fn new(sources: &[SourceDef], trees: &TreeMap) -> Self {
        let mut index = HashMap::with_capacity(sources.len());
        let mut unapplied: HashMap<TreeId, u32> = HashMap::new();
        for source in sources {
            if let Some(tree) = trees.tree_of(&source.session_id) {
                index.insert(source.relative_path.clone(), tree);
                *unapplied.entry(tree).or_default() += 1;
            }
        }
        Self {
            index,
            unapplied,
            waiting: Vec::new(),
            ready: VecDeque::new(),
            flipped: HashSet::new(),
            flipped_sessions: HashSet::new(),
        }
    }

    /// The fold's partition: each source path's tree.
    pub(crate) fn unit_of_path(&self) -> HashMap<String, u32> {
        self.index.clone()
    }

    /// A source's terminal committed.
    pub(crate) fn applied(&mut self, path: &str) {
        if let Some(tree) = self.index.get(path)
            && let Some(left) = self.unapplied.get_mut(tree)
        {
            *left = left.saturating_sub(1);
        }
    }

    /// A tree the preparer closed, carried by a source that just committed.
    pub(crate) fn closed(&mut self, tree: ClosedTree) {
        self.waiting.push(tree);
    }

    /// Move every waiting tree whose sources have all committed to ready, in
    /// closing order.
    pub(crate) fn promote(&mut self) {
        let unapplied = &self.unapplied;
        let (ready, waiting): (Vec<_>, Vec<_>) = std::mem::take(&mut self.waiting)
            .into_iter()
            .partition(|tree| unapplied.get(&tree.tree).copied().unwrap_or(0) == 0);
        self.ready.extend(ready);
        self.waiting = waiting;
    }

    pub(crate) fn ready(&self) -> usize {
        self.ready.len()
    }

    /// Every tree closed but not flipped, ready or waiting: the runtime's
    /// close admits them with its remainder.
    pub(crate) fn take_unflipped(&mut self) -> Vec<ClosedTree> {
        let mut trees: Vec<ClosedTree> = self.ready.drain(..).collect();
        trees.append(&mut self.waiting);
        trees
    }

    pub(crate) fn is_flipped(&self, session: &str) -> bool {
        self.flipped_sessions.contains(session)
    }

    pub(crate) fn flipped_trees(&self) -> usize {
        self.flipped.len()
    }

    pub(crate) fn flipped_sessions(&self) -> usize {
        self.flipped_sessions.len()
    }

    pub(crate) fn flipped_session_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.flipped_sessions.iter().cloned().collect();
        ids.sort_unstable();
        ids
    }
}

impl StreamedRuntime {
    /// Trees ready to flip.
    pub fn ready_trees(&self) -> usize {
        self.trees.as_ref().map_or(0, TreeLedger::ready)
    }

    /// Trees flipped so far.
    pub fn flipped_trees(&self) -> usize {
        self.trees.as_ref().map_or(0, TreeLedger::flipped_trees)
    }

    /// Sessions flipped so far.
    pub fn flipped_sessions(&self) -> usize {
        self.trees.as_ref().map_or(0, TreeLedger::flipped_sessions)
    }

    /// Every session flipped so far, ascending.
    pub fn flipped_session_ids(&self) -> Vec<String> {
        self.trees
            .as_ref()
            .map_or_else(Vec::new, TreeLedger::flipped_session_ids)
    }

    /// Those of `sessions` already flipped: a usage displacement there
    /// changed rows a reader may be looking at (R1 D15).
    pub fn flipped_among<'a>(&self, sessions: impl IntoIterator<Item = &'a String>) -> Vec<String> {
        let Some(trees) = &self.trees else {
            return Vec::new();
        };
        sessions
            .into_iter()
            .filter(|session| trees.is_flipped(session))
            .cloned()
            .collect()
    }

    /// Flip one batch of ready trees, up to `caps`, in one writer
    /// transaction: their tails folded, their renumber subset, and their
    /// readiness flip last. Returns what it admitted (no tree when none was
    /// ready).
    pub fn admit_trees(
        &mut self,
        store: &mut Store,
        instrument: &Instrument,
        caps: FlipCaps,
    ) -> Result<TreeAdmission, PipelineError> {
        let Some(trees) = &mut self.trees else {
            return Ok(TreeAdmission::default());
        };
        let mut batch: Vec<ClosedTree> = Vec::new();
        let (mut sessions, mut tail_bytes) = (0_usize, 0_u64);
        while let Some(front) = trees.ready.front() {
            let bytes = self.reconciliation.unit_bytes(front.tree);
            if !batch.is_empty()
                && (batch.len() + 1 > caps.trees
                    || sessions + front.sessions.len() > caps.sessions
                    || tail_bytes.saturating_add(bytes) > caps.tail_bytes)
            {
                break;
            }
            sessions += front.sessions.len();
            tail_bytes = tail_bytes.saturating_add(bytes);
            batch.push(trees.ready.pop_front().expect("front exists"));
        }
        if batch.is_empty() {
            return Ok(TreeAdmission::default());
        }
        let started = Instant::now();
        let mut fold = ColdReconciliation::default();
        let mut renumber = Vec::new();
        let mut states = Vec::with_capacity(sessions);
        for tree in &batch {
            fold.merge(self.reconciliation.take_unit(tree.tree)?)?;
            renumber.extend(tree.renumber.iter().cloned());
            for session in &tree.sessions {
                let clean = self.session_health.remove(session).ok_or_else(|| {
                    PipelineError::Config(format!(
                        "tree {} flipped before session {session} applied a source",
                        tree.tree
                    ))
                })?;
                states.push((session.clone(), stream_state(clean)));
            }
        }
        let admitted = instrument.admit_cold_units(store, fold, &renumber, &states)?;
        for tree in &batch {
            trees.flipped.insert(tree.tree);
            trees.flipped_sessions.extend(tree.sessions.iter().cloned());
        }
        crate::stream::merge_stats(&mut self.report.stats, &admitted.stats);
        Ok(TreeAdmission {
            trees: batch.iter().map(|tree| tree.tree).collect(),
            changed: admitted.changed,
            stats: admitted.stats,
            wall_ns: u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX),
            lock_wait_ns: admitted.lock_wait_nanos,
            lock_held_ns: admitted.lock_held_nanos,
            started_at: Some(started),
            committed_at: admitted.committed_at,
            tail_bytes,
            renumber_rows: admitted.renumber_rows,
        })
    }

    /// Readiness states of every session not flipped yet, and the sessions
    /// the close must renumber besides the preparer's: those of trees that
    /// closed but never flipped.
    pub(super) fn remainder(&mut self) -> (Vec<(String, StreamState)>, Vec<String>) {
        let mut renumber = Vec::new();
        if let Some(trees) = &mut self.trees {
            for tree in trees.take_unflipped() {
                renumber.extend(tree.renumber);
            }
        }
        let states = std::mem::take(&mut self.session_health)
            .into_iter()
            .map(|(session, clean)| (session, stream_state(clean)))
            .collect();
        (states, renumber)
    }
}

/// A source-backed session's badge: Indexed when every source of it was a
/// complete pass, else Streaming, as a live pass would leave it.
pub(super) fn stream_state(clean: bool) -> StreamState {
    if clean {
        StreamState::Indexed
    } else {
        StreamState::Streaming
    }
}
