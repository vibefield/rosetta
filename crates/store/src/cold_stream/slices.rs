//! The bounded transactions a chunked closed source is applied in before its
//! terminal: Stage slices of its immediate ops, and resolved-row slices of
//! the sessions it completes. Neither changes a cursor's checkpoint, its
//! active generation or any `stream_state`, so every row they write stays
//! behind its session's Pending barrier until the session's tree flips.

use std::time::Instant;

use crate::apply::{ApplyStats, ResolvedSession};
use crate::{CommitKind, Store, StoreError, StoreMode, elapsed_nanos, lock_order};

use super::{ClosedTx, ColdReconciliation, StreamedSource};

impl Store {
    /// Apply one Stage slice of a chunked closed source in its own writer
    /// transaction: a direct-cold projection of the slice's immediate ops.
    /// Only the cursor's `highest_generation` moves; its checkpoint, active
    /// generation and stream state wait for the terminal slice. A slice's
    /// deferred relational facts are staged in `state` under its cursor and
    /// merged into the source's one terminal delta, never into the fold; a
    /// withheld slice carries none.
    pub fn apply_streamed_slice(
        &mut self,
        state: &mut ColdReconciliation,
        slice: StreamedSource,
    ) -> Result<ApplyStats, StoreError> {
        if let Some(source) = slice.batch.source.as_ref() {
            state.check_stage(source.cursor_id, source.generation)?;
        }
        let (totals, deltas) = self.apply_closed_source_sql(vec![slice], false, ClosedTx::Stage)?;
        for (delta, _) in deltas {
            state.stage(delta)?;
        }
        Ok(totals)
    }

    /// Write the next resolved rows of `session`, up to `max_bytes` and at
    /// least one, in their own writer transaction. Returns whether rows are
    /// left; the source's terminal transaction writes the rest, with the
    /// session's touches.
    pub fn apply_resolved_slice(
        &mut self,
        session: &mut ResolvedSession,
        max_bytes: u64,
    ) -> Result<(ApplyStats, bool), StoreError> {
        if self.mode != StoreMode::Bulk || !self.bulk_ordinals_precomputed {
            return Err(StoreError::BulkLoadState {
                message: "resolved-row slices require a streamed private cold rebuild".into(),
            });
        }
        let sliced = (|| {
            let (tx, lock_wait) = lock_order::writer_tx(
                &mut self.conn,
                self.shared_readers.as_deref(),
                "resolved-row slice",
            )?;
            let started = Instant::now();
            let (mut stats, left) = session.write_slice(&tx, max_bytes)?;
            stats.canonical_apply_nanos = elapsed_nanos(started);
            let commit_started = Instant::now();
            tx.commit()?;
            stats.sqlite_commit_nanos = elapsed_nanos(commit_started);
            stats.profile.session_lock_wait_nanos = lock_wait;
            Ok((stats, left))
        })()
        .map_err(self.lock_order("resolved-row slice"));
        if sliced.is_ok() {
            self.committed(CommitKind::ResolvedSlice);
        }
        sliced
    }
}
