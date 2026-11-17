//! Process-wide codec admission: at most two jobs decode, resize or encode at
//! once, whether `prepare()` jobs or permit holders. A background job starts
//! only when no other job is running or waiting, so at most one runs and a
//! foreground request waits for at most one of them. Repeated `prepare()`
//! content coalesces and shares a 32 MiB compressed LRU.
use crate::{MediaError, PreparedImage, prepare_uncached};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};

const CACHE_BYTES: usize = 32 * 1024 * 1024;
const WORKERS: usize = 2;
type Key = [u8; 32];

/// Who is asking for codec time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lane {
    /// A person is waiting: admitted whenever a slot is free.
    Foreground,
    /// Maintenance: admitted only into an idle pool.
    Background,
}

/// The admission rule, over the jobs running and the foreground requests
/// waiting for a slot.
pub(crate) fn admits(active: usize, foreground_waiting: usize, lane: Lane) -> bool {
    match lane {
        Lane::Foreground => active < WORKERS,
        Lane::Background => active == 0 && foreground_waiting == 0,
    }
}

#[derive(Default)]
struct State {
    active: usize,
    foreground_waiting: usize,
    admitted: u64,
    preparing: BTreeSet<Key>,
    cache: BTreeMap<Key, Arc<PreparedImage>>,
    order: VecDeque<Key>,
    bytes: usize,
}
impl State {
    fn admit(&mut self) {
        self.active += 1;
        self.admitted += 1;
    }
}

#[derive(Default)]
pub(crate) struct Pool {
    state: Mutex<State>,
    wake: Condvar,
}

/// One admitted codec slot, released on drop. `renditions()` requires one, so
/// every encode is counted against the pool. Do not call `prepare()` while
/// holding a permit: it takes a slot of its own.
#[must_use = "dropping the permit releases its codec slot"]
pub struct CodecPermit {
    pool: &'static Pool,
}
impl Drop for CodecPermit {
    fn drop(&mut self) {
        self.pool.release(None);
    }
}
impl fmt::Debug for CodecPermit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("CodecPermit")
    }
}

struct Job {
    pool: &'static Pool,
    key: Key,
}
impl Drop for Job {
    fn drop(&mut self) {
        self.pool.release(Some(self.key));
    }
}

impl Pool {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn release(&self, key: Option<Key>) {
        let mut state = self.lock();
        state.active -= 1;
        if let Some(key) = key {
            state.preparing.remove(&key);
        }
        self.wake.notify_all();
    }

    pub(crate) fn admit(&'static self, lane: Lane) -> CodecPermit {
        let mut state = self.lock();
        if !admits(state.active, state.foreground_waiting, lane) {
            let foreground = usize::from(lane == Lane::Foreground);
            state.foreground_waiting += foreground;
            state = self
                .wake
                .wait_while(state, |s| !admits(s.active, s.foreground_waiting, lane))
                .unwrap_or_else(|p| p.into_inner());
            state.foreground_waiting -= foreground;
        }
        state.admit();
        CodecPermit { pool: self }
    }

    pub(crate) fn try_admit(&'static self, lane: Lane) -> Option<CodecPermit> {
        let mut state = self.lock();
        if !admits(state.active, state.foreground_waiting, lane) {
            return None;
        }
        state.admit();
        Some(CodecPermit { pool: self })
    }

    pub(crate) fn admitted(&self) -> u64 {
        self.lock().admitted
    }

    #[cfg(test)]
    pub(crate) fn snapshot(&self) -> (usize, usize) {
        let state = self.lock();
        (state.active, state.foreground_waiting)
    }

    /// A foreground job: waits for a slot, or for the same content in flight.
    pub(crate) fn prepare(&'static self, bytes: &[u8]) -> Result<Arc<PreparedImage>, MediaError> {
        let key = *blake3::hash(bytes).as_bytes();
        let mut state = self.lock();
        let mut waiting = false;
        loop {
            if let Some(value) = state.cache.get(&key).cloned() {
                state.order.retain(|item| item != &key);
                state.order.push_back(key);
                if waiting {
                    state.foreground_waiting -= 1;
                    self.wake.notify_all();
                }
                return Ok(value);
            }
            if !state.preparing.contains(&key)
                && admits(state.active, state.foreground_waiting, Lane::Foreground)
            {
                break;
            }
            if !waiting {
                state.foreground_waiting += 1;
                waiting = true;
            }
            state = self.wake.wait(state).unwrap_or_else(|p| p.into_inner());
        }
        if waiting {
            state.foreground_waiting -= 1;
        }
        state.preparing.insert(key);
        state.admit();
        drop(state);
        let _job = Job { pool: self, key };
        let result = Arc::new(prepare_uncached(bytes)?);
        let size = result.asset.materialized_byte_len();
        if size <= CACHE_BYTES {
            let mut state = self.lock();
            while state.bytes + size > CACHE_BYTES {
                let Some(old) = state.order.pop_front() else {
                    break;
                };
                if let Some(value) = state.cache.remove(&old) {
                    state.bytes -= value.asset.materialized_byte_len();
                }
            }
            state.bytes += size;
            state.order.push_back(key);
            state.cache.insert(key, result.clone());
        }
        Ok(result)
    }
}

static POOL: OnceLock<Pool> = OnceLock::new();
fn pool() -> &'static Pool {
    POOL.get_or_init(Pool::default)
}

/// Wait for a codec slot. A background caller that must stay interruptible
/// uses [`try_admit`] instead: this wait cannot be cancelled.
pub fn admit(lane: Lane) -> CodecPermit {
    pool().admit(lane)
}

/// A codec slot if the lane is admitted right now; never waits.
pub fn try_admit(lane: Lane) -> Option<CodecPermit> {
    pool().try_admit(lane)
}

/// Codec jobs admitted since the process started: `prepare()` jobs that missed
/// its cache, plus permits. Cache hits and probes are not jobs.
pub fn admitted_jobs() -> u64 {
    pool().admitted()
}

pub(super) fn prepare(bytes: &[u8]) -> Result<Arc<PreparedImage>, MediaError> {
    pool().prepare(bytes)
}
