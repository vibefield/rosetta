use crate::{MediaError, PreparedImage, prepare_uncached};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, Condvar, Mutex, OnceLock};

const CACHE_BYTES: usize = 32 * 1024 * 1024;
const WORKERS: usize = 2;
type Key = [u8; 32];

#[derive(Default)]
struct State {
    active: BTreeSet<Key>,
    cache: BTreeMap<Key, Arc<PreparedImage>>,
    order: VecDeque<Key>,
    bytes: usize,
}
#[derive(Default)]
struct Pool {
    state: Mutex<State>,
    wake: Condvar,
}
static POOL: OnceLock<Pool> = OnceLock::new();
struct Job<'a> {
    pool: &'a Pool,
    key: Key,
}
impl Drop for Job<'_> {
    fn drop(&mut self) {
        let mut state = self.pool.state.lock().unwrap_or_else(|p| p.into_inner());
        state.active.remove(&self.key);
        self.pool.wake.notify_all();
    }
}

pub(super) fn prepare(bytes: &[u8]) -> Result<Arc<PreparedImage>, MediaError> {
    let pool = POOL.get_or_init(Pool::default);
    let key = *blake3::hash(bytes).as_bytes();
    let mut state = pool.state.lock().unwrap_or_else(|p| p.into_inner());
    loop {
        if let Some(value) = state.cache.get(&key).cloned() {
            state.order.retain(|item| item != &key);
            state.order.push_back(key);
            return Ok(value);
        }
        if !state.active.contains(&key) && state.active.len() < WORKERS {
            break;
        }
        state = pool.wake.wait(state).unwrap_or_else(|p| p.into_inner());
    }
    state.active.insert(key);
    drop(state);
    let _job = Job { pool, key };
    let result = Arc::new(prepare_uncached(bytes)?);
    let size = result.asset.materialized_byte_len();
    if size <= CACHE_BYTES {
        let mut state = pool.state.lock().unwrap_or_else(|p| p.into_inner());
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
