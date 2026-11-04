//! Streamed fresh-cold apply. A fresh `serve` writes each runtime's closed
//! sources into the one shared canonical RAM projection while other runtimes
//! are still capturing, so a single private cold store now interleaves several
//! runtimes' writes. Everything here keeps those runtimes apart until each
//! one's admission: a runtime's deferred relational facts are collected into
//! its own RAM fold as its sources commit, folded and written only at its
//! admission, and the session stream-state flip stays the only visibility
//! boundary.

use std::collections::{BTreeSet, HashMap};
use std::time::Instant;

use rusqlite::params;

use crate::apply::{
    self, ApplyStats, Batch, BulkReconciliationDelta, ResolvedSession, SourceMode, WithheldFacts,
};
use crate::assets::AssetWriteStats;
use crate::{
    CommitKind, Store, StoreError, StoreMode, elapsed_nanos, lock_order, validate_source_batch,
};

mod slices;
mod units;

pub(crate) use units::set_stream_states_in;
pub use units::{ColdAdmission, ColdReconciliation};

/// One closed source for [`Store::apply_streamed_sources`], as the producer
/// prepared it: its batch, and what the producer resolved or gave back with
/// it. Everything but the batch is empty for a source the producer passed
/// through unchanged.
#[derive(Debug)]
pub struct StreamedSource {
    pub batch: Batch,
    /// The producer took this source's relational facts out of the batch
    /// ([`WithheldFacts::withhold`]); they reach the runtime's fold later,
    /// resolved or released, never with the batch.
    pub withheld: bool,
    /// Sessions this source completed, resolved from their withheld facts:
    /// whatever of their tool rows no earlier resolved-row slice wrote is
    /// written in this source's transaction.
    pub resolved: Vec<ResolvedSession>,
    /// Withheld facts of sessions the producer could not prove
    /// self-contained, returned unresolved to the runtime's fold.
    pub released: Vec<WithheldFacts>,
}

impl From<Batch> for StreamedSource {
    fn from(batch: Batch) -> Self {
        Self {
            batch,
            withheld: false,
            resolved: Vec::new(),
            released: Vec::new(),
        }
    }
}

impl StreamedSource {
    fn prepared(&self) -> bool {
        self.withheld || !self.resolved.is_empty() || !self.released.is_empty()
    }
}

/// Which transaction of a closed source [`Store::apply_closed_source_sql`]
/// runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClosedTx {
    /// The source's last transaction: its terminal mode, checkpoint and
    /// whatever of its resolved rows is left (every source at HEAD).
    Terminal,
    /// One Stage slice of a chunked source: some of its immediate ops,
    /// before its terminal; no checkpoint, no stream state.
    Stage,
}

/// Each applied closed source's deferred facts, with its cursor path.
type ClosedDeltas = Vec<(BulkReconciliationDelta, Option<String>)>;

impl Store {
    /// The SQL half of applying closed source batches: every source's final
    /// immediate rows, checkpoint, and generation — and the tool rows of the
    /// sessions a source completed, resolved from what the producer withheld
    /// — in one private transaction. Returns each source's deferred
    /// relational facts, and any withheld facts it carries back, with their
    /// paths for the caller's reconciliation; `validate_retries` checks them
    /// against the store's own buffer first. A [`ClosedTx::Stage`] call
    /// takes exactly one Stage slice instead.
    pub(crate) fn apply_closed_source_sql(
        &mut self,
        sources: Vec<StreamedSource>,
        validate_retries: bool,
        mode: ClosedTx,
    ) -> Result<(ApplyStats, ClosedDeltas), StoreError> {
        let site = match mode {
            ClosedTx::Terminal => "closed sources",
            ClosedTx::Stage => "stage slice",
        };
        self.apply_closed_source_sql_in_tx(sources, validate_retries, mode)
            .map_err(self.lock_order(site))
    }

