//! The lane-merge walk of root-tree runtimes against its own serial driver
//! and against the path walk: whatever the pool's size or a chaos schedule,
//! each lane emits in its own order, every unit is closed from its own
//! events, cursor ids and ledger file ids stay path ranks, the canonical
//! projection and each source's ledger content equal the serial walk's, a
//! held lane never stalls the others, and the newest small tree does not wait
//! for a giant. Runtime-admitted adapters, and root-tree ones under the kill
//! switch, keep the path walk byte for byte.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::time::Duration;

use rosetta_adapter_claude_code::ClaudeCodeAdapter;
use rosetta_adapter_codex::CodexAdapter;
use rosetta_adapter_support::{AgentAdapter, ColdAdmission};
use rosetta_capture::{Ledger, LedgerEntry, SegmentCodec, scan_ledger};
use rosetta_store::Store;
use rusqlite::types::Value;
use tempfile::TempDir;

use super::super::{CapturePool, PoolBudgets};
use super::{
    adapters, assert_same_dump, claude_line, cold_store, copy_tree, dump, fixture_roots, rows,
    tiny_batches,
};
use crate::capture::tests_switch;
use crate::closed::trees::harness::homes;
use crate::{
    ClosedSource, ColdEmit, ColdWalkOptions, EmitOutcome, IngestOptions, Instrument, LaneId,
    PipelineError, SessionFile, StreamedPreparer, StreamedRuntime, WalkOverride, WalkProfile,
    discover, expand_data_root, meta_ingest_inventory_into, stream_closed_sources_into,
    stream_closed_sources_pooled, streamed_setup_for, walk_summary,
};

/// When the harness's emitter holds a lane's emission instead of sending it.
trait Hold {
    /// Whether `lane`'s emission (a retry when `retry`) stays held.
    fn hold(&mut self, lane: LaneId, retry: bool) -> bool;
}

/// One emission event the harness's emitter saw.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Emission {
    lane: LaneId,
    sent: bool,
    paths: Vec<String>,
}

/// The harness's emitter: prepare on emission (as the producer does), send
/// to the writer, or hold by `policy`.
type Prepare<'a> = Box<dyn FnMut(&mut Vec<ClosedSource>) -> Result<(), PipelineError> + 'a>;
type Send<'a> = Box<dyn FnMut(LaneId, Vec<ClosedSource>) -> Result<(), PipelineError> + 'a>;

struct HarnessEmit<'a, 'p> {
    prepare: Prepare<'a>,
    send: Send<'a>,
    policy: Option<&'p mut dyn Hold>,
    held: HashMap<LaneId, Vec<ClosedSource>>,
    log: Vec<Emission>,
    /// Wake the sequencer after each held retry, as a budget release would.
    pool: Option<&'p CapturePool>,
}

impl ColdEmit for HarnessEmit<'_, '_> {
    fn emit(
        &mut self,
        lane: LaneId,
        mut sources: Vec<ClosedSource>,
    ) -> Result<EmitOutcome, PipelineError> {
        assert!(
            !self.held.contains_key(&lane),
            "lane {lane} emitted while held"
        );
        (self.prepare)(&mut sources)?;
        let paths = sources
            .iter()
            .map(|source| source.relative_path().to_owned())
            .collect();
        if self
            .policy
            .as_deref_mut()
            .is_some_and(|policy| policy.hold(lane, false))
        {
            self.log.push(Emission {
                lane,
                sent: false,
                paths,
            });
            self.held.insert(lane, sources);
            return Ok(EmitOutcome::Held);
        }
        self.log.push(Emission {
            lane,
            sent: true,
            paths,
        });
        (self.send)(lane, sources)?;
        Ok(EmitOutcome::Sent)
    }

