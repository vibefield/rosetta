//! Tool lifecycles resolved at file close.
//!
//! A streamed fresh-cold runtime folds its relational facts at admission,
//! because in general a correlation's opener, lifecycle facts and response
//! usage may come from several of the runtime's sources. When the producer has
//! proven at runtime that one closed source holds every such fact of its
//! session — the roster gives the session no other source, and no other source
//! addresses it — the runtime fold restricted to that session is exactly this
//! source's own fold. [`ResolvedSourceTools::resolve`] runs it where the source
//! is emitted, off the canonical writer: the writer then inserts the final tool
//! rows inside the source's own transaction, and admission folds only what the
//! source left behind (usage its tool rows did not absorb, and file edits),
//! at the source positions the whole-runtime fold would have used.

use std::collections::BTreeMap;

use rosetta_core::IngestOp;

use crate::StoreError;
use crate::apply::ApplyStats;

use super::fold::{FoldState, split_owned};
use super::{BulkReconciliationPlan, SourceKey, deferred_op, defers};

/// One closed source's tool rows, resolved in RAM before the writer sees it.
/// Process-local, like every fold: the raw ledger remains the recovery source.
#[derive(Debug)]
pub struct ResolvedSourceTools {
    /// The source position of each operation left in the source, in order.
    positions: Vec<u64>,
    /// The source's operation count: its activation's end-exclusive sequence.
    activation_end: u64,
    /// Result-timestamp bounds per resolved tool row, by source position.
    activity: BTreeMap<u64, (i64, i64)>,
    session_touches: BTreeMap<String, i64>,
    tool_rows: u64,
    native_updates: u64,
    matched_updates: u64,
    unmatched_updates: u64,
    folded_usage: u64,
}

impl ResolvedSourceTools {
    /// Fold one activated closed source of `generation` at `path` on its
    /// own. Returns the source's operations with every tool opener replaced
    /// by its resolved row (losing duplicate openers, lifecycle updates, and
    /// usage absorbed into a resolved row are consumed), in source order.
    ///
    /// The caller must have proven that no other source contributes a fact
    /// to the sessions this source's facts address; that proof, not this
    /// function, is what makes the result equal to the runtime fold's.
    pub fn resolve(
        path: &str,
        generation: u64,
        ops: Vec<IngestOp>,
    ) -> Result<(Vec<IngestOp>, Self), StoreError> {
        let activation_end =
            u64::try_from(ops.len()).map_err(|_| StoreError::ProjectionInvalid {
                message: format!("closed source {path} has more operations than u64 can count"),
            })?;
        let mut kept = Vec::with_capacity(ops.len());
        let mut kept_positions = Vec::with_capacity(ops.len());
        let mut deferred = Vec::new();
        for (position, op) in (0_u64..).zip(ops) {
            if defers(&op) {
                deferred.push((position, split_owned(deferred_op(op))));
            } else {
                kept.push(op);
                kept_positions.push(position);
            }
        }
        let mut state = FoldState::default();
        state.collect_source(
            SourceKey {
                cursor_id: 0,
                generation,
            },
            activation_end,
            path,
            deferred,
        )?;
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

        // Put the resolved rows and the unabsorbed tail back at their source
        // positions. All three inputs are already in position order.
        let tool_rows = u64::try_from(messages.len()).unwrap_or(u64::MAX);
        let mut returned = Vec::with_capacity(messages.len() + tail.len());
        let mut activity = BTreeMap::new();
        let mut messages = messages.into_iter().peekable();
        let mut tail = tail.into_iter().peekable();
        loop {
            let next = match (messages.peek(), tail.peek()) {
                (Some(message), Some(buffered)) => message.order.sequence < buffered.order.sequence,
                (Some(_), None) => true,
                (None, Some(_)) => false,
                (None, None) => break,
            };
            if next {
                let message = messages.next().expect("peeked");
                if let Some(bounds) = message.activity {
                    activity.insert(message.order.sequence, bounds);
                }
                returned.push((
                    message.order.sequence,
                    IngestOp::Message(Box::new(message.row)),
                ));
            } else {
                let buffered = tail.next().expect("peeked");
                returned.push((buffered.order.sequence, buffered.op));
            }
        }
        let mut ops = Vec::with_capacity(kept.len() + returned.len());
        let mut positions = Vec::with_capacity(kept.len() + returned.len());
        let mut returned = returned.into_iter().peekable();
        for (op, position) in kept.into_iter().zip(kept_positions) {
            while let Some((at, _)) = returned.peek()
                && *at < position
            {
                let (at, op) = returned.next().expect("peeked");
                ops.push(op);
                positions.push(at);
            }
            ops.push(op);
            positions.push(position);
        }
        for (at, op) in returned {
            ops.push(op);
            positions.push(at);
        }

        Ok((
            ops,
            Self {
                positions,
                activation_end,
                activity,
                session_touches,
                tool_rows,
                native_updates,
                matched_updates,
                unmatched_updates,
                folded_usage,
            },
        ))
    }

    /// The source position of each operation left in the source, in order:
    /// what a writer must rank and key them by instead of their index.
    pub fn positions(&self) -> &[u64] {
        &self.positions
    }

    /// Resolved tool rows the writer inserts with the source.
    pub fn tool_rows(&self) -> u64 {
        self.tool_rows
    }

    pub(super) fn activation_end(&self) -> u64 {
        self.activation_end
    }

    pub(super) fn activity(&self, position: u64) -> Option<(i64, i64)> {
        self.activity.get(&position).copied()
    }

    pub(super) fn session_touches(&self) -> &BTreeMap<String, i64> {
        &self.session_touches
    }

    /// The runtime fold's accounting for this source's share of the plan.
    pub(super) fn stats(&self) -> ApplyStats {
        let mut stats = ApplyStats {
            session_touches: Some(BTreeMap::new()),
            ..ApplyStats::default()
        };
        stats.profile.tool_result_attempts = self.native_updates;
        stats.profile.usage_attempts = self.folded_usage;
        stats.tool_results_merged = self.matched_updates;
        stats.tool_results_unmatched = self.unmatched_updates;
        stats.usage_applied = self.folded_usage;
        stats.profile.bulk_tool_rows_reconciled = self.tool_rows;
        stats
    }
}
