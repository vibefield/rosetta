//! Canonical row upserts plus usage-bucket maintenance coupled to message
//! mutations. Legacy in-file FTS helpers remain only for projection-format
//! compatibility tests; production search is a detached projection repaired
//! from `search_dirty`.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use rusqlite::{OptionalExtension, Transaction, params};

use rosetta_core::fold::{HOUR_MS, hour_bucket};
use rosetta_core::rows::{Ms, TokenSnapshot};
use rosetta_core::timeline::StatusBasis;
use rosetta_core::{
    EvidenceOrder, IngestOp, SourceIdentity, TimelineBody, TimelineEntry, ToolInteraction,
    ToolMergeState, ToolStatus, ToolStatusFact, ToolUpdatePatch,
};

use crate::StoreError;
use crate::hot::{Effects, Mutation};
use crate::{message_codec, tool_codec};

use super::{ApplyStats, apply_op, elapsed_nanos};

/// Derived structures maintained by this apply. A private cold rebuild turns
/// both off and constructs them once before publishing the projection.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Maintenance {
    pub buckets: bool,
}

pub(crate) const LIVE_MAINTENANCE: Maintenance = Maintenance { buckets: true };

pub(crate) const BULK_MAINTENANCE: Maintenance = Maintenance { buckets: false };

#[derive(Debug, Default)]
pub(super) struct PendingGate {
    sessions: BTreeMap<String, PendingSession>,
}

#[derive(Debug, Default)]
struct PendingSession {
    loaded: bool,
    usage: BTreeSet<String>,
}

impl PendingSession {
    fn matches(&self, row: &TimelineEntry) -> bool {
        self.usage.contains(&row.timeline_id)
    }

    fn insert(&mut self, kind: &str, target: &str) {
        if kind == "usage" {
            self.usage.insert(target.to_string());
        }
    }

    fn remove(&mut self, kind: &str, target: &str) {
        if kind == "usage" {
            self.usage.remove(target);
        }
    }
}

