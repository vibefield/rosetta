//! A streamed runtime's RAM fold, partitioned into admission units, and the
//! one writer transaction that admits a batch of them.
//!
//! A runtime admitted whole collects every closed source's deferred facts
//! into one fold, folded at its admission. A runtime admitted per root tree
//! partitions that fold by tree: each source's facts — its terminal delta,
//! its staged slice deltas, the tails of the sessions it resolved and the
//! facts it released — go to the buffer of the tree its path belongs to, so
//! a tree whose last source has committed is folded and flipped on its own
//! ([`Store::admit_cold_units`]) while the others still capture. A tree that
//! flips on its own is closure-proven: no other tree's source addressed it,
//! so every tool correlation, usage target and file edit of its facts lies
//! in its partition. Trees bound by a cross-tree reference are not: their
//! partitions are merged back into one fold ([`ColdFold::absorb`]) before
//! anything is resolved, so the remainder folds exactly as a runtime
//! admitted whole would. Everything here is RAM only (R1 invariant I3); the
//! raw ledger stays the recovery source.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::time::Instant;

use rosetta_core::rows::StreamState;
use rusqlite::{Transaction, params};

use crate::apply::{self, ApplyStats, BulkReconciliationDelta, ColdFold, SourceMode};
use crate::{CommitKind, Store, StoreError, StoreMode, elapsed_nanos, lock_order};

/// One runtime's deferred cold facts — tool openers, lifecycle updates,
/// response usage, and file edits — collected from its closed sources as they
/// commit into a shared private projection, whole or partitioned into
/// admission units. Process-local like the store's own buffer: the raw
/// ledger, never SQLite, is the recovery source.
#[derive(Debug, Default)]
pub struct ColdReconciliation {
    /// Everything not routed to a unit: the whole runtime when it is
    /// admitted whole, and the remainder at a partitioned runtime's close,
    /// with every unit merged in ([`ColdReconciliation::merge`],
    /// [`ColdReconciliation::absorb_units`]) as one fold.
    fold: ColdFold,
    /// Approx bytes of every fact collected or merged into `fold`.
    bytes: u64,
    units: Option<Units>,
    /// Each chunked source's Stage-slice deltas, merged into one per cursor
    /// until its terminal slice commits.
    staged: HashMap<i64, BulkReconciliationDelta>,
}

#[derive(Debug, Default)]
struct Units {
    unit_of_path: HashMap<String, u32>,
    folds: BTreeMap<u32, (ColdFold, u64)>,
    /// Units already taken or absorbed: collecting into one is a bug.
    closed: BTreeSet<u32>,
}

impl ColdReconciliation {
    /// A fold partitioned by admission unit: each source path's facts go to
    /// its unit's buffer; a path with no unit goes to the remainder.
    pub fn partitioned(unit_of_path: HashMap<String, u32>) -> Self {
        Self {
            units: Some(Units {
                unit_of_path,
                ..Units::default()
            }),
            ..Self::default()
        }
    }

    /// Collect one committed source delta at `path`, with whatever its
    /// earlier Stage slices staged for the same cursor merged in first.
    pub(crate) fn collect(
        &mut self,
        delta: BulkReconciliationDelta,
        path: &str,
    ) -> Result<(), StoreError> {
        let delta = match self.staged.remove(&delta.cursor_id()) {
            Some(mut staged) => {
                if delta.mode() == SourceMode::Abandon {
                    return Err(StoreError::SourceState {
                        cursor_id: delta.cursor_id(),
                        message: "an abandoned source was applied in slices".into(),
                    });
                }
                staged.merge_slice(delta)?;
                staged
            }
            None => delta,
        };
        let bytes = delta.approx_bytes();
        if let Some(units) = &mut self.units
            && let Some(unit) = units.unit_of_path.get(path).copied()
        {
            if units.closed.contains(&unit) {
                return Err(StoreError::SourceState {
                    cursor_id: delta.cursor_id(),
                    message: format!("{path} reached admission unit {unit} after its admission"),
                });
            }
            let (fold, unit_bytes) = units.folds.entry(unit).or_default();
            *unit_bytes = unit_bytes.saturating_add(bytes);
            return fold.collect(delta, path);
        }
        self.bytes = self.bytes.saturating_add(bytes);
        self.fold.collect(delta, path)
    }

