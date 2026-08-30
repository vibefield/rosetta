//! One-shot relational reconciliation for an unpublished cold projection.
//!
//! A private rebuild already has the raw ledger as its durable authority and
//! cannot be observed before validation. Re-reading and rewriting a tool row
//! for every lifecycle fragment therefore buys no correctness. This buffer is
//! generation-aware: committed source batches contribute closed canonical ops;
//! once every source for a runtime is sealed, all correlations are folded in
//! RAM and each final tool row is written exactly once. Reduced source-owned
//! facts are still persisted so later live generation replacement can subtract
//! one source without replaying the whole ledger.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use rusqlite::{Connection, Transaction, params};

use rosetta_core::timeline::StatusBasis;
use rosetta_core::{
    EvidenceOrder, IngestOp, SourceIdentity, TimelineBody, TimelineEntry, ToolInteraction,
    ToolMergeState, ToolStatus, ToolStatusFact, ToolUpdatePatch,
};

use crate::StoreError;

use super::rows::{
    BULK_MAINTENANCE, BucketKey, PendingGate, apply_reconciled_tool_message, persist_tool_fact,
    touch_session, update_reconciled_tool,
};
use super::{ApplyStats, Batch, SourceMode, apply_op, elapsed_nanos, profile_ordinal_lookup};

#[derive(Debug)]
enum DeferredOp {
    ToolMessage(Box<TimelineEntry>),
    ToolUpdate {
        session_id: String,
        correlation_key: String,
        evidence: EvidenceOrder,
        patch: Box<ToolUpdatePatch>,
        at: Option<i64>,
    },
    Tail(IngestOp),
}

#[derive(Debug)]
pub(crate) struct BulkReconciliationDelta {
    cursor_id: i64,
    generation: u64,
    mode: SourceMode,
    activation_end: u64,
    /// Compact identity for every normalized source position. Immediate ops
    /// keep their existing SQLite conflict path; only deferred relational ops
    /// are hashed, so large assets and prose are never serialized a second
    /// time merely to maintain the RAM buffer.
    fingerprints: BTreeMap<u64, OpFingerprint>,
    deferred: BTreeMap<u64, DeferredOp>,
    invalidated: BTreeSet<ToolKey>,
}

impl BulkReconciliationDelta {
    pub(crate) fn from_batch(
        batch: &Batch,
        conn: &Connection,
        capture_replacement: bool,
    ) -> Result<Option<Self>, StoreError> {
        let Some(source) = batch.source.as_ref() else {
            return Ok(None);
        };
        let activation_end = source
            .first_sequence
            .checked_add(
                u64::try_from(batch.ops.len()).map_err(|_| StoreError::SourceState {
                    cursor_id: source.cursor_id,
                    message: "source batch contains more operations than u64 can represent".into(),
                })?,
            )
            .ok_or_else(|| StoreError::SourceState {
                cursor_id: source.cursor_id,
                message: "source sequence overflowed while buffering cold reconciliation".into(),
            })?;
        let mut fingerprints = BTreeMap::new();
        let mut deferred_ops = BTreeMap::new();
        for (index, op) in batch.ops.iter().enumerate() {
            let sequence = source
                .first_sequence
                .checked_add(u64::try_from(index).map_err(|_| StoreError::SourceState {
                    cursor_id: source.cursor_id,
                    message: "source batch index exceeds u64".into(),
                })?)
                .ok_or_else(|| StoreError::SourceState {
                    cursor_id: source.cursor_id,
                    message: "source sequence overflowed while buffering a cold operation".into(),
                })?;
            let deferred = match op {
                IngestOp::Message(row) if matches!(&row.body, TimelineBody::ToolUse { .. }) => {
                    Some(DeferredOp::ToolMessage(row.clone()))
                }
                IngestOp::ToolUpdate {
                    session_id,
                    correlation_key,
                    evidence,
                    patch,
                    at,
                } => Some(DeferredOp::ToolUpdate {
                    session_id: session_id.clone(),
                    correlation_key: correlation_key.clone(),
                    evidence: evidence.clone(),
                    patch: patch.clone(),
                    at: *at,
                }),
                IngestOp::Usage { .. } | IngestOp::FileEdit(_) => {
                    Some(DeferredOp::Tail(op.clone()))
                }
                _ => None,
            };
            let Some(deferred_op) = deferred else {
                fingerprints.insert(sequence, OpFingerprint::Immediate);
                continue;
            };
            fingerprints.insert(
                sequence,
                OpFingerprint::Deferred(*blake3::hash(&serde_json::to_vec(op)?).as_bytes()),
            );
            deferred_ops.insert(sequence, deferred_op);
        }
        let invalidated = if capture_replacement
            && matches!(source.mode, SourceMode::Stage | SourceMode::Activate)
        {
            replacement_invalidations(conn, source.cursor_id, source.generation)?
        } else {
            BTreeSet::new()
        };
        Ok(Some(Self {
            cursor_id: source.cursor_id,
            generation: source.generation,
            mode: source.mode,
            activation_end,
            fingerprints,
            deferred: deferred_ops,
            invalidated,
        }))
    }
}