    fn retry(&mut self, lane: LaneId) -> Result<EmitOutcome, PipelineError> {
        assert!(self.held.contains_key(&lane), "lane {lane} retried unheld");
        if self
            .policy
            .as_deref_mut()
            .is_some_and(|policy| policy.hold(lane, true))
        {
            if let Some(pool) = self.pool {
                pool.notify_output();
            }
            return Ok(EmitOutcome::Held);
        }
        let sources = self.held.remove(&lane).expect("checked above");
        self.log.push(Emission {
            lane,
            sent: true,
            paths: sources
                .iter()
                .map(|source| source.relative_path().to_owned())
                .collect(),
        });
        (self.send)(lane, sources)?;
        Ok(EmitOutcome::Sent)
    }

    fn held(&self, lane: LaneId) -> bool {
        self.held.contains_key(&lane)
    }

    fn blocking(&self) -> bool {
        self.policy.is_none()
    }
}

/// Everything one lane walk leaves behind.
#[derive(Debug)]
struct LaneRun {
    result: Result<(), String>,
    cursors: Vec<Vec<Value>>,
    /// Each emitted source: (lane, path, payload, generation, skipped,
    /// complete), in emission order.
    emitted: Vec<(LaneId, String, u64, u64, bool, bool)>,
    log: Vec<Emission>,
    report: Vec<u64>,
    /// Each source's ledger entries, in their order: what sourceSetDigest
    /// folds.
    sources: BTreeMap<String, Vec<String>>,
    /// Every non-meta ledger entry in ledger order.
    entries: Vec<String>,
    ledger: Vec<(String, Vec<u8>)>,
    canonical: BTreeMap<String, Vec<String>>,
    digest: String,
    /// Each lane's sources in plan order.
    lanes: BTreeMap<LaneId, Vec<String>>,
    profile: Option<WalkProfile>,
    /// What each emitted source's capture steps measured, by path: never
    /// compared between runs.
    clocks: BTreeMap<String, crate::CaptureClock>,
}

impl Default for LaneRun {
    fn default() -> Self {
        Self {
            result: Ok(()),
            cursors: Vec::new(),
            emitted: Vec::new(),
            log: Vec::new(),
            report: Vec::new(),
            sources: BTreeMap::new(),
            entries: Vec::new(),
            ledger: Vec::new(),
            canonical: BTreeMap::new(),
            digest: String::new(),
            lanes: BTreeMap::new(),
            profile: None,
            clocks: BTreeMap::new(),
        }
    }
}

/// How the harness splits trees into lanes.
#[derive(Debug, Clone, Copy, Default)]
enum Split {
    /// The plan's own split.
    #[default]
    Plan,
    /// At the roster's median tree, so small fixtures still walk two lanes.
    Median,
}

/// How one harness run walks.
#[derive(Default)]
struct Setup<'a> {
    /// Serial without one.
    pool: Option<&'a CapturePool>,
    walk: ColdWalkOptions,
    split: Split,
    /// Hold emissions by this policy (the emitter is then non-blocking).
    policy: Option<&'a mut dyn Hold>,
    /// Runs between the metadata phase and the walk.
    before_walk: Option<&'a dyn Fn()>,
    /// Keep every source on the sequencer (the boundary-evidence sample
    /// does), so every lane is always ready.
    local: bool,
}

