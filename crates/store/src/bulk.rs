//! Private cold-rebuild maintenance. The file this operates on is never the
//! installed projection: replay can therefore defer canonical presentation
//! ordinals, derived structures, and secondary indexes, validate once, and
//! publish only the finished database.

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

/// Refuse to materialize/publish a scratch database while any source is still
/// between generations. The direct first-generation cold path has no
/// `source_op` rows, so the cursor generation check is independently required.
pub(crate) fn validate_complete(conn: &Connection) -> Result<(), StoreError> {
    let staged: i64 = conn.query_row("SELECT COUNT(*) FROM source_op", [], |row| row.get(0))?;
    let incomplete: i64 = conn.query_row(
        "SELECT COUNT(*) FROM ingest_cursor
         WHERE active_generation != highest_generation",
        [],
        |row| row.get(0),
    )?;
    if staged != 0 || incomplete != 0 {
        return Err(StoreError::BulkLoadState {
            message: format!(
                "cold rebuild has {staged} staged ops and {incomplete} incomplete source generations"
            ),
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
                SUM(cache_w_tokens), SUM(think_tokens), COUNT(*),
                SUM(CASE WHEN type = 'tool_use' THEN 1 ELSE 0 END)
         FROM (
           SELECT session_id,
                  CASE WHEN COALESCE(ts, 0) >= 0
                    THEN (COALESCE(ts, 0) / ?1) * ?1
                    ELSE - ((-COALESCE(ts, 0) + ?1 - 1) / ?1) * ?1
                  END AS hour_ts,
                  COALESCE(model, 'unknown') AS model,
                  in_tokens, out_tokens, cache_r_tokens, cache_w_tokens,
                  think_tokens, type
           FROM message
         )
         GROUP BY session_id, hour_ts, model",
        params![HOUR_MS],
    )?;
    stats.bucket_nanos = elapsed_nanos(started);

    // External-content FTS can rebuild directly from the finished message
    // table. This is the intended FTS5 bulk-load path and avoids tokenizing
    // each row inside thousands of source activation transactions.
    let started = Instant::now();
    tx.execute("INSERT INTO message_fts(message_fts) VALUES('rebuild')", [])?;
    tx.execute(
        "INSERT INTO message_fts(message_fts, rank) VALUES('integrity-check', 1)",
        [],
    )?;
    stats.fts_nanos = elapsed_nanos(started);

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