fn replacement_invalidations(
    conn: &Connection,
    cursor_id: i64,
    generation: u64,
) -> Result<BTreeSet<ToolKey>, StoreError> {
    let active: i64 = conn
        .prepare_cached("SELECT active_generation FROM ingest_cursor WHERE id = ?1")?
        .query_row(params![cursor_id], |row| row.get(0))?;
    let active = u64::try_from(active).map_err(|_| StoreError::SourceState {
        cursor_id,
        message: "active generation is negative while capturing tool invalidations".into(),
    })?;
    if active == 0 || generation <= active {
        return Ok(BTreeSet::new());
    }
    let mut statement = conn.prepare(
        "SELECT DISTINCT f.session_id, f.correlation_key
         FROM tool_update_fact f
         JOIN message m
           ON m.session_id = f.session_id
          AND m.tool_correlation_key = f.correlation_key
          AND m.type = 'tool_use'
         WHERE f.src_cursor = ?1
           AND (m.src_cursor IS NULL OR m.src_cursor != ?1)
         ORDER BY f.session_id, f.correlation_key",
    )?;
    Ok(statement
        .query_map(params![cursor_id], |row| {
            Ok(ToolKey {
                session_id: row.get(0)?,
                correlation_key: row.get(1)?,
            })
        })?
        .collect::<Result<BTreeSet<_>, _>>()?)
}

pub(super) fn defers(op: &IngestOp) -> bool {
    matches!(
        op,
        IngestOp::Message(row) if matches!(&row.body, TimelineBody::ToolUse { .. })
    ) || matches!(
        op,
        IngestOp::ToolUpdate { .. } | IngestOp::Usage { .. } | IngestOp::FileEdit(_)
    )
}

