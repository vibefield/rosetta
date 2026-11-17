//! Diagnostic drift aggregation. Every distinct drift event counts once per
//! aggregate key, and the aggregate keeps one sample location. That sample
//! is the least (cursor path, offset) any event offered, compared by path
//! rather than by cursor id because a rebuild or resume assigns cursor ids
//! in another order: whichever order sources land in, the same event wins
//! (R1, decision D6).
//!
//! A cold build applies about one drift event per unknown-type line, so the
//! two lookups that order-free sample needs (the event's cursor path and the
//! aggregate's stored sample) are answered from [`DriftCache`], which is
//! process-local like the usage owner index.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::Arc;
use std::time::Instant;

use rosetta_core::rows::DriftFact;
use rusqlite::{OptionalExtension, Transaction, params};

use crate::StoreError;

use super::{ApplyStats, elapsed_nanos};

/// Entries past which a map starts over: the cache only saves reads, so
/// forgetting is always safe, and this bounds a long-lived daemon's RAM.
const CACHE_LIMIT: usize = 1 << 16;

/// The Store's RAM view of what drift apply would otherwise read per event.
///
/// - `paths` maps a cursor id to its path. A cursor row is never deleted and
///   its path never changes, so an entry stays true until its id is created
///   again, which [`DriftCache::cursor_created`] forgets.
/// - `samples` maps an aggregate key to its stored sample, as this process
///   last read or wrote it. The stored minimum only ever falls, so a cached
///   sample is never below the stored one: an event that does not sort
///   before it cannot lower the minimum and needs no read. Any other event
///   reads the stored sample, which stays the authority.
///
/// Both hold only while every write they saw committed. The Store takes the
/// cache out for each writer transaction that can apply drift and puts it
/// back only after the commit, so a rolled-back write leaves an empty cache.
#[derive(Debug, Default)]
pub(crate) struct DriftCache {
    paths: HashMap<i64, Arc<str>>,
    samples: HashMap<Box<str>, Sample>,
    /// Reused for the aggregate key, so a cache hit allocates nothing.
    key: String,
}

/// A stored aggregate sample: the cursor, its path, and the offset.
#[derive(Debug, Clone)]
struct Sample {
    cursor: Option<i64>,
    path: Option<Arc<str>>,
    offset: Option<i64>,
}

impl Sample {
    fn order_key(&self) -> (Option<&str>, Option<i64>) {
        (self.path.as_deref(), self.offset)
    }
}

impl DriftCache {
    /// A cursor row was just created with `id`. Forget whatever was cached
    /// under an id SQLite handed out again after its row went away.
    pub(crate) fn cursor_created(&mut self, id: i64) {
        self.paths.remove(&id);
        self.samples.retain(|_, sample| sample.cursor != Some(id));
    }

    fn path(
        &mut self,
        tx: &Transaction,
        cursor: Option<i64>,
    ) -> Result<Option<Arc<str>>, StoreError> {
        let Some(cursor) = cursor else {
            return Ok(None);
        };
        if let Some(path) = self.paths.get(&cursor) {
            debug_assert_eq!(
                stored_path(tx, cursor)?.as_deref(),
                Some(&**path),
                "cached path of cursor {cursor}"
            );
            return Ok(Some(Arc::clone(path)));
        }
        let Some(path) = stored_path(tx, cursor)? else {
            return Ok(None);
        };
        let path: Arc<str> = path.into();
        if self.paths.len() >= CACHE_LIMIT {
            self.paths.clear();
        }
        self.paths.insert(cursor, Arc::clone(&path));
        Ok(Some(path))
    }

    /// `fact`'s aggregate key in the reused buffer: each part length-prefixed,
    /// so no two keys share an encoding.
    fn fill_key(&mut self, fact: &DriftFact) {
        self.key.clear();
        for part in [
            &fact.runtime_id,
            &fact.agent_version,
            &fact.stream,
            &fact.record_kind,
            &fact.error_code,
            &fact.shape_hash,
        ] {
            let _ = write!(self.key, "{}:", part.len());
            self.key.push_str(part);
        }
    }

