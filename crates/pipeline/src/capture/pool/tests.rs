//! The pool's lane accounting, head grants and `next_ready`, driven by hand:
//! each test plays the workers (pick a task, return its one step) and the
//! sequencers (pop and advance heads) against the pool's own state, so every
//! bound is checked exactly where it is decided.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use rosetta_adapter_claude_code::ClaudeCodeAdapter;
use rosetta_adapter_support::{FileDiscipline, FileFormat, StreamDriver};
use rosetta_core::rows::RuntimeProvenance;
use rosetta_store::Batch;

use super::*;
use crate::stream::ReportDelta;

fn runtime_row() -> RuntimeRow {
    RuntimeRow {
        id: "rt".into(),
        kind: "claude-code".into(),
        data_root: "~/.claude".into(),
        enabled: true,
        account: None,
        config: None,
        skills: None,
        versions_seen: None,
        discovered_at: 0,
        label: None,
        provenance: RuntimeProvenance::default(),
    }
}

fn file(path: &str, size: u64) -> SessionFile {
    SessionFile {
        ledger_only: false,
        relative_path: path.to_owned(),
        stream_id: "transcript".to_owned(),
        file_format: FileFormat::Jsonl,
        discipline: FileDiscipline::AppendOnly,
        driver: StreamDriver::JsonLines,
        session_id: path.to_owned(),
        native_session_id: path.to_owned(),
        size,
        modified_ms: 0,
        agent_version: None,
        source_missing: false,
        tree_parent: None,
        cut_len: None,
        cut_modified_ms: None,
    }
}

/// One lane of `units` sources of `size` bytes each, never stepped for real.
fn lane_sources(name: &str, units: usize, size: u64) -> Shared {
    Shared {
        adapter: &ClaudeCodeAdapter,
        runtime: runtime_row(),
        root: PathBuf::from("/nonexistent"),
        opts: IngestOptions::default(),
        tailer: Tailer::new(rosetta_capture::TailConfig::json_lines()).unwrap(),
        frames: false,
        compress: None,
        units: (0..units)
            .map(|index| UnitSpec {
                file: file(&format!("{name}/{index}.jsonl"), size),
                cursor: CursorState {
                    id: index as i64 + 1,
                    checkpoint: String::new(),
                    active_generation: 0,
                    highest_generation: 0,
                    session_id: None,
                },
                key: index as u32,
            })
            .collect(),
    }
}

/// A step's output of `bytes`, as a worker queues it.
fn batch(bytes: u64) -> Event {
    Event::Batch(Box::new(Output {
        frames: None,
        batch: Batch {
            ops: Vec::new(),
            checkpoints: Vec::new(),
            source: None,
        },
        deltas: ReportDelta {
            payload_bytes: bytes,
            ..ReportDelta::default()
        },
        profile: Instrument::new().snapshot(),
        ends: None,
    }))
}

const MIB: u64 = 1024 * 1024;

fn budgets(look_ahead: u64) -> PoolBudgets {
    PoolBudgets {
        look_ahead_bytes: look_ahead,
        runtime_floor_bytes: 64 * MIB,
        head_bytes: 64 * MIB,
    }
}

/// One worker step of exactly `unit` (its next dispatch or a parked pass),
/// returning `bytes` and ending the source.
fn step(pool: &CapturePool, runtime: usize, unit: usize, bytes: u64) {
    let mut state = pool.lock();
    let task = state.take(runtime, unit);
    assert!(task.pass.is_none(), "every test source is one step");
    state.returned(runtime, unit, None, batch(bytes));
    assert_accounts(&state);
}

/// The pool's pick, then that one step returning `bytes`.
fn pick_and_step(pool: &CapturePool, chaos: bool, bytes: u64) -> Option<(usize, usize)> {
    let mut state = pool.lock();
    let task = state.pick(&pool.budgets, chaos)?;
    let (runtime, unit) = (task.runtime, task.unit);
    state.returned(runtime, unit, None, batch(bytes));
    assert_accounts(&state);
    Some((runtime, unit))
}

