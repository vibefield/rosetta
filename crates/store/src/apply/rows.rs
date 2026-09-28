//! Canonical row upserts plus usage-bucket maintenance coupled to message
//! mutations. Legacy in-file FTS helpers remain only for projection-format
//! compatibility tests; production search is a detached projection repaired
//! from `search_dirty`.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use rusqlite::{OptionalExtension, Transaction, params};

use rosetta_core::fold::{HOUR_MS, hour_bucket};
use rosetta_core::rows::{Ms, ServiceTier, TokenSnapshot};
use rosetta_core::timeline::StatusBasis;
use rosetta_core::{
    EvidenceOrder, IngestOp, SourceIdentity, TimelineBody, TimelineEntry, ToolFoldState,
    ToolInteraction, ToolStatus, ToolStatusFact, ToolUpdatePatch,
};

use crate::StoreError;
use crate::hot::{Effects, Mutation};
use crate::{message_codec, tool_codec};

use super::{ApplyStats, elapsed_nanos};

/// Derived structures maintained by this apply. A private cold rebuild turns
/// both off and constructs them once before publishing the projection.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Maintenance {
    pub buckets: bool,
}

pub(crate) const LIVE_MAINTENANCE: Maintenance = Maintenance { buckets: true };

pub(crate) const BULK_MAINTENANCE: Maintenance = Maintenance { buckets: false };

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

/// Existing canonical row state needed for replacement, usage, and a direct
/// update of the resolved user-facing tool lifecycle.
pub(super) struct OldRow {
    pub(super) id: i64,
    pub(super) timeline_id: String,
    pub(super) session_id: String,
    pub(super) tool: Option<ToolInteraction>,
    pub(super) model: Option<String>,
    pub(super) ts: Option<Ms>,
}

fn load_old_by_timeline(tx: &Transaction, timeline_id: &str) -> Result<Option<OldRow>, StoreError> {
    load_old(
        tx,
        "SELECT id, timeline_id, session_id, model, ts,
                tool_correlation_key, tool_correlation_kind, tool_native_id,
                tool_namespace, tool_provider, tool_name, tool_kind, tool_status,
                tool_status_basis, tool_detail, tool_error_kind,
                tool_error_summary, tool_diagnostic, tool_started_at,
                tool_completed_at, tool_duration_ms
         FROM message WHERE timeline_id = ?1",
        params![timeline_id],
    )
}

/// What a usage snapshot needs of its response row: its identity, owner, and
/// the model and timestamp the snapshot may only fill in. Every one of these
/// columns precedes `content`, so the read never walks a row's overflow
/// chain, and nothing tool-shaped is decoded.
struct UsageTarget {
    id: i64,
    session_id: String,
    model: Option<String>,
    ts: Option<Ms>,
}

fn load_usage_target(
    tx: &Transaction,
    timeline_id: &str,
) -> Result<Option<UsageTarget>, StoreError> {
    let target = tx
        .prepare_cached("SELECT id, session_id, model, ts FROM message WHERE timeline_id = ?1")?
        .query_row(params![timeline_id], |row| {
            Ok(UsageTarget {
                id: row.get(0)?,
                session_id: row.get(1)?,
                model: row.get(2)?,
                ts: row.get(3)?,
            })
        })
        .optional()?;
    Ok(target)
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
                tool_status_basis, tool_detail, tool_error_kind,
                tool_error_summary, tool_diagnostic, tool_started_at,
                tool_completed_at, tool_duration_ms
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
            let tool = tool_codec::decode(row)?;
            Ok(OldRow {
                id: row.get(0)?,
                timeline_id: row.get(1)?,
                session_id: row.get(2)?,
                model: row.get(3)?,
                ts: row.get(4)?,
                tool,
            })
        })
        .optional()?;
    Ok(row)
}

