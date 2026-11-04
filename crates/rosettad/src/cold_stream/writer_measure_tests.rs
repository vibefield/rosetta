//! The writer's measurement-only profile fields through a whole streamed
//! build of a generated fixture (never a home directory), under
//! `--json --profile`: each `tree-admission` event's commit and publication
//! times on the milestone clock and the top-level conversations it made
//! readable; the `streamed-writer` profile's five slowest apply steps and
//! flip transactions; and the resolve accounting, which now samples where
//! withheld facts are resolved (as an item begins) instead of around the
//! step, where it always read 0.

use serde_json::Value;

use super::*;
use crate::bench::captured;
use crate::test_support::fixtures::{claude_nested, claude_root};

fn number(event: &Value, field: &str) -> f64 {
    event[field]
        .as_f64()
        .unwrap_or_else(|| panic!("{field} in {event}"))
}

/// One streamed build of `claude_nested` (two root trees: `m` with nested
/// subagents, and `n`) and the stage profiles the writer thread emitted.
fn profiled_build(workers: &str) -> (Built, Vec<Value>) {
    let home = tempfile::TempDir::new().unwrap();
    claude_nested(home.path(), 64 * 1024);
    let root = claude_root(home.path());
    let request = request(&ClaudeCodeAdapter, &root, IngestOptions::default());
    let started = Instant::now();
    let (built, events) = captured::events(|| {
        build(
            vec![request],
            &["--json", "--profile", "--capture-workers", workers],
            Some(4 * 1024),
        )
    });
    assert!(built.failures.is_empty(), "{:?}", built.failures);
    built.result.as_ref().unwrap();
    assert!(started.elapsed() < Duration::from_secs(120));
    let profiles = events
        .into_iter()
        .filter(|event| event["event"] == "stage-profile")
        .collect();
    (built, profiles)
}

fn stage<'a>(profiles: &'a [Value], name: &str) -> Vec<&'a Value> {
    profiles
        .iter()
        .filter(|event| event["stage"] == name)
        .collect()
}

#[test]
fn a_tree_admission_reports_its_commit_its_publication_and_its_roots() {
    for workers in ["0", "2"] {
        let (built, profiles) = profiled_build(workers);
        let flips = stage(&profiles, "tree-admission");
        assert!(!flips.is_empty(), "workers {workers}: {profiles:?}");
        let mut roots = 0;
        let mut sessions = 0;
        let mut last = 0.0;
        for flip in &flips {
            let (committed, published) = (number(flip, "elapsedS"), number(flip, "publishedS"));
            // Commit, then the hub absorb; batches in commit order.
            assert!(committed > 0.0 && committed <= published, "{flip}");
            assert!(committed >= last, "{flips:?}");
            last = committed;
            assert!(
                number(flip, "lockHeldMs") <= number(flip, "wallMs"),
                "{flip}"
            );
            let flip_roots = flip["roots"].as_u64().unwrap();
            let flip_sessions = flip["sessions"].as_u64().unwrap();
            assert!(flip_roots <= flip_sessions, "{flip}");
            roots += flip_roots;
            sessions += flip_sessions;
        }
        // The flips made readable exactly the published top-level
        // conversations, `m` and `n`; the subagents are not roots.
        let top_level: i64 = built
            .store
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM session
                 WHERE parent_session_id IS NULL AND stream_state != 'pending'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(roots, 2, "workers {workers}: {flips:?}");
        assert_eq!(top_level, 2, "workers {workers}");
        assert!(sessions > roots, "subagents flipped too: {flips:?}");
    }
}