    fn apply_closed_source_sql_in_tx(
        &mut self,
        sources: Vec<StreamedSource>,
        validate_retries: bool,
        mode: ClosedTx,
    ) -> Result<(ApplyStats, ClosedDeltas), StoreError> {
        if self.mode != StoreMode::Bulk {
            return Err(StoreError::BulkLoadState {
                message: "closed source batches require a private cold rebuild".to_string(),
            });
        }
        if mode == ClosedTx::Stage {
            let [streamed] = sources.as_slice() else {
                return Err(StoreError::BulkLoadState {
                    message: "a stage slice transaction takes exactly one source".to_string(),
                });
            };
            let staged = streamed.batch.source.as_ref().map(|source| source.mode);
            if staged != Some(SourceMode::Stage)
                || !streamed.batch.checkpoints.is_empty()
                || !streamed.resolved.is_empty()
                || !streamed.released.is_empty()
                || validate_retries
            {
                return Err(StoreError::BulkLoadState {
                    message: "a stage slice must be one Stage source batch without checkpoints \
                              or resolved facts"
                        .to_string(),
                });
            }
        }

        let mut cursor_ids = BTreeSet::new();
        let mut deltas = Vec::with_capacity(sources.len());
        let mut asset_totals = Vec::with_capacity(sources.len());
        let mut abandoned = false;
        for streamed in &sources {
            let batch = &streamed.batch;
            let Some(source) = batch.source.as_ref() else {
                // A roster source the build skipped — nothing of it could be
                // read — writes nothing of its own; it only carries the facts
                // of a session it completed.
                if !batch.ops.is_empty() || !batch.checkpoints.is_empty() || streamed.withheld {
                    return Err(StoreError::BulkLoadState {
                        message: "closed bulk source batch is missing source identity".to_string(),
                    });
                }
                if streamed.prepared() && (validate_retries || !self.bulk_ordinals_precomputed) {
                    return Err(StoreError::BulkLoadState {
                        message: "a skipped source carries facts prepared for a streamed runtime \
                                  outside one"
                            .to_string(),
                    });
                }
                asset_totals.push(AssetWriteStats::default());
                continue;
            };
            let terminal = matches!(source.mode, SourceMode::Activate | SourceMode::Abandon);
            if mode == ClosedTx::Terminal && !terminal {
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
            // Resolved rows carry final ordinals and are the only write of
            // their tool lifecycles: only a streamed runtime, whose fold
            // nothing else feeds and whose ordinals are precomputed, may
            // carry them.
            if streamed.prepared() && (validate_retries || !self.bulk_ordinals_precomputed) {
                return Err(StoreError::BulkLoadState {
                    message: format!(
                        "cursor {} carries facts prepared for a streamed runtime outside one",
                        source.cursor_id
                    ),
                });
            }
            abandoned |= source.mode == SourceMode::Abandon;
            validate_source_batch(batch)?;
            asset_totals.push(self.assets.materialize_batch(batch, false)?);
        }

        let mut totals = ApplyStats::default();
        let site = match mode {
            ClosedTx::Terminal => "closed sources",
            ClosedTx::Stage => "stage slice",
        };
        let (tx, session_lock_wait) =
            lock_order::writer_tx(&mut self.conn, self.shared_readers.as_deref(), site)?;
        let held_from = Instant::now();
        totals.profile.session_lock_wait_nanos = session_lock_wait;
        // Back in the Store only once this transaction commits.
        let mut drift = std::mem::take(&mut self.drift);
        for (streamed, asset_stats) in sources.into_iter().zip(asset_totals) {
            let StreamedSource {
                batch,
                withheld,
                resolved,
                released,
            } = streamed;
            let skipped = batch.source.is_none();
            if !skipped {
                let mut stats = apply::apply_batch(
                    &tx,
                    &batch,
                    apply::BULK_MAINTENANCE,
                    true,
                    false,
                    self.bulk_ordinals_precomputed,
                    &mut self.usage_owners,
                    &mut drift,
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
            }
            for mut session in resolved {
                let started = Instant::now();
                let mut written = session.write(&tx)?;
                written.canonical_apply_nanos = elapsed_nanos(started);
                totals.merge_from(&written);
                for tail in session.into_tails() {
                    let (delta, path) = tail.into_delta()?;
                    deltas.push((delta, Some(path)));
                }
            }
            for facts in released {
                let (delta, path) = facts.into_delta()?;
                deltas.push((delta, Some(path)));
            }
            if skipped {
                continue;
            }
            if withheld {
                // The producer took every relational fact out of this batch;
                // what is left SQL has consumed.
                if batch.ops.iter().any(apply::defers_relational) {
                    return Err(StoreError::BulkLoadState {
                        message: "a withheld closed source still carries relational facts"
                            .to_string(),
                    });
                }
                crate::reclaim::release(batch);
                continue;
            }
            let path = batch
                .checkpoints
                .first()
                .map(|checkpoint| checkpoint.path.clone());
            // SQL has consumed the immediate rows. Move deferred facts into
            // the provisional RAM delta and release the rest of this batch
            // now, instead of retaining a second runtime-sized payload tree.
            let delta = BulkReconciliationDelta::from_owned_batch(batch, !validate_retries)?
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
        totals.profile.session_lock_held_nanos = elapsed_nanos(held_from);
        self.drift = drift;
        self.committed(match mode {
            ClosedTx::Terminal => CommitKind::Sources,
            ClosedTx::Stage => CommitKind::StageSlice,
        });
        if mode == ClosedTx::Terminal {
            for cursor_id in cursor_ids {
                self.retired_sources.remove(&cursor_id);
            }
        }
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
    /// A source applied in slices first merges what its Stage slices staged.
    pub fn apply_streamed_sources(
        &mut self,
        state: &mut ColdReconciliation,
        sources: Vec<StreamedSource>,
    ) -> Result<ApplyStats, StoreError> {
        let (mut totals, deltas) =
            self.apply_closed_source_sql(sources, false, ClosedTx::Terminal)?;
        let collect_started = Instant::now();
        for (delta, path) in deltas {
            let path = path.ok_or_else(|| StoreError::BulkLoadState {
                message: "streamed source batch carries no cursor checkpoint".to_string(),
            })?;
            state.collect(delta, &path)?;
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
        self.fold_cold_reconciliation_in_tx(state)
            .map_err(self.lock_order("cold fold"))
    }

    fn fold_cold_reconciliation_in_tx(
        &mut self,
        state: ColdReconciliation,
    ) -> Result<ApplyStats, StoreError> {
        if self.mode != StoreMode::Bulk {
            return Err(StoreError::BulkLoadState {
                message: "a streamed cold fold requires a private cold rebuild".to_string(),
            });
        }
        if state.staged_slices() != 0 {
            return Err(StoreError::BulkLoadState {
                message: "a streamed cold fold found a source between its slices".into(),
            });
        }
        let reconcile_started = Instant::now();
        let (mut plan, addressed) = state.into_fold()?.finish()?;
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

        let (tx, session_lock_wait) =
            lock_order::writer_tx(&mut self.conn, self.shared_readers.as_deref(), "cold fold")?;
        let apply_started = Instant::now();
        let mut stats = apply::apply_bulk_reconciliation(
            &tx,
            &mut plan,
            self.bulk_ordinals_precomputed,
            &mut self.usage_owners,
        )?;
        stats.profile.tool_reconcile_nanos = reconcile_nanos;
        stats.profile.session_lock_wait_nanos = session_lock_wait;
        stats.canonical_apply_nanos = elapsed_nanos(apply_started);
        let commit_started = Instant::now();
        tx.commit()?;
        stats.sqlite_commit_nanos = elapsed_nanos(commit_started);
        self.committed(CommitKind::Fold);
        let cleanup_started = Instant::now();
        crate::reclaim::release(plan);
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

    /// The batches a cold-capture planner retained for one cursor since the
    /// last take, in order, leaving every other cursor's. Source-less
    /// batches (a roster's cursor setup) are discarded: no closed source is
    /// made of them.
    pub fn drain_planned_batches_for(&mut self, cursor_id: i64) -> Result<Vec<Batch>, StoreError> {
        Ok(self
            .drain_planned_batches_for_cursors(&[cursor_id])?
            .pop()
            .unwrap_or_default())
    }

    /// [`Store::drain_planned_batches_for`] for several cursors in one pass
    /// (a physical group's members): each cursor's batches, in order, at its
    /// position in `cursor_ids`. One pass, so closing a group of `n` members
    /// stays linear in what the planner retained.
    pub fn drain_planned_batches_for_cursors(
        &mut self,
        cursor_ids: &[i64],
    ) -> Result<Vec<Vec<Batch>>, StoreError> {
        let planned = self
            .planned_batches
            .as_mut()
            .ok_or_else(|| StoreError::BulkLoadState {
                message: "store is not a cold-capture planner".to_string(),
            })?;
        let slots: HashMap<i64, usize> = cursor_ids
            .iter()
            .enumerate()
            .map(|(slot, id)| (*id, slot))
            .collect();
        let mut drained: Vec<Vec<Batch>> = cursor_ids.iter().map(|_| Vec::new()).collect();
        let mut kept = Vec::with_capacity(planned.len());
        for batch in planned.drain(..) {
            // A source-less batch is dropped.
            let Some(id) = batch.source.as_ref().map(|source| source.cursor_id) else {
                continue;
            };
            match slots.get(&id) {
                Some(slot) => drained[*slot].push(batch),
                None => kept.push(batch),
            }
        }
        *planned = kept;
        Ok(drained)
    }

    /// Every cursor a cold-capture planner still retains source batches of.
    pub fn planned_batch_cursors(&self) -> Result<BTreeSet<i64>, StoreError> {
        let planned = self
            .planned_batches
            .as_ref()
            .ok_or_else(|| StoreError::BulkLoadState {
                message: "store is not a cold-capture planner".to_string(),
            })?;
        Ok(planned
            .iter()
            .filter_map(|batch| batch.source.as_ref().map(|source| source.cursor_id))
            .collect())
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
        self.renumber_sessions_in_tx(session_ids)
            .map_err(self.lock_order("renumber"))
    }

    fn renumber_sessions_in_tx(&mut self, session_ids: &[String]) -> Result<u64, StoreError> {
        let (tx, _) =
            lock_order::writer_tx(&mut self.conn, self.shared_readers.as_deref(), "renumber")?;
        let rows = units::renumber_in(&tx, session_ids)?;
        tx.commit()?;
        self.committed(CommitKind::Renumber);
        Ok(rows)
    }
}
