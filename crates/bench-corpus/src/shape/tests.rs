use super::{ShapePlan, ShapeSpec, apportion, plan};

const HOUR: i64 = 3_600_000;

fn macbook() -> ShapePlan {
    plan(&ShapeSpec::macbook_v1(), 1)
}

#[test]
fn macbook_counts_match_the_census() {
    let p = macbook();
    let spec = &p.spec;
    assert_eq!(p.trees.len(), 1_139);
    let subagents: usize = p.trees.iter().map(|t| t.children.len()).sum();
    let sidecars = p
        .trees
        .iter()
        .flat_map(|t| &t.children)
        .filter(|c| c.sidecar.is_some())
        .count();
    let sidecar_only: usize = p.trees.iter().map(|t| t.sidecar_only.len()).sum();
    assert_eq!(subagents, 6_923);
    assert_eq!(p.trees.len() + subagents, 8_062, "roster sessions");
    assert_eq!(sidecars, 5_245);
    assert_eq!(sidecar_only, 550);
    assert_eq!(
        p.trees.len() + subagents + sidecar_only,
        8_612,
        "metadata sessions"
    );
    assert_eq!(p.claude_files(), 13_857);
    let projects: std::collections::BTreeSet<u64> = p.trees.iter().map(|t| t.project).collect();
    assert_eq!(projects.len(), 119);
    assert_eq!(p.trees.iter().filter(|t| t.files() == 1).count(), 351);
    assert!(
        p.trees
            .iter()
            .all(|t| t.children.len() <= spec.max_subagents_per_tree as usize)
    );
}

#[test]
fn big_means_the_largest_own_source_is_over_the_line() {
    let p = macbook();
    let line = p.spec.big_line_bytes;
    for t in &p.trees {
        assert_eq!(t.big, t.max_file() > line, "tree flag vs largest source");
    }
    assert_eq!(p.trees.iter().filter(|t| t.big).count(), 40);
}

#[test]
fn newest_is_the_exact_giant() {
    let p = macbook();
    let giant = &p.trees[0];
    assert_eq!(giant.bytes(), 668_350_304);
    assert_eq!(giant.files(), 459);
    assert_eq!(giant.root_bytes, 210_808_432);
    assert_eq!(
        giant.max_file(),
        giant.root_bytes,
        "the root transcript is the largest file"
    );
    assert!(giant.sidecar_only.is_empty());
    assert!(giant.children.iter().all(|c| c.sidecar.is_some()));
    assert_eq!(giant.root_mtime_ms, p.spec.newest_ms);
    // NEWEST is strictly the newest root, and the newest file of the home.
    assert!(
        p.trees[1..]
            .iter()
            .all(|t| t.root_mtime_ms < giant.root_mtime_ms)
    );
    let newest_child = p
        .trees
        .iter()
        .flat_map(|t| &t.children)
        .map(|c| c.mtime_ms)
        .max()
        .unwrap();
    assert!(newest_child < giant.root_mtime_ms);
}

#[test]
fn the_rings_hold_their_trees() {
    let p = macbook();
    let age = |t: &super::TreePlan| (p.spec.newest_ms - t.root_mtime_ms) as f64 / HOUR as f64;
    let count = |lo: f64, hi: f64| {
        let trees: Vec<_> = p
            .trees
            .iter()
            .filter(|t| age(t) > lo && age(t) <= hi)
            .collect();
        (
            trees.len() + usize::from(lo < 0.0),
            trees.iter().filter(|t| t.big).count(),
        )
    };
    // The giant sits at age 0; (-1, 1] counts it.
    assert_eq!(p.trees.iter().filter(|t| age(t) <= 1.0).count(), 23);
    assert_eq!(p.trees.iter().filter(|t| age(t) <= 24.0).count(), 24);
    assert_eq!(
        p.trees.iter().filter(|t| age(t) <= 24.0 && t.big).count(),
        13
    );
    assert_eq!(count(24.0, 168.0), (15, 9));
    assert_eq!(count(168.0, 720.0), (14, 3));
    assert_eq!(count(720.0, 2_900.0), (1_086, 15));
    // The newest day keeps its bytes; no small tree there holds a big file.
    let day: Vec<_> = p.trees.iter().filter(|t| t.ring == 0).collect();
    let big: u64 = day.iter().filter(|t| t.big).map(|t| t.bytes()).sum();
    let small: u64 = day.iter().filter(|t| !t.big).map(|t| t.bytes()).sum();
    assert_eq!(big, 4_480_000_000);
    assert_eq!(small, 1_200_000_000);
    // Children of a newest-day tree are past 24 h, so the day's files are its roots.
    for t in &day {
        assert!(
            t.children
                .iter()
                .all(|c| p.spec.newest_ms - c.mtime_ms > 24 * HOUR)
        );
    }
    // NEWEST's project holds several of the day's roots (H3's painted leg).
    assert!(day.iter().filter(|t| t.project == day[0].project).count() >= 4);
}

