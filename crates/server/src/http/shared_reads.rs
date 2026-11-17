//! The reader half of the shared-cache lock discipline (R1).
//!
//! While a fresh build is unpublished, every canonical read opens against
//! the named shared-cache RAM generation the sole writer is still filling.
//! Each read snapshot runs the store's prologue right after
//! `BEGIN DEFERRED`, so its first table lock is `session` — the lock every
//! writer transaction takes first (`rosetta_store::lock_order`). A reader
//! can then wait on the writer but never close a cycle with it.
//!
//! The discipline rules out `SQLITE_LOCKED`, so a refusal here is a bug, not
//! contention; it is counted, the prologue gets one fresh connection, and a
//! second refusal — or one inside the read itself, which cannot be retried —
//! answers `not_ready` (retryable) instead of an internal error. A durable
//! file target is a WAL reader and runs no prologue.

use std::sync::Arc;
use std::time::Instant;

use rosetta_store::SharedCacheLocks;
use rosetta_store::lock_order::{is_locked, reader_prologue, take_locked_seen};
use rusqlite::Connection;

use super::{Failure, ProjectionReader, store_err};

const LOCKED: &str = "the canonical RAM view is momentarily locked; retry";

/// One read on the projection's current target: its snapshot opened, `read`
/// run in it, and the outcome settled — the whole closure timed into the
/// shared generation's lock evidence (its reader closure wall).
pub(super) fn run<T>(
    projection: &ProjectionReader,
    read: impl FnOnce(&Connection) -> Result<T, Failure>,
) -> Result<T, Failure> {
    let started = Instant::now();
    let (conn, locks) = begin(projection)?;
    let read = read(&conn);
    if let Some(locks) = &locks {
        locks
            .record_reader_closure(u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX));
    }
    finish(locks.as_deref(), read)
}

/// Open one read snapshot on the projection's current target.
fn begin(
    projection: &ProjectionReader,
) -> Result<(Connection, Option<Arc<SharedCacheLocks>>), Failure> {
    let (conn, locks) = projection.open()?;
    let Some(locks) = locks else {
        conn.execute_batch("BEGIN DEFERRED")
            .map_err(|error| store_err(error.into()))?;
        return Ok((conn, None));
    };
    let conn = begin_shared(
        &locks,
        conn,
        || projection.open().map(|(conn, _)| conn),
        reader_prologue,
    )?;
    // A refusal before this point is accounted for; only the read's count.
    take_locked_seen();
    Ok((conn, Some(locks)))
}

/// `BEGIN DEFERRED` and the prologue, timed; one fresh connection after a
/// lock refusal, then `not_ready`.
fn begin_shared(
    locks: &SharedCacheLocks,
    first: Connection,
    mut reopen: impl FnMut() -> Result<Connection, Failure>,
    prologue: impl Fn(&Connection) -> rusqlite::Result<()>,
) -> Result<Connection, Failure> {
    let mut conn = first;
    let mut retried = false;
    loop {
        let started = Instant::now();
        match conn
            .execute_batch("BEGIN DEFERRED")
            .and_then(|()| prologue(&conn))
        {
            Ok(()) => {
                let waited = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
                locks.record_reader_wait(waited);
                return Ok(conn);
            }
            Err(error) if is_locked(&error) && !retried => {
                locks.note_reader_retry();
                retried = true;
                // Dropping the refused connection ends its transaction.
                drop(conn);
                conn = reopen()?;
            }
            Err(error) if is_locked(&error) => {
                locks.note_reader_refusal();
                return Err(Failure::not_ready(LOCKED));
            }
            Err(error) => return Err(store_err(error.into())),
        }
    }
}

