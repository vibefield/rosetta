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
         PRAGMA synchronous = OFF;
         PRAGMA foreign_keys = OFF;
         PRAGMA ignore_check_constraints = ON;",
    )?;
    conn.execute_batch(schema::BULK_DROP_INDEXES)?;
    Ok(())
}

/// Re-enable row constraints before the final whole-projection audit. SQLite
/// `quick_check` then evaluates every CHECK against the finished rows, while
/// `foreign_key_check` below evaluates the complete relationship graph. A
/// violation keeps the scratch projection private and unpublishable.
pub(crate) fn enable_constraint_validation(conn: &Connection) -> Result<(), StoreError> {
    conn.execute_batch("PRAGMA ignore_check_constraints = OFF;")?;
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

pub(crate) fn finalize(
    tx: &Transaction<'_>,
    ordinals_precomputed: bool,
) -> Result<BulkLoadStats, StoreError> {
    let mut stats = BulkLoadStats::default();
    // Bulk activation assigns collision-free provisional ordinals. Rank every
    // session once after all sources have landed instead of running one
    // growing window/sort for each source activation.
    if !ordinals_precomputed {
        let started = Instant::now();
        stats.renumber_rows = crate::apply::renumber_all_sessions(tx)?;
        stats.renumber_nanos = elapsed_nanos(started);
    }

    // One set-based fold replaces per-message and per-session rescans. SUM's
    // NULL behavior preserves the model's never-reported != zero invariant.
    // The CASE implements Rust's div_euclid hour floor for pre-epoch values;
    // SQLite integer division otherwise truncates toward zero.
    let started = Instant::now();
    refold_usage_buckets(tx)?;
    stats.bucket_nanos = elapsed_nanos(started);

    let started = Instant::now();
    tx.execute_batch(schema::BULK_CREATE_INDEXES)?;
    tx.execute_batch("ANALYZE;")?;
    stats.index_nanos = elapsed_nanos(started);
    Ok(stats)
}

/// Replace every `usage_bucket` row from the stored usage facts in one
/// set-based pass, pricing each record with the compiled catalog, and stamp
/// the pricing version (catalog plus calculator). SUM's NULL behavior preserves the model's
/// never-reported != zero invariant. Cold finalization and a price-catalog
/// change both come through here; neither replays the ledger.
pub(crate) fn refold_usage_buckets(tx: &Connection) -> Result<(), StoreError> {
    tx.execute("DELETE FROM usage_bucket", [])?;
    tx.execute(&crate::usage_sql::FOLD_ALL, params![HOUR_MS])?;
    tx.execute(
        "UPDATE projection_state SET pricing_version = ?1 WHERE singleton = 1",
        params![rosetta_pricing::pricing_version()],
    )?;
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
         PRAGMA synchronous = NORMAL;
         PRAGMA ignore_check_constraints = OFF;
         PRAGMA foreign_keys = ON;",
    )?;
    Ok(())
}
