//! Process-local staging and late-arrival reconciliation.
//!
//! Normalized operations are projection inputs, not a third durable data
//! representation. Replacement generations therefore wait here until they
//! activate, and usage/tool updates that precede their message wait here until
//! the opener arrives. SQLite receives only the resulting canonical rows. If
//! the process exits, the active projection remains intact and the raw ledger
//! is replayed to reconstruct this disposable state.

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{Connection, params};

use rosetta_core::{EvidenceOrder, IngestOp, TimelineBody};

use crate::StoreError;

use super::{Batch, SourceMode};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ToolTarget {
    session_id: String,
    correlation_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct UsageTarget {
    session_id: String,
    response: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum RelationTarget {
    Tool(ToolTarget),
    Usage(UsageTarget),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct PendingKey {
    target: RelationTarget,
    /// Tool patches retain every evidence position. Usage keeps one latest
    /// snapshot (`None`). Ordering by target then optional evidence also lets
    /// an opener seek to `None` and visit only its own pending fragments.
    evidence: Option<EvidenceOrder>,
}

impl PendingKey {
    fn target(&self) -> RelationTarget {
        self.target.clone()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct SourceKey {
    cursor_id: i64,
    generation: u64,
}

#[derive(Debug, Clone)]
struct PendingEntry {
    op: IngestOp,
    source: Option<SourceKey>,
}

#[derive(Debug, Default)]
pub(crate) struct RamReconciliationBuffer {
    pending: BTreeMap<PendingKey, PendingEntry>,
    /// Source transitions invalidate only their own generations. This index
    /// changes with `pending` after the SQL commit; target lookups use the
    /// primary map's ordering and need no second copy of every orphan key.
    by_source: BTreeMap<SourceKey, BTreeSet<PendingKey>>,
    /// Complete normalized source positions for a not-yet-active live
    /// replacement. This is deliberately not serialized.
    staged: BTreeMap<SourceKey, BTreeMap<u64, IngestOp>>,
}

#[derive(Debug, Default)]
pub(crate) struct RamReconciliationDelta {
    /// `None` means apply the caller's batch unchanged. `Some` is the complete
    /// operation list to project for this transaction (empty for Stage).
    projected_ops: Option<Vec<IngestOp>>,
    observed: BTreeMap<PendingKey, PendingEntry>,
    drained: BTreeSet<PendingKey>,
    invalidated: BTreeSet<PendingKey>,
    stage: Vec<(SourceKey, u64, IngestOp)>,
    clear_cursor: Option<i64>,
}

#[derive(Debug, Default)]
pub(crate) struct RamReconciliationOutcome {
    remove: BTreeSet<PendingKey>,
    insert: BTreeMap<PendingKey, PendingEntry>,
}

impl RamReconciliationBuffer {
    pub(crate) fn prepare(
        &self,
        conn: &Connection,
        batch: &Batch,
        direct_first_generation: bool,
        track_relations: bool,
    ) -> Result<RamReconciliationDelta, StoreError> {
        let progressive_direct = if direct_first_generation {
            if let Some(source) = batch.source.as_ref() {
                conn.prepare_cached(
                    "SELECT active_generation = 0 FROM ingest_cursor WHERE id = ?1",
                )?
                .query_row(params![source.cursor_id], |row| row.get::<_, bool>(0))?
            } else {
                false
            }
        } else {
            false
        };

        let mut projected_ops = None;
        let mut stage = Vec::new();
        let mut clear_cursor = None;
        match batch.source.as_ref().map(|source| source.mode) {
            None | Some(SourceMode::Append) => {}
            Some(_) if progressive_direct => {}
            Some(SourceMode::Stage) => {
                let source = batch.source.as_ref().expect("source mode came from source");
                let source_key = source_key(source.cursor_id, source.generation);
                for (sequence, op) in positioned_ops(source, &batch.ops)? {
                    validate_staged_position(self.staged.get(&source_key), source, sequence, op)?;
                    stage.push((source_key, sequence, op.clone()));
                }
                // The checkpoint/generation watermark still commits, but no
                // normalized payload is projected before activation.
                projected_ops = Some(Vec::new());
            }
            Some(SourceMode::Activate | SourceMode::Abandon) => {
                let source = batch.source.as_ref().expect("source mode came from source");
                let source_key = source_key(source.cursor_id, source.generation);
                let mut complete = self.staged.get(&source_key).cloned().unwrap_or_default();
                for (sequence, op) in positioned_ops(source, &batch.ops)? {
                    if let Some(previous) = complete.get(&sequence) {
                        if previous != op {
                            return Err(StoreError::SourceConflict {
                                cursor_id: source.cursor_id,
                                generation: source.generation,
                                sequence,
                            });
                        }
                    } else {
                        complete.insert(sequence, op.clone());
                    }
                }
                projected_ops = Some(complete.into_values().collect());
                clear_cursor = Some(source.cursor_id);
            }
        }

        let source = batch
            .source
            .as_ref()
            .map(|source| source_key(source.cursor_id, source.generation));
        let relation_ops = match (
            track_relations,
            batch.source.as_ref().map(|source| source.mode),
        ) {
            (false, _) => &[][..],
            (true, Some(SourceMode::Abandon)) => &[][..],
            (true, Some(SourceMode::Stage)) if !progressive_direct => &[][..],
            (true, _) => projected_ops.as_deref().unwrap_or(&batch.ops),
        };

        let mut openers = BTreeSet::new();
        let mut observed = BTreeMap::new();
        for op in relation_ops {
            note_openers(&mut openers, op);
            let Some(key) = pending_key(op) else {
                continue;
            };
            let entry = PendingEntry {
                op: op.clone(),
                source,
            };
            if let Some(previous) = self.pending.get(&key) {
                validate_pending_identity(&key, &previous.op, op)?;
            }
            insert_observed(&mut observed, key, entry)?;
        }

        // Replacing one source invalidates unresolved fragments contributed by
        // its older generation. Resolved rows are healed by the affected-scope
        // ledger replay; no hidden inverse facts are retained here or in SQL.
        let mut invalidated = BTreeSet::new();
        if let Some(source) = batch.source.as_ref() {
            let stamp = source_key(source.cursor_id, source.generation);
            match source.mode {
                SourceMode::Activate => {
                    for keys in self
                        .by_source
                        .range(source_key(source.cursor_id, 0)..stamp)
                        .map(|(_, keys)| keys)
                    {
                        invalidated.extend(keys.iter().cloned());
                    }
                }
                SourceMode::Abandon => {
                    if let Some(keys) = self.by_source.get(&stamp) {
                        invalidated.extend(keys.iter().cloned());
                    }
                }
                SourceMode::Stage | SourceMode::Append => {}
            }
        }

        let may_drain = track_relations
            && match batch.source.as_ref().map(|source| source.mode) {
                Some(SourceMode::Abandon) => false,
                Some(SourceMode::Stage) => progressive_direct,
                _ => true,
            };
        let mut drained = BTreeSet::new();
        if may_drain {
            for target in &openers {
                let start = PendingKey {
                    target: target.clone(),
                    evidence: None,
                };
                for (key, _) in self.pending.range(start..) {
                    if &key.target != target {
                        break;
                    }
                    if !invalidated.contains(key) && !observed.contains_key(key) {
                        drained.insert(key.clone());
                    }
                }
            }
        }
        if !drained.is_empty() {
            let ops = projected_ops.get_or_insert_with(|| batch.ops.clone());
            // Preserve PendingKey/evidence order across all matching targets.
            ops.extend(drained.iter().map(|key| self.pending[key].op.clone()));
        }

        Ok(RamReconciliationDelta {
            projected_ops,
            observed,
            drained,
            invalidated,
            stage,
            clear_cursor,
        })
    }

    pub(crate) fn projected_batch(
        &self,
        batch: &Batch,
        delta: &RamReconciliationDelta,
    ) -> Option<Batch> {
        delta.projected_ops.as_ref().map(|ops| Batch {
            ops: ops.clone(),
            checkpoints: batch.checkpoints.clone(),
            source: batch.source.clone(),
        })
    }

    pub(crate) fn resolve(
        &self,
        conn: &Connection,
        delta: &RamReconciliationDelta,
    ) -> Result<RamReconciliationOutcome, StoreError> {
        let mut outcome = RamReconciliationOutcome {
            remove: delta.invalidated.clone(),
            insert: BTreeMap::new(),
        };
        let targets = delta
            .observed
            .keys()
            .chain(&delta.drained)
            .map(PendingKey::target)
            .collect::<BTreeSet<_>>();
        if targets.is_empty() {
            return Ok(outcome);
        }
        let mut matched = BTreeSet::new();
        let mut tool_statement = conn.prepare_cached(
            "SELECT EXISTS(
               SELECT 1 FROM message
               WHERE session_id = ?1 AND tool_correlation_key = ?2 AND type = 'tool_use'
             )",
        )?;
        let mut usage_statement = conn.prepare_cached(
            "SELECT EXISTS(
               SELECT 1 FROM message WHERE session_id = ?1 AND timeline_id = ?2
             )",
        )?;
        for target in targets {
            let exists = match &target {
                RelationTarget::Tool(target) => tool_statement
                    .query_row(params![target.session_id, target.correlation_key], |row| {
                        row.get::<_, bool>(0)
                    })?,
                RelationTarget::Usage(target) => usage_statement
                    .query_row(params![target.session_id, target.response], |row| {
                        row.get::<_, bool>(0)
                    })?,
            };
            if exists {
                matched.insert(target);
            }
        }

        for (key, entry) in &delta.observed {
            if matched.contains(&key.target()) {
                outcome.remove.insert(key.clone());
            } else {
                insert_observed(&mut outcome.insert, key.clone(), entry.clone())?;
            }
        }
        for key in &delta.drained {
            if matched.contains(&key.target()) {
                outcome.remove.insert(key.clone());
            }
        }
        Ok(outcome)
    }

    pub(crate) fn commit(&mut self, outcome: RamReconciliationOutcome) {
        for key in outcome.remove {
            self.remove_pending(&key);
        }
        for (key, entry) in outcome.insert {
            // A newer usage snapshot (or exact tool retry) may now belong to
            // another source. Remove its old ownership before reindexing it.
            self.remove_pending(&key);
            if let Some(source) = entry.source {
                self.by_source
                    .entry(source)
                    .or_default()
                    .insert(key.clone());
            }
            self.pending.insert(key, entry);
        }
    }

    fn remove_pending(&mut self, key: &PendingKey) {
        let Some(entry) = self.pending.remove(key) else {
            return;
        };
        if let Some(source) = entry.source
            && let Some(keys) = self.by_source.get_mut(&source)
        {
            keys.remove(key);
            if keys.is_empty() {
                self.by_source.remove(&source);
            }
        }
    }

    pub(crate) fn commit_delta(&mut self, delta: &RamReconciliationDelta) {
        if let Some(cursor_id) = delta.clear_cursor {
            self.staged.retain(|key, _| key.cursor_id != cursor_id);
        }
        for (source, sequence, op) in &delta.stage {
            // A newer staged attempt supersedes an older unactivated attempt.
            self.staged
                .retain(|key, _| key.cursor_id != source.cursor_id || key == source);
            self.staged
                .entry(*source)
                .or_default()
                .entry(*sequence)
                .or_insert_with(|| op.clone());
        }
    }
}

fn source_key(cursor_id: i64, generation: u64) -> SourceKey {
    SourceKey {
        cursor_id,
        generation,
    }
}

fn positioned_ops<'a>(
    source: &super::SourceWrite,
    ops: &'a [IngestOp],
) -> Result<Vec<(u64, &'a IngestOp)>, StoreError> {
    ops.iter()
        .enumerate()
        .map(|(index, op)| {
            let index = u64::try_from(index).map_err(|_| StoreError::SourceState {
                cursor_id: source.cursor_id,
                message: "source batch index exceeds u64".into(),
            })?;
            let sequence = source.first_sequence.checked_add(index).ok_or_else(|| {
                StoreError::SourceState {
                    cursor_id: source.cursor_id,
                    message: "source sequence overflowed while staging in RAM".into(),
                }
            })?;
            Ok((sequence, op))
        })
        .collect()
}

fn validate_staged_position(
    staged: Option<&BTreeMap<u64, IngestOp>>,
    source: &super::SourceWrite,
    sequence: u64,
    op: &IngestOp,
) -> Result<(), StoreError> {
    if let Some(previous) = staged.and_then(|ops| ops.get(&sequence))
        && previous != op
    {
        return Err(StoreError::SourceConflict {
            cursor_id: source.cursor_id,
            generation: source.generation,
            sequence,
        });
    }
    Ok(())
}

fn note_openers(targets: &mut BTreeSet<RelationTarget>, op: &IngestOp) {
    let IngestOp::Message(row) = op else { return };
    targets.insert(RelationTarget::Usage(UsageTarget {
        session_id: row.session_id.clone(),
        response: row.timeline_id.clone(),
    }));
    if let TimelineBody::ToolUse { tool } = &row.body {
        targets.insert(RelationTarget::Tool(ToolTarget {
            session_id: row.session_id.clone(),
            correlation_key: tool.correlation.key.clone(),
        }));
    }
}

fn pending_key(op: &IngestOp) -> Option<PendingKey> {
    match op {
        IngestOp::ToolUpdate {
            session_id,
            correlation_key,
            evidence,
            ..
        } => Some(PendingKey {
            target: RelationTarget::Tool(ToolTarget {
                session_id: session_id.clone(),
                correlation_key: correlation_key.clone(),
            }),
            evidence: Some(evidence.clone()),
        }),
        IngestOp::Usage {
            session_id,
            response,
            ..
        } => Some(PendingKey {
            target: RelationTarget::Usage(UsageTarget {
                session_id: session_id.clone(),
                response: response.clone(),
            }),
            evidence: None,
        }),
        _ => None,
    }
}

fn validate_pending_identity(
    key: &PendingKey,
    previous: &IngestOp,
    current: &IngestOp,
) -> Result<(), StoreError> {
    if matches!(key.target, RelationTarget::Tool(_)) && previous != current {
        return Err(StoreError::ProjectionInvalid {
            message: format!("conflicting RAM tool updates share evidence order for {key:?}"),
        });
    }
    Ok(())
}

fn insert_observed(
    target: &mut BTreeMap<PendingKey, PendingEntry>,
    key: PendingKey,
    entry: PendingEntry,
) -> Result<(), StoreError> {
    if let Some(previous) = target.get(&key) {
        validate_pending_identity(&key, &previous.op, &entry.op)?;
    }
    // Usage intentionally keeps the last source-ordered snapshot. Tool
    // identity validation above permits only exact retry replacement.
    target.insert(key, entry);
    Ok(())
}
