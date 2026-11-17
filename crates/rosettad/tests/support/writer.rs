//! A harness copy of fresh serve's R1 writer for one runtime (R1 brief §4.6;
//! rosettad `cold_stream/writer.rs` and `queues.rs`, which the binary keeps
//! private): one queue per lane served round-robin, one transaction per
//! turn; span `P` ahead of its runtime's lanes; small sources coalesced
//! strictly; a source over the chunk bound stepped in Stage and resolved-row
//! slices, so another lane's turn comes between two of its slices; ready
//! trees flipped between turns; the remainder admitted at the close, after
//! the ready trees. How turns interleave with emissions, and flips with
//! turns, is seeded, so one test sweeps schedules the threaded writer could
//! take.
//!
//! [`build`] drives one runtime end to end as fresh serve does — metadata,
//! the walk (serial, or on a chaos pool, under any harness walk order) into
//! a raw ledger, the writer — and returns what the equivalence tests compare.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::Path;

use rosetta_adapter_support::{AgentAdapter, ColdAdmission};
use rosetta_capture::{Ledger, LedgerEntry, scan_ledger, verify_ledger};
use rosetta_pipeline::{
    ApplyItem, BlockingEmit, CapturePool, ClosedSource, ColdWalkOptions, EarlyToolReport, FlipCaps,
    IngestOptions, Instrument, LaneId, PipelineError, SessionFile, Step, StreamedPreparer,
    StreamedRuntime, TreeAdmission, TreeMap, TxKind, stream_closed_sources_into,
    stream_closed_sources_pooled, streamed_setup_for, walk_summary,
};
use rosetta_store::Store;
use tempfile::TempDir;

use super::harness::{Planned, plan_into};
use super::projection_dump::{Dump, dump};

const MIB: u64 = 1024 * 1024;
/// rosettad `queues.rs`: what one coalesced transaction may hold.
const TRANSACTION_BYTES: u64 = 16 * MIB;
const TRANSACTION_SOURCES: usize = 256;
/// rosettad `flips.rs` CAPS.
const CAPS: FlipCaps = FlipCaps {
    trees: 64,
    sessions: 512,
    tail_bytes: 16 * MIB,
};

/// How the writer's turns fall between emissions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Schedule {
    /// Every emission applied and every ready tree flipped before the next
    /// emission: the writer never lags.
    Drain,
    /// After each emission zero to two turns; a ready tree flips at a turn
    /// half the time, in batches of one to three; at the close half the
    /// ready batches are left to the remainder.
    Seeded(u64),
}

/// The admission a build sets its runtime up with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    /// What the adapter declares, as fresh serve does.
    Declared,
    /// Runtime admission: the pre-R1 reference.
    Runtime,
}

/// Where the lane split falls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Split {
    /// The plan's own (`BIG_SOURCE_BYTES`).
    Plan,
    /// At the roster's median tree, so small corpora walk two lanes.
    Median,
    /// At this many bytes (a scaled corpus).
    Bytes(u64),
}

/// One harness build.
#[derive(Debug, Clone)]
pub struct BuildSpec {
    pub admission: Admission,
    pub walk: ColdWalkOptions,
    pub split: Split,
    /// Capture on `CapturePool::chaos(4, seed)`; serial without one.
    pub pool: Option<u64>,
    /// The writer's chunk bound; `u64::MAX` applies every source whole.
    pub chunk: u64,
    pub schedule: Schedule,
    pub opts: IngestOptions,
}

impl BuildSpec {
    /// The reference: runtime admission, the path walk, serial, every
    /// source whole, admitted at the close.
    pub fn reference() -> Self {
        Self {
            admission: Admission::Runtime,
            walk: ColdWalkOptions::default(),
            split: Split::Plan,
            pool: None,
            chunk: u64::MAX,
            schedule: Schedule::Drain,
            opts: IngestOptions::default(),
        }
    }

    /// Fresh serve's R1 build: the declared admission and the plan's walk.
    pub fn declared() -> Self {
        Self {
            admission: Admission::Declared,
            ..Self::reference()
        }
    }
}

/// What a test hears from the writer between its transactions.
pub trait WriterHooks {
    /// After a committed step or flip: the sessions the writer invalidates
    /// because a usage displacement changed a tree already flipped (D15).
    fn invalidated(&mut self, _sessions: &[String]) {}
    /// After a flip batch committed.
    fn flipped(&mut self, _admission: &TreeAdmission) {}
}