fn update_encoded_tool(
    tx: &Transaction,
    id: i64,
    tool: &ToolInteraction,
) -> Result<(), StoreError> {
    let encoded = tool_codec::encode(Some(tool))?;
    tx.prepare_cached(
        "UPDATE message SET
           tool_correlation_key = ?2, tool_correlation_kind = ?3,
           tool_native_id = ?4, tool_namespace = ?5, tool_provider = ?6,
           tool_name = ?7, tool_kind = ?8, tool_status = ?9,
           tool_status_basis = ?10, tool_detail = ?11,
           tool_error_kind = ?12, tool_error_summary = ?13, tool_diagnostic = ?14,
           tool_started_at = ?15, tool_completed_at = ?16, tool_duration_ms = ?17,
           activity_first_at = (SELECT MIN(value) FROM
             (SELECT activity_first_at AS value UNION ALL SELECT ?15 UNION ALL SELECT ?16)),
           activity_last_at = (SELECT MAX(value) FROM
             (SELECT activity_last_at AS value UNION ALL SELECT ?15 UNION ALL SELECT ?16))
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
    stats: &mut ApplyStats,
) -> Result<(), StoreError> {
    let started = Instant::now();
    update_encoded_tool(tx, message_id, tool)?;
    stats.profile.tool_result_write_nanos = stats
        .profile
        .tool_result_write_nanos
        .saturating_add(elapsed_nanos(started));
    Ok(())
}

fn status_rank(basis: StatusBasis) -> u8 {
    match basis {
        StatusBasis::RequestObserved => 1,
        StatusBasis::SourceFinalization => 2,
        StatusBasis::NativeTerminalRecord => 3,
        StatusBasis::NativeState => 4,
        StatusBasis::GroundTruthProtocol => 5,
    }
}

/// Apply one normalized patch to an already-resolved public row. The detailed
/// evidence map is intentionally transaction-local: only the authority carried
/// by the public status survives a process boundary, so the fold state is
/// SEEDED from it — every present status/timing/failure field is assumed to
/// hold the row's winning basis. Without that seed a late cross-batch patch
/// faces a virgin stamp table and clobbers proven fields a one-pass fold
/// would have refused (a rank-100 usage record overwriting a NativeState
/// completion time was the observed shape). Lower-authority evidence may
/// still fill an unresolved opener; it may not erase stronger user-facing
/// truth.
fn apply_projected_patch(
    tool: &mut ToolInteraction,
    patch: &ToolUpdatePatch,
    evidence: &EvidenceOrder,
) -> Result<bool, StoreError> {
    let mut patch = patch.clone();
    if patch
        .status
        .is_some_and(|incoming| status_rank(incoming.basis) < status_rank(tool.status.basis))
    {
        patch.status = None;
    }
    let before = tool.clone();
    let mut transient = ToolFoldState::assume_resolved(tool);
    tool.apply_patch(&mut transient, &patch, evidence)
        .map_err(|error| StoreError::ProjectionInvalid {
            message: format!("invalid normalized tool update: {error}"),
        })?;
    tool.action.compact_command_preview();
    Ok(*tool != before)
}

/// Close unresolved openers from a completed source without retaining a
/// synthetic fact. This updates only the public status; a stronger native or
/// protocol status already on the row wins in RAM and is left untouched.
pub(super) fn finalize_source_tools(
    tx: &Transaction,
    cursor_id: i64,
    generation: u64,
) -> Result<usize, StoreError> {
    let path: String = tx
        .prepare_cached("SELECT path FROM ingest_cursor WHERE id = ?1")?
        .query_row(params![cursor_id], |row| row.get(0))?;
    let evidence = EvidenceOrder {
        source_identity: SourceIdentity(format!("{path}#source-finalization")),
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
        let Some(old) = load_old_by_tool(tx, session_id, correlation_key)? else {
            continue;
        };
        let mut tool = old.tool.ok_or_else(|| StoreError::ProjectionInvalid {
            message: format!("tool row {} has no public interaction", old.timeline_id),
        })?;
        if apply_projected_patch(&mut tool, &patch, &evidence)? {
            update_encoded_tool(tx, old.id, &tool)?;
        }
    }
    Ok(targets.len())
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
        .prepare_cached(&crate::usage_sql::REBUILD_ONE)?
        .query_row(params![HOUR_MS, session_id, hour_ts, model], |row| {
            Ok((
                BucketValues {
                    input: row.get(0)?,
                    output: row.get(1)?,
                    cache_read: row.get(2)?,
                    cache_write: row.get(3)?,
                    thinking: row.get(4)?,
                    cache_write_1h: row.get(5)?,
                    web_searches: row.get(6)?,
                    cost_pusd: row.get(7)?,
                    billed_pusd: row.get(8)?,
                    unpriced_tokens: row.get(9)?,
                    unrecorded_output: row.get(10)?,
                    messages: row.get::<_, Option<i64>>(11)?.unwrap_or(0),
                    tools: row.get::<_, Option<i64>>(12)?.unwrap_or(0),
                },
                row.get::<_, i64>(13)?,
            ))
        })?;
    let (values, contributors) = aggregate;
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
            cache_w_tokens, think_tokens, cache_w1h_tokens, web_searches, cost_pusd,
            billed_pusd, unpriced_tokens, unrecorded_output, message_count, tool_call_count)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
         ON CONFLICT(session_id, hour_ts, model) DO UPDATE SET
           in_tokens = excluded.in_tokens, out_tokens = excluded.out_tokens,
           cache_r_tokens = excluded.cache_r_tokens,
           cache_w_tokens = excluded.cache_w_tokens,
           think_tokens = excluded.think_tokens,
           cache_w1h_tokens = excluded.cache_w1h_tokens,
           web_searches = excluded.web_searches,
           cost_pusd = excluded.cost_pusd,
           billed_pusd = excluded.billed_pusd,
           unpriced_tokens = excluded.unpriced_tokens,
           unrecorded_output = excluded.unrecorded_output,
           message_count = excluded.message_count,
           tool_call_count = excluded.tool_call_count",
    )?
    .execute(params![
        session_id,
        hour_ts,
        model,
        values.input,
        values.output,
        values.cache_read,
        values.cache_write,
        values.thinking,
        values.cache_write_1h,
        values.web_searches,
        values.cost_pusd,
        values.billed_pusd,
        values.unpriced_tokens,
        values.unrecorded_output,
        values.messages,
        values.tools,
    ])?;
    Ok(())
}

