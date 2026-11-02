//! The walk plan of one streamed fresh-cold runtime: which sources its
//! sequencer consumes in which lane, and in what order.
//!
//! A runtime admitted per root tree ([`ColdAdmission::RootTree`]) walks its
//! newest trees first, in lanes: a prefix span `P` of the physical files that
//! several trees read (a Codex `state_N.sqlite`), lane `S` of every tree whose
//! largest source is at most [`BIG_SOURCE_BYTES`], and [`BIG_LANES`] big lanes
//! for the rest. Each lane holds whole trees by recency (newest first, then
//! tree id), each tree's sources in path order. The global serial order is
//! `P`, then every lane's trees merged by that same key: the serial driver's
//! walk and the reference for every equality test.
//!
//! Every other runtime — admitted whole, fallen back to that, or under the
//! admission kill switch — keeps exactly the path walk it always had.
//!
//! Two index spaces stay apart: a source's **path rank** (its position in
//! path order) names its planner cursor, its canonical cursor row and its
//! ledger file id whatever the walk; its **lane position** only orders the
//! pool and the sequencer.
//!
//! The plan is pure and RAM only (R1 invariant I3).
//!
//! [`ColdAdmission::RootTree`]: rosetta_adapter_support::ColdAdmission::RootTree

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64};

use rosetta_adapter_support::{AgentAdapter, StreamDriver};

use super::trees::{TreeId, TreeMap};
use crate::meta::SessionFile;

const MIB: u64 = 1024 * 1024;

/// A tree whose largest own source is larger than this walks a big lane.
/// Equal to the census's large-source bound (`ingest_metrics::LARGE_BYTES`).
pub const BIG_SOURCE_BYTES: u64 = 64 * MIB;

/// Big lanes per runtime. R1.5 raises it only together with streamed pieces
/// (R1 brief §19): two open giants without them overrun the memory gate.
pub const BIG_LANES: u8 = 1;

/// One span of a walk plan; every span but `P` is a sequencer lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum LaneId {
    /// Physical files several trees read, before every tree.
    Prefix,
    /// Trees whose largest source is at most [`BIG_SOURCE_BYTES`].
    Small,
    /// The other trees, dealt in recency order over [`BIG_LANES`] lanes.
    Big(u8),
    /// `--experiment-cold-lanes 1`: every tree in one strict recency lane.
    Single,
    /// The path walk: runtime admission, a fallback, or the kill switch.
    Path,
}

impl LaneId {
    /// Lanes whose emissions share the writer's small budget class.
    pub fn is_small_class(self) -> bool {
        matches!(self, Self::Prefix | Self::Small)
    }

    pub fn is_big(self) -> bool {
        matches!(self, Self::Big(_))
    }
}

impl fmt::Display for LaneId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Prefix => formatter.write_str("P"),
            Self::Small => formatter.write_str("S"),
            Self::Big(index) => write!(formatter, "B{index}"),
            Self::Single => formatter.write_str("single"),
            Self::Path => formatter.write_str("path"),
        }
    }
}

/// How a plan orders its sources.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalkOrder {
    /// Newest trees first, in lanes.
    Recency,
    /// Exactly the path walk, and why.
    Path { reason: &'static str },
}

/// Test harness orders (never a flag).
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalkOverride {
    /// The pre-R1 path walk, whatever the admission.
    Path,
    /// The default recency plan.
    Recency,
    /// Trees oldest first (the recency key reversed).
    Reversed,
    /// Trees in a seeded random order.
    Seeded(u64),
}

/// What a resume-kill test can observe at each sequencer event.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProbeView {
    pub event: u64,
    /// Lanes whose head unit has consumed at least one event and not closed.
    pub lanes_mid_source: u32,
    /// Writer items between their first and terminal transaction.
    pub items_mid_sequence: u32,
    /// Flip batches the writer has committed.
    pub flips_committed: u64,
    /// Span `P` has captured a group that it has not closed yet.
    pub prefix_mid_group: bool,
}

/// The writer's side of [`ProbeView`], which it updates as it goes.
#[doc(hidden)]
#[derive(Debug, Default)]
pub struct ProbeCounters {
    pub items_mid_sequence: AtomicU32,
    pub flips_committed: AtomicU64,
}

