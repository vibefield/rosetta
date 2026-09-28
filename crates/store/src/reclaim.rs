//! Deallocation off the canonical writer.
//!
//! A cold build drops very large owned trees on the sole canonical writer:
//! each closed batch's rows once their SQL has run, and each admitted
//! runtime's resolved plan after its commit. Freeing them is pure allocator
//! work with no ordering or visibility meaning, so the writer hands them to
//! one background thread instead of paying for it between transactions. The
//! queue is short and bounded: when the reclaimer falls behind, the writer
//! waits for a slot rather than piling garbage up in RAM.

use std::sync::OnceLock;
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};

/// Values queued for the reclaimer at once. Each one is at most a closed
/// transaction's rows or one runtime's plan.
const QUEUED: usize = 8;

type Garbage = Box<dyn Send>;

fn reclaimer() -> Option<&'static SyncSender<Garbage>> {
    static RECLAIMER: OnceLock<Option<SyncSender<Garbage>>> = OnceLock::new();
    RECLAIMER
        .get_or_init(|| {
            let (sender, receiver): (SyncSender<Garbage>, Receiver<Garbage>) = sync_channel(QUEUED);
            std::thread::Builder::new()
                .name("rosetta-reclaim".into())
                .spawn(move || {
                    for garbage in receiver {
                        drop(garbage);
                    }
                })
                .ok()
                .map(|_| sender)
        })
        .as_ref()
}

/// Drop `value` on the reclaimer thread, or here if it cannot run.
pub(crate) fn release<T: Send + 'static>(value: T) {
    match reclaimer() {
        Some(sender) => {
            if let Err(returned) = sender.send(Box::new(value)) {
                drop(returned.0);
            }
        }
        None => drop(value),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use super::*;

    struct Counted(Arc<AtomicUsize>);

    impl Drop for Counted {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn released_values_are_dropped_on_the_reclaimer_thread() {
        let dropped = Arc::new(AtomicUsize::new(0));
        for _ in 0..(QUEUED * 4) {
            release(Counted(dropped.clone()));
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        while dropped.load(Ordering::SeqCst) < QUEUED * 4 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(dropped.load(Ordering::SeqCst), QUEUED * 4);
    }
}
