//! T6 test 10: round-robin turns and strict coalescing.

use super::*;

const MIB: u64 = 1024 * 1024;
const CHUNK: u64 = 16 * MIB;

fn source(path: &str, bytes: u64, lane: LaneId) -> ClosedSource {
    ClosedSource::synthetic(path, "s", Vec::new(), bytes, lane)
}

fn push(queues: &mut WriterQueues, runtime: &str, lane: LaneId, paths: &[(&str, u64)]) {
    let sources: Vec<ClosedSource> = paths
        .iter()
        .map(|(path, bytes)| source(path, *bytes, lane))
        .collect();
    let bytes = paths.iter().map(|(_, bytes)| bytes).sum();
    queues.push(
        runtime,
        lane,
        BudgetClass::Shared,
        sources,
        bytes,
        Instant::now(),
    );
}

/// The keys served by the next `turns` turns, each turn's item put back
/// when it is a chunked one (it has more slices), and every transaction's
/// budget bytes.
fn serve(queues: &mut WriterQueues, turns: usize) -> Vec<(String, u64, usize)> {
    let mut served = Vec::new();
    for _ in 0..turns {
        let Some((key, work)) = queues.take_turn(Instant::now()) else {
            break;
        };
        match work {
            Work::Fresh {
                sources,
                bytes,
                chunk_bytes,
                ..
            } => {
                served.push((format!("{}/{}", key.0, key.1), bytes, sources.len()));
                assert!(chunk_bytes == u64::MAX || sources.len() == 1);
            }
            Work::Active(_) => unreachable!("no item is put back here"),
        }
    }
    served
}

#[test]
fn turns_go_round_robin_over_the_keys_with_work() {
    let mut queues = WriterQueues::new(false, CHUNK);
    // Claude's S and B lanes and Codex's S lane, all queued at once.
    for index in 0..3 {
        push(
            &mut queues,
            "claude",
            LaneId::Small,
            &[(&format!("s{index}"), 20 * MIB)],
        );
        push(
            &mut queues,
            "claude",
            LaneId::Big(0),
            &[(&format!("b{index}"), 20 * MIB)],
        );
        push(
            &mut queues,
            "codex",
            LaneId::Small,
            &[(&format!("c{index}"), 20 * MIB)],
        );
    }
    let keys: Vec<String> = serve(&mut queues, 9)
        .into_iter()
        .map(|(key, _, _)| key)
        .collect();
    assert_eq!(
        keys,
        [
            "claude/S",
            "claude/B0",
            "codex/S",
            "claude/S",
            "claude/B0",
            "codex/S",
            "claude/S",
            "claude/B0",
            "codex/S"
        ],
        "FIFO would serve every key's queue in arrival order instead"
    );
}

/// A chunked item's slices never take two turns in a row while another
/// key has work: its key goes to the back of the round after every slice.
#[test]
fn a_chunked_item_yields_to_lane_s_after_every_slice() {
    let mut queues = WriterQueues::new(false, CHUNK);
    push(
        &mut queues,
        "claude",
        LaneId::Big(0),
        &[("giant", 300 * MIB)],
    );
    for index in 0..4 {
        push(
            &mut queues,
            "claude",
            LaneId::Small,
            // Too large to coalesce with each other: one turn each.
            &[(&format!("small{index}"), 10 * MIB)],
        );
    }
    let big: Key = ("claude".into(), LaneId::Big(0));
    let mut order = Vec::new();
    for _ in 0..6 {
        let (key, work) = queues.take_turn(Instant::now()).unwrap();
        order.push(key.1);
        if let Work::Fresh {
            sources,
            chunk_bytes,
            ..
        } = &work
            && key == big
        {
            assert_eq!(chunk_bytes, &CHUNK, "the giant is its own chunked item");
            assert_eq!(sources.len(), 1);
        }
        if key == big {
            // Stand-in for a begun item with slices left: the giant's key
            // is busy again at once.
            push(
                &mut queues,
                "claude",
                LaneId::Big(0),
                &[("next-slice", 300 * MIB)],
            );
        }
    }
    for pair in order.windows(2) {
        assert!(
            !(pair[0] == LaneId::Big(0) && pair[1] == LaneId::Big(0)),
            "two big-lane turns in a row while lane S had work: {order:?}"
        );
    }
}

/// Small sources of one key coalesce only while the total stays within the
/// bound: 10 × 2 MiB then a 15 MiB source never make a transaction over
/// 16 MiB. (HEAD's `< TRANSACTION_BYTES` loop took one message past it.)
#[test]
fn coalescing_never_exceeds_the_transaction_bound() {
    let mut queues = WriterQueues::new(false, CHUNK);
    let mut paths: Vec<(String, u64)> = (0..10).map(|i| (format!("small{i}"), 2 * MIB)).collect();
    paths.push(("medium".into(), 15 * MIB));
    for (path, bytes) in &paths {
        push(&mut queues, "claude", LaneId::Small, &[(path, *bytes)]);
    }
    let served = serve(&mut queues, 10);
    assert!(served.len() >= 2);
    for (_, bytes, _) in &served {
        assert!(*bytes <= TRANSACTION_BYTES, "a {bytes}-byte transaction");
    }
    let total: u64 = served.iter().map(|(_, bytes, _)| bytes).sum();
    assert_eq!(total, 35 * MIB);
    assert_eq!(served.iter().map(|(_, _, n)| n).sum::<usize>(), 11);
}