/// Discover, plan metadata, set up a root-tree runtime and its writer —
/// fresh serve's steps — then walk it as `setup` says, into a 4 KiB-segment
/// ledger, applying each emission as it is sent.
fn run_lanes(
    adapter: &'static dyn AgentAdapter,
    home: &Path,
    opts: &IngestOptions,
    setup: Setup<'_>,
) -> LaneRun {
    let Setup {
        pool,
        walk,
        split,
        policy,
        before_walk,
        local,
    } = setup;
    let instrument = Instrument::new();
    if local {
        instrument.enable_boundary_evidence();
    }
    let discovery = discover(&[adapter], home, opts, &instrument)
        .unwrap()
        .pop()
        .unwrap();
    let mut runtime = discovery.runtime;
    runtime.enabled = true;
    runtime.discovered_at = 1;
    let ledger_dir = TempDir::new().unwrap();
    let mut ledger = Ledger::open(ledger_dir.path())
        .unwrap()
        .with_codec(SegmentCodec::Plain)
        .with_max_segment_bytes(4 * 1024)
        .defer_sync_until_flush()
        .into_async_bounded(64 * 1024);
    let mut planner = Store::open_in_memory_for_cold_planning().unwrap();
    let (_, files) = meta_ingest_inventory_into(
        adapter,
        &runtime,
        &discovery.root,
        &mut planner,
        &discovery.inventory,
        discovery.walk_truncated,
        opts,
        &instrument,
        Some(&mut ledger),
    )
    .unwrap();
    let mut canonical = cold_store();
    for batch in planner.planned_metadata_batches().unwrap() {
        canonical.apply(&batch).unwrap();
    }
    let setup = streamed_setup_for(
        &files,
        &mut planner,
        adapter.cold_admission(),
        discovery.walk_truncated,
    )
    .unwrap();
    let trees = setup.trees().cloned();
    let mut walk = walk;
    if let (Split::Median, Some(trees)) = (split, trees.as_deref()) {
        let mut sizes: Vec<u64> = trees
            .trees()
            .iter()
            .filter(|tree| tree.roster_files > 0)
            .map(|tree| tree.max_source_bytes)
            .collect();
        sizes.sort_unstable();
        walk.big_source_bytes = sizes.get(sizes.len().saturating_sub(1) / 2).copied();
    }
    let summary = walk_summary(adapter, &files, trees.as_deref(), &walk);
    let mut writer =
        StreamedRuntime::open(adapter, &runtime, setup, &mut canonical, &instrument).unwrap();
    let mut preparer = StreamedPreparer::new(&files);
    let mut emitted = Vec::new();
    let mut clocks = BTreeMap::new();
    let has_policy = policy.is_some();
    let mut emit = HarnessEmit {
        prepare: Box::new(|sources| preparer.prepare(sources)),
        send: Box::new(|lane, sources| {
            assert!(sources.iter().all(|source| source.lane() == lane));
            clocks.extend(
                sources
                    .iter()
                    .map(|source| (source.relative_path().to_owned(), source.capture())),
            );
            emitted.extend(sources.iter().map(|source| {
                (
                    lane,
                    source.relative_path().to_owned(),
                    source.payload_bytes(),
                    source.generation,
                    source.skipped(),
                    source.complete,
                )
            }));
            writer
                .apply(&mut canonical, sources, &instrument)
                .map(|_| ())
        }),
        policy,
        held: HashMap::new(),
        log: Vec::new(),
        pool: pool.filter(|_| has_policy),
    };
    if let Some(before_walk) = before_walk {
        before_walk();
    }
    let walked = match pool {
        None => stream_closed_sources_into(
            adapter,
            &runtime,
            &discovery.root,
            &mut planner,
            &files,
            opts,
            &instrument,
            Some(&mut ledger),
            trees.as_deref(),
            &walk,
            &mut emit,
        ),
        Some(pool) => pool.scoped(|| {
            stream_closed_sources_pooled(
                pool,
                adapter,
                &runtime,
                &discovery.root,
                &mut planner,
                &files,
                opts,
                &instrument,
                Some(&mut ledger),
                trees.as_deref(),
                &walk,
                &mut emit,
            )
        }),
    };
    let log = std::mem::take(&mut emit.log);
    drop(emit);
    ledger.flush().unwrap();
    drop(ledger);

    let mut run = LaneRun {
        cursors: rows(&planner, "SELECT * FROM ingest_cursor ORDER BY id"),
        log,
        digest: summary.digest_hex16(),
        ..LaneRun::default()
    };
    let by_rank: HashMap<u32, &SessionFile> = {
        let mut ordered: Vec<&SessionFile> = files.iter().collect();
        ordered.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
        ordered
            .into_iter()
            .enumerate()
            .map(|(rank, file)| (rank as u32, file))
            .collect()
    };
    for source in &summary.sources {
        run.lanes
            .entry(source.lane)
            .or_default()
            .push(by_rank[&source.path_rank].relative_path.clone());
    }
    match walked {
        Ok(mut report) => {
            run.profile = report.walk.take().map(|profile| *profile);
            run.report = vec![
                report.files as u64,
                report.files_missing as u64,
                report.batches,
                report.records,
                report.payload_bytes,
                report.oversized,
                report.parsed,
                report.unknown,
                report.hard_errors,
                report.generations_restarted,
                report.files_audited,
                report.conflicts,
                report.retries_exhausted,
                report.files_incomplete,
                report.files_paused,
                report.sessions_indexed,
                report.siblings_healed,
                report.quarantined,
            ];
            let admitted = preparer
                .finish()
                .map_err(|error| error.to_string())
                .and_then(|prepared| {
                    writer
                        .admit(&mut canonical, &instrument, &prepared)
                        .map_err(|error| error.to_string())
                })
                .and_then(|_| {
                    canonical
                        .finish_bulk_load()
                        .map_err(|error| format!("{error:?}"))
                });
            run.result = admitted.map(|_| ());
            run.canonical = dump(&canonical);
        }
        Err(error) => run.result = Err(error.to_string()),
    }
    run.emitted = emitted;
    run.clocks = clocks;
    for entry in scan_ledger(ledger_dir.path()).unwrap().entries {
        let (path, line) = match &entry {
            LedgerEntry::Raw {
                source,
                generation,
                start_offset,
                payload,
            } => (
                &source.relative_path,
                format!(
                    "raw g{generation} @{start_offset} {}",
                    blake3::hash(payload)
                ),
            ),
            LedgerEntry::Document {
                source,
                generation,
                start_offset,
                document,
            } => (
                &source.relative_path,
                format!(
                    "document g{generation} @{start_offset} {}",
                    blake3::hash(&document.bytes)
                ),
            ),
            LedgerEntry::GenerationComplete {
                source,
                generation,
                normalizer_version,
                normalized_clean,
            } => (
                &source.relative_path,
                format!("seal g{generation} v{normalizer_version} {normalized_clean}"),
            ),
            LedgerEntry::Meta { .. } => continue,
        };
        run.entries.push(format!("{path} {line}"));
        run.sources.entry(path.clone()).or_default().push(line);
    }
    run.ledger = super::ledger_files(ledger_dir.path());
    run
}

