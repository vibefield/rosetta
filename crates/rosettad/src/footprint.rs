//! What the daemon keeps on this machine and holds in memory: `/api/status`'s
//! `storage` and `memory` (2026-11-23), which a host's import sign reads.
//!
//! A status read never walks a directory. It takes the last walk's figures and,
//! when they are older than the walk's own pace allows, wakes one utility-QoS
//! thread to walk again; the walk runs only while someone asks, so an idle
//! daemon measures nothing. A walk waits at least [`MIN_INTERVAL`] and at most
//! [`MAX_INTERVAL`] behind the last, and never takes more than a fiftieth of
//! the time between walks. Memory is a few system calls, read at each ask.
//!
//! The parts are rosetta's own layout: the record (the raw ledger, a fresh
//! build's staged generation with it), the archive (the projection file, its
//! journal, and a cold build's copy while publication writes it), search
//! (`<projection>.search`) and media (`<projection>.assets`). Sizes are the
//! blocks allocated on disk, as `du` counts them, each inode once. A path that
//! does not exist counts zero; one that vanishes mid-walk is skipped.

use std::collections::HashSet;
use std::fs::Metadata;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use rosetta_server::{DaemonMemory, DaemonStorage, Footprint, FootprintSource, ProjectionReader};

use crate::engine::now_ms;
use crate::ingest_metrics::MemorySample;

/// The shortest and the longest wait between two walks.
pub(crate) const MIN_INTERVAL: Duration = Duration::from_secs(5);
pub(crate) const MAX_INTERVAL: Duration = Duration::from_secs(60);
/// The waits a walk's own duration buys: one that took 200 ms is followed by
/// at least 10 s, so walking costs at most 2% of one thread.
const PACE: u32 = 50;

/// Where each part lives.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct FootprintPaths {
    pub(crate) record: Vec<PathBuf>,
    pub(crate) archive: Vec<PathBuf>,
    pub(crate) search: Vec<PathBuf>,
    pub(crate) media: Vec<PathBuf>,
}

impl FootprintPaths {
    /// A serve's layout: the projection `db`, its public ledger root, and a
    /// cold build's private projection copy when there is one.
    pub(crate) fn serve(db: &Path, ledger: &Path, cold_copy: Option<&Path>) -> Self {
        let mut record = vec![ledger.to_path_buf()];
        if let Ok(staged) = crate::cold_stage::staged_path(ledger) {
            record.push(staged);
        }
        let mut archive = vec![
            db.to_path_buf(),
            crate::sidecar(db, "-wal"),
            crate::sidecar(db, "-shm"),
            crate::sidecar(db, "-journal"),
        ];
        archive.extend(cold_copy.map(Path::to_path_buf));
        Self {
            record,
            archive,
            search: vec![crate::sidecar(db, ".search")],
            media: vec![rosetta_store::assets::asset_root(db)],
        }
    }

    /// One walk of every part.
    pub(crate) fn measure(&self) -> DaemonStorage {
        DaemonStorage {
            record_bytes: allocated(&self.record),
            archive_bytes: allocated(&self.archive),
            search_bytes: allocated(&self.search),
            media_bytes: allocated(&self.media),
            measured_at: now_ms(),
        }
    }
}

/// How long the walk that took `took` waits behind the last.
pub(crate) fn interval(took: Duration) -> Duration {
    took.saturating_mul(PACE).clamp(MIN_INTERVAL, MAX_INTERVAL)
}

#[derive(Default)]
struct State {
    storage: Option<DaemonStorage>,
    /// When the last walk finished, and how long it took.
    walked: Option<(Instant, Duration)>,
    /// A read found the figures due and asked the thread to walk.
    wanted: bool,
    stop: bool,
}

struct Shared {
    paths: FootprintPaths,
    /// The canonical read target: the archive is in memory until it reads
    /// the installed file.
    reader: Arc<ProjectionReader>,
    state: Mutex<State>,
    wake: Condvar,
}

impl Shared {
    fn read(&self) -> Footprint {
        let storage = {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            let due = state
                .walked
                .is_none_or(|(at, took)| at.elapsed() >= interval(took));
            if due && !state.wanted && !state.stop {
                state.wanted = true;
                self.wake.notify_one();
            }
            state.storage
        };
        let sample = MemorySample::probe();
        let memory = sample
            .phys_footprint
            .or(sample.resident)
            .map(|footprint_bytes| DaemonMemory {
                footprint_bytes,
                archive_bytes: if self.reader.is_published() {
                    None
                } else {
                    sample.sqlite_memory_used
                },
            });
        Footprint { storage, memory }
    }

    fn run(&self) {
        crate::maintenance_pace::lower_priority("footprint");
        loop {
            {
                let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
                while !state.wanted && !state.stop {
                    state = self
                        .wake
                        .wait(state)
                        .unwrap_or_else(PoisonError::into_inner);
                }
                if state.stop {
                    return;
                }
            }
            let started = Instant::now();
            let storage = self.paths.measure();
            let took = started.elapsed();
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            state.storage = Some(storage);
            state.walked = Some((Instant::now(), took));
            state.wanted = false;
        }
    }
}

/// The daemon's measurer; its thread stops when it drops.
pub(crate) struct FootprintMeter {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl FootprintMeter {
    pub(crate) fn start(paths: FootprintPaths, reader: Arc<ProjectionReader>) -> Self {
        let shared = Arc::new(Shared {
            paths,
            reader,
            state: Mutex::new(State::default()),
            wake: Condvar::new(),
        });
        let thread = std::thread::Builder::new()
            .name("rosettad-footprint".into())
            .spawn({
                let shared = shared.clone();
                move || shared.run()
            })
            .map_err(|error| {
                eprintln!("footprint: no measuring thread ({error}); storage stays unknown")
            })
            .ok();
        Self { shared, thread }
    }