    fn remember(&mut self, sample: Sample) {
        if let Some(cached) = self.samples.get_mut(self.key.as_str()) {
            *cached = sample;
            return;
        }
        if self.samples.len() >= CACHE_LIMIT {
            self.samples.clear();
        }
        self.samples.insert(self.key.as_str().into(), sample);
    }
}

pub(super) fn apply_drift(
    tx: &Transaction,
    fact: &DriftFact,
    cache: &mut DriftCache,
    stats: &mut ApplyStats,
) -> Result<(), StoreError> {
    stats.profile.drift_attempts += 1;
    let started = Instant::now();
    let result = record(tx, fact, cache, stats);
    stats.profile.drift_apply_nanos = stats
        .profile
        .drift_apply_nanos
        .saturating_add(elapsed_nanos(started));
    result
}

fn record(
    tx: &Transaction,
    fact: &DriftFact,
    cache: &mut DriftCache,
    stats: &mut ApplyStats,
) -> Result<(), StoreError> {
    // Cursor rowids depend on source admission order. The durable dedupe
    // identity must not: resolve provenance to the cursor's stable path
    // so private, progressive, and replay projections converge.
    let sample_path = cache.path(tx, fact.sample_cursor)?;
    let event_key = serde_json::to_string(&(
        &fact.runtime_id,
        &fact.agent_version,
        &fact.stream,
        &fact.record_kind,
        &fact.error_code,
        &fact.shape_hash,
        fact.at,
        sample_path.as_deref(),
        fact.sample_offset,
    ))?;
    let inserted = tx
        .prepare_cached("INSERT OR IGNORE INTO drift_seen (event_key) VALUES (?1)")?
        .execute(params![event_key])?;
    if inserted == 0 {
        return Ok(());
    }
    let offered = (sample_path.as_deref(), fact.sample_offset);
    cache.fill_key(fact);
    let cached = cache.samples.get(cache.key.as_str());
    let lowers = match cached {
        Some(cached) if sample_order(offered, cached.order_key()) != Ordering::Less => {
            debug_assert!(
                stored_sample(tx, fact)?
                    .is_some_and(
                        |stored| sample_order(offered, stored.order_key()) != Ordering::Less
                    ),
                "a cached drift sample is never below the stored one"
            );
            false
        }
        _ => {
            stats.profile.drift_sample_reads += 1;
            match stored_sample(tx, fact)? {
                Some(stored) if sample_order(offered, stored.order_key()) != Ordering::Less => {
                    cache.remember(stored);
                    false
                }
                _ => true,
            }
        }
    };
    // A new aggregate inserts the event's own sample; an existing one takes
    // it only when it sorts first.
    tx.prepare_cached(
        "INSERT INTO drift_agg
           (runtime_id, agent_version, stream, record_kind, error_code, shape_hash,
            count, first_seen, last_seen, sample_cursor, sample_offset)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, ?7, ?7, ?8, ?9)
         ON CONFLICT(runtime_id, agent_version, stream, record_kind, error_code, shape_hash)
         DO UPDATE SET
           count = count + 1,
           first_seen = CASE
             WHEN first_seen IS NULL THEN excluded.first_seen
             WHEN excluded.first_seen IS NULL THEN first_seen
             ELSE MIN(first_seen, excluded.first_seen) END,
           last_seen = CASE
             WHEN last_seen IS NULL THEN excluded.last_seen
             WHEN excluded.last_seen IS NULL THEN last_seen
             ELSE MAX(last_seen, excluded.last_seen) END,
           sample_cursor = CASE WHEN ?10 THEN excluded.sample_cursor ELSE sample_cursor END,
           sample_offset = CASE WHEN ?10 THEN excluded.sample_offset ELSE sample_offset END",
    )?
    .execute(params![
        fact.runtime_id,
        fact.agent_version,
        fact.stream,
        fact.record_kind,
        fact.error_code,
        fact.shape_hash,
        fact.at,
        fact.sample_cursor,
        fact.sample_offset,
        lowers,
    ])?;
    if lowers {
        cache.remember(Sample {
            cursor: fact.sample_cursor,
            path: sample_path,
            offset: fact.sample_offset,
        });
    }
    stats.drift_recorded += 1;
    Ok(())
}