    /// Keep one Stage slice's delta until its source's terminal slice. A
    /// slice of another generation than the one already staged for its
    /// cursor is refused: no restart follows a slice.
    pub(crate) fn stage(&mut self, delta: BulkReconciliationDelta) -> Result<(), StoreError> {
        match self.staged.get_mut(&delta.cursor_id()) {
            Some(staged) => {
                if staged.generation() != delta.generation() {
                    return Err(StoreError::SourceState {
                        cursor_id: delta.cursor_id(),
                        message: format!(
                            "slice of cursor {} disagrees with its staged generation",
                            delta.cursor_id()
                        ),
                    });
                }
                staged.merge_slice(delta)
            }
            None => {
                self.staged.insert(delta.cursor_id(), delta);
                Ok(())
            }
        }
    }

    /// Refuse, before it is applied, a slice of another generation than the
    /// one already staged for its cursor: no restart follows a slice.
    pub(crate) fn check_stage(&self, cursor_id: i64, generation: u64) -> Result<(), StoreError> {
        match self.staged.get(&cursor_id) {
            Some(staged) if staged.generation() != generation => Err(StoreError::SourceState {
                cursor_id,
                message: format!(
                    "slice of cursor {cursor_id} disagrees with its staged generation"
                ),
            }),
            _ => Ok(()),
        }
    }

    /// Cursors holding staged slice deltas: zero once no item is between its
    /// slices (at a runtime's close).
    pub fn staged_slices(&self) -> usize {
        self.staged.len()
    }

    /// Whether `cursor_id` holds staged slice deltas: never after its own
    /// terminal, whatever other cursors of the runtime are mid-sequence.
    pub fn is_staged(&self, cursor_id: i64) -> bool {
        self.staged.contains_key(&cursor_id)
    }

    /// Take `unit`'s facts as a fold of their own, exactly once.
    pub fn take_unit(&mut self, unit: u32) -> Result<ColdReconciliation, StoreError> {
        let units = self
            .units
            .as_mut()
            .ok_or_else(|| StoreError::BulkLoadState {
                message: "a cold fold admitted whole has no units".into(),
            })?;
        if !units.closed.insert(unit) {
            return Err(StoreError::BulkLoadState {
                message: format!("cold admission unit {unit} was taken twice"),
            });
        }
        let (fold, bytes) = units.folds.remove(&unit).unwrap_or_default();
        Ok(ColdReconciliation {
            fold,
            bytes,
            ..ColdReconciliation::default()
        })
    }

    /// The approx bytes buffered for `unit` (zero once taken).
    pub fn unit_bytes(&self, unit: u32) -> u64 {
        self.units
            .as_ref()
            .and_then(|units| units.folds.get(&unit))
            .map_or(0, |(_, bytes)| *bytes)
    }

    /// Fold `other` in with this one: a batch of units admitted together.
    pub fn merge(&mut self, other: ColdReconciliation) -> Result<(), StoreError> {
        if !other.staged.is_empty() || other.units.is_some() {
            return Err(StoreError::BulkLoadState {
                message: "only a taken admission unit merges into a cold fold".into(),
            });
        }
        self.bytes = self.bytes.saturating_add(other.bytes);
        self.fold.absorb(other.fold)
    }

    /// Merge every unit not taken yet into the remainder, as one fold: the
    /// runtime's close admits whatever never flipped on its own — bound
    /// trees among it, whose facts may address each other's sessions.
    pub fn absorb_units(&mut self) -> Result<(), StoreError> {
        let Some(units) = &mut self.units else {
            return Ok(());
        };
        for (unit, (fold, bytes)) in std::mem::take(&mut units.folds) {
            units.closed.insert(unit);
            self.bytes = self.bytes.saturating_add(bytes);
            self.fold.absorb(fold)?;
        }
        Ok(())
    }

