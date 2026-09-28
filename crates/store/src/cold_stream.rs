//! Streamed fresh-cold apply. A fresh `serve` writes each runtime's closed
//! sources into the one shared canonical RAM projection while other runtimes
//! are still capturing, so a single private cold store now interleaves several
//! runtimes' writes. Everything here keeps those runtimes apart until each
//! one's admission: a runtime's deferred relational facts are collected into
//! its own RAM fold as its sources commit, folded and written only at its
//! admission, and the session stream-state flip stays the only visibility
//! boundary.

use std::collections::BTreeSet;
use std::time::Instant;

use rusqlite::params;

use crate::apply::{
    self, ApplyStats, Batch, BulkReconciliationDelta, ResolvedSourceTools, SourceMode,
};
use crate::{Store, StoreError, StoreMode, elapsed_nanos, validate_source_batch};

/// One runtime's deferred cold facts — tool openers, lifecycle updates,
/// response usage, and file edits — collected from its closed sources as they
/// commit into a shared private projection. Process-local like the store's own
/// buffer: the raw ledger, never SQLite, is the recovery source.
#[derive(Debug, Default)]
pub struct ColdReconciliation {
    fold: apply::ColdFold,
}

/// One closed source for [`Store::apply_streamed_sources`]: its batch and,
/// when its producer proved the source holds every tool fact of its session,
/// that session's tool lifecycles already resolved
/// ([`ResolvedSourceTools::resolve`]). The batch's operations are then the
/// resolved source's, keyed by [`ResolvedSourceTools::positions`].
#[derive(Debug)]
pub struct StreamedSource {
    pub batch: Batch,
    pub resolved: Option<ResolvedSourceTools>,
}

impl From<Batch> for StreamedSource {
    fn from(batch: Batch) -> Self {
        Self {
            batch,
            resolved: None,
        }
    }
}

/// Each applied closed source's deferred facts, with its cursor path.
type ClosedDeltas = Vec<(BulkReconciliationDelta, Option<String>)>;

impl Store {
    /// The SQL half of applying closed source batches: every source's final
    /// immediate rows, checkpoint, and generation — and the tool rows of a
    /// source resolved at file close — in one private transaction. Returns
    /// each source's deferred relational facts with its path for the caller's
    /// reconciliation; `validate_retries` checks them against the store's own
    /// buffer first.
    pub(crate) fn apply_closed_source_sql(
        &mut self,
        sources: Vec<StreamedSource>,
        validate_retries: bool,
    ) -> Result<(ApplyStats, ClosedDeltas), StoreError> {
        if self.mode != StoreMode::Bulk {
            return Err(StoreError::BulkLoadState {
                message: "closed source batches require a private cold rebuild".to_string(),
            });
        }

        let mut cursor_ids = BTreeSet::new();
        let mut deltas = Vec::with_capacity(sources.len());
        let mut asset_totals = Vec::with_capacity(sources.len());
        let mut abandoned = false;
        for StreamedSource { batch, resolved } in &sources {
            let source = batch
                .source
                .as_ref()
                .ok_or_else(|| StoreError::BulkLoadState {
                    message: "closed bulk source batch is missing source identity".to_string(),
                })?;
            if !matches!(source.mode, SourceMode::Activate | SourceMode::Abandon) {
                return Err(StoreError::BulkLoadState {
                    message: "closed bulk source batch is not terminal".to_string(),
                });
            }
            if !cursor_ids.insert(source.cursor_id) {
                return Err(StoreError::BulkLoadState {
                    message: format!(
                        "closed bulk transaction repeats cursor {}",
                        source.cursor_id
                    ),
                });
            }
            if let Some(resolved) = resolved {
                // Resolved rows carry final ordinals and are the only write
                // of their tool lifecycles, so they need an activation and a
                // store that trusts precomputed ordinals.
                if source.mode != SourceMode::Activate
                    || !self.bulk_ordinals_precomputed
                    || resolved.positions().len() != batch.ops.len()
                    || validate_retries
                {
                    return Err(StoreError::BulkLoadState {
                        message: format!(
                            "cursor {} carries tool rows resolved at file close outside a \
                             streamed activation with precomputed ordinals",
                            source.cursor_id
                        ),
                    });
                }
            }
            abandoned |= source.mode == SourceMode::Abandon;
            validate_source_batch(batch)?;
            asset_totals.push(self.assets.materialize_batch(batch, false)?);
        }

        let mut totals = ApplyStats::default();
        let tx = self.conn.transaction()?;
        for (StreamedSource { batch, resolved }, asset_stats) in
            sources.into_iter().zip(asset_totals)
        {
            let mut stats = apply::apply_batch(
                &tx,
                &batch,
                apply::BULK_MAINTENANCE,
                true,
                false,
                self.bulk_ordinals_precomputed,
                &mut self.usage_owners,
                apply::Observation {
                    rows: None,
                    meta: None,
                },
            )?;
            stats.projection_assets = asset_stats.assets;
            stats.asset_files_written = asset_stats.files_written;
            stats.asset_files_reused = asset_stats.files_reused;
            stats.asset_bytes = asset_stats.bytes;
            stats.asset_write_nanos = asset_stats.nanos;
            totals.merge_from(&stats);
            if let Some(resolved) = &resolved {
                let started = Instant::now();
                let mut resolved_stats = apply::apply_resolved_tools(&tx, &batch.ops, resolved)?;
                resolved_stats.canonical_apply_nanos = elapsed_nanos(started);
                totals.merge_from(&resolved_stats);
            }
            let path = batch
                .checkpoints
                .first()
                .map(|checkpoint| checkpoint.path.clone());
            // SQL has consumed the immediate rows. Move deferred facts into
            // the provisional RAM delta and release the rest of this batch
            // now, instead of retaining a second runtime-sized payload tree.
            let delta = match &resolved {
                Some(resolved) => BulkReconciliationDelta::from_resolved_batch(batch, resolved)?,
                None => BulkReconciliationDelta::from_owned_batch(batch)?,
            }
            .ok_or_else(|| StoreError::BulkLoadState {
                message: "closed source batch did not produce reconciliation state".to_string(),
            })?;
            if validate_retries {
                self.bulk_reconciliation.validate(&delta)?;
            }
            deltas.push((delta, path));
        }
        let commit_started = Instant::now();
        tx.commit()?;
        totals.sqlite_commit_nanos = totals
            .sqlite_commit_nanos
            .saturating_add(elapsed_nanos(commit_started));
        if abandoned {
            self.bulk_abandoned = true;
        }
        Ok((totals, deltas))
    }