#[test]
fn the_writer_profile_keeps_its_slowest_steps_and_flips_with_what_they_wrote() {
    let (_built, profiles) = profiled_build("2");
    let writers = stage(&profiles, "streamed-writer");
    assert_eq!(writers.len(), 1, "{profiles:?}");
    let writer = writers[0];
    for (field, longest, calls) in [
        ("slowestApplySteps", "longestApplyTxMs", "applyCalls"),
        ("slowestFlipTxs", "longestFlipTxMs", "flipBatches"),
    ] {
        let slowest = writer[field]
            .as_array()
            .unwrap_or_else(|| panic!("{writer}"));
        let calls = writer[calls].as_u64().unwrap();
        assert_eq!(slowest.len() as u64, calls.min(5), "{field}: {writer}");
        assert_eq!(
            number(&slowest[0], "wallMs"),
            number(writer, longest),
            "{writer}"
        );
        for pair in slowest.windows(2) {
            assert!(
                number(&pair[0], "wallMs") >= number(&pair[1], "wallMs"),
                "{field}"
            );
        }
        for entry in slowest {
            assert!(number(entry, "elapsedS") > 0.0, "{entry}");
            assert!(
                number(entry, "lockHeldMs") <= number(entry, "wallMs"),
                "{entry}"
            );
            assert!(entry["ops"]["messages"].is_u64(), "{entry}");
            assert!(entry["runtime"].is_string(), "{entry}");
        }
    }
    for step in writer["slowestApplySteps"].as_array().unwrap() {
        let kind = step["kind"].as_str().unwrap();
        assert!(
            ["Stage", "ResolvedRows", "Terminal"].contains(&kind),
            "{step}"
        );
        assert!(["S", "B0", "P", "single", "path"].contains(&step["lane"].as_str().unwrap()));
        assert!(step["sources"].as_u64().unwrap() >= 1, "{step}");
        let parts: u64 = ["assetBytes", "immediateBytes", "resolvedBytes"]
            .iter()
            .map(|field| step[*field].as_u64().unwrap())
            .sum();
        assert_eq!(parts, step["approxBytes"].as_u64().unwrap(), "{step}");
    }
    for flip in writer["slowestFlipTxs"].as_array().unwrap() {
        assert_eq!(flip["kind"], "Flip");
        assert!(flip["roots"].as_u64().unwrap() <= flip["sessions"].as_u64().unwrap());
    }
}

#[test]
fn resolving_is_accounted_where_it_happens_and_counts_as_busy() {
    let (_built, profiles) = profiled_build("2");
    let writer = stage(&profiles, "streamed-writer")[0];
    let applied = stage(&profiles, "streamed-apply");
    assert_eq!(applied.len(), 1, "one runtime: {profiles:?}");
    // The writer's own total is every item's resolve, so it equals the
    // runtime's: nothing resolves outside an item's begin.
    let resolve = number(writer, "resolveMs");
    assert!(resolve > 0.0, "{writer}");
    assert_eq!(resolve, number(applied[0], "resolveMs"), "{writer}");
    // Busy is every apply step, every flip and every resolve: with one
    // runtime its apply wall is the runtime's (`streamed-apply`).
    let capture = number(writer, "captureWallMs");
    let busy_ms = number(writer, "busyShareOfCapture") * capture;
    let parts = number(applied[0], "wallMs") + number(writer, "flipWallMs") + resolve;
    assert!(
        (busy_ms - parts).abs() < 1e-6 * parts.max(1.0),
        "{busy_ms} vs {parts}: {writer}"
    );
    // Non-idle is the writer's whole wall less all its waits, over the
    // same capture window as busy, so busy never exceeds it.
    let non_idle = number(writer, "nonIdleShareOfCapture");
    let busy = number(writer, "busyShareOfCapture");
    assert!(busy > 0.0 && non_idle + 1e-9 >= busy, "{writer}");
    let awake = number(writer, "wallMs") - number(writer, "idleMs");
    assert!(
        (non_idle * capture - awake).abs() < 1e-6 * awake.max(1.0),
        "{non_idle} * {capture} vs {awake}: {writer}"
    );
    // Within capture: the window less the waits inside it, a share of it.
    let within = number(writer, "nonIdleShareWithinCapture");
    assert!(within > 0.0 && within <= 1.0, "{writer}");
    let idle_in_capture = number(writer, "captureIdleMs");
    assert!(
        idle_in_capture <= number(writer, "idleMs") + 1e-9,
        "{writer}"
    );
    assert!(capture <= number(writer, "wallMs") + 1e-9, "{writer}");
    assert!(
        ((1.0 - within) * capture - idle_in_capture).abs() < 1e-6,
        "{writer}"
    );
}
