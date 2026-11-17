//! The projection side of cross-file usage ownership (ARCHITECTURE §4.1.3,
//! counting rule 2): apply an identity-bearing usage record only when
//! [`UsageOwners`] says it is the owner's, remember what it wrote, and undo
//! a displaced owner's targets so every non-owner copy ends exactly as if
//! its usage had been dropped on arrival.
//!
//! The undo writes what a dropped copy shows. A row's usage columns go NULL
//! (copied rows carry no inline usage; the usage travels as its own op) and,
//! if the usage had filled in the row's model or time, the prior values come
//! back — but only while the row still holds what the usage wrote, since a
//! later message upsert's values are what the dropped copy would show. A
//! sample row is deleted. One residual stays: `session.updated_at` only ever
//! rises, so a displaced copy keeps the time its usage raised it to. That
//! differs from the dropped copy only when a usage time exceeds every other
//! time the session reports.

use std::collections::BTreeSet;
use std::time::Instant;

use rosetta_core::rows::{Ms, SessionUsageRow, TokenSnapshot};
use rusqlite::{OptionalExtension, Transaction, params};

use crate::StoreError;
use crate::hot::{Effects, Mutation};

use super::rows::{BucketKey, Maintenance, apply_usage, mark_bucket, touch_session};
use super::usage_owners::{Claim, OwnerSlot, RowStamp, Undo, UsageOwners};
use super::{ApplyStats, elapsed_nanos};

/// What became of one row-targeted usage record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RowUsage {
    /// A copy of usage another session owns: nothing was written.
    Dropped,
    Applied,
    /// The response row does not exist (yet).
    Unmatched,
}

/// Apply one `Usage` op: claim its identity, write the row's usage, and
/// remember what the owner wrote.
#[allow(clippy::too_many_arguments)]
pub(super) fn apply_row_usage(
    tx: &Transaction,
    owners: &mut UsageOwners,
    identity: Option<&str>,
    session_id: &str,
    response: &str,
    ts: Ms,
    model: Option<&str>,
    tokens: &TokenSnapshot,
    stats: &mut ApplyStats,
    maintenance: Maintenance,
    dirty_buckets: &mut BTreeSet<BucketKey>,
    mut effects: Option<&mut Effects>,
) -> Result<RowUsage, StoreError> {
    let slot = match identity {
        Some(identity) => {
            let claimed = claim(
                tx,
                owners,
                identity,
                session_id,
                stats,
                maintenance,
                dirty_buckets,
                effects.as_deref_mut(),
            )?;
            let Some(slot) = claimed else {
                return Ok(RowUsage::Dropped);
            };
            Some(slot)
        }
        None => None,
    };
    let Some(write) = apply_usage(
        tx,
        session_id,
        response,
        ts,
        model,
        tokens,
        stats,
        maintenance,
        dirty_buckets,
        effects,
    )?
    else {
        return Ok(RowUsage::Unmatched);
    };
    if let Some(slot) = slot {
        owners.touch_row(slot, response, write.old.as_ref(), write.next.as_ref());
    }
    Ok(RowUsage::Applied)
}

/// Apply one `SessionUsage` op: claim its identity, upsert the sample, and
/// remember it as the owner's.
#[allow(clippy::too_many_arguments)]
pub(super) fn apply_sample_usage(
    tx: &Transaction,
    owners: &mut UsageOwners,
    row: &SessionUsageRow,
    src_cursor: Option<i64>,
    stats: &mut ApplyStats,
    maintenance: Maintenance,
    dirty_buckets: &mut BTreeSet<BucketKey>,
    effects: Option<&mut Effects>,
) -> Result<(), StoreError> {
    let slot = match &row.identity {
        Some(identity) => {
            let claimed = claim(
                tx,
                owners,
                identity,
                &row.session_id,
                stats,
                maintenance,
                dirty_buckets,
                effects,
            )?;
            let Some(slot) = claimed else {
                return Ok(());
            };
            Some(slot)
        }
        None => None,
    };
    if apply_session_usage(tx, row, src_cursor, stats, maintenance, dirty_buckets)?
        && let Some(slot) = slot
    {
        owners.touch_sample(slot, &row.key);
    }
    Ok(())
}

