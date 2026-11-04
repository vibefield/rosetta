//! The writer budgets' producer side: classes, the drain ticket, the release
//! wake and an abort that leaves no class blocked.

use std::collections::VecDeque;
use std::sync::atomic::AtomicUsize;
use std::time::{Duration, Instant};

use super::*;

const MIB: u64 = 1024 * 1024;

#[test]
fn lanes_map_to_their_classes() {
    let split = LaneBudgets::new(true, None);
    assert_eq!(split.class_of(LaneId::Small), BudgetClass::Small);
    assert_eq!(split.class_of(LaneId::Prefix), BudgetClass::Small);
    for lane in [LaneId::Big(0), LaneId::Big(1), LaneId::Path, LaneId::Single] {
        assert_eq!(split.class_of(lane), BudgetClass::Shared, "{lane}");
    }
    assert_eq!(split.budget(BudgetClass::Small).limit, 64 * MIB);
    assert_eq!(split.budget(BudgetClass::Shared).limit, 192 * MIB);
    let one = LaneBudgets::new(false, None);
    for lane in [LaneId::Small, LaneId::Prefix, LaneId::Big(0), LaneId::Path] {
        assert_eq!(one.class_of(lane), BudgetClass::All, "{lane}");
    }
    assert_eq!(one.budget(BudgetClass::All).limit, IN_FLIGHT_BYTES);
}

#[test]
fn an_item_over_its_class_moves_alone_once_the_class_drains() {
    let budgets = LaneBudgets::new(true, None);
    let shared = budgets.budget(BudgetClass::Shared);
    let (streamer, giant) = (budgets.ticket(), budgets.ticket());
    // The streamer fills the class with 16 MiB items.
    let mut in_flight: VecDeque<u64> = VecDeque::new();
    for _ in 0..12 {
        assert!(shared.try_acquire(16 * MIB, streamer).unwrap());
        in_flight.push_back(16 * MIB);
    }
    assert_eq!(shared.in_flight(), 192 * MIB);
    // A 300 MiB item does not fit and takes the class's ticket.
    assert!(!shared.try_acquire(300 * MIB, giant).unwrap());
    let mut admitted = None;
    let mut streamer_passed = 0;
    for release in 1..=200 {
        let Some(bytes) = in_flight.pop_front() else {
            break;
        };
        shared.release(bytes);
        // The streamer keeps trying; the ticket holds it off even though its
        // 16 MiB would fit again (without one it refills the class).
        if shared.try_acquire(16 * MIB, streamer).unwrap() {
            streamer_passed += 1;
            in_flight.push_back(16 * MIB);
        }
        if shared.try_acquire(300 * MIB, giant).unwrap() {
            admitted = Some(release);
            break;
        }
    }
    assert_eq!(streamer_passed, 0, "the streamer got past the ticket");
    // Within one drain of what was in flight.
    assert_eq!(admitted, Some(12));
    assert_eq!(shared.in_flight(), 300 * MIB);
    // The ticket cleared with its holder: the class is HEAD's rule again.
    assert!(!shared.try_acquire(16 * MIB, streamer).unwrap());
    shared.release(300 * MIB);
    assert!(shared.try_acquire(16 * MIB, streamer).unwrap());
    // The small class never saw any of it.
    assert!(
        budgets
            .budget(BudgetClass::Small)
            .try_acquire(64 * MIB, streamer)
            .unwrap()
    );
}

#[test]
fn a_blocking_acquire_honours_the_ticket() {
    let budget = ByteBudget::new(32 * MIB, None);
    assert!(budget.try_acquire(32 * MIB, 1).unwrap());
    assert!(
        !budget.try_acquire(32 * MIB, 2).unwrap(),
        "2 takes the ticket"
    );
    std::thread::scope(|scope| {
        let waiter = scope.spawn(|| {
            // Fits once 16 MiB are released, but 2 holds the ticket.
            budget.acquire(16 * MIB, 3)
        });
        std::thread::sleep(Duration::from_millis(30));
        budget.release(32 * MIB);
        std::thread::sleep(Duration::from_millis(30));
        assert!(!waiter.is_finished(), "3 waits behind the ticket");
        assert!(budget.try_acquire(32 * MIB, 2).unwrap(), "the holder first");
        budget.release(32 * MIB);
        waiter.join().unwrap().unwrap();
    });
    assert_eq!(budget.in_flight(), 16 * MIB);
}

