//! rosetta-store — the disposable canonical SQLite projection: schema, the
//! transactional write path, and the typed read surface. Foreign schema versions
//! fail closed on ordinary open; `rosettad rebuild` explicitly reconstructs and
//! atomically replaces the projection from the durable raw ledger.

pub mod apply;
pub mod hot;
pub mod queries;
pub mod schema;

use std::fmt;
use std::path::Path;

use rusqlite::Connection;

pub use apply::{ApplyStats, Batch, CheckpointUpdate, SourceMode, SourceWrite};
pub use hot::{Effects, HotConfig, MetaMirror, Mutation, Replay, Stamp, TailRings};

#[derive(Debug)]
pub enum StoreError {
    Sqlite(rusqlite::Error),
    Json(serde_json::Error),
    /// The checkpoint CAS failed: another writer advanced this file's cursor,
    /// or the expected state was stale. The batch rolls back untouched.
    CheckpointConflict {
        path: String,
    },
    /// Opening a projection with foreign DDL fails closed. Replacement is an
    /// explicit raw-ledger rebuild, never a side effect of opening the store.
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
                "projection schema version {found} does not match {expected}; run an explicit raw-ledger rebuild"
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
    /// Hot-tier observation: `Some` collects the row-level [`Effects`] of
    /// every committed batch until drained. `None` (the default) costs
    /// nothing — backfill and rebuild never allocate a mutation log.
    effects: Option<Effects>,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let conn = Connection::open(path)?;
        schema::init(&conn)?;
        Ok(Self {
            conn,
            effects: None,
        })
    }

    pub fn open_in_memory() -> Result<Self, StoreError> {
        let conn = Connection::open_in_memory()?;
        schema::init(&conn)?;
        Ok(Self {
            conn,
            effects: None,
        })
    }

    /// Apply one batch in one transaction. On any error — including a
    /// checkpoint CAS conflict — nothing is committed, and nothing reaches
    /// the observed effects.
    pub fn apply(&mut self, batch: &Batch) -> Result<ApplyStats, StoreError> {
        let mut fresh = self.effects.is_some().then(Effects::default);
        let tx = self.conn.transaction()?;
        let stats = apply::apply_batch(&tx, batch, fresh.as_mut())?;
        tx.commit()?;
        if let (Some(pending), Some(committed)) = (self.effects.as_mut(), fresh) {
            pending.merge(committed);
        }
        Ok(stats)
    }

    /// Start collecting hot-tier effects. The daemon enables this before its
    /// live loop and drains between ticks; leaving it enabled without
    /// draining trips the overflow flag rather than growing without bound.
    pub fn observe_effects(&mut self) {
        if self.effects.is_none() {
            self.effects = Some(Effects::default());
        }
    }

    /// Take everything observed since the last drain (empty when observation
    /// is disabled). Call strictly after the batches in question committed —
    /// which is guaranteed here, because [`Store::apply`] merges effects only
    /// on commit.
    pub fn drain_effects(&mut self) -> Effects {
        self.effects
            .as_mut()
            .map(std::mem::take)
            .unwrap_or_default()
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
