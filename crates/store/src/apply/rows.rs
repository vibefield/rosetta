//! Canonical row upserts plus usage-bucket maintenance coupled to message
//! mutations. Legacy in-file FTS helpers remain only for projection-format
//! compatibility tests; production search is a detached projection repaired
//! from `search_dirty`.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use rusqlite::{OptionalExtension, Transaction, params};

use rosetta_core::IngestOp;
use rosetta_core::fold::{HOUR_MS, hour_bucket};
use rosetta_core::rows::{MessageRow, Ms, TokenSnapshot};

use crate::StoreError;
use crate::hot::{Effects, Mutation};

use super::metadata::refresh_session_title;
use super::{ApplyStats, apply_op, elapsed_nanos};

/// Derived structures maintained by this apply. A private cold rebuild turns
/// both off and constructs them once before publishing the projection.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Maintenance {
    pub fts: bool,
    pub buckets: bool,
}

pub(crate) const LIVE_MAINTENANCE: Maintenance = Maintenance {
    fts: false,
    buckets: true,
};

pub(crate) const BULK_MAINTENANCE: Maintenance = Maintenance {
    fts: false,
    buckets: false,
};

#[derive(Debug, Default)]
pub(super) struct PendingGate {
    sessions: BTreeMap<String, PendingSession>,
}

#[derive(Debug, Default)]
struct PendingSession {
    loaded: bool,
    usage: BTreeSet<String>,
    tool_results: BTreeSet<String>,
}

impl PendingSession {
    fn matches(&self, row: &MessageRow) -> bool {
        self.usage.contains(&row.timeline_id)
            || row
                .tool
                .as_ref()
                .and_then(|tool| tool.tool_id.as_deref())
                .is_some_and(|tool_id| self.tool_results.contains(tool_id))
    }

    fn insert(&mut self, kind: &str, target: &str) {
        match kind {
            "usage" => {
                self.usage.insert(target.to_string());
            }
            "tool_result" => {
                self.tool_results.insert(target.to_string());
            }
            _ => {}
        }
    }

    fn remove(&mut self, kind: &str, target: &str) {
        match kind {
            "usage" => {
                self.usage.remove(target);
            }
            "tool_result" => {
                self.tool_results.remove(target);
            }
            _ => {}
        }
    }
}

impl PendingGate {
    fn should_lookup(
        &mut self,
        tx: &Transaction,
        row: &MessageRow,
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
                 WHERE kind IN ('tool_result', 'usage') AND session_id = ?1",
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

/// The columns of an existing message row that FTS resync, usage
/// replacement, and hot-tier patch emission need.
pub(super) struct OldRow {
    pub(super) id: i64,
    pub(super) timeline_id: String,
    pub(super) session_id: String,
    pub(super) content: Option<String>,
    pub(super) tool_input: Option<String>,
    pub(super) tool_result: Option<String>,
    pub(super) model: Option<String>,
    pub(super) ts: Option<Ms>,
}

fn load_old_by_timeline(tx: &Transaction, timeline_id: &str) -> Result<Option<OldRow>, StoreError> {
    load_old(
        tx,
        "SELECT id, timeline_id, session_id, content, tool_input, tool_result, model, ts
         FROM message WHERE timeline_id = ?1",
        params![timeline_id],
    )
}

fn load_old_by_tool(
    tx: &Transaction,
    session_id: &str,
    tool_id: &str,
) -> Result<Option<OldRow>, StoreError> {
    load_old(
        tx,
        "SELECT id, timeline_id, session_id, content, tool_input, tool_result, model, ts
         FROM message WHERE session_id = ?1 AND tool_id = ?2",
        params![session_id, tool_id],
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
            Ok(OldRow {
                id: row.get(0)?,
                timeline_id: row.get(1)?,
                session_id: row.get(2)?,
                content: row.get(3)?,
                tool_input: row.get(4)?,
                tool_result: row.get(5)?,
                model: row.get(6)?,
                ts: row.get(7)?,
            })
        })
        .optional()?;
    Ok(row)
}

pub(super) fn fts_delete(tx: &Transaction, old: &OldRow) -> Result<(), StoreError> {
    tx.prepare_cached(
        "INSERT INTO message_fts (message_fts, rowid, content, tool_input, tool_result)
         VALUES ('delete', ?1, ?2, ?3, ?4)",
    )?
    .execute(params![
        old.id,
        old.content,
        old.tool_input,
        old.tool_result
    ])?;
    Ok(())
}

fn fts_insert(
    tx: &Transaction,
    rowid: i64,
    content: Option<&str>,
    tool_input: Option<&str>,
    tool_result: Option<&str>,
) -> Result<(), StoreError> {
    tx.prepare_cached(
        "INSERT INTO message_fts (rowid, content, tool_input, tool_result)
         VALUES (?1, ?2, ?3, ?4)",
    )?
    .execute(params![rowid, content, tool_input, tool_result])?;
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
    row: &MessageRow,
    stats: &mut ApplyStats,
    maintenance: Maintenance,
    dirty_buckets: &mut BTreeSet<BucketKey>,
    pending_gate: &mut PendingGate,
    mut effects: Option<&mut Effects>,
) -> Result<(), StoreError> {
    stats.profile.message_attempts += 1;
    let started = Instant::now();
    let loaded = load_old_by_timeline(tx, &row.timeline_id);
    stats.profile.message_lookup_nanos += elapsed_nanos(started);
    let old = loaded?;
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
        row.ts,
        row.model.as_deref(),
    );
    let new_usage = row.usage.unwrap_or_default();
    let tool_input = row.tool.as_ref().map(|tool| tool.input.to_string());
    let tool_payload = row
        .tool
        .as_ref()
        .and_then(|tool| tool.payload.as_ref().map(|p| p.to_string()));