fn pools() -> Vec<(String, CapturePool)> {
    let mut pools = vec![
        (
            "pool 1".to_owned(),
            CapturePool::new(1, PoolBudgets::DEFAULT),
        ),
        (
            "pool 2".to_owned(),
            CapturePool::new(2, PoolBudgets::DEFAULT),
        ),
        (
            "pool 4".to_owned(),
            CapturePool::new(4, PoolBudgets::DEFAULT),
        ),
    ];
    pools.extend((0..20).map(|seed| (format!("chaos 4 seed {seed}"), CapturePool::chaos(4, seed))));
    pools
}

/// One emitted source: (path, payload, generation, skipped, complete).
type Closed = (String, u64, u64, bool, bool);

/// Pooled equals serial in everything but the interleaving of different
/// sources' ledger frames, and each lane emits in its own order.
fn assert_lanes_match(label: &str, serial: &LaneRun, pooled: &LaneRun) {
    assert_eq!(pooled.result, serial.result, "{label}: result");
    assert_eq!(pooled.report, serial.report, "{label}: report");
    assert_eq!(pooled.cursors, serial.cursors, "{label}: planner cursors");
    assert_eq!(pooled.sources, serial.sources, "{label}: per-source ledger");
    assert_eq!(pooled.digest, serial.digest, "{label}: walk digest");
    let per_lane = |run: &LaneRun| {
        let mut lanes: BTreeMap<LaneId, Vec<Closed>> = BTreeMap::new();
        for (lane, path, payload, generation, skipped, complete) in &run.emitted {
            lanes.entry(*lane).or_default().push((
                path.clone(),
                *payload,
                *generation,
                *skipped,
                *complete,
            ));
        }
        lanes
    };
    assert_eq!(
        per_lane(pooled),
        per_lane(serial),
        "{label}: lane emissions"
    );
    for (lane, emitted) in per_lane(pooled) {
        let paths: Vec<&String> = emitted.iter().map(|(path, ..)| path).collect();
        let planned: Vec<&String> = pooled.lanes[&lane].iter().collect();
        assert_eq!(paths, planned, "{label}: lane {lane} emits in its order");
    }
    assert_same_dump(label, &serial.canonical, &pooled.canonical);
}

