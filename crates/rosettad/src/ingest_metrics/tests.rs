use rosetta_adapter_support::{FileDiscipline, FileFormat};
use rosetta_core::IngestOp;
use rosetta_core::rows::{RuntimeRow, SessionRole, SessionRow, StreamState};
use rosetta_pipeline::TreeId;

use super::memory::{Peak, Sample};
use super::*;

use std::sync::Arc;

const CUT: i64 = 1_800_000_000_000;
const MIB_BYTES: u64 = 1024 * 1024;

/// One transcript, its own physical file.
fn file<'a>(path: &'a str, size: u64, age_ms: i64, session: Option<&'a str>) -> CensusFile<'a> {
    CensusFile {
        physical: path,
        size,
        modified_ms: CUT - age_ms,
        session,
    }
}

/// The census of `files` with its trees grouped by `links`.
fn census<'a>(files: impl IntoIterator<Item = CensusFile<'a>>, links: &[SessionLink]) -> Census {
    let (census, per_session) = Census::of(files, CUT);
    census.with_trees(&per_session, &TreeMap::build(&[], links))
}

/// A session row's links: `(id, parent, root)`.
fn link(id: &str, parent: Option<&str>, root: &str) -> SessionLink {
    (
        id.to_owned(),
        parent.map(str::to_owned),
        Some(root.to_owned()),
    )
}

/// A roster source as meta admits it.
fn roster_file(
    driver: StreamDriver,
    path: &str,
    native: &str,
    size: u64,
    age_ms: i64,
) -> SessionFile {
    SessionFile {
        input: None,
        ledger_only: false,
        relative_path: path.to_owned(),
        stream_id: "stream".to_owned(),
        file_format: FileFormat::Jsonl,
        discipline: FileDiscipline::AppendOnly,
        driver,
        session_id: format!("session-{native}"),
        native_session_id: native.to_owned(),
        size,
        modified_ms: CUT - age_ms,
        agent_version: None,
        source_missing: false,
        tree_parent: None,
        cut_len: None,
        cut_modified_ms: None,
    }
}

#[test]
fn nearest_rank_percentiles_pick_an_observed_value() {
    let hundred = (1..=100).collect::<Vec<u64>>();
    assert_eq!(
        (
            percentile(&hundred, 50),
            percentile(&hundred, 90),
            percentile(&hundred, 99),
            percentile(&hundred, 100),
        ),
        (50, 90, 99, 100)
    );
    // Few values: high percentiles are the maximum, low ones the minimum.
    let few = [3, 7, 40];
    assert_eq!(percentile(&few, 50), 7);
    assert_eq!(percentile(&few, 99), 40);
    assert_eq!(percentile(&few, 0), 3);
    assert_eq!(percentile(&[9], 50), 9);
    assert_eq!(percentile(&[], 50), 0);
}

#[test]
fn a_distribution_sorts_its_values_and_counts_the_large_ones() {
    let values = vec![5, 64 * MIB_BYTES, 64 * MIB_BYTES + 1, 1, 200 * MIB_BYTES];
    assert_eq!(
        Distribution::of(values),
        Distribution {
            count: 5,
            p50: 64 * MIB_BYTES,
            p90: 200 * MIB_BYTES,
            p99: 200 * MIB_BYTES,
            max: 200 * MIB_BYTES,
            // Strictly over 64 MiB.
            large: 2,
        }
    );
    assert_eq!(Distribution::of(Vec::new()), Distribution::default());
}

#[test]
fn census_age_buckets_are_cumulative_and_older_is_the_rest() {
    let census = census(
        [
            // Modified after the cut (still growing): the newest bucket.
            file("a1", 1, -5_000, Some("a")),
            file("a2", 10, HOUR_MS, Some("a")),
            file("b", 100, HOUR_MS + 1, Some("b")),
            file("c", 1_000, 3 * DAY_MS, Some("c")),
            file("ledger", 10_000, 29 * DAY_MS, None),
            file("d", 100_000, 31 * DAY_MS, Some("d")),
        ],
        &[],
    );
    assert_eq!((census.files, census.bytes), (6, 111_111));
    assert_eq!(
        census.ages,
        [(2, 11), (3, 111), (4, 1_111), (5, 11_111)],
        "le1h, le24h, le7d, le30d"
    );
    assert_eq!(census.older, (1, 100_000));
    assert_eq!(
        census.largest,
        [100_000, 10_000, 1_000, 100, 10, 1],
        "descending"
    );
    // The ledger-only file belongs to no session, and none is shared.
    assert_eq!(census.sessions.count, 4);
    assert_eq!(census.sessions.max, 100_000);
    assert_eq!(census.shared, (0, 0));
}