/// One emission of several sources is split per source, and the budget
/// bytes it acquired are conserved across them.
#[test]
fn an_emission_splits_per_source_conserving_its_bytes() {
    let mut queues = WriterQueues::new(false, CHUNK);
    push(
        &mut queues,
        "codex",
        LaneId::Prefix,
        &[
            ("state#a", 3 * MIB),
            ("state#b", 20 * MIB),
            ("state#c", MIB),
        ],
    );
    let key: Key = ("codex".into(), LaneId::Prefix);
    assert_eq!(queues.pending_sources(&key), 3);
    let served = serve(&mut queues, 3);
    // The 20 MiB member is its own item; the others coalesce around it only
    // in order.
    assert_eq!(
        served
            .iter()
            .map(|(_, bytes, _)| *bytes)
            .collect::<Vec<_>>(),
        [3 * MIB, 20 * MIB, MIB]
    );
}

/// The kill switch keeps HEAD's writer: one FIFO over every runtime, and a
/// runtime's close waits for whatever arrived before it.
#[test]
fn the_kill_switch_keeps_one_fifo() {
    let mut queues = WriterQueues::new(true, CHUNK);
    push(&mut queues, "a", LaneId::Path, &[("a1", MIB)]);
    push(&mut queues, "b", LaneId::Path, &[("b1", MIB)]);
    push(&mut queues, "a", LaneId::Path, &[("a2", MIB)]);
    let closed = queues.next_seq();
    assert!(queues.older_than(closed));
    let keys: Vec<String> = serve(&mut queues, 3)
        .into_iter()
        .map(|(key, _, _)| key)
        .collect();
    assert_eq!(keys, ["a/path", "b/path", "a/path"]);
    assert!(!queues.older_than(closed));
}

/// A runtime's span P is served before its lanes, whatever the round-robin
/// position: its members' rows commit before any lane source that resolves
/// facts bound to their cursors. (Without the rule the lane key, first in
/// the round, would go first.)
#[test]
fn span_p_goes_before_its_runtimes_lanes() {
    let mut queues = WriterQueues::new(false, CHUNK);
    push(&mut queues, "codex", LaneId::Small, &[("rollout", MIB)]);
    push(
        &mut queues,
        "codex",
        LaneId::Prefix,
        &[("state#a", 10 * MIB)],
    );
    push(
        &mut queues,
        "codex",
        LaneId::Prefix,
        &[("state#b", 10 * MIB)],
    );
    push(&mut queues, "claude", LaneId::Small, &[("other", MIB)]);
    let keys: Vec<String> = serve(&mut queues, 4)
        .into_iter()
        .map(|(key, _, _)| key)
        .collect();
    // Codex's lane S waits for both of its P members; Claude keeps its turn.
    let position = |key: &str| keys.iter().rposition(|served| served == key).unwrap();
    assert_eq!(keys[0], "codex/P");
    assert!(position("codex/P") < position("codex/S"), "{keys:?}");
    assert!(keys.contains(&"claude/S".to_string()), "{keys:?}");
}

