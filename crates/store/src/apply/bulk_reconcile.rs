//! One-shot relational reconciliation for an unpublished cold projection.
//!
//! A private rebuild already has the raw ledger as its durable authority and
//! cannot be observed before validation. Re-reading and rewriting a tool row
//! for every lifecycle fragment therefore buys no correctness. This buffer is
//! generation-aware: committed source batches contribute closed canonical ops;
//! once every source for a runtime is sealed, all correlations are folded in
//! RAM and each final tool row is written exactly once. The resolved row is the
//! only lifecycle representation written to SQLite; source replacement heals
//! affected sessions by replaying their raw-ledger sources.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{Connection, Transaction, params};

use rosetta_core::{
    EvidenceOrder, IngestOp, TimelineBody, TimelineEntry, ToolFoldState, ToolInteraction,
    ToolUpdatePatch,
};

use crate::StoreError;

use super::rows::{
    BULK_MAINTENANCE, BucketKey, apply_reconciled_tool_message, apply_usage, flush_session_touches,
    touch_session, update_reconciled_tool,
};
use super::{ApplyStats, Batch, SourceMode, SourceWrite, apply_op, profile_ordinal_lookup};

mod fold;
mod resolved;

pub(crate) use fold::ColdFold;
pub use resolved::{ResolvedSession, WithheldFacts};

#[derive(Debug, Clone)]
enum DeferredOp {
    ToolMessage(Box<TimelineEntry>),
    ToolUpdate {
        session_id: String,
        correlation_key: String,
        evidence: EvidenceOrder,
        patch: Box<ToolUpdatePatch>,
        at: Option<i64>,
    },
    Tail(Box<IngestOp>),
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
}

impl BulkReconciliationDelta {
    pub(crate) fn from_batch(
        batch: &Batch,
        _conn: &Connection,
        _capture_replacement: bool,
    ) -> Result<Option<Self>, StoreError> {
        let Some(source) = batch.source.as_ref() else {
            return Ok(None);
        };
        Self::from_ops(
            source,
            batch.ops.iter().map(Cow::Borrowed),
            true,
            &mut Vec::new(),
        )
        .map(Some)
    }

    /// A closed source's delta, which never carries retry identity: a closed
    /// generation is applied exactly once — a repeated cursor is refused
    /// within one transaction and fails its checkpoint compare-and-set in any
    /// later one — so hashing every deferred op for a retry that cannot
    /// happen would only serialize the runtime's relational payload twice.
    /// The rows SQL has consumed are freed off the calling thread.
    pub(crate) fn from_owned_batch(batch: Batch) -> Result<Option<Self>, StoreError> {
        let Some(source) = batch.source.as_ref() else {
            return Ok(None);
        };
        let mut consumed = Vec::new();
        let delta = Self::from_ops(
            source,
            batch.ops.into_iter().map(Cow::Owned),
            false,
            &mut consumed,
        );
        crate::reclaim::release(consumed);
        delta.map(Some)
    }