/// Every account equals the sum it stands for.
fn assert_accounts(state: &State) {
    let mut total = 0;
    let mut shared = 0;
    let mut shared_running = 0;
    let mut heads = 0;
    for (_, runtime) in state.registered() {
        let queued: u64 = runtime
            .units
            .iter()
            .enumerate()
            .filter(|(unit, _)| *unit != runtime.head)
            .map(|(_, source)| source.queued)
            .sum();
        assert_eq!(runtime.look_ahead, queued, "a lane's look-ahead");
        let reserved: u64 = runtime.units.iter().map(|source| source.reserved).sum();
        assert_eq!(
            runtime.running_reserved, reserved,
            "a lane's running reservations"
        );
        total += runtime.look_ahead;
        if runtime.class == LaneClass::Shared {
            shared += runtime.look_ahead;
            shared_running += runtime.running_reserved;
        }
        heads += runtime
            .units
            .get(runtime.head)
            .map_or(0, |head| head.queued);
    }
    assert_eq!(state.look_ahead, total, "G's account");
    assert_eq!(state.shared_look_ahead, shared, "the shared account");
    assert_eq!(
        state.shared_running, shared_running,
        "the shared running account"
    );
    assert_eq!(state.head_bytes, heads, "the head queues");
}

fn bounds(pool: &CapturePool) -> Bounds {
    pool.lock().bounds(&pool.budgets)
}

#[test]
fn a_reserved_lane_steps_while_the_shared_account_is_full() {
    let pool = CapturePool::new(2, budgets(512 * MIB));
    let small = pool.register_lane(lane_sources("s", 8, MIB), LaneClass::Reserved);
    let big = pool.register_lane(lane_sources("b", 8, 512 * MIB), LaneClass::Shared);
    // Both heads step first, and hold no look-ahead.
    step(&pool, small.runtime, 0, MIB);
    step(&pool, big.runtime, 0, MIB);
    // The big lane fills what the reserved slice leaves of G: 448 MiB.
    assert_eq!(bounds(&pool).shared_bytes, 448 * MIB);
    for unit in 1..=4 {
        step(&pool, big.runtime, unit, 112 * MIB);
    }
    assert_eq!(pool.lock().shared_look_ahead, 448 * MIB);
    // Lane S still steps its look-ahead, up to its 64 MiB slice; B does not.
    let picked = pick_and_step(&pool, false, 40 * MIB).unwrap();
    assert_eq!(picked, (small.runtime, 1), "lane S steps");
    let picked = pick_and_step(&pool, false, 40 * MIB).unwrap();
    assert_eq!(picked, (small.runtime, 2), "lane S steps under its slice");
    assert_eq!(
        pick_and_step(&pool, false, MIB),
        None,
        "both lanes are full"
    );
    let state = pool.lock();
    assert!(
        !state
            .bounds(&pool.budgets)
            .eligible(state.runtimes[big.runtime].as_ref().unwrap())
    );
}

#[test]
fn the_big_share_is_what_lane_s_leaves_then_its_residual() {
    let pool = CapturePool::new(2, budgets(512 * MIB));
    let small = pool.register_lane(lane_sources("s", 3, MIB), LaneClass::Reserved);
    let _big = pool.register_lane(lane_sources("b", 8, 512 * MIB), LaneClass::Shared);
    assert_eq!(bounds(&pool).share, 448 * MIB, "S open: 512 - 64");
    // S steps everything: its head, then 40 MiB of look-ahead it never
    // consumes, and has no steps left.
    step(&pool, small.runtime, 0, MIB);
    step(&pool, small.runtime, 1, 20 * MIB);
    step(&pool, small.runtime, 2, 20 * MIB);
    let done = bounds(&pool);
    assert_eq!(done.shared_bytes, 472 * MIB, "S done: G minus its residual");
    assert_eq!(done.share, 472 * MIB);
    // As its residual drains (the sequencer consumes through it), B gets it.
    small.advance(0);
    assert_eq!(bounds(&pool).shared_bytes, 492 * MIB);
    small.advance(1);
    assert_eq!(bounds(&pool).shared_bytes, 512 * MIB);
    small.advance(2);
    assert_eq!(bounds(&pool).shared_bytes, 512 * MIB);
}