/// Everything one build leaves behind.
#[derive(Debug)]
pub struct Built {
    pub result: Result<(), String>,
    /// The finished projection's logical dump; empty when the build failed.
    pub dump: Dump,
    /// The raw ledger's order-sensitive source digest and its
    /// order-insensitive source set digest (I4, I5).
    pub source_digest: String,
    pub source_set_digest: String,
    /// Every non-meta ledger entry, in ledger order.
    pub entries: Vec<String>,
    /// Closure violations: an op of a root-tree source naming a session of
    /// another tree, or a span-`P` member emitting ops (I7).
    pub closure: Vec<String>,
    /// Flip batches committed (the close's own included), and the trees
    /// and sessions they made readable before the remainder.
    pub flips: usize,
    pub flipped_trees: usize,
    pub flipped_sessions: Vec<String>,
    /// Sessions the remainder's commit moved.
    pub remainder: Vec<String>,
    /// Stage and resolved-row transactions.
    pub slices: u64,
    /// Items begun while another lane's item was between its slices.
    pub beside: u64,
    pub report: EarlyToolReport,
    pub path_walk: bool,
    /// Every source in the walk's global order.
    pub order: Vec<String>,
    /// The raw ledger the build wrote, kept for a rebuild, and its runtime.
    pub ledger: Option<TempDir>,
    pub runtime: Option<rosetta_core::rows::RuntimeRow>,
}

impl Default for Built {
    fn default() -> Self {
        Self {
            result: Ok(()),
            dump: Dump::new(),
            source_digest: String::new(),
            source_set_digest: String::new(),
            entries: Vec::new(),
            closure: Vec::new(),
            flips: 0,
            flipped_trees: 0,
            flipped_sessions: Vec::new(),
            remainder: Vec::new(),
            slices: 0,
            beside: 0,
            report: EarlyToolReport::default(),
            path_walk: false,
            order: Vec::new(),
            ledger: None,
            runtime: None,
        }
    }
}

struct LaneQueue {
    pending: VecDeque<(ClosedSource, u64)>,
    active: Option<ApplyItem>,
}

impl LaneQueue {
    fn has_work(&self) -> bool {
        self.active.is_some() || !self.pending.is_empty()
    }
}

/// The harness writer of one runtime.
pub struct HarnessWriter<'h> {
    runtime: StreamedRuntime,
    /// Lanes in first-arrival order: the round-robin order.
    keys: Vec<LaneId>,
    lanes: HashMap<LaneId, LaneQueue>,
    next: usize,
    chunk: u64,
    schedule: Schedule,
    rng: u64,
    hooks: Option<&'h mut dyn WriterHooks>,
    flips: usize,
    slices: u64,
    beside: u64,
}

impl<'h> HarnessWriter<'h> {
    pub fn new(
        runtime: StreamedRuntime,
        chunk: u64,
        schedule: Schedule,
        hooks: Option<&'h mut dyn WriterHooks>,
    ) -> Self {
        let seed = match schedule {
            Schedule::Drain => 0,
            Schedule::Seeded(seed) => seed,
        };
        Self {
            runtime,
            keys: Vec::new(),
            lanes: HashMap::new(),
            next: 0,
            chunk,
            schedule,
            // xorshift must not start at zero.
            rng: seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1,
            hooks,
            flips: 0,
            slices: 0,
            beside: 0,
        }
    }

    pub fn runtime(&self) -> &StreamedRuntime {
        &self.runtime
    }

    fn random(&mut self) -> u64 {
        let mut value = self.rng;
        value ^= value << 13;
        value ^= value >> 7;
        value ^= value << 17;
        self.rng = value;
        value
    }

    /// Queue one emission of `lane`, per source with its budget bytes.
    pub fn push(&mut self, lane: LaneId, sources: Vec<ClosedSource>) {
        if !self.lanes.contains_key(&lane) {
            self.keys.push(lane);
            self.lanes.insert(
                lane,
                LaneQueue {
                    pending: VecDeque::new(),
                    active: None,
                },
            );
        }
        let queue = self.lanes.get_mut(&lane).expect("inserted above");
        for source in sources {
            let bytes = source.budget_bytes();
            queue.pending.push_back((source, bytes));
        }
    }

