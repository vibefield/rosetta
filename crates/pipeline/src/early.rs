//! Closed sources prepared where they are emitted, off the canonical writer.
//!
//! A streamed runtime's sources reach the sole canonical writer in emission
//! order. Three pieces of per-runtime work need only that order, not the
//! store, and run on the producer as each source is emitted:
//!
//! - **Final ordinals.** [`OrdinalPlan`] ranks every message exactly as the
//!   whole-runtime precompute would, source by source.
//! - **Tool lifecycles.** The admission fold resolves every tool correlation
//!   of the runtime at once, because in general an opener, its lifecycle
//!   updates and the usage of its response may come from any of the runtime's
//!   sources. A session whose facts provably come only from its own sources
//!   needs nothing from the others: the admission fold restricted to it is the
//!   fold of its own sources. The preparer takes each activated source's
//!   relational facts out of its batch until the session's last roster source
//!   has been emitted, then hands them to the writer with that source; the
//!   writer resolves them (R1 D22) and inserts the final rows, and admission
//!   folds only what is left.
//! - **Root-tree closure** (a runtime admitted per tree, [`closure`]): a tree
//!   whose last roster source has been emitted, that no other tree's source
//!   addressed and none of whose sources abandoned, is closed — it can be
//!   flipped on its own as soon as the writer has committed its sources.
//!
//! Nothing here assumes how an adapter behaves. A session is resolved only
//! when this runtime's own inputs prove it self-contained: every roster source
//! of the session has been emitted, and no source addressed the session from
//! outside it — no row, tool fact, usage or file edit of another session's (or
//! a ledger-only) source names it — nor did one of its own sources address
//! another session. A session crossed before it completes gets its withheld
//! facts back into the runtime fold, unresolved, as if never withheld; a
//! source that addresses an already resolved session fails the build closed,
//! the same way admission refuses a fold that meets a stored tool row.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use rosetta_core::IngestOp;
use rosetta_store::WithheldFacts;

use crate::PipelineError;
use crate::closed::{BIG_SOURCE_BYTES, ClosedSource, LaneId, TreeId, TreeMap};
use crate::meta::SessionFile;
use crate::streamed::OrdinalPlan;

mod closure;

use closure::TreeClosure;
pub use closure::{BigSourceSplit, ClosedTree};

/// What the preparer did to one source, for the writer.
#[derive(Debug, Default)]
pub(crate) struct Prepared {
    /// The source's relational facts were taken out of its operations.
    pub(crate) withheld: bool,
    /// Sessions this source completed: each one's withheld facts, which the
    /// writer resolves into its tool rows (R1 D22).
    pub(crate) to_resolve: Vec<Vec<WithheldFacts>>,
    /// Withheld facts returned unresolved to the runtime fold.
    pub(crate) released: Vec<WithheldFacts>,
    /// Approx bytes of the facts withheld from earlier emissions that this
    /// source carries to the writer: part of its writer budget.
    pub(crate) carried_bytes: u64,
    /// Root trees this source closed.
    pub(crate) closed_trees: Vec<ClosedTree>,
    /// The tree this source's abandoned generation taints.
    pub(crate) tainted: Option<TreeId>,
}

/// What the preparer did for one runtime.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct EarlyToolReport {
    /// Sessions handed to the writer to resolve, and their tool openers.
    pub sessions: u64,
    pub tool_rows: u64,
    /// Sources whose relational facts were withheld until their session
    /// completed.
    pub withheld_sources: u64,
    /// Sessions left to the admission fold because a source crossed into or
    /// out of them, and withheld sources released back to that fold.
    pub crossed_sessions: u64,
    pub released_sources: u64,
    /// The most sessions waiting for their last source at once.
    pub peak_pending_sessions: u64,
    /// Producer time spent preparing: ordinals, withholding, closure.
    pub nanos: u64,
    /// Root trees closed early, bound to another tree, and tainted by an
    /// abandoned source (a runtime admitted per tree).
    pub trees_closed: u64,
    pub trees_bound: u64,
    pub trees_tainted: u64,
    /// Sessions resolved with no fact of their own (root-tree mode).
    pub factless_resolved: u64,
    /// Usage samples (`SessionUsage`, `SessionUsageTotals`) naming another
    /// tree's session.
    pub late_samples: u64,
    /// Sources addressing a session of another tree (R1 D21; gated at 0).
    pub cross_tree_crossings: u64,
    /// The most withheld approx bytes waiting on the producer at once
    /// (unbudgeted, as at HEAD).
    pub peak_pending_withheld_bytes: u64,
}