/// A predicate over [`ProbeView`]: the walk cancels when it returns true.
#[doc(hidden)]
pub type WalkProbe = Arc<dyn Fn(&ProbeView) -> bool + Send + Sync>;

/// How a fresh cold build walks its root-tree runtimes.
#[derive(Clone, Default)]
pub struct ColdWalkOptions {
    /// `--experiment-cold-admission runtime`: every runtime walks its paths.
    pub admission_override: bool,
    /// `--experiment-cold-lanes 1`: one strict recency lane per runtime.
    pub single_lane: bool,
    /// Big lanes instead of [`BIG_LANES`]; walk-plan tests only, no flag.
    #[doc(hidden)]
    pub big_lanes: Option<u8>,
    /// The lane split instead of [`BIG_SOURCE_BYTES`]; tests only (scaled
    /// corpora), no flag.
    #[doc(hidden)]
    pub big_source_bytes: Option<u64>,
    /// A harness order instead of the plan's own.
    #[doc(hidden)]
    pub walk_override: Option<WalkOverride>,
    /// Tests: cancel the walk at the first event this returns true for.
    #[doc(hidden)]
    pub probe: Option<WalkProbe>,
    /// The writer's counters the probe reads; zeros when absent.
    #[doc(hidden)]
    pub probe_counters: Option<Arc<ProbeCounters>>,
}

impl fmt::Debug for ColdWalkOptions {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ColdWalkOptions")
            .field("admission_override", &self.admission_override)
            .field("single_lane", &self.single_lane)
            .field("big_lanes", &self.big_lanes)
            .field("big_source_bytes", &self.big_source_bytes)
            .field("walk_override", &self.walk_override)
            .field("probe", &self.probe.is_some())
            .finish()
    }
}

impl ColdWalkOptions {
    pub(crate) fn lanes(&self) -> u8 {
        self.big_lanes.unwrap_or(BIG_LANES).max(1)
    }

    pub(crate) fn split_bytes(&self) -> u64 {
        self.big_source_bytes.unwrap_or(BIG_SOURCE_BYTES)
    }
}

/// One runtime's walk.
pub(crate) struct WalkPlan<'a> {
    pub(crate) order: WalkOrder,
    /// `--experiment-cold-lanes 1`.
    pub(crate) single: bool,
    /// Each span with its sources in lane order, spans in lane order.
    pub(crate) spans: Vec<(LaneId, Vec<&'a SessionFile>)>,
    /// The global serial order: (span, position in that span).
    pub(crate) global: Vec<(LaneId, u32)>,
    /// Each source's position in path order, by relative path.
    pub(crate) path_rank: HashMap<&'a str, u32>,
    pub(crate) digest: [u8; 32],
    /// Physical files hoisted into `P`.
    pub(crate) groups_hoisted: u32,
    /// The lane of each tree's own sources (`P` for a tree that has only
    /// hoisted members), by [`TreeId`]; empty for a path walk.
    pub(crate) lane_of_tree: Vec<Option<LaneId>>,
}

impl WalkPlan<'_> {
    /// The plan's sources in global order.
    pub(crate) fn global_files(&self) -> impl Iterator<Item = (LaneId, &SessionFile)> + '_ {
        let spans: HashMap<LaneId, &Vec<&SessionFile>> = self
            .spans
            .iter()
            .map(|(lane, files)| (*lane, files))
            .collect();
        self.global
            .iter()
            .map(move |(lane, index)| (*lane, spans[lane][*index as usize]))
    }

    fn mode(&self) -> &'static str {
        match self.order {
            WalkOrder::Path { .. } => "path",
            WalkOrder::Recency if self.single => "single",
            WalkOrder::Recency => "recency",
        }
    }
}

/// [`walk_plan`] for `adapter`'s roster: a snapshot member is hoistable only
/// when its stream reads no per-session record queries.
pub(crate) fn walk_plan_for<'a>(
    adapter: &dyn AgentAdapter,
    files: &'a [SessionFile],
    trees: Option<&TreeMap>,
    opts: &ColdWalkOptions,
) -> WalkPlan<'a> {
    plan(files, trees, opts, &|file: &SessionFile| {
        file.driver == StreamDriver::SqliteSnapshot
            && adapter
                .snapshot_plan(&file.stream_id)
                .is_some_and(|plan| plan.record_queries.is_empty())
    })
}

