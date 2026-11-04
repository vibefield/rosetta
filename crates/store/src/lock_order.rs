//! The shared-cache lock discipline of the canonical RAM generation.
//!
//! Fresh `serve` builds the canonical projection in a named shared-cache
//! memory database while HTTP/WS readers attach to it. Shared cache locks
//! whole tables until a transaction ends, and `unlock_notify` (the build's
//! rusqlite feature) answers a waits-for cycle with an immediate
//! `SQLITE_LOCKED` for whichever side closes it. Source transactions lock
//! `ingest_cursor` → `message` → `session`, and readers lock `session` →
//! `message`, so without an order one timeline read could fail the sole
//! canonical writer's transaction and park the whole build. Retrying the
//! writer is not an option: usage ownership, assets and reconciliation
//! mutate RAM inside the transaction and are not rolled back with it.
//!
//! The discipline, active only on the shared generation:
//!
//! - **W1.** Every writer transaction opens through `writer_tx`, whose
//!   first statement takes the `session` write lock ([`SESSION_LOCK`]). A
//!   reader that holds any table lock also holds `session` (R1), so once the
//!   writer has `session` no reader holds anything it will need, and before
//!   that the writer holds nothing a reader needs: it waits, never cycles.
//!   Shared cache stops new readers while a writer's lock is pending, so a
//!   stream of readers cannot starve it.
//! - **W2.** A DDL transaction opens through `ddl_tx`, runs DDL first and
//!   never takes W1 or writes `session`: DDL needs the schema write lock,
//!   which every open read transaction blocks — including a reader parked at
//!   its prologue behind W1, so W1 followed by DDL would be a cycle.
//! - **R1.** Every reader of the shared generation runs [`reader_prologue`]
//!   right after `BEGIN DEFERRED`, so its first table lock is `session`.
//!
//! A single autocommit statement holds no lock while it waits, so the
//! single-row readiness, consent and audit writes need no W1. Both W1 and
//! R1 bind `''`, which names no session: SQLite takes a statement's table
//! locks in its prologue, before any row is matched
//! (`shared_cache_lock_order.rs` pins that).
//!
//! The waits are measurement only: the writer's W1 waits and the readers'
//! prologue waits, split at finalization, with the readers' lock retries
//! and refusals ([`SharedCacheLocks::report`]).

use std::cell::Cell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Instant;

use rusqlite::{Connection, ErrorCode, Transaction};
use serde::Serialize;

use crate::{Store, StoreError, elapsed_nanos};

/// W1: a zero-row update that takes the `session` write lock.
pub const SESSION_LOCK: &str = "UPDATE session SET updated_at = updated_at WHERE id = ?1";
/// R1: a zero-row read that takes the `session` read lock.
pub const READER_PROLOGUE: &str = "SELECT 1 FROM session WHERE id = ?1";
/// The id W1 and R1 bind. Canonical session ids are digests, never empty.
const NO_SESSION: &str = "";

/// Whether an error is a shared-cache table or schema lock refusal: an
/// unlock-notify deadlock or a conflict nothing waited out.
pub fn is_locked(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(failure, _) if failure.code == ErrorCode::DatabaseLocked
    )
}

thread_local! {
    static LOCKED_SEEN: Cell<bool> = const { Cell::new(false) };
}

/// Every SQLite error that becomes a [`StoreError`] passes here, so a
/// reader can learn that a read failed on a lock however its route
/// formatted the failure.
pub(crate) fn note(error: &rusqlite::Error) {
    if is_locked(error) {
        LOCKED_SEEN.with(|seen| seen.set(true));
    }
}

/// Whether a lock refusal reached a [`StoreError`] on this thread since the
/// last call. Readers clear it before a read and check it after.
pub fn take_locked_seen() -> bool {
    LOCKED_SEEN.with(|seen| seen.replace(false))
}

/// Open one writer transaction: W1 first on the shared generation (timed,
/// and returned as the wait), an ordinary deferred transaction anywhere
/// else. `site` names the transaction in a lock-order error.
pub(crate) fn writer_tx<'c>(
    conn: &'c mut Connection,
    locks: Option<&SharedCacheLocks>,
    site: &'static str,
) -> Result<(Transaction<'c>, u64), StoreError> {
    let tx = conn.transaction()?;
    let Some(locks) = locks else {
        return Ok((tx, 0));
    };
    let started = Instant::now();
    tx.prepare_cached(SESSION_LOCK)
        .and_then(|mut lock| lock.execute([NO_SESSION]))
        .map_err(|error| StoreError::from(error).in_writer_tx(Some(locks), site))?;
    let waited = elapsed_nanos(started);
    locks.record_writer_wait(waited);
    Ok((tx, waited))
}