/// Cursor ids are path ranks: 1, 2, … in path order.
fn assert_path_numbered(label: &str, run: &LaneRun) {
    let paths: Vec<String> = run
        .cursors
        .iter()
        .enumerate()
        .map(|(index, row)| {
            assert_eq!(row[0], Value::Integer(index as i64 + 1), "{label}");
            match &row[3] {
                Value::Text(path) => path.clone(),
                other => panic!("{label}: cursor path {other:?}"),
            }
        })
        .collect();
    let mut sorted = paths.clone();
    sorted.sort();
    assert_eq!(paths, sorted, "{label}: cursor ids follow path order");
}

const ROOT_TREE: [&dyn AgentAdapter; 2] = [&ClaudeCodeAdapter, &CodexAdapter];

#[test]
fn lane_walks_match_their_serial_walk_and_the_path_walk() {
    for adapter in ROOT_TREE {
        assert_eq!(adapter.cold_admission(), ColdAdmission::RootTree);
        for (name, home) in homes(adapter) {
            for split in [Split::Plan, Split::Median] {
                let label = format!("{} {name} {split:?}", adapter.kind());
                let opts = tiny_batches(2);
                let walk = ColdWalkOptions::default();
                let serial = run_lanes(
                    adapter,
                    home.path(),
                    &opts,
                    Setup {
                        walk: walk.clone(),
                        split,
                        ..Setup::default()
                    },
                );
                assert!(serial.result.is_ok(), "{label}: {:?}", serial.result);
                assert_path_numbered(&label, &serial);
                // The path walk's projection: the logical result of any walk.
                let path = ColdWalkOptions {
                    walk_override: Some(WalkOverride::Path),
                    ..ColdWalkOptions::default()
                };
                let reference = run_lanes(
                    adapter,
                    home.path(),
                    &opts,
                    Setup {
                        walk: path.clone(),
                        split,
                        ..Setup::default()
                    },
                );
                assert_same_dump(
                    &format!("{label} vs path"),
                    &reference.canonical,
                    &serial.canonical,
                );
                assert_eq!(
                    reference.sources, serial.sources,
                    "{label}: per-source ledger vs path"
                );
                let seeds = if name == "merged-fixtures" || name == "nested-family" {
                    pools()
                } else {
                    pools().into_iter().take(6).collect()
                };
                for (pool_name, pool) in seeds {
                    let pooled = run_lanes(
                        adapter,
                        home.path(),
                        &opts,
                        Setup {
                            pool: Some(&pool),
                            walk: walk.clone(),
                            split,
                            ..Setup::default()
                        },
                    );
                    assert_lanes_match(&format!("{label} {pool_name}"), &serial, &pooled);
                }
            }
        }
    }
}