/// The walk plan of `files`, the roster of a runtime admitted per tree when
/// `trees` is `Some`, treating every snapshot member as hoistable.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn walk_plan<'a>(
    files: &'a [SessionFile],
    trees: Option<&TreeMap>,
    opts: &ColdWalkOptions,
) -> WalkPlan<'a> {
    plan(files, trees, opts, &|file: &SessionFile| {
        file.driver == StreamDriver::SqliteSnapshot
    })
}

pub(crate) fn path_ordered(files: &[SessionFile]) -> Vec<&SessionFile> {
    let mut ordered: Vec<&SessionFile> = files.iter().collect();
    ordered.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    ordered
}

/// A source's bytes as the lane split measures them: the skim's cut, else
/// the census size (`SessionFile::read_len`).
pub(crate) fn read_len(file: &SessionFile) -> u64 {
    file.cut_len.unwrap_or(file.size)
}

/// A source's own recency: the skim's mtime, else the census's.
fn recency_ms(file: &SessionFile) -> i64 {
    file.cut_modified_ms.unwrap_or(file.modified_ms)
}

fn plan<'a>(
    files: &'a [SessionFile],
    trees: Option<&TreeMap>,
    opts: &ColdWalkOptions,
    hoistable: &dyn Fn(&SessionFile) -> bool,
) -> WalkPlan<'a> {
    let ordered = path_ordered(files);
    let path_rank: HashMap<&str, u32> = ordered
        .iter()
        .enumerate()
        .map(|(rank, file)| {
            let rank = u32::try_from(rank).expect("a roster holds fewer than 2^32 sources");
            (file.relative_path.as_str(), rank)
        })
        .collect();
    let path = |reason| path_plan(&ordered, path_rank.clone(), reason, opts);
    let Some(trees) = trees else {
        return path("runtime");
    };
    if opts.admission_override {
        return path("override");
    }
    if opts.walk_override == Some(WalkOverride::Path) {
        return path("override");
    }

    // Each source's tree, and which trees read each physical member file.
    let mut tree_of = Vec::with_capacity(ordered.len());
    let mut readers: HashMap<&str, Option<TreeId>> = HashMap::new();
    for file in &ordered {
        let Some(tree) = trees.tree_of_file(file) else {
            return path("unmapped");
        };
        tree_of.push(tree);
        if file.driver != StreamDriver::JsonLines {
            readers
                .entry(file.physical_path())
                .and_modify(|seen| {
                    if *seen != Some(tree) {
                        *seen = None;
                    }
                })
                .or_insert(Some(tree));
        }
    }
    let shared = |file: &SessionFile| {
        file.driver != StreamDriver::JsonLines && readers.get(file.physical_path()) == Some(&None)
    };

    // P: members of a physical file several trees read, by physical path.
    let mut prefix: Vec<&SessionFile> = ordered
        .iter()
        .copied()
        .filter(|file| shared(file))
        .collect();
    if prefix.iter().any(|file| !hoistable(file)) {
        return path("hoist");
    }
    prefix.sort_by(|left, right| {
        (left.physical_path(), &left.relative_path)
            .cmp(&(right.physical_path(), &right.relative_path))
    });
    let groups_hoisted = readers.values().filter(|seen| seen.is_none()).count();

    // Each tree's own sources, in path order.
    let mut own: BTreeMap<TreeId, Vec<&SessionFile>> = BTreeMap::new();
    for (file, tree) in ordered.iter().zip(&tree_of) {
        if !shared(file) {
            own.entry(*tree).or_default().push(file);
        }
    }
    let mut order: Vec<TreeId> = own.keys().copied().collect();
    let newest_first = |left: &TreeId, right: &TreeId| {
        trees
            .tree(*right)
            .k
            .cmp(&trees.tree(*left).k)
            .then(left.cmp(right))
    };
    match opts.walk_override {
        Some(WalkOverride::Reversed) => order.sort_by(|left, right| newest_first(right, left)),
        Some(WalkOverride::Seeded(seed)) => {
            order.sort_by(newest_first);
            shuffle(&mut order, seed);
        }
        _ => order.sort_by(newest_first),
    }

    // Lanes: whole trees, dealt in that order.
    let split = opts.split_bytes();
    let lanes = opts.lanes();
    let mut lane_of_tree = vec![None; trees.trees().len()];
    let mut big_seen = 0_u32;
    for tree in &order {
        let lane = if opts.single_lane {
            LaneId::Single
        } else if trees.tree(*tree).max_source_bytes <= split {
            LaneId::Small
        } else {
            let lane = LaneId::Big((big_seen % u32::from(lanes)) as u8);
            big_seen += 1;
            lane
        };
        lane_of_tree[*tree as usize] = Some(lane);
    }
    for file in &prefix {
        let tree = trees.tree_of_file(file).expect("mapped above") as usize;
        lane_of_tree[tree].get_or_insert(LaneId::Prefix);
    }

    let mut spans: BTreeMap<LaneId, Vec<&SessionFile>> = BTreeMap::new();
    let mut global = Vec::with_capacity(ordered.len());
    for file in prefix {
        let span = spans.entry(LaneId::Prefix).or_default();
        global.push((LaneId::Prefix, position(span.len())));
        span.push(file);
    }
    for tree in &order {
        let lane = lane_of_tree[*tree as usize].expect("dealt above");
        for file in &own[tree] {
            let span = spans.entry(lane).or_default();
            global.push((lane, position(span.len())));
            span.push(file);
        }
    }
    let mut plan = WalkPlan {
        order: WalkOrder::Recency,
        single: opts.single_lane,
        spans: spans.into_iter().collect(),
        global,
        path_rank,
        digest: [0; 32],
        groups_hoisted: u32::try_from(groups_hoisted).unwrap_or(u32::MAX),
        lane_of_tree,
    };
    plan.digest = digest(&plan, opts);
    plan
}

