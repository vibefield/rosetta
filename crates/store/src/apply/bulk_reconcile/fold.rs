//! The relational fold behind every cold reconciliation plan, in two steps.
//!
//! `collect_source` gathers one activated source's deferred ops: the winning
//! opener per tool correlation (the latest source position), identity-deduped
//! lifecycle facts grouped by correlation, the source-finalization fact of each
//! of its openers, and its tail. Every step is order-independent, so a
//! retryable flush can collect its whole buffer at once while a streamed
//! fresh-cold runtime collects each closed source as it commits — in the
//! writer's idle time during capture. `finish` then resolves each opener
//! against its facts once and orders the plan; it is the only step left for a
//! streamed runtime's admission.

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::Connection;

use rosetta_core::timeline::StatusBasis;
use rosetta_core::{
    EvidenceOrder, IngestOp, SourceIdentity, TimelineBody, TimelineEntry, ToolStatus,
    ToolStatusFact, ToolUpdatePatch,
};

use crate::StoreError;

use super::{
    BufferedTail, BulkReconciliationDelta, BulkReconciliationPlan, DeferredOp,
    ResolvedExistingTool, ResolvedToolMessage, SourceKey, SourceOrder, ToolKey, UsageKey,
    load_existing_openers, note_session_touch, resolve_tool,
};
use crate::apply::SourceMode;

/// A tool opener row, borrowed from a retryable buffer (copied only if it
/// wins) or owned by a consumed one (moved).
#[derive(Debug)]
pub(super) enum OpenerRow<'a> {
    Borrowed(&'a TimelineEntry),
    Owned(Box<TimelineEntry>),
}

impl std::ops::Deref for OpenerRow<'_> {
    type Target = TimelineEntry;

    fn deref(&self) -> &TimelineEntry {
        match self {
            Self::Borrowed(row) => row,
            Self::Owned(row) => row,
        }
    }
}

impl OpenerRow<'_> {
    fn into_owned(self) -> TimelineEntry {
        match self {
            Self::Borrowed(row) => row.clone(),
            Self::Owned(row) => *row,
        }
    }
}

#[derive(Debug)]
struct BufferedOpener<'a> {
    order: SourceOrder,
    row: OpenerRow<'a>,
}

/// One lifecycle patch. A source-finalization fact is only a marker: its
/// patch is the same for every opener, so it is materialized once per fold.
#[derive(Debug)]
pub(super) enum PatchSlot<'a> {
    Borrowed(&'a ToolUpdatePatch),
    Owned(Box<ToolUpdatePatch>),
    Finalization,
}

impl PatchSlot<'_> {
    fn get<'s>(&'s self, finalization: &'s ToolUpdatePatch) -> &'s ToolUpdatePatch {
        match self {
            Self::Borrowed(patch) => patch,
            Self::Owned(patch) => patch,
            Self::Finalization => finalization,
        }
    }
}

#[derive(Debug)]
struct BufferedFact<'a> {
    patch: PatchSlot<'a>,
    at: Option<i64>,
}

fn finalization_patch() -> ToolUpdatePatch {
    ToolUpdatePatch {
        status: Some(ToolStatusFact {
            value: ToolStatus::Unknown,
            basis: StatusBasis::SourceFinalization,
        }),
        ..ToolUpdatePatch::default()
    }
}

/// A deferred op split into the parts the fold keeps, copying only what a
/// borrowed op cannot give up.
pub(super) enum DeferredParts<'a> {
    Opener(OpenerRow<'a>),
    Update {
        key: ToolKey,
        evidence: EvidenceOrder,
        patch: PatchSlot<'a>,
        at: Option<i64>,
    },
    Tail(Box<IngestOp>),
}

fn split_borrowed(op: &DeferredOp) -> DeferredParts<'_> {
    match op {
        DeferredOp::ToolMessage(row) => DeferredParts::Opener(OpenerRow::Borrowed(row)),
        DeferredOp::ToolUpdate {
            session_id,
            correlation_key,
            evidence,
            patch,
            at,
        } => DeferredParts::Update {
            key: ToolKey {
                session_id: session_id.clone(),
                correlation_key: correlation_key.clone(),
            },
            evidence: evidence.clone(),
            patch: PatchSlot::Borrowed(patch),
            at: *at,
        },
        DeferredOp::Tail(op) => DeferredParts::Tail(op.clone()),
    }
}