#[test]
fn look_ahead_stays_within_g_plus_the_workers_overshoot() {
    const WORKERS: usize = 2;
    let pool = CapturePool::new(WORKERS, budgets(512 * MIB));
    let small = pool.register_lane(lane_sources("s", 6, 20 * MIB), LaneClass::Reserved);
    let big = pool.register_lane(lane_sources("b", 80, 16 * MIB), LaneClass::Shared);
    let mut inflight: VecDeque<(usize, usize)> = VecDeque::new();
    let mut small_head = 0;
    let mut rounds = 0;
    loop {
        // Workers pick until each holds a task.
        while inflight.len() < WORKERS {
            let Some(task) = pool.lock().pick(&pool.budgets, false) else {
                break;
            };
            inflight.push_back((task.runtime, task.unit));
        }
        let Some((runtime, unit)) = inflight.pop_front() else {
            break;
        };
        let step = if runtime == small.runtime { 20 } else { 16 } * MIB;
        {
            let mut state = pool.lock();
            state.returned(runtime, unit, None, batch(step));
            assert_accounts(&state);
            let peak = state.stats.peak_look_ahead_bytes;
            assert!(
                peak <= 512 * MIB + WORKERS as u64 * 20 * MIB,
                "round {rounds}: peak {peak}"
            );
            let s = state.runtimes[small.runtime].as_ref().unwrap();
            if s.has_steps_left() {
                assert!(s.look_ahead <= 64 * MIB + 20 * MIB, "S within its slice");
            }
        }
        // The big lane's sequencer is blocked: its head never moves. Lane S
        // consumes its first three sources as they are captured, then stops
        // with the last three queued.
        while small_head < 3
            && pool.lock().runtimes[small.runtime].as_ref().unwrap().units[small_head].finished
        {
            small.advance(small_head);
            small_head += 1;
        }
        rounds += 1;
        assert!(rounds < 1_000, "the pool stops picking");
    }
    let state = pool.lock();
    let s = state.runtimes[small.runtime].as_ref().unwrap();
    assert!(!s.has_steps_left(), "S stepped every source");
    assert_eq!(
        s.look_ahead,
        40 * MIB,
        "S holds two sources behind its head"
    );
    assert_eq!(state.bounds(&pool.budgets).shared_bytes, 472 * MIB);
    drop(state);
    drop(big);
    assert_accounts(&pool.lock());
}

#[test]
fn running_look_ahead_steps_count_against_g() {
    // Every worker picks before any step returns. Each running look-ahead
    // step holds its payload against the bounds, so the picks stop at G
    // instead of overshooting it by a batch per worker.
    const WORKERS: usize = 12;
    let pool = CapturePool::new(WORKERS, budgets(128 * MIB));
    let big = pool.register_lane(lane_sources("b", 40, 16 * MIB), LaneClass::Shared);
    let mut running = Vec::new();
    while running.len() < WORKERS {
        let Some(task) = pool.lock().pick(&pool.budgets, false) else {
            break;
        };
        running.push((task.runtime, task.unit));
    }
    assert_eq!(
        running.len(),
        1 + 8,
        "the exempt head, then 128 MiB of look-ahead"
    );
    {
        let state = pool.lock();
        assert_eq!(state.shared_running, 128 * MIB);
        assert_eq!(state.look_ahead, 0, "nothing returned yet");
        assert_accounts(&state);
    }
    for (runtime, unit) in running {
        let mut state = pool.lock();
        state.returned(runtime, unit, None, batch(16 * MIB));
        assert_accounts(&state);
    }
    let mut state = pool.lock();
    assert_eq!(state.shared_running, 0);
    assert_eq!(
        state.runtimes[big.runtime]
            .as_ref()
            .unwrap()
            .running_reserved,
        0
    );
    assert_eq!(state.look_ahead, 128 * MIB);
    assert_eq!(
        state.stats.peak_look_ahead_bytes,
        128 * MIB,
        "no overshoot past G"
    );
    assert!(
        state.pick(&pool.budgets, false).is_none(),
        "G is full: look-ahead parks"
    );
}

#[test]
fn a_reservation_is_the_payload_left_at_most_one_batch() {
    let batch_bytes = IngestOptions::default().tail.max_batch_bytes as u64;
    let pool = CapturePool::new(2, budgets(512 * MIB));
    let lane = pool.register_lane(lane_sources("b", 3, 100 * MIB), LaneClass::Shared);
    let head = pool.lock().pick(&pool.budgets, false).unwrap();
    assert_eq!(head.unit, 0);
    assert_eq!(pool.lock().shared_running, 0, "the head is exempt");
    let ahead = pool.lock().pick(&pool.budgets, false).unwrap();
    assert_eq!(ahead.unit, 1);
    {
        let state = pool.lock();
        assert_eq!(state.shared_running, batch_bytes.min(100 * MIB));
        assert_accounts(&state);
    }
    drop((head, ahead, lane));
}