#[derive(Debug)]
struct BufferedSource {
    /// End-exclusive operation sequence sealed by `SourceMode::Activate`.
    /// Zero means this buffer began with an already-active append; appended
    /// openers must not receive a synthetic end-of-generation fact.
    activation_end: Option<u64>,
    fingerprints: BTreeMap<u64, OpFingerprint>,
    deferred: BTreeMap<u64, DeferredOp>,
    invalidated: BTreeSet<ToolKey>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpFingerprint {
    Immediate,
    Deferred([u8; 32]),
}

impl BufferedSource {
    fn new(mode: SourceMode) -> Self {
        Self {
            activation_end: (mode == SourceMode::Append).then_some(0),
            fingerprints: BTreeMap::new(),
            deferred: BTreeMap::new(),
            invalidated: BTreeSet::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct SourceKey {
    cursor_id: i64,
    generation: u64,
}

#[derive(Debug, Default)]
pub(crate) struct BulkReconciliationBuffer {
    sources: BTreeMap<SourceKey, BufferedSource>,
}

impl BulkReconciliationBuffer {
    /// Validate retry identity before the source transaction starts. A failed
    /// comparison therefore changes neither SQLite nor the committed RAM
    /// generation. Exact retries are suppressed when the delta is committed.
    pub(crate) fn validate(&self, delta: &BulkReconciliationDelta) -> Result<(), StoreError> {
        let key = SourceKey {
            cursor_id: delta.cursor_id,
            generation: delta.generation,
        };
        let Some(source) = self.sources.get(&key) else {
            return Ok(());
        };
        for (sequence, fingerprint) in &delta.fingerprints {
            if source
                .fingerprints
                .get(sequence)
                .is_some_and(|existing| existing != fingerprint)
            {
                return Err(StoreError::SourceConflict {
                    cursor_id: delta.cursor_id,
                    generation: delta.generation,
                    sequence: *sequence,
                });
            }
        }
        Ok(())
    }

    pub(crate) fn commit(&mut self, delta: BulkReconciliationDelta) {
        let key = SourceKey {
            cursor_id: delta.cursor_id,
            generation: delta.generation,
        };
        if delta.mode == SourceMode::Abandon {
            self.sources.remove(&key);
            return;
        }

        // A newer incomplete generation supersedes only another incomplete
        // attempt. The last activated generation remains available until the
        // replacement's Activate transaction commits; Abandon therefore falls
        // back to it exactly like the canonical projection does.
        if delta.mode == SourceMode::Stage {
            self.sources.retain(|candidate, source| {
                candidate.cursor_id != delta.cursor_id
                    || candidate.generation == delta.generation
                    || source.activation_end.is_some()
            });
        } else if delta.mode == SourceMode::Activate {
            self.sources.retain(|candidate, _| {
                candidate.cursor_id != delta.cursor_id || candidate.generation == delta.generation
            });
        }
        let source = self
            .sources
            .entry(key)
            .or_insert_with(|| BufferedSource::new(delta.mode));
        for (sequence, fingerprint) in delta.fingerprints {
            source.fingerprints.entry(sequence).or_insert(fingerprint);
        }
        for (sequence, op) in delta.deferred {
            source.deferred.entry(sequence).or_insert(op);
        }
        source.invalidated.extend(delta.invalidated);
        if delta.mode == SourceMode::Activate {
            source.activation_end = Some(delta.activation_end);
        } else if delta.mode == SourceMode::Append && source.activation_end.is_none() {
            source.activation_end = Some(0);
        }
    }

    pub(crate) fn prepare(&self, conn: &Connection) -> Result<BulkReconciliationPlan, StoreError> {
        let source_keys = self
            .sources
            .iter()
            .filter_map(|(key, source)| source.activation_end.map(|_| *key))
            .collect::<BTreeSet<_>>();
        if source_keys.is_empty() {
            return Ok(BulkReconciliationPlan::default());
        }
        let source_ids = source_keys
            .iter()
            .map(|key| key.cursor_id)
            .collect::<BTreeSet<_>>();

        let mut paths = BTreeMap::new();
        let mut statement = conn.prepare("SELECT id, path FROM ingest_cursor ORDER BY id")?;
        for row in statement.query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })? {
            let (cursor_id, path) = row?;
            if source_ids.contains(&cursor_id) {
                paths.insert(cursor_id, path);
            }
        }
        for cursor_id in &source_ids {
            if !paths.contains_key(cursor_id) {
                return Err(StoreError::SourceState {
                    cursor_id: *cursor_id,
                    message: "cold reconciliation source cursor vanished".into(),
                });
            }
        }

        let mut openers: BTreeMap<ToolKey, BufferedOpener> = BTreeMap::new();
        let mut new_facts: BTreeMap<FactKey, BufferedFact> = BTreeMap::new();
        let mut tail = Vec::new();
        let mut native_updates = Vec::new();
        let mut session_touches = BTreeMap::new();
        let mut opener_finalizations = Vec::new();
        for source_key in &source_keys {
            let source = self
                .sources
                .get(source_key)
                .expect("selected source exists");
            let path = paths
                .get(&source_key.cursor_id)
                .expect("selected source path exists");
            let activation_end = source.activation_end.expect("selected source is activated");
            for (sequence, op) in &source.deferred {
                let order = SourceOrder {
                    path: path.clone(),
                    sequence: *sequence,
                };
                match op {
                    DeferredOp::ToolMessage(row) => {
                        let TimelineBody::ToolUse { tool } = &row.body else {
                            unreachable!("only tool rows enter the bulk reconciliation buffer")
                        };
                        let key = ToolKey {
                            session_id: row.session_id.clone(),
                            correlation_key: tool.correlation.key.clone(),
                        };
                        note_session_touch(&mut session_touches, &row.session_id, row.at);
                        if activation_end != 0 && *sequence < activation_end {
                            opener_finalizations.push(BufferedFinalization {
                                key: key.clone(),
                                cursor_id: source_key.cursor_id,
                                generation: source_key.generation,
                            });
                        }
                        let candidate = BufferedOpener {
                            order,
                            row: (**row).clone(),
                        };
                        if let Some(previous) = openers.get_mut(&key) {
                            if previous.row.timeline_id != candidate.row.timeline_id {
                                return Err(StoreError::ProjectionInvalid {
                                    message: format!(
                                        "cold tool openers {} and {} share correlation {}/{}",
                                        previous.row.timeline_id,
                                        candidate.row.timeline_id,
                                        key.session_id,
                                        key.correlation_key
                                    ),
                                });
                            }
                            if previous.order == candidate.order && previous.row != candidate.row {
                                return Err(StoreError::ProjectionInvalid {
                                    message: format!(
                                        "cold tool opener {} has conflicting payloads at {}:{}",
                                        candidate.row.timeline_id,
                                        candidate.order.path,
                                        candidate.order.sequence
                                    ),
                                });
                            }
                            // Claude can repeat one native assistant record on
                            // a later branch with the same UUID/tool ID but a
                            // different parent. Ordinary SQLite upsert semantics
                            // made the later source position authoritative; the
                            // one-shot fold must preserve that exact behavior.
                            if candidate.order > previous.order {
                                *previous = candidate;
                            }
                        } else {
                            openers.insert(key, candidate);
                        }
                    }
                    DeferredOp::ToolUpdate {
                        session_id,
                        correlation_key,
                        evidence,
                        patch,
                        at,
                    } => {
                        let key = ToolKey {
                            session_id: session_id.clone(),
                            correlation_key: correlation_key.clone(),
                        };
                        native_updates.push((key.clone(), *at));
                        insert_fact(
                            &mut new_facts,
                            BufferedFact {
                                key,
                                evidence: evidence.clone(),
                                patch: (**patch).clone(),
                                at: *at,
                                src_cursor: Some(source_key.cursor_id),
                            },
                        )?;
                    }
                    DeferredOp::Tail(op) => tail.push(BufferedTail {
                        cursor_id: source_key.cursor_id,
                        order,
                        op: op.clone(),
                    }),
                }
            }
        }

        for finalization in opener_finalizations {
            let path = paths
                .get(&finalization.cursor_id)
                .expect("opener source path was loaded");
            insert_fact(
                &mut new_facts,
                BufferedFact {
                    key: finalization.key,
                    evidence: EvidenceOrder {
                        source_identity: SourceIdentity(format!("{path}#source-finalization")),
                        generation: finalization.generation,
                        record_offset: i64::MAX as u64,
                        mutation_index: u32::MAX,
                    },
                    patch: ToolUpdatePatch {
                        status: Some(ToolStatusFact {
                            value: ToolStatus::Unknown,
                            basis: StatusBasis::SourceFinalization,
                        }),
                        ..ToolUpdatePatch::default()
                    },
                    at: None,
                    src_cursor: Some(finalization.cursor_id),
                },
            )?;
        }

        let invalidated = source_keys.iter().flat_map(|source_key| {
            self.sources
                .get(source_key)
                .expect("selected source exists")
                .invalidated
                .iter()
                .cloned()
        });
        let targets = openers
            .keys()
            .cloned()
            .chain(native_updates.iter().map(|(key, _)| key.clone()))
            .chain(invalidated)
            .collect::<BTreeSet<_>>();
        let existing_openers = load_existing_openers(conn, &targets)?;
        for (key, opener) in &openers {
            if let Some(existing) = existing_openers.get(key)
                && existing.timeline_id != opener.row.timeline_id
            {
                return Err(StoreError::ProjectionInvalid {
                    message: format!(
                        "cold tool opener {} conflicts with existing row {} for {}/{}",
                        opener.row.timeline_id,
                        existing.timeline_id,
                        key.session_id,
                        key.correlation_key
                    ),
                });
            }
        }

        // A runtime normally flushes once and this scan is empty. Retaining
        // previously reduced facts makes a later private append correct too:
        // scan once, fold in RAM once, and update each affected row once.
        let mut all_facts = load_existing_facts(conn, &targets)?;
        for fact in new_facts.values().cloned() {
            insert_fact(&mut all_facts, fact)?;
        }
        let mut facts_by_tool: BTreeMap<ToolKey, Vec<&BufferedFact>> = BTreeMap::new();
        for fact in all_facts.values() {
            facts_by_tool
                .entry(fact.key.clone())
                .or_default()
                .push(fact);
        }
        let mut messages = Vec::with_capacity(openers.len());
        for (key, opener) in &openers {
            let TimelineBody::ToolUse { tool } = &opener.row.body else {
                unreachable!("buffered opener remains a tool row")
            };
            let (resolved, state) = resolve_tool(key, (**tool).clone(), facts_by_tool.get(key))?;
            messages.push(ResolvedToolMessage {
                order: opener.order.clone(),
                row: opener.row.clone(),
                tool: resolved,
                state,
                known_absent: !existing_openers.contains_key(key),
            });
        }
        let mut existing = Vec::new();
        for key in &targets {
            if openers.contains_key(key) {
                continue;
            }
            let Some(opener) = existing_openers.get(key) else {
                continue;
            };
            let (tool, state) = resolve_tool(key, opener.tool.clone(), facts_by_tool.get(key))?;
            existing.push(ResolvedExistingTool {
                message_id: opener.message_id,
                tool,
                state,
            });
        }
        messages.sort_by(|left, right| left.order.cmp(&right.order));
        tail.sort_by(|left, right| left.order.cmp(&right.order));

        // A response-level usage correction targeting a tool row in this same
        // cold plan is part of the row's final value, not a reason to insert
        // and immediately update it. Preserve normal last-writer semantics in
        // the deterministic source order; unrelated response usage stays in
        // the tail and uses the ordinary canonical path.
        let message_indexes = messages
            .iter()
            .enumerate()
            .map(|(index, message)| {
                (
                    (
                        message.row.session_id.clone(),
                        message.row.timeline_id.clone(),
                    ),
                    index,
                )
            })
            .collect::<BTreeMap<_, _>>();
        let pending_usage = load_pending_usage_targets(conn, &message_indexes)?;
        let mut remaining_tail = Vec::with_capacity(tail.len());
        let mut folded_usage = 0_u64;
        for buffered in tail {
            let IngestOp::Usage {
                session_id,
                response,
                ts,
                model,
                tokens,
            } = &buffered.op
            else {
                remaining_tail.push(buffered);
                continue;
            };
            let Some(index) = message_indexes.get(&(session_id.clone(), response.clone())) else {
                remaining_tail.push(buffered);
                continue;
            };
            if pending_usage.contains(&(session_id.clone(), response.clone())) {
                remaining_tail.push(buffered);
                continue;
            }
            let message = &mut messages[*index].row;
            message.usage = *tokens;
            if let Some(model) = model {
                message.model = Some(model.clone());
            }
            if message.at.is_none() {
                message.at = Some(*ts);
            }
            note_session_touch(&mut session_touches, session_id, Some(*ts));
            folded_usage = folded_usage.saturating_add(1);
        }
        let tail = remaining_tail;

        for (key, at) in &native_updates {
            if openers.contains_key(key) || existing_openers.contains_key(key) {
                note_session_touch(&mut session_touches, &key.session_id, *at);
            }
        }
        let matched_updates = native_updates
            .iter()
            .filter(|(key, _)| openers.contains_key(key) || existing_openers.contains_key(key))
            .count() as u64;
        let native_update_count = native_updates.len() as u64;
        let unmatched_updates = native_update_count.saturating_sub(matched_updates);
        Ok(BulkReconciliationPlan {
            source_keys,
            messages,
            existing,
            facts: new_facts.into_values().collect(),
            tail,
            native_updates: native_update_count,
            matched_updates,
            unmatched_updates,
            session_touches,
            folded_usage,
        })
    }

    pub(crate) fn committed(&mut self, plan: &BulkReconciliationPlan) {
        for source_key in &plan.source_keys {
            self.sources.remove(source_key);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ToolKey {
    session_id: String,
    correlation_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct SourceOrder {
    path: String,
    sequence: u64,
}

#[derive(Debug)]
struct BufferedOpener {
    order: SourceOrder,
    row: TimelineEntry,
}

#[derive(Debug)]
struct BufferedFinalization {
    key: ToolKey,
    cursor_id: i64,
    generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct FactKey {
    tool: ToolKey,
    evidence: EvidenceOrder,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BufferedFact {
    key: ToolKey,
    evidence: EvidenceOrder,
    patch: ToolUpdatePatch,
    at: Option<i64>,
    src_cursor: Option<i64>,
}

fn insert_fact(
    facts: &mut BTreeMap<FactKey, BufferedFact>,
    fact: BufferedFact,
) -> Result<(), StoreError> {
    let identity = FactKey {
        tool: fact.key.clone(),
        evidence: fact.evidence.clone(),
    };
    if let Some(previous) = facts.get(&identity) {
        if previous != &fact {
            return Err(StoreError::ProjectionInvalid {
                message: format!(
                    "conflicting cold tool facts share evidence order for {}/{}",
                    identity.tool.session_id, identity.tool.correlation_key
                ),
            });
        }
        return Ok(());
    }
    facts.insert(identity, fact);
    Ok(())
}

fn note_session_touch(touches: &mut BTreeMap<String, i64>, session_id: &str, at: Option<i64>) {
    let Some(at) = at else { return };
    touches
        .entry(session_id.to_string())
        .and_modify(|latest| *latest = (*latest).max(at))
        .or_insert(at);
}

#[derive(Debug)]
struct ExistingToolOpener {
    message_id: i64,
    timeline_id: String,
    tool: ToolInteraction,
}

fn load_existing_openers(
    conn: &Connection,
    targets: &BTreeSet<ToolKey>,
) -> Result<BTreeMap<ToolKey, ExistingToolOpener>, StoreError> {
    if targets.is_empty() {
        return Ok(BTreeMap::new());
    }
    let mut result = BTreeMap::new();
    let mut statement = conn.prepare(
        "SELECT id, timeline_id, session_id, tool_correlation_key, tool_open
         FROM message
         WHERE type = 'tool_use'
         ORDER BY session_id, tool_correlation_key",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
        ))
    })?;
    for row in rows {
        let (message_id, timeline_id, session_id, correlation_key, tool_open) = row?;
        let key = ToolKey {
            session_id,
            correlation_key,
        };
        if !targets.contains(&key) {
            continue;
        }
        let tool: ToolInteraction = serde_json::from_str(&tool_open)?;
        if tool.correlation.key != key.correlation_key {
            return Err(StoreError::ProjectionInvalid {
                message: format!(
                    "stored tool opener {} has correlation {:?}, expected {:?}",
                    timeline_id, tool.correlation.key, key.correlation_key
                ),
            });
        }
        result.insert(
            key,
            ExistingToolOpener {
                message_id,
                timeline_id,
                tool,
            },
        );
    }
    Ok(result)
}

fn load_existing_facts(
    conn: &Connection,
    targets: &BTreeSet<ToolKey>,
) -> Result<BTreeMap<FactKey, BufferedFact>, StoreError> {
    if targets.is_empty() {
        return Ok(BTreeMap::new());
    }
    let mut result = BTreeMap::new();
    let mut statement = conn.prepare(
        "SELECT session_id, correlation_key, source_identity, generation,
                record_offset, mutation_index, patch, at, src_cursor
         FROM tool_update_fact
         ORDER BY session_id, correlation_key, source_identity,
                  generation, record_offset, mutation_index",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, i64>(4)?,
            row.get::<_, i64>(5)?,
            row.get::<_, String>(6)?,
            row.get::<_, Option<i64>>(7)?,
            row.get::<_, Option<i64>>(8)?,
        ))
    })?;
    for row in rows {
        let (
            session_id,
            correlation_key,
            source_identity,
            generation,
            record_offset,
            mutation_index,
            patch,
            at,
            src_cursor,
        ) = row?;
        let key = ToolKey {
            session_id,
            correlation_key,
        };
        if !targets.contains(&key) {
            continue;
        }
        let evidence = EvidenceOrder {
            source_identity: SourceIdentity(source_identity),
            generation: u64::try_from(generation).map_err(|_| StoreError::ProjectionInvalid {
                message: "stored cold tool fact has a negative generation".into(),
            })?,
            record_offset: u64::try_from(record_offset).map_err(|_| {
                StoreError::ProjectionInvalid {
                    message: "stored cold tool fact has a negative record offset".into(),
                }
            })?,
            mutation_index: u32::try_from(mutation_index).map_err(|_| {
                StoreError::ProjectionInvalid {
                    message: "stored cold tool fact has an invalid mutation index".into(),
                }
            })?,
        };
        insert_fact(
            &mut result,
            BufferedFact {
                key,
                evidence,
                patch: serde_json::from_str(&patch)?,
                at,
                src_cursor,
            },
        )?;
    }
    Ok(result)
}

fn load_pending_usage_targets(
    conn: &Connection,
    message_indexes: &BTreeMap<(String, String), usize>,
) -> Result<BTreeSet<(String, String)>, StoreError> {
    if message_indexes.is_empty() {
        return Ok(BTreeSet::new());
    }
    let mut result = BTreeSet::new();
    let mut statement = conn.prepare(
        "SELECT session_id, target
         FROM pending_merge
         WHERE kind = 'usage'
         ORDER BY session_id, target",
    )?;
    for row in statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })? {
        let key = row?;
        if message_indexes.contains_key(&key) {
            result.insert(key);
        }
    }
    Ok(result)
}

