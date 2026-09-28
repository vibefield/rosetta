//! Tool lifecycles resolved per session, before the writer.
//!
//! A streamed fresh-cold runtime folds its relational facts at admission,
//! because in general a correlation's opener, lifecycle facts and response
//! usage may come from any of the runtime's sources. A session whose facts are
//! proven to come only from its own sources needs none of the other sessions'
//! facts: the runtime fold restricted to that session is the fold of those
//! sources alone. The producer therefore takes each activated source's
//! relational facts out of its batch ([`WithheldFacts::withhold`]) until the
//! session's last source has been emitted, and then folds them
//! ([`ResolvedSession::resolve`]) with the same collect-and-finish code the
//! admission fold runs. The writer inserts the resolved rows once, inside the
//! transaction of the source that completed the session; what the session's
//! rows did not absorb — usage for other rows, file edits — joins the runtime
//! fold at the positions the whole-runtime fold would have given it. A session
//! the producer cannot prove self-contained gets its withheld facts back into
//! the runtime fold instead, unresolved.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use rosetta_core::{IngestOp, TimelineEntry};
use rusqlite::Transaction;

use crate::StoreError;
use crate::apply::rows::{
    BULK_MAINTENANCE, BucketKey, apply_reconciled_tool_message, flush_session_touches,
    touch_session,
};
use crate::apply::{ApplyStats, SourceMode};

use super::fold::{FoldState, split_owned};
use super::{BulkReconciliationDelta, BulkReconciliationPlan, SourceKey, deferred_op, defers};

/// One activated closed source's relational facts — tool openers, lifecycle
/// updates, usage and file edits — held apart from its batch. Process-local:
/// the raw ledger remains the recovery source.
#[derive(Debug)]
pub struct WithheldFacts {
    path: String,
    generation: u64,
    /// The source's operation count: its activation's end-exclusive sequence.
    activation_end: u64,
    /// The canonical cursor of the source, once the writer bound it.
    cursor_id: Option<i64>,
    /// Each fact with its source position, in position order.
    deferred: Vec<(u64, IngestOp)>,
}

impl WithheldFacts {
    /// Take every relational fact out of one activated source's `ops`,
    /// leaving its other operations, in order, for the source's batch.
    pub fn withhold(path: &str, generation: u64, ops: Vec<IngestOp>) -> (Vec<IngestOp>, Self) {
        let activation_end = u64::try_from(ops.len()).unwrap_or(u64::MAX);
        let mut kept = Vec::with_capacity(ops.len());
        let mut deferred = Vec::new();
        for (position, op) in (0_u64..).zip(ops) {
            if defers(&op) {
                deferred.push((position, op));
            } else {
                kept.push(op);
            }
        }
        (
            kept,
            Self {
                path: path.to_owned(),
                generation,
                activation_end,
                cursor_id: None,
                deferred,
            },
        )
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn is_empty(&self) -> bool {
        self.deferred.is_empty()
    }

    /// Whether any fact is a tool opener or lifecycle update, as opposed to
    /// usage and file edits alone.
    pub fn has_tool_facts(&self) -> bool {
        self.deferred
            .iter()
            .any(|(_, op)| !matches!(op, IngestOp::Usage { .. } | IngestOp::FileEdit(_)))
    }

    /// Bind the source's canonical cursor, stamping it on every withheld row
    /// as the writer stamps the rows it applies.
    pub fn bind_cursor(&mut self, cursor_id: i64) {
        self.cursor_id = Some(cursor_id);
        for (_, op) in &mut self.deferred {
            if let IngestOp::Message(row) = op {
                row.provenance.src_cursor = Some(cursor_id);
            }
        }
    }

    /// The runtime fold's delta for these facts: what the source's own batch
    /// would have contributed.
    pub(crate) fn into_delta(self) -> Result<(BulkReconciliationDelta, String), StoreError> {
        let cursor_id = self.cursor_id.ok_or_else(|| StoreError::BulkLoadState {
            message: format!(
                "withheld facts of {} were never bound to a cursor",
                self.path
            ),
        })?;
        let deferred = self
            .deferred
            .into_iter()
            .map(|(position, op)| (position, deferred_op(op)))
            .collect();
        Ok((
            BulkReconciliationDelta {
                cursor_id,
                generation: self.generation,
                mode: SourceMode::Activate,
                activation_end: self.activation_end,
                fingerprints: BTreeMap::new(),
                deferred,
            },
            self.path,
        ))
    }
}

#[derive(Debug)]
struct ResolvedRow {
    /// Index of the source whose opener won.
    origin: usize,
    row: TimelineEntry,
    activity: Option<(i64, i64)>,
}

/// One session's tool rows, resolved from the withheld facts of all its
/// sources, in the order the admission fold would write them.
#[derive(Debug)]
pub struct ResolvedSession {
    /// Each origin source's path, by its index.
    origins: Vec<String>,
    rows: Vec<ResolvedRow>,
    /// Per origin source, what its facts left for the runtime fold: usage
    /// the session's rows did not absorb, and file edits.
    tails: Vec<WithheldFacts>,
    session_touches: BTreeMap<String, i64>,
    native_updates: u64,
    matched_updates: u64,
    unmatched_updates: u64,
    folded_usage: u64,
}

impl ResolvedSession {
    /// Fold the withheld facts of every activated source of one session.
    ///
    /// The caller must have proven that no other source contributes a fact
    /// these sources' facts can meet; that proof, not this function, is what
    /// makes the result equal to the runtime fold's for the session.
    pub fn resolve(sources: Vec<WithheldFacts>) -> Result<Self, StoreError> {
        let mut origins: HashMap<String, usize> = HashMap::with_capacity(sources.len());
        let mut tails = Vec::with_capacity(sources.len());
        let mut state = FoldState::default();
        for (index, source) in sources.into_iter().enumerate() {
            let WithheldFacts {
                path,
                generation,
                activation_end,
                deferred,
                ..
            } = source;
            if origins.insert(path.clone(), index).is_some() {
                return Err(StoreError::BulkLoadState {
                    message: format!("source {path} was withheld twice for one session"),
                });
            }
            state.collect_source(
                SourceKey {
                    cursor_id: i64::try_from(index).unwrap_or(i64::MAX),
                    generation,
                },
                activation_end,
                &path,
                deferred
                    .into_iter()
                    .map(|(position, op)| (position, split_owned(deferred_op(op)))),
            )?;
            tails.push(WithheldFacts {
                path,
                generation,
                activation_end,
                cursor_id: None,
                deferred: Vec::new(),
            });
        }
        let (plan, _) = state.finish(Vec::new(), None)?;
        let BulkReconciliationPlan {
            messages,
            existing,
            tail,
            native_updates,
            matched_updates,
            unmatched_updates,
            session_touches,
            tool_activity,
            folded_usage,
            ..
        } = plan;
        debug_assert!(
            existing.is_empty() && tool_activity.is_empty(),
            "a consumed fold resolves only its own openers"
        );
        let rows = messages
            .into_iter()
            .map(|message| ResolvedRow {
                origin: origins[&message.order.path],
                row: message.row,
                activity: message.activity,
            })
            .collect();
        // The tail is in source order; each fact returns to its own source.
        for buffered in tail {
            tails[origins[&buffered.order.path]]
                .deferred
                .push((buffered.order.sequence, buffered.op));
        }
        let origins = tails.iter().map(|tail| tail.path.clone()).collect();
        tails.retain(|tail| !tail.deferred.is_empty());
        Ok(Self {
            origins,
            rows,
            tails,
            session_touches,
            native_updates,
            matched_updates,
            unmatched_updates,
            folded_usage,
        })
    }

