//! Walk-plan properties over seeded generated rosters: input order never
//! matters, trees stay whole and in path order, `P` comes first, every
//! session keeps its path order, the path walk is exactly path order, trees
//! go newest first with the tree id as tie-break, the lane split is
//! `read_len` at [`BIG_SOURCE_BYTES`], big trees deal in recency order, the
//! single lane is the global order, and the digest names exactly the walk.

use std::collections::{BTreeMap, HashMap, HashSet};

use rosetta_adapter_support::{FileDiscipline, FileFormat, StreamDriver};

use super::*;
use crate::closed::trees::SessionLink;

const CASES: u64 = 200;

/// xorshift, never zero.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound.max(1)
    }

    fn shuffle<T>(&mut self, items: &mut [T]) {
        for index in (1..items.len()).rev() {
            items.swap(index, self.below(index as u64 + 1) as usize);
        }
    }
}

fn transcript(path: &str, session: &str, size: u64, modified_ms: i64) -> SessionFile {
    SessionFile {
        input: None,
        ledger_only: false,
        relative_path: path.to_owned(),
        stream_id: "transcript".to_owned(),
        file_format: FileFormat::Jsonl,
        discipline: FileDiscipline::AppendOnly,
        driver: StreamDriver::JsonLines,
        session_id: session.to_owned(),
        native_session_id: session.to_owned(),
        size,
        modified_ms,
        agent_version: None,
        source_missing: false,
        tree_parent: None,
        cut_len: None,
        cut_modified_ms: None,
    }
}

fn member(physical: &str, session: &str, driver: StreamDriver, modified_ms: i64) -> SessionFile {
    let (stream, format, discipline) = match driver {
        StreamDriver::SqliteSnapshot => ("state-db", FileFormat::Sqlite, FileDiscipline::Database),
        _ => (
            "subagent-metadata",
            FileFormat::Json,
            FileDiscipline::RewriteInPlace,
        ),
    };
    SessionFile {
        input: None,
        stream_id: stream.to_owned(),
        file_format: format,
        discipline,
        driver,
        ..transcript(&format!("{physical}#{session}"), session, 4096, modified_ms)
    }
}

const BIG: u64 = BIG_SOURCE_BYTES;

/// One generated roster: its files and its metadata links.
struct Roster {
    files: Vec<SessionFile>,
    links: Vec<SessionLink>,
}

/// A roster of 1–10 families, each a root with up to three children, each
/// session with 1–3 transcripts whose sizes straddle the lane split, some
/// cut below their census size; some families read a sidecar document of
/// their own, and some share one snapshot database.
fn roster(seed: u64) -> Roster {
    let mut rng = Rng::new(seed);
    let mut files = Vec::new();
    let mut links = Vec::new();
    let families = 1 + rng.below(10);
    let shared_db = rng.below(2) == 0;
    for family in 0..families {
        let root = format!("s{family:02}-0");
        let children = rng.below(4);
        // Coarse times, so ties between trees happen.
        let base = (rng.below(5) as i64) * 1_000;
        for child in 0..=children {
            let session = format!("s{family:02}-{child}");
            if child > 0 {
                links.push((session.clone(), Some(root.clone()), Some(root.clone())));
            }
            for index in 0..1 + rng.below(3) {
                let size = match rng.below(6) {
                    0 => BIG,
                    1 => BIG + 1,
                    2 => BIG * 2,
                    _ => 1 + rng.below(1 << 20),
                };
                let path = if index == 0 {
                    format!("p{}/{session}.jsonl", rng.below(3))
                } else {
                    format!("p{}/{session}/agent-{index}.jsonl", rng.below(3))
                };
                let mut file = transcript(&path, &session, size, base + rng.below(3) as i64);
                if rng.below(3) == 0 {
                    // A skim cut below the census: the cut measures the lane.
                    let cut = if size > BIG { BIG } else { size / 2 };
                    file.cut_len = Some(cut);
                    file.cut_modified_ms = Some(base + 2_000 + rng.below(3) as i64);
                }
                files.push(file);
            }
            if rng.below(4) == 0 {
                files.push(member(
                    &format!("p0/{root}/meta.json"),
                    &session,
                    StreamDriver::DocumentSnapshot,
                    base,
                ));
            }
            if shared_db && rng.below(2) == 0 {
                files.push(member(
                    "state_5.sqlite",
                    &session,
                    StreamDriver::SqliteSnapshot,
                    base,
                ));
            }
        }
    }
    Roster { files, links }
}

