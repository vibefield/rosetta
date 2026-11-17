//! The writer's queues (R1 brief §4.6): one per (runtime, lane), filled by a
//! non-blocking drain of the channel and served round-robin, one
//! transaction per key per turn.
//!
//! Small whole sources of one key coalesce into one transaction while the
//! total stays within [`TRANSACTION_BYTES`] and fewer than
//! [`TRANSACTION_SOURCES`] — strictly: HEAD's loop admitted one message past
//! the bound. A source over the chunk bound is its own item, stepped one
//! transaction per turn, so another key's turn always comes between two of
//! its slices. The kill switch keeps HEAD's writer: one FIFO over every
//! runtime's messages, consecutive messages of one runtime coalescing while
//! the total is under the bound.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use rosetta_pipeline::{ApplyItem, ClosedSource, LaneId, Samples};

use super::emit::BudgetClass;

/// Queued sources of one key share a private transaction up to these
/// bounds, so commits amortize while a shared-cache reader never waits
/// behind one very long write.
pub(super) const TRANSACTION_BYTES: u64 = 16 * 1024 * 1024;
pub(super) const TRANSACTION_SOURCES: usize = 256;

/// A writer queue: one runtime's lane.
pub(super) type Key = (String, LaneId);

/// One queued emission: a source (per source in R1; a whole message under
/// the kill switch) and the budget bytes it holds.
pub(super) struct Queued {
    pub(super) sources: Vec<ClosedSource>,
    pub(super) bytes: u64,
    pub(super) class: BudgetClass,
    /// Arrival order of its message over every key.
    seq: u64,
    at: Instant,
}

/// An item between its first and its terminal transaction.
pub(super) struct Active {
    pub(super) item: ApplyItem,
    pub(super) class: BudgetClass,
    /// It completes the first session of the build (a milestone).
    pub(super) completes: bool,
}

/// What a turn works on.
pub(super) enum Work {
    /// The next transaction of an item already begun.
    Active(Active),
    /// Sources to begin an item with; `chunk_bytes` is `u64::MAX` for a
    /// coalesced item, which is never chunked.
    Fresh {
        sources: Vec<ClosedSource>,
        bytes: u64,
        class: BudgetClass,
        chunk_bytes: u64,
    },
}

/// One key's measurements.
#[derive(Default)]
pub(super) struct KeyStats {
    pub(super) txs: u64,
    pub(super) stage_txs: u64,
    pub(super) resolved_slice_txs: u64,
    /// Each source's wait from arrival to the start of its item.
    pub(super) queue_wait: Samples,
}

#[derive(Default)]
struct KeyQueue {
    pending: VecDeque<Queued>,
    active: Option<Active>,
    stats: KeyStats,
}

impl KeyQueue {
    fn has_work(&self) -> bool {
        self.active.is_some() || !self.pending.is_empty()
    }
}

pub(super) struct WriterQueues {
    /// Keys in first-arrival order: the round-robin order.
    keys: Vec<Key>,
    queues: HashMap<Key, KeyQueue>,
    /// The round-robin position: the key after the last one served.
    next: usize,
    /// HEAD's FIFO writer (the kill switch).
    fifo: bool,
    chunk_bytes: u64,
    seq: u64,
}

impl WriterQueues {
    pub(super) fn new(fifo: bool, chunk_bytes: u64) -> Self {
        Self {
            keys: Vec::new(),
            queues: HashMap::new(),
            next: 0,
            fifo,
            chunk_bytes: if fifo { u64::MAX } else { chunk_bytes },
            seq: 0,
        }
    }

    fn queue(&mut self, key: Key) -> &mut KeyQueue {
        if !self.queues.contains_key(&key) {
            self.keys.push(key.clone());
        }
        self.queues.entry(key).or_default()
    }

    /// Queue one emission of `runtime`'s `lane` holding `bytes`: per source
    /// in R1, each with its own budget bytes (their sum is `bytes`, as the
    /// emitter acquired it); whole under the kill switch.
    pub(super) fn push(
        &mut self,
        runtime: &str,
        lane: LaneId,
        class: BudgetClass,
        sources: Vec<ClosedSource>,
        bytes: u64,
        now: Instant,
    ) {
        let seq = self.seq;
        self.seq += 1;
        let fifo = self.fifo;
        let queue = self.queue((runtime.to_owned(), lane));
        if fifo {
            queue.pending.push_back(Queued {
                sources,
                bytes,
                class,
                seq,
                at: now,
            });
            return;
        }
        let count = sources.len();
        let mut left = bytes;
        for (index, source) in sources.into_iter().enumerate() {
            // Conservation: the last source takes whatever the measured
            // shares leave, so the item bytes sum to what was acquired.
            let share = if index + 1 == count {
                left
            } else {
                source.budget_bytes().min(left)
            };
            left -= share;
            queue.pending.push_back(Queued {
                sources: vec![source],
                bytes: share,
                class,
                seq,
                at: now,
            });
        }
    }

    /// Whether any key has work.
    pub(super) fn has_work(&self) -> bool {
        self.queues.values().any(KeyQueue::has_work)
    }