#[test]
fn a_withdrawn_lane_leaves_no_reservation_behind() {
    let pool = CapturePool::new(4, budgets(512 * MIB));
    let big = pool.register_lane(lane_sources("b", 8, 16 * MIB), LaneClass::Shared);
    let _small = pool.register_lane(lane_sources("s", 4, MIB), LaneClass::Reserved);
    let runtime = big.runtime;
    let mut running = Vec::new();
    for _ in 0..3 {
        let task = pool.lock().pick(&pool.budgets, false).unwrap();
        if task.runtime == runtime {
            running.push(task.unit);
        }
    }
    assert!(
        pool.lock().shared_running > 0,
        "big-lane look-ahead is running"
    );
    drop(big);
    assert_eq!(pool.lock().shared_running, 0, "withdrawn with its lane");
    // Steps still running return into a withdrawn lane and count nowhere.
    for unit in running {
        let mut state = pool.lock();
        state.returned(runtime, unit, None, batch(16 * MIB));
        assert_accounts(&state);
        assert_eq!(state.shared_running, 0);
        assert_eq!(state.shared_look_ahead, 0);
    }
}

#[test]
fn chaos_bounds_saturate_and_shared_lanes_still_step() {
    let pool = CapturePool::chaos(4, 7);
    let tiny = PoolBudgets {
        look_ahead_bytes: 1,
        runtime_floor_bytes: 1,
        head_bytes: 1,
    };
    assert_eq!(pool.budgets, tiny);
    let claude_s = pool.register_lane(lane_sources("cs", 4, 10), LaneClass::Reserved);
    let codex_s = pool.register_lane(lane_sources("xs", 4, 10), LaneClass::Reserved);
    let big = pool.register_lane(lane_sources("b", 4, 10), LaneClass::Shared);
    // Two reserved holds of one byte each exceed G: G_shared saturates at 1.
    let bounds = bounds(&pool);
    assert_eq!((bounds.slice, bounds.shared_bytes, bounds.share), (1, 1, 1));
    for lane in [&claude_s, &codex_s, &big] {
        step(&pool, lane.runtime, 0, 1);
    }
    // No head is runnable now (each is stepped and over H): only look-ahead
    // is picked, and the shared lane gets its turn.
    let mut stepped_big = false;
    for _ in 0..12 {
        match pick_and_step(&pool, true, 1) {
            Some((runtime, unit)) => {
                assert_ne!(unit, 0);
                stepped_big |= runtime == big.runtime;
            }
            None => break,
        }
    }
    assert!(stepped_big, "a shared lane steps under saturated bounds");
}

#[test]
fn accounts_survive_returns_advances_and_withdrawals() {
    let pool = CapturePool::chaos(3, 11);
    let lanes: Vec<Lane<'_>> = (0..4)
        .map(|index| {
            let class = if index % 2 == 0 {
                LaneClass::Reserved
            } else {
                LaneClass::Shared
            };
            pool.register_lane(lane_sources(&format!("l{index}"), 6, 10), class)
        })
        .collect();
    let mut heads = [0_usize; 4];
    let mut lanes: Vec<Option<Lane<'_>>> = lanes.into_iter().map(Some).collect();
    let mut rng = 0x1234_5678_u64;
    for _ in 0..200 {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        let lane = (rng % 4) as usize;
        match (rng >> 8) % 4 {
            0 | 1 => {
                pick_and_step(&pool, true, 1 + (rng >> 16) % 5);
            }
            2 => {
                if let Some(handle) = &lanes[lane]
                    && heads[lane] < 6
                {
                    handle.advance(heads[lane]);
                    heads[lane] += 1;
                }
            }
            _ => {
                if (rng >> 20).is_multiple_of(8) {
                    lanes[lane] = None;
                }
            }
        }
        assert_accounts(&pool.lock());
    }
}

#[test]
fn head_grants_are_fair() {
    // P = 2 workers, three runnable heads; B has by far the most left.
    let pool = CapturePool::new(2, budgets(512 * MIB));
    let claude_s = pool.register_lane(lane_sources("cs", 40, MIB), LaneClass::Reserved);
    let claude_b = pool.register_lane(lane_sources("cb", 40, 900 * MIB), LaneClass::Shared);
    let codex_s = pool.register_lane(lane_sources("xs", 40, 2 * MIB), LaneClass::Reserved);
    let lanes = [&claude_s, &claude_b, &codex_s];
    let mut heads = [0_usize; 3];
    let mut inflight: VecDeque<(usize, usize)> = VecDeque::new();
    let mut grants = Vec::new();
    while grants.len() < 30 {
        while inflight.len() < 2 {
            let mut state = pool.lock();
            let task = state
                .pick(&pool.budgets, false)
                .expect("a head is runnable");
            let runtime = state.runtimes[task.runtime].as_ref().unwrap();
            assert_eq!(task.unit, runtime.head, "only heads are granted");
            grants.push(task.runtime);
            inflight.push_back((task.runtime, task.unit));
        }
        let (runtime, unit) = inflight.pop_front().unwrap();
        pool.lock().returned(runtime, unit, None, batch(MIB));
        // Its sequencer consumes it at once: the next source is the head.
        let index = lanes
            .iter()
            .position(|lane| lane.runtime == runtime)
            .unwrap();
        lanes[index].advance(unit);
        heads[index] += 1;
    }
    for window in grants.windows(3) {
        let mut seen = window.to_vec();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), 3, "every head in every 3 grants: {grants:?}");
    }
}