/// A generated Claude home: one transcript per `(session, records, bytes
/// per record, age in seconds)`, under one project, its modification time
/// pinned that many seconds before a fixed instant.
fn aged_home(sessions: &[(&str, usize, usize, u64)]) -> TempDir {
    let home = TempDir::new().unwrap();
    let project = expand_data_root(&ClaudeCodeAdapter.layout().data_root, home.path())
        .join("projects/-w-pool");
    std::fs::create_dir_all(&project).unwrap();
    for (session, records, bytes, age) in sessions {
        let content = "x".repeat(*bytes);
        let body: String = (0..*records)
            .map(|index| claude_line(session, &format!("{session}-{index}"), &content) + "\n")
            .collect();
        let path = project.join(format!("{session}.jsonl"));
        std::fs::write(&path, body).unwrap();
        pin_age(&path, *age);
    }
    home
}

fn pin_age(path: &Path, age: u64) {
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(std::time::UNIX_EPOCH + Duration::from_secs(1_790_000_000 - age))
        .unwrap();
}

/// Twelve trees, three of them over an 8 KiB split, ages interleaved.
fn mixed_home() -> TempDir {
    aged_home(&[
        ("a-big", 60, 300, 50),
        ("b-small", 3, 40, 10),
        ("c-small", 2, 40, 70),
        ("d-big", 80, 200, 5),
        ("e-small", 4, 40, 30),
        ("f-small", 1, 40, 90),
        ("g-big", 40, 400, 120),
        ("h-small", 5, 40, 15),
        ("i-small", 2, 40, 60),
        ("j-small", 3, 40, 1),
        ("k-small", 2, 40, 100),
        ("l-small", 6, 40, 45),
    ])
}

fn split_at(bytes: u64) -> ColdWalkOptions {
    ColdWalkOptions {
        big_source_bytes: Some(bytes),
        ..ColdWalkOptions::default()
    }
}

#[test]
fn a_generated_corpus_walks_two_lanes_like_its_serial_walk() {
    let home = mixed_home();
    let opts = tiny_batches(4);
    let walk = split_at(8 * 1024);
    let serial = run_lanes(
        &ClaudeCodeAdapter,
        home.path(),
        &opts,
        Setup {
            walk: walk.clone(),
            ..Setup::default()
        },
    );
    assert!(serial.result.is_ok(), "{:?}", serial.result);
    assert_eq!(
        serial.lanes.keys().copied().collect::<Vec<_>>(),
        [LaneId::Small, LaneId::Big(0)]
    );
    // Big lane: newest first.
    let big: Vec<&str> = serial.lanes[&LaneId::Big(0)]
        .iter()
        .map(|path| path.rsplit('/').next().unwrap())
        .collect();
    assert_eq!(big, ["d-big.jsonl", "a-big.jsonl", "g-big.jsonl"]);
    // The serial driver walks the global order: newest tree first, any lane.
    let first: Vec<&str> = serial
        .emitted
        .iter()
        .take(4)
        .map(|(_, path, ..)| path.rsplit('/').next().unwrap())
        .collect();
    assert_eq!(
        first,
        [
            "j-small.jsonl",
            "d-big.jsonl",
            "b-small.jsonl",
            "h-small.jsonl"
        ]
    );
    assert_path_numbered("mixed", &serial);
    let path = ColdWalkOptions {
        walk_override: Some(WalkOverride::Path),
        ..walk.clone()
    };
    let reference = run_lanes(
        &ClaudeCodeAdapter,
        home.path(),
        &opts,
        Setup {
            walk: path.clone(),
            ..Setup::default()
        },
    );
    assert_same_dump("mixed vs path", &reference.canonical, &serial.canonical);
    for (name, pool) in pools() {
        let pooled = run_lanes(
            &ClaudeCodeAdapter,
            home.path(),
            &opts,
            Setup {
                pool: Some(&pool),
                walk: walk.clone(),
                ..Setup::default()
            },
        );
        assert_lanes_match(&format!("mixed {name}"), &serial, &pooled);
        let profile = pooled
            .profile
            .as_ref()
            .expect("a lane walk reports its lanes");
        assert_eq!(
            profile
                .lanes
                .iter()
                .map(|lane| lane.lane)
                .collect::<Vec<_>>(),
            [LaneId::Small, LaneId::Big(0)]
        );
        for lane in &profile.lanes {
            assert_eq!(lane.sources, serial.lanes[&lane.lane].len() as u64);
            assert!(lane.end_nanos > 0, "{name}: lane {} ended", lane.lane);
        }
    }
}