fn plan_of<'a>(
    files: &'a [SessionFile],
    links: &[SessionLink],
    opts: &ColdWalkOptions,
) -> (WalkPlan<'a>, TreeMap) {
    let trees = TreeMap::build(files, links);
    (walk_plan(files, Some(&trees), opts), trees)
}

/// A plan by path: its spans, its global order and its digest.
type Shape = (Vec<(LaneId, Vec<String>)>, Vec<(LaneId, String)>, [u8; 32]);

/// What a plan is, by path: comparable across input permutations.
fn shape(plan: &WalkPlan<'_>) -> Shape {
    (
        plan.spans
            .iter()
            .map(|(lane, files)| {
                (
                    *lane,
                    files
                        .iter()
                        .map(|file| file.relative_path.clone())
                        .collect(),
                )
            })
            .collect(),
        plan.global_files()
            .map(|(lane, file)| (lane, file.relative_path.clone()))
            .collect(),
        plan.digest,
    )
}

fn paths(files: &[&SessionFile]) -> Vec<String> {
    files
        .iter()
        .map(|file| file.relative_path.clone())
        .collect()
}

fn is_shared_member(file: &SessionFile, trees: &TreeMap, files: &[SessionFile]) -> bool {
    file.driver != StreamDriver::JsonLines
        && files
            .iter()
            .filter(|other| other.physical_path() == file.physical_path())
            .map(|other| trees.tree_of_file(other))
            .collect::<HashSet<_>>()
            .len()
            > 1
}

#[test]
fn a_plan_ignores_the_order_of_its_inputs() {
    for seed in 0..CASES {
        let mut roster = roster(seed);
        let opts = ColdWalkOptions::default();
        let expected = {
            let (plan, _) = plan_of(&roster.files, &roster.links, &opts);
            shape(&plan)
        };
        let mut rng = Rng::new(seed ^ 0xA5A5);
        for _ in 0..3 {
            rng.shuffle(&mut roster.files);
            rng.shuffle(&mut roster.links);
            let (plan, _) = plan_of(&roster.files, &roster.links, &opts);
            assert_eq!(shape(&plan), expected, "seed {seed}");
        }
    }
}

