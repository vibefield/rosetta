//! Reconcile cumulative session-tree totals with independently priced requests.
//! Only the selected totals and their user-facing rollup survive; no snapshot
//! history or native parser state is persisted.

use std::collections::{BTreeMap, BTreeSet};

use rosetta_core::SessionUsageTotals;
use rosetta_core::fold::hour_bucket;
use rusqlite::{Connection, OptionalExtension, params};

use crate::StoreError;

pub(crate) fn replace(
    conn: &Connection,
    totals: &SessionUsageTotals,
    src_cursor: Option<i64>,
) -> Result<Vec<(i64, String)>, StoreError> {
    let mut touched = conn
        .prepare_cached("SELECT ts, model FROM session_usage_total WHERE session_id = ?1")?
        .query_map([&totals.session_id], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    conn.execute(
        "DELETE FROM session_usage_total WHERE session_id = ?1",
        [&totals.session_id],
    )?;
    let mut insert = conn.prepare_cached(
        "INSERT INTO session_usage_total (session_id, model, ts, in_tokens, output_total,
         think_tokens, cache_r_tokens, cache_write_total, web_searches, estimated_pusd, src_cursor)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
    )?;
    for model in &totals.models {
        insert.execute(params![
            totals.session_id,
            model.model,
            totals.ts,
            model.input,
            model.output,
            model.thinking,
            model.cache_read,
            model.cache_write,
            model.web_searches,
            model.estimated_pico_usd,
            src_cursor
        ])?;
        touched.push((totals.ts, model.model.clone()));
    }
    Ok(touched)
}

pub(crate) fn reconcile_all(conn: &Connection) -> Result<(), StoreError> {
    let sessions = conn
        .prepare("SELECT DISTINCT session_id FROM session_usage_total")?
        .query_map([], |r| r.get(0))?
        .collect::<Result<BTreeSet<String>, _>>()?;
    reconcile_sessions(conn, &sessions)
}

pub(crate) fn reconcile_sessions(
    conn: &Connection,
    sessions: &BTreeSet<String>,
) -> Result<(), StoreError> {
    let mut roots = BTreeSet::new();
    let mut root = conn.prepare_cached("SELECT root_session_id FROM session WHERE id = ?1")?;
    for session in sessions {
        if let Some(id) = root
            .query_row([session], |r| r.get::<_, String>(0))
            .optional()?
        {
            roots.insert(id);
        }
    }
    for root in roots {
        reconcile_tree(conn, &root)?;
    }
    Ok(())
}

#[derive(Default)]
struct Total {
    // input, inclusive output, cache read, combined cache write, thinking, searches
    counts: [i64; 6],
    cost: Option<i64>,
    model: String,
    ts: i64,
    unpriced_tokens: i64,
}

impl Total {
    fn add(&mut self, row: &Self) -> Result<(), StoreError> {
        if row.cost.is_none() {
            for value in &row.counts[..4] {
                self.unpriced_tokens = checked_add(self.unpriced_tokens, *value)?;
            }
        }
        for (sum, value) in self.counts.iter_mut().zip(row.counts) {
            *sum = checked_add(*sum, value)?;
        }
        self.cost = match (self.cost, row.cost) {
            (Some(a), Some(b)) => Some(checked_add(a, b)?),
            (a, b) => a.or(b),
        };
        if self.model.is_empty() {
            self.ts = row.ts;
            self.model.clone_from(&row.model);
        } else {
            self.ts = self.ts.max(row.ts);
        }
        Ok(())
    }
}

fn checked_add(a: i64, b: i64) -> Result<i64, StoreError> {
    a.checked_add(b)
        .ok_or_else(|| StoreError::ProjectionInvalid {
            message: "cumulative usage exceeds the projection's integer range".into(),
        })
}

fn model_key(model: &str) -> String {
    rosetta_pricing::resolve(model)
        .map_or(model, |m| m.id)
        .to_owned()
}

