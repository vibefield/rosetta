//! rosetta-store — the disposable canonical SQLite projection: schema, the
//! transactional write path, and the typed read surface. Foreign schema versions
//! fail closed on ordinary open; `rosettad rebuild` explicitly reconstructs and
//! atomically replaces the projection from the durable raw ledger.

pub mod apply;
mod bulk;
pub mod hot;
pub mod queries;
pub mod schema;

use std::fmt;
use std::path::Path;
use std::time::Instant;

use rusqlite::Connection;

pub use apply::{ApplyStats, Batch, CheckpointUpdate, SourceMode, SourceWrite};
pub use hot::{Effects, HotConfig, MetaMirror, Mutation, Replay, Stamp, TailRings};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct BulkLoadStats {
    pub bucket_nanos: u64,
    pub fts_nanos: u64,
    pub index_nanos: u64,
    pub commit_nanos: u64,
    pub validate_nanos: u64,
    pub pragma_nanos: u64,
}

fn elapsed_nanos(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

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
    /// Cold loading is valid only for a private, empty projection and must be
    /// finalized before that projection can be published.
    BulkLoadState {
        message: String,
    },
    /// A private rebuild failed its pre-publication SQLite/FTS validation.
    ProjectionInvalid {
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
            Self::BulkLoadState { message } => write!(f, "invalid bulk-load state: {message}"),
            Self::ProjectionInvalid { message } => write!(f, "invalid projection: {message}"),
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
    mode: StoreMode,
    /// An abandoned generation means the private projection was built from an
    /// incomplete source. Keep accepting batches so callers can finish their
    /// diagnostic pass, but never allow that scratch file to be published.
    bulk_abandoned: bool,
    /// Hot-tier observation: `Some` collects the row-level [`Effects`] of
    /// every committed batch until drained. `None` (the default) costs
    /// nothing — backfill and rebuild never allocate a mutation log.
    effects: Option<Effects>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StoreMode {
    Live,
    Bulk,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let conn = Connection::open(path)?;
        schema::init(&conn)?;
        Ok(Self {
            conn,
            mode: StoreMode::Live,
            bulk_abandoned: false,
            effects: None,
        })
    }

    pub fn open_in_memory() -> Result<Self, StoreError> {
        let conn = Connection::open_in_memory()?;
        schema::init(&conn)?;
        Ok(Self {
            conn,
            mode: StoreMode::Live,
            bulk_abandoned: false,
            effects: None,
        })
    }

    /// Open a brand-new private projection for ledger rebuild. Derived FTS,
    /// buckets, and read-only secondary indexes are deferred until
    /// [`Store::finish_bulk_load`]. Existing schemas are rejected: this mode
    /// must never make an installed projection temporarily inconsistent.
    pub fn open_for_rebuild(path: &Path) -> Result<Self, StoreError> {
        let conn = Connection::open(path)?;
        Self::from_empty_connection_for_rebuild(conn)
    }

    /// In-memory counterpart to [`Store::open_for_rebuild`], primarily for
    /// parity and regression tests of the cold-load path.
    pub fn open_in_memory_for_rebuild() -> Result<Self, StoreError> {
        Self::from_empty_connection_for_rebuild(Connection::open_in_memory()?)
    }

    fn from_empty_connection_for_rebuild(conn: Connection) -> Result<Self, StoreError> {
        let objects: i64 = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%'",
            [],
            |row| row.get(0),
        )?;
        if objects != 0 {
            return Err(StoreError::BulkLoadState {
                message: "cold rebuild requires an empty SQLite file".to_string(),
            });
        }
        schema::init(&conn)?;
        bulk::prepare(&conn)?;
        Ok(Self {
            conn,
            mode: StoreMode::Bulk,
            bulk_abandoned: false,
            effects: None,
        })
    }

    /// Apply one batch in one transaction. On any error — including a
    /// checkpoint CAS conflict — nothing is committed, and nothing reaches
    /// the observed effects.
    pub fn apply(&mut self, batch: &Batch) -> Result<ApplyStats, StoreError> {
        let mut fresh = self.effects.is_some().then(Effects::default);
        let tx = self.conn.transaction()?;
        let maintenance = match self.mode {
            StoreMode::Live => apply::LIVE_MAINTENANCE,
            StoreMode::Bulk => apply::BULK_MAINTENANCE,
        };
        let bulk_load = self.mode == StoreMode::Bulk;
        let mut stats = apply::apply_batch(&tx, batch, maintenance, bulk_load, fresh.as_mut())?;
        let started = Instant::now();
        tx.commit()?;
        stats.sqlite_commit_nanos = elapsed_nanos(started);
        if bulk_load
            && matches!(
                batch.source.as_ref().map(|source| source.mode),
                Some(SourceMode::Abandon)
            )
        {
            self.bulk_abandoned = true;
        }
        if let (Some(pending), Some(committed)) = (self.effects.as_mut(), fresh) {
            pending.merge(committed);
        }
        Ok(stats)
    }

    /// Materialize every derived structure for a private cold rebuild, build
    /// deferred indexes, analyze the finished database, and validate it. Only
    /// after this succeeds is the store switched to ordinary live semantics.
    pub fn finish_bulk_load(&mut self) -> Result<BulkLoadStats, StoreError> {
        if self.mode != StoreMode::Bulk {
            return Err(StoreError::BulkLoadState {
                message: "store is not in cold rebuild mode".to_string(),
            });
        }
        if self.bulk_abandoned {
            return Err(StoreError::BulkLoadState {
                message: "cold rebuild contains an abandoned source generation".to_string(),
            });
        }
        bulk::validate_complete(&self.conn)?;
        let tx = self.conn.transaction()?;
        let mut stats = bulk::finalize(&tx)?;
        let started = Instant::now();
        tx.commit()?;
        stats.commit_nanos = elapsed_nanos(started);
        let started = Instant::now();
        bulk::validate(&self.conn)?;
        stats.validate_nanos = elapsed_nanos(started);
        let started = Instant::now();
        bulk::restore_live_pragmas(&self.conn)?;
        stats.pragma_nanos = elapsed_nanos(started);
        self.mode = StoreMode::Live;
        self.bulk_abandoned = false;
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
