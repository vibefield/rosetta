//! The background rendition sweep. After publication and the initial live
//! catch-up, and only once search is Ready, one utility-QoS thread paced at
//! 50% of one core makes the display renditions the room shows (thumbs and
//! preview, from one decode) for every image an attachment names, newest
//! sessions first. Full of an image over 1600 px waits for its first 1:1
//! view; until the sweep reaches an image, its first view makes what it
//! needs. Nothing is queued or persisted: a warm start drops finished bases
//! by `stat` alone and redoes the rest.
//!
//! The sweep never writes SQLite: it reads its work list in short pages on a
//! read-only connection that it closes before the first encode. It never
//! waits on the codec pool (a refused slot parks and retries, so a stop is
//! seen within [`ADMISSION_POLL`]) and it skips an image a viewer is making.
//! A stop request is also checked between animation frames.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use rosetta_media::{CodecPermit, Lane, MediaError};
use rosetta_search::{Fts5SidecarSearch, SearchBackend, SearchReadiness};
use rosetta_store::assets::image_refs::{ImageWork, image_work};
use rosetta_store::assets::renditions::{
    Ensured, RenditionError, ensure, is_complete, remove_stale_tempfiles,
};
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};

use crate::maintenance_pace::{Pace, lower_priority, thread_cpu_time};

const CPU_PERCENT: u32 = 50;
/// Older rendition tempfiles were left by an interrupted write.
const TEMPFILE_AGE: Duration = Duration::from_secs(10 * 60);
const ADMISSION_POLL: Duration = Duration::from_millis(50);

pub(crate) struct BackgroundRenditions {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

/// Start the sweep over the projection `conn` has open, gated on `search`
/// being Ready. Nothing starts when `--image-renditions off` was given, when
/// the daemon is stopping, or for a projection that is not a file.
pub(crate) fn start_after_catch_up(
    enabled: bool,
    conn: &Connection,
    search: &Fts5SidecarSearch,
    stop: &AtomicBool,
) -> Option<BackgroundRenditions> {
    if !enabled || stop.load(Ordering::SeqCst) {
        return None;
    }
    let db = PathBuf::from(conn.path().filter(|path| !path.is_empty())?);
    let search = search.clone();
    match BackgroundRenditions::start(db, move || {
        search.state().readiness == SearchReadiness::Ready
    }) {
        Ok(worker) => Some(worker),
        Err(error) => {
            eprintln!("image renditions deferred: {error}");
            None
        }
    }
}

/// Reclaim image members and rendition caches no row names, right after a
/// fresh RAM projection is installed and before any live runner writes. A
/// failure leaves everything for the next cleanup, as after a rebuild.
pub(crate) fn prune_after_publication(conn: &Connection, db: &Path) {
    if let Err(error) = rosetta_store::assets::prune_unreferenced_images(conn, db) {
        eprintln!("image cache cleanup deferred: {error}");
    }
}

impl BackgroundRenditions {
    pub(crate) fn start(
        db: PathBuf,
        ready: impl Fn() -> bool + Send + 'static,
    ) -> io::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let thread = thread::Builder::new()
            .name("rosetta-image-renditions".into())
            .spawn(move || {
                lower_priority("image renditions");
                let mut pace = Pace::new(stopped.clone(), CPU_PERCENT, ready);
                sweep(
                    &db,
                    &mut pace,
                    &|| stopped.load(Ordering::Acquire),
                    &mut |event| eprintln!("{event}"),
                );
            })?;
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for BackgroundRenditions {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread.thread().unpark();
            // Join before the caller can release the projection lock. Every
            // wait is a park the unpark ends; an encode ends at its next frame.
            let _ = thread.join();
        }
    }
}

/// Where the sweep yields: CPU pacing, the readiness gate and stop.
trait Checkpoint {
    /// Errs once the sweep must stop.
    fn checkpoint(&mut self) -> io::Result<()>;
    /// Time spent waiting is not CPU credit for the next image.
    fn idle(&mut self);
}

impl<F: Fn() -> bool> Checkpoint for Pace<F> {
    fn checkpoint(&mut self) -> io::Result<()> {
        Pace::checkpoint(self)
    }