#[test]
fn a_snapshot_shared_by_several_sessions_counts_once_and_is_charged_to_none() {
    let db = 100 * MIB_BYTES;
    let roster = [
        // One database enumerating two sessions: each virtual source carries
        // the whole file's size, and its age is the session's own update.
        roster_file(
            StreamDriver::SqliteSnapshot,
            "state.db#a",
            "a",
            db,
            3 * DAY_MS,
        ),
        roster_file(
            StreamDriver::SqliteSnapshot,
            "state.db#b",
            "b",
            db,
            HOUR_MS / 2,
        ),
        // A's own transcript.
        roster_file(StreamDriver::JsonLines, "rollout-a.jsonl", "a", 10, 0),
        // A document listing one session holds that session's bytes.
        roster_file(
            StreamDriver::DocumentSnapshot,
            "doc.json#c",
            "c",
            5,
            2 * DAY_MS,
        ),
        // A transcript whose own name ends like a member is still a file.
        roster_file(StreamDriver::JsonLines, "log#d", "d", 7, 0),
    ];
    let census = census(roster.iter().map(CensusFile::from), &[]);
    assert_eq!((census.files, census.bytes), (4, db + 10 + 5 + 7));
    assert_eq!(census.largest, [db, 10, 7, 5]);
    assert_eq!(census.shared, (1, db));
    // The database is as new as its newest source.
    assert_eq!(census.ages[0], (3, db + 10 + 7), "le1h");
    assert_eq!(census.older, (0, 0));
    // Sessions own only what they alone read: no session is 100 MiB, and
    // b, which reads only the shared database, has no own bytes.
    assert_eq!(census.sessions.count, 3);
    assert_eq!((census.sessions.max, census.sessions.large), (10, 0));
}

#[test]
fn census_groups_sessions_into_their_metadata_trees() {
    let files = [
        file("r", 10, 0, Some("root")),
        file("c1", 20, 0, Some("child")),
        file("c2", 5, 0, Some("child")),
        file("g", 1, 0, Some("grandchild")),
        file("x", 7, 0, Some("alone")),
    ];
    // A Codex child's root is its immediate parent: grouping by root would
    // make the grandchild a tree of its own.
    let links = [
        link("root", None, "root"),
        link("child", Some("root"), "root"),
        link("grandchild", Some("child"), "child"),
    ];
    let census = census(files, &links);
    assert_eq!(census.sessions.count, 4);
    assert_eq!(census.sessions.max, 25);
    // `alone` has no row and is its own tree.
    assert_eq!(census.roots.count, 2);
    assert_eq!(census.roots.max, 36);
    assert_eq!(census.roots.p50, 7);
}

/// What the producer's tree map sums per tree, for comparison.
fn tree_bytes(per_session: &[(String, u64)], trees: &TreeMap) -> Distribution {
    let mut sums = vec![0_u64; trees.trees().len()];
    let mut alone = Vec::new();
    for (session, bytes) in per_session {
        match trees.tree_of(session) {
            Some(tree) => sums[tree as usize] += bytes,
            None => alone.push(*bytes),
        }
    }
    // A tree of only shared or ledger-only sources owns no bytes.
    let owned = trees
        .trees()
        .iter()
        .zip(sums)
        .filter(|(tree, _)| {
            tree.sessions
                .iter()
                .any(|session| per_session.iter().any(|(owned, _)| owned == session))
        })
        .map(|(_, sum)| sum);
    Distribution::of(owned.chain(alone).collect())
}