#[test]
fn trees_stay_whole_newest_first_with_prefix_first_and_sessions_in_path_order() {
    for seed in 0..CASES {
        let roster = roster(seed);
        let (plan, trees) = plan_of(&roster.files, &roster.links, &ColdWalkOptions::default());
        assert_eq!(plan.order, WalkOrder::Recency, "seed {seed}");
        let global: Vec<(LaneId, &SessionFile)> = plan.global_files().collect();
        assert_eq!(
            global.len(),
            roster.files.len(),
            "seed {seed}: every source once"
        );

        // (c) P first and contiguous, holding exactly the shared members.
        let prefix = global
            .iter()
            .take_while(|(lane, _)| *lane == LaneId::Prefix)
            .count();
        assert!(
            global[prefix..]
                .iter()
                .all(|(lane, _)| *lane != LaneId::Prefix),
            "seed {seed}: P is contiguous"
        );
        for (lane, file) in &global {
            assert_eq!(
                *lane == LaneId::Prefix,
                is_shared_member(file, &trees, &roster.files),
                "seed {seed}: {} in {lane}",
                file.relative_path
            );
        }

        // (b) Each tree is one contiguous run, in path order; (f) runs go
        // newest first, then by tree id.
        let mut runs: Vec<(TreeId, Vec<&str>)> = Vec::new();
        for (_, file) in &global[prefix..] {
            let tree = trees.tree_of_file(file).unwrap();
            match runs.last_mut() {
                Some((last, run)) if *last == tree => run.push(&file.relative_path),
                _ => runs.push((tree, vec![&file.relative_path])),
            }
        }
        let distinct: HashSet<TreeId> = runs.iter().map(|(tree, _)| *tree).collect();
        assert_eq!(
            distinct.len(),
            runs.len(),
            "seed {seed}: a tree split in two"
        );
        for (tree, run) in &runs {
            let mut sorted = run.clone();
            sorted.sort_unstable();
            assert_eq!(run, &sorted, "seed {seed}: tree {tree} out of path order");
        }
        for pair in runs.windows(2) {
            let (left, right) = (trees.tree(pair[0].0), trees.tree(pair[1].0));
            assert!(
                left.k > right.k || (left.k == right.k && pair[0].0 < pair[1].0),
                "seed {seed}: tree {} (k {}) before {} (k {})",
                left.id,
                left.k,
                right.id,
                right.k
            );
        }

        // (d) Each session's sources are a subsequence of path order.
        let mut by_session: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for (lane, file) in &global {
            if *lane != LaneId::Prefix {
                by_session
                    .entry(&file.session_id)
                    .or_default()
                    .push(&file.relative_path);
            }
        }
        for (session, run) in by_session {
            let mut sorted = run.clone();
            sorted.sort_unstable();
            assert_eq!(run, sorted, "seed {seed}: session {session}");
        }

        // The lanes hold the same trees in the same order as the global walk.
        for (lane, files) in &plan.spans {
            let in_global: Vec<&str> = global
                .iter()
                .filter(|(span, _)| span == lane)
                .map(|(_, file)| file.relative_path.as_str())
                .collect();
            assert_eq!(
                files
                    .iter()
                    .map(|file| file.relative_path.as_str())
                    .collect::<Vec<_>>(),
                in_global,
                "seed {seed}: lane {lane}"
            );
        }
        // Cursor ids stay path ranks, whatever the walk.
        let ordered = path_ordered(&roster.files);
        for (rank, file) in ordered.iter().enumerate() {
            assert_eq!(plan.path_rank[file.relative_path.as_str()], rank as u32);
        }
    }
}

#[test]
fn the_path_walk_is_exactly_path_order() {
    for seed in 0..CASES {
        let roster = roster(seed);
        let expected = paths(&path_ordered(&roster.files));
        let trees = TreeMap::build(&roster.files, &roster.links);
        let check = |plan: WalkPlan<'_>, reason: &str| {
            assert_eq!(
                plan.order,
                WalkOrder::Path {
                    reason: match reason {
                        "runtime" => "runtime",
                        "override" => "override",
                        _ => "hoist",
                    }
                },
                "seed {seed}"
            );
            assert_eq!(plan.spans.len(), 1, "seed {seed}");
            assert_eq!(plan.spans[0].0, LaneId::Path);
            assert_eq!(paths(&plan.spans[0].1), expected, "seed {seed} {reason}");
            let global: Vec<String> = plan
                .global_files()
                .map(|(_, file)| file.relative_path.clone())
                .collect();
            assert_eq!(global, expected, "seed {seed} {reason}");
        };
        // Runtime admission and a fallback: no trees.
        check(
            walk_plan(&roster.files, None, &ColdWalkOptions::default()),
            "runtime",
        );
        // The kill switch, and the harness's path order.
        let killed = ColdWalkOptions {
            admission_override: true,
            ..ColdWalkOptions::default()
        };
        check(walk_plan(&roster.files, Some(&trees), &killed), "override");
        let path = ColdWalkOptions {
            walk_override: Some(WalkOverride::Path),
            ..ColdWalkOptions::default()
        };
        check(walk_plan(&roster.files, Some(&trees), &path), "override");
        // A shared group whose members cannot be hoisted.
        let unhoistable = plan(
            &roster.files,
            Some(&trees),
            &ColdWalkOptions::default(),
            &|_| false,
        );
        if roster
            .files
            .iter()
            .any(|file| is_shared_member(file, &trees, &roster.files))
        {
            check(unhoistable, "hoist");
        } else {
            assert_eq!(unhoistable.order, WalkOrder::Recency, "seed {seed}");
        }
    }
}