/// What a runtime's admission needs from its preparer once every source has
/// been emitted.
#[derive(Debug)]
pub struct PreparedAdmission {
    renumber: Vec<String>,
    report: EarlyToolReport,
    big_source_splits: Vec<BigSourceSplit>,
}

impl PreparedAdmission {
    /// Sessions whose ordinals a cross-source repeat or an out-of-order
    /// source left to be canonicalized at admission. Sessions of a tree that
    /// closed early are not among them: its flip renumbers them.
    pub fn sessions_to_renumber(&self) -> &[String] {
        &self.renumber
    }

    pub fn report(&self) -> EarlyToolReport {
        self.report
    }

    /// Every source over [`BIG_SOURCE_BYTES`], as structure only.
    pub fn big_source_splits(&self) -> &[BigSourceSplit] {
        &self.big_source_splits
    }
}

/// One runtime's preparer, fed every closed source in emission order.
#[derive(Debug)]
pub struct StreamedPreparer {
    ordinals: OrdinalPlan,
    early_tools: bool,
    /// Roster sources per session not emitted yet.
    remaining: HashMap<String, u32>,
    /// Withheld facts per session, until its last source.
    pending: HashMap<String, Vec<WithheldFacts>>,
    /// Their approx bytes, per session and in all.
    pending_bytes: HashMap<String, u64>,
    pending_total: u64,
    crossed: HashSet<String>,
    resolved: HashSet<String>,
    /// Root-tree closure; `None` for a runtime admitted whole.
    trees: Option<TreeClosure>,
    /// Read lengths of the roster's sources over [`BIG_SOURCE_BYTES`], when
    /// their splits are measured.
    big_sources: Option<HashMap<String, u64>>,
    big_source_splits: Vec<BigSourceSplit>,
    report: EarlyToolReport,
}

impl StreamedPreparer {
    /// A preparer for the runtime whose capture roster is `files`, admitted
    /// whole.
    pub fn new(files: &[SessionFile]) -> Self {
        let mut remaining: HashMap<String, u32> = HashMap::new();
        for file in files {
            *remaining.entry(file.session_id.clone()).or_default() += 1;
        }
        Self {
            ordinals: OrdinalPlan::default(),
            early_tools: true,
            remaining,
            pending: HashMap::new(),
            pending_bytes: HashMap::new(),
            pending_total: 0,
            crossed: HashSet::new(),
            resolved: HashSet::new(),
            trees: None,
            big_sources: None,
            big_source_splits: Vec::new(),
            report: EarlyToolReport::default(),
        }
    }

    /// A preparer for a runtime admitted per root tree: `trees` is its
    /// [`TreeMap`] and `lane_of_tree` the lane each tree walks
    /// ([`crate::WalkSummary::lane_of_tree`]). Every tree that closes
    /// unbound and untainted is handed to the writer with its last source.
    pub fn with_trees(
        files: &[SessionFile],
        trees: Arc<TreeMap>,
        lane_of_tree: Vec<Option<LaneId>>,
    ) -> Self {
        Self {
            trees: Some(TreeClosure::new(files, trees, lane_of_tree)),
            ..Self::new(files)
        }
    }