#[test]
fn the_census_histogram_is_the_producers_tree_grouping() {
    let runtime = "rt";
    let session = |native: &str| format!("session-{native}");
    let mut guardian = roster_file(StreamDriver::JsonLines, "x.jsonl", "x", 3, 0);
    guardian.ledger_only = true;
    guardian.tree_parent = Some(session("g"));
    let roster = vec![
        roster_file(StreamDriver::JsonLines, "r.jsonl", "r", 10, 0),
        roster_file(StreamDriver::JsonLines, "c.jsonl", "c", 20, 0),
        roster_file(StreamDriver::JsonLines, "g.jsonl", "g", 30, 0),
        guardian,
        roster_file(StreamDriver::JsonLines, "u.jsonl", "u", 40, 0),
        // A stub: on the roster, with no row.
        roster_file(StreamDriver::JsonLines, "s.jsonl", "s", 50, 0),
    ];
    let row = |native: &str, parent: Option<&str>, root: &str| SessionRow {
        id: session(native),
        native_session_id: native.to_owned(),
        runtime_id: runtime.to_owned(),
        project_id: None,
        cwd: None,
        parent_session_id: parent.map(session),
        delegation: None,
        root_session_id: session(root),
        role: if parent.is_some() {
            SessionRole::Subagent
        } else {
            SessionRole::Main
        },
        title: None,
        summary: None,
        started_at: 1,
        updated_at: 1,
        activity: None,
        stream_state: StreamState::Pending,
    };
    let rows = [
        row("r", None, "r"),
        row("c", Some("r"), "r"),
        row("g", Some("c"), "c"),
        row("u", None, "u"),
    ];
    let mut store = Store::open_in_memory().unwrap();
    store
        .apply(&rosetta_store::Batch {
            ops: std::iter::once(IngestOp::Runtime(RuntimeRow {
                id: runtime.to_owned(),
                kind: "codex".into(),
                data_root: "/codex".into(),
                enabled: true,
                account: None,
                config: None,
                skills: None,
                versions_seen: None,
                discovered_at: 1,
                label: None,
                provenance: Default::default(),
            }))
            .chain(rows.iter().cloned().map(IngestOp::SessionMeta))
            .collect(),
            checkpoints: vec![],
            source: None,
        })
        .unwrap();

    // The producer's map: the roster and the rows its metadata batch held.
    let planned: Vec<SessionLink> = rows
        .iter()
        .map(|row| {
            (
                row.id.clone(),
                row.parent_session_id.clone(),
                Some(row.root_session_id.clone()),
            )
        })
        .collect();
    let producer = TreeMap::build(&roster, &planned);

    let mut milestones = ColdMilestones::new(Instant::now(), CUT, 1, false);
    let roster_census = milestones.metadata_public(Duration::ZERO, runtime, "codex", &roster);
    let mut links = session_links(&store, runtime).unwrap();
    links.extend(roster_census.source_links.clone());
    let from_store = TreeMap::build(&[], &links);
    // The same partition: an unmapped session (the stub, which no row
    // links) is a tree of its own, named by its own id, on either side.
    for file in &roster {
        let session = file.session_id.as_str();
        let name = |map: &TreeMap, tree: Option<TreeId>| {
            tree.map_or(session.to_owned(), |tree| map.tree(tree).id.clone())
        };
        assert_eq!(
            name(&producer, producer.tree_of_file(file)),
            name(&from_store, from_store.tree_of(session)),
            "{session}"
        );
    }
    let grouped = Census::of(roster.iter().map(CensusFile::from), CUT)
        .0
        .with_trees(&roster_census.per_session, &from_store);
    assert_eq!(
        grouped.roots,
        tree_bytes(&roster_census.per_session, &producer)
    );
    // r, c and g are one tree (the guardian owns no bytes); u and the stub
    // are their own.
    assert_eq!((grouped.roots.count, grouped.roots.max), (3, 60));
    milestones.census(roster_census, &store);
}

#[test]
fn the_largest_files_are_capped() {
    let paths = (1..=50).map(|n| n.to_string()).collect::<Vec<_>>();
    let files = paths
        .iter()
        .zip(1..=50)
        .map(|(path, size)| file(path, size, 0, Some("s")))
        .collect::<Vec<_>>();
    let census = census(files, &[]);
    assert_eq!(census.largest, (31..=50).rev().collect::<Vec<_>>());
    assert_eq!(largest(vec![3, 9, 1]), [9, 3, 1]);
}

#[test]
fn the_census_line_and_event_carry_structure_only() {
    let roots = vec![link("session-secret", None, "session-secret-root")];
    let census = census(
        [file(
            "/home/secret.jsonl",
            42,
            DAY_MS * 2,
            Some("session-secret"),
        )],
        &roots,
    );
    let line = census.line("rt", "claude-code");
    assert_eq!(
        line,
        "census: runtime=rt agent=claude-code files=1 bytes=42 le1h=0/0 le24h=0/0 le7d=1/42 \
         le30d=1/42 older=0/0 shared=0/0 sessions=1 session_p50=42 session_p90=42 \
         session_p99=42 session_max=42 sessions_over_64mib=0 roots=1 root_p50=42 \
         root_p90=42 root_p99=42 root_max=42 roots_over_64mib=0 largest=42"
    );
    let event = census.event("rt", "claude-code");
    for secret in ["session-secret", "secret.jsonl"] {
        assert!(!event.to_string().contains(secret), "{event}");
    }
    assert_eq!(event["agesCumulative"], true);
    assert_eq!(event["ages"]["le7d"], json!({"files": 1, "bytes": 42}));
    assert_eq!(event["shared"], json!({"files": 0, "bytes": 0}));
    assert_eq!(event["rootBytes"]["count"], 1);
}