#[test]
fn a_shared_document_group_falls_back_to_the_path_walk() {
    let files = vec![
        transcript("a/one.jsonl", "one", 10, 1),
        transcript("a/two.jsonl", "two", 10, 2),
        member("a/shared.json", "one", StreamDriver::DocumentSnapshot, 1),
        member("a/shared.json", "two", StreamDriver::DocumentSnapshot, 1),
    ];
    let trees = TreeMap::build(&files, &[]);
    let walked = walk_plan(&files, Some(&trees), &ColdWalkOptions::default());
    assert_eq!(walked.order, WalkOrder::Path { reason: "hoist" });
    // A snapshot database the adapter reads with record queries does too.
    let files = vec![
        transcript("a/one.jsonl", "one", 10, 1),
        transcript("a/two.jsonl", "two", 10, 2),
        member("state.sqlite", "one", StreamDriver::SqliteSnapshot, 1),
        member("state.sqlite", "two", StreamDriver::SqliteSnapshot, 1),
    ];
    let trees = TreeMap::build(&files, &[]);
    assert_eq!(
        walk_plan(&files, Some(&trees), &ColdWalkOptions::default()).order,
        WalkOrder::Recency
    );
    assert_eq!(
        plan(&files, Some(&trees), &ColdWalkOptions::default(), &|_| {
            false
        })
        .order,
        WalkOrder::Path { reason: "hoist" }
    );
}

#[test]
fn the_lane_split_is_read_len_at_big_source_bytes() {
    // Census sizes far over the split, cuts on either side of it.
    let mut at = transcript("p/at.jsonl", "at", BIG * 4, 10);
    at.cut_len = Some(BIG);
    at.cut_modified_ms = Some(10);
    let mut over = transcript("p/over.jsonl", "over", BIG * 4, 20);
    over.cut_len = Some(BIG + 1);
    over.cut_modified_ms = Some(20);
    let census_at = transcript("p/census-at.jsonl", "census-at", BIG, 30);
    let census_over = transcript("p/census-over.jsonl", "census-over", BIG + 1, 40);
    let files = vec![at, over, census_at, census_over];
    let trees = TreeMap::build(&files, &[]);
    // T2's measure is the lane split's (R1 brief §8.2, T2 item 1).
    for tree in trees.trees() {
        let own = files
            .iter()
            .filter(|file| file.session_id == tree.id)
            .map(read_len)
            .max()
            .unwrap();
        assert_eq!(tree.max_source_bytes, own, "{}", tree.id);
    }
    let plan = walk_plan(&files, Some(&trees), &ColdWalkOptions::default());
    let lane_of = |session: &str| {
        plan.global_files()
            .find(|(_, file)| file.session_id == session)
            .unwrap()
            .0
    };
    assert_eq!(lane_of("at"), LaneId::Small);
    assert_eq!(lane_of("census-at"), LaneId::Small);
    assert_eq!(lane_of("over"), LaneId::Big(0));
    assert_eq!(lane_of("census-over"), LaneId::Big(0));
}

/// Every tree's lane, in global order, and its recency key.
fn tree_lanes(plan: &WalkPlan<'_>, trees: &TreeMap) -> Vec<(TreeId, LaneId)> {
    let mut seen = Vec::new();
    for (lane, file) in plan.global_files() {
        let tree = trees.tree_of_file(file).unwrap();
        if lane != LaneId::Prefix && seen.last().is_none_or(|(last, _)| *last != tree) {
            seen.push((tree, lane));
        }
    }
    seen
}