#[test]
fn a_release_wakes_the_sequencers_at_once() {
    let woken = Arc::new(AtomicUsize::new(0));
    let at = Arc::new(Mutex::new(None));
    let hook: ReleaseHook = {
        let (woken, at) = (Arc::clone(&woken), Arc::clone(&at));
        Arc::new(move || {
            woken.fetch_add(1, Ordering::SeqCst);
            *at.lock().unwrap() = Some(Instant::now());
        })
    };
    let budgets = LaneBudgets::new(true, Some(hook));
    for class in [BudgetClass::Small, BudgetClass::Shared] {
        let budget = budgets.budget(class);
        assert!(budget.try_acquire(MIB, budgets.ticket()).unwrap());
        let released = Instant::now();
        budget.release(MIB);
        let woke = at.lock().unwrap().expect("the hook ran");
        assert!(woke.duration_since(released) < Duration::from_millis(10));
    }
    assert_eq!(woken.load(Ordering::SeqCst), 2);
}

#[test]
fn abort_wakes_every_class() {
    let budgets = Arc::new(LaneBudgets::new(true, None));
    assert!(
        budgets
            .budget(BudgetClass::Small)
            .try_acquire(64 * MIB, budgets.ticket())
            .unwrap()
    );
    assert!(
        budgets
            .budget(BudgetClass::Shared)
            .try_acquire(192 * MIB, budgets.ticket())
            .unwrap()
    );
    // Detached, so a class abort forgets cannot hang the test.
    let (done, finished) = std::sync::mpsc::channel();
    for class in [BudgetClass::Small, BudgetClass::Shared] {
        let (budgets, done) = (Arc::clone(&budgets), done.clone());
        std::thread::spawn(move || {
            let ticket = budgets.ticket();
            let _ = done.send((class, budgets.budget(class).acquire(MIB, ticket)));
        });
    }
    assert!(
        finished.recv_timeout(Duration::from_millis(30)).is_err(),
        "both acquires block"
    );
    budgets.abort();
    for _ in 0..2 {
        let (class, acquired) = finished
            .recv_timeout(Duration::from_secs(1))
            .expect("abort wakes every class");
        assert!(acquired.is_err(), "{class:?}");
    }
    assert!(
        budgets
            .budget(BudgetClass::Small)
            .try_acquire(1, budgets.ticket())
            .is_err()
    );
}

#[test]
fn a_dropped_ticket_frees_its_class() {
    let budget = ByteBudget::new(16 * MIB, None);
    assert!(budget.try_acquire(16 * MIB, 1).unwrap());
    assert!(!budget.try_acquire(16 * MIB, 2).unwrap());
    budget.release(16 * MIB);
    // 2's emitter is gone (its walk failed): 3 must not wait on it forever.
    budget.drop_ticket(2);
    assert!(budget.try_acquire(MIB, 3).unwrap());
}

#[test]
fn a_cleared_ticket_wakes_the_sequencers_it_held_off() {
    let woken = Arc::new(AtomicUsize::new(0));
    let hook: ReleaseHook = {
        let woken = Arc::clone(&woken);
        Arc::new(move || {
            woken.fetch_add(1, Ordering::SeqCst);
        })
    };
    let budget = ByteBudget::new(32 * MIB, Some(hook));
    assert!(budget.try_acquire(32 * MIB, 1).unwrap());
    // 2 does not fit and takes the ticket; 3 would fit later but is held off.
    assert!(!budget.try_acquire(16 * MIB, 2).unwrap());
    budget.release(32 * MIB);
    assert_eq!(woken.load(Ordering::SeqCst), 1, "the release wakes");
    // 3 retries before 2 and is still held off by the ticket.
    assert!(!budget.try_acquire(8 * MIB, 3).unwrap());
    // 2's admission clears the ticket: 3 fits now and must not wait for the
    // next release (a giant's apply away) or a sequencer's wait slice.
    assert!(budget.try_acquire(16 * MIB, 2).unwrap());
    assert_eq!(woken.load(Ordering::SeqCst), 2, "a cleared ticket wakes");
    // A ticket given up by a gone emitter wakes them too.
    assert!(!budget.try_acquire(32 * MIB, 4).unwrap());
    budget.drop_ticket(4);
    assert_eq!(woken.load(Ordering::SeqCst), 3, "a dropped ticket wakes");
    assert!(budget.try_acquire(8 * MIB, 3).unwrap());
    // Admission without a ticket wakes nobody.
    assert_eq!(woken.load(Ordering::SeqCst), 3);
}