#[test]
fn a_horizon_counts_files_modified_after_the_cut() {
    assert!(within(CUT, CUT + 1, DAY_MS));
    assert!(within(CUT, CUT - DAY_MS, DAY_MS));
    assert!(!within(CUT, CUT - DAY_MS - 1, DAY_MS));
    assert!(!within(CUT, i64::MIN, DAY_MS));
}

#[test]
fn the_memory_peak_keeps_the_highest_footprint_including_the_kernels() {
    let mut peak = Peak::default();
    peak.record(&Sample {
        phys_footprint: Some(300 * MIB_BYTES),
        resident: Some(500 * MIB_BYTES),
        lifetime_peak_footprint: Some(800 * MIB_BYTES),
        ..Sample::default()
    });
    peak.record(&Sample {
        phys_footprint: Some(400 * MIB_BYTES),
        resident: Some(100 * MIB_BYTES),
        swap_used: Some(2 * MIB_BYTES),
        ..Sample::default()
    });
    assert_eq!(peak.phys_footprint, Some(800 * MIB_BYTES));
    assert_eq!(peak.resident, Some(500 * MIB_BYTES));
    assert_eq!(peak.swap_used, Some(2 * MIB_BYTES));
    let base = Sample {
        swapouts: Some(10),
        pageouts: Some(4),
        ..Sample::default()
    };
    let now = Sample {
        swapouts: Some(15),
        pageouts: Some(4),
        ..Sample::default()
    };
    assert_eq!(
        peak.line(&now, &base, 1.5),
        "milestone: memory-peak phys_footprint_mb=800 resident_mb=500 swap_used_mb=2 \
         swapouts=5 pageouts=0 elapsed_s=1.50"
    );
    // What a platform cannot report reads as missing, never as zero.
    assert_eq!(
        Peak::default().line(&Sample::default(), &Sample::default(), 0.0),
        "milestone: memory-peak phys_footprint_mb=na resident_mb=na swap_used_mb=na \
         swapouts=na pageouts=na elapsed_s=0.00"
    );
}

#[test]
fn a_memory_sample_event_leaves_out_what_the_platform_lacks() {
    let sample = Sample {
        resident: Some(7),
        compressor_pages: Some(3),
        page_size: Some(16_384),
        ..Sample::default()
    };
    let event = sample.event(2.0);
    assert_eq!(event["event"], "memory-sample");
    assert_eq!(event["residentBytes"], 7);
    assert_eq!(event["compressorBytes"], 3 * 16_384);
    assert!(event.get("physFootprintBytes").is_none(), "{event}");
    assert!(
        sample
            .line(&Sample::default(), 2.0)
            .starts_with("memory: elapsed_s=2.00 phys_footprint_mb=na resident_mb=0"),
    );
}

#[test]
fn this_platform_reports_the_process_memory() {
    let sample = Sample::probe();
    if cfg!(target_os = "macos") {
        assert!(sample.phys_footprint.is_some_and(|bytes| bytes > 0));
        assert!(sample.resident.is_some_and(|bytes| bytes > 0));
        assert!(sample.swapouts.is_some());
        assert!(sample.page_size.is_some());
    }
}

/// The in-RAM canonical store's share of the footprint: SQLite's global
/// allocator, read on every platform, reaches the `memory-sample` event.
#[test]
fn a_memory_sample_carries_sqlite_memory_use_and_its_high_water_mark() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE t(x); INSERT INTO t VALUES (zeroblob(65536));")
        .unwrap();
    let sample = Sample::probe();
    let used = sample.sqlite_memory_used.expect("SQLite memory statistics");
    let highwater = sample.sqlite_memory_highwater.expect("its high-water mark");
    assert!(used > 0 && highwater >= used, "{used} {highwater}");
    let event = sample.event(1.0);
    assert_eq!(event["sqliteMemoryUsedBytes"], used);
    assert_eq!(event["sqliteMemoryHighwaterBytes"], highwater);
    drop(conn);
}