#[test]
fn big_trees_deal_in_recency_order() {
    for seed in 0..CASES {
        let roster = roster(seed);
        for lanes in [None, Some(2_u8)] {
            let opts = ColdWalkOptions {
                big_lanes: lanes,
                ..ColdWalkOptions::default()
            };
            let (plan, trees) = plan_of(&roster.files, &roster.links, &opts);
            let big: Vec<(TreeId, LaneId)> = tree_lanes(&plan, &trees)
                .into_iter()
                .filter(|(_, lane)| lane.is_big())
                .collect();
            for (index, (tree, lane)) in big.iter().enumerate() {
                assert!(
                    trees.tree(*tree).max_source_bytes > BIG,
                    "seed {seed}: small tree {tree} in {lane}"
                );
                let expected = LaneId::Big((index % usize::from(lanes.unwrap_or(1))) as u8);
                assert_eq!(
                    *lane, expected,
                    "seed {seed} lanes {lanes:?}: big tree {index}"
                );
            }
            for (tree, lane) in tree_lanes(&plan, &trees) {
                if lane == LaneId::Small {
                    assert!(trees.tree(tree).max_source_bytes <= BIG, "seed {seed}");
                }
            }
        }
    }
}

#[test]
fn the_single_lane_is_the_global_order() {
    for seed in 0..CASES {
        let roster = roster(seed);
        let (lanes, _) = plan_of(&roster.files, &roster.links, &ColdWalkOptions::default());
        let single = ColdWalkOptions {
            single_lane: true,
            ..ColdWalkOptions::default()
        };
        let (plan, _) = plan_of(&roster.files, &roster.links, &single);
        let spans: Vec<LaneId> = plan.spans.iter().map(|(lane, _)| *lane).collect();
        assert!(
            spans == [LaneId::Single] || spans == [LaneId::Prefix, LaneId::Single],
            "seed {seed}: {spans:?}"
        );
        let global: Vec<String> = plan
            .global_files()
            .filter(|(lane, _)| *lane != LaneId::Prefix)
            .map(|(_, file)| file.relative_path.clone())
            .collect();
        let single_span = &plan.spans.last().unwrap().1;
        assert_eq!(paths(single_span), global, "seed {seed}");
        // The same global order as the lanes walk.
        let lanes_global: Vec<String> = lanes
            .global_files()
            .map(|(_, file)| file.relative_path.clone())
            .collect();
        let single_global: Vec<String> = plan
            .global_files()
            .map(|(_, file)| file.relative_path.clone())
            .collect();
        assert_eq!(single_global, lanes_global, "seed {seed}");
    }
}

#[test]
fn the_digest_names_the_walk() {
    for seed in 0..CASES {
        let mut roster = roster(seed);
        let opts = ColdWalkOptions::default();
        let (plan, _) = plan_of(&roster.files, &roster.links, &opts);
        let digest = plan.digest;
        let mut rng = Rng::new(seed ^ 0x5A5A);
        for _ in 0..3 {
            rng.shuffle(&mut roster.files);
            let (plan, _) = plan_of(&roster.files, &roster.links, &opts);
            assert_eq!(plan.digest, digest, "seed {seed}");
        }
        let two = ColdWalkOptions {
            big_lanes: Some(2),
            ..ColdWalkOptions::default()
        };
        let (plan, _) = plan_of(&roster.files, &roster.links, &two);
        assert_ne!(plan.digest, digest, "seed {seed}: K is in the digest");
        let single = ColdWalkOptions {
            single_lane: true,
            ..ColdWalkOptions::default()
        };
        let (plan, _) = plan_of(&roster.files, &roster.links, &single);
        assert_ne!(
            plan.digest, digest,
            "seed {seed}: the mode is in the digest"
        );
    }
}