/// Stress (L minor 11, I10): producers on both classes — non-blocking
/// lanes holding and retrying under their drain tickets, items over their
/// class among them, and a blocking runtime-admission producer — feed one
/// writer that splits every emission per source, coalesces strictly, and
/// releases each chunked item slice by slice by remainder. Every producer
/// finishes, every class drains to zero, and each class got back exactly
/// what it lent, whatever the interleaving.
#[test]
fn budgets_drain_under_chunked_release_with_tickets_and_giants() {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::mpsc;
    use std::sync::{Arc, Condvar, Mutex};
    use std::time::Duration;

    use super::super::emit::LaneBudgets;

    struct Emission {
        runtime: &'static str,
        lane: LaneId,
        class: BudgetClass,
        sizes: Vec<u64>,
    }
    // The pool's `output` condvar: the release hook bumps an epoch.
    let wake = Arc::new((Mutex::new(0_u64), Condvar::new()));
    let hook = {
        let wake = Arc::clone(&wake);
        Arc::new(move || {
            *wake.0.lock().unwrap() += 1;
            wake.1.notify_all();
        })
    };
    let budgets = Arc::new(LaneBudgets::new(true, Some(hook)));
    let acquired = Arc::new([AtomicU64::new(0), AtomicU64::new(0)]);
    let slot = |class: BudgetClass| usize::from(class == BudgetClass::Shared);
    let (sender, receiver) = mpsc::channel::<Emission>();
    let mut producers = Vec::new();
    // (runtime, lane, blocking, seed): lanes S and B of two runtimes, and a
    // runtime admitted whole on the shared class, emitting blocking.
    for (runtime, lane, blocking, seed) in [
        ("claude", LaneId::Small, false, 1_u64),
        ("claude", LaneId::Big(0), false, 2),
        ("codex", LaneId::Small, false, 3),
        ("codex", LaneId::Big(0), false, 4),
        ("gemini", LaneId::Path, true, 5),
    ] {
        let (budgets, acquired, wake, sender) = (
            Arc::clone(&budgets),
            Arc::clone(&acquired),
            Arc::clone(&wake),
            sender.clone(),
        );
        producers.push(std::thread::spawn(move || {
            let class = budgets.class_of(lane);
            let ticket = budgets.ticket();
            let mut state = seed;
            let mut next = move |bound: u64| {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                (state >> 33) % bound
            };
            for index in 0..60 {
                // Mostly small sources, some over the chunk bound, and now
                // and then one over the whole class.
                let count = 1 + next(4) as usize;
                let sizes: Vec<u64> = (0..count)
                    .map(|_| match next(20) {
                        0 => 200 * MIB + next(150 * MIB),
                        1..=4 => CHUNK + 1 + next(40 * MIB),
                        _ => 1 + next(2 * MIB),
                    })
                    .collect();
                let bytes: u64 = sizes.iter().sum();
                if blocking || index % 7 == 0 {
                    budgets
                        .budget(class)
                        .acquire(bytes, budgets.ticket())
                        .unwrap();
                } else {
                    // Held until a release (or a cleared ticket) lets it in.
                    loop {
                        let epoch = *wake.0.lock().unwrap();
                        if budgets.budget(class).try_acquire(bytes, ticket).unwrap() {
                            break;
                        }
                        let guard = wake.0.lock().unwrap();
                        let _ = wake
                            .1
                            .wait_timeout_while(guard, Duration::from_millis(100), |now| {
                                *now == epoch
                            })
                            .unwrap();
                    }
                }
                acquired[slot(class)].fetch_add(bytes, Ordering::SeqCst);
                sender
                    .send(Emission {
                        runtime,
                        lane,
                        class,
                        sizes,
                    })
                    .unwrap();
            }
        }));
    }
    drop(sender);

    let (finished, outcome) = mpsc::channel();
    {
        let budgets = Arc::clone(&budgets);
        std::thread::spawn(move || {
            let mut queues = WriterQueues::new(false, CHUNK);
            let mut released = [0_u64, 0_u64];
            let mut open = true;
            while open || queues.has_work() {
                loop {
                    let message = if queues.has_work() {
                        match receiver.try_recv() {
                            Ok(message) => message,
                            Err(mpsc::TryRecvError::Empty) => break,
                            Err(mpsc::TryRecvError::Disconnected) => {
                                open = false;
                                break;
                            }
                        }
                    } else {
                        match receiver.recv_timeout(Duration::from_secs(10)) {
                            Ok(message) => message,
                            Err(mpsc::RecvTimeoutError::Disconnected) => {
                                open = false;
                                break;
                            }
                            Err(mpsc::RecvTimeoutError::Timeout) => {
                                // A producer never sent: no result.
                                return;
                            }
                        }
                    };
                    let sources: Vec<ClosedSource> = message
                        .sizes
                        .iter()
                        .enumerate()
                        .map(|(index, bytes)| source(&format!("p{index}"), *bytes, message.lane))
                        .collect();
                    let bytes = message.sizes.iter().sum();
                    queues.push(
                        message.runtime,
                        message.lane,
                        message.class,
                        sources,
                        bytes,
                        Instant::now(),
                    );
                }
                let Some((_, work)) = queues.take_turn(Instant::now()) else {
                    continue;
                };
                let Work::Fresh {
                    bytes,
                    class,
                    chunk_bytes,
                    ..
                } = work
                else {
                    unreachable!("no item is put back here")
                };
                // Release by remainder: each Stage slice its share (rounded
                // down), the terminal whatever the item still holds.
                let slices = if chunk_bytes == u64::MAX {
                    1
                } else {
                    bytes.div_ceil(chunk_bytes)
                };
                let mut left = bytes;
                for _ in 1..slices {
                    let share = (bytes / slices).min(left);
                    left -= share;
                    budgets.release(class, share);
                    std::thread::yield_now();
                }
                budgets.release(class, left);
                released[slot(class)] += bytes;
            }
            let _ = finished.send(released);
        });
    }
    // A deadlock fails the test instead of hanging it: abort wakes every
    // producer still waiting on a class.
    let Ok(released) = outcome.recv_timeout(Duration::from_secs(30)) else {
        budgets.abort();
        panic!("a producer or the writer never finished: a class never drained");
    };
    for producer in producers {
        producer.join().expect("a producer finished");
    }
    for class in [BudgetClass::Small, BudgetClass::Shared] {
        assert_eq!(budgets.in_flight(class), 0, "{class:?} drained");
        assert_eq!(
            released[slot(class)],
            acquired[slot(class)].load(Ordering::SeqCst),
            "{class:?} got back what it lent"
        );
    }
}