#[test]
fn next_ready_takes_the_first_ready_head_in_order() {
    let pool = CapturePool::new(1, budgets(512 * MIB));
    let first = pool.register_lane(lane_sources("a", 2, 10), LaneClass::Reserved);
    let second = pool.register_lane(lane_sources("b", 2, 10), LaneClass::Shared);
    step(&pool, first.runtime, 0, 5);
    step(&pool, second.runtime, 0, 7);
    let opts = IngestOptions::default();
    let heads = [(&second, 0), (&first, 0)];
    let (index, event) = pool
        .next_ready(&heads, Duration::ZERO, &opts)
        .unwrap()
        .expect("both heads are ready");
    assert_eq!(index, 0, "the first ready head in the given order");
    assert_eq!(event.bytes(), 7);
    let (index, event) = pool
        .next_ready(&heads, Duration::ZERO, &opts)
        .unwrap()
        .unwrap();
    assert_eq!((index, event.bytes()), (1, 5));
    assert_accounts(&pool.lock());
    // Nothing queued: a timeout, then cancellation.
    let started = Instant::now();
    assert!(
        pool.next_ready(&heads, Duration::from_millis(30), &opts)
            .unwrap()
            .is_none()
    );
    assert!(started.elapsed() >= Duration::from_millis(30));
    let cancelled = IngestOptions {
        cancel: Some(Arc::new(AtomicBool::new(true))),
        ..IngestOptions::default()
    };
    assert!(matches!(
        pool.next_ready(&heads, Duration::from_secs(5), &cancelled),
        Err(PipelineError::Cancelled)
    ));
    pool.close();
    assert!(pool.next_ready(&heads, Duration::ZERO, &opts).is_err());
}

#[test]
fn next_ready_wakes_on_notify_output() {
    let pool = CapturePool::new(1, budgets(512 * MIB));
    let lane = pool.register_lane(lane_sources("a", 2, 10), LaneClass::Shared);
    let notified: Mutex<Option<Instant>> = Mutex::new(None);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            std::thread::sleep(Duration::from_millis(50));
            *notified.lock().unwrap() = Some(Instant::now());
            pool.notify_output();
        });
        let opts = IngestOptions::default();
        let woke = pool
            .next_ready(&[(&lane, 0)], Duration::from_secs(5), &opts)
            .unwrap();
        assert!(woke.is_none());
        let returned = Instant::now();
        let notified = notified.lock().unwrap().expect("notified before the wake");
        assert!(
            returned.duration_since(notified) < Duration::from_millis(10),
            "woke {:?} after the notify",
            returned.duration_since(notified)
        );
    });
    // A notify before the wait is not lost: the epoch was read before it.
    let epoch = pool.output_epoch();
    pool.notify_output();
    let started = Instant::now();
    let opts = IngestOptions::default();
    assert!(
        pool.next_ready_since(&[(&lane, 0)], Duration::from_secs(5), &opts, epoch)
            .unwrap()
            .is_none()
    );
    assert!(started.elapsed() < Duration::from_millis(10));
}

#[test]
fn a_span_without_pool_units_registers_nothing() {
    let pool = CapturePool::new(1, budgets(512 * MIB));
    let planner = rosetta_store::Store::open_in_memory_for_cold_planning().unwrap();
    let mut member = file("state.sqlite#a", 10);
    member.driver = StreamDriver::SqliteSnapshot;
    let files = [&member];
    let ranks = std::collections::HashMap::from([(member.relative_path.as_str(), 0)]);
    let registered = crate::capture::register(
        &pool,
        &ClaudeCodeAdapter,
        &runtime_row(),
        Path::new("/nonexistent"),
        &planner,
        &files,
        &ranks,
        LaneClass::Reserved,
        &IngestOptions::default(),
        &Instrument::new(),
        None,
    )
    .unwrap();
    assert!(registered.is_none());
    assert_eq!(pool.stats().runtimes, 0);
}