#[test]
fn ledger_file_ids_are_path_ranks_not_lane_positions() {
    let home = mixed_home();
    let opts = tiny_batches(4);
    let walk = split_at(8 * 1024);
    let pool = CapturePool::new(2, PoolBudgets::DEFAULT);
    let run = run_lanes(
        &ClaudeCodeAdapter,
        home.path(),
        &opts,
        Setup {
            pool: Some(&pool),
            walk: walk.clone(),
            ..Setup::default()
        },
    );
    assert!(run.result.is_ok(), "{:?}", run.result);
    assert_path_numbered("keys", &run);
    tests_switch::KEY_BY_LANE_POSITION.set(true);
    let pool = CapturePool::new(2, PoolBudgets::DEFAULT);
    let keyed = run_lanes(
        &ClaudeCodeAdapter,
        home.path(),
        &opts,
        Setup {
            pool: Some(&pool),
            walk: walk.clone(),
            ..Setup::default()
        },
    );
    tests_switch::KEY_BY_LANE_POSITION.set(false);
    let error = keyed.result.expect_err("frames keyed by lane position");
    assert!(error.contains("roster key"), "{error}");
}

#[test]
fn runtime_adapters_and_the_kill_switch_keep_the_path_walk_byte_for_byte() {
    let mut scenarios = 0;
    for adapter in adapters() {
        let kill = ColdWalkOptions {
            admission_override: true,
            ..ColdWalkOptions::default()
        };
        let walks: Vec<ColdWalkOptions> = match adapter.cold_admission() {
            ColdAdmission::Runtime => vec![ColdWalkOptions::default()],
            ColdAdmission::RootTree => vec![kill],
        };
        let path = ColdWalkOptions {
            walk_override: Some(WalkOverride::Path),
            ..ColdWalkOptions::default()
        };
        for (scenario, data) in fixture_roots(adapter.kind()) {
            let home = TempDir::new().unwrap();
            copy_tree(
                &data,
                &expand_data_root(&adapter.layout().data_root, home.path()),
            );
            let opts = tiny_batches(2);
            for walk in &walks {
                for pooled in [false, true] {
                    let pool = || pooled.then(|| CapturePool::chaos(4, 3));
                    let (first, second) = (pool(), pool());
                    let label = format!("{} {scenario} {walk:?} pooled {pooled}", adapter.kind());
                    let reference = run_lanes(
                        adapter,
                        home.path(),
                        &opts,
                        Setup {
                            pool: first.as_ref(),
                            walk: path.clone(),
                            ..Setup::default()
                        },
                    );
                    let run = run_lanes(
                        adapter,
                        home.path(),
                        &opts,
                        Setup {
                            pool: second.as_ref(),
                            walk: walk.clone(),
                            ..Setup::default()
                        },
                    );
                    assert_eq!(run.result, reference.result, "{label}");
                    assert!(run.profile.is_none(), "{label}: the path walk");
                    assert!(
                        run.lanes.keys().all(|lane| *lane == LaneId::Path),
                        "{label}"
                    );
                    assert_eq!(run.entries, reference.entries, "{label}: ordered entries");
                    assert!(run.ledger == reference.ledger, "{label}: ledger bytes");
                    assert_eq!(run.emitted, reference.emitted, "{label}: emissions");
                }
            }
            scenarios += 1;
        }
    }
    assert!(scenarios >= 9, "only {scenarios} scenarios");
}

mod attribution;
mod clock;
mod schedule;
