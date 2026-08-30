//! Cold-load finalization shared by private atomic rebuilds and the final gate
//! of a progressive public first run. Canonical ordinals, rollups, and read
//! indexes are materialized here; full-text search is a detached projection
//! with its own worker and readiness.

use std::time::Instant;

use rusqlite::{Connection, Transaction, params};

use rosetta_core::fold::HOUR_MS;

use crate::{BulkLoadStats, StoreError, schema};

fn elapsed_nanos(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

pub(crate) fn prepare(conn: &Connection) -> Result<(), StoreError> {
    // Rollback still works, but a private scratch database does not need a
    // durable WAL for each intermediate transaction. The ledger remains the
    // durable truth and a failed scratch file is never installed.
    conn.execute_batch(
        "PRAGMA journal_mode = MEMORY;
         PRAGMA synchronous = OFF;",
    )?;
    conn.execute_batch(schema::BULK_DROP_INDEXES)?;
    Ok(())
}

/// Keep the progressive projection fully durable/readable while deferring
/// secondary indexes that are not needed by the resident list mirror or the
/// `(session_id, ordinal)` timeline identity. This retains WAL + NORMAL
/// durability and ordinary auto-checkpointing; only private rebuilds may use
/// MEMORY + OFF.
pub(crate) fn prepare_progressive(conn: &Connection) -> Result<(), StoreError> {
    conn.execute_batch(schema::BULK_DROP_INDEXES)?;
    Ok(())
}

/// Fold as much of the progressive WAL as possible into the canonical file.
/// A transient reader can prevent TRUNCATE from resetting the WAL; PASSIVE
/// still copies every unpinned frame. Callers keep canonical readiness false
/// while checkpointing so the detached search worker cannot itself pin the
/// pre-finalization WAL.
pub(crate) fn checkpoint_progressive(conn: &Connection) -> Result<(), StoreError> {
    let checkpoint = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
        Ok((row.get(0)?, row.get(1)?, row.get(2)?))
    });
    let (busy, _, _): (i64, i64, i64) = checkpoint?;
    if busy != 0 {
        let _: (i64, i64, i64) = conn.query_row("PRAGMA wal_checkpoint(PASSIVE)", [], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?;
    }
    Ok(())
}

/// Finish the final checkpoint before canonical readiness becomes visible.
pub(crate) fn finish_progressive_pragmas(conn: &Connection) -> Result<(), StoreError> {
    checkpoint_progressive(conn)
}

/// Refuse to materialize/publish a scratch database while any source is still
/// between generations. Normalized staging is process-local, so the durable
/// cursor watermark is the publication gate.
pub(crate) fn validate_complete(conn: &Connection) -> Result<(), StoreError> {
    let incomplete: i64 = conn.query_row(
        "SELECT COUNT(*) FROM ingest_cursor
         WHERE active_generation != highest_generation",
        [],
        |row| row.get(0),
    )?;
    if incomplete != 0 {
        return Err(StoreError::BulkLoadState {
            message: format!("cold rebuild has {incomplete} incomplete source generations"),
        });
    }
    Ok(())
}

pub(crate) fn finalize(tx: &Transaction<'_>) -> Result<BulkLoadStats, StoreError> {
    let mut stats = BulkLoadStats::default();
    // Bulk activation assigns collision-free provisional ordinals. Rank every
    // session once after all sources have landed instead of running one
    // growing window/sort for each source activation.
    let started = Instant::now();
    stats.renumber_rows = crate::apply::renumber_all_sessions(tx)?;
    stats.renumber_nanos = elapsed_nanos(started);

    // One set-based fold replaces per-message and per-session rescans. SUM's
    // NULL behavior preserves the model's never-reported != zero invariant.
    // The CASE implements Rust's div_euclid hour floor for pre-epoch values;
    // SQLite integer division otherwise truncates toward zero.
    let started = Instant::now();
    tx.execute("DELETE FROM usage_bucket", [])?;
    tx.execute(
        "INSERT INTO usage_bucket
           (session_id, hour_ts, model, in_tokens, out_tokens, cache_r_tokens,
            cache_w_tokens, think_tokens, message_count, tool_call_count)
         SELECT session_id, hour_ts, model,
                SUM(in_tokens), SUM(out_tokens), SUM(cache_r_tokens),
                SUM(cache_w_tokens), SUM(think_tokens), SUM(message_count),
                SUM(tool_call_count)
         FROM (
           SELECT session_id,
                  CASE WHEN COALESCE(ts, 0) >= 0
                    THEN (COALESCE(ts, 0) / ?1) * ?1
                    ELSE - ((-COALESCE(ts, 0) + ?1 - 1) / ?1) * ?1
                  END AS hour_ts,
                  COALESCE(model, 'unknown') AS model,
                  in_tokens, out_tokens, cache_r_tokens, cache_w_tokens,
                  think_tokens, 1 AS message_count,
                  CASE WHEN type = 'tool_use' THEN 1 ELSE 0 END AS tool_call_count
           FROM message
           UNION ALL
           SELECT session_id,
                  CASE WHEN ts >= 0
                    THEN (ts / ?1) * ?1
                    ELSE - ((-ts + ?1 - 1) / ?1) * ?1
                  END AS hour_ts,
                  COALESCE(model, 'unknown') AS model,
                  in_tokens, out_tokens, cache_r_tokens, cache_w_tokens,
                  think_tokens, 0 AS message_count, 0 AS tool_call_count
           FROM session_usage
         )
         GROUP BY session_id, hour_ts, model",
        params![HOUR_MS],
    )?;
    stats.bucket_nanos = elapsed_nanos(started);

    let started = Instant::now();
    tx.execute_batch(schema::BULK_CREATE_INDEXES)?;
    tx.execute_batch("ANALYZE;")?;
    stats.index_nanos = elapsed_nanos(started);
    Ok(stats)
}

pub(crate) fn validate(conn: &Connection) -> Result<(), StoreError> {
    let quick: String = conn.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
    if quick != "ok" {
        return Err(StoreError::ProjectionInvalid {
            message: format!("SQLite quick_check returned {quick}"),
        });
    }

    let mut statement = conn.prepare("PRAGMA foreign_key_check")?;
    if statement.query([])?.next()?.is_some() {
        return Err(StoreError::ProjectionInvalid {
            message: "foreign_key_check reported a violation".to_string(),
        });
    }
    Ok(())
}

pub(crate) fn restore_live_pragmas(conn: &Connection) -> Result<(), StoreError> {
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;",
    )?;
    Ok(())
}