pub(super) fn split_owned(op: DeferredOp) -> DeferredParts<'static> {
    match op {
        DeferredOp::ToolMessage(row) => DeferredParts::Opener(OpenerRow::Owned(row)),
        DeferredOp::ToolUpdate {
            session_id,
            correlation_key,
            evidence,
            patch,
            at,
        } => DeferredParts::Update {
            key: ToolKey {
                session_id,
                correlation_key,
            },
            evidence,
            patch: PatchSlot::Owned(patch),
            at,
        },
        DeferredOp::Tail(op) => DeferredParts::Tail(op),
    }
}

/// Collected, not yet resolved, relational facts of some activated sources.
#[derive(Debug, Default)]
pub(super) struct FoldState<'a> {
    source_keys: BTreeSet<SourceKey>,
    openers: BTreeMap<ToolKey, BufferedOpener<'a>>,
    facts: BTreeMap<ToolKey, BTreeMap<EvidenceOrder, BufferedFact<'a>>>,
    tail: Vec<BufferedTail>,
    native_updates: Vec<(ToolKey, Option<i64>)>,
    session_touches: BTreeMap<String, i64>,
}

impl<'a> FoldState<'a> {
    fn insert_fact(
        &mut self,
        key: ToolKey,
        evidence: EvidenceOrder,
        fact: BufferedFact<'a>,
    ) -> Result<(), StoreError> {
        let Some(facts) = self.facts.get_mut(&key) else {
            self.facts.insert(key, BTreeMap::from([(evidence, fact)]));
            return Ok(());
        };
        let Some(previous) = facts.get(&evidence) else {
            facts.insert(evidence, fact);
            return Ok(());
        };
        let finalization = finalization_patch();
        if previous.patch.get(&finalization) != fact.patch.get(&finalization)
            || previous.at != fact.at
        {
            return Err(StoreError::ProjectionInvalid {
                message: format!(
                    "conflicting cold tool facts share evidence order for {}/{}",
                    key.session_id, key.correlation_key
                ),
            });
        }
        Ok(())
    }

