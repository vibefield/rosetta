//! Snapshot shapes: a deterministic PLAN of a whole agent home — root trees,
//! their subagents and sidecars, every file's size and mtime — from a
//! [`ShapeSpec`] and a seed. [`crate::snapshot`] materializes a plan.
//!
//! [`ShapeSpec::macbook_v1`] is the MacBook-shaped UI corpus (metrics:X8b):
//! the counts and the newest day's bytes of the maintainer's MacBook census
//! (structure-only aggregates of trace-1: tree, session, file and big-tree
//! counts, the newest giant's size, age rings), never a per-tree row, a name
//! or a byte of content. Sizes inside each class are synthetic draws.
//!
//! What is kept and what is scaled (bytes only; every count is kept):
//! - the newest day (24 trees, 13 big) at full byte scale, the newest giant
//!   (NEWEST, 668,350,304 bytes in 459 files, its root transcript the largest
//!   file at 210,808,432 bytes) exactly;
//! - every older big tree stays big (its root transcript is 64 MiB plus
//!   0.5–4 MiB, so it walks the big lane), but its other files, and every
//!   older small tree, are a heavy-tailed size × [`ShapeSpec::tail_scale`].
//!
//! Pure: no IO, no clock, no environment. Same spec + seed, same plan.

use serde::Serialize;

use crate::prng::Rng;
use crate::synth;

const HOUR_MS: i64 = 3_600_000;
const MIB: u64 = 1024 * 1024;

/// A ring of trees by age (the tree's newest mtime before NEWEST).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RingSpec {
    pub name: &'static str,
    pub trees: u32,
    /// How many of them are big (largest own source over the big line).
    pub big: u32,
    pub min_age_h: f64,
    pub max_age_h: f64,
}

/// NEWEST: the newest root, a big tree kept at its exact size.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GiantSpec {
    pub tree_bytes: u64,
    pub root_bytes: u64,
    /// Subagents, each with a transcript and a sidecar.
    pub subagents: u32,
}

/// The newest day, kept at full byte scale.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NewestDaySpec {
    pub trees: u32,
    pub big: u32,
    /// Trees within 1 h of NEWEST (NEWEST included); the rest sit at 9–11 h.
    pub within_1h: u32,
    /// Bytes of the big trees, the giant included.
    pub big_bytes: u64,
    pub small_bytes: u64,
}

/// A small codex corpus (line mode), so both enrolled agents have sources.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CodexShape {
    pub sessions: u64,
    pub lines: u64,
    pub seed: u64,
    /// One age (hours before NEWEST) per rollout, in path order.
    pub ages_h: Vec<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ShapeSpec {
    pub name: &'static str,
    pub roots: u32,
    pub projects: u32,
    /// A tree is big when its largest own source exceeds this (the daemon's
    /// `BIG_SOURCE_BYTES`).
    pub big_line_bytes: u64,
    /// Subagents with a transcript (roster sessions = roots + this).
    pub subagents: u32,
    /// Sidecars beside a subagent transcript.
    pub sidecars: u32,
    /// Subagents with a sidecar and no transcript (metadata-only sessions).
    pub sidecar_only: u32,
    /// Trees that are their root transcript alone.
    pub single_file_trees: u32,
    pub max_subagents_per_tree: u32,
    pub giant: GiantSpec,
    pub newest_day: NewestDaySpec,
    /// The older rings, newest first.
    pub rings: Vec<RingSpec>,
    /// The tail's bytes: a big tree's non-root files and every small tree.
    pub tail_scale: f64,
    /// Unscaled bytes of the older small trees, shared by heavy-tailed draws.
    pub tail_small_real_bytes: u64,
    /// Unscaled bytes of the older big trees' non-root files.
    pub tail_big_child_real_bytes: u64,
    /// An older big tree's root transcript: the big line plus [lo, hi).
    pub tail_big_root_extra: (u64, u64),
    pub min_transcript_bytes: u64,
    /// NEWEST's mtime.
    pub newest_ms: i64,
    pub codex: CodexShape,
}

