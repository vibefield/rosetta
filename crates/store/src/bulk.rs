//! Private cold-rebuild maintenance. The file this operates on is never the
//! installed projection: replay can therefore defer derived structures and
//! secondary indexes, validate once, and publish only the finished database.

use rusqlite::{Connection, Transaction, params};

use rosetta_core::fold::HOUR_MS;

use crate::{StoreError, schema};

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

pub(crate) fn finalize(tx: &Transaction<'_>) -> Result<(), StoreError> {
    // One set-based fold replaces per-message and per-session rescans. SUM's
    // NULL behavior preserves the model's never-reported != zero invariant.
    // The CASE implements Rust's div_euclid hour floor for pre-epoch values;
    // SQLite integer division otherwise truncates toward zero.
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

    // External-content FTS can rebuild directly from the finished message
    // table. This is the intended FTS5 bulk-load path and avoids tokenizing
    // each row inside thousands of source activation transactions.
    tx.execute("INSERT INTO message_fts(message_fts) VALUES('rebuild')", [])?;
    tx.execute(
        "INSERT INTO message_fts(message_fts, rank) VALUES('integrity-check', 1)",
        [],
    )?;

    tx.execute_batch(schema::BULK_CREATE_INDEXES)?;
    tx.execute_batch("ANALYZE;")?;
    Ok(())
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
