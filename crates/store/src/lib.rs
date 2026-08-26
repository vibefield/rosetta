//! rosetta-store — the canonical SQLite projection: schema, the transactional
//! write path, and the typed read surface. It is intended to be disposable once
//! the durable raw-ledger rebuild path ships. Until then, foreign schema versions
//! fail closed so the only indexed copy cannot be destroyed.

pub mod apply;
pub mod queries;
pub mod schema;

use std::fmt;
use std::path::Path;

use rusqlite::Connection;

pub use apply::{ApplyStats, Batch, CheckpointUpdate, SourceMode, SourceWrite};

#[derive(Debug)]
pub enum StoreError {
    Sqlite(rusqlite::Error),
    Json(serde_json::Error),
    /// The checkpoint CAS failed: another writer advanced this file's cursor,
    /// or the expected state was stale. The batch rolls back untouched.
    CheckpointConflict {
        path: String,
    },
    /// Until the durable raw-ledger rebuild command ships, opening a projection
    /// with foreign DDL must fail closed. Dropping it would destroy the only
    /// indexed copy after an upstream agent has garbage-collected its source.
    SchemaVersionMismatch {
        found: i64,
        expected: i64,
    },
    SourceConflict {
        cursor_id: i64,
        generation: u64,
        sequence: u64,
    },
    SourceState {
        cursor_id: i64,
        message: String,
    },
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sqlite(err) => write!(f, "sqlite: {err}"),
            Self::Json(err) => write!(f, "json: {err}"),
            Self::CheckpointConflict { path } => {
                write!(f, "checkpoint CAS conflict for {path}")
            }
            Self::SchemaVersionMismatch { found, expected } => write!(
                f,
                "projection schema version {found} does not match {expected}; rebuild from the raw ledger is not available yet"
            ),
            Self::SourceConflict {
                cursor_id,
                generation,
                sequence,
            } => write!(
                f,
                "source op conflict at cursor {cursor_id}, generation {generation}, sequence {sequence}"
            ),
            Self::SourceState { cursor_id, message } => {
                write!(f, "invalid source state for cursor {cursor_id}: {message}")
            }
        }
    }
}

impl std::error::Error for StoreError {}

impl From<rusqlite::Error> for StoreError {
    fn from(err: rusqlite::Error) -> Self {
        Self::Sqlite(err)
    }
}

impl From<serde_json::Error> for StoreError {
    fn from(err: serde_json::Error) -> Self {
        Self::Json(err)
    }
}

/// The projection. Single connection; the pipeline's writer thread owns it
/// exclusively — reads for serving go through their own connections.
pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let conn = Connection::open(path)?;
        schema::init(&conn)?;
        Ok(Self { conn })
    }

    pub fn open_in_memory() -> Result<Self, StoreError> {
        let conn = Connection::open_in_memory()?;
        schema::init(&conn)?;
        Ok(Self { conn })
    }

    /// Apply one batch in one transaction. On any error — including a
    /// checkpoint CAS conflict — nothing is committed.
    pub fn apply(&mut self, batch: &Batch) -> Result<ApplyStats, StoreError> {
        let tx = self.conn.transaction()?;
        let stats = apply::apply_batch(&tx, batch)?;
        tx.commit()?;
        Ok(stats)
    }

    /// Advance the persistent rotating-audit cursor after a source completed
    /// its forced replay. Repeating an audit after a crash is harmless.
    pub fn set_audit_path(&mut self, runtime_id: &str, path: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO source_audit (runtime_id, last_path) VALUES (?1, ?2)
             ON CONFLICT(runtime_id) DO UPDATE SET last_path = excluded.last_path",
            rusqlite::params![runtime_id, path],
        )?;
        Ok(())
    }

    /// Read access for queries (single-threaded use; the server opens its
    /// own read connections against the same file).
    pub fn conn(&self) -> &Connection {
        &self.conn
    }
}