    fn idle(&mut self) {
        self.reset_window();
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Tally {
    bases: u64,
    complete: u64,
    made: u64,
    busy: u64,
    failed: u64,
    missing: u64,
    files: u64,
    bytes: u64,
}

/// One run: tempfile cleanup, the work list, then each unfinished base.
/// Emits `started` and then `finished`, `stopped` or `deferred`; nothing when
/// stopped before `started` (at the gate, or while the work list and its stat
/// pass were read). Returns the final state.
fn sweep(
    db: &Path,
    pace: &mut dyn Checkpoint,
    stopped: &dyn Fn() -> bool,
    emit: &mut dyn FnMut(Value),
) -> &'static str {
    if pace.checkpoint().is_err() {
        return "stopped";
    }
    let started = Instant::now();
    let cpu_started = thread_cpu_time();
    let event = |state: &str, tally: Tally, error: Option<String>| {
        json!({"event": "image-renditions", "state": state,
            "bases": tally.bases, "complete": tally.complete, "made": tally.made,
            "busy": tally.busy, "failed": tally.failed, "missing": tally.missing,
            "files": tally.files, "bytes": tally.bytes,
            "cpuSeconds": cpu_started.zip(thread_cpu_time())
                .map(|(before, after)| after.saturating_sub(before).as_secs_f64()),
            "wallSeconds": started.elapsed().as_secs_f64(),
            "cpuBudgetPercent": CPU_PERCENT, "error": error})
    };
    let root = rosetta_store::assets::asset_root(db);
    if let Err(error) = remove_stale_tempfiles(&root, TEMPFILE_AGE) {
        eprintln!("image renditions: stale tempfiles kept: {error}");
    }
    let mut tally = Tally::default();
    // The work list and the stat pass grow with the projection, so both stop
    // within one page or one base: Drop never waits for a whole scan.
    let work = match read_work(db, stopped) {
        Ok(Some(work)) => work,
        Ok(None) => return "stopped",
        Err(error) => {
            emit(event("deferred", tally, Some(error)));
            return "deferred";
        }
    };
    tally.bases = work.len() as u64;
    let mut pending = Vec::new();
    for item in work {
        if stopped() {
            return "stopped";
        }
        if !is_complete(&root, &item.name, item.dimensions) {
            pending.push(item);
        }
    }
    tally.complete = tally.bases - pending.len() as u64;
    emit(event("started", tally, None));
    let (mut state, mut error) = ("finished", None);
    for item in pending {
        if pace.checkpoint().is_err() {
            state = "stopped";
            break;
        }
        let Some(permit) = background_slot(pace, stopped) else {
            state = "stopped";
            break;
        };
        match ensure(&permit, &root, &item.name, item.dimensions, stopped) {
            Ok(Ensured::Complete) => tally.complete += 1,
            Ok(Ensured::Busy) => tally.busy += 1,
            Ok(Ensured::Made { files, bytes }) => {
                tally.made += 1;
                tally.files += files;
                tally.bytes += bytes;
            }
            Err(RenditionError::BaseMissing) => tally.missing += 1,
            Err(RenditionError::Media(MediaError::Stopped)) => {
                state = "stopped";
                break;
            }
            // Disk full or an unwritable cache: the next start retries.
            Err(RenditionError::Io(failure)) => {
                (state, error) = ("deferred", Some(failure.to_string()));
                break;
            }
            // Counted, and retried by the next start only.
            Err(RenditionError::BaseCorrupt | RenditionError::Media(_)) => tally.failed += 1,
        }
    }
    emit(event(state, tally, error));
    state
}

/// A background codec slot, polled without blocking so a stop is never
/// stuck behind foreground work.
fn background_slot(pace: &mut dyn Checkpoint, stopped: &dyn Fn() -> bool) -> Option<CodecPermit> {
    let mut waited = false;
    loop {
        if stopped() {
            return None;
        }
        if let Some(permit) = rosetta_media::try_admit(Lane::Background) {
            if waited {
                pace.idle();
            }
            return Some(permit);
        }
        waited = true;
        thread::park_timeout(ADMISSION_POLL);
    }
}

/// The work list, from a read-only connection closed before any encode;
/// `None` when `stopped` ended the read.
fn read_work(db: &Path, stopped: &dyn Fn() -> bool) -> Result<Option<Vec<ImageWork>>, String> {
    let conn = Connection::open_with_flags(
        db,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| format!("opening projection: {error}"))?;
    let work = image_work(&conn, stopped).map_err(|error| format!("reading image work: {error}"));
    drop(conn);
    work
}

#[cfg(test)]
mod tests;