    /// Approx bytes of the facts this fold (not its units) holds.
    pub fn tail_bytes(&self) -> u64 {
        self.bytes
    }

    /// The one fold to resolve: every unit not taken must have been
    /// absorbed first.
    pub(crate) fn into_fold(self) -> Result<ColdFold, StoreError> {
        if self
            .units
            .as_ref()
            .is_some_and(|units| !units.folds.is_empty())
        {
            return Err(StoreError::BulkLoadState {
                message: "a partitioned cold fold was resolved with units not absorbed".into(),
            });
        }
        Ok(self.fold)
    }
}

/// What [`Store::admit_cold_units`] wrote.
#[derive(Debug, Default)]
pub struct ColdAdmission {
    pub stats: ApplyStats,
    /// The sessions whose `stream_state` this commit moved.
    pub changed: Vec<String>,
    /// Message rows the renumber rewrote.
    pub renumber_rows: u64,
    /// The writer's wait for the `session` write lock (W1).
    pub lock_wait_nanos: u64,
    /// From W1 taken to the commit: how long the transaction held it.
    /// Measurement only.
    pub lock_held_nanos: u64,
    /// The moment the transaction committed. Measurement only.
    pub committed_at: Option<Instant>,
}

impl Store {
    /// Admit a batch of a streamed runtime's units in one writer transaction:
    /// the guard (no session the fold's tool facts address holds a stored
    /// tool row), the fold of their facts, the renumber of `renumber`, and
    /// the readiness flip of `states` **last** — the first moment any of
    /// their rows can be read. Returns the sessions whose state moved.
    pub fn admit_cold_units(
        &mut self,
        fold: ColdReconciliation,
        renumber: &[String],
        states: &[(String, StreamState)],
    ) -> Result<ColdAdmission, StoreError> {
        let admitted = self
            .admit_cold_units_in_tx(fold, renumber, states)
            .map_err(self.lock_order("cold admission"))?;
        self.note_stream_states(&admitted.changed);
        Ok(admitted)
    }

    fn admit_cold_units_in_tx(
        &mut self,
        fold: ColdReconciliation,
        renumber: &[String],
        states: &[(String, StreamState)],
    ) -> Result<ColdAdmission, StoreError> {
        if self.mode != StoreMode::Bulk {
            return Err(StoreError::BulkLoadState {
                message: "a streamed cold admission requires a private cold rebuild".to_string(),
            });
        }
        if fold.staged_slices() != 0 {
            return Err(StoreError::BulkLoadState {
                message: "a streamed cold admission found a source between its slices".into(),
            });
        }
        let reconcile_started = Instant::now();
        let (mut plan, addressed) = fold.into_fold()?.finish()?;
        let reconcile_nanos = elapsed_nanos(reconcile_started);

        let (tx, lock_wait_nanos) = lock_order::writer_tx(
            &mut self.conn,
            self.shared_readers.as_deref(),
            "cold admission",
        )?;
        let held_from = Instant::now();
        guard_no_stored_tool_rows(&tx, &addressed)?;
        let apply_started = Instant::now();
        let mut stats = if plan.is_empty() {
            ApplyStats::default()
        } else {
            apply::apply_bulk_reconciliation(
                &tx,
                &mut plan,
                self.bulk_ordinals_precomputed,
                &mut self.usage_owners,
            )?
        };
        let renumber_rows = renumber_in(&tx, renumber)?;
        stats.profile.session_renumbers = renumber.len() as u64;
        stats.profile.session_renumber_rows = renumber_rows;
        let changed = set_stream_states_in(&tx, states)?;
        stats.profile.tool_reconcile_nanos = reconcile_nanos;
        stats.profile.session_lock_wait_nanos = lock_wait_nanos;
        stats.canonical_apply_nanos = elapsed_nanos(apply_started);
        let commit_started = Instant::now();
        tx.commit()?;
        let committed_at = Instant::now();
        stats.sqlite_commit_nanos = elapsed_nanos(commit_started);
        let lock_held_nanos = elapsed_nanos(held_from);
        stats.profile.session_lock_held_nanos = lock_held_nanos;
        self.committed(CommitKind::Admission);
        let cleanup_started = Instant::now();
        crate::reclaim::release(plan);
        stats.profile.tool_reconcile_cleanup_nanos = elapsed_nanos(cleanup_started);
        Ok(ColdAdmission {
            stats,
            changed,
            renumber_rows,
            lock_wait_nanos,
            lock_held_nanos,
            committed_at: Some(committed_at),
        })
    }