    /// The turns that follow one emission.
    pub fn after_emission(
        &mut self,
        store: &mut Store,
        instrument: &Instrument,
    ) -> Result<(), PipelineError> {
        match self.schedule {
            Schedule::Drain => self.drain(store, instrument),
            Schedule::Seeded(_) => {
                for _ in 0..self.random() % 3 {
                    self.turn(store, instrument, true)?;
                }
                Ok(())
            }
        }
    }

    /// Run turns until nothing is queued and no tree is ready.
    pub fn drain(
        &mut self,
        store: &mut Store,
        instrument: &Instrument,
    ) -> Result<(), PipelineError> {
        while self.turn(store, instrument, true)? {}
        Ok(())
    }

    fn flip_now(&mut self) -> bool {
        match self.schedule {
            Schedule::Drain => true,
            Schedule::Seeded(_) => self.random().is_multiple_of(2),
        }
    }

    fn flip(&mut self, store: &mut Store, instrument: &Instrument) -> Result<(), PipelineError> {
        let caps = match self.schedule {
            Schedule::Drain => CAPS,
            Schedule::Seeded(_) => FlipCaps {
                trees: 1 + (self.random() % 3) as usize,
                ..CAPS
            },
        };
        let admitted = self.runtime.admit_trees(store, instrument, caps)?;
        assert!(!admitted.trees.is_empty(), "a ready tree flips");
        self.flips += 1;
        let displaced = self
            .runtime
            .flipped_among(&admitted.stats.usage_displaced_sessions);
        if let Some(hooks) = self.hooks.as_deref_mut() {
            hooks.flipped(&admitted);
            hooks.invalidated(&displaced);
        }
        Ok(())
    }

    /// One writer transaction: a flip batch when one is ready (and
    /// `may_flip`), else the next lane's turn. Returns whether it ran one.
    fn turn(
        &mut self,
        store: &mut Store,
        instrument: &Instrument,
        may_flip: bool,
    ) -> Result<bool, PipelineError> {
        let ready = self.runtime.ready_trees() > 0;
        if ready && may_flip && self.flip_now() {
            self.flip(store, instrument)?;
            return Ok(true);
        }
        let Some(lane) = self.next_key() else {
            if ready && may_flip {
                self.flip(store, instrument)?;
                return Ok(true);
            }
            return Ok(false);
        };
        let chunk = self.chunk;
        let others_active = self
            .lanes
            .iter()
            .any(|(other, queue)| *other != lane && queue.active.is_some());
        let queue = self.lanes.get_mut(&lane).expect("a key has a queue");
        let mut item = match queue.active.take() {
            Some(item) => item,
            None => {
                let (first, mut bytes) = queue.pending.pop_front().expect("work queued");
                let mut sources = vec![first];
                let single = bytes > chunk;
                while !single
                    && let Some((_, next)) = queue.pending.front()
                    && *next <= chunk
                    && bytes.saturating_add(*next) <= TRANSACTION_BYTES
                    && sources.len() < TRANSACTION_SOURCES
                {
                    let (source, next) = queue.pending.pop_front().expect("front exists");
                    bytes = bytes.saturating_add(next);
                    sources.push(source);
                }
                if others_active {
                    self.beside += 1;
                }
                self.runtime
                    .begin_item(sources, bytes, if single { chunk } else { u64::MAX })?
            }
        };
        let (step, _, stats) = self.runtime.step(store, &mut item, instrument)?;
        if item
            .last_step()
            .is_some_and(|last| last.kind != TxKind::Terminal)
        {
            self.slices += 1;
        }
        if step == Step::More {
            self.lanes.get_mut(&lane).expect("a key has a queue").active = Some(item);
        }
        let displaced = self.runtime.flipped_among(&stats.usage_displaced_sessions);
        if let Some(hooks) = self.hooks.as_deref_mut() {
            hooks.invalidated(&displaced);
        }
        Ok(true)
    }

    /// The next lane with work, round-robin, span `P` first.
    fn next_key(&mut self) -> Option<LaneId> {
        let count = self.keys.len();
        let index = (0..count)
            .map(|offset| (self.next + offset) % count)
            .find(|index| self.lanes[&self.keys[*index]].has_work())?;
        self.next = index + 1;
        let prefix_busy = self
            .lanes
            .get(&LaneId::Prefix)
            .is_some_and(LaneQueue::has_work);
        Some(if prefix_busy {
            LaneId::Prefix
        } else {
            self.keys[index]
        })
    }