/// Settle who owns a usage record that carries a response identity. Returns
/// the owner's slot when the record should be applied; a displaced owner's
/// targets are undone here.
#[allow(clippy::too_many_arguments)]
fn claim(
    tx: &Transaction,
    owners: &mut UsageOwners,
    identity: &str,
    session_id: &str,
    stats: &mut ApplyStats,
    maintenance: Maintenance,
    dirty_buckets: &mut BTreeSet<BucketKey>,
    mut effects: Option<&mut Effects>,
) -> Result<Option<OwnerSlot>, StoreError> {
    match owners.claim(tx, identity, session_id)? {
        Claim::Own(slot) => {
            stats.usage_identities_owned += 1;
            Ok(Some(slot))
        }
        Claim::DropCopy => {
            stats.usage_copies_dropped += 1;
            Ok(None)
        }
        Claim::Displace {
            slot,
            session_id: displaced,
            undo,
        } => {
            stats.usage_copies_displaced += 1;
            let started = Instant::now();
            let visible = !undo.is_empty() && session_visible(tx, &displaced)?;
            // A displaced owner whose records never matched a row or session
            // wrote nothing, so nothing changes for a reader to see.
            let mut changed = false;
            for target in undo {
                match target {
                    Undo::Row {
                        timeline_id,
                        restore,
                    } => {
                        let undone = undo_row(
                            tx,
                            &timeline_id,
                            restore.as_ref(),
                            maintenance,
                            dirty_buckets,
                            effects.as_deref_mut(),
                        )?;
                        if undone && visible {
                            stats.usage_displaced_visible_rows += 1;
                        }
                        changed |= undone;
                    }
                    Undo::Sample { key } => {
                        changed |= delete_sample(tx, &displaced, &key, maintenance, dirty_buckets)?;
                    }
                }
            }
            stats.profile.usage_write_nanos += elapsed_nanos(started);
            if changed {
                stats.usage_displaced_visible += u64::from(visible);
                stats.usage_displaced_sessions.insert(displaced);
            }
            Ok(Some(slot))
        }
    }
}

/// Whether readers can already see the session: it left Pending, so a
/// displacement changes usage someone may be looking at (decision U-1).
fn session_visible(tx: &Transaction, session_id: &str) -> Result<bool, StoreError> {
    let state: Option<String> = tx
        .prepare_cached("SELECT stream_state FROM session WHERE id = ?1")?
        .query_row(params![session_id], |row| row.get(0))
        .optional()?;
    Ok(state.is_some_and(|state| state != "pending"))
}

/// Leave a displaced owner's row as a dropped copy leaves it. Returns
/// whether the row was there to undo.
fn undo_row(
    tx: &Transaction,
    timeline_id: &str,
    restore: Option<&(RowStamp<String>, RowStamp<String>)>,
    maintenance: Maintenance,
    dirty_buckets: &mut BTreeSet<BucketKey>,
    effects: Option<&mut Effects>,
) -> Result<bool, StoreError> {
    let current = tx
        .prepare_cached("SELECT id, session_id, model, ts FROM message WHERE timeline_id = ?1")?
        .query_row(params![timeline_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                RowStamp {
                    model: row.get::<_, Option<String>>(2)?,
                    ts: row.get::<_, Option<Ms>>(3)?,
                },
            ))
        })
        .optional()?;
    // A sweep removed the row; a re-projection inserts it without usage.
    let Some((id, session_id, current)) = current else {
        return Ok(false);
    };
    let next = match restore {
        Some((prior, written)) if current == *written => prior.clone(),
        _ => current.clone(),
    };
    mark_bucket(
        dirty_buckets,
        maintenance,
        &session_id,
        current.ts,
        current.model.as_deref(),
    );
    mark_bucket(
        dirty_buckets,
        maintenance,
        &session_id,
        next.ts,
        next.model.as_deref(),
    );
    tx.prepare_cached(
        "UPDATE message SET in_tokens = NULL, out_tokens = NULL, cache_r_tokens = NULL,
                cache_w_tokens = NULL, think_tokens = NULL, cache_w1h_tokens = NULL,
                web_searches = NULL, service_tier = NULL, route = NULL, billed_pusd = NULL,
                usage_scope = NULL, requests = NULL, model = ?2, ts = ?3
         WHERE id = ?1",
    )?
    .execute(params![id, next.model, next.ts])?;
    if let Some(fx) = effects {
        let tokens = TokenSnapshot::default();
        let cost = crate::pricing::usage_cost(next.model.as_deref(), next.ts, &tokens);
        fx.push(Mutation::Usage {
            session_id,
            timeline_id: timeline_id.to_owned(),
            ts: next.ts,
            model: next.model,
            tokens: Box::new(tokens),
            cost,
        });
    }
    Ok(true)
}