    /// Gather one activated source's deferred ops at `path`. `activation_end`
    /// is the end-exclusive sequence its activation sealed (zero for an
    /// append that began already active: no finalization facts).
    pub(super) fn collect_source(
        &mut self,
        source_key: SourceKey,
        activation_end: u64,
        path: &str,
        deferred: impl IntoIterator<Item = (u64, DeferredParts<'a>)>,
    ) -> Result<(), StoreError> {
        self.source_keys.insert(source_key);
        let mut finalized = Vec::new();
        for (sequence, op) in deferred {
            match op {
                DeferredParts::Opener(row) => {
                    let TimelineBody::ToolUse { tool } = &row.body else {
                        unreachable!("only tool rows enter the bulk reconciliation buffer")
                    };
                    let key = ToolKey {
                        session_id: row.session_id.clone(),
                        correlation_key: tool.correlation.key.clone(),
                    };
                    note_session_touch(&mut self.session_touches, &row.session_id, row.at);
                    if activation_end != 0 && sequence < activation_end {
                        finalized.push(key.clone());
                    }
                    let candidate = BufferedOpener {
                        order: SourceOrder {
                            path: path.to_owned(),
                            sequence,
                        },
                        row,
                    };
                    if let Some(previous) = self.openers.get_mut(&key) {
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
                        if previous.order == candidate.order && *previous.row != *candidate.row {
                            return Err(StoreError::ProjectionInvalid {
                                message: format!(
                                    "cold tool opener {} has conflicting payloads at {}:{}",
                                    candidate.row.timeline_id,
                                    candidate.order.path,
                                    candidate.order.sequence
                                ),
                            });
                        }
                        // Claude can repeat one native assistant record on a
                        // later branch with the same UUID/tool ID but a
                        // different parent. Ordinary SQLite upsert semantics
                        // made the later source position authoritative; the
                        // one-shot fold must preserve that exact behavior.
                        if candidate.order > previous.order {
                            *previous = candidate;
                        }
                    } else {
                        self.openers.insert(key, candidate);
                    }
                }
                DeferredParts::Update {
                    key,
                    evidence,
                    patch,
                    at,
                } => {
                    self.native_updates.push((key.clone(), at));
                    self.insert_fact(key, evidence, BufferedFact { patch, at })?;
                }
                DeferredParts::Tail(op) => self.tail.push(BufferedTail {
                    cursor_id: source_key.cursor_id,
                    order: SourceOrder {
                        path: path.to_owned(),
                        sequence,
                    },
                    op: *op,
                }),
            }
        }
        for key in finalized {
            self.insert_fact(
                key,
                EvidenceOrder {
                    source_identity: SourceIdentity(format!("{path}#source-finalization")),
                    generation: source_key.generation,
                    record_offset: i64::MAX as u64,
                    mutation_index: u32::MAX,
                },
                BufferedFact {
                    patch: PatchSlot::Finalization,
                    at: None,
                },
            )?;
        }
        Ok(())
    }

    /// Resolve every collected opener once and order the plan. `existing`
    /// scans stored openers the facts may target — the retryable path —
    /// while a consumed fold instead returns every session its facts address,
    /// for the caller to prove no stored opener can exist there.
    pub(super) fn finish(
        self,
        prior_usage: Vec<BufferedTail>,
        existing: Option<&Connection>,
    ) -> Result<(BulkReconciliationPlan, BTreeSet<String>), StoreError> {
        let FoldState {
            source_keys,
            openers,
            facts,
            tail,
            native_updates,
            mut session_touches,
        } = self;
        if source_keys.is_empty() {
            return Ok((BulkReconciliationPlan::default(), BTreeSet::new()));
        }
        let finalization = finalization_patch();
        let facts_of = |key: &ToolKey| {
            facts.get(key).into_iter().flat_map(|facts| {
                facts
                    .iter()
                    .map(|(evidence, fact)| (evidence, fact.patch.get(&finalization)))
            })
        };

        let mut addressed = BTreeSet::new();
        let (targets, existing_openers) = match existing {
            Some(conn) => {
                let targets = openers
                    .keys()
                    .cloned()
                    .chain(native_updates.iter().map(|(key, _)| key.clone()))
                    .collect::<BTreeSet<_>>();
                let existing_openers = load_existing_openers(conn, &targets)?;
                (targets, existing_openers)
            }
            None => {
                addressed.extend(openers.keys().map(|key| key.session_id.clone()));
                addressed.extend(native_updates.iter().map(|(key, _)| key.session_id.clone()));
                (BTreeSet::new(), BTreeMap::new())
            }
        };
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

        let mut existing = Vec::new();
        for key in &targets {
            if openers.contains_key(key) {
                continue;
            }
            let Some(opener) = existing_openers.get(key) else {
                continue;
            };
            let mut tool = opener.tool.clone();
            resolve_tool(key, &mut tool, facts_of(key))?;
            existing.push(ResolvedExistingTool {
                message_id: opener.message_id,
                tool,
            });
        }

        let mut tool_activity: BTreeMap<ToolKey, (i64, i64)> = BTreeMap::new();
        for (key, at) in &native_updates {
            if openers.contains_key(key) || existing_openers.contains_key(key) {
                note_session_touch(&mut session_touches, &key.session_id, *at);
                if let Some(at) = *at {
                    let bounds = tool_activity.entry(key.clone()).or_insert((at, at));
                    bounds.0 = bounds.0.min(at);
                    bounds.1 = bounds.1.max(at);
                }
            }
        }
        let matched_updates = native_updates
            .iter()
            .filter(|(key, _)| openers.contains_key(key) || existing_openers.contains_key(key))
            .count() as u64;
        let native_update_count = native_updates.len() as u64;
        let unmatched_updates = native_update_count.saturating_sub(matched_updates);

        let mut messages = Vec::with_capacity(openers.len());
        for (key, opener) in openers {
            let mut row = opener.row.into_owned();
            let TimelineBody::ToolUse { tool } = &mut row.body else {
                unreachable!("buffered opener remains a tool row")
            };
            resolve_tool(&key, tool, facts_of(&key))?;
            // This is the owned final row; compact once here so writing it
            // needs no second opener/tool payload copy.
            tool.action.compact_command_preview();
            messages.push(ResolvedToolMessage {
                order: opener.order,
                row,
                known_absent: !existing_openers.contains_key(&key),
                // Put result timestamps into the initial canonical write,
                // instead of updating every inserted tool row once or twice.
                activity: tool_activity.remove(&key),
            });
        }
        drop(facts);
        messages.sort_by(|left, right| left.order.cmp(&right.order));
        let mut tail = tail;
        tail.sort_by(|left, right| left.order.cmp(&right.order));
        // A fragment retained from an earlier committed flush happened before
        // every fragment in this flush, regardless of lexical source path.
        // Preserve that commit order so the newest usage snapshot wins.
        let mut ordered_tail = prior_usage;
        ordered_tail.extend(tail);
        let tail = ordered_tail;

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
        let mut remaining_tail = Vec::with_capacity(tail.len());
        let mut folded_usage = 0_u64;
        let mut usage_seen = BTreeSet::new();
        for buffered in tail {
            let IngestOp::Usage {
                session_id,
                response,
                ts,
                model,
                tokens,
                ..
            } = &buffered.op
            else {
                remaining_tail.push(buffered);
                continue;
            };
            usage_seen.insert(UsageKey {
                session_id: session_id.clone(),
                response: response.clone(),
            });
            let Some(index) = message_indexes.get(&(session_id.clone(), response.clone())) else {
                remaining_tail.push(buffered);
                continue;
            };
            let message = &mut messages[*index].row;
            if message.at.is_none() {
                // Usage may supply the accounting bucket's missing timestamp,
                // but that is not a timestamped conversation record. Use the
                // ordinary usage path after inserting the undated opener so
                // live ingest and cold replay preserve the same activity.
                remaining_tail.push(buffered);
                continue;
            }
            message.usage = tokens.clone();
            if let Some(model) = model {
                message.model = Some(model.clone());
            }
            note_session_touch(&mut session_touches, session_id, Some(*ts));
            folded_usage = folded_usage.saturating_add(1);
        }
        let tail = remaining_tail;

        Ok((
            BulkReconciliationPlan {
                source_keys,
                messages,
                existing,
                tail,
                native_updates: native_update_count,
                matched_updates,
                unmatched_updates,
                session_touches,
                tool_activity,
                folded_usage,
                usage_seen,
                unmatched_usage: BTreeMap::new(),
            },
            addressed,
        ))
    }
}

/// Collect a retryable buffer's activated sources, borrowing every op.
pub(super) fn collect_borrowed<'a>(
    sources: impl IntoIterator<Item = (SourceKey, u64, &'a str, &'a BTreeMap<u64, DeferredOp>)>,
) -> Result<FoldState<'a>, StoreError> {
    let mut state = FoldState::default();
    for (key, activation_end, path, deferred) in sources {
        state.collect_source(
            key,
            activation_end,
            path,
            deferred
                .iter()
                .map(|(sequence, op)| (*sequence, split_borrowed(op))),
        )?;
    }
    Ok(state)
}

