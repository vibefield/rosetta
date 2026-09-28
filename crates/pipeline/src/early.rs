//! Closed sources prepared where they are emitted, off the canonical writer.
//!
//! A streamed runtime's sources reach the sole canonical writer in path
//! order. Two pieces of per-runtime work need only that order, not the store,
//! and run on the producer as each source is emitted:
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
//!   has been emitted, then resolves them here; the writer inserts the final
//!   rows in that source's transaction, and admission folds only what is left.
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
use std::time::Instant;

use rosetta_core::IngestOp;
use rosetta_store::{ResolvedSession, WithheldFacts};

use crate::PipelineError;
use crate::closed::ClosedSource;
use crate::meta::SessionFile;
use crate::streamed::OrdinalPlan;

/// What the preparer did to one source, for the writer.
#[derive(Debug, Default)]
pub(crate) struct Prepared {
    /// The source's relational facts were taken out of its operations.
    pub(crate) withheld: bool,
    /// Sessions this source completed, with their tool rows resolved.
    pub(crate) resolved: Vec<ResolvedSession>,
    /// Withheld facts returned unresolved to the runtime fold.
    pub(crate) released: Vec<WithheldFacts>,
}

/// What the preparer did for one runtime.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct EarlyToolReport {
    /// Sessions resolved on the producer, and their tool rows.
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
    /// Producer time spent preparing: ordinals, withholding, resolution.
    pub nanos: u64,
}

/// What a runtime's admission needs from its preparer once every source has
/// been emitted.
#[derive(Debug)]
pub struct PreparedAdmission {
    renumber: Vec<String>,
    report: EarlyToolReport,
}

impl PreparedAdmission {
    /// Sessions whose ordinals a cross-source repeat or an out-of-order
    /// source left to be canonicalized at admission.
    pub fn sessions_to_renumber(&self) -> &[String] {
        &self.renumber
    }

    pub fn report(&self) -> EarlyToolReport {
        self.report
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
    crossed: HashSet<String>,
    resolved: HashSet<String>,
    report: EarlyToolReport,
}

impl StreamedPreparer {
    /// A preparer for the runtime whose capture roster is `files`.
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
            crossed: HashSet::new(),
            resolved: HashSet::new(),
            report: EarlyToolReport::default(),
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

        // An abandoned generation projects nothing but drift, and its facts
        // never reach a fold: it only counts towards its session's sources.
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
                    self.cross(target, &mut prepared);
                }
                self.cross(&own, &mut prepared);
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
        if *remaining == 0
            && let Some(facts) = self.pending.remove(&own)
        {
            // Every source of the session is here and nothing crossed it:
            // its facts are the whole of its share of the runtime fold.
            let session = ResolvedSession::resolve(facts)?;
            self.report.sessions += 1;
            self.report.tool_rows += session.tool_rows();
            self.resolved.insert(own);
            prepared.resolved.push(session);
        }
        source.prepared = Some(prepared);
        Ok(())
    }

    /// Leave `session` to the admission fold: whatever of it was withheld
    /// goes back with the source being prepared.
    fn cross(&mut self, session: &str, prepared: &mut Prepared) {
        if self.crossed.insert(session.to_owned()) {
            self.report.crossed_sessions += 1;
        }
        if let Some(facts) = self.pending.remove(session) {
            self.report.released_sources += u64::try_from(facts.len()).unwrap_or(u64::MAX);
            prepared.released.extend(facts);
        }
    }
}

#[cfg(test)]
#[path = "early_tests.rs"]
mod tests;