/// The read's outcome; a lock refusal inside a shared-generation read
/// answers `not_ready` and is counted.
fn finish<T>(locks: Option<&SharedCacheLocks>, read: Result<T, Failure>) -> Result<T, Failure> {
    let locked = take_locked_seen();
    match (read, locks) {
        (Err(_), Some(locks)) if locked => {
            locks.note_reader_refusal();
            Err(Failure::not_ready(LOCKED))
        }
        (read, _) => read,
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::time::Duration;

    use rosetta_core::{IngestOp, SessionRow};
    use rosetta_protocol::ErrorCode;
    use rosetta_store::{Batch, Store, StoreError, queries};
    use rusqlite::ffi;
    use serde_json::json;

    use super::*;

    fn locked() -> rusqlite::Error {
        rusqlite::Error::SqliteFailure(ffi::Error::new(ffi::SQLITE_LOCKED_SHAREDCACHE), None)
    }

    fn memory() -> Connection {
        Connection::open_in_memory().unwrap()
    }

    #[test]
    fn a_refused_prologue_retries_once_on_a_fresh_connection() {
        let locks = SharedCacheLocks::default();
        let refusals = Cell::new(1_u32);
        let reopened = Cell::new(0);
        let conn = begin_shared(
            &locks,
            memory(),
            || {
                reopened.set(reopened.get() + 1);
                Ok(memory())
            },
            |_| match refusals.replace(refusals.get().saturating_sub(1)) {
                0 => Ok(()),
                _ => Err(locked()),
            },
        );
        assert!(conn.is_ok());
        assert_eq!(reopened.get(), 1);
        let report = locks.report();
        assert_eq!(report.reader_locked_retries, 1);
        assert_eq!(report.reader_locked_refusals, 0);
        assert_eq!(report.streaming.reader_prologue_wait.count, 1);
    }

    #[test]
    fn a_second_refusal_answers_not_ready() {
        let locks = SharedCacheLocks::default();
        let refused = begin_shared(&locks, memory(), || Ok(memory()), |_| Err(locked()));
        let Err(Failure(status, error)) = refused else {
            panic!("a twice-refused prologue must not open a snapshot");
        };
        assert_eq!(status.as_u16(), 503);
        assert_eq!(error.code, ErrorCode::NotReady);
        let report = locks.report();
        assert_eq!(report.reader_locked_retries, 1);
        assert_eq!(report.reader_locked_refusals, 1);
        assert_eq!(report.streaming.reader_prologue_wait.count, 0);

        // Any other prologue error is internal, and is not retried.
        let failed = begin_shared(
            &locks,
            memory(),
            || panic!("no retry"),
            |_| Err(rusqlite::Error::QueryReturnedNoRows),
        );
        assert!(matches!(failed, Err(Failure(status, _)) if status.as_u16() == 500));
    }

    #[test]
    fn a_lock_refusal_inside_a_shared_read_answers_not_ready() {
        let locks = SharedCacheLocks::default();
        take_locked_seen();
        let read: Result<(), Failure> = Err(store_err(StoreError::from(locked())));
        let Err(Failure(status, _)) = finish(Some(&locks), read) else {
            panic!("refused");
        };
        assert_eq!(status.as_u16(), 503);
        assert_eq!(locks.report().reader_locked_refusals, 1);
        // Other failures and successes pass through untouched.
        let read: Result<(), Failure> = Err(Failure::not_found("gone"));
        assert!(
            matches!(finish(Some(&locks), read), Err(Failure(status, _)) if status.as_u16() == 404)
        );
        assert!(finish(Some(&locks), Ok(7)).is_ok());
        assert_eq!(locks.report().reader_locked_refusals, 1);
    }

    /// Fresh serve's canonical RAM generation holding one runtime and one
    /// session, with a read router attached to it.
    fn shared(name: &str) -> (Store, Arc<ProjectionReader>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let uri = format!(
            "file:rosetta-shared-reads-{name}-{}?mode=memory&cache=shared",
            std::process::id()
        );
        let mut store =
            Store::open_shared_in_memory_for_rebuild_with_assets(&uri, &dir.path().join("p"))
                .unwrap();
        let session: SessionRow = serde_json::from_value(json!({
            "id": "s1", "native_session_id": "s1", "runtime_id": "rt", "root_session_id": "s1",
            "role": "main", "started_at": 1, "updated_at": 1, "stream_state": "pending"
        }))
        .unwrap();
        store
            .apply(&Batch {
                ops: vec![
                    IngestOp::Runtime(
                        serde_json::from_value(json!({
                            "id": "rt", "kind": "claude-code", "data_root": "/agent",
                            "enabled": true, "discovered_at": 1
                        }))
                        .unwrap(),
                    ),
                    IngestOp::SessionMeta(session),
                ],
                ..Batch::default()
            })
            .unwrap();
        let projection = Arc::new(ProjectionReader::shared_memory(&uri).unwrap());
        (store, projection, dir)
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap()
    }

    #[test]
    fn a_shared_read_waits_for_the_writers_transaction_and_records_it() {
        let (mut store, projection, _dir) = shared("waits");
        let runtime = runtime();

        let tx = store.writer_tx().unwrap();
        let read = runtime.spawn(super::super::with_read(projection.clone(), |conn| {
            queries::session(conn, "s1").map_err(store_err)
        }));
        std::thread::sleep(Duration::from_millis(300));
        assert!(!read.is_finished(), "R1 waits for the writer's W1");
        tx.commit().unwrap();
        let session = runtime.block_on(read).unwrap().ok().flatten();
        assert_eq!(session.map(|row| row.id), Some("s1".to_string()));

        let report = projection.shared_cache_locks().unwrap().report();
        assert_eq!(report.streaming.reader_prologue_wait.count, 1);
        // Slack for a late blocking-pool start; the read did wait on W1.
        assert!(report.streaming.reader_prologue_wait.max_ms >= 100.0);
        // The whole read closure includes that wait (P3's closure wall).
        assert_eq!(report.streaming.reader_closure_wall.count, 1);
        assert!(
            report.streaming.reader_closure_wall.max_ms
                >= report.streaming.reader_prologue_wait.max_ms
        );
        assert_eq!(
            report.reader_locked_retries + report.reader_locked_refusals,
            0
        );
        assert!(Arc::ptr_eq(
            &projection.shared_cache_locks().unwrap(),
            &store.shared_cache_locks().unwrap()
        ));
    }

    #[test]
    fn a_projection_state_first_read_never_deadlocks_finalization() {
        // The artifacts list and the watermark reads open with
        // projection_state, which finalization's refold writes after its
        // W1. Without R1 the read holds projection_state, the refold holds
        // session and waits on it, and the read's session lookup closes the
        // cycle. With R1 the read holds session first, so W1 waits instead.
        let (mut store, projection, _dir) = shared("finalize");
        let runtime = runtime();
        let (watermark_read, watermarked) = std::sync::mpsc::channel();
        let (resume, resumed) = std::sync::mpsc::channel::<()>();
        let read = runtime.spawn(super::super::with_read(projection.clone(), move |conn| {
            queries::projection_state(conn).map_err(store_err)?;
            watermark_read.send(()).unwrap();
            resumed.recv_timeout(Duration::from_secs(10)).unwrap();
            queries::session(conn, "s1").map_err(store_err)
        }));
        watermarked.recv_timeout(Duration::from_secs(10)).unwrap();
        let writer = std::thread::spawn(move || {
            let finished = store.finish_bulk_load();
            (store, finished)
        });
        // Long enough for tx A to reach the refold's projection_state write
        // if nothing held it at W1.
        std::thread::sleep(Duration::from_millis(500));
        resume.send(()).unwrap();
        let session = runtime.block_on(read).unwrap();
        let (_store, finished) = writer.join().unwrap();
        let session = session.map_err(|Failure(status, error)| (status, error.message));
        assert_eq!(
            session.unwrap().map(|row| row.id),
            Some("s1".to_string()),
            "the read finishes on its own snapshot"
        );
        finished.expect("finalization commits once the read is done");
        let report = projection.shared_cache_locks().unwrap().report();
        assert_eq!(report.finalize.reader_prologue_wait.count, 0);
        assert_eq!(report.finalize.session_lock_wait.count, 1, "tx A only");
        assert_eq!(
            report.reader_locked_retries
                + report.reader_locked_refusals
                + report.writer_lock_order_violations,
            0,
            "{report:#?}"
        );
    }
}