#[test]
fn the_memory_sampler_runs_only_when_wanted_and_stops_at_once() {
    let started = Instant::now();
    // A warm daemon without --json --profile samples nothing.
    assert!(MemorySampler::start(started, false, false).is_none());
    let sampler = MemorySampler::start(started, true, false).expect("a cold build samples");
    let stopping = Instant::now();
    drop(sampler);
    // Dropping wakes the thread instead of waiting out its 1 s interval.
    assert!(
        stopping.elapsed() < std::time::Duration::from_millis(900),
        "{:?}",
        stopping.elapsed()
    );
}

/// A roster transcript `age_ms` before `CUT`, of session `session-<native>`.
fn transcript(native: &str, size: u64, age_ms: i64) -> SessionFile {
    roster_file(
        StreamDriver::JsonLines,
        &format!("{native}.jsonl"),
        native,
        size,
        age_ms,
    )
}

fn session(native: &str) -> String {
    format!("session-{native}")
}

fn ids(natives: &[&str]) -> Vec<String> {
    natives.iter().map(|native| session(native)).collect()
}

/// Session `native`'s row links: its parent, which is also its root.
fn child_of(native: &str, parent: Option<&str>) -> SessionLink {
    (
        session(native),
        parent.map(session),
        Some(session(parent.unwrap_or(native))),
    )
}