impl PendingGate {
    fn should_lookup(
        &mut self,
        tx: &Transaction,
        row: &TimelineEntry,
        stats: &mut ApplyStats,
    ) -> Result<bool, StoreError> {
        if let Some(session) = self.sessions.get(&row.session_id) {
            if session.matches(row) {
                return Ok(true);
            }
            if session.loaded {
                return Ok(false);
            }
        }

        stats.profile.pending_gate_checks += 1;
        let started = Instant::now();
        let loaded = (|| -> Result<Vec<(String, String)>, StoreError> {
            let mut stmt = tx.prepare_cached(
                "SELECT kind, target
                 FROM pending_merge
                 WHERE kind = 'usage' AND session_id = ?1",
            )?;
            Ok(stmt
                .query_map([&row.session_id], |record| {
                    Ok((record.get(0)?, record.get(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()?)
        })();
        stats.profile.pending_gate_nanos += elapsed_nanos(started);
        let loaded = loaded?;
        let session = self.sessions.entry(row.session_id.clone()).or_default();
        for (kind, target) in loaded {
            session.insert(&kind, &target);
        }
        session.loaded = true;
        Ok(session.matches(row))
    }

    fn note_queued(&mut self, kind: &str, session_id: &str, target: &str) {
        self.sessions
            .entry(session_id.to_string())
            .or_default()
            .insert(kind, target);
    }

    fn note_drained(&mut self, kind: &str, session_id: &str, target: &str) {
        if let Some(session) = self.sessions.get_mut(session_id) {
            session.remove(kind, target);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct BucketKey {
    pub(super) session_id: String,
    pub(super) hour_ts: Ms,
    pub(super) model: String,
}

pub(super) fn mark_bucket(
    dirty: &mut BTreeSet<BucketKey>,
    maintenance: Maintenance,
    session_id: &str,
    ts: Option<Ms>,
    model: Option<&str>,
) {
    if maintenance.buckets {
        dirty.insert(BucketKey {
            session_id: session_id.to_string(),
            hour_ts: hour_bucket(ts.unwrap_or(0)),
            model: bucket_model(model).to_string(),
        });
    }
}

/// Existing canonical row state needed for replacement, usage, and typed
/// lifecycle merge operations.
pub(super) struct OldRow {
    pub(super) id: i64,
    pub(super) timeline_id: String,
    pub(super) session_id: String,
    pub(super) tool: Option<ToolInteraction>,
    pub(super) tool_open: Option<ToolInteraction>,
    pub(super) tool_merge_state: Option<ToolMergeState>,
    pub(super) model: Option<String>,
    pub(super) ts: Option<Ms>,
}

fn load_old_by_timeline(tx: &Transaction, timeline_id: &str) -> Result<Option<OldRow>, StoreError> {
    load_old(
        tx,
        "SELECT id, timeline_id, session_id, model, ts,
                tool_correlation_key, tool_correlation_kind, tool_native_id,
                tool_namespace, tool_provider, tool_name, tool_kind, tool_status,
                tool_status_basis, tool_detail, tool_merge_state, tool_error_kind,
                tool_error_summary, tool_diagnostic, tool_started_at,
                tool_completed_at, tool_duration_ms, tool_open
         FROM message WHERE timeline_id = ?1",
        params![timeline_id],
    )
}

fn load_old_by_tool(
    tx: &Transaction,
    session_id: &str,
    correlation_key: &str,
) -> Result<Option<OldRow>, StoreError> {
    load_old(
        tx,
        "SELECT id, timeline_id, session_id, model, ts,
                tool_correlation_key, tool_correlation_kind, tool_native_id,
                tool_namespace, tool_provider, tool_name, tool_kind, tool_status,
                tool_status_basis, tool_detail, tool_merge_state, tool_error_kind,
                tool_error_summary, tool_diagnostic, tool_started_at,
                tool_completed_at, tool_duration_ms, tool_open
         FROM message
         WHERE session_id = ?1 AND tool_correlation_key = ?2 AND type = 'tool_use'",
        params![session_id, correlation_key],
    )
}

fn load_old(
    tx: &Transaction,
    sql: &str,
    params: impl rusqlite::Params,
) -> Result<Option<OldRow>, StoreError> {
    let row = tx
        .prepare_cached(sql)?
        .query_row(params, |row| {
            let (tool, tool_merge_state) = tool_codec::decode(row)?;
            let tool_open = tool_codec::decode_open(row)?;
            Ok(OldRow {
                id: row.get(0)?,
                timeline_id: row.get(1)?,
                session_id: row.get(2)?,
                model: row.get(3)?,
                ts: row.get(4)?,
                tool,
                tool_open,
                tool_merge_state,
            })
        })
        .optional()?;
    Ok(row)
}

fn sqlite_u64(value: u64, field: &str) -> Result<i64, StoreError> {
    i64::try_from(value).map_err(|_| StoreError::ProjectionInvalid {
        message: format!("tool evidence {field} exceeds SQLite's integer range"),
    })
}

fn load_tool_facts(
    tx: &Transaction,
    session_id: &str,
    correlation_key: &str,
) -> Result<Vec<(EvidenceOrder, ToolUpdatePatch)>, StoreError> {
    let mut statement = tx.prepare_cached(
        "SELECT source_identity, generation, record_offset, mutation_index, patch
         FROM tool_update_fact
         WHERE session_id = ?1 AND correlation_key = ?2
         ORDER BY source_identity, generation, record_offset, mutation_index",
    )?;
    let rows = statement.query_map(params![session_id, correlation_key], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, String>(4)?,
        ))
    })?;
    let mut facts = Vec::new();
    for row in rows {
        let (source_identity, generation, record_offset, mutation_index, patch) = row?;
        facts.push((
            EvidenceOrder {
                source_identity: SourceIdentity(source_identity),
                generation: u64::try_from(generation).map_err(|_| {
                    StoreError::ProjectionInvalid {
                        message: "negative tool evidence generation".into(),
                    }
                })?,
                record_offset: u64::try_from(record_offset).map_err(|_| {
                    StoreError::ProjectionInvalid {
                        message: "negative tool evidence record offset".into(),
                    }
                })?,
                mutation_index: u32::try_from(mutation_index).map_err(|_| {
                    StoreError::ProjectionInvalid {
                        message: "invalid tool evidence mutation index".into(),
                    }
                })?,
            },
            serde_json::from_str(&patch)?,
        ));
    }
    Ok(facts)
}

fn fold_tool_facts(
    tx: &Transaction,
    session_id: &str,
    mut tool: ToolInteraction,
) -> Result<(ToolInteraction, ToolMergeState, usize), StoreError> {
    tool.validate()
        .map_err(|error| StoreError::ProjectionInvalid {
            message: format!("invalid tool opener: {error}"),
        })?;
    let mut state = ToolMergeState::default();
    let facts = load_tool_facts(tx, session_id, &tool.correlation.key)?;
    let fact_count = facts.len();
    for (evidence, patch) in facts {
        tool.apply_patch(&mut state, &patch, &evidence)
            .map_err(|error| StoreError::ProjectionInvalid {
                message: format!("invalid persisted tool update: {error}"),
            })?;
    }
    Ok((tool, state, fact_count))
}

pub(super) fn persist_tool_fact(
    tx: &Transaction,
    session_id: &str,
    correlation_key: &str,
    evidence: &EvidenceOrder,
    patch: &ToolUpdatePatch,
    at: Option<Ms>,
    src_cursor: Option<i64>,
) -> Result<bool, StoreError> {
    let generation = sqlite_u64(evidence.generation, "generation")?;
    let record_offset = sqlite_u64(evidence.record_offset, "record offset")?;
    let mutation_index = i64::from(evidence.mutation_index);
    let patch = serde_json::to_string(patch)?;
    let inserted = tx
        .prepare_cached(
            "INSERT INTO tool_update_fact
               (session_id, correlation_key, source_identity, generation,
                record_offset, mutation_index, patch, at, src_cursor)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT DO NOTHING",
        )?
        .execute(params![
            session_id,
            correlation_key,
            evidence.source_identity.0,
            generation,
            record_offset,
            mutation_index,
            patch,
            at,
            src_cursor,
        ])?;
    if inserted == 0 {
        let existing: (String, Option<Ms>, Option<i64>) = tx
            .prepare_cached(
                "SELECT patch, at, src_cursor FROM tool_update_fact
             WHERE session_id = ?1 AND correlation_key = ?2 AND source_identity = ?3
               AND generation = ?4 AND record_offset = ?5 AND mutation_index = ?6",
            )?
            .query_row(
                params![
                    session_id,
                    correlation_key,
                    evidence.source_identity.0,
                    generation,
                    record_offset,
                    mutation_index,
                ],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
        if existing != (patch, at, src_cursor) {
            return Err(StoreError::ProjectionInvalid {
                message: format!(
                    "conflicting tool facts share evidence order for {session_id}/{correlation_key}"
                ),
            });
        }
    }
    Ok(inserted == 1)
}

fn update_encoded_tool(
    tx: &Transaction,
    id: i64,
    tool: &ToolInteraction,
    merge_state: &ToolMergeState,
) -> Result<(), StoreError> {
    let encoded = tool_codec::encode(Some(tool), Some(merge_state))?;
    tx.prepare_cached(
        "UPDATE message SET
           tool_correlation_key = ?2, tool_correlation_kind = ?3,
           tool_native_id = ?4, tool_namespace = ?5, tool_provider = ?6,
           tool_name = ?7, tool_kind = ?8, tool_status = ?9,
           tool_status_basis = ?10, tool_detail = ?11, tool_merge_state = ?12,
           tool_error_kind = ?13, tool_error_summary = ?14, tool_diagnostic = ?15,
           tool_started_at = ?16, tool_completed_at = ?17, tool_duration_ms = ?18
         WHERE id = ?1",
    )?
    .execute(params![
        id,
        encoded.correlation_key,
        encoded.correlation_kind,
        encoded.native_id,
        encoded.namespace,
        encoded.provider,
        encoded.name,
        encoded.kind,
        encoded.status,
        encoded.status_basis,
        encoded.detail,
        encoded.merge_state,
        encoded.error_kind,
        encoded.error_summary,
        encoded.diagnostic,
        encoded.started_at,
        encoded.completed_at,
        encoded.duration_ms,
    ])?;
    Ok(())
}

/// Apply one already-folded lifecycle to an opener that was emitted by an
/// earlier private-cold reconciliation boundary. This is the bounded fallback
/// for a later append: one update per affected correlation, never one lookup
/// and rewrite per lifecycle fragment.
pub(super) fn update_reconciled_tool(
    tx: &Transaction,
    message_id: i64,
    tool: &ToolInteraction,
    state: &ToolMergeState,
    stats: &mut ApplyStats,
) -> Result<(), StoreError> {
    let started = Instant::now();
    update_encoded_tool(tx, message_id, tool, state)?;
    stats.profile.tool_result_write_nanos = stats
        .profile
        .tool_result_write_nanos
        .saturating_add(elapsed_nanos(started));
    Ok(())
}

/// Add one low-authority terminal fallback for every opener in a completed
/// source generation. The fact is source-owned and therefore disappears on a
/// rewrite; any native state or terminal record outranks it. Retaining the
/// fallback even while a sibling terminal fact is active lets reconciliation
/// return to `unknown`, never to a fabricated pending/success state, if that
/// sibling fact later vanishes.
pub(super) fn finalize_source_tools(
    tx: &Transaction,
    cursor_id: i64,
    generation: u64,
) -> Result<usize, StoreError> {
    let path: String = tx
        .prepare_cached("SELECT path FROM ingest_cursor WHERE id = ?1")?
        .query_row(params![cursor_id], |row| row.get(0))?;
    let source_identity = SourceIdentity(format!("{path}#source-finalization"));
    let evidence = EvidenceOrder {
        source_identity,
        generation,
        record_offset: i64::MAX as u64,
        mutation_index: u32::MAX,
    };
    let patch = ToolUpdatePatch {
        status: Some(ToolStatusFact {
            value: ToolStatus::Unknown,
            basis: StatusBasis::SourceFinalization,
        }),
        ..ToolUpdatePatch::default()
    };
    let targets = {
        let mut statement = tx.prepare_cached(
            "SELECT session_id, tool_correlation_key
             FROM message
             WHERE src_cursor = ?1 AND type = 'tool_use'
             ORDER BY id",
        )?;
        statement
            .query_map(params![cursor_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?
    };
    for (session_id, correlation_key) in &targets {
        persist_tool_fact(
            tx,
            session_id,
            correlation_key,
            &evidence,
            &patch,
            None,
            Some(cursor_id),
        )?;
        let old = load_old_by_tool(tx, session_id, correlation_key)?.ok_or_else(|| {
            StoreError::ProjectionInvalid {
                message: format!(
                    "tool opener vanished during source finalization for {session_id}/{correlation_key}"
                ),
            }
        })?;
        let opener = old.tool_open.ok_or_else(|| StoreError::ProjectionInvalid {
            message: format!(
                "tool row {} is missing its closed opener during source finalization",
                old.timeline_id
            ),
        })?;
        let (tool, merge_state, _) = fold_tool_facts(tx, session_id, opener)?;
        update_encoded_tool(tx, old.id, &tool, &merge_state)?;
    }
    Ok(targets.len())
}

/// Remove one source generation's active typed facts and repair any opener
/// owned by a different source. Rows owned by the swept source are about to be
/// deleted and will fold surviving sibling facts when their replacement opens.
pub(super) fn remove_source_tool_facts(
    tx: &Transaction,
    cursor_id: i64,
    reconcile_survivors: bool,
) -> Result<(), StoreError> {
    let targets = if reconcile_survivors {
        let mut statement = tx.prepare_cached(
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
        let targets = statement
            .query_map(params![cursor_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        targets
    } else {
        Vec::new()
    };
    tx.prepare_cached("DELETE FROM tool_update_fact WHERE src_cursor = ?1")?
        .execute(params![cursor_id])?;
    for (session_id, correlation_key) in targets {
        let Some(old) = load_old_by_tool(tx, &session_id, &correlation_key)? else {
            continue;
        };
        let opener = old.tool_open.ok_or_else(|| StoreError::ProjectionInvalid {
            message: format!(
                "tool row {} is missing its closed opener during source reconciliation",
                old.timeline_id
            ),
        })?;
        let (tool, merge_state, _) = fold_tool_facts(tx, &session_id, opener)?;
        update_encoded_tool(tx, old.id, &tool, &merge_state)?;
    }
    Ok(())
}

/// Recompute one bucket from its source rows. Replacement can change hour,
/// model, kind, or reported-component presence; recomputation makes all of
/// those moves exact and lets SQL SUM restore NULL when nobody reports a
/// component.
pub(super) fn rebuild_bucket(
    tx: &Transaction,
    session_id: &str,
    hour_ts: Ms,
    model: &str,
) -> Result<(), StoreError> {
    let aggregate = tx
        .prepare_cached(
            "SELECT SUM(in_tokens), SUM(out_tokens), SUM(cache_r_tokens),
                    SUM(cache_w_tokens), SUM(think_tokens), SUM(message_count),
                    SUM(tool_call_count), COUNT(*)
             FROM (
               SELECT in_tokens, out_tokens, cache_r_tokens, cache_w_tokens,
                      think_tokens, 1 AS message_count,
                      CASE WHEN type = 'tool_use' THEN 1 ELSE 0 END AS tool_call_count
               FROM message
               WHERE session_id = ?1
                 AND COALESCE(ts, 0) >= ?2 AND COALESCE(ts, 0) < ?2 + ?4
                 AND COALESCE(model, 'unknown') = ?3
               UNION ALL
               SELECT in_tokens, out_tokens, cache_r_tokens, cache_w_tokens,
                      think_tokens, 0 AS message_count, 0 AS tool_call_count
               FROM session_usage
               WHERE session_id = ?1
                 AND ts >= ?2 AND ts < ?2 + ?4
                 AND COALESCE(model, 'unknown') = ?3
             )",
        )?
        .query_row(params![session_id, hour_ts, model, HOUR_MS], |row| {
            Ok((
                TokenSnapshot {
                    input: row.get(0)?,
                    output: row.get(1)?,
                    cache_read: row.get(2)?,
                    cache_write: row.get(3)?,
                    thinking: row.get(4)?,
                },
                row.get::<_, Option<i64>>(5)?.unwrap_or(0),
                row.get::<_, Option<i64>>(6)?.unwrap_or(0),
                row.get::<_, i64>(7)?,
            ))
        })?;
    let (tokens, messages, tools, contributors) = aggregate;
    if contributors == 0 {
        tx.prepare_cached(
            "DELETE FROM usage_bucket WHERE session_id = ?1 AND hour_ts = ?2 AND model = ?3",
        )?
        .execute(params![session_id, hour_ts, model])?;
        return Ok(());
    }
    tx.prepare_cached(
        "INSERT INTO usage_bucket
           (session_id, hour_ts, model, in_tokens, out_tokens, cache_r_tokens,
            cache_w_tokens, think_tokens, message_count, tool_call_count)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(session_id, hour_ts, model) DO UPDATE SET
           in_tokens = excluded.in_tokens, out_tokens = excluded.out_tokens,
           cache_r_tokens = excluded.cache_r_tokens,
           cache_w_tokens = excluded.cache_w_tokens,
           think_tokens = excluded.think_tokens,
           message_count = excluded.message_count,
           tool_call_count = excluded.tool_call_count",
    )?
    .execute(params![
        session_id,
        hour_ts,
        model,
        tokens.input,
        tokens.output,
        tokens.cache_read,
        tokens.cache_write,
        tokens.thinking,
        messages,
        tools,
    ])?;
    Ok(())
}

pub(super) fn touch_session(
    tx: &Transaction,
    session_id: &str,
    ts: Option<Ms>,
    stats: &mut ApplyStats,
) -> Result<(), StoreError> {
    if let Some(ts) = ts {
        // Meta ingest usually starts from the source file's mtime, which is
        // newer than its records. Avoid a no-op row write for every message.
        stats.profile.session_touch_attempts += 1;
        let started = Instant::now();
        let changed = tx
            .prepare_cached(
                "UPDATE session SET updated_at = ?2
                 WHERE id = ?1 AND updated_at < ?2",
            )?
            .execute(params![session_id, ts]);
        stats.profile.session_touch_nanos += elapsed_nanos(started);
        stats.profile.session_touch_rows += changed? as u64;
    }
    Ok(())
}

fn bucket_model(model: Option<&str>) -> &str {
    model.unwrap_or("unknown")
}

#[allow(clippy::too_many_arguments)]
pub(super) fn apply_message(
    tx: &Transaction,
    row: &TimelineEntry,
    stats: &mut ApplyStats,
    maintenance: Maintenance,
    dirty_buckets: &mut BTreeSet<BucketKey>,
    pending_gate: &mut PendingGate,
    effects: Option<&mut Effects>,
) -> Result<(), StoreError> {
    apply_message_inner(
        tx,
        row,
        None,
        false,
        true,
        stats,
        maintenance,
        dirty_buckets,
        pending_gate,
        effects,
    )
}

/// Insert a cold-build tool opener after its complete lifecycle was folded in
/// memory. `row` remains the original opener so `tool_open` retains the exact
/// source-owned base needed by future generation reconciliation; only the
/// public/current tool value and its private field stamps are precomputed.
#[allow(clippy::too_many_arguments)]
pub(super) fn apply_reconciled_tool_message(
    tx: &Transaction,
    row: &TimelineEntry,
    resolved_tool: &ToolInteraction,
    resolved_state: &ToolMergeState,
    known_absent: bool,
    stats: &mut ApplyStats,
    maintenance: Maintenance,
    dirty_buckets: &mut BTreeSet<BucketKey>,
    pending_gate: &mut PendingGate,
) -> Result<(), StoreError> {
    apply_message_inner(
        tx,
        row,
        Some((resolved_tool, resolved_state)),
        known_absent,
        false,
        stats,
        maintenance,
        dirty_buckets,
        pending_gate,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn apply_message_inner(
    tx: &Transaction,
    row: &TimelineEntry,
    reconciled_tool: Option<(&ToolInteraction, &ToolMergeState)>,
    known_absent: bool,
    touch_message_session: bool,
    stats: &mut ApplyStats,
    maintenance: Maintenance,
    dirty_buckets: &mut BTreeSet<BucketKey>,
    pending_gate: &mut PendingGate,
    mut effects: Option<&mut Effects>,
) -> Result<(), StoreError> {
    stats.profile.message_attempts += 1;
    let old = if known_absent {
        None
    } else {
        let started = Instant::now();
        let loaded = load_old_by_timeline(tx, &row.timeline_id);
        stats.profile.message_lookup_nanos += elapsed_nanos(started);
        loaded?
    };
    if let Some(old) = old.as_ref() {
        mark_bucket(
            dirty_buckets,
            maintenance,
            &old.session_id,
            old.ts,
            old.model.as_deref(),
        );
    }
    mark_bucket(
        dirty_buckets,
        maintenance,
        &row.session_id,
        row.at,
        row.model.as_deref(),
    );
    let new_usage = row.usage;
    let incoming_tool = match &row.body {
        TimelineBody::ToolUse { tool } => Some(tool.as_ref()),
        _ => None,
    };
    let (tool, tool_merge_state, folded_tool_facts) = match reconciled_tool {
        Some((resolved, state)) => {
            let opener = incoming_tool.ok_or_else(|| StoreError::ProjectionInvalid {
                message: format!(
                    "pre-reconciled tool row {} does not contain a tool opener",
                    row.timeline_id
                ),
            })?;
            if opener.correlation != resolved.correlation {
                return Err(StoreError::ProjectionInvalid {
                    message: format!(
                        "pre-reconciled tool correlation changed for row {}",
                        row.timeline_id
                    ),
                });
            }
            (Some(resolved.clone()), Some(state.clone()), 0)
        }
        None => resolved_open_tool(tx, &row.session_id, incoming_tool, old.as_ref())?,
    };
    stats.tool_results_merged = stats
        .tool_results_merged
        .saturating_add(u64::try_from(folded_tool_facts).unwrap_or(u64::MAX));
    let tool_open = incoming_tool.map(serde_json::to_string).transpose()?;
    let encoded = tool_codec::encode(tool.as_ref(), tool_merge_state.as_ref())?;
    let mut emitted = row.clone();
    if let Some(tool) = tool {
        emitted.body = TimelineBody::ToolUse {
            tool: Box::new(tool),
        };
    }
    let message = message_codec::encode(&emitted)?;
    let response_id = emitted
        .response
        .as_ref()
        .map(|response| response.id.as_str());
    let response_derivation = emitted
        .response
        .as_ref()
        .map(|response| message_codec::derivation_name(response.derivation));
    let block_index = emitted.block_index.map(i64::from);

    let started = Instant::now();
    let written = (|| -> Result<(), StoreError> {
        match old {
            Some(_) => {
                tx.prepare_cached(
                    "UPDATE message SET
                       session_id = ?2, ordinal = ?3, group_id = ?4, group_kind = ?5,
                       group_derivation = ?6, response_id = ?7, response_derivation = ?8,
                       block_index = ?9, type = ?10, uuid = ?11, parent_uuid = ?12,
                       ts = ?13, model = ?14, effort = ?15, content = ?16, detail = ?17,
                       tool_correlation_key = ?18, tool_correlation_kind = ?19,
                       tool_native_id = ?20, tool_namespace = ?21, tool_provider = ?22,
                       tool_name = ?23, tool_kind = ?24, tool_status = ?25,
                       tool_status_basis = ?26, tool_detail = ?27, tool_merge_state = ?28,
                       tool_error_kind = ?29, tool_error_summary = ?30, tool_diagnostic = ?31,
                       tool_started_at = ?32, tool_completed_at = ?33, tool_duration_ms = ?34,
                       parent_tool_native_id = ?35, in_tokens = ?36, out_tokens = ?37,
                       cache_r_tokens = ?38, cache_w_tokens = ?39, think_tokens = ?40,
                       agent_version = ?41, src_cursor = ?42, src_offset = ?43,
                       tool_open = ?44
                     WHERE timeline_id = ?1",
                )?
                .execute(params![
                    emitted.timeline_id,
                    emitted.session_id,
                    emitted.ordinal,
                    emitted.group.id,
                    message_codec::group_kind_name(emitted.group.kind),
                    message_codec::derivation_name(emitted.group.derivation),
                    response_id,
                    response_derivation,
                    block_index,
                    message.kind,
                    emitted.native_id,
                    emitted.parent_native_id,
                    emitted.at,
                    emitted.model,
                    emitted.effort,
                    message.content,
                    message.detail,
                    encoded.correlation_key,
                    encoded.correlation_kind,
                    encoded.native_id,
                    encoded.namespace,
                    encoded.provider,
                    encoded.name,
                    encoded.kind,
                    encoded.status,
                    encoded.status_basis,
                    encoded.detail,
                    encoded.merge_state,
                    encoded.error_kind,
                    encoded.error_summary,
                    encoded.diagnostic,
                    encoded.started_at,
                    encoded.completed_at,
                    encoded.duration_ms,
                    emitted.parent_tool_native_id,
                    new_usage.input,
                    new_usage.output,
                    new_usage.cache_read,
                    new_usage.cache_write,
                    new_usage.thinking,
                    emitted.provenance.agent_version,
                    emitted.provenance.src_cursor,
                    emitted.provenance.src_offset,
                    tool_open,
                ])?;
                stats.messages_updated += 1;
            }
            None => {
                tx.prepare_cached(
                    "INSERT INTO message
                       (timeline_id, session_id, ordinal, group_id, group_kind, group_derivation,
                        response_id, response_derivation, block_index, type, uuid, parent_uuid, ts,
                        model, effort, content, detail, tool_correlation_key, tool_correlation_kind,
                        tool_native_id, tool_namespace, tool_provider, tool_name, tool_kind,
                        tool_status, tool_status_basis, tool_detail, tool_merge_state,
                        tool_error_kind, tool_error_summary, tool_diagnostic, tool_started_at,
                        tool_completed_at, tool_duration_ms, parent_tool_native_id, in_tokens,
                        out_tokens, cache_r_tokens, cache_w_tokens, think_tokens, agent_version,
                        src_cursor, src_offset, tool_open)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                             ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26,
                             ?27, ?28, ?29, ?30, ?31, ?32, ?33, ?34, ?35, ?36, ?37, ?38,
                             ?39, ?40, ?41, ?42, ?43, ?44)",
                )?
                .execute(params![
                    emitted.timeline_id,
                    emitted.session_id,
                    emitted.ordinal,
                    emitted.group.id,
                    message_codec::group_kind_name(emitted.group.kind),
                    message_codec::derivation_name(emitted.group.derivation),
                    response_id,
                    response_derivation,
                    block_index,
                    message.kind,
                    emitted.native_id,
                    emitted.parent_native_id,
                    emitted.at,
                    emitted.model,
                    emitted.effort,
                    message.content,
                    message.detail,
                    encoded.correlation_key,
                    encoded.correlation_kind,
                    encoded.native_id,
                    encoded.namespace,
                    encoded.provider,
                    encoded.name,
                    encoded.kind,
                    encoded.status,
                    encoded.status_basis,
                    encoded.detail,
                    encoded.merge_state,
                    encoded.error_kind,
                    encoded.error_summary,
                    encoded.diagnostic,
                    encoded.started_at,
                    encoded.completed_at,
                    encoded.duration_ms,
                    emitted.parent_tool_native_id,
                    new_usage.input,
                    new_usage.output,
                    new_usage.cache_read,
                    new_usage.cache_write,
                    new_usage.thinking,
                    emitted.provenance.agent_version,
                    emitted.provenance.src_cursor,
                    emitted.provenance.src_offset,
                    tool_open,
                ])?;
                stats.messages_inserted += 1;
            }
        }
        Ok(())
    })();
    stats.profile.message_write_nanos += elapsed_nanos(started);
    written?;
    if touch_message_session {
        touch_session(tx, &row.session_id, row.at, stats)?;
    }
    if let Some(fx) = effects.as_deref_mut() {
        fx.push(Mutation::Message(Box::new(emitted.clone())));
    }
    if pending_gate.should_lookup(tx, &emitted, stats)? {
        apply_pending_for_message(
            tx,
            &emitted,
            stats,
            maintenance,
            dirty_buckets,
            pending_gate,
            effects,
        )?;
    } else {
        stats.profile.pending_lookup_skips += 1;
    }
    Ok(())
}

fn resolved_open_tool(
    tx: &Transaction,
    session_id: &str,
    incoming: Option<&ToolInteraction>,
    old: Option<&OldRow>,
) -> Result<(Option<ToolInteraction>, Option<ToolMergeState>, usize), StoreError> {
    let Some(incoming) = incoming else {
        return Ok((None, None, 0));
    };
    if let Some(old) = old
        && let Some(previous) = old.tool_open.as_ref()
        && previous.correlation.key != incoming.correlation.key
    {
        return Err(StoreError::ProjectionInvalid {
            message: format!(
                "timeline row changed tool correlation from {:?} to {:?}",
                previous.correlation.key, incoming.correlation.key
            ),
        });
    }
    let (tool, state, fact_count) = fold_tool_facts(tx, session_id, incoming.clone())?;
    Ok((Some(tool), Some(state), fact_count))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn apply_tool_update(
    tx: &Transaction,
    session_id: &str,
    correlation_key: &str,
    evidence: &EvidenceOrder,
    patch: &ToolUpdatePatch,
    at: Option<Ms>,
    src_cursor: Option<i64>,
    stats: &mut ApplyStats,
    effects: Option<&mut Effects>,
) -> Result<bool, StoreError> {
    stats.profile.tool_result_attempts += 1;
    persist_tool_fact(
        tx,
        session_id,
        correlation_key,
        evidence,
        patch,
        at,
        src_cursor,
    )?;
    let started = Instant::now();
    let loaded = load_old_by_tool(tx, session_id, correlation_key);
    stats.profile.tool_result_lookup_nanos += elapsed_nanos(started);
    let Some(old) = loaded? else {
        stats.tool_results_unmatched += 1;
        return Ok(false);
    };
    let opener = old
        .tool_open
        .clone()
        .ok_or_else(|| StoreError::ProjectionInvalid {
            message: format!(
                "correlation key {correlation_key:?} resolved to non-tool row {}",
                old.timeline_id
            ),
        })?;
    let (tool, merge_state, _) = fold_tool_facts(tx, session_id, opener)?;
    let changed =
        old.tool.as_ref() != Some(&tool) || old.tool_merge_state.as_ref() != Some(&merge_state);
    let started = Instant::now();
    if changed {
        update_encoded_tool(tx, old.id, &tool, &merge_state)?;
        if let Some(fx) = effects {
            fx.push(Mutation::ToolUpdated {
                session_id: session_id.to_string(),
                timeline_id: old.timeline_id.clone(),
                tool: Box::new(tool),
                at,
            });
        }
    }
    stats.profile.tool_result_write_nanos += elapsed_nanos(started);
    touch_session(tx, session_id, at, stats)?;
    stats.tool_results_merged += 1;
    Ok(true)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn apply_usage(
    tx: &Transaction,
    session_id: &str,
    response: &str,
    ts: Ms,
    model: Option<&str>,
    tokens: &TokenSnapshot,
    stats: &mut ApplyStats,
    maintenance: Maintenance,
    dirty_buckets: &mut BTreeSet<BucketKey>,
    effects: Option<&mut Effects>,
) -> Result<bool, StoreError> {
    stats.profile.usage_attempts += 1;
    let started = Instant::now();
    let loaded = load_old_by_timeline(tx, response);
    stats.profile.usage_lookup_nanos += elapsed_nanos(started);
    let Some(old) = loaded? else {
        stats.usage_unmatched += 1;
        return Ok(false);
    };
    let next_model = model.map(str::to_owned).or_else(|| old.model.clone());
    let next_ts = old.ts.or(Some(ts));
    mark_bucket(
        dirty_buckets,
        maintenance,
        &old.session_id,
        old.ts,
        old.model.as_deref(),
    );
    mark_bucket(
        dirty_buckets,
        maintenance,
        session_id,
        next_ts,
        next_model.as_deref(),
    );
    let started = Instant::now();
    let written = tx
        .prepare_cached(
            "UPDATE message SET in_tokens = ?2, out_tokens = ?3, cache_r_tokens = ?4,
                    cache_w_tokens = ?5, think_tokens = ?6, model = ?7, ts = ?8
             WHERE id = ?1",
        )?
        .execute(params![
            old.id,
            tokens.input,
            tokens.output,
            tokens.cache_read,
            tokens.cache_write,
            tokens.thinking,
            next_model,
            next_ts,
        ]);
    stats.profile.usage_write_nanos += elapsed_nanos(started);
    written?;
    touch_session(tx, session_id, Some(ts), stats)?;
    stats.usage_applied += 1;
    if let Some(fx) = effects {
        fx.push(Mutation::Usage {
            session_id: session_id.to_string(),
            timeline_id: old.timeline_id,
            ts: next_ts,
            model: next_model,
            tokens: *tokens,
        });
    }
    Ok(true)
}

fn pending_key(op: &IngestOp) -> Option<(&'static str, &str, &str)> {
    match op {
        IngestOp::Usage {
            session_id,
            response,
            ..
        } => Some(("usage", session_id, response)),
        _ => None,
    }
}

pub(super) fn queue_pending(
    tx: &Transaction,
    op: &IngestOp,
    src_cursor: Option<i64>,
    stats: &mut ApplyStats,
    pending_gate: &mut PendingGate,
) -> Result<(), StoreError> {
    let Some((kind, session_id, target)) = pending_key(op) else {
        return Ok(());
    };
    stats.profile.pending_queues += 1;
    let started = Instant::now();
    let queued = (|| -> Result<(), StoreError> {
        tx.prepare_cached(
            "INSERT INTO pending_merge (kind, session_id, target, op_json, src_cursor)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(kind, session_id, target) DO UPDATE SET
               op_json = excluded.op_json, src_cursor = excluded.src_cursor",
        )?
        .execute(params![
            kind,
            session_id,
            target,
            serde_json::to_string(op)?,
            src_cursor,
        ])?;
        Ok(())
    })();
    stats.profile.pending_queue_nanos += elapsed_nanos(started);
    queued?;
    // A result queued after this batch loaded the session's pending keys must
    // make its target message probe immediately in the same transaction.
    pending_gate.note_queued(kind, session_id, target);
    Ok(())
}

/// A message may arrive after its usage/result in another file or append
/// batch. Drain matching derived state in the same transaction that creates
/// the target so subscribers never observe an intermediate committed state.
fn apply_pending_for_message(
    tx: &Transaction,
    row: &TimelineEntry,
    stats: &mut ApplyStats,
    maintenance: Maintenance,
    dirty_buckets: &mut BTreeSet<BucketKey>,
    pending_gate: &mut PendingGate,
    mut effects: Option<&mut Effects>,
) -> Result<(), StoreError> {
    stats.profile.pending_lookups += 1;
    let started = Instant::now();
    let loaded = (|| -> Result<Vec<_>, StoreError> {
        let mut stmt = tx.prepare_cached(
            "SELECT kind, target, op_json, src_cursor
             FROM pending_merge
             WHERE session_id = ?1
               AND kind = 'usage' AND target = ?2
             ORDER BY kind, target",
        )?;
        let pending = stmt
            .query_map(params![row.session_id, row.timeline_id], |record| {
                Ok((
                    record.get::<_, String>(0)?,
                    record.get::<_, String>(1)?,
                    record.get::<_, String>(2)?,
                    record.get::<_, Option<i64>>(3)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(pending)
    })();
    stats.profile.pending_lookup_nanos += elapsed_nanos(started);
    let pending = loaded?;
    stats.profile.pending_rows += pending.len() as u64;

    for (kind, target, json, src_cursor) in pending {
        tx.prepare_cached(
            "DELETE FROM pending_merge
             WHERE kind = ?1 AND session_id = ?2 AND target = ?3",
        )?
        .execute(params![kind, row.session_id, target])?;
        pending_gate.note_drained(&kind, &row.session_id, &target);
        let op: IngestOp = serde_json::from_str(&json)?;
        apply_op(
            tx,
            &op,
            src_cursor,
            stats,
            maintenance,
            dirty_buckets,
            pending_gate,
            effects.as_deref_mut(),
        )?;
    }
    Ok(())
}