/// Open one DDL transaction (W2). Its first statement must be DDL, and it
/// must never write `session`: the schema write lock is what keeps readers
/// out, and it can only be taken while no read transaction is open.
pub(crate) fn ddl_tx(conn: &mut Connection) -> Result<Transaction<'_>, StoreError> {
    Ok(conn.transaction()?)
}

/// R1, for a reader of the shared generation, right after `BEGIN DEFERRED`.
pub fn reader_prologue(conn: &Connection) -> rusqlite::Result<()> {
    conn.prepare_cached(READER_PROLOGUE)?
        .exists([NO_SESSION])
        .map(|_| ())
}

impl Store {
    /// The lock evidence of a shared RAM generation; `None` for every other
    /// store.
    pub fn shared_cache_locks(&self) -> Option<Arc<SharedCacheLocks>> {
        self.shared_readers.clone()
    }

    /// Open one writer transaction under the shared-cache lock discipline:
    /// on the shared RAM generation its first statement takes the `session`
    /// write lock (W1); on every other store it is an ordinary deferred
    /// transaction. Never use it for DDL (W2). Store code that must borrow
    /// other fields while the transaction is open uses
    /// `lock_order::writer_tx` on `self.conn` directly.
    pub fn writer_tx(&mut self) -> Result<rusqlite::Transaction<'_>, StoreError> {
        writer_tx(&mut self.conn, self.shared_readers.as_deref(), "writer").map(|(tx, _)| tx)
    }

    /// Name a lock refusal of the writer transaction `site` (see
    /// [`StoreError::LockOrder`]).
    pub(crate) fn lock_order(
        &self,
        site: &'static str,
    ) -> impl FnOnce(StoreError) -> StoreError + '_ {
        move |error| error.in_writer_tx(self.shared_readers.as_deref(), site)
    }
}

impl StoreError {
    /// Name a lock refusal inside a writer transaction on the shared
    /// generation: under the discipline it cannot happen, so it is a bug in
    /// the order, never contention. Other errors pass unchanged.
    pub(crate) fn in_writer_tx(self, locks: Option<&SharedCacheLocks>, tx: &'static str) -> Self {
        match (locks, self) {
            (Some(locks), Self::Sqlite(source)) if is_locked(&source) => {
                locks.writer_violations.fetch_add(1, Ordering::Relaxed);
                Self::LockOrder { tx, source }
            }
            (_, other) => other,
        }
    }
}

/// Lock evidence of one shared-cache generation, shared by its writer and
/// its readers through the generation's URI.
#[derive(Debug, Default)]
pub struct SharedCacheLocks {
    finalizing: AtomicBool,
    writer: Mutex<Windows>,
    readers: Mutex<Windows>,
    /// Each reader's whole read closure: prologue, queries and finish.
    closures: Mutex<Windows>,
    reader_locked_retries: AtomicU64,
    reader_locked_refusals: AtomicU64,
    writer_violations: AtomicU64,
}

#[derive(Debug, Default)]
struct Windows {
    streaming: LockWaits,
    finalize: LockWaits,
}

impl Windows {
    fn current(&mut self, finalizing: bool) -> &mut LockWaits {
        if finalizing {
            &mut self.finalize
        } else {
            &mut self.streaming
        }
    }
}

type Registry = Mutex<HashMap<String, Weak<SharedCacheLocks>>>;

impl SharedCacheLocks {
    /// The evidence of the shared-cache database named `uri`: SQLite keys
    /// a shared cache by its name, and so does this, so a writer and the
    /// readers attached to it record into one place without new plumbing.
    pub fn for_uri(uri: &str) -> Arc<Self> {
        static REGISTRY: OnceLock<Registry> = OnceLock::new();
        let mut registry = REGISTRY
            .get_or_init(Registry::default)
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        registry.retain(|_, locks| locks.strong_count() > 0);
        if let Some(locks) = registry.get(uri).and_then(Weak::upgrade) {
            return locks;
        }
        let locks = Arc::new(Self::default());
        registry.insert(uri.to_string(), Arc::downgrade(&locks));
        locks
    }

    /// Later waits belong to the finalization window.
    pub(crate) fn begin_finalize(&self) {
        self.finalizing.store(true, Ordering::Relaxed);
    }

    fn record_writer_wait(&self, nanos: u64) {
        record(&self.writer, self.finalizing.load(Ordering::Relaxed), nanos);
    }

    /// One reader prologue's wait.
    pub fn record_reader_wait(&self, nanos: u64) {
        record(
            &self.readers,
            self.finalizing.load(Ordering::Relaxed),
            nanos,
        );
    }