#[test]
fn harness_orders_keep_trees_whole_and_permute_them() {
    let roster = roster(7);
    let trees = TreeMap::build(&roster.files, &roster.links);
    let order = |walk_override| {
        let opts = ColdWalkOptions {
            walk_override,
            single_lane: true,
            ..ColdWalkOptions::default()
        };
        tree_lanes(&walk_plan(&roster.files, Some(&trees), &opts), &trees)
            .into_iter()
            .map(|(tree, _)| tree)
            .collect::<Vec<_>>()
    };
    let recency = order(None);
    assert_eq!(order(Some(WalkOverride::Recency)), recency);
    let mut reversed = order(Some(WalkOverride::Reversed));
    reversed.reverse();
    let mut ties = recency.clone();
    // Reversing the key also reverses the tie-break, so the orders mirror.
    ties.sort_by(|left, right| {
        trees
            .tree(*right)
            .k
            .cmp(&trees.tree(*left).k)
            .then(left.cmp(right))
    });
    assert_eq!(reversed, ties);
    let seeded: Vec<Vec<TreeId>> = (0..5)
        .map(|seed| order(Some(WalkOverride::Seeded(seed))))
        .collect();
    for permutation in &seeded {
        let mut sorted = permutation.clone();
        sorted.sort_unstable();
        let mut expected = recency.clone();
        expected.sort_unstable();
        assert_eq!(sorted, expected, "a seeded order is a permutation");
    }
    assert_eq!(
        order(Some(WalkOverride::Seeded(3))),
        seeded[3],
        "seeded orders repeat"
    );
}

#[test]
fn a_summary_counts_the_lanes_and_reports_structure_only() {
    let mut big = transcript("p/big.jsonl", "big", BIG + 1, 50);
    big.cut_len = Some(BIG + 1);
    big.cut_modified_ms = Some(100);
    let files = vec![
        big,
        transcript("p/small.jsonl", "small", 10, 90),
        member("state.sqlite", "big", StreamDriver::SqliteSnapshot, 1),
        member("state.sqlite", "small", StreamDriver::SqliteSnapshot, 1),
    ];
    let trees = TreeMap::build(&files, &[]);
    let plan = walk_plan(&files, Some(&trees), &ColdWalkOptions::default());
    let summary = summarize(&plan, Some(&trees));
    assert_eq!(
        (
            summary.trees,
            summary.small,
            summary.big,
            summary.lanes,
            summary.groups_hoisted
        ),
        (2, 1, 1, 2, 1)
    );
    assert_eq!(summary.big_bytes, BIG + 1);
    let lanes: Vec<LaneId> = summary.sources.iter().map(|source| source.lane).collect();
    assert_eq!(
        lanes,
        [
            LaneId::Prefix,
            LaneId::Prefix,
            LaneId::Big(0),
            LaneId::Small
        ]
    );
    let ranks: HashMap<u32, u64> = summary
        .sources
        .iter()
        .map(|source| (source.path_rank, source.age_ms))
        .collect();
    // Path ranks: p/big, p/small, state#big, state#small; ages from the
    // newest source (p/big's cut time).
    assert_eq!(ranks[&0], 0);
    assert_eq!(ranks[&1], 10);
    let line = summary.line("rt", "claude-code", false);
    assert!(
        line.starts_with(
            "walk: runtime=rt agent=claude-code order=recency reason=- trees=2 small=1 big=1 \
             lanes=2 groups_hoisted=1 big_bytes="
        ),
        "{line}"
    );
    assert!(line.ends_with(&format!("digest={}", summary.digest_hex16())));
    let path = summarize(&walk_plan(&files, None, &ColdWalkOptions::default()), None);
    assert!(path.is_path());
    assert!(
        path.line("rt", "codex", true)
            .contains("order=path reason=fallback")
    );
}