impl ShapeSpec {
    /// The MacBook-shaped UI corpus (census aggregates of trace-1, 2026-11-02).
    pub fn macbook_v1() -> Self {
        Self {
            name: "macbook-shape-v1",
            roots: 1_139,
            projects: 119,
            big_line_bytes: 64 * MIB,
            subagents: 6_923,
            sidecars: 5_245,
            sidecar_only: 550,
            single_file_trees: 351,
            max_subagents_per_tree: 1_051,
            giant: GiantSpec {
                tree_bytes: 668_350_304,
                root_bytes: 210_808_432,
                subagents: 229,
            },
            newest_day: NewestDaySpec {
                trees: 24,
                big: 13,
                within_1h: 23,
                big_bytes: 4_480_000_000,
                small_bytes: 1_200_000_000,
            },
            rings: vec![
                ring("7d", 15, 9, 24.0, 168.0),
                ring("30d", 14, 3, 168.0, 720.0),
                ring("older", 1_086, 15, 720.0, 2_900.0),
            ],
            tail_scale: 1.0 / 40.0,
            tail_small_real_bytes: 5_630_000_000,
            tail_big_child_real_bytes: 7_000_000_000,
            tail_big_root_extra: (MIB / 2, 4 * MIB),
            min_transcript_bytes: crate::claude_code::MIN_SHAPED_BYTES,
            newest_ms: 1_788_220_800_000, // 2026-09-01T00:00:00Z
            codex: CodexShape {
                sessions: 9,
                lines: 1_500,
                seed: 44,
                ages_h: vec![0.5, 3.0, 20.0, 48.0, 72.0, 120.0, 216.0, 336.0, 600.0],
            },
        }
    }

    /// A test-sized shape: the same classes at a 64 KiB big line.
    pub fn tiny() -> Self {
        Self {
            name: "tiny-shape",
            roots: 12,
            projects: 4,
            big_line_bytes: 64 * 1024,
            subagents: 20,
            sidecars: 14,
            sidecar_only: 3,
            single_file_trees: 3,
            max_subagents_per_tree: 8,
            giant: GiantSpec {
                tree_bytes: 600_000,
                root_bytes: 200_000,
                subagents: 6,
            },
            newest_day: NewestDaySpec {
                trees: 5,
                big: 3,
                within_1h: 4,
                big_bytes: 1_500_000,
                small_bytes: 150_000,
            },
            rings: vec![
                ring("7d", 3, 1, 24.0, 168.0),
                ring("30d", 2, 0, 168.0, 720.0),
                ring("older", 2, 1, 720.0, 2_900.0),
            ],
            tail_scale: 1.0 / 40.0,
            tail_small_real_bytes: 2_000_000,
            tail_big_child_real_bytes: 2_000_000,
            tail_big_root_extra: (8 * 1024, 32 * 1024),
            min_transcript_bytes: crate::claude_code::MIN_SHAPED_BYTES,
            newest_ms: 1_788_220_800_000,
            codex: CodexShape {
                sessions: 2,
                lines: 60,
                seed: 44,
                ages_h: vec![0.5, 30.0],
            },
        }
    }

    pub fn by_name(name: &str) -> Option<Self> {
        match name {
            "macbook-shape-v1" => Some(Self::macbook_v1()),
            "tiny-shape" => Some(Self::tiny()),
            _ => None,
        }
    }
}

fn ring(name: &'static str, trees: u32, big: u32, min_age_h: f64, max_age_h: f64) -> RingSpec {
    RingSpec {
        name,
        trees,
        big,
        min_age_h,
        max_age_h,
    }
}

/// A subagent with a transcript.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChildPlan {
    pub agent_id: String,
    pub bytes: u64,
    pub mtime_ms: i64,
    pub seed: u64,
    /// Its `agent-<id>.meta.json`, when it has one.
    pub sidecar: Option<String>,
}

/// A subagent known only from its sidecar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SidecarOnlyPlan {
    pub agent_id: String,
    pub mtime_ms: i64,
    pub document: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TreePlan {
    pub root_id: String,
    pub project: u64,
    /// 0: the newest day, then the spec's rings in order.
    pub ring: usize,
    pub big: bool,
    pub root_bytes: u64,
    pub root_mtime_ms: i64,
    pub root_seed: u64,
    pub children: Vec<ChildPlan>,
    pub sidecar_only: Vec<SidecarOnlyPlan>,
}

impl TreePlan {
    pub fn cwd(&self) -> String {
        synth::project_cwd(self.project)
    }

    /// `projects/<slug>` under the claude data root.
    pub fn project_dir(&self) -> String {
        format!("projects/{}", synth::project_slug(&self.cwd()))
    }