/// Delete a displaced owner's sample: a dropped copy never inserts it, and
/// its hour bucket is redone. Returns whether the sample was there.
fn delete_sample(
    tx: &Transaction,
    session_id: &str,
    key: &str,
    maintenance: Maintenance,
    dirty_buckets: &mut BTreeSet<BucketKey>,
) -> Result<bool, StoreError> {
    let previous = tx
        .prepare_cached(
            "SELECT ts, model FROM session_usage WHERE session_id = ?1 AND sample_key = ?2",
        )?
        .query_row(params![session_id, key], |row| {
            Ok((row.get::<_, Ms>(0)?, row.get::<_, Option<String>>(1)?))
        })
        .optional()?;
    let Some((ts, model)) = previous else {
        return Ok(false);
    };
    tx.prepare_cached("DELETE FROM session_usage WHERE session_id = ?1 AND sample_key = ?2")?
        .execute(params![session_id, key])?;
    mark_bucket(
        dirty_buckets,
        maintenance,
        session_id,
        Some(ts),
        model.as_deref(),
    );
    Ok(true)
}

/// Upsert one session sample. Returns whether it was written: a sample
/// naming a session the projection does not hold is out of coverage.
fn apply_session_usage(
    tx: &Transaction,
    row: &SessionUsageRow,
    src_cursor: Option<i64>,
    stats: &mut ApplyStats,
    maintenance: Maintenance,
    dirty_buckets: &mut BTreeSet<BucketKey>,
) -> Result<bool, StoreError> {
    // A sample may name another session than its source (a Codex guardian
    // review attributes its usage to the parent thread). The parent row
    // exists whenever its rollout was captured; otherwise the sample is out
    // of coverage, never a foreign-key failure of the whole batch.
    let known: Option<i64> = tx
        .prepare_cached("SELECT 1 FROM session WHERE id = ?1")?
        .query_row(params![row.session_id], |record| record.get(0))
        .optional()?;
    if known.is_none() {
        stats.usage_unmatched += 1;
        return Ok(false);
    }
    let previous = tx
        .prepare_cached(
            "SELECT ts, model FROM session_usage
             WHERE session_id = ?1 AND sample_key = ?2",
        )?
        .query_row(params![row.session_id, row.key], |record| {
            Ok((record.get::<_, Ms>(0)?, record.get::<_, Option<String>>(1)?))
        })
        .optional()?;
    if let Some((ts, model)) = previous {
        mark_bucket(
            dirty_buckets,
            maintenance,
            &row.session_id,
            Some(ts),
            model.as_deref(),
        );
    }
    mark_bucket(
        dirty_buckets,
        maintenance,
        &row.session_id,
        Some(row.ts),
        row.model.as_deref(),
    );
    tx.prepare_cached(
        "INSERT INTO session_usage
           (session_id, sample_key, ts, model, in_tokens, out_tokens,
            cache_r_tokens, cache_w_tokens, think_tokens, src_cursor, scope,
            cache_w1h_tokens, web_searches, service_tier, route, billed_pusd, requests)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)
         ON CONFLICT(session_id, sample_key) DO UPDATE SET
           ts = excluded.ts, model = excluded.model, scope = excluded.scope,
           in_tokens = excluded.in_tokens, out_tokens = excluded.out_tokens,
           cache_r_tokens = excluded.cache_r_tokens,
           cache_w_tokens = excluded.cache_w_tokens,
           think_tokens = excluded.think_tokens,
           cache_w1h_tokens = excluded.cache_w1h_tokens,
           web_searches = excluded.web_searches,
           service_tier = excluded.service_tier,
           route = excluded.route,
           billed_pusd = excluded.billed_pusd,
           requests = excluded.requests,
           src_cursor = excluded.src_cursor",
    )?
    .execute(params![
        row.session_id,
        row.key,
        row.ts,
        row.model,
        row.tokens.input,
        row.tokens.output,
        row.tokens.cache_read,
        row.tokens.cache_write,
        row.tokens.thinking,
        src_cursor,
        row.tokens.scope.as_str(),
        row.tokens.cache_write_1h,
        row.tokens.web_searches,
        row.tokens
            .service_tier
            .map(rosetta_core::ServiceTier::as_str),
        row.tokens.route,
        row.tokens.billed_pico_usd,
        row.tokens.requests,
    ])?;
    touch_session(tx, &row.session_id, Some(row.ts), stats)?;
    stats.usage_applied += 1;
    Ok(true)
}

impl crate::Store {
    /// The session that owns `identity`'s usage in this process, if any
    /// record claimed it. Diagnostic: tests assert the owner converges.
    #[doc(hidden)]
    pub fn usage_owner(&self, identity: &str) -> Option<&str> {
        self.usage_owners.owner_of(identity)
    }
}