fn stored_path(tx: &Transaction, cursor: i64) -> Result<Option<String>, StoreError> {
    Ok(tx
        .prepare_cached("SELECT path FROM ingest_cursor WHERE id = ?1")?
        .query_row(params![cursor], |row| row.get::<_, String>(0))
        .optional()?)
}

/// The aggregate's stored sample, its cursor resolved to a path (none when
/// the cursor row is missing, which sorts last).
fn stored_sample(tx: &Transaction, fact: &DriftFact) -> Result<Option<Sample>, StoreError> {
    Ok(tx
        .prepare_cached(
            "SELECT d.sample_cursor, d.sample_offset, c.path
             FROM drift_agg d LEFT JOIN ingest_cursor c ON c.id = d.sample_cursor
             WHERE d.runtime_id = ?1 AND d.agent_version = ?2 AND d.stream = ?3
               AND d.record_kind = ?4 AND d.error_code = ?5 AND d.shape_hash = ?6",
        )?
        .query_row(
            params![
                fact.runtime_id,
                fact.agent_version,
                fact.stream,
                fact.record_kind,
                fact.error_code,
                fact.shape_hash,
            ],
            |row| {
                Ok(Sample {
                    cursor: row.get(0)?,
                    offset: row.get(1)?,
                    path: row.get::<_, Option<String>>(2)?.map(Arc::from),
                })
            },
        )
        .optional()?)
}

/// The lexicographic order of two samples' (cursor path, offset), where a
/// missing component (NULL) is absent and sorts after every present one.
fn sample_order(a: (Option<&str>, Option<i64>), b: (Option<&str>, Option<i64>)) -> Ordering {
    fn key((path, offset): (Option<&str>, Option<i64>)) -> (bool, Option<&str>, bool, Option<i64>) {
        (path.is_none(), path, offset.is_none(), offset)
    }
    key(a).cmp(&key(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_present_path_and_offset_sort_before_absent_ones() {
        let order = |a, b| sample_order(a, b);
        assert_eq!(
            order((Some("a.jsonl"), Some(9)), (Some("b.jsonl"), Some(1))),
            Ordering::Less
        );
        assert_eq!(
            order((Some("a.jsonl"), Some(2)), (Some("a.jsonl"), Some(9))),
            Ordering::Less
        );
        assert_eq!(
            order((Some("z.jsonl"), None), (None, Some(0))),
            Ordering::Less
        );
        assert_eq!(
            order((Some("a.jsonl"), Some(1)), (Some("a.jsonl"), None)),
            Ordering::Less
        );
        assert_eq!(order((None, Some(1)), (None, None)), Ordering::Less);
        assert_eq!(order((None, None), (None, None)), Ordering::Equal);
    }

    #[test]
    fn aggregate_keys_never_share_an_encoding() {
        let fact = |runtime: &str, version: &str| DriftFact {
            runtime_id: runtime.into(),
            agent_version: version.into(),
            stream: "s".into(),
            record_kind: "k".into(),
            error_code: "e".into(),
            shape_hash: "h".into(),
            at: None,
            sample_cursor: None,
            sample_offset: None,
        };
        let mut cache = DriftCache::default();
        cache.fill_key(&fact("ab", "c"));
        let first = cache.key.clone();
        cache.fill_key(&fact("a", "bc"));
        assert_ne!(first, cache.key);
    }
}
