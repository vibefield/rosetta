use std::sync::mpsc;
use std::time::Duration;

use rosetta_capture::{Ledger, scan_ledger, sniff_segment};

use super::*;
use crate::maintenance_pace::budget_delay;

#[test]
fn budget_targets_ten_percent_without_banking_idle_time() {
    assert_eq!(
        budget_delay(
            Duration::from_millis(5),
            Duration::from_millis(5),
            CPU_PERCENT
        ),
        Duration::from_millis(45),
    );
    assert_eq!(
        budget_delay(
            Duration::from_millis(5),
            Duration::from_millis(80),
            CPU_PERCENT
        ),
        Duration::ZERO,
    );
}

#[test]
fn waits_for_readiness_preserves_live_segment_and_stops_while_waiting() {
    let dir = tempfile::TempDir::new().unwrap();
    let mut cold = Ledger::open(dir.path())
        .unwrap()
        .with_codec(SegmentCodec::Plain);
    cold.append_meta(1, "metadata").unwrap();
    cold.flush().unwrap();
    drop(cold);
    let mut live = Ledger::open(dir.path()).unwrap();
    let paths = live.closed_segment_paths().unwrap();
    assert_eq!(paths.len(), 1);
    live.append_meta(1, "live-before").unwrap();
    live.flush().unwrap();
    assert_eq!(live.closed_segment_paths().unwrap(), paths);
    let before = scan_ledger(dir.path()).unwrap().entries;

    let allowed = Arc::new(AtomicBool::new(false));
    let gate = allowed.clone();
    let (observed_tx, observed_rx) = mpsc::channel();
    let worker = BackgroundCompression::start(paths.clone(), move || {
        let _ = observed_tx.send(());
        gate.load(Ordering::Acquire)
    })
    .unwrap();
    observed_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(sniff_segment(&paths[0]).unwrap(), Some(SegmentCodec::Plain));
    allowed.store(true, Ordering::Release);
    let until = Instant::now() + Duration::from_secs(5);
    while sniff_segment(&paths[0]).unwrap() == Some(SegmentCodec::Plain) {
        assert!(
            Instant::now() < until,
            "compression did not finish after readiness"
        );
        thread::sleep(Duration::from_millis(10));
    }
    drop(worker);
    assert_eq!(scan_ledger(dir.path()).unwrap().entries, before);
    live.append_meta(1, "live-after").unwrap();
    live.flush().unwrap();
    assert_eq!(
        scan_ledger(dir.path()).unwrap().entries.len(),
        before.len() + 1
    );

    // Dropping a worker waiting on search must not wait for search to finish.
    let (waiting_tx, waiting_rx) = mpsc::channel();
    let blocked = BackgroundCompression::start(paths, move || {
        let _ = waiting_tx.send(());
        false
    })
    .unwrap();
    waiting_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let started = Instant::now();
    drop(blocked);
    assert!(started.elapsed() < Duration::from_secs(2));
}