    /// The runtime's close: everything queued applied, the ready trees
    /// flipped (a seeded schedule leaves some to the remainder), then the
    /// remainder in one commit.
    pub fn close(
        mut self,
        store: &mut Store,
        instrument: &Instrument,
        prepared: &rosetta_pipeline::PreparedAdmission,
    ) -> Result<Closed, PipelineError> {
        while self.turn(store, instrument, false)? {}
        while self.runtime.ready_trees() > 0 && self.flip_now() {
            self.flip(store, instrument)?;
        }
        let flipped_trees = self.runtime.flipped_trees();
        let flipped_sessions = self.runtime.flipped_session_ids();
        let admitted = self.runtime.admit_remainder(store, instrument, prepared)?;
        if let Some(hooks) = self.hooks.as_deref_mut() {
            hooks.invalidated(&admitted.displaced);
        }
        Ok(Closed {
            flips: self.flips,
            flipped_trees,
            flipped_sessions,
            remainder: admitted.changed,
            slices: self.slices,
            beside: self.beside,
        })
    }
}

/// What a runtime's close leaves.
pub struct Closed {
    pub flips: usize,
    pub flipped_trees: usize,
    pub flipped_sessions: Vec<String>,
    pub remainder: Vec<String>,
    pub slices: u64,
    pub beside: u64,
}

/// The lane split's bytes for `split`.
fn split_bytes(split: Split, trees: Option<&TreeMap>) -> Option<u64> {
    match split {
        Split::Plan => None,
        Split::Bytes(bytes) => Some(bytes),
        Split::Median => {
            let mut sizes: Vec<u64> = trees?
                .trees()
                .iter()
                .filter(|tree| tree.roster_files > 0)
                .map(|tree| tree.max_source_bytes)
                .collect();
            sizes.sort_unstable();
            sizes.get(sizes.len().saturating_sub(1) / 2).copied()
        }
    }
}

/// I7 on one emission of a root-tree runtime, before the preparer takes
/// anything out: every op names a session of its source's own tree, and a
/// span-`P` member emits none. [`rosetta_core::IngestOp::named_session`] is
/// an exhaustive match, so a new op variant is classified before it builds.
fn audit_closure(
    trees: &TreeMap,
    files: &HashMap<&str, &SessionFile>,
    lane: LaneId,
    sources: &[ClosedSource],
    violations: &mut Vec<String>,
) {
    for source in sources {
        let path = source.relative_path();
        if lane == LaneId::Prefix {
            if !source.ops().is_empty() {
                violations.push(format!("{path}: span P emitted {}", source.ops().len()));
            }
            continue;
        }
        let own = files.get(path).and_then(|file| trees.tree_of_file(file));
        for op in source.ops() {
            if let Some(target) = op.named_session()
                && trees.tree_of(target) != own
            {
                violations.push(format!("{path} writes {target}"));
            }
        }
    }
}

fn ledger_entries(dir: &Path) -> Vec<String> {
    let mut entries = Vec::new();
    for entry in scan_ledger(dir).unwrap().entries {
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
        entries.push(format!("{path} {line}"));
    }
    entries
}