    /// One reader's whole read closure, from opening its snapshot to its
    /// last row (P3 gates its streaming-window p99).
    pub fn record_reader_closure(&self, nanos: u64) {
        record(
            &self.closures,
            self.finalizing.load(Ordering::Relaxed),
            nanos,
        );
    }

    /// A reader's prologue was refused and it retried on a fresh connection.
    pub fn note_reader_retry(&self) {
        self.reader_locked_retries.fetch_add(1, Ordering::Relaxed);
    }

    /// A reader answered `not_ready` because of a lock refusal.
    pub fn note_reader_refusal(&self) {
        self.reader_locked_refusals.fetch_add(1, Ordering::Relaxed);
    }

    pub fn report(&self) -> SharedCacheLockReport {
        let writer = lock(&self.writer);
        let readers = lock(&self.readers);
        let closures = lock(&self.closures);
        SharedCacheLockReport {
            streaming: WindowReport {
                session_lock_wait: writer.streaming.summary(),
                reader_prologue_wait: readers.streaming.summary(),
                reader_closure_wall: closures.streaming.summary(),
            },
            finalize: WindowReport {
                session_lock_wait: writer.finalize.summary(),
                reader_prologue_wait: readers.finalize.summary(),
                reader_closure_wall: closures.finalize.summary(),
            },
            reader_locked_retries: self.reader_locked_retries.load(Ordering::Relaxed),
            reader_locked_refusals: self.reader_locked_refusals.load(Ordering::Relaxed),
            writer_lock_order_violations: self.writer_violations.load(Ordering::Relaxed),
        }
    }
}

fn lock(windows: &Mutex<Windows>) -> std::sync::MutexGuard<'_, Windows> {
    windows
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn record(windows: &Mutex<Windows>, finalizing: bool, nanos: u64) {
    lock(windows).current(finalizing).record(nanos);
}