    /// [`Store::apply_closed_bulk_sources`] for one streamed runtime: the same
    /// transaction, with each source's deferred facts collected into
    /// `state` — that runtime's own fold — the moment it commits, so one
    /// runtime's admission can never fold another runtime's half-captured
    /// lifecycle facts, and the fold's gathering runs while capture continues.
    pub fn apply_streamed_sources(
        &mut self,
        state: &mut ColdReconciliation,
        sources: Vec<StreamedSource>,
    ) -> Result<ApplyStats, StoreError> {
        let (mut totals, deltas) = self.apply_closed_source_sql(sources, false)?;
        let collect_started = Instant::now();
        for (delta, path) in deltas {
            let path = path.ok_or_else(|| StoreError::BulkLoadState {
                message: "streamed source batch carries no cursor checkpoint".to_string(),
            })?;
            state.fold.collect(delta, &path)?;
        }
        totals.profile.tool_reconcile_nanos = elapsed_nanos(collect_started);
        Ok(totals)
    }

    /// Fold one streamed runtime's collected relational facts at its
    /// admission and write each resolved tool row once — the streamed
    /// counterpart of [`Store::flush_bulk_reconciliation`], for a runtime
    /// whose tool rows only this fold writes. Nothing is copied and no stored
    /// opener is scanned: none can match. That assumption is checked, not
    /// trusted — a session the plan addresses that already holds a tool row
    /// fails the admission closed.
    pub fn fold_cold_reconciliation(
        &mut self,
        state: ColdReconciliation,
    ) -> Result<ApplyStats, StoreError> {
        if self.mode != StoreMode::Bulk {
            return Err(StoreError::BulkLoadState {
                message: "a streamed cold fold requires a private cold rebuild".to_string(),
            });
        }
        let reconcile_started = Instant::now();
        let (mut plan, addressed) = state.fold.finish()?;
        let reconcile_nanos = elapsed_nanos(reconcile_started);
        if plan.is_empty() {
            return Ok(ApplyStats::default());
        }
        {
            let mut stored = self.conn.prepare_cached(
                "SELECT EXISTS(
                   SELECT 1 FROM message WHERE session_id = ?1 AND type = 'tool_use'
                 )",
            )?;
            for session_id in &addressed {
                if stored.query_row(params![session_id], |row| row.get::<_, bool>(0))? {
                    return Err(StoreError::ProjectionInvalid {
                        message: format!(
                            "streamed cold fold found a stored tool row in session {session_id}"
                        ),
                    });
                }
            }
        }