fn resolve_tool(
    key: &ToolKey,
    mut tool: ToolInteraction,
    updates: Option<&Vec<&BufferedFact>>,
) -> Result<(ToolInteraction, ToolMergeState), StoreError> {
    tool.validate()
        .map_err(|error| StoreError::ProjectionInvalid {
            message: format!(
                "invalid cold tool opener for {}/{}: {error}",
                key.session_id, key.correlation_key
            ),
        })?;
    let mut state = ToolMergeState::default();
    if let Some(updates) = updates {
        for fact in updates {
            tool.apply_patch(&mut state, &fact.patch, &fact.evidence)
                .map_err(|error| StoreError::ProjectionInvalid {
                    message: format!(
                        "invalid cold tool update for {}/{}: {error}",
                        key.session_id, key.correlation_key
                    ),
                })?;
        }
    }
    Ok((tool, state))
}

#[derive(Debug)]
struct ResolvedToolMessage {
    order: SourceOrder,
    row: TimelineEntry,
    tool: ToolInteraction,
    state: ToolMergeState,
    known_absent: bool,
}

#[derive(Debug)]
struct ResolvedExistingTool {
    message_id: i64,
    tool: ToolInteraction,
    state: ToolMergeState,
}

#[derive(Debug)]
struct BufferedTail {
    cursor_id: i64,
    order: SourceOrder,
    op: IngestOp,
}