/// The milestone lines said so far that start with `prefix` (a name and
/// any leading fields), without their `elapsed_s`.
fn said(milestones: &ColdMilestones, prefix: &str) -> Vec<String> {
    milestones
        .said
        .iter()
        .map(|(line, _)| line.as_str())
        .filter(|line| line.starts_with(prefix))
        .map(|line| {
            line.split_whitespace()
                .filter(|field| !field.starts_with("elapsed_s="))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect()
}

/// One root-tree runtime's metadata, then its open setup.
fn root_tree_runtime(
    milestones: &mut ColdMilestones,
    runtime: &str,
    roster: &[SessionFile],
    links: &[SessionLink],
) {
    let census = milestones.metadata_public(Duration::ZERO, runtime, "claude-code", roster);
    drop(census);
    milestones.trees_known(runtime, Some(Arc::new(TreeMap::build(roster, links))));
}

/// The `@newest` horizons measure back from the newest roster session, so a
/// frozen corpus keeps its membership however old it gets, while the census
/// horizons follow the wall clock.
#[test]
fn relative_horizons_ignore_the_wall_clock() {
    let roster = [
        transcript("new", 10, HOUR_MS),
        transcript("day", 10, HOUR_MS + DAY_MS),
        transcript("week", 10, 3 * DAY_MS),
        transcript("old", 10, 9 * DAY_MS),
    ];
    let sessions = roster
        .iter()
        .map(|file| file.session_id.clone())
        .collect::<Vec<_>>();
    let run = |cut_ms: i64| {
        let mut milestones = ColdMilestones::new(Instant::now(), cut_ms, 1, false);
        let census = milestones.metadata_public(Duration::ZERO, "rt", "codex", &roster);
        drop(census);
        milestones.trees_known("rt", None);
        let members = milestones
            .newest
            .iter()
            .map(|horizon| horizon.members().into_iter().map(str::to_owned).collect())
            .collect::<Vec<Vec<String>>>();
        milestones.readable("rt", "codex", &sessions, &roster);
        (members, milestones)
    };
    let (fresh_members, fresh) = run(CUT);
    let (aged_members, aged) = run(CUT + 30 * DAY_MS);
    assert_eq!(
        fresh_members,
        [ids(&["day", "new"]), ids(&["day", "new", "week"])],
        "24h@newest, 7d@newest"
    );
    assert_eq!(aged_members, fresh_members);
    for milestones in [&fresh, &aged] {
        assert_eq!(
            said(milestones, "milestone: recent-ready horizon=24h@newest "),
            ["milestone: recent-ready horizon=24h@newest horizon_sessions=2"]
        );
        assert_eq!(
            said(milestones, "milestone: recent-ready horizon=7d@newest "),
            ["milestone: recent-ready horizon=7d@newest horizon_sessions=3"]
        );
    }
    // From the cut: one session within 24 h, three within 7 d; thirty days
    // on, the wall-clock sets are empty.
    assert_eq!(
        said(&fresh, "milestone: recent-ready horizon=24h "),
        ["milestone: recent-ready horizon=24h horizon_sessions=1"]
    );
    assert_eq!(
        said(&fresh, "milestone: recent-ready horizon=7d "),
        ["milestone: recent-ready horizon=7d horizon_sessions=3"]
    );
    assert_eq!(
        said(&aged, "milestone: recent-ready horizon=24h "),
        ["milestone: recent-ready horizon=24h horizon_sessions=0"]
    );
    assert_eq!(
        said(&aged, "milestone: recent-ready horizon=7d "),
        ["milestone: recent-ready horizon=7d horizon_sessions=0"]
    );
    // Runtime admission has no trees: no tree milestone.
    assert!(said(&fresh, "milestone: newest-").is_empty());
    assert!(said(&fresh, "milestone: small-recent-ready").is_empty());
}

/// "Newest" is over every roster: a later roster with a newer session drops
/// what an earlier one counted, and a session readable before the horizon
/// was decided still counts as a member.
#[test]
fn a_later_roster_moves_newest_and_keeps_what_was_already_readable() {
    let older = [
        transcript("a1", 10, 2 * DAY_MS),
        transcript("a2", 10, 3 * DAY_MS),
    ];
    let newer = [transcript("b1", 10, 0), transcript("b2", 10, 20 * HOUR_MS)];
    let mut milestones = ColdMilestones::new(Instant::now(), CUT, 2, true);
    let census = milestones.metadata_public(Duration::ZERO, "a", "codex", &older);
    drop(census);
    milestones.trees_known("a", None);
    // Admitted before the second roster is known: nothing is decided yet.
    milestones.readable("a", "codex", &ids(&["a1", "a2"]), &older);
    assert!(said(&milestones, "milestone: recent-ready").is_empty());
    let census = milestones.metadata_public(Duration::ZERO, "b", "codex", &newer);
    drop(census);
    milestones.trees_known("b", None);
    // a1 is 48 h older than b1, so only b1 and b2 are 24h@newest, all four
    // 7d@newest; b's sessions are still Pending.
    assert!(said(&milestones, "milestone: recent-ready horizon=24h@newest").is_empty());
    milestones.sessions_readable("b", &ids(&["b2"]));
    assert!(said(&milestones, "milestone: recent-ready horizon=7d@newest").is_empty());
    milestones.sessions_readable("b", &ids(&["b1"]));
    assert_eq!(
        said(&milestones, "milestone: recent-ready horizon=24h@newest"),
        ["milestone: recent-ready horizon=24h@newest horizon_sessions=2"]
    );
    assert_eq!(
        said(&milestones, "milestone: recent-ready horizon=7d@newest"),
        ["milestone: recent-ready horizon=7d@newest horizon_sessions=4"]
    );
    // Each new line has its JSON event.
    let (_, event) = milestones
        .said
        .iter()
        .find(|(line, _)| line.starts_with("milestone: recent-ready horizon=7d@newest"))
        .unwrap();
    let event = event.as_ref().expect("an event under --json --profile");
    assert_eq!(event["event"], "milestone");
    assert_eq!(event["milestone"], "recent-ready");
    assert_eq!(event["horizon"], "7d@newest");
    assert_eq!(event["horizonSessions"], 4);
    assert!(event["elapsedS"].is_f64());
}

/// A tree requires exactly its members with a public roster source: a
/// placeholder root (named only by its children's links) never holds its
/// tree back, while every sourced member, not just the root, must flip.
#[test]
fn newest_trees_wait_for_every_required_session() {
    let roster = [
        // Tree p: a placeholder root with two sourced children, the newest
        // tree, and a giant.
        transcript("p-a", LARGE_BYTES + 1, 0),
        transcript("p-b", 10, 0),
        // Tree q: a sourced root and a sourced child.
        transcript("q-root", 10, HOUR_MS),
        transcript("q-child", 10, HOUR_MS),
    ];
    let links = [
        child_of("p-a", Some("p-root")),
        child_of("p-b", Some("p-root")),
        child_of("p-root", None),
        child_of("q-root", None),
        child_of("q-child", Some("q-root")),
    ];
    let mut milestones = ColdMilestones::new(Instant::now(), CUT, 1, false);
    root_tree_runtime(&mut milestones, "rt", &roster, &links);

    milestones.sessions_readable("rt", &ids(&["p-a"]));
    assert!(said(&milestones, "milestone: newest-").is_empty());
    // p's sourced members are readable; its placeholder root never is.
    milestones.sessions_readable("rt", &ids(&["p-b"]));
    assert_eq!(
        said(&milestones, "milestone: newest-giant-ready"),
        [format!(
            "milestone: newest-giant-ready tree_bytes={}",
            LARGE_BYTES + 11
        )]
    );
    // q's root alone does not make q ready.
    milestones.sessions_readable("rt", &ids(&["q-root"]));
    assert!(said(&milestones, "milestone: newest-trees-ready").is_empty());
    assert!(said(&milestones, "milestone: small-recent-ready").is_empty());
    milestones.sessions_readable("rt", &ids(&["q-child"]));
    assert_eq!(
        said(&milestones, "milestone: newest-trees-ready"),
        ["milestone: newest-trees-ready n=2"]
    );
    // The giant is not small: each small horizon holds q alone.
    assert_eq!(
        said(&milestones, "milestone: small-recent-ready"),
        [
            "milestone: small-recent-ready horizon=24h@newest trees=1",
            "milestone: small-recent-ready horizon=7d@newest trees=1",
        ]
    );
    // Runtime admission later changes nothing that was said.
    let before = milestones.said.len();
    let all = roster
        .iter()
        .map(|file| file.session_id.clone())
        .collect::<Vec<_>>();
    milestones.readable("rt", "claude-code", &all, &roster);
    assert_eq!(
        milestones.said[before..]
            .iter()
            .filter(|(line, _)| !line.starts_with("milestone: runtime-readable"))
            .count(),
        0
    );
}

/// The ten newest trees by k, then smallest id, over every root-tree
/// runtime; a tree with nothing to admit (only ledger-only sources) is not
/// ranked; and nothing is decided until every runtime's setup is known.
#[test]
fn the_newest_ten_trees_rank_by_recency_then_id_over_every_root_tree_runtime() {
    // Twelve single-session trees in two runtimes: a0..a5 at 0, 0, 1, 1, 2
    // and 2 h, b0 and b1 at 3 h, and b2..b5 all at 4 h.
    let first = (0..6)
        .map(|n| transcript(&format!("a{n}"), 10, (n / 2) * HOUR_MS))
        .collect::<Vec<_>>();
    let mut second = (0..6_i64)
        .map(|n| transcript(&format!("b{n}"), 10, (3 + n.min(2) / 2) * HOUR_MS))
        .collect::<Vec<_>>();
    // A newer ledger-only orphan: a tree with no required session.
    let mut orphan = transcript("orphan", 10, 0);
    orphan.ledger_only = true;
    second.push(orphan);
    let mut milestones = ColdMilestones::new(Instant::now(), CUT, 3, false);
    root_tree_runtime(&mut milestones, "a", &first, &[]);
    root_tree_runtime(&mut milestones, "b", &second, &[]);
    let census = milestones.metadata_public(Duration::ZERO, "c", "codex", &[]);
    drop(census);
    milestones.sessions_readable("a", &ids(&["a0", "a1", "a2", "a3", "a4", "a5"]));
    milestones.sessions_readable("b", &ids(&["b0", "b1", "b4", "b5"]));
    // The 4 h tie ranks b2 and b3 into the ten, not b4 and b5.
    milestones.trees_known("c", None);
    assert!(said(&milestones, "milestone: newest-trees-ready").is_empty());
    milestones.sessions_readable("b", &ids(&["b2"]));
    assert!(said(&milestones, "milestone: newest-trees-ready").is_empty());
    assert!(said(&milestones, "milestone: small-recent-ready").is_empty());
    milestones.sessions_readable("b", &ids(&["b3"]));
    assert_eq!(
        said(&milestones, "milestone: newest-trees-ready"),
        ["milestone: newest-trees-ready n=10"]
    );
    // The orphan is not a tree to wait for, nor counted.
    assert_eq!(
        said(&milestones, "milestone: small-recent-ready"),
        [
            "milestone: small-recent-ready horizon=24h@newest trees=12",
            "milestone: small-recent-ready horizon=7d@newest trees=12",
        ]
    );
    assert!(said(&milestones, "milestone: newest-giant-ready").is_empty());
}

/// Nothing about trees is decided while another runtime's setup could still
/// hold a newer tree.
#[test]
fn tree_milestones_wait_for_every_runtime_setup() {
    let roster = [transcript("only", 10, 0)];
    let mut milestones = ColdMilestones::new(Instant::now(), CUT, 2, false);
    root_tree_runtime(&mut milestones, "a", &roster, &[]);
    let census = milestones.metadata_public(Duration::ZERO, "b", "codex", &[]);
    drop(census);
    milestones.sessions_readable("a", &ids(&["only"]));
    // The session horizons are decided; the trees are not.
    assert_eq!(
        said(&milestones, "milestone: recent-ready horizon=24h@newest"),
        ["milestone: recent-ready horizon=24h@newest horizon_sessions=1"]
    );
    assert!(said(&milestones, "milestone: newest-trees-ready").is_empty());
    milestones.trees_known("b", None);
    assert_eq!(
        said(&milestones, "milestone: newest-trees-ready"),
        ["milestone: newest-trees-ready n=1"]
    );
}

/// Exactly 64 MiB is a small tree; one byte more is the giant. "Newest" is
/// the newest tree of any size.
#[test]
fn newest_giant_and_small_lanes_split_at_large_bytes() {
    let roster = [
        transcript("giant", LARGE_BYTES + 1, 0),
        transcript("small", LARGE_BYTES, 2 * HOUR_MS),
        // 25 h older than the giant: outside 24h@newest.
        transcript("older", 10, 25 * HOUR_MS),
    ];
    let mut milestones = ColdMilestones::new(Instant::now(), CUT, 1, true);
    root_tree_runtime(&mut milestones, "rt", &roster, &[]);
    milestones.sessions_readable("rt", &ids(&["small"]));
    assert_eq!(
        said(&milestones, "milestone: small-recent-ready"),
        ["milestone: small-recent-ready horizon=24h@newest trees=1"]
    );
    assert!(said(&milestones, "milestone: newest-giant-ready").is_empty());
    milestones.sessions_readable("rt", &ids(&["giant"]));
    assert_eq!(
        said(&milestones, "milestone: newest-giant-ready"),
        [format!(
            "milestone: newest-giant-ready tree_bytes={}",
            LARGE_BYTES + 1
        )]
    );
    milestones.sessions_readable("rt", &ids(&["older"]));
    assert_eq!(
        said(&milestones, "milestone: small-recent-ready"),
        [
            "milestone: small-recent-ready horizon=24h@newest trees=1",
            "milestone: small-recent-ready horizon=7d@newest trees=2",
        ]
    );
    assert_eq!(
        said(&milestones, "milestone: newest-trees-ready"),
        ["milestone: newest-trees-ready n=3"]
    );
    // Structure only, with JSON twins.
    let events = milestones
        .said
        .iter()
        .filter(|(line, _)| {
            line.starts_with("milestone: newest-") || line.starts_with("milestone: small-")
        })
        .map(|(_, event)| event.clone().expect("an event"))
        .collect::<Vec<_>>();
    assert_eq!(events.len(), 4);
    assert_eq!(events[0]["milestone"], "small-recent-ready");
    assert_eq!(events[0]["trees"], 1);
    assert_eq!(events[1]["milestone"], "newest-giant-ready");
    assert_eq!(events[1]["treeBytes"], LARGE_BYTES + 1);
    for event in &events {
        assert!(!event.to_string().contains("session-"), "{event}");
    }
}

/// A session no roster names, such as one born during import (R1 D12), is
/// never a member: reported readable, it joins no `@newest` horizon, makes
/// no tree ready and is counted by none, even at the runtime's admission. A
/// member reported twice (a flip, then the admission's roster) counts once.
#[test]
fn sessions_outside_every_roster_are_never_counted_or_waited_for() {
    let roster = [
        transcript("kept", 10, 0),
        transcript("kept-child", 10, 0),
        transcript("other", 10, HOUR_MS),
    ];
    let links = [child_of("kept", None), child_of("kept-child", Some("kept"))];
    let mut milestones = ColdMilestones::new(Instant::now(), CUT, 1, false);
    root_tree_runtime(&mut milestones, "rt", &roster, &links);
    milestones.sessions_readable("rt", &ids(&["born"]));
    assert!(said(&milestones, "milestone: recent-ready horizon=24h@newest").is_empty());
    assert!(said(&milestones, "milestone: newest-").is_empty());
    assert!(said(&milestones, "milestone: small-recent-ready").is_empty());
    milestones.sessions_readable("rt", &ids(&["kept", "other"]));
    milestones.sessions_readable("rt", &ids(&["kept"]));
    // kept's tree still waits for kept-child.
    assert!(said(&milestones, "milestone: newest-trees-ready").is_empty());
    assert!(said(&milestones, "milestone: small-recent-ready").is_empty());
    let all = ids(&["kept", "kept-child", "other", "born"]);
    milestones.readable("rt", "claude-code", &all, &roster);
    assert_eq!(
        said(&milestones, "milestone: recent-ready horizon=24h@newest"),
        ["milestone: recent-ready horizon=24h@newest horizon_sessions=3"]
    );
    assert_eq!(
        said(&milestones, "milestone: newest-trees-ready"),
        ["milestone: newest-trees-ready n=2"]
    );
    assert_eq!(
        said(&milestones, "milestone: small-recent-ready"),
        [
            "milestone: small-recent-ready horizon=24h@newest trees=2",
            "milestone: small-recent-ready horizon=7d@newest trees=2",
        ]
    );
}