fn reconcile_tree(conn: &Connection, root: &str) -> Result<(), StoreError> {
    // Subagent records may arrive after the parent's snapshot, or be corrected
    // or removed later. Rebuild the difference from current facts each time.
    conn.prepare_cached(
        "DELETE FROM usage_bucket WHERE source = 'reported' AND session_id IN
         (SELECT id FROM session WHERE root_session_id = ?1)",
    )?
    .execute([root])?;
    let reported_sessions = conn
        .prepare_cached(
            "SELECT DISTINCT t.session_id FROM session_usage_total t
         JOIN session s ON s.id = t.session_id WHERE s.root_session_id = ?1",
        )?
        .query_map([root], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    if reported_sessions.is_empty() {
        return Ok(());
    }

    let parents = conn
        .prepare_cached("SELECT id, parent_session_id FROM session WHERE root_session_id = ?1")?
        .query_map([root], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
        })?
        .collect::<Result<BTreeMap<_, _>, _>>()?;
    let mut scopes = Vec::new();
    for session in reported_sessions {
        let members: Vec<_> = parents
            .keys()
            .filter(|member| session == root || is_descendant(member, &session, &parents))
            .cloned()
            .collect();
        scopes.push((session, members));
    }
    // A nested total is reconciled first. Its parent's comparison includes
    // that correction, so overlapping cumulative ledgers still count once.
    scopes.sort_by(|a, b| a.1.len().cmp(&b.1.len()).then(a.0.cmp(&b.0)));
    for (session, members) in scopes {
        reconcile_one(conn, &session, &members)?;
    }
    Ok(())
}

fn is_descendant(member: &str, ancestor: &str, parents: &BTreeMap<String, Option<String>>) -> bool {
    let mut cursor = Some(member);
    // Bounded even if malformed metadata contains a parent cycle.
    for _ in 0..=parents.len() {
        let Some(id) = cursor else { break };
        if id == ancestor {
            return true;
        }
        cursor = parents.get(id).and_then(|p| p.as_deref());
    }
    false
}

fn reconcile_one(conn: &Connection, session: &str, members: &[String]) -> Result<(), StoreError> {
    let mut recorded: BTreeMap<String, Total> = BTreeMap::new();
    let mut buckets = conn.prepare_cached(
        "SELECT model, COALESCE(SUM(in_tokens),0),
         SUM(COALESCE(out_tokens,0) + COALESCE(think_tokens,0)),
         COALESCE(SUM(cache_r_tokens),0),
         SUM(COALESCE(cache_w_tokens,0) + COALESCE(cache_w1h_tokens,0)),
         COALESCE(SUM(think_tokens),0), COALESCE(SUM(web_searches),0), SUM(cost_pusd)
         FROM usage_bucket WHERE session_id = ?1 GROUP BY model",
    )?;
    for member in members {
        let values = buckets.query_map([member], |r| {
            Ok(Total {
                model: r.get(0)?,
                counts: [
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                ],
                cost: r.get(7)?,
                ts: 0,
                unpriced_tokens: 0,
            })
        })?;
        for row in values {
            let row = row?;
            recorded
                .entry(model_key(&row.model))
                .or_default()
                .add(&row)?;
        }
    }
    let mut reported: BTreeMap<String, Total> = BTreeMap::new();
    let mut stmt = conn.prepare_cached(
        "SELECT model, in_tokens, output_total, cache_r_tokens, cache_write_total,
         COALESCE(think_tokens,0), web_searches, estimated_pusd, ts
         FROM session_usage_total WHERE session_id = ?1 ORDER BY model",
    )?;
    for row in stmt.query_map([session], |r| {
        Ok(Total {
            model: r.get(0)?,
            counts: [
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
            ],
            cost: r.get(7)?,
            ts: r.get(8)?,
            unpriced_tokens: 0,
        })
    })? {
        let row = row?;
        reported
            .entry(model_key(&row.model))
            .or_default()
            .add(&row)?;
    }
    let mut insert = conn.prepare_cached(
        "INSERT INTO usage_bucket (session_id, hour_ts, model, source, in_tokens, out_tokens,
         cache_r_tokens, cache_w_tokens, think_tokens, web_searches, cost_pusd, unpriced_tokens)
         VALUES (?1, ?2, ?3, 'reported', ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
    )?;
    for (key, total) in reported {
        let known = recorded.get(&key);
        let mut extra = [0_i64; 6];
        for (index, value) in extra.iter_mut().enumerate() {
            *value = (total.counts[index] - known.map_or(0, |k| k.counts[index])).max(0);
        }
        // Thinking is a subset of output. It may fill missing output, but
        // cannot increase that inclusive total a second time. Likewise cache
        // writes compare both TTLs together, preserving every recorded 1h write.
        extra[4] = extra[4].min(extra[1]);
        let cost = total
            .cost
            .map(|c| (c - known.and_then(|k| k.cost).unwrap_or(0)).max(0));
        let added_tokens = extra[..4]
            .iter()
            .try_fold(0, |sum, v| checked_add(sum, *v))?;
        let unpriced = added_tokens.min(total.unpriced_tokens);
        let model = known.map_or(&total.model, |k| &k.model);
        insert.execute(params![
            session,
            hour_bucket(total.ts),
            model,
            extra[0],
            extra[1] - extra[4],
            extra[2],
            extra[3],
            extra[4],
            extra[5],
            cost,
            unpriced
        ])?;
    }
    Ok(())
}
