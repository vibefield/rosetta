//! rosetta-store — the disposable canonical SQLite projection: schema, the
//! transactional write path, and the typed read surface. Foreign schema versions
//! fail closed on ordinary open; `rosettad rebuild` explicitly reconstructs and
//! atomically replaces the projection from the durable raw ledger.

pub mod apply;
pub mod assets;
mod bulk;
mod columns;
pub mod hot;
mod message_codec;
pub mod queries;
pub mod schema;
mod search_sync;
pub mod timeline_meta;
mod tool_codec;

use std::collections::BTreeSet;
use std::fmt;
use std::path::Path;
use std::time::Instant;

use rosetta_core::rows::StreamState;
use rusqlite::{Connection, OpenFlags};

pub use apply::{ApplyProfile, ApplyStats, Batch, CheckpointUpdate, SourceMode, SourceWrite};
pub use hot::{
    Effects, HotConfig, MetaEffects, MetaMirror, Mutation, Replay, RowScope, Stamp, TailRings,
};

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
    pub asset_sync_nanos: u64,
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
            Self::AssetInvalid { message } => write!(f, "invalid projection asset: {message}"),
            Self::CheckpointConflict { path } => {
                write!(f, "checkpoint CAS conflict for {path}")
            }
            Self::SchemaVersionMismatch { found, expected } => write!(
                f,
                "projection schema version {found} does not match {expected}; run an explicit rebuild, or a fresh source ingest when the new version admits metadata absent from the old ledger"
            ),
            Self::SourceConflict {
                cursor_id,
                generation,
                sequence,
            } => write!(
                f,
                "RAM-staged source conflict at cursor {cursor_id}, generation {generation}, sequence {sequence}"
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
    /// Whether the connection is a process-local SQLite database. Named
    /// shared-memory projections have a URI in `Connection::path()`, so the
    /// path alone cannot distinguish them from durable files at publish.
    transient_memory: bool,
    assets: assets::AssetStore,
    mode: StoreMode,
    /// Relational lifecycle fragments for an unpublished cold projection.
    /// The buffer follows committed source generations, then folds each tool
    /// correlation once before emitting final canonical rows to SQLite.
    bulk_reconciliation: apply::BulkReconciliationBuffer,
    /// Cross-file lifecycle fragments that have not met their opener yet.
    /// This is intentionally process-local: raw-ledger replay, not SQLite, is
    /// the recovery path after restart.
    ram_reconciliation: apply::RamReconciliationBuffer,
    /// An abandoned generation means the private projection was built from an
    /// incomplete source. Keep accepting batches so callers can finish their
    /// diagnostic pass, but never allow that scratch file to be published.
    bulk_abandoned: bool,
    /// A closed cold planner assigned final presentation ordinals before any
    /// message reached SQLite, so finalization need not rank and rewrite the
    /// complete message table.
    bulk_ordinals_precomputed: bool,
    /// Cold-capture planner: metadata/cursor control rows are projected into
    /// this private in-memory connection, while source payload batches remain
    /// process-local for the final closed writer.
    planned_batches: Option<Vec<Batch>>,
    /// Hot-tier observation: `Some` collects the row-level [`Effects`] of
    /// every committed batch until drained. `None` (the default) costs
    /// nothing — backfill and rebuild never allocate a mutation log.
    effects: Option<Effects>,
    /// The other granularity, independently switched: `Some` collects the
    /// identity keys every committed batch may have moved. Cheap enough for
    /// the cold paths, which suppress `effects` above.
    meta_effects: Option<MetaEffects>,
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
            transient_memory: false,
            assets: assets::AssetStore::for_projection(path),
            mode: StoreMode::Live,
            bulk_reconciliation: apply::BulkReconciliationBuffer::default(),
            ram_reconciliation: apply::RamReconciliationBuffer::default(),
            bulk_abandoned: false,
            bulk_ordinals_precomputed: false,
            planned_batches: None,
            effects: None,
            meta_effects: None,
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
            transient_memory: true,
            assets: assets::AssetStore::in_memory(),
            mode: StoreMode::Live,
            bulk_reconciliation: apply::BulkReconciliationBuffer::default(),
            ram_reconciliation: apply::RamReconciliationBuffer::default(),
            bulk_abandoned: false,
            bulk_ordinals_precomputed: false,
            planned_batches: None,
            effects: None,
            meta_effects: None,
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
            transient_memory: false,
            assets: assets::AssetStore::for_projection(path),
            mode: if state.canonical_ready {
                StoreMode::Live
            } else {
                StoreMode::Progressive
            },
            bulk_reconciliation: apply::BulkReconciliationBuffer::default(),
            ram_reconciliation: apply::RamReconciliationBuffer::default(),
            bulk_abandoned: false,
            bulk_ordinals_precomputed: false,
            planned_batches: None,
            effects: None,
            meta_effects: None,
        })
    }

    /// Open a brand-new private projection for ledger rebuild. Canonical
    /// presentation ordinals, derived FTS/buckets, and read-only secondary
    /// indexes are deferred until [`Store::finish_bulk_load`]. Existing
    /// schemas are rejected: this mode must never make an installed projection
    /// temporarily inconsistent.
    pub fn open_for_rebuild(path: &Path) -> Result<Self, StoreError> {
        let conn = Connection::open(path)?;
        Self::from_empty_connection_for_rebuild(
            conn,
            assets::AssetStore::for_projection(path),
            false,
        )
    }

    /// Open a private SQLite sibling whose projection members belong to the final
    /// projection's stable sidecar. Content-addressing lets the old and new
    /// databases share that directory safely across the atomic DB swap.
    pub fn open_for_rebuild_with_assets(
        path: &Path,
        projection: &Path,
    ) -> Result<Self, StoreError> {
        let conn = Connection::open(path)?;
        Self::from_empty_connection_for_rebuild(
            conn,
            assets::AssetStore::for_projection(projection),
            false,
        )
    }

    /// In-memory counterpart to [`Store::open_for_rebuild`], primarily for
    /// parity and regression tests of the cold-load path.
    pub fn open_in_memory_for_rebuild() -> Result<Self, StoreError> {
        Self::from_empty_connection_for_rebuild(
            Connection::open_in_memory()?,
            assets::AssetStore::in_memory(),
            true,
        )
    }

    /// In-memory canonical scratch projection with production asset
    /// materialization. The caller must back the validated connection up to a
    /// durable private file before atomic publication.
    pub fn open_in_memory_for_rebuild_with_assets(projection: &Path) -> Result<Self, StoreError> {
        Self::from_empty_connection_for_rebuild(
            Connection::open_in_memory()?,
            assets::AssetStore::for_projection(projection),
            true,
        )
    }

    /// A private canonical projection held in named process-local memory.
    /// Other daemon connections may attach to `uri` and read committed rows
    /// while the sole writer continues the cold build. The URI must use
    /// SQLite's `mode=memory&cache=shared`; it is never persisted in the
    /// canonical model or used as a recovery source.
    pub fn open_shared_in_memory_for_rebuild_with_assets(
        uri: &str,
        projection: &Path,
    ) -> Result<Self, StoreError> {
        // `cache=shared` in the URI is the scope switch. Match SQLite's
        // documented/rusqlite-tested shared-memory flags exactly.
        let flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_URI;
        let conn = Connection::open_with_flags(uri, flags)?;
        Self::from_empty_connection_for_rebuild(
            conn,
            assets::AssetStore::for_projection(projection),
            true,
        )
    }

    /// In-memory control plane for a fresh cold capture. It advances source
    /// cursors/generations so the ordinary framing drivers can run unchanged,
    /// but retains normalized source batches in RAM instead of projecting
    /// their canonical rows into a throwaway database.
    pub fn open_in_memory_for_cold_planning() -> Result<Self, StoreError> {
        let mut store = Self::open_in_memory_for_rebuild()?;
        store.planned_batches = Some(Vec::new());
        Ok(store)
    }

    pub fn take_planned_batches(&mut self) -> Result<Vec<Batch>, StoreError> {
        self.planned_batches
            .take()
            .ok_or_else(|| StoreError::BulkLoadState {
                message: "store is not a cold-capture planner".to_string(),
            })
    }

    /// Clone the canonical metadata batches captured so far without closing
    /// the planner. Fresh serve publishes these rows to its RAM projection
    /// immediately, then lets the same planner continue streaming. Source
    /// control batches and payload batches stay private until the runtime is
    /// closed and reconciled.
    pub fn planned_metadata_batches(&self) -> Result<Vec<Batch>, StoreError> {
        let planned = self
            .planned_batches
            .as_ref()
            .ok_or_else(|| StoreError::BulkLoadState {
                message: "store is not a cold-capture planner".to_string(),
            })?;
        Ok(planned
            .iter()
            .filter(|batch| batch.source.is_none() && !batch.ops.is_empty())
            .cloned()
            .collect())
    }

    fn from_empty_connection_for_rebuild(
        conn: Connection,
        assets: assets::AssetStore,
        transient_memory: bool,
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
            transient_memory,
            assets,
            mode: StoreMode::Bulk,
            bulk_reconciliation: apply::BulkReconciliationBuffer::default(),
            ram_reconciliation: apply::RamReconciliationBuffer::default(),
            bulk_abandoned: false,
            bulk_ordinals_precomputed: false,
            planned_batches: None,
            effects: None,
            meta_effects: None,
        })
    }

    /// Apply one batch in one transaction. On any error — including a
    /// checkpoint CAS conflict — nothing is committed, and nothing reaches
    /// the observed effects.
    pub fn apply(&mut self, batch: &Batch) -> Result<ApplyStats, StoreError> {
        // Session identity, topology, cwd, and project ownership are settled
        // only by the ledgered metadata batch. Keep this invariant at the
        // final write boundary so a future adapter cannot quietly restore
        // transcript-driven reassignment.
        if let Some(source) = &batch.source
            && batch
                .ops
                .iter()
                .any(|op| matches!(op, rosetta_core::IngestOp::SessionMeta(_)))
        {
            return Err(StoreError::SourceState {
                cursor_id: source.cursor_id,
                message: "source-scoped stream batch contains metadata-only session_meta"
                    .to_string(),
            });
        }
        if self.planned_batches.is_some() && batch.source.is_some() {
            return self.apply_planning_source(batch);
        }
        let asset_stats = self
            .assets
            .materialize_batch(batch, self.mode != StoreMode::Bulk)?;
        let ram_delta = self.ram_reconciliation.prepare(
            &self.conn,
            batch,
            matches!(self.mode, StoreMode::Bulk | StoreMode::Progressive),
            self.mode != StoreMode::Bulk,
        )?;
        let projected_batch = self.ram_reconciliation.projected_batch(batch, &ram_delta);
        let canonical_batch = projected_batch.as_ref().unwrap_or(batch);
        let bulk_delta = if self.mode == StoreMode::Bulk {
            apply::BulkReconciliationDelta::from_batch(batch, &self.conn, false)?
        } else {
            None
        };
        if let Some(delta) = bulk_delta.as_ref() {
            self.bulk_reconciliation.validate(delta)?;
        }
        let mut fresh = self.effects.as_ref().map(Effects::scoped_like);
        let mut fresh_meta = self.meta_effects.is_some().then(MetaEffects::default);
        let tx = self.conn.transaction()?;
        let search_before = if self.mode == StoreMode::Live {
            Some(search_sync::before_batch(&tx, canonical_batch)?)
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
            canonical_batch,
            maintenance,
            bulk_load,
            self.mode == StoreMode::Progressive,
            self.bulk_ordinals_precomputed,
            apply::Observation {
                rows: fresh.as_mut(),
                meta: fresh_meta.as_mut(),
            },
        )?;
        let ram_outcome = self.ram_reconciliation.resolve(&tx, &ram_delta)?;
        stats.projection_assets = asset_stats.assets;
        stats.asset_files_written = asset_stats.files_written;
        stats.asset_files_reused = asset_stats.files_reused;
        stats.asset_bytes = asset_stats.bytes;
        stats.asset_write_nanos = asset_stats.nanos;
        if let Some(search_before) = search_before {
            search_sync::record_batch(&tx, canonical_batch, search_before)?;
        }
        let started = Instant::now();
        tx.commit()?;
        stats.sqlite_commit_nanos = elapsed_nanos(started);
        if let Some(delta) = bulk_delta {
            self.bulk_reconciliation.commit(delta);
        }
        self.ram_reconciliation.commit(ram_outcome);
        self.ram_reconciliation.commit_delta(&ram_delta);
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
        if let (Some(pending), Some(committed)) = (self.meta_effects.as_mut(), fresh_meta) {
            pending.merge(committed);
        }
        if let Some(planned) = self.planned_batches.as_mut() {
            planned.push(batch.clone());
        }
        Ok(stats)
    }

    fn apply_planning_source(&mut self, batch: &Batch) -> Result<ApplyStats, StoreError> {
        let control = Batch {
            ops: Vec::new(),
            checkpoints: batch.checkpoints.clone(),
            source: batch.source.clone(),
        };
        let tx = self.conn.transaction()?;
        let mut stats = apply::apply_batch(
            &tx,
            &control,
            apply::BULK_MAINTENANCE,
            true,
            false,
            false,
            apply::Observation {
                rows: None,
                meta: None,
            },
        )?;
        let commit_started = Instant::now();
        tx.commit()?;
        stats.sqlite_commit_nanos = elapsed_nanos(commit_started);
        self.planned_batches
            .as_mut()
            .expect("planning source branch requires a planning buffer")
            .push(batch.clone());
        Ok(stats)
    }

    /// Apply closed first-generation source batches in one private
    /// transaction. The caller has already normalized, ordered, and sealed
    /// every source; there are no readers and the raw ledger remains the
    /// retry boundary, so per-source commits add cost without adding an
    /// observable correctness guarantee.
    pub fn apply_closed_bulk_sources(
        &mut self,
        batches: &[Batch],
    ) -> Result<ApplyStats, StoreError> {
        if self.mode != StoreMode::Bulk {
            return Err(StoreError::BulkLoadState {
                message: "closed source batches require a private cold rebuild".to_string(),
            });
        }

        let mut cursor_ids = BTreeSet::new();
        let mut deltas = Vec::with_capacity(batches.len());
        let mut asset_totals = Vec::with_capacity(batches.len());
        for batch in batches {
            let source = batch
                .source
                .as_ref()
                .ok_or_else(|| StoreError::BulkLoadState {
                    message: "closed bulk source batch is missing source identity".to_string(),
                })?;
            if !matches!(source.mode, SourceMode::Activate | SourceMode::Abandon) {
                return Err(StoreError::BulkLoadState {
                    message: "closed bulk source batch is not terminal".to_string(),
                });
            }
            if !cursor_ids.insert(source.cursor_id) {
                return Err(StoreError::BulkLoadState {
                    message: format!(
                        "closed bulk transaction repeats cursor {}",
                        source.cursor_id
                    ),
                });
            }
            if batch
                .ops
                .iter()
                .any(|op| matches!(op, rosetta_core::IngestOp::SessionMeta(_)))
            {
                return Err(StoreError::SourceState {
                    cursor_id: source.cursor_id,
                    message: "source-scoped stream batch contains metadata-only session_meta"
                        .to_string(),
                });
            }
            asset_totals.push(self.assets.materialize_batch(batch, false)?);
            let delta = apply::BulkReconciliationDelta::from_batch(batch, &self.conn, false)?
                .ok_or_else(|| StoreError::BulkLoadState {
                    message: "closed source batch did not produce reconciliation state".to_string(),
                })?;
            self.bulk_reconciliation.validate(&delta)?;
            deltas.push(delta);
        }

        let mut totals = ApplyStats::default();
        let tx = self.conn.transaction()?;
        for (batch, asset_stats) in batches.iter().zip(asset_totals) {
            let mut stats = apply::apply_batch(
                &tx,
                batch,
                apply::BULK_MAINTENANCE,
                true,
                false,
                self.bulk_ordinals_precomputed,
                apply::Observation {
                    rows: None,
                    meta: None,
                },
            )?;
            stats.projection_assets = asset_stats.assets;
            stats.asset_files_written = asset_stats.files_written;
            stats.asset_files_reused = asset_stats.files_reused;
            stats.asset_bytes = asset_stats.bytes;
            stats.asset_write_nanos = asset_stats.nanos;
            totals.merge_from(&stats);
        }
        let commit_started = Instant::now();
        tx.commit()?;
        totals.sqlite_commit_nanos = totals
            .sqlite_commit_nanos
            .saturating_add(elapsed_nanos(commit_started));
        for delta in deltas {
            self.bulk_reconciliation.commit(delta);
        }
        if batches.iter().any(|batch| {
            matches!(
                batch.source.as_ref().map(|source| source.mode),
                Some(SourceMode::Abandon)
            )
        }) {
            self.bulk_abandoned = true;
        }
        Ok(totals)
    }

    /// Close every activated private-cold source currently buffered, fold its
    /// relational tool lifecycle in RAM, and emit each final tool row once.
    /// Calling this outside private rebuild mode is a harmless no-op so the
    /// pipeline can place the boundary at the end of any runtime stream.
    pub fn flush_bulk_reconciliation(&mut self) -> Result<ApplyStats, StoreError> {
        if self.mode != StoreMode::Bulk {
            return Ok(ApplyStats::default());
        }

        let reconcile_started = Instant::now();
        let mut plan = self.bulk_reconciliation.prepare(&self.conn)?;
        let reconcile_nanos = elapsed_nanos(reconcile_started);
        if plan.is_empty() {
            return Ok(ApplyStats::default());
        }

        let tx = self.conn.transaction()?;
        let apply_started = Instant::now();
        let mut stats =
            apply::apply_bulk_reconciliation(&tx, &mut plan, self.bulk_ordinals_precomputed)?;
        stats.profile.tool_reconcile_nanos = reconcile_nanos;
        stats.canonical_apply_nanos = elapsed_nanos(apply_started);
        let commit_started = Instant::now();
        tx.commit()?;
        stats.sqlite_commit_nanos = elapsed_nanos(commit_started);
        self.bulk_reconciliation.committed(&plan);
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
        let _ = self.flush_bulk_reconciliation()?;
        let tx = self.conn.transaction()?;
        let mut stats = bulk::finalize(&tx, self.bulk_ordinals_precomputed)?;
        search_sync::mark_canonical_ready(&tx)?;
        let started = Instant::now();
        tx.commit()?;
        stats.commit_nanos = elapsed_nanos(started);
        bulk::enable_constraint_validation(&self.conn)?;
        let started = Instant::now();
        bulk::validate(&self.conn)?;
        stats.validate_nanos = elapsed_nanos(started);
        let started = Instant::now();
        bulk::restore_live_pragmas(&self.conn)?;
        stats.pragma_nanos = elapsed_nanos(started);
        stats.asset_sync_nanos = self.assets.finish_pending()?;
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
        let mut stats = bulk::finalize(&tx, false)?;
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

    pub fn is_bulk_rebuild(&self) -> bool {
        self.mode == StoreMode::Bulk
    }

    /// Admit final ordinals supplied by a closed cold planner. Call this
    /// before applying any source batch; live and progressive writers must
    /// continue deriving order from committed state.
    pub fn use_precomputed_bulk_ordinals(&mut self) -> Result<(), StoreError> {
        if self.mode != StoreMode::Bulk {
            return Err(StoreError::BulkLoadState {
                message: "precomputed ordinals require a private cold rebuild".to_string(),
            });
        }
        let messages: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM message", [], |row| row.get(0))?;
        if messages != 0 {
            return Err(StoreError::BulkLoadState {
                message: "precomputed ordinals must be enabled before message writes".to_string(),
            });
        }
        self.bulk_ordinals_precomputed = true;
        Ok(())
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
        if changed != 0 {
            if let Some(effects) = self.effects.as_mut() {
                effects.touch_session(session_id);
            }
            if let Some(meta) = self.meta_effects.as_mut() {
                meta.note_session(session_id);
            }
        }
        Ok(changed != 0)
    }

    /// Apply a closed cold batch's readiness decisions in one transaction.
    /// The single-row variant remains the live scheduler primitive; replay
    /// and RAM planning already know the complete runtime and must not pay one
    /// SQLite autocommit per session.
    pub fn set_session_stream_states(
        &mut self,
        states: &[(String, StreamState)],
    ) -> Result<u64, StoreError> {
        if states.is_empty() {
            return Ok(0);
        }
        let tx = self.conn.transaction()?;
        let mut changed_ids = Vec::new();
        {
            let mut update = tx.prepare_cached(
                "UPDATE session SET stream_state = ?2
                 WHERE id = ?1 AND stream_state != ?2",
            )?;
            for (session_id, state) in states {
                if update.execute(rusqlite::params![session_id, state.as_str()])? != 0 {
                    changed_ids.push(session_id.clone());
                }
            }
        }
        tx.commit()?;
        for session_id in &changed_ids {
            if let Some(effects) = self.effects.as_mut() {
                effects.touch_session(session_id);
            }
            if let Some(meta) = self.meta_effects.as_mut() {
                meta.note_session(session_id);
            }
        }
        Ok(u64::try_from(changed_ids.len()).unwrap_or(u64::MAX))
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

    /// Start collecting hot-tier effects for every session. The daemon
    /// enables this before its live loop and drains between ticks; leaving
    /// it enabled without draining trips the overflow flag rather than
    /// growing without bound. Widens a narrower armed scope in place.
    pub fn observe_effects(&mut self) {
        let effects = self.effects.get_or_insert_with(Effects::default);
        effects.scope = RowScope::All;
    }

    /// Start collecting hot-tier effects for the named sessions only — the
    /// cold paths' arming, refreshed from the hub's open session
    /// subscriptions between passes. Unwatched sessions collect nothing, so
    /// a backfill pass costs RAM only for transcripts a reader actually has
    /// open; their metadata keeps flowing through the always-on
    /// [`Store::observe_meta_effects`] channel. Re-arming replaces the scope
    /// for batches that follow; already-collected, undrained effects are
    /// kept.
    pub fn observe_effects_watched(&mut self, watched: BTreeSet<String>) {
        let effects = self.effects.get_or_insert_with(Effects::default);
        effects.scope = RowScope::Watched(watched);
    }

    /// Take everything observed since the last drain (empty when observation
    /// is disabled). Call strictly after the batches in question committed —
    /// which is guaranteed here, because [`Store::apply`] merges effects only
    /// on commit. The armed scope survives the drain.
    pub fn drain_effects(&mut self) -> Effects {
        match self.effects.as_mut() {
            Some(armed) => {
                let taken = std::mem::take(armed);
                armed.scope = taken.scope.clone();
                taken
            }
            None => Effects::default(),
        }
    }

    /// Start collecting metadata-tier effects. Independent of
    /// [`Store::observe_effects`] and cheap enough to enable for cold work:
    /// a batch contributes at most one id per identity row it projected, and
    /// there is no mutation log to cap or overflow.
    pub fn observe_meta_effects(&mut self) {
        if self.meta_effects.is_none() {
            self.meta_effects = Some(MetaEffects::default());
        }
    }

    /// Take every identity key observed since the last drain (empty when
    /// observation is disabled). Same commit discipline as
    /// [`Store::drain_effects`]: a rolled-back batch contributes nothing.
    pub fn drain_meta_effects(&mut self) -> MetaEffects {
        self.meta_effects
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

    /// True for anonymous and named process-local projections. Publication
    /// must back these pages up to the staged durable file before install.
    pub fn is_transient_memory(&self) -> bool {
        self.transient_memory
    }
}