    /// Resolved tool rows the writer inserts.
    pub fn tool_rows(&self) -> u64 {
        u64::try_from(self.rows.len()).unwrap_or(u64::MAX)
    }

    /// Bind every origin source's canonical cursor by its path: stamped on
    /// the rows its openers won, and on what it left for the runtime fold.
    pub fn bind_cursors(
        &mut self,
        mut cursor_of: impl FnMut(&str) -> Option<i64>,
    ) -> Result<(), StoreError> {
        let cursors = self
            .origins
            .iter()
            .map(|path| {
                cursor_of(path).ok_or_else(|| StoreError::BulkLoadState {
                    message: format!("resolved source {path} has no cursor"),
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        for resolved in &mut self.rows {
            resolved.row.provenance.src_cursor = Some(cursors[resolved.origin]);
        }
        for tail in &mut self.tails {
            let origin = self
                .origins
                .iter()
                .position(|path| *path == tail.path)
                .expect("every tail belongs to an origin");
            tail.bind_cursor(cursors[origin]);
        }
        Ok(())
    }

    /// Write the session's resolved rows, in the admission fold's order, and
    /// its timestamp touches: [`super::apply_plan`]'s writes for the part of
    /// the runtime fold this session decided. Rows carry their final
    /// ordinals and bound cursors.
    pub(crate) fn write(&self, tx: &Transaction) -> Result<ApplyStats, StoreError> {
        let mut stats = ApplyStats {
            session_touches: Some(BTreeMap::new()),
            ..ApplyStats::default()
        };
        stats.profile.tool_result_attempts = self.native_updates;
        stats.profile.usage_attempts = self.folded_usage;
        stats.tool_results_merged = self.matched_updates;
        stats.tool_results_unmatched = self.unmatched_updates;
        stats.usage_applied = self.folded_usage;
        stats.profile.bulk_tool_rows_reconciled = self.tool_rows();
        let mut dirty_buckets = BTreeSet::<BucketKey>::new();
        for resolved in &self.rows {
            if resolved.row.provenance.src_cursor.is_none() {
                return Err(StoreError::BulkLoadState {
                    message: format!(
                        "resolved tool row {} was never bound to its source cursor",
                        resolved.row.timeline_id
                    ),
                });
            }
            apply_reconciled_tool_message(
                tx,
                &resolved.row,
                true,
                resolved.activity,
                &mut stats,
                BULK_MAINTENANCE,
                &mut dirty_buckets,
            )?;
        }
        for (session_id, at) in &self.session_touches {
            touch_session(tx, session_id, Some(*at), &mut stats)?;
        }
        flush_session_touches(tx, &mut stats)?;
        debug_assert!(dirty_buckets.is_empty(), "cold maintenance defers buckets");
        Ok(stats)
    }

    /// What the session's facts left for the runtime fold, per source.
    pub(crate) fn into_tails(self) -> Vec<WithheldFacts> {
        crate::reclaim::release(self.rows);
        self.tails
    }
}