#[test]
fn the_macbook_shape_fits_the_disk_budget() {
    let p = macbook();
    let bytes = p.claude_bytes();
    assert!(bytes <= 8_000_000_000, "{bytes} bytes");
    assert!(
        bytes >= 7_000_000_000,
        "{bytes} bytes: the tail shrank too far"
    );
    assert!(p.trees.iter().all(|t| {
        t.root_bytes >= p.spec.min_transcript_bytes
            && t.children
                .iter()
                .all(|c| c.bytes >= p.spec.min_transcript_bytes)
    }));
}

#[test]
fn ids_are_unique_and_shaped() {
    let p = macbook();
    let mut roots = std::collections::HashSet::new();
    let mut agents = std::collections::HashSet::new();
    for t in &p.trees {
        assert_eq!(t.root_id.len(), 36);
        assert!(
            t.root_id
                .chars()
                .all(|c| c == '-' || c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
        assert!(roots.insert(t.root_id.clone()));
        for id in t
            .children
            .iter()
            .map(|c| &c.agent_id)
            .chain(t.sidecar_only.iter().map(|s| &s.agent_id))
        {
            assert!(agents.insert(id.clone()));
        }
    }
}

#[test]
fn the_plan_is_seed_stable() {
    let spec = ShapeSpec::macbook_v1();
    assert_eq!(plan(&spec, 7), plan(&spec, 7));
    let (a, b) = (plan(&spec, 7), plan(&spec, 8));
    assert_ne!(a.trees, b.trees);
    // A seed moves draws, never the counts or the giant.
    assert_eq!(a.claude_files(), b.claude_files());
    assert_eq!(a.trees[0].bytes(), b.trees[0].bytes());
}

#[test]
fn the_tiny_shape_plans() {
    let p = plan(&ShapeSpec::tiny(), 3);
    assert_eq!(p.trees.len(), 12);
    assert_eq!(p.trees[0].bytes(), 600_000);
    assert_eq!(p.trees.iter().filter(|t| t.big).count(), 5);
}

#[test]
fn apportion_sums_exactly_within_floors_and_caps() {
    let split = apportion(1_000, &[1.0, 5.0, 100.0], &[10, 10, 10], &[400, 400, 400]);
    assert_eq!(split.iter().sum::<u64>(), 1_000);
    assert!(split.iter().all(|&x| (10..=400).contains(&x)));
    assert_eq!(
        apportion(7, &[1.0, 1.0], &[0, 0], &[u64::MAX, u64::MAX])
            .iter()
            .sum::<u64>(),
        7
    );
}

#[test]
fn every_seed_plans() {
    for seed in 0..24 {
        let p = plan(&ShapeSpec::macbook_v1(), seed);
        assert_eq!(p.claude_files(), 13_857, "seed {seed}");
        assert!(p.claude_bytes() <= 8_000_000_000, "seed {seed}");
        assert_eq!(p.trees.iter().filter(|t| t.big).count(), 40, "seed {seed}");
    }
    for seed in 0..64 {
        let p = plan(&ShapeSpec::tiny(), seed);
        assert_eq!(p.trees.iter().filter(|t| t.big).count(), 5, "seed {seed}");
    }
}