    /// [`Store::set_session_stream_states`], returning the sessions whose
    /// state moved instead of their count.
    pub fn set_session_stream_states_changed(
        &mut self,
        states: &[(String, StreamState)],
    ) -> Result<Vec<String>, StoreError> {
        if states.is_empty() {
            return Ok(Vec::new());
        }
        let changed = (|| {
            let (tx, _) = lock_order::writer_tx(
                &mut self.conn,
                self.shared_readers.as_deref(),
                "stream states",
            )?;
            let changed = set_stream_states_in(&tx, states)?;
            tx.commit()?;
            Ok(changed)
        })()
        .map_err(self.lock_order("stream states"))?;
        self.committed(CommitKind::StreamStates);
        self.note_stream_states(&changed);
        Ok(changed)
    }

    /// Note committed readiness changes for the hub: a row effect and a
    /// metadata effect per changed session.
    pub(crate) fn note_stream_states(&mut self, changed: &[String]) {
        for session_id in changed {
            if let Some(effects) = self.effects.as_mut() {
                effects.touch_session(session_id);
            }
            if let Some(meta) = self.meta_effects.as_mut() {
                meta.note_session(session_id);
            }
        }
    }
}

/// A consumed fold resolves only openers it holds: a session it addresses
/// that already holds a tool row would be written twice. Checked, not
/// trusted.
pub(crate) fn guard_no_stored_tool_rows(
    tx: &Transaction,
    addressed: &BTreeSet<String>,
) -> Result<(), StoreError> {
    let mut stored = tx.prepare_cached(
        "SELECT EXISTS(
           SELECT 1 FROM message WHERE session_id = ?1 AND type = 'tool_use'
         )",
    )?;
    for session_id in addressed {
        if stored.query_row(params![session_id], |row| row.get::<_, bool>(0))? {
            return Err(StoreError::ProjectionInvalid {
                message: format!(
                    "streamed cold fold found a stored tool row in session {session_id}"
                ),
            });
        }
    }
    Ok(())
}

/// Set each session's readiness badge inside `tx`; returns the sessions
/// whose state moved.
pub(crate) fn set_stream_states_in(
    tx: &Transaction,
    states: &[(String, StreamState)],
) -> Result<Vec<String>, StoreError> {
    let mut changed = Vec::new();
    let mut update = tx.prepare_cached(
        "UPDATE session SET stream_state = ?2
         WHERE id = ?1 AND stream_state != ?2",
    )?;
    for (session_id, state) in states {
        if update.execute(params![session_id, state.as_str()])? != 0 {
            changed.push(session_id.clone());
        }
    }
    Ok(changed)
}

/// Canonicalize the named sessions' presentation ordinals inside `tx`:
/// source path first, then the stored ordinal
/// ([`Store::renumber_sessions_by_source_order`]). Returns the rows moved.
pub(crate) fn renumber_in(tx: &Transaction, session_ids: &[String]) -> Result<u64, StoreError> {
    if session_ids.is_empty() {
        return Ok(0);
    }
    apply::prepare_renumber_table(tx)?;
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
    apply::apply_renumber_table(tx)?;
    Ok(rows)
}