    let started = Instant::now();
    let written = (|| -> Result<(), StoreError> {
        match old {
            Some(old) => {
                if maintenance.fts {
                    fts_delete(tx, &old)?;
                }
                tx.prepare_cached(
                    "UPDATE message SET
                       session_id = ?2, ordinal = ?3, type = ?4, subtype = ?5, uuid = ?6,
                       parent_uuid = ?7, ts = ?8, model = ?9, effort = ?10, content = ?11,
                       tool_id = ?12, tool_name = ?13, tool_input = ?14, tool_result = ?15,
                       tool_payload = ?16, tool_is_error = ?17, branch_tool_id = ?18,
                       in_tokens = ?19, out_tokens = ?20, cache_r_tokens = ?21,
                       cache_w_tokens = ?22, think_tokens = ?23, agent_version = ?24,
                       src_cursor = ?25, src_offset = ?26
                     WHERE timeline_id = ?1",
                )?
                .execute(params![
                    row.timeline_id,
                    row.session_id,
                    row.ordinal,
                    row.kind.as_str(),
                    row.subtype,
                    row.uuid,
                    row.parent_uuid,
                    row.ts,
                    row.model,
                    row.effort,
                    row.content,
                    row.tool.as_ref().and_then(|t| t.tool_id.as_deref()),
                    row.tool.as_ref().map(|t| t.name.as_str()),
                    tool_input,
                    row.tool.as_ref().and_then(|t| t.result.as_deref()),
                    tool_payload,
                    row.tool.as_ref().and_then(|t| t.is_error),
                    row.branch_tool_id,
                    new_usage.input,
                    new_usage.output,
                    new_usage.cache_read,
                    new_usage.cache_write,
                    new_usage.thinking,
                    row.agent_version,
                    row.src_cursor,
                    row.src_offset,
                ])?;
                if maintenance.fts {
                    fts_insert(
                        tx,
                        old.id,
                        row.content.as_deref(),
                        tool_input.as_deref(),
                        row.tool.as_ref().and_then(|t| t.result.as_deref()),
                    )?;
                }
                stats.messages_updated += 1;
            }
            None => {
                tx.prepare_cached(
                    "INSERT INTO message
                       (timeline_id, session_id, ordinal, type, subtype, uuid, parent_uuid, ts,
                        model, effort, content, tool_id, tool_name, tool_input, tool_result,
                        tool_payload, tool_is_error, branch_tool_id, in_tokens, out_tokens,
                        cache_r_tokens, cache_w_tokens, think_tokens, agent_version,
                        src_cursor, src_offset)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
                             ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26)",
                )?
                .execute(params![
                    row.timeline_id,
                    row.session_id,
                    row.ordinal,
                    row.kind.as_str(),
                    row.subtype,
                    row.uuid,
                    row.parent_uuid,
                    row.ts,
                    row.model,
                    row.effort,
                    row.content,
                    row.tool.as_ref().and_then(|t| t.tool_id.as_deref()),
                    row.tool.as_ref().map(|t| t.name.as_str()),
                    tool_input,
                    row.tool.as_ref().and_then(|t| t.result.as_deref()),
                    tool_payload,
                    row.tool.as_ref().and_then(|t| t.is_error),
                    row.branch_tool_id,
                    new_usage.input,
                    new_usage.output,
                    new_usage.cache_read,
                    new_usage.cache_write,
                    new_usage.thinking,
                    row.agent_version,
                    row.src_cursor,
                    row.src_offset,
                ])?;
                let rowid = tx.last_insert_rowid();
                if maintenance.fts {
                    fts_insert(
                        tx,
                        rowid,
                        row.content.as_deref(),
                        tool_input.as_deref(),
                        row.tool.as_ref().and_then(|t| t.result.as_deref()),
                    )?;
                }
                stats.messages_inserted += 1;
            }
        }
        Ok(())
    })();
    stats.profile.message_write_nanos += elapsed_nanos(started);
    written?;
    if matches!(
        row.subtype.as_deref(),
        Some("ai_title" | "custom_title" | "title_fallback")
    ) {
        refresh_session_title(tx, &row.session_id)?;
    }
    touch_session(tx, &row.session_id, row.ts, stats)?;
    if let Some(fx) = effects.as_deref_mut() {
        fx.push(Mutation::Message(Box::new(row.clone())));
    }
    if pending_gate.should_lookup(tx, row, stats)? {
        apply_pending_for_message(
            tx,
            row,
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

#[allow(clippy::too_many_arguments)]
pub(super) fn apply_tool_result(
    tx: &Transaction,
    session_id: &str,
    tool_id: &str,
    result: Option<&str>,
    payload: Option<&serde_json::Value>,
    is_error: Option<bool>,
    at: Option<Ms>,
    stats: &mut ApplyStats,
    maintenance: Maintenance,
    effects: Option<&mut Effects>,
) -> Result<bool, StoreError> {
    stats.profile.tool_result_attempts += 1;
    let started = Instant::now();
    let loaded = load_old_by_tool(tx, session_id, tool_id);
    stats.profile.tool_result_lookup_nanos += elapsed_nanos(started);
    let Some(old) = loaded? else {
        stats.tool_results_unmatched += 1;
        return Ok(false);
    };
    let started = Instant::now();
    let written = (|| -> Result<(), StoreError> {
        if maintenance.fts {
            fts_delete(tx, &old)?;
        }
        tx.prepare_cached(
            "UPDATE message SET tool_result = ?2, tool_payload = ?3, tool_is_error = ?4
             WHERE id = ?1",
        )?
        .execute(params![
            old.id,
            result,
            payload.map(|p| p.to_string()),
            is_error
        ])?;
        if maintenance.fts {
            fts_insert(
                tx,
                old.id,
                old.content.as_deref(),
                old.tool_input.as_deref(),
                result,
            )?;
        }
        Ok(())
    })();
    stats.profile.tool_result_write_nanos += elapsed_nanos(started);
    written?;
    touch_session(tx, session_id, at, stats)?;
    stats.tool_results_merged += 1;
    if let Some(fx) = effects {
        fx.push(Mutation::ToolResult {
            session_id: session_id.to_string(),
            timeline_id: old.timeline_id,
            result: result.map(str::to_owned),
            payload: payload.cloned(),
            is_error,
            at,
        });
    }
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
        IngestOp::ToolResult {
            session_id,
            tool_id,
            ..
        } => Some(("tool_result", session_id, tool_id)),
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
    row: &MessageRow,
    stats: &mut ApplyStats,
    maintenance: Maintenance,
    dirty_buckets: &mut BTreeSet<BucketKey>,
    pending_gate: &mut PendingGate,
    mut effects: Option<&mut Effects>,
) -> Result<(), StoreError> {
    let tool_id = row.tool.as_ref().and_then(|tool| tool.tool_id.as_deref());
    stats.profile.pending_lookups += 1;
    let started = Instant::now();
    let loaded = (|| -> Result<Vec<_>, StoreError> {
        let mut stmt = tx.prepare_cached(
            "SELECT kind, target, op_json, src_cursor
             FROM pending_merge
             WHERE session_id = ?1
               AND ((kind = 'usage' AND target = ?2)
                 OR (kind = 'tool_result' AND target = ?3))
             ORDER BY kind, target",
        )?;
        let pending = stmt
            .query_map(
                params![row.session_id, row.timeline_id, tool_id],
                |record| {
                    Ok((
                        record.get::<_, String>(0)?,
                        record.get::<_, String>(1)?,
                        record.get::<_, String>(2)?,
                        record.get::<_, Option<i64>>(3)?,
                    ))
                },
            )?
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