    fn from_ops<'a>(
        source: &SourceWrite,
        ops: impl ExactSizeIterator<Item = Cow<'a, IngestOp>>,
        fingerprint: bool,
        consumed: &mut Vec<IngestOp>,
    ) -> Result<Self, StoreError> {
        let activation_end = source
            .first_sequence
            .checked_add(
                u64::try_from(ops.len()).map_err(|_| StoreError::SourceState {
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
        for (index, op) in ops.enumerate() {
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
            if !defers(op.as_ref()) {
                if fingerprint {
                    fingerprints.insert(sequence, OpFingerprint::Immediate);
                }
                if let Cow::Owned(op) = op {
                    consumed.push(op);
                }
                continue;
            }
            // Retry identity is computed before taking ownership, so a
            // retryable batch keeps exactly the same source-position checks.
            if fingerprint {
                fingerprints.insert(
                    sequence,
                    OpFingerprint::Deferred(
                        *blake3::hash(&serde_json::to_vec(op.as_ref())?).as_bytes(),
                    ),
                );
            }
            deferred_ops.insert(sequence, deferred_op(op.into_owned()));
        }
        Ok(Self {
            cursor_id: source.cursor_id,
            generation: source.generation,
            mode: source.mode,
            activation_end,
            fingerprints,
            deferred: deferred_ops,
        })
    }
}

fn deferred_op(op: IngestOp) -> DeferredOp {
    match op {
        IngestOp::Message(row) => DeferredOp::ToolMessage(row),
        IngestOp::ToolUpdate {
            session_id,
            correlation_key,
            evidence,
            patch,
            at,
        } => DeferredOp::ToolUpdate {
            session_id,
            correlation_key,
            evidence,
            patch,
            at,
        },
        op @ (IngestOp::Usage { .. } | IngestOp::FileEdit(_)) => DeferredOp::Tail(Box::new(op)),
        _ => unreachable!("defers admits only relational operations"),
    }
}

pub(crate) fn defers(op: &IngestOp) -> bool {
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
    /// Usage snapshots whose response row has not appeared yet. This is the
    /// cold-build counterpart to the Store's live RAM waiting room.
    pending_usage: BTreeMap<UsageKey, BufferedTail>,
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

        if delta.mode == SourceMode::Activate {
            self.pending_usage
                .retain(|_, pending| pending.cursor_id != delta.cursor_id);
        }

        // A newer incomplete generation supersedes only another incomplete
        // attempt. The last activated generation remains available until the
        // replacement's Activate transaction commits; Abandon therefore falls
        // back to it exactly like the canonical projection does.
        if matches!(delta.mode, SourceMode::Stage | SourceMode::Activate) {
            // Only generations of this cursor can be superseded. Scanning
            // every previously admitted source makes a closed runtime O(n²).
            let obsolete = self
                .sources
                .range(
                    SourceKey {
                        cursor_id: delta.cursor_id,
                        generation: 0,
                    }..=SourceKey {
                        cursor_id: delta.cursor_id,
                        generation: u64::MAX,
                    },
                )
                .filter(|(candidate, source)| {
                    candidate.generation != delta.generation
                        && (delta.mode == SourceMode::Activate || source.activation_end.is_none())
                })
                .map(|(candidate, _)| *candidate)
                .collect::<Vec<_>>();
            for candidate in obsolete {
                self.sources.remove(&candidate);
            }
        }
        let source = match self.sources.entry(key) {
            // A first delta for this generation moves its maps in whole
            // instead of re-inserting every position.
            std::collections::btree_map::Entry::Vacant(vacant) => vacant.insert(BufferedSource {
                fingerprints: delta.fingerprints,
                deferred: delta.deferred,
                ..BufferedSource::new(delta.mode)
            }),
            std::collections::btree_map::Entry::Occupied(occupied) => {
                let source = occupied.into_mut();
                for (sequence, fingerprint) in delta.fingerprints {
                    source.fingerprints.entry(sequence).or_insert(fingerprint);
                }
                for (sequence, op) in delta.deferred {
                    source.deferred.entry(sequence).or_insert(op);
                }
                source
            }
        };
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

        let state = fold::collect_borrowed(source_keys.iter().map(|key| {
            let source = self.sources.get(key).expect("selected source exists");
            (
                *key,
                source.activation_end.expect("selected source is activated"),
                paths
                    .get(&key.cursor_id)
                    .expect("selected source path exists")
                    .as_str(),
                &source.deferred,
            )
        }))?;
        let prior_usage = self.pending_usage.values().cloned().collect::<Vec<_>>();
        fold::finish_borrowed(state, prior_usage, conn)
    }

    pub(crate) fn committed(&mut self, plan: &BulkReconciliationPlan) {
        for source_key in &plan.source_keys {
            self.sources.remove(source_key);
        }
        for key in &plan.usage_seen {
            self.pending_usage.remove(key);
        }
        for (key, pending) in &plan.unmatched_usage {
            self.pending_usage.insert(key.clone(), pending.clone());
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
        "SELECT id, timeline_id, session_id,
                tool_correlation_key, tool_correlation_kind, tool_native_id,
                tool_namespace, tool_provider, tool_name, tool_kind, tool_status,
                tool_status_basis, tool_detail, tool_error_kind,
                tool_error_summary, tool_diagnostic, tool_started_at,
                tool_completed_at, tool_duration_ms
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
            crate::tool_codec::decode(row)?,
        ))
    })?;
    for row in rows {
        let (message_id, timeline_id, session_id, correlation_key, tool) = row?;
        let key = ToolKey {
            session_id,
            correlation_key,
        };
        if !targets.contains(&key) {
            continue;
        }
        let tool = tool.ok_or_else(|| StoreError::ProjectionInvalid {
            message: format!("stored tool row {timeline_id} has no public interaction"),
        })?;
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

fn resolve_tool<'f>(
    key: &ToolKey,
    tool: &mut ToolInteraction,
    updates: impl IntoIterator<Item = (&'f EvidenceOrder, &'f ToolUpdatePatch)>,
) -> Result<(), StoreError> {
    tool.validate()
        .map_err(|error| StoreError::ProjectionInvalid {
            message: format!(
                "invalid cold tool opener for {}/{}: {error}",
                key.session_id, key.correlation_key
            ),
        })?;
    let mut state = ToolFoldState::default();
    for (evidence, patch) in updates {
        tool.apply_patch(&mut state, patch, evidence)
            .map_err(|error| StoreError::ProjectionInvalid {
                message: format!(
                    "invalid cold tool update for {}/{}: {error}",
                    key.session_id, key.correlation_key
                ),
            })?;
    }
    Ok(())
}

#[derive(Debug)]
struct ResolvedToolMessage {
    order: SourceOrder,
    row: TimelineEntry,
    known_absent: bool,
    activity: Option<(i64, i64)>,
}

