//! Priority scheduler: live ▸ on-open ▸ newest-first backfill. Only the
//! backfill lane has producers today; the other lanes are the contract the
//! watcher and the app's on-open promotion plug into.

use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Lane {
    Live,
    OnOpen,
    Backfill,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkItem {
    pub lane: Lane,
    pub relative_path: String,
    pub modified_ms: i64,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct Prioritized {
    item: WorkItem,
    sequence: u64,
}

impl Ord for Prioritized {
    fn cmp(&self, other: &Self) -> Ordering {
        // BinaryHeap is a max-heap: better work must compare GREATER.
        // Lower lane wins, then newer files, then FIFO arrival.
        other
            .item
            .lane
            .cmp(&self.item.lane)
            .then(self.item.modified_ms.cmp(&other.item.modified_ms))
            .then(other.sequence.cmp(&self.sequence))
    }
}

impl PartialOrd for Prioritized {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// A file appears at most once: pushing it again keeps only the best lane
/// (stale heap entries are dropped lazily on pop).
#[derive(Debug, Default)]
pub struct WorkQueue {
    heap: BinaryHeap<Prioritized>,
    best: HashMap<String, Lane>,
    sequence: u64,
}

impl WorkQueue {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, item: WorkItem) {
        match self.best.get(&item.relative_path) {
            Some(queued) if *queued <= item.lane => return,
            _ => {}
        }
        self.best.insert(item.relative_path.clone(), item.lane);
        self.sequence += 1;
        self.heap.push(Prioritized {
            item,
            sequence: self.sequence,
        });
    }

    /// Re-queue an already-known file at a better lane (the app opened it).
    pub fn promote(&mut self, relative_path: &str, lane: Lane, modified_ms: i64) {
        self.push(WorkItem {
            lane,
            relative_path: relative_path.to_string(),
            modified_ms,
        });
    }

    pub fn pop(&mut self) -> Option<WorkItem> {
        while let Some(candidate) = self.heap.pop() {
            match self.best.get(&candidate.item.relative_path) {
                Some(best) if *best == candidate.item.lane => {
                    self.best.remove(&candidate.item.relative_path);
                    return Some(candidate.item);
                }
                // Superseded by a better-lane push — stale entry.
                _ => continue,
            }
        }
        None
    }

    pub fn is_empty(&self) -> bool {
        self.best.is_empty()
    }

    pub fn len(&self) -> usize {
        self.best.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(lane: Lane, path: &str, modified_ms: i64) -> WorkItem {
        WorkItem {
            lane,
            relative_path: path.to_string(),
            modified_ms,
        }
    }

    #[test]
    fn lanes_beat_recency_and_backfill_is_newest_first() {
        let mut queue = WorkQueue::new();
        queue.push(item(Lane::Backfill, "old.jsonl", 100));
        queue.push(item(Lane::Backfill, "new.jsonl", 900));
        queue.push(item(Lane::OnOpen, "opened.jsonl", 1));
        queue.push(item(Lane::Live, "live.jsonl", 0));

        let order: Vec<String> = std::iter::from_fn(|| queue.pop())
            .map(|work| work.relative_path)
            .collect();
        assert_eq!(
            order,
            ["live.jsonl", "opened.jsonl", "new.jsonl", "old.jsonl"]
        );
    }

    #[test]
    fn promotion_supersedes_a_queued_backfill_entry() {
        let mut queue = WorkQueue::new();
        queue.push(item(Lane::Backfill, "a.jsonl", 100));
        queue.push(item(Lane::Backfill, "b.jsonl", 200));
        queue.promote("a.jsonl", Lane::OnOpen, 100);

        assert_eq!(queue.len(), 2);
        assert_eq!(queue.pop().unwrap().relative_path, "a.jsonl");
        assert_eq!(queue.pop().unwrap().relative_path, "b.jsonl");
        assert!(queue.pop().is_none() && queue.is_empty());
    }

    #[test]
    fn worse_lane_push_never_downgrades() {
        let mut queue = WorkQueue::new();
        queue.push(item(Lane::OnOpen, "a.jsonl", 100));
        queue.push(item(Lane::Backfill, "a.jsonl", 100));
        let popped = queue.pop().unwrap();
        assert_eq!(popped.lane, Lane::OnOpen);
        assert!(queue.pop().is_none());
    }
}
