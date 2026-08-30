//! rosetta-store — the disposable canonical SQLite projection: schema, the
//! transactional write path, and the typed read surface. Foreign schema versions
//! fail closed on ordinary open; `rosettad rebuild` explicitly reconstructs and
//! atomically replaces the projection from the durable raw ledger.

pub mod apply;
pub mod assets;
mod bulk;
pub mod hot;
pub mod queries;
pub mod schema;
mod search_sync;

use std::fmt;
use std::path::Path;
use std::time::Instant;

use rosetta_core::rows::StreamState;
use rusqlite::Connection;

pub use apply::{ApplyProfile, ApplyStats, Batch, CheckpointUpdate, SourceMode, SourceWrite};
pub use hot::{Effects, HotConfig, MetaMirror, Mutation, Replay, Stamp, TailRings};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct BulkLoadStats {
    pub renumber_rows: u64,
    pub renumber_nanos: u64,
    pub bucket_nanos: u64,
    pub fts_nanos: u64,
    pub index_nanos: u64,
    pub commit_nanos: u64,
    pub validate_nanos: u64,
    pub pragma_nanos: u64,
    pub image_sync_nanos: u64,
}

fn elapsed_nanos(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

#[derive(Debug)]
pub enum StoreError {
    Sqlite(rusqlite::Error),
    Json(serde_json::Error),
    Io(std::io::Error),
    AssetInvalid {
        message: String,
    },
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
            Self::Io(err) => write!(f, "filesystem: {err}"),
            Self::AssetInvalid { message } => write!(f, "invalid image asset: {message}"),
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

impl From<std::io::Error> for StoreError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err)
    }
}