#[derive(Debug)]
struct ResolvedExistingTool {
    message_id: i64,
    tool: ToolInteraction,
}

#[derive(Debug, Clone)]
struct BufferedTail {
    cursor_id: i64,
    order: SourceOrder,
    op: IngestOp,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct UsageKey {
    session_id: String,
    response: String,
}

#[derive(Debug, Default)]
pub(crate) struct BulkReconciliationPlan {
    source_keys: BTreeSet<SourceKey>,
    messages: Vec<ResolvedToolMessage>,
    existing: Vec<ResolvedExistingTool>,
    tail: Vec<BufferedTail>,
    native_updates: u64,
    matched_updates: u64,
    unmatched_updates: u64,
    session_touches: BTreeMap<String, i64>,
    tool_activity: BTreeMap<ToolKey, (i64, i64)>,
    folded_usage: u64,
    usage_seen: BTreeSet<UsageKey>,
    unmatched_usage: BTreeMap<UsageKey, BufferedTail>,
}

impl BulkReconciliationPlan {
    pub(crate) fn is_empty(&self) -> bool {
        self.source_keys.is_empty()
    }
}

pub(crate) fn apply_plan(
    tx: &Transaction,
    plan: &mut BulkReconciliationPlan,
    preserve_message_ordinals: bool,
    owners: &mut super::UsageOwners,
) -> Result<ApplyStats, StoreError> {
    let mut stats = ApplyStats {
        session_touches: Some(BTreeMap::new()),
        ..ApplyStats::default()
    };
    stats.profile.tool_result_attempts = plan.native_updates;
    stats.profile.usage_attempts = plan.folded_usage;
    stats.tool_results_merged = plan.matched_updates;
    stats.tool_results_unmatched = plan.unmatched_updates;
    stats.usage_applied = plan.folded_usage;
    stats.profile.bulk_tool_rows_reconciled =
        plan.messages.len().saturating_add(plan.existing.len()) as u64;

    let mut next_ordinals = BTreeMap::new();
    let mut dirty_buckets = BTreeSet::<BucketKey>::new();
    for message in &mut plan.messages {
        if !preserve_message_ordinals {
            let next = match next_ordinals.get_mut(&message.row.session_id) {
                Some(next) => next,
                None => {
                    let ordinal = profile_ordinal_lookup(&mut stats, || {
                        Ok(tx
                            .prepare_cached(
                                "SELECT COALESCE(MAX(ordinal) + 1, 0)
                                 FROM message WHERE session_id = ?1",
                            )?
                            .query_row(params![message.row.session_id], |row| {
                                row.get::<_, i64>(0)
                            })?)
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
        }
        apply_reconciled_tool_message(
            tx,
            &message.row,
            message.known_absent,
            message.activity,
            &mut stats,
            BULK_MAINTENANCE,
            &mut dirty_buckets,
        )?;
    }
    for existing in &plan.existing {
        update_reconciled_tool(tx, existing.message_id, &existing.tool, &mut stats)?;
    }
    for tail in &plan.tail {
        if let IngestOp::Usage {
            session_id,
            response,
            ts,
            model,
            tokens,
            identity,
        } = &tail.op
        {
            let key = UsageKey {
                session_id: session_id.clone(),
                response: response.clone(),
            };
            plan.usage_seen.insert(key.clone());
            if let Some(identity) = identity
                && !super::claim_usage(
                    tx,
                    owners,
                    identity,
                    session_id,
                    super::OwnerTarget::Row(response.clone()),
                    !tokens.is_empty(),
                    &mut stats,
                    BULK_MAINTENANCE,
                    &mut dirty_buckets,
                    None,
                )?
            {
                continue;
            }
            if !apply_usage(
                tx,
                session_id,
                response,
                *ts,
                model.as_deref(),
                tokens,
                &mut stats,
                BULK_MAINTENANCE,
                &mut dirty_buckets,
                None,
            )? {
                plan.unmatched_usage.insert(key, tail.clone());
            }
            continue;
        }
        apply_op(
            tx,
            &tail.op,
            Some(tail.cursor_id),
            &mut stats,
            BULK_MAINTENANCE,
            &mut dirty_buckets,
            owners,
            None,
        )?;
    }
    for (session_id, at) in &plan.session_touches {
        touch_session(tx, session_id, Some(*at), &mut stats)?;
    }
    for (key, (first, last)) in &plan.tool_activity {
        crate::activity::touch_tool(tx, &key.session_id, &key.correlation_key, *first)?;
        if first != last {
            crate::activity::touch_tool(tx, &key.session_id, &key.correlation_key, *last)?;
        }
    }
    flush_session_touches(tx, &mut stats)?;
    debug_assert!(dirty_buckets.is_empty(), "cold maintenance defers buckets");
    Ok(stats)
}