fn path_plan<'a>(
    ordered: &[&'a SessionFile],
    path_rank: HashMap<&'a str, u32>,
    reason: &'static str,
    opts: &ColdWalkOptions,
) -> WalkPlan<'a> {
    let mut plan = WalkPlan {
        order: WalkOrder::Path { reason },
        single: false,
        spans: vec![(LaneId::Path, ordered.to_vec())],
        global: (0..ordered.len())
            .map(|index| (LaneId::Path, position(index)))
            .collect(),
        path_rank,
        digest: [0; 32],
        groups_hoisted: 0,
        lane_of_tree: Vec::new(),
    };
    plan.digest = digest(&plan, opts);
    plan
}

fn position(index: usize) -> u32 {
    u32::try_from(index).expect("a roster holds fewer than 2^32 sources")
}

/// blake3 over the split, the big-lane count, the mode and the (span, path)
/// sequence in global order.
fn digest(plan: &WalkPlan<'_>, opts: &ColdWalkOptions) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(&opts.split_bytes().to_le_bytes());
    hasher.update(&[opts.lanes()]);
    let mode = plan.mode();
    hasher.update(&(mode.len() as u64).to_le_bytes());
    hasher.update(mode.as_bytes());
    for (lane, file) in plan.global_files() {
        let lane = lane.to_string();
        hasher.update(&(lane.len() as u64).to_le_bytes());
        hasher.update(lane.as_bytes());
        hasher.update(&(file.relative_path.len() as u64).to_le_bytes());
        hasher.update(file.relative_path.as_bytes());
    }
    *hasher.finalize().as_bytes()
}

/// Fisher–Yates over a seeded xorshift.
fn shuffle(order: &mut [TreeId], seed: u64) {
    let mut state = seed | 1;
    for index in (1..order.len()).rev() {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        order.swap(index, (state % (index as u64 + 1)) as usize);
    }
}

/// One source of a walk, as structure only: no path, id or content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WalkSource {
    pub path_rank: u32,
    pub lane: LaneId,
    /// `None` for a path walk, which knows no trees.
    pub tree: Option<TreeId>,
    pub read_len: u64,
    /// Milliseconds older than the roster's newest source.
    pub age_ms: u64,
}