pub(super) fn finish_borrowed(
    state: FoldState<'_>,
    prior_usage: Vec<BufferedTail>,
    conn: &Connection,
) -> Result<BulkReconciliationPlan, StoreError> {
    state.finish(prior_usage, Some(conn)).map(|(plan, _)| plan)
}

/// One streamed fresh-cold runtime's fold, collected as each of its closed
/// sources commits instead of being buffered per source and gathered at its
/// admission. It owns every op it keeps; nothing is ever copied.
#[derive(Debug, Default)]
pub(crate) struct ColdFold {
    state: FoldState<'static>,
}

impl ColdFold {
    /// Collect one committed closed source at `path`. An abandoned
    /// generation contributes nothing, exactly as the buffer drops it.
    pub(crate) fn collect(
        &mut self,
        delta: BulkReconciliationDelta,
        path: &str,
    ) -> Result<(), StoreError> {
        let activation_end = match delta.mode {
            SourceMode::Abandon => return Ok(()),
            SourceMode::Activate => delta.activation_end,
            SourceMode::Stage | SourceMode::Append => {
                return Err(StoreError::SourceState {
                    cursor_id: delta.cursor_id,
                    message: "a streamed cold fold admits only terminal closed sources".into(),
                });
            }
        };
        let key = SourceKey {
            cursor_id: delta.cursor_id,
            generation: delta.generation,
        };
        if self.state.source_keys.contains(&key) {
            return Err(StoreError::SourceState {
                cursor_id: delta.cursor_id,
                message: "a streamed cold fold collected one closed source twice".into(),
            });
        }
        self.state.collect_source(
            key,
            activation_end,
            path,
            delta
                .deferred
                .into_iter()
                .map(|(sequence, op)| (sequence, split_owned(op))),
        )
    }

    /// Resolve and order the collected runtime. Also returns every session
    /// the plan's tool facts address: no stored opener may exist there.
    pub(crate) fn finish(self) -> Result<(BulkReconciliationPlan, BTreeSet<String>), StoreError> {
        self.state.finish(Vec::new(), None)
    }
}
