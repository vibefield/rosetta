//! Root-tree closure, decided on the producer as each source is emitted.
//!
//! A runtime admitted per root tree may flip a tree readable as soon as its
//! last roster source has been applied, provided no source of another tree
//! wrote into it and none of its own sources abandoned. Every op of every
//! activated source is classified by the session it writes
//! ([`IngestOp::named_session`]). A target in another tree follows R1 D21, so
//! the outcome depends only on the walk, never on the schedule:
//!
//! - **same lane** (lanes are walked in a fixed order): the target tree not
//!   closed yet → both trees are *bound* and admitted at the runtime's close;
//!   the target tree already closed → the build fails closed;
//! - **another lane, or span `P`**: always fails closed, whatever that tree's
//!   progress — the preparer runs in emission order, which follows readiness
//!   across lanes, so any other rule would let one frozen corpus publish on
//!   one run and refuse on the next.
//!
//! A ledger-only source's samples naming its own tree's sessions (a Codex
//! guardian's parent) are normal. A session no tree knows binds the source's
//! own tree. An abandoned terminal taints its tree, which then never flips
//! early; a skipped source (generation 0) does not.

use std::collections::HashSet;
use std::sync::Arc;

use rosetta_core::IngestOp;

use super::{EarlyToolReport, Prepared};
use crate::PipelineError;
use crate::closed::{ClosedSource, LaneId, TreeId, TreeMap};
use crate::meta::SessionFile;
use crate::streamed::OrdinalPlan;

/// One root tree whose last roster source has been emitted, unbound and
/// untainted: what its flip needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClosedTree {
    pub tree: TreeId,
    /// The members with a public roster source, ascending: the sessions the
    /// flip makes readable. A metadata-only placeholder never flips.
    pub sessions: Vec<String>,
    /// Members whose ordinals a cross-source repeat left to canonicalize;
    /// the runtime's admission renumber no longer holds them.
    pub renumber: Vec<String>,
}

/// One source over the big-lane split, as structure only: its read length
/// and the approx bytes of its immediate, relational (withheld or deferred)
/// and asset ops, how many asset ops, and what its capture steps measured.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BigSourceSplit {
    pub read_len: u64,
    pub immediate: u64,
    pub relational: u64,
    pub asset: u64,
    pub asset_ops: u64,
    pub capture: crate::capture::CaptureClock,
}

#[derive(Debug)]
pub(super) struct TreeClosure {
    map: Arc<TreeMap>,
    lane_of_tree: Vec<Option<LaneId>>,
    /// Roster sources per tree not emitted yet.
    remaining: Vec<u32>,
    /// Members with a public roster source, per tree.
    required: Vec<Vec<String>>,
    bound: HashSet<TreeId>,
    tainted: HashSet<TreeId>,
    closed: HashSet<TreeId>,
}

impl TreeClosure {
    pub(super) fn new(
        files: &[SessionFile],
        map: Arc<TreeMap>,
        lane_of_tree: Vec<Option<LaneId>>,
    ) -> Self {
        let trees = map.trees().len();
        let mut remaining = vec![0_u32; trees];
        let mut required: Vec<Vec<String>> = vec![Vec::new(); trees];
        for file in files {
            let Some(tree) = map.tree_of_file(file) else {
                continue;
            };
            remaining[tree as usize] += 1;
            if !file.ledger_only {
                required[tree as usize].push(file.session_id.clone());
            }
        }
        for sessions in &mut required {
            sessions.sort_unstable();
            sessions.dedup();
        }
        Self {
            map,
            lane_of_tree,
            remaining,
            required,
            bound: HashSet::new(),
            tainted: HashSet::new(),
            closed: HashSet::new(),
        }
    }

    fn bind(&mut self, tree: TreeId, report: &mut EarlyToolReport) {
        if self.bound.insert(tree) {
            report.trees_bound += 1;
        }
    }

    /// Classify every op of `source` by the session it writes (D21) and
    /// return the source's own tree.
    pub(super) fn scan(
        &mut self,
        source: &ClosedSource,
        report: &mut EarlyToolReport,
    ) -> Result<TreeId, PipelineError> {
        let path = &source.source.relative_path;
        let own = &source.source.session_id;
        let own_tree = self.map.tree_of(own).ok_or_else(|| {
            PipelineError::Config(format!("cold source {path} belongs to no root tree"))
        })?;
        // An abandoned or skipped generation writes no row of any session.
        if !source.activate {
            return Ok(own_tree);
        }
        let mut crossed: Vec<&str> = Vec::new();
        for op in &source.ops {
            let Some(target) = op.named_session() else {
                continue;
            };
            if !source.source.ledger_only && target == own {
                continue;
            }
            let target_tree = self.map.tree_of(target);
            if target_tree == Some(own_tree) {
                continue;
            }
            if matches!(
                op,
                IngestOp::SessionUsage(_) | IngestOp::SessionUsageTotals(_)
            ) {
                report.late_samples += 1;
            }
            if crossed.contains(&target) {
                continue;
            }
            crossed.push(target);
            report.cross_tree_crossings += 1;
            let Some(target_tree) = target_tree else {
                // A session no tree knows can only wait for the close.
                self.bind(own_tree, report);
                continue;
            };
            let target_lane = self
                .lane_of_tree
                .get(target_tree as usize)
                .copied()
                .flatten();
            let name = &self.map.tree(target_tree).id;
            if source.lane == LaneId::Prefix || target_lane != Some(source.lane) {
                let lane = target_lane.map_or_else(|| "-".to_string(), |lane| lane.to_string());
                return Err(PipelineError::Config(format!(
                    "cold source {path} addresses session {target} of tree {name} in lane {lane}"
                )));
            }
            if self.closed.contains(&target_tree) {
                return Err(PipelineError::Config(format!(
                    "cold source {path} addresses session {target} of closed tree {name}"
                )));
            }
            self.bind(own_tree, report);
            self.bind(target_tree, report);
        }
        Ok(own_tree)
    }

    /// Count `source` towards its tree, taint the tree on an abandoned
    /// terminal, and close the tree at its last source when nothing bound or
    /// tainted it.
    pub(super) fn emitted(
        &mut self,
        tree: TreeId,
        source: &ClosedSource,
        ordinals: &mut OrdinalPlan,
        prepared: &mut Prepared,
        report: &mut EarlyToolReport,
    ) {
        if !source.activate && !source.skipped() {
            if self.tainted.insert(tree) {
                report.trees_tainted += 1;
            }
            prepared.tainted = Some(tree);
        }
        let remaining = &mut self.remaining[tree as usize];
        *remaining = remaining.saturating_sub(1);
        if *remaining != 0
            || self.bound.contains(&tree)
            || self.tainted.contains(&tree)
            || !self.closed.insert(tree)
        {
            return;
        }
        report.trees_closed += 1;
        prepared.closed_trees.push(ClosedTree {
            tree,
            sessions: self.required[tree as usize].clone(),
            renumber: ordinals.take_renumber(&self.map.tree(tree).sessions),
        });
    }
}