#[derive(Debug, Default)]
pub(crate) struct BulkReconciliationPlan {
    source_keys: BTreeSet<SourceKey>,
    messages: Vec<ResolvedToolMessage>,
    existing: Vec<ResolvedExistingTool>,
    facts: Vec<BufferedFact>,
    tail: Vec<BufferedTail>,
    native_updates: u64,
    matched_updates: u64,
    unmatched_updates: u64,
    session_touches: BTreeMap<String, i64>,
    folded_usage: u64,
}

impl BulkReconciliationPlan {
    pub(crate) fn is_empty(&self) -> bool {
        self.source_keys.is_empty()
    }
}

pub(crate) fn apply_plan(
    tx: &Transaction,
    plan: &mut BulkReconciliationPlan,
) -> Result<ApplyStats, StoreError> {
    let mut stats = ApplyStats::default();
    stats.profile.tool_result_attempts = plan.native_updates;
    stats.profile.usage_attempts = plan.folded_usage;
    stats.tool_results_merged = plan.matched_updates;
    stats.tool_results_unmatched = plan.unmatched_updates;
    stats.usage_applied = plan.folded_usage;
    stats.profile.bulk_tool_rows_reconciled =
        plan.messages.len().saturating_add(plan.existing.len()) as u64;
    let started = Instant::now();
    for fact in &plan.facts {
        if persist_tool_fact(
            tx,
            &fact.key.session_id,
            &fact.key.correlation_key,
            &fact.evidence,
            &fact.patch,
            fact.at,
            fact.src_cursor,
        )? {
            stats.profile.bulk_tool_facts_written =
                stats.profile.bulk_tool_facts_written.saturating_add(1);
        }
    }
    stats.profile.tool_fact_write_nanos = elapsed_nanos(started);

    let mut next_ordinals = BTreeMap::new();
    let mut dirty_buckets = BTreeSet::<BucketKey>::new();
    let mut pending_gate = PendingGate::default();
    for message in &mut plan.messages {
        let next = match next_ordinals.get_mut(&message.row.session_id) {
            Some(next) => next,
            None => {
                let ordinal = profile_ordinal_lookup(&mut stats, || {
                    Ok(tx
                        .prepare_cached(
                            "SELECT COALESCE(MAX(ordinal) + 1, 0)
                             FROM message WHERE session_id = ?1",
                        )?
                        .query_row(params![message.row.session_id], |row| row.get::<_, i64>(0))?)
                })?;
                next_ordinals.insert(message.row.session_id.clone(), ordinal);
                next_ordinals
                    .get_mut(&message.row.session_id)
                    .expect("inserted ordinal cursor")
            }
        };
        message.row.ordinal = *next;
        *next = next
            .checked_add(1)
            .ok_or_else(|| StoreError::ProjectionInvalid {
                message: format!(
                    "session ordinal overflowed during cold tool write for {}",
                    message.row.session_id
                ),
            })?;
        apply_reconciled_tool_message(
            tx,
            &message.row,
            &message.tool,
            &message.state,
            message.known_absent,
            &mut stats,
            BULK_MAINTENANCE,
            &mut dirty_buckets,
            &mut pending_gate,
        )?;
    }
    for existing in &plan.existing {
        update_reconciled_tool(
            tx,
            existing.message_id,
            &existing.tool,
            &existing.state,
            &mut stats,
        )?;
    }
    for tail in &plan.tail {
        apply_op(
            tx,
            &tail.op,
            Some(tail.cursor_id),
            &mut stats,
            BULK_MAINTENANCE,
            &mut dirty_buckets,
            &mut pending_gate,
            None,
        )?;
    }
    for (session_id, at) in &plan.session_touches {
        touch_session(tx, session_id, Some(*at), &mut stats)?;
    }
    debug_assert!(dirty_buckets.is_empty(), "cold maintenance defers buckets");
    Ok(stats)
}