        let tx = self.conn.transaction()?;
        let apply_started = Instant::now();
        let mut stats = apply::apply_bulk_reconciliation(
            &tx,
            &mut plan,
            self.bulk_ordinals_precomputed,
            &mut self.usage_owners,
        )?;
        stats.profile.tool_reconcile_nanos = reconcile_nanos;
        stats.canonical_apply_nanos = elapsed_nanos(apply_started);
        let commit_started = Instant::now();
        tx.commit()?;
        stats.sqlite_commit_nanos = elapsed_nanos(commit_started);
        let cleanup_started = Instant::now();
        drop(plan);
        stats.profile.tool_reconcile_cleanup_nanos = elapsed_nanos(cleanup_started);
        Ok(stats)
    }

    /// Apply a source-free batch without noting metadata effects. A streamed
    /// runtime's setup (cursor rows and session stubs for sources that no
    /// metadata batch named) must not reach the metadata mirror before its
    /// admission; the admission's stream-state flip notes those sessions.
    pub fn apply_unobserved(&mut self, batch: &Batch) -> Result<ApplyStats, StoreError> {
        let observed = self.meta_effects.take();
        let result = self.apply(batch);
        self.meta_effects = observed;
        result
    }

    /// Take the batches a cold-capture planner retained since the last take,
    /// leaving it a planner. A streaming producer drains after every capture
    /// unit and releases each closed source as soon as it is emitted.
    pub fn drain_planned_batches(&mut self) -> Result<Vec<Batch>, StoreError> {
        self.planned_batches
            .as_mut()
            .map(std::mem::take)
            .ok_or_else(|| StoreError::BulkLoadState {
                message: "store is not a cold-capture planner".to_string(),
            })
    }

    /// Every cursor one runtime owns, as `(id, path)`.
    pub fn runtime_cursor_ids(&self, runtime_id: &str) -> Result<Vec<(i64, String)>, StoreError> {
        let mut statement = self
            .conn
            .prepare_cached("SELECT id, path FROM ingest_cursor WHERE runtime_id = ?1")?;
        let cursors = statement
            .query_map(params![runtime_id], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(cursors)
    }

    /// Every session one runtime owns. Cheaper than a session list: no
    /// activity is computed.
    pub fn runtime_session_ids(&self, runtime_id: &str) -> Result<Vec<String>, StoreError> {
        let mut statement = self
            .conn
            .prepare_cached("SELECT id FROM session WHERE runtime_id = ?1 ORDER BY id")?;
        let ids = statement
            .query_map(params![runtime_id], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ids)
    }

    /// Every session one runtime owns, with the project it settled into —
    /// the cold progress board's ownership, without a session list's
    /// activity queries.
    pub fn runtime_session_owners(
        &self,
        runtime_id: &str,
    ) -> Result<Vec<(String, Option<String>)>, StoreError> {
        let mut statement = self.conn.prepare_cached(
            "SELECT id, project_id FROM session WHERE runtime_id = ?1 ORDER BY id",
        )?;
        let owners = statement
            .query_map(params![runtime_id], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(owners)
    }

    /// Canonicalize the named sessions' presentation ordinals from the
    /// ordinals already stored: source path first, then the stored ordinal.
    ///
    /// A streamed cold runtime assigns each closed source's messages final
    /// file-local order as it lands (source position, sequence, timeline id —
    /// exactly the cold precompute's key), above everything earlier sources of
    /// the session took. That is already canonical unless a later source
    /// repeated a timeline identity (it takes its final position there and
    /// leaves a gap) or a source arrived out of path order. Ranking by path and
    /// stored ordinal closes both cases without any per-row key in SQLite.
    /// Moves through negative space, so `UNIQUE(session_id, ordinal)` never
    /// sees a transient collision; only rows whose ordinal changes are
    /// rewritten.
    pub fn renumber_sessions_by_source_order(
        &mut self,
        session_ids: &[String],
    ) -> Result<u64, StoreError> {
        if session_ids.is_empty() {
            return Ok(0);
        }
        let tx = self.conn.transaction()?;
        apply::prepare_renumber_table(&tx)?;
        let mut rows = 0_u64;
        {
            let mut rank = tx.prepare_cached(
                "INSERT INTO renumber_message (message_id, ordinal)
                 SELECT message_id, ranked_ordinal
                 FROM (
                   SELECT m.id AS message_id, m.ordinal AS current_ordinal,
                          ROW_NUMBER() OVER (
                            ORDER BY COALESCE(c.path, ''), m.ordinal
                          ) - 1 AS ranked_ordinal
                   FROM message m LEFT JOIN ingest_cursor c ON c.id = m.src_cursor
                   WHERE m.session_id = ?1
                 )
                 WHERE current_ordinal != ranked_ordinal",
            )?;
            for session_id in session_ids {
                rows += rank.execute(params![session_id])? as u64;
            }
        }
        apply::apply_renumber_table(&tx)?;
        tx.commit()?;
        Ok(rows)
    }
}