    /// A preparer that ranks ordinals only and leaves every tool lifecycle to
    /// the admission fold.
    pub fn without_early_tools(files: &[SessionFile]) -> Self {
        Self {
            early_tools: false,
            ..Self::new(files)
        }
    }

    /// Also measure the immediate / relational / asset split of every source
    /// over [`BIG_SOURCE_BYTES`] (structure only; a profile run).
    pub fn measuring_big_sources(mut self, files: &[SessionFile]) -> Self {
        self.big_sources = Some(
            files
                .iter()
                .filter(|file| file.read_len() > BIG_SOURCE_BYTES)
                .map(|file| (file.relative_path.clone(), file.read_len()))
                .collect(),
        );
        self
    }

    pub fn report(&self) -> EarlyToolReport {
        self.report
    }

    /// Prepare the runtime's next emission, in emission order.
    pub fn prepare(&mut self, sources: &mut [ClosedSource]) -> Result<(), PipelineError> {
        let started = Instant::now();
        let result = sources
            .iter_mut()
            .try_for_each(|source| self.prepare_one(source));
        self.report.nanos = self
            .report
            .nanos
            .saturating_add(u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX));
        result
    }

    /// Close the runtime: every roster source must have been emitted.
    pub fn finish(self) -> Result<PreparedAdmission, PipelineError> {
        if let Some(session) = self.pending.keys().next() {
            return Err(PipelineError::Config(format!(
                "streamed session {session} still waits for a source that was never emitted"
            )));
        }
        Ok(PreparedAdmission {
            renumber: self.ordinals.sessions_to_renumber(),
            report: self.report,
            big_source_splits: self.big_source_splits,
        })
    }

    fn prepare_one(&mut self, source: &mut ClosedSource) -> Result<(), PipelineError> {
        if source.prepared.is_some() {
            return Err(PipelineError::Config(format!(
                "cold source {} was prepared twice",
                source.source.relative_path
            )));
        }
        let mut prepared = Prepared::default();
        self.ordinals
            .assign(&source.source.relative_path, &mut source.ops)?;
        let own = source.source.session_id.clone();
        let split = self.measure_split(source);

        // Root-tree closure first: a source that addresses another tree's
        // session in a way the walk cannot order fails before anything of
        // it is withheld.
        let own_tree = match &mut self.trees {
            Some(trees) => Some(trees.scan(source, &mut self.report)?),
            None => None,
        };

        // An abandoned generation projects nothing but drift, and its facts
        // never reach a fold: it only counts towards its session's sources.
        let mut moved = 0_u64;
        let mut withheld_here = 0_u64;
        if self.early_tools && source.activate {
            let mut outside = Vec::new();
            for op in &source.ops {
                let target = match op {
                    IngestOp::Message(row) => &row.session_id,
                    IngestOp::ToolUpdate { session_id, .. }
                    | IngestOp::Usage { session_id, .. } => session_id,
                    IngestOp::FileEdit(row) => &row.session_id,
                    _ => continue,
                };
                if (source.source.ledger_only || *target != own) && !outside.contains(target) {
                    if self.resolved.contains(target) {
                        return Err(PipelineError::Config(format!(
                            "cold source {} addresses session {target}, whose tool rows were \
                             already resolved",
                            source.source.relative_path
                        )));
                    }
                    outside.push(target.clone());
                }
            }
            if !outside.is_empty() {
                for target in &outside {
                    moved += self.cross(target, &mut prepared);
                }
                moved += self.cross(&own, &mut prepared);
            }
            if !self.crossed.contains(&own) {
                let (kept, facts) = WithheldFacts::withhold(
                    &source.source.relative_path,
                    source.generation,
                    std::mem::take(&mut source.ops),
                );
                source.ops = kept;
                prepared.withheld = true;
                self.report.withheld_sources += 1;
                if !facts.is_empty() {
                    withheld_here = facts.approx_bytes();
                    *self.pending_bytes.entry(own.clone()).or_default() += withheld_here;
                    self.pending_total = self.pending_total.saturating_add(withheld_here);
                    self.report.peak_pending_withheld_bytes = self
                        .report
                        .peak_pending_withheld_bytes
                        .max(self.pending_total);
                    self.pending.entry(own.clone()).or_default().push(facts);
                    self.report.peak_pending_sessions = self
                        .report
                        .peak_pending_sessions
                        .max(u64::try_from(self.pending.len()).unwrap_or(u64::MAX));
                }
            }
        }

        let remaining = self.remaining.get_mut(&own).ok_or_else(|| {
            PipelineError::Config(format!(
                "cold source {} is not in its runtime's roster",
                source.source.relative_path
            ))
        })?;
        *remaining = remaining.checked_sub(1).ok_or_else(|| {
            PipelineError::Config(format!(
                "session {own} emitted more sources than its roster holds"
            ))
        })?;
        let mut own_moved = false;
        if *remaining == 0 && !self.crossed.contains(&own) {
            if let Some(facts) = self.pending.remove(&own) {
                // Every source of the session is here and nothing crossed it:
                // its facts are the whole of its share of the runtime fold.
                self.report.sessions += 1;
                self.report.tool_rows += facts.iter().map(WithheldFacts::tool_openers).sum::<u64>();
                moved += self.take_pending_bytes(&own);
                own_moved = true;
                self.resolved.insert(own.clone());
                prepared.to_resolve.push(facts);
            } else if self.trees.is_some() && !source.source.ledger_only {
                // A root tree proves a session complete with or without
                // facts: a later source naming it fails closed.
                self.report.factless_resolved += 1;
                self.resolved.insert(own.clone());
            }
        }
        // What this source withheld itself is in its own payload bytes.
        prepared.carried_bytes = moved.saturating_sub(if own_moved { withheld_here } else { 0 });

        if let (Some(trees), Some(tree)) = (&mut self.trees, own_tree) {
            trees.emitted(
                tree,
                source,
                &mut self.ordinals,
                &mut prepared,
                &mut self.report,
            );
        }
        if let Some(split) = split {
            self.big_source_splits.push(split);
        }
        source.prepared = Some(prepared);
        Ok(())
    }

    /// The immediate / relational / asset split of a big source, before
    /// anything of it is withheld.
    fn measure_split(&self, source: &ClosedSource) -> Option<BigSourceSplit> {
        let read_len = *self
            .big_sources
            .as_ref()?
            .get(&source.source.relative_path)?;
        let mut split = BigSourceSplit {
            read_len,
            capture: source.capture,
            ..BigSourceSplit::default()
        };
        for op in &source.ops {
            let bytes = op.approx_bytes();
            if matches!(op, IngestOp::ProjectionAsset { .. }) {
                split.asset = split.asset.saturating_add(bytes);
                split.asset_ops += 1;
            } else if WithheldFacts::is_relational(op) {
                split.relational = split.relational.saturating_add(bytes);
            } else {
                split.immediate = split.immediate.saturating_add(bytes);
            }
        }
        Some(split)
    }

    fn take_pending_bytes(&mut self, session: &str) -> u64 {
        let bytes = self.pending_bytes.remove(session).unwrap_or(0);
        self.pending_total = self.pending_total.saturating_sub(bytes);
        bytes
    }

    /// Leave `session` to the admission fold: whatever of it was withheld
    /// goes back with the source being prepared. Returns its approx bytes.
    fn cross(&mut self, session: &str, prepared: &mut Prepared) -> u64 {
        if self.crossed.insert(session.to_owned()) {
            self.report.crossed_sessions += 1;
        }
        if let Some(facts) = self.pending.remove(session) {
            self.report.released_sources += u64::try_from(facts.len()).unwrap_or(u64::MAX);
            prepared.released.extend(facts);
            return self.take_pending_bytes(session);
        }
        0
    }
}

#[cfg(test)]
#[path = "early_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "early_trees_tests.rs"]
mod trees_tests;