    /// What `/api/status` reads: the last walk and this moment's memory.
    pub(crate) fn source(&self) -> FootprintSource {
        let shared = self.shared.clone();
        Arc::new(move || shared.read())
    }
}

impl Drop for FootprintMeter {
    fn drop(&mut self) {
        self.shared
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .stop = true;
        self.shared.wake.notify_all();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Bytes allocated under every path, each a file or a tree, links never
/// followed and every inode counted once.
fn allocated(paths: &[PathBuf]) -> u64 {
    let mut seen = HashSet::new();
    let mut total = 0_u64;
    for path in paths {
        let Ok(meta) = std::fs::symlink_metadata(path) else {
            continue;
        };
        total = total.saturating_add(counted(&meta, &mut seen));
        if !meta.is_dir() {
            continue;
        }
        let mut pending = vec![path.clone()];
        while let Some(dir) = pending.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                // `DirEntry::metadata` does not traverse a symbolic link.
                let Ok(meta) = entry.metadata() else {
                    continue;
                };
                if meta.is_dir() {
                    pending.push(entry.path());
                }
                total = total.saturating_add(counted(&meta, &mut seen));
            }
        }
    }
    total
}

#[cfg(unix)]
fn counted(meta: &Metadata, seen: &mut HashSet<(u64, u64)>) -> u64 {
    use std::os::unix::fs::MetadataExt;
    if meta.nlink() > 1 && !meta.is_dir() && !seen.insert((meta.dev(), meta.ino())) {
        return 0;
    }
    meta.blocks().saturating_mul(512)
}

#[cfg(not(unix))]
fn counted(meta: &Metadata, _seen: &mut HashSet<(u64, u64)>) -> u64 {
    meta.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, bytes: usize) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, vec![7_u8; bytes]).unwrap();
    }

    #[test]
    fn each_part_counts_its_own_tree_and_a_missing_one_counts_zero() {
        let home = tempfile::tempdir().unwrap();
        let db = home.path().join("rosetta.db");
        let ledger = home.path().join("rosetta.ledger");
        write(&db, 64 * 1024);
        write(&ledger.join("rt1").join("00000001.rlz"), 128 * 1024);
        write(
            &home
                .path()
                .join("rosetta.ledger.staged")
                .join("rt2")
                .join("a.rlz"),
            32 * 1024,
        );
        write(
            &home
                .path()
                .join("rosetta.db.assets")
                .join("ab")
                .join("c.png"),
            16 * 1024,
        );
        let paths = FootprintPaths::serve(&db, &ledger, None);
        let storage = paths.measure();
        assert!(storage.record_bytes >= 160 * 1024, "{storage:?}");
        assert!(storage.archive_bytes >= 64 * 1024, "{storage:?}");
        assert_eq!(storage.search_bytes, 0, "no index yet");
        assert!(storage.media_bytes >= 16 * 1024, "{storage:?}");
        assert!(storage.record_bytes < storage.archive_bytes + 1024 * 1024);
    }

    #[cfg(unix)]
    #[test]
    fn a_hard_linked_member_counts_once_and_a_symbolic_link_is_not_followed() {
        let home = tempfile::tempdir().unwrap();
        let media = home.path().join("media");
        write(&media.join("one.bin"), 64 * 1024);
        std::fs::hard_link(media.join("one.bin"), media.join("two.bin")).unwrap();
        let outside = home.path().join("outside");
        write(&outside.join("big.bin"), 256 * 1024);
        std::os::unix::fs::symlink(&outside, media.join("elsewhere")).unwrap();
        let alone = allocated(std::slice::from_ref(&media.join("one.bin")));
        let tree = allocated(&[media]);
        assert!(tree >= alone, "the tree holds the file");
        assert!(
            tree < alone * 2,
            "one inode, two names: counted once ({tree} vs {alone})"
        );
    }

    #[test]
    fn the_pace_waits_behind_a_slow_walk_within_its_bounds() {
        assert_eq!(interval(Duration::ZERO), MIN_INTERVAL);
        assert_eq!(
            interval(Duration::from_millis(200)),
            Duration::from_secs(10)
        );
        assert_eq!(interval(Duration::from_secs(30)), MAX_INTERVAL);
    }

    #[test]
    fn a_read_answers_at_once_and_wakes_one_walk_that_lands_for_the_next() {
        let home = tempfile::tempdir().unwrap();
        let db = home.path().join("rosetta.db");
        write(&db, 8 * 1024);
        let meter = FootprintMeter::start(
            FootprintPaths::serve(&db, &home.path().join("rosetta.ledger"), None),
            Arc::new(ProjectionReader::file(db.clone())),
        );
        let source = meter.source();
        let first = source();
        assert_eq!(
            first.storage, None,
            "nothing measured before the first walk"
        );
        if let Some(memory) = first.memory {
            assert!(memory.footprint_bytes > 0);
            assert_eq!(
                memory.archive_bytes, None,
                "a file reader holds no archive in memory"
            );
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        let storage = loop {
            if let Some(storage) = source().storage {
                break storage;
            }
            assert!(Instant::now() < deadline, "the walk never landed");
            std::thread::sleep(Duration::from_millis(10));
        };
        assert!(storage.archive_bytes >= 8 * 1024);
        assert!(storage.measured_at > 0);
        drop(meter);
    }
}