    /// Whether `runtime` has nothing queued and no item in progress.
    pub(super) fn runtime_idle(&self, runtime: &str) -> bool {
        self.queues
            .iter()
            .filter(|((owner, _), _)| owner == runtime)
            .all(|(_, queue)| !queue.has_work())
    }

    /// Under the kill switch: whether anything that arrived before `seq` is
    /// still queued (a runtime's close waits for it, as HEAD's FIFO did).
    pub(super) fn older_than(&self, seq: u64) -> bool {
        self.fifo
            && self
                .queues
                .values()
                .any(|queue| queue.pending.front().is_some_and(|queued| queued.seq < seq))
    }

    /// The arrival sequence the next message will take.
    pub(super) fn next_seq(&mut self) -> u64 {
        let seq = self.seq;
        self.seq += 1;
        seq
    }

    /// Take the next turn's work: round-robin over keys with work (FIFO
    /// under the kill switch), one transaction's worth. A runtime's span `P`
    /// goes before its lanes: the sequencer emits all of `P` first, and a
    /// lane source that completes a session may carry resolved rows bound to
    /// a `P` member's cursor, which that member's own first transaction
    /// would otherwise sweep as a superseded projection.
    pub(super) fn take_turn(&mut self, now: Instant) -> Option<(Key, Work)> {
        let mut index = if self.fifo {
            self.oldest()?
        } else {
            let count = self.keys.len();
            (0..count)
                .map(|offset| (self.next + offset) % count.max(1))
                .find(|index| self.queues[&self.keys[*index]].has_work())?
        };
        let (runtime, lane) = &self.keys[index];
        if *lane != LaneId::Prefix
            && let Some(prefix) = self
                .keys
                .iter()
                .position(|(owner, lane)| owner == runtime && *lane == LaneId::Prefix)
            && self.queues[&self.keys[prefix]].has_work()
        {
            index = prefix;
        }
        self.next = index + 1;
        let key = self.keys[index].clone();
        let (fifo, chunk_bytes) = (self.fifo, self.chunk_bytes);
        let queue = self.queues.get_mut(&key).expect("every key has a queue");
        if let Some(active) = queue.active.take() {
            return Some((key, Work::Active(active)));
        }
        let first = queue.pending.pop_front()?;
        let class = first.class;
        let mut bytes = first.bytes;
        let mut last_seq = first.seq;
        queue
            .stats
            .queue_wait
            .record(now.saturating_duration_since(first.at));
        let mut sources = first.sources;
        let single = !fifo && bytes > chunk_bytes;
        if !single {
            while let Some(next) = queue.pending.front() {
                let fits = if fifo {
                    // HEAD: the next message of the same runtime, received right
                    // after, while the total is under the bound.
                    bytes < TRANSACTION_BYTES
                        && sources.len() < TRANSACTION_SOURCES
                        && next.seq == last_seq + 1
                } else {
                    next.bytes <= chunk_bytes
                        && bytes.saturating_add(next.bytes) <= TRANSACTION_BYTES
                        && sources.len() + next.sources.len() <= TRANSACTION_SOURCES
                };
                if !fits {
                    break;
                }
                let next = queue.pending.pop_front().expect("front exists");
                queue
                    .stats
                    .queue_wait
                    .record(now.saturating_duration_since(next.at));
                bytes = bytes.saturating_add(next.bytes);
                last_seq = next.seq;
                sources.extend(next.sources);
            }
        }
        Some((
            key,
            Work::Fresh {
                sources,
                bytes,
                class,
                chunk_bytes: if single { chunk_bytes } else { u64::MAX },
            },
        ))
    }

    /// The key whose front arrived first.
    fn oldest(&self) -> Option<usize> {
        self.keys
            .iter()
            .enumerate()
            .filter_map(|(index, key)| {
                let queue = &self.queues[key];
                if queue.active.is_some() {
                    return Some((0, index));
                }
                queue.pending.front().map(|queued| (queued.seq, index))
            })
            .min()
            .map(|(_, index)| index)
    }

    /// Put an item that has more transactions back at its key's head.
    pub(super) fn put_back(&mut self, key: &Key, active: Active) {
        self.queues
            .get_mut(key)
            .expect("an active item's key has a queue")
            .active = Some(active);
    }

    pub(super) fn stats_mut(&mut self, key: &Key) -> &mut KeyStats {
        &mut self.queue(key.clone()).stats
    }

    /// Each key with its measurements, in round-robin order.
    pub(super) fn stats(&self) -> impl Iterator<Item = (&Key, &KeyStats)> {
        self.keys.iter().map(|key| (key, &self.queues[key].stats))
    }

    /// Sources queued for `key` and not begun yet.
    #[cfg(test)]
    pub(super) fn pending_sources(&self, key: &Key) -> usize {
        self.queues.get(key).map_or(0, |queue| {
            queue.pending.iter().map(|q| q.sources.len()).sum()
        })
    }
}

/// How long the writer may block before the next due flip: `None` blocks.
pub(super) fn block_for(soonest_flip: Option<Duration>) -> Option<Duration> {
    soonest_flip.map(|wait| wait.max(Duration::from_millis(1)))
}

#[cfg(test)]
#[path = "queues_tests.rs"]
mod tests;