/// What the `walk:` line and the `walk-plan` event report of one runtime's
/// plan, and the lane each tree walks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalkSummary {
    pub order: WalkOrder,
    /// `recency`, `single` or `path`.
    pub mode: &'static str,
    /// Trees with a roster source.
    pub trees: u32,
    pub small: u32,
    pub big: u32,
    /// Sequencer lanes holding sources (`P` excluded).
    pub lanes: u32,
    pub groups_hoisted: u32,
    /// Bytes of the trees in big lanes.
    pub big_bytes: u64,
    pub digest: [u8; 32],
    /// By [`TreeId`]; empty for a path walk.
    pub lane_of_tree: Vec<Option<LaneId>>,
    /// Every source in global order.
    pub sources: Vec<WalkSource>,
}

impl WalkSummary {
    /// The digest's first 16 hex digits.
    pub fn digest_hex16(&self) -> String {
        self.digest[..8]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    /// Whether the walk is the path walk, whose emission is blocking.
    pub fn is_path(&self) -> bool {
        matches!(self.order, WalkOrder::Path { .. })
    }

    /// The `walk:` stdout line.
    pub fn line(&self, runtime_id: &str, agent: &str, fell_back: bool) -> String {
        let (order, reason) = match self.order {
            WalkOrder::Recency => ("recency", "-"),
            WalkOrder::Path { .. } if fell_back => ("path", "fallback"),
            WalkOrder::Path { reason } => ("path", reason),
        };
        format!(
            "walk: runtime={runtime_id} agent={agent} order={order} reason={reason} trees={} \
             small={} big={} lanes={} groups_hoisted={} big_bytes={} digest={}",
            self.trees,
            self.small,
            self.big,
            self.lanes,
            self.groups_hoisted,
            self.big_bytes,
            self.digest_hex16()
        )
    }
}

/// The plan [`crate::stream_closed_sources_pooled`] walks for this roster,
/// summarized for the `walk:` line, the `walk-plan` event and the writer.
pub fn walk_summary(
    adapter: &dyn AgentAdapter,
    files: &[SessionFile],
    trees: Option<&TreeMap>,
    opts: &ColdWalkOptions,
) -> WalkSummary {
    let plan = walk_plan_for(adapter, files, trees, opts);
    summarize(&plan, trees)
}

pub(crate) fn summarize(plan: &WalkPlan<'_>, trees: Option<&TreeMap>) -> WalkSummary {
    let newest = plan
        .global_files()
        .map(|(_, file)| recency_ms(file))
        .max()
        .unwrap_or(0);
    let tree_of = |file: &SessionFile| match plan.order {
        WalkOrder::Recency => trees.and_then(|trees| trees.tree_of_file(file)),
        WalkOrder::Path { .. } => None,
    };
    let sources = plan
        .global_files()
        .map(|(lane, file)| WalkSource {
            path_rank: plan.path_rank[file.relative_path.as_str()],
            lane,
            tree: tree_of(file),
            read_len: read_len(file),
            age_ms: u64::try_from(newest.saturating_sub(recency_ms(file))).unwrap_or(0),
        })
        .collect();
    let mut summary = WalkSummary {
        order: plan.order,
        mode: plan.mode(),
        trees: 0,
        small: 0,
        big: 0,
        lanes: u32::try_from(
            plan.spans
                .iter()
                .filter(|(lane, files)| *lane != LaneId::Prefix && !files.is_empty())
                .count(),
        )
        .unwrap_or(u32::MAX),
        groups_hoisted: plan.groups_hoisted,
        big_bytes: 0,
        digest: plan.digest,
        lane_of_tree: plan.lane_of_tree.clone(),
        sources,
    };
    if let Some(trees) = trees {
        summary.trees = u32::try_from(
            trees
                .trees()
                .iter()
                .filter(|tree| tree.roster_files > 0)
                .count(),
        )
        .unwrap_or(u32::MAX);
        for (tree, lane) in plan.lane_of_tree.iter().enumerate() {
            match lane {
                Some(LaneId::Small) => summary.small += 1,
                Some(LaneId::Big(_)) => {
                    summary.big += 1;
                    summary.big_bytes = summary.big_bytes.saturating_add(trees.trees()[tree].bytes);
                }
                _ => {}
            }
        }
    }
    summary
}

#[cfg(test)]
mod tests;
