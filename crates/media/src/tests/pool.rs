//! Codec admission, on private pools so parallel tests cannot interfere.
use super::corpus;
use crate::Lane::{Background, Foreground};
use crate::pool::{Pool, admits};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

fn pool() -> &'static Pool {
    Box::leak(Box::default())
}

fn wait_until(pool: &Pool, expected: (usize, usize)) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while pool.snapshot() != expected {
        assert!(Instant::now() < deadline, "pool never reached {expected:?}");
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn rule_keeps_two_slots_and_admits_background_only_into_an_idle_pool() {
    assert!(admits(0, 0, Foreground));
    assert!(admits(1, 0, Foreground));
    assert!(admits(1, 3, Foreground));
    assert!(!admits(2, 0, Foreground));
    assert!(admits(0, 0, Background));
    assert!(!admits(1, 0, Background), "a job is running");
    assert!(!admits(0, 1, Background), "a foreground request is waiting");
    assert!(!admits(2, 0, Background));
}

#[test]
fn one_background_job_runs_beside_at_most_one_foreground_job() {
    let pool = pool();
    let foreground = pool.admit(Foreground);
    assert!(pool.try_admit(Background).is_none());
    drop(foreground);
    let background = pool
        .try_admit(Background)
        .expect("an idle pool admits background");
    assert!(pool.try_admit(Background).is_none());
    let foreground = pool
        .try_admit(Foreground)
        .expect("foreground beside background");
    assert!(pool.try_admit(Foreground).is_none());
    assert!(pool.try_admit(Background).is_none());
    assert_eq!(pool.snapshot(), (2, 0));
    drop((background, foreground));
    assert_eq!(pool.snapshot(), (0, 0));
    assert_eq!(pool.admitted(), 3);
}

#[test]
fn foreground_admission_waits_for_a_free_slot() {
    let pool = pool();
    let first = pool.admit(Foreground);
    let second = pool.admit(Foreground);
    let (sender, receiver) = mpsc::channel();
    let waiter = std::thread::spawn(move || {
        let permit = pool.admit(Foreground);
        sender.send(()).unwrap();
        drop(permit);
    });
    wait_until(pool, (2, 1));
    assert!(receiver.recv_timeout(Duration::from_millis(50)).is_err());
    assert!(pool.try_admit(Background).is_none());
    drop(first);
    receiver
        .recv_timeout(Duration::from_secs(10))
        .expect("admitted once a slot freed");
    waiter.join().unwrap();
    drop(second);
    assert_eq!(pool.snapshot(), (0, 0));
    assert!(pool.try_admit(Background).is_some());
}

#[test]
fn prepare_is_a_foreground_job_that_counts_while_it_waits() {
    let pool = pool();
    let bytes = corpus::png_rgba(16, 16);
    let background = pool.try_admit(Background).unwrap();
    let prepared = pool.prepare(&bytes).expect("admitted beside background");
    assert_eq!(prepared.dimensions.width, 16);
    pool.prepare(&bytes).unwrap();
    assert_eq!(pool.admitted(), 2, "a cache hit is not a codec job");
    drop(background);

    let held = [pool.admit(Foreground), pool.admit(Foreground)];
    let other = corpus::png_rgba(24, 8);
    let job = std::thread::spawn(move || pool.prepare(&other).map(|p| p.dimensions.width));
    wait_until(pool, (2, 1));
    drop(held);
    assert_eq!(job.join().unwrap(), Ok(24));
    assert_eq!(pool.snapshot(), (0, 0));
}

#[test]
fn concurrent_lanes_never_exceed_two_jobs_or_one_background_job() {
    let pool = pool();
    let running = &*Box::leak(Box::new(AtomicUsize::new(0)));
    let background = &*Box::leak(Box::new(AtomicUsize::new(0)));
    let peaks = &*Box::leak(Box::new([AtomicUsize::new(0), AtomicUsize::new(0)]));
    let workers: Vec<_> = (0..8)
        .map(|index| {
            std::thread::spawn(move || {
                for round in 0..200 {
                    let is_background = (index + round) % 3 == 0;
                    let permit = if is_background {
                        match pool.try_admit(Background) {
                            Some(permit) => permit,
                            None => {
                                std::thread::yield_now();
                                continue;
                            }
                        }
                    } else {
                        pool.admit(Foreground)
                    };
                    let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                    peaks[0].fetch_max(now, Ordering::SeqCst);
                    if is_background {
                        let now = background.fetch_add(1, Ordering::SeqCst) + 1;
                        peaks[1].fetch_max(now, Ordering::SeqCst);
                    }
                    std::thread::yield_now();
                    if is_background {
                        background.fetch_sub(1, Ordering::SeqCst);
                    }
                    running.fetch_sub(1, Ordering::SeqCst);
                    drop(permit);
                }
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }
    assert!(peaks[0].load(Ordering::SeqCst) <= 2);
    assert!(peaks[1].load(Ordering::SeqCst) <= 1);
    assert_eq!(pool.snapshot(), (0, 0));
}