/// Everything [`SharedCacheLocks`] measured, for the stage profiles.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SharedCacheLockReport {
    /// From the generation's first transaction to finalization.
    pub streaming: WindowReport,
    /// `finish_bulk_load` onward, until readers switch to disk.
    pub finalize: WindowReport,
    pub reader_locked_retries: u64,
    pub reader_locked_refusals: u64,
    pub writer_lock_order_violations: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowReport {
    pub session_lock_wait: LockWaitSummary,
    pub reader_prologue_wait: LockWaitSummary,
    pub reader_closure_wall: LockWaitSummary,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LockWaitSummary {
    pub count: u64,
    pub total_ms: f64,
    pub p50_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
}

/// Sub-buckets per power of two: a reported percentile is at most 1/8
/// above the true nearest-rank value (and never above the maximum).
const SUB_BITS: u32 = 3;
const SUB: usize = 1 << SUB_BITS;
const BUCKETS: usize = (64 - SUB_BITS as usize + 1) * SUB;

/// A bounded log-linear histogram of waits in nanoseconds: 4 KiB however
/// many transactions or requests a build sees.
#[derive(Debug, Clone, Default)]
pub(crate) struct LockWaits {
    buckets: Vec<u64>,
    count: u64,
    total: u64,
    max: u64,
}

fn bucket(nanos: u64) -> usize {
    if nanos < SUB as u64 {
        return nanos as usize;
    }
    let shift = (63 - nanos.leading_zeros()) - SUB_BITS;
    let mantissa = ((nanos >> shift) as usize) & (SUB - 1);
    (shift as usize + 1) * SUB + mantissa
}

/// The largest value [`bucket`] maps to `index`.
fn bucket_ceiling(index: usize) -> u64 {
    if index < SUB {
        return index as u64;
    }
    let shift = (index / SUB - 1) as u32;
    let mantissa = (index % SUB) as u128;
    let ceiling = ((SUB as u128 + mantissa + 1) << shift) - 1;
    u64::try_from(ceiling).unwrap_or(u64::MAX)
}

impl LockWaits {
    fn record(&mut self, nanos: u64) {
        if self.buckets.is_empty() {
            self.buckets = vec![0; BUCKETS];
        }
        self.buckets[bucket(nanos)] += 1;
        self.count += 1;
        self.total = self.total.saturating_add(nanos);
        self.max = self.max.max(nanos);
    }

    /// Nearest-rank percentile in nanoseconds; 0 when empty.
    fn percentile(&self, percent: u64) -> u64 {
        if self.count == 0 {
            return 0;
        }
        let rank = (self.count.saturating_mul(percent.min(100)))
            .div_ceil(100)
            .max(1);
        let mut seen = 0;
        for (index, count) in self.buckets.iter().enumerate() {
            seen += count;
            if seen >= rank {
                return bucket_ceiling(index).min(self.max);
            }
        }
        self.max
    }

    fn summary(&self) -> LockWaitSummary {
        let ms = |nanos: u64| nanos as f64 / 1e6;
        LockWaitSummary {
            count: self.count,
            total_ms: ms(self.total),
            p50_ms: ms(self.percentile(50)),
            p99_ms: ms(self.percentile(99)),
            max_ms: ms(self.max),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buckets_cover_every_value_and_their_ceilings_bound_them() {
        for nanos in (0..4096).chain([u64::MAX / 3, u64::MAX - 1, u64::MAX]) {
            let index = bucket(nanos);
            assert!(index < BUCKETS, "{nanos}");
            assert!(bucket_ceiling(index) >= nanos, "{nanos}");
            if index > 0 {
                assert!(bucket_ceiling(index - 1) < nanos, "{nanos}");
            }
        }
    }

    #[test]
    fn percentiles_are_within_an_eighth_of_nearest_rank_and_never_past_the_max() {
        let mut waits = LockWaits::default();
        assert_eq!(waits.percentile(50), 0);
        let values: Vec<u64> = (1..=1000).map(|n| n * 1_000).collect();
        for value in &values {
            waits.record(*value);
        }
        for percent in [1, 50, 90, 99, 100] {
            let exact = values[(values.len() * percent as usize).div_ceil(100) - 1];
            let reported = waits.percentile(percent);
            assert!(
                reported >= exact && reported <= exact + exact / 8,
                "{percent}"
            );
        }
        assert_eq!(waits.percentile(100), 1_000_000);
        let summary = waits.summary();
        assert_eq!(summary.count, 1000);
        assert_eq!(summary.max_ms, 1.0);
    }

    #[test]
    fn a_lock_refusal_is_named_only_on_the_shared_generation() {
        let locked = || {
            StoreError::Sqlite(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_LOCKED_SHAREDCACHE),
                Some("database table is locked: session".into()),
            ))
        };
        let locks = SharedCacheLocks::default();
        let named = locked().in_writer_tx(Some(&locks), "apply");
        assert!(matches!(named, StoreError::LockOrder { tx: "apply", .. }));
        assert_eq!(
            named.to_string(),
            "shared-cache lock-order violation in apply: database table is locked: session"
        );
        assert_eq!(locks.report().writer_lock_order_violations, 1);
        // Elsewhere SQLITE_LOCKED is a same-connection conflict, not order.
        assert!(matches!(
            locked().in_writer_tx(None, "apply"),
            StoreError::Sqlite(_)
        ));
        let other = StoreError::BulkLoadState {
            message: "x".into(),
        };
        assert!(matches!(
            other.in_writer_tx(Some(&locks), "apply"),
            StoreError::BulkLoadState { .. }
        ));
        assert_eq!(locks.report().writer_lock_order_violations, 1);
    }

    #[test]
    fn a_lock_refusal_marks_the_thread_until_taken() {
        assert!(!take_locked_seen());
        let _ = StoreError::from(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_LOCKED),
            None,
        ));
        assert!(take_locked_seen());
        assert!(!take_locked_seen());
        let _ = StoreError::from(rusqlite::Error::QueryReturnedNoRows);
        assert!(!take_locked_seen());
    }

    #[test]
    fn writers_and_readers_of_one_uri_share_their_evidence() {
        let uri = "file:lock-order-registry?mode=memory&cache=shared";
        let writer = SharedCacheLocks::for_uri(uri);
        let reader = SharedCacheLocks::for_uri(uri);
        assert!(Arc::ptr_eq(&writer, &reader));
        assert!(!Arc::ptr_eq(
            &writer,
            &SharedCacheLocks::for_uri("file:other?mode=memory&cache=shared")
        ));
        writer.record_writer_wait(2_000_000);
        reader.record_reader_wait(1_000_000);
        reader.record_reader_closure(40_000_000);
        reader.begin_finalize();
        reader.record_reader_wait(3_000_000);
        reader.note_reader_retry();
        let report = writer.report();
        assert_eq!(report.streaming.session_lock_wait.count, 1);
        assert_eq!(report.streaming.reader_prologue_wait.max_ms, 1.0);
        assert_eq!(report.streaming.reader_closure_wall.max_ms, 40.0);
        assert_eq!(report.finalize.reader_closure_wall.count, 0);
        assert_eq!(report.finalize.reader_prologue_wait.max_ms, 3.0);
        assert_eq!(report.finalize.session_lock_wait.count, 0);
        assert_eq!(report.reader_locked_retries, 1);
        drop((writer, reader));
        // A finished generation's evidence is released with it.
        assert_eq!(
            SharedCacheLocks::for_uri(uri)
                .report()
                .reader_locked_retries,
            0
        );
    }
}