/// One `usage_bucket` row's values, as [`crate::usage_sql::REBUILD_ONE`]
/// aggregates them.
struct BucketValues {
    input: Option<i64>,
    output: Option<i64>,
    cache_read: Option<i64>,
    cache_write: Option<i64>,
    thinking: Option<i64>,
    cache_write_1h: Option<i64>,
    web_searches: Option<i64>,
    cost_pusd: Option<i64>,
    billed_pusd: Option<i64>,
    unpriced_tokens: i64,
    unrecorded_output: i64,
    messages: i64,
    tools: i64,
}

pub(super) fn touch_session(
    tx: &Transaction,
    session_id: &str,
    ts: Option<Ms>,
    stats: &mut ApplyStats,
) -> Result<(), StoreError> {
    if let Some(ts) = ts {
        if let Some(touches) = &mut stats.session_touches {
            if let Some(latest) = touches.get_mut(session_id) {
                *latest = (*latest).max(ts);
            } else {
                touches.insert(session_id.to_owned(), ts);
            }
            return Ok(());
        }
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

pub(super) fn flush_session_touches(
    tx: &Transaction,
    stats: &mut ApplyStats,
) -> Result<(), StoreError> {
    let started = Instant::now();
    if let Some(touches) = stats.session_touches.take() {
        for (session_id, at) in touches {
            touch_session(tx, &session_id, Some(at), stats)?;
        }
    }
    stats.canonical_apply_nanos += elapsed_nanos(started);
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
    effects: Option<&mut Effects>,
) -> Result<(), StoreError> {
    apply_message_inner(
        tx,
        row,
        false,
        true,
        None,
        stats,
        maintenance,
        dirty_buckets,
        effects,
    )
}

/// Insert a cold-build tool row after its complete lifecycle was folded in
/// memory. Only the resolved, user-facing interaction crosses into SQLite.
#[allow(clippy::too_many_arguments)]
pub(super) fn apply_reconciled_tool_message(
    tx: &Transaction,
    row: &TimelineEntry,
    known_absent: bool,
    activity: Option<(Ms, Ms)>,
    stats: &mut ApplyStats,
    maintenance: Maintenance,
    dirty_buckets: &mut BTreeSet<BucketKey>,
) -> Result<(), StoreError> {
    apply_message_inner(
        tx,
        row,
        known_absent,
        false,
        activity,
        stats,
        maintenance,
        dirty_buckets,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn apply_message_inner(
    tx: &Transaction,
    row: &TimelineEntry,
    known_absent: bool,
    touch_message_session: bool,
    activity: Option<(Ms, Ms)>,
    stats: &mut ApplyStats,
    maintenance: Maintenance,
    dirty_buckets: &mut BTreeSet<BucketKey>,
    effects: Option<&mut Effects>,
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
    let new_usage = &row.usage;
    let incoming_tool = match &row.body {
        TimelineBody::ToolUse { tool } => Some(tool.as_ref()),
        _ => None,
    };
    if let (Some(previous), Some(incoming)) = (
        old.as_ref().and_then(|row| row.tool.as_ref()),
        incoming_tool,
    ) && previous.correlation.key != incoming.correlation.key
    {
        return Err(StoreError::ProjectionInvalid {
            message: format!(
                "timeline row changed tool correlation from {:?} to {:?}",
                previous.correlation.key, incoming.correlation.key
            ),
        });
    }
    // Prose and already resolved cold tools can be serialized by reference.
    // Only the ordinary tool path needs an owned row for defensive command
    // compaction; the private reconciler already compacted its final row.
    let mut emitted = Cow::Borrowed(row);
    if touch_message_session
        && incoming_tool.is_some()
        && let TimelineBody::ToolUse { tool } = &mut emitted.to_mut().body
    {
        tool.action.compact_command_preview();
    }
    let tool = match &emitted.body {
        TimelineBody::ToolUse { tool } => Some(tool.as_ref()),
        _ => None,
    };
    let encoded = tool_codec::encode(tool)?;
    let message = message_codec::encode(&emitted)?;
    let (mut activity_first, mut activity_last) = crate::activity::message_bounds(&emitted);
    if let Some((first, last)) = activity {
        activity_first = Some(activity_first.map_or(first, |at| at.min(first)));
        activity_last = Some(activity_last.map_or(last, |at| at.max(last)));
    }
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
                       tool_status_basis = ?26, tool_detail = ?27,
                       tool_error_kind = ?28, tool_error_summary = ?29, tool_diagnostic = ?30,
                       tool_started_at = ?31, tool_completed_at = ?32, tool_duration_ms = ?33,
                       parent_tool_native_id = ?34,
                       in_tokens = CASE WHEN ?52 THEN in_tokens ELSE ?35 END,
                       out_tokens = CASE WHEN ?52 THEN out_tokens ELSE ?36 END,
                       cache_r_tokens = CASE WHEN ?52 THEN cache_r_tokens ELSE ?37 END,
                       cache_w_tokens = CASE WHEN ?52 THEN cache_w_tokens ELSE ?38 END,
                       think_tokens = CASE WHEN ?52 THEN think_tokens ELSE ?39 END,
                       agent_version = ?40, src_cursor = ?41, src_offset = ?42,
                       cache_w1h_tokens = CASE WHEN ?52 THEN cache_w1h_tokens ELSE ?45 END,
                       web_searches = CASE WHEN ?52 THEN web_searches ELSE ?46 END,
                       service_tier = CASE WHEN ?52 THEN service_tier ELSE ?47 END,
                       route = CASE WHEN ?52 THEN route ELSE ?48 END,
                       billed_pusd = CASE WHEN ?52 THEN billed_pusd ELSE ?49 END,
                       usage_scope = CASE WHEN ?52 THEN usage_scope ELSE ?50 END,
                       requests = CASE WHEN ?52 THEN requests ELSE ?51 END,
                       activity_first_at = CASE WHEN ?10 = 'notice' THEN NULL ELSE
                         (SELECT MIN(value) FROM (SELECT activity_first_at AS value UNION ALL SELECT ?43)) END,
                       activity_last_at = CASE WHEN ?10 = 'notice' THEN NULL ELSE
                         (SELECT MAX(value) FROM (SELECT activity_last_at AS value UNION ALL SELECT ?44)) END
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
                    activity_first,
                    activity_last,
                    new_usage.cache_write_1h,
                    new_usage.web_searches,
                    new_usage.service_tier.map(ServiceTier::as_str),
                    new_usage.route,
                    new_usage.billed_pico_usd,
                    message_codec::message_scope_column(new_usage.scope),
                    new_usage.requests,
                    // A re-emitted row that carries no usage (a replayed record;
                    // a row whose usage travels as a separate `Usage` op) keeps
                    // the usage it already has, never wipes it.
                    new_usage.is_empty(),
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
                        tool_status, tool_status_basis, tool_detail,
                        tool_error_kind, tool_error_summary, tool_diagnostic, tool_started_at,
                        tool_completed_at, tool_duration_ms, parent_tool_native_id, in_tokens,
                        out_tokens, cache_r_tokens, cache_w_tokens, think_tokens, agent_version,
                        src_cursor, src_offset, activity_first_at, activity_last_at,
                        cache_w1h_tokens, web_searches, service_tier, route, billed_pusd,
                        usage_scope, requests)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                             ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26,
                             ?27, ?28, ?29, ?30, ?31, ?32, ?33, ?34, ?35, ?36, ?37, ?38,
                             ?39, ?40, ?41, ?42, ?43, ?44, ?45, ?46, ?47, ?48, ?49, ?50,
                             ?51)",
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
                    activity_first,
                    activity_last,
                    new_usage.cache_write_1h,
                    new_usage.web_searches,
                    new_usage.service_tier.map(ServiceTier::as_str),
                    new_usage.route,
                    new_usage.billed_pico_usd,
                    message_codec::message_scope_column(new_usage.scope),
                    new_usage.requests,
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
    if let Some(fx) = effects {
        let mut live = emitted.into_owned();
        live.cost = crate::pricing::usage_cost(live.model.as_deref(), live.at, &live.usage);
        fx.push(Mutation::Message(Box::new(live)));
    }
    Ok(())
}

/// Fold every normalized lifecycle fragment in this transaction in RAM, then
/// write each affected user-facing tool row at most once. Unmatched fragments
/// are counted and left only in the raw ledger; SQLite is not a waiting room.
pub(super) fn apply_tool_updates(
    tx: &Transaction,
    updates: &[&IngestOp],
    stats: &mut ApplyStats,
    mut effects: Option<&mut Effects>,
) -> Result<(), StoreError> {
    type ToolKey = (String, String);
    type PatchRef<'a> = (&'a EvidenceOrder, &'a ToolUpdatePatch, Option<Ms>);
    let mut grouped: BTreeMap<ToolKey, Vec<PatchRef<'_>>> = BTreeMap::new();
    for op in updates {
        let IngestOp::ToolUpdate {
            session_id,
            correlation_key,
            evidence,
            patch,
            at,
        } = op
        else {
            continue;
        };
        stats.profile.tool_result_attempts += 1;
        grouped
            .entry((session_id.clone(), correlation_key.clone()))
            .or_default()
            .push((evidence, patch, *at));
    }

    for ((session_id, correlation_key), patches) in grouped {
        let mut ordered = BTreeMap::<EvidenceOrder, (&ToolUpdatePatch, Option<Ms>)>::new();
        for (evidence, patch, at) in patches {
            if let Some((previous, previous_at)) = ordered.get(evidence) {
                if *previous != patch || *previous_at != at {
                    return Err(StoreError::ProjectionInvalid {
                        message: format!(
                            "conflicting RAM tool updates share evidence order for {session_id}/{correlation_key}"
                        ),
                    });
                }
            } else {
                ordered.insert(evidence.clone(), (patch, at));
            }
        }
        let attempts = u64::try_from(ordered.len()).unwrap_or(u64::MAX);
        let started = Instant::now();
        let loaded = load_old_by_tool(tx, &session_id, &correlation_key);
        stats.profile.tool_result_lookup_nanos += elapsed_nanos(started);
        let Some(old) = loaded? else {
            stats.tool_results_unmatched = stats.tool_results_unmatched.saturating_add(attempts);
            continue;
        };
        let mut tool = old
            .tool
            .clone()
            .ok_or_else(|| StoreError::ProjectionInvalid {
                message: format!(
                    "correlation key {correlation_key:?} resolved to non-tool row {}",
                    old.timeline_id
                ),
            })?;
        let before = tool.clone();
        // Seed from the loaded row: its present fields carry the winning
        // status basis, so a later batch's weaker evidence cannot clobber
        // what an uninterrupted fold would have refused (see
        // apply_projected_patch).
        let mut merge_state = ToolFoldState::assume_resolved(&tool);
        let mut earliest_at = None;
        let mut latest_at = None;
        for (evidence, (patch, at)) in ordered {
            let mut patch = patch.clone();
            if patch.status.is_some_and(|incoming| {
                status_rank(incoming.basis) < status_rank(tool.status.basis)
            }) {
                patch.status = None;
            }
            tool.apply_patch(&mut merge_state, &patch, &evidence)
                .map_err(|error| StoreError::ProjectionInvalid {
                    message: format!(
                        "invalid normalized tool update for {session_id}/{correlation_key}: {error}"
                    ),
                })?;
            if let Some(at) = at {
                earliest_at = Some(earliest_at.map_or(at, |current: Ms| current.min(at)));
                latest_at = Some(latest_at.map_or(at, |current: Ms| current.max(at)));
            }
        }
        tool.action.compact_command_preview();
        let started = Instant::now();
        if tool != before {
            update_encoded_tool(tx, old.id, &tool)?;
            if let Some(fx) = effects.as_deref_mut() {
                fx.push(Mutation::ToolUpdated {
                    session_id: session_id.clone(),
                    timeline_id: old.timeline_id.clone(),
                    tool: Box::new(tool),
                    at: latest_at,
                });
            }
        }
        for at in earliest_at
            .into_iter()
            .chain(latest_at)
            .collect::<BTreeSet<_>>()
        {
            crate::activity::touch_tool(tx, &session_id, &correlation_key, at)?;
        }
        stats.profile.tool_result_write_nanos += elapsed_nanos(started);
        touch_session(tx, &session_id, latest_at, stats)?;
        stats.tool_results_merged = stats.tool_results_merged.saturating_add(attempts);
    }
    Ok(())
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
    let loaded = load_usage_target(tx, response);
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
                    cache_w_tokens = ?5, think_tokens = ?6, model = ?7, ts = ?8,
                    cache_w1h_tokens = ?9, web_searches = ?10, service_tier = ?11,
                    route = ?12, billed_pusd = ?13, usage_scope = ?14, requests = ?15
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
            tokens.cache_write_1h,
            tokens.web_searches,
            tokens.service_tier.map(ServiceTier::as_str),
            tokens.route,
            tokens.billed_pico_usd,
            message_codec::message_scope_column(tokens.scope),
            tokens.requests,
        ]);
    stats.profile.usage_write_nanos += elapsed_nanos(started);
    written?;
    touch_session(tx, session_id, Some(ts), stats)?;
    stats.usage_applied += 1;
    if let Some(fx) = effects {
        let cost = crate::pricing::usage_cost(next_model.as_deref(), next_ts, tokens);
        fx.push(Mutation::Usage {
            session_id: session_id.to_string(),
            timeline_id: response.to_owned(),
            ts: next_ts,
            model: next_model,
            tokens: Box::new(tokens.clone()),
            cost,
        });
    }
    Ok(true)
}
