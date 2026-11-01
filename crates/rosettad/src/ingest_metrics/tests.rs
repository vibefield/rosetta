use rosetta_adapter_support::{FileDiscipline, FileFormat};

use super::memory::{Peak, Sample};
use super::*;

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

/// The census of `files` with its trees grouped by `roots`.
fn census<'a>(
    files: impl IntoIterator<Item = CensusFile<'a>>,
    roots: &[(String, String)],
) -> Census {
    let (census, per_session) = Census::of(files, CUT);
    census.with_roots(&per_session, roots)
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
fn census_groups_sessions_into_trees_by_their_settled_root() {
    let files = [
        file("r", 10, 0, Some("root")),
        file("c1", 20, 0, Some("child")),
        file("c2", 5, 0, Some("child")),
        file("x", 7, 0, Some("alone")),
    ];
    // The store names only sessions that are not their tree's root.
    let roots = vec![("child".to_owned(), "root".to_owned())];
    let census = census(files, &roots);
    assert_eq!(census.sessions.count, 3);
    assert_eq!(census.sessions.max, 25);
    // `alone` has no settled row and is its own tree.
    assert_eq!(census.roots.count, 2);
    assert_eq!(census.roots.max, 35);
    assert_eq!(census.roots.p50, 7);
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
    let roots = vec![(
        "session-secret".to_owned(),
        "session-secret-root".to_owned(),
    )];
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