/// The projection. Single connection; the pipeline's writer thread owns it
/// exclusively — reads for serving go through their own connections.
pub struct Store {
    conn: Connection,
    images: assets::ImageStore,
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
    Progressive,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let conn = Connection::open(path)?;
        let objects: i64 = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%'",
            [],
            |row| row.get(0),
        )?;
        schema::init(&conn)?;
        let state = queries::projection_state(&conn)?;
        if objects == 0 {
            conn.execute(
                "UPDATE projection_state SET canonical_ready = 1 WHERE singleton = 1",
                [],
            )?;
        } else if !state.canonical_ready {
            return Err(StoreError::BulkLoadState {
                message: "projection has an interrupted progressive backfill; resume it through ingest/watch/serve"
                    .into(),
            });
        }
        Ok(Self {
            conn,
            images: assets::ImageStore::for_projection(path),
            mode: StoreMode::Live,
            bulk_abandoned: false,
            effects: None,
        })
    }

    pub fn open_in_memory() -> Result<Self, StoreError> {
        let conn = Connection::open_in_memory()?;
        schema::init(&conn)?;
        conn.execute(
            "UPDATE projection_state SET canonical_ready = 1 WHERE singleton = 1",
            [],
        )?;
        Ok(Self {
            conn,
            images: assets::ImageStore::in_memory(),
            mode: StoreMode::Live,
            bulk_abandoned: false,
            effects: None,
        })
    }

    /// Open or resume the one public projection used by a progressive first
    /// run. A completed projection returns in ordinary live mode; an
    /// interrupted one retains cold direct-first-generation behavior and is
    /// finalized only after every selected runtime completes cleanly.
    pub fn open_progressive(path: &Path) -> Result<Self, StoreError> {
        let conn = Connection::open(path)?;
        schema::init_deferred(&conn)?;
        let state = queries::projection_state(&conn)?;
        if state.canonical_ready {
            conn.execute_batch(schema::BULK_CREATE_INDEXES)?;
        } else {
            bulk::prepare_progressive(&conn)?;
        }
        Ok(Self {
            conn,
            images: assets::ImageStore::for_projection(path),
            mode: if state.canonical_ready {
                StoreMode::Live
            } else {
                StoreMode::Progressive
            },
            bulk_abandoned: false,
            effects: None,
        })
    }

    /// Open a brand-new private projection for ledger rebuild. Canonical
    /// presentation ordinals, derived FTS/buckets, and read-only secondary
    /// indexes are deferred until [`Store::finish_bulk_load`]. Existing
    /// schemas are rejected: this mode must never make an installed projection
    /// temporarily inconsistent.
    pub fn open_for_rebuild(path: &Path) -> Result<Self, StoreError> {
        let conn = Connection::open(path)?;
        Self::from_empty_connection_for_rebuild(conn, assets::ImageStore::for_projection(path))
    }

    /// Open a private SQLite sibling whose decoded images belong to the final
    /// projection's stable sidecar. Content-addressing lets the old and new
    /// databases share that directory safely across the atomic DB swap.
    pub fn open_for_rebuild_with_assets(
        path: &Path,
        projection: &Path,
    ) -> Result<Self, StoreError> {
        let conn = Connection::open(path)?;
        Self::from_empty_connection_for_rebuild(
            conn,
            assets::ImageStore::for_projection(projection),
        )
    }

    /// In-memory counterpart to [`Store::open_for_rebuild`], primarily for
    /// parity and regression tests of the cold-load path.
    pub fn open_in_memory_for_rebuild() -> Result<Self, StoreError> {
        Self::from_empty_connection_for_rebuild(
            Connection::open_in_memory()?,
            assets::ImageStore::in_memory(),
        )
    }

    fn from_empty_connection_for_rebuild(
        conn: Connection,
        images: assets::ImageStore,
    ) -> Result<Self, StoreError> {
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
        schema::init_deferred(&conn)?;
        bulk::prepare(&conn)?;
        Ok(Self {
            conn,
            images,
            mode: StoreMode::Bulk,
            bulk_abandoned: false,
            effects: None,
        })
    }

    /// Apply one batch in one transaction. On any error — including a
    /// checkpoint CAS conflict — nothing is committed, and nothing reaches
    /// the observed effects.
    pub fn apply(&mut self, batch: &Batch) -> Result<ApplyStats, StoreError> {
        let image_stats = self
            .images
            .materialize_batch(batch, self.mode != StoreMode::Bulk)?;
        let mut fresh = self.effects.is_some().then(Effects::default);
        let tx = self.conn.transaction()?;
        let search_before = if self.mode == StoreMode::Live {
            Some(search_sync::before_batch(&tx, batch)?)
        } else {
            None
        };
        let maintenance = match self.mode {
            StoreMode::Live => apply::LIVE_MAINTENANCE,
            StoreMode::Bulk | StoreMode::Progressive => apply::BULK_MAINTENANCE,
        };
        let bulk_load = matches!(self.mode, StoreMode::Bulk | StoreMode::Progressive);
        let mut stats = apply::apply_batch(
            &tx,
            batch,
            maintenance,
            bulk_load,
            self.mode == StoreMode::Progressive,
            fresh.as_mut(),
        )?;
        stats.image_assets = image_stats.assets;
        stats.image_files_written = image_stats.files_written;
        stats.image_files_reused = image_stats.files_reused;
        stats.image_bytes = image_stats.bytes;
        stats.image_write_nanos = image_stats.nanos;
        if let Some(search_before) = search_before {
            search_sync::record_batch(&tx, batch, search_before)?;
        }
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

    /// Canonicalize presentation ordinals, materialize every derived structure
    /// for a private cold rebuild, build deferred indexes, analyze the finished
    /// database, and validate it. Only after this succeeds is the store
    /// switched to ordinary live semantics.
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
        search_sync::mark_canonical_ready(&tx)?;
        let started = Instant::now();
        tx.commit()?;
        stats.commit_nanos = elapsed_nanos(started);
        let started = Instant::now();
        bulk::validate(&self.conn)?;
        stats.validate_nanos = elapsed_nanos(started);
        let started = Instant::now();
        bulk::restore_live_pragmas(&self.conn)?;
        stats.pragma_nanos = elapsed_nanos(started);
        stats.image_sync_nanos = self.images.finish_pending()?;
        self.mode = StoreMode::Live;
        self.bulk_abandoned = false;
        Ok(stats)
    }

    /// Finish an interrupted-or-fresh public cold load in place. Canonical
    /// rows have already been readable after each source commit; this gate
    /// materializes global ordinals and rollups, validates the whole file, and
    /// durably marks it canonical-ready. Search remains an independent worker.
    pub fn finish_progressive_load(&mut self) -> Result<BulkLoadStats, StoreError> {
        if self.mode != StoreMode::Progressive {
            return Err(StoreError::BulkLoadState {
                message: "store is not in progressive cold-load mode".to_string(),
            });
        }
        if self.bulk_abandoned {
            return Err(StoreError::BulkLoadState {
                message: "progressive cold load contains an abandoned source generation"
                    .to_string(),
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
        bulk::finish_progressive_pragmas(&self.conn)?;
        stats.pragma_nanos = elapsed_nanos(started);

        // Readiness is the final, tiny commit. Publishing it any earlier lets
        // the detached worker pin the large cold WAL while it builds search,
        // multiplying both I/O and startup wall time.
        let tx = self.conn.transaction()?;
        search_sync::mark_canonical_ready(&tx)?;
        let started = Instant::now();
        tx.commit()?;
        stats.commit_nanos = stats.commit_nanos.saturating_add(elapsed_nanos(started));
        self.mode = StoreMode::Live;
        self.bulk_abandoned = false;
        Ok(stats)
    }

    pub fn is_progressive(&self) -> bool {
        self.mode == StoreMode::Progressive
    }

    /// Change only the derived readiness badge for one session. Pipeline
    /// completion must not round-trip a projected `SessionRow`: doing so
    /// would feed the public title back as fresh explicit title evidence.
    pub fn set_session_stream_state(
        &mut self,
        session_id: &str,
        state: StreamState,
    ) -> Result<bool, StoreError> {
        let changed = self.conn.execute(
            "UPDATE session SET stream_state = ?2
             WHERE id = ?1 AND stream_state != ?2",
            rusqlite::params![session_id, state.as_str()],
        )?;
        if changed != 0
            && let Some(effects) = self.effects.as_mut()
        {
            effects.touched_sessions.insert(session_id.to_owned());
        }
        Ok(changed != 0)
    }

    /// Apply a search worker acknowledgement through the canonical single
    /// writer. A stale projection epoch is rejected without deleting repairs.
    pub fn acknowledge_search(
        &mut self,
        projection_epoch: &str,
        through_revision: u64,
    ) -> Result<bool, StoreError> {
        let tx = self.conn.transaction()?;
        let accepted = search_sync::acknowledge(&tx, projection_epoch, through_revision)?;
        tx.commit()?;
        Ok(accepted)
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