    pub fn bytes(&self) -> u64 {
        self.root_bytes
            + self
                .children
                .iter()
                .map(|c| c.bytes + c.sidecar.as_ref().map_or(0, |s| s.len() as u64))
                .sum::<u64>()
            + self
                .sidecar_only
                .iter()
                .map(|s| s.document.len() as u64)
                .sum::<u64>()
    }

    pub fn files(&self) -> u64 {
        1 + self.children.len() as u64
            + self.children.iter().filter(|c| c.sidecar.is_some()).count() as u64
            + self.sidecar_only.len() as u64
    }

    pub fn max_file(&self) -> u64 {
        self.children
            .iter()
            .map(|c| c.bytes)
            .fold(self.root_bytes, u64::max)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ShapePlan {
    pub spec: ShapeSpec,
    pub seed: u64,
    /// Trees newest first; `trees[0]` is NEWEST, the giant.
    pub trees: Vec<TreePlan>,
}

impl ShapePlan {
    pub fn claude_files(&self) -> u64 {
        self.trees.iter().map(TreePlan::files).sum()
    }

    pub fn claude_bytes(&self) -> u64 {
        self.trees.iter().map(TreePlan::bytes).sum()
    }
}

/// Plan `spec` from `seed`. Panics on a spec whose classes cannot hold their
/// counts (a spec error, caught by the tests).
pub fn plan(spec: &ShapeSpec, seed: u64) -> ShapePlan {
    let mut rng = Rng::stream(seed, 0x5AA9_E001);
    let rings = ring_layout(spec, &mut rng);
    let n = rings.len();
    let newest_day = spec.newest_day.trees as usize;
    let line = spec.big_line_bytes;
    let small_cap = line - line / 20;

    // Tree bytes and minimum file counts, before subagent counts.
    let mut tree_bytes = vec![0u64; n];
    let mut min_children = vec![0u32; n];
    tree_bytes[0] = spec.giant.tree_bytes;
    let day_big: Vec<usize> = (1..newest_day).filter(|&i| rings[i].1).collect();
    let day_small: Vec<usize> = (1..newest_day).filter(|&i| !rings[i].1).collect();
    let day_big_total = spec.newest_day.big_bytes - spec.giant.tree_bytes;
    for (i, b) in day_big.iter().zip(shares(
        day_big_total,
        &log_weights(&mut rng, day_big.len(), 8.0),
    )) {
        tree_bytes[*i] = b;
        min_children[*i] = 1;
    }
    for (i, b) in day_small.iter().zip(shares(
        spec.newest_day.small_bytes,
        &log_weights(&mut rng, day_small.len(), 8.0),
    )) {
        tree_bytes[*i] = b;
        // Root and children all at most `small_cap`.
        min_children[*i] = (b.div_ceil(small_cap) as u32).max(2) - 1;
    }

    // Subagent counts: the giant's fixed, `single_file_trees` older trees
    // with none, the rest heavy-tailed (the newest day weighted up).
    let tail: Vec<usize> = (newest_day..n).collect();
    let mut zero = vec![false; n];
    for &i in sample(&mut rng, &tail, spec.single_file_trees as usize).iter() {
        zero[i] = true;
    }
    let eligible: Vec<usize> = (1..n).filter(|&i| !zero[i]).collect();
    let weights: Vec<f64> = eligible
        .iter()
        .map(|&i| pareto(&mut rng, 0.9, 0.0008) * if i < newest_day { 4.0 } else { 1.0 })
        .collect();
    let mins: Vec<u64> = eligible
        .iter()
        .map(|&i| u64::from(min_children[i].max(1)))
        .collect();
    let pool = u64::from(spec.subagents - spec.giant.subagents);
    // A newest-day tree's bytes are fixed: it holds no more files than fit.
    let caps: Vec<u64> = eligible
        .iter()
        .map(|&i| {
            let most = u64::from(spec.max_subagents_per_tree);
            if i < newest_day {
                most.min(tree_bytes[i] / (spec.min_transcript_bytes + 512) - 1)
            } else {
                most
            }
        })
        .collect();
    let mut child_count = vec![0u64; n];
    child_count[0] = u64::from(spec.giant.subagents);
    for (k, c) in apportion(pool, &weights, &mins, &caps)
        .into_iter()
        .enumerate()
    {
        child_count[eligible[k]] = c;
    }

    // Sidecars beside transcripts: every giant child, then an exact sample.
    let others: Vec<(usize, u64)> = (1..n)
        .flat_map(|i| (0..child_count[i]).map(move |c| (i, c)))
        .collect();
    let want = (spec.sidecars - spec.giant.subagents) as usize;
    let mut with_sidecar = std::collections::HashSet::new();
    for k in sample(&mut rng, &(0..others.len()).collect::<Vec<_>>(), want) {
        with_sidecar.insert(others[k]);
    }

    // Sidecar-only subagents: older trees that have subagents.
    let hosts: Vec<usize> = tail
        .iter()
        .copied()
        .filter(|&i| child_count[i] > 0)
        .collect();
    let mut sidecar_only_at = vec![0u32; n];
    for _ in 0..spec.sidecar_only {
        sidecar_only_at[*rng.pick(&hosts)] += 1;
    }

    // Tail bytes (scaled).
    let tail_big: Vec<usize> = tail.iter().copied().filter(|&i| rings[i].1).collect();
    let tail_small: Vec<usize> = tail.iter().copied().filter(|&i| !rings[i].1).collect();
    let big_child = shares(
        (spec.tail_big_child_real_bytes as f64 * spec.tail_scale) as u64,
        &pareto_weights(&mut rng, tail_big.len()),
    );
    let small_tree = shares(
        (spec.tail_small_real_bytes as f64 * spec.tail_scale) as u64,
        &pareto_weights(&mut rng, tail_small.len()),
    );

    // Projects: the newest day in a few, NEWEST's the most; every project
    // gets an older tree, the rest Zipf-ish.
    let mut project = vec![0u64; n];
    for p in project.iter_mut().take(newest_day).skip(1) {
        *p = if rng.chance(0.45) {
            0
        } else {
            1 + rng.below(4)
        };
    }
    let order = sample(&mut rng, &tail, tail.len());
    let zipf: Vec<f64> = (0..spec.projects).map(|k| 1.0 / f64::from(k + 1)).collect();
    for (k, &i) in order.iter().enumerate() {
        project[i] = if k < spec.projects as usize {
            k as u64
        } else {
            pick_weighted(&mut rng, &zipf) as u64
        };
    }

    let min = spec.min_transcript_bytes;
    let mut agent_ids = std::collections::HashSet::new();
    let mut trees = Vec::with_capacity(n);
    for i in 0..n {
        let (age_ms, big) = rings[i];
        let ring_index = ring_of(spec, i);
        let root_mtime_ms = spec.newest_ms - age_ms;
        let root_id = rng.uuid(4);
        let count = child_count[i] as usize;
        let mut sidecars: Vec<Option<String>> = (0..count)
            .map(|c| {
                (i == 0 || with_sidecar.contains(&(i, c as u64)))
                    .then(|| sidecar_document(&mut rng, c))
            })
            .collect();
        let sidecar_bytes: u64 = sidecars.iter().flatten().map(|s| s.len() as u64).sum();
        let (root_bytes, child_bytes): (u64, Vec<u64>) = if i == 0 {
            let rest = spec.giant.tree_bytes - spec.giant.root_bytes - sidecar_bytes;
            let w = pareto_weights(&mut rng, count);
            (
                spec.giant.root_bytes,
                apportion(
                    rest,
                    &w,
                    &vec![min; count],
                    &vec![spec.giant.root_bytes - 1; count],
                ),
            )
        } else if i < newest_day && big {
            let t = tree_bytes[i] - sidecar_bytes;
            let lo = line + spec.tail_big_root_extra.0;
            // The root is the largest file: at least an even share.
            let share =
                ((t as f64 * (0.3 + 0.6 * rng.unit())) as u64).max(t.div_ceil(count as u64 + 1));
            let root = share.clamp(lo, t - min * count as u64);
            let w = pareto_weights(&mut rng, count);
            (
                root,
                apportion(t - root, &w, &vec![min; count], &vec![root; count]),
            )
        } else if i < newest_day {
            let t = tree_bytes[i] - sidecar_bytes;
            let share = ((t as f64 * (0.2 + 0.4 * rng.unit())) as u64)
                .max(t.saturating_sub(small_cap * count as u64));
            let root = share.clamp(min, small_cap.min(t - min * count as u64));
            let w = pareto_weights(&mut rng, count);
            (
                root,
                apportion(t - root, &w, &vec![min; count], &vec![small_cap; count]),
            )
        } else if big {
            let (lo, hi) = spec.tail_big_root_extra;
            let root = line + rng.range(lo, hi);
            let k = tail_big.iter().position(|&j| j == i).expect("tail big");
            let w = pareto_weights(&mut rng, count);
            // A big tree that is its root alone has no other files to share.
            let rest = if count == 0 {
                0
            } else {
                big_child[k].clamp(min * count as u64, small_cap * count as u64)
            };
            (
                root,
                apportion(rest, &w, &vec![min; count], &vec![small_cap; count]),
            )
        } else {
            let k = tail_small.iter().position(|&j| j == i).expect("tail small");
            let t = small_tree[k].clamp(min * (count as u64 + 1), small_cap * (count as u64 + 1));
            let share = (t as f64 * (0.2 + 0.6 * rng.unit())) as u64;
            let root = if count == 0 {
                t.min(small_cap)
            } else {
                share.clamp(min, (t - min * count as u64).min(small_cap))
            };
            let w = pareto_weights(&mut rng, count);
            (
                root,
                apportion(t - root, &w, &vec![min; count], &vec![small_cap; count]),
            )
        };
        // Older children: past 24 h of the root for the newest day (so the
        // day's file bucket holds its roots alone), else 1 h – 7 d before it.
        let lo_h = if i < newest_day { 25.0 } else { 1.0 };
        let children = child_bytes
            .into_iter()
            .enumerate()
            .map(|(c, bytes)| ChildPlan {
                agent_id: unique_agent_id(&mut rng, &mut agent_ids),
                bytes,
                mtime_ms: root_mtime_ms - hours_ms(&mut rng, lo_h, 168.0),
                seed: rng.next_u64(),
                sidecar: sidecars[c].take(),
            })
            .collect();
        let sidecar_only = (0..sidecar_only_at[i])
            .map(|c| SidecarOnlyPlan {
                agent_id: unique_agent_id(&mut rng, &mut agent_ids),
                mtime_ms: root_mtime_ms - hours_ms(&mut rng, 1.0, 168.0),
                document: sidecar_document(&mut rng, c as usize),
            })
            .collect();
        trees.push(TreePlan {
            root_id,
            project: project[i],
            ring: ring_index,
            big,
            root_bytes,
            root_mtime_ms,
            root_seed: rng.next_u64(),
            children,
            sidecar_only,
        });
    }
    ShapePlan {
        spec: spec.clone(),
        seed,
        trees,
    }
}

/// (age before NEWEST in ms, big) per tree, newest first; NEWEST at 0.
fn ring_layout(spec: &ShapeSpec, rng: &mut Rng) -> Vec<(i64, bool)> {
    let day = &spec.newest_day;
    let mut day_trees: Vec<(i64, bool)> = Vec::new();
    let within: Vec<i64> = (1..day.within_1h)
        .map(|_| 60_000 + rng.below(54 * 60_000) as i64)
        .collect();
    let big_within = sample(
        rng,
        &(0..within.len()).collect::<Vec<_>>(),
        (day.big - 1) as usize,
    );
    for (k, age) in within.iter().enumerate() {
        day_trees.push((*age, big_within.contains(&k)));
    }
    for _ in day.within_1h..day.trees {
        day_trees.push((hours_ms(rng, 9.0, 11.0), false));
    }
    day_trees.sort_by_key(|t| t.0);
    let mut out = vec![(0, true)];
    out.extend(day_trees);
    for ring in &spec.rings {
        let ages: Vec<i64> = (0..ring.trees)
            .map(|_| {
                hours_ms(rng, ring.min_age_h, ring.max_age_h)
                    .max((ring.min_age_h * HOUR_MS as f64) as i64 + 1)
            })
            .collect();
        let big = sample(rng, &(0..ages.len()).collect::<Vec<_>>(), ring.big as usize);
        let mut trees: Vec<(i64, bool)> = ages
            .iter()
            .enumerate()
            .map(|(k, a)| (*a, big.contains(&k)))
            .collect();
        trees.sort_by_key(|t| t.0);
        out.extend(trees);
    }
    assert_eq!(
        out.len(),
        spec.roots as usize,
        "shape spec: rings do not sum to roots"
    );
    out
}

fn ring_of(spec: &ShapeSpec, index: usize) -> usize {
    let mut edge = spec.newest_day.trees as usize;
    if index < edge {
        return 0;
    }
    for (k, ring) in spec.rings.iter().enumerate() {
        edge += ring.trees as usize;
        if index < edge {
            return k + 1;
        }
    }
    unreachable!("tree index past the rings")
}

fn hours_ms(rng: &mut Rng, lo: f64, hi: f64) -> i64 {
    ((lo + (hi - lo) * rng.unit()) * HOUR_MS as f64) as i64
}

fn unique_agent_id(rng: &mut Rng, seen: &mut std::collections::HashSet<String>) -> String {
    loop {
        let id = format!("a{}", rng.hex(16));
        if seen.insert(id.clone()) {
            return id;
        }
    }
}

const AGENT_TYPES: &[&str] = &["Explore", "general-purpose", "Plan", "code-reviewer"];

/// A subagent sidecar: the two fields every observed sidecar carries.
fn sidecar_document(rng: &mut Rng, index: usize) -> String {
    serde_json::json!({
        "agentType": rng.pick(AGENT_TYPES),
        "description": synth::text(rng, "delegated task", index as u64, 3, 10),
    })
    .to_string()
}

/// A Pareto(alpha) draw with quantile floor `floor`: >= 1, heavy-tailed.
fn pareto(rng: &mut Rng, alpha: f64, floor: f64) -> f64 {
    (floor + rng.unit() * (1.0 - floor)).powf(-1.0 / alpha)
}

fn pareto_weights(rng: &mut Rng, n: usize) -> Vec<f64> {
    (0..n).map(|_| pareto(rng, 1.16, 0.002)).collect()
}

/// Log-uniform weights in [1, ratio).
fn log_weights(rng: &mut Rng, n: usize, ratio: f64) -> Vec<f64> {
    (0..n).map(|_| ratio.powf(rng.unit())).collect()
}

/// `total` split by `weights`, summing exactly.
fn shares(total: u64, weights: &[f64]) -> Vec<u64> {
    apportion(
        total,
        weights,
        &vec![0; weights.len()],
        &vec![u64::MAX; weights.len()],
    )
}

/// `total` split by `weights` with per-item floors and caps, summing exactly.
/// Panics when the floors exceed the total or the caps cannot reach it.
pub(crate) fn apportion(total: u64, weights: &[f64], mins: &[u64], caps: &[u64]) -> Vec<u64> {
    let n = weights.len();
    if n == 0 {
        assert_eq!(total, 0, "apportion: {total} over no items");
        return Vec::new();
    }
    let floor: u64 = mins.iter().sum();
    assert!(floor <= total, "apportion: floors {floor} exceed {total}");
    let mut out = mins.to_vec();
    let mut left = total - floor;
    // Proportional passes, re-spreading what the caps refuse.
    for _ in 0..8 {
        let open: Vec<usize> = (0..n).filter(|&i| out[i] < caps[i]).collect();
        let w: f64 = open.iter().map(|&i| weights[i]).sum();
        if left == 0 || open.is_empty() || w <= 0.0 {
            break;
        }
        let mut given = 0;
        for &i in &open {
            let add = ((weights[i] / w) * left as f64) as u64;
            let add = add.min(caps[i] - out[i]);
            out[i] += add;
            given += add;
        }
        left -= given;
        if given == 0 {
            break;
        }
    }
    // The rounding remainder, heaviest first, one unit (or the gap) at a time.
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| weights[b].total_cmp(&weights[a]).then(a.cmp(&b)));
    while left > 0 {
        let mut moved = false;
        for &i in &order {
            if left == 0 {
                break;
            }
            let room = caps[i] - out[i];
            if room > 0 {
                let add = room.min(left.div_ceil(n as u64));
                out[i] += add;
                left -= add;
                moved = true;
            }
        }
        assert!(moved, "apportion: caps cannot hold {total}");
    }
    out
}

/// `k` distinct items of `items`, in a deterministic shuffled order.
fn sample<T: Copy>(rng: &mut Rng, items: &[T], k: usize) -> Vec<T> {
    assert!(k <= items.len(), "sample: {k} of {}", items.len());
    let mut pool = items.to_vec();
    for i in 0..k {
        let j = i + rng.below((pool.len() - i) as u64) as usize;
        pool.swap(i, j);
    }
    pool.truncate(k);
    pool
}

fn pick_weighted(rng: &mut Rng, weights: &[f64]) -> usize {
    let total: f64 = weights.iter().sum();
    let mut x = rng.unit() * total;
    for (i, w) in weights.iter().enumerate() {
        if x < *w {
            return i;
        }
        x -= w;
    }
    weights.len() - 1
}

#[cfg(test)]
mod tests;