/// Build one runtime of `home` into `canonical` as `spec` says, finishing
/// the bulk load when it publishes.
pub fn build(
    adapter: &'static dyn AgentAdapter,
    home: &Path,
    spec: &BuildSpec,
    canonical: &mut Store,
    hooks: Option<&mut dyn WriterHooks>,
) -> Built {
    let ledger_dir = TempDir::new().unwrap();
    let mut ledger = Ledger::open(ledger_dir.path()).unwrap();
    let Planned {
        adapter,
        runtime,
        root,
        mut planner,
        files,
        opts,
    } = plan_into(
        adapter,
        home,
        canonical,
        spec.opts.clone(),
        Some(&mut ledger),
    );
    let instrument = Instrument::new();
    let admission = match spec.admission {
        Admission::Declared => adapter.cold_admission(),
        Admission::Runtime => ColdAdmission::Runtime,
    };
    let setup = streamed_setup_for(&files, &mut planner, admission, false).unwrap();
    let trees = setup.trees().cloned();
    let mut walk = spec.walk.clone();
    if let Some(bytes) = split_bytes(spec.split, trees.as_deref()) {
        walk.big_source_bytes = Some(bytes);
    }
    let summary = walk_summary(adapter, &files, trees.as_deref(), &walk);
    let path_walk = summary.is_path();
    let mut ranked: Vec<&str> = files
        .iter()
        .map(|file| file.relative_path.as_str())
        .collect();
    ranked.sort_unstable();
    let order: Vec<String> = summary
        .sources
        .iter()
        .map(|source| ranked[source.path_rank as usize].to_owned())
        .collect();
    // I7 holds whatever the walk: every root-tree source is audited.
    let closure_trees = trees.clone();
    let mut preparer = match &trees {
        Some(trees) if !path_walk => StreamedPreparer::with_trees(
            &files,
            std::sync::Arc::clone(trees),
            summary.lane_of_tree.clone(),
        ),
        _ => StreamedPreparer::new(&files),
    };
    let streamed = StreamedRuntime::open(adapter, &runtime, setup, canonical, &instrument).unwrap();
    let mut writer = HarnessWriter::new(streamed, spec.chunk, spec.schedule, hooks);
    let by_path: HashMap<&str, &SessionFile> = files
        .iter()
        .map(|file| (file.relative_path.as_str(), file))
        .collect();
    let mut closure = Vec::new();
    let walked = {
        let mut emit = BlockingEmit(|lane, mut sources: Vec<ClosedSource>| {
            if let Some(trees) = closure_trees.as_deref() {
                audit_closure(trees, &by_path, lane, &sources, &mut closure);
            }
            preparer.prepare(&mut sources)?;
            writer.push(lane, sources);
            writer.after_emission(canonical, &instrument)
        });
        match spec.pool {
            None => stream_closed_sources_into(
                adapter,
                &runtime,
                &root,
                &mut planner,
                &files,
                &opts,
                &instrument,
                Some(&mut ledger),
                trees.as_deref(),
                &walk,
                &mut emit,
            ),
            Some(seed) => {
                let pool = CapturePool::chaos(4, seed);
                pool.scoped(|| {
                    stream_closed_sources_pooled(
                        &pool,
                        adapter,
                        &runtime,
                        &root,
                        &mut planner,
                        &files,
                        &opts,
                        &instrument,
                        Some(&mut ledger),
                        trees.as_deref(),
                        &walk,
                        &mut emit,
                    )
                })
            }
        }
    };
    ledger.flush().unwrap();
    drop(ledger);
    let report = preparer.report();
    let mut built = Built {
        closure,
        report,
        path_walk,
        order,
        runtime: Some(runtime.clone()),
        ..Built::default()
    };
    let closed = walked
        .map_err(|error| error.to_string())
        .and_then(|_| preparer.finish().map_err(|error| error.to_string()))
        .and_then(|prepared| {
            built.report = prepared.report();
            writer
                .close(canonical, &instrument, &prepared)
                .map_err(|error| error.to_string())
        })
        .and_then(|closed| {
            canonical
                .finish_bulk_load()
                .map(|_| closed)
                .map_err(|error| format!("{error:?}"))
        });
    match closed {
        Ok(closed) => {
            built.flips = closed.flips;
            built.flipped_trees = closed.flipped_trees;
            built.flipped_sessions = closed.flipped_sessions;
            built.remainder = closed.remainder;
            built.slices = closed.slices;
            built.beside = closed.beside;
            built.dump = dump(canonical);
        }
        Err(error) => built.result = Err(error),
    }
    let verify = verify_ledger(ledger_dir.path()).unwrap();
    built.source_digest = verify.source_digest.to_hex().to_string();
    built.source_set_digest = verify.source_set_digest.to_hex().to_string();
    built.entries = ledger_entries(ledger_dir.path());
    built.ledger = Some(ledger_dir);
    built
}

/// Each session's tree id, for messages about trees.
pub fn tree_ids(trees: &TreeMap) -> BTreeMap<String, String> {
    trees
        .trees()
        .iter()
        .flat_map(|tree| {
            tree.sessions
                .iter()
                .map(move |session| (session.clone(), tree.id.clone()))
        })
        .collect()
}
