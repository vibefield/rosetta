//! R1 brief §10 tests 1, 9 and 11: whatever the walk, the pool's schedule,
//! the chunk cut or the writer's interleaving of lanes, a streamed build is
//! the path walk's build.
//!
//! - **Permutation and interleaving (I4, I5, I7).** Every adapter's recorded
//!   fixtures, the generated corpora and the §10 fixtures are built under
//!   the path, recency and reversed walks and 20 seeded tree orders, each
//!   serial and on a chaos pool, chunked, with seeded writer turns and
//!   flips. Each build's logical projection and sourceSetDigest equal the
//!   reference (runtime admission, path walk, unchunked); a root-tree
//!   source's ops name only its own tree; a runtime adapter's ledger is the
//!   reference's entry for entry.
//! - **Rowid order (test 9).** Every `ORDER BY` over message rowids is known
//!   and stays within one tree, so walk order cannot reach a read.
//! - **Cross-lane determinism (I15, D21).** A reference from lane S into a
//!   big-lane tree refuses identically on every schedule; a forward
//!   reference within one lane publishes on every schedule, both trees
//!   admitted at the close.

mod support;

use std::collections::BTreeMap;
use std::path::Path;

use rosetta_adapter_claude_code::ClaudeCodeAdapter;
use rosetta_adapter_codex::CodexAdapter;
use rosetta_adapter_support::{AgentAdapter, ColdAdmission};
use rosetta_bench_corpus::Agent;
use rosetta_core::ids;
use rosetta_pipeline::{ColdWalkOptions, IngestOptions, WalkOverride};
use tempfile::TempDir;

use support::crossing::{CROSSING, CROSSING_SPLIT, cross_lane, same_lane};
use support::fixtures::{
    claude_fork, claude_nested, codex_nested, generated, giant_newest, restarting_home,
};
use support::harness::{adapters, cold_store, fixture_roots, home_with};
use support::projection_dump::{Dump, assert_same_dump, dump_difference};
use support::writer::{Admission, BuildSpec, Built, Schedule, Split, WriterHooks, build};

const KIB: u64 = 1024;

/// One build of the sweep.
struct Walk {
    name: String,
    spec: BuildSpec,
}

/// The sweep: path, recency, reversed and 20 seeded tree orders, each serial
/// and on `CapturePool::chaos(4, seed)`, every source over `chunk` stepped in
/// slices, the writer's turns and flips seeded.
fn walks(chunk: u64, opts: &IngestOptions) -> Vec<Walk> {
    let mut orders = vec![
        ("path".to_owned(), WalkOverride::Path, Split::Plan),
        ("recency".to_owned(), WalkOverride::Recency, Split::Plan),
        ("reversed".to_owned(), WalkOverride::Reversed, Split::Median),
    ];
    orders.extend((0..20).map(|seed| {
        let split = if seed % 2 == 0 {
            Split::Median
        } else {
            Split::Plan
        };
        (format!("seeded {seed}"), WalkOverride::Seeded(seed), split)
    }));
    let mut out = Vec::new();
    for (index, (order, walk_override, split)) in orders.into_iter().enumerate() {
        for pooled in [false, true] {
            let index = index as u64;
            out.push(Walk {
                name: format!("{order} {}", if pooled { "pooled" } else { "serial" }),
                spec: BuildSpec {
                    admission: Admission::Declared,
                    walk: ColdWalkOptions {
                        walk_override: Some(walk_override),
                        ..ColdWalkOptions::default()
                    },
                    split,
                    pool: pooled.then_some(100 + index),
                    chunk,
                    schedule: Schedule::Seeded(index * 2 + u64::from(pooled)),
                    opts: opts.clone(),
                },
            });
        }
    }
    out
}

/// What a sweep exercised, so no assertion holds vacuously.
#[derive(Debug, Default)]
struct Coverage {
    builds: u64,
    recency_builds: u64,
    flips: u64,
    slices: u64,
    beside: u64,
    entry_checked: u64,
}

/// The reference build of one corpus: runtime admission, the path walk,
/// serial, every source whole.
fn reference(
    label: &str,
    adapter: &'static dyn AgentAdapter,
    home: &Path,
    opts: &IngestOptions,
) -> Built {
    let spec = BuildSpec {
        opts: opts.clone(),
        ..BuildSpec::reference()
    };
    let built = build(adapter, home, &spec, &mut cold_store(), None);
    assert_eq!(built.result, Ok(()), "{label}: reference");
    built
}

/// One walk of `home` held to `reference`; `ignored` columns are
/// run-dependent for this corpus.
fn check_walk(
    label: &str,
    adapter: &'static dyn AgentAdapter,
    home: &Path,
    spec: &BuildSpec,
    reference: &Built,
    ignored: &[(&str, &str)],
    coverage: &mut Coverage,
) {
    let built = build(adapter, home, spec, &mut cold_store(), None);
    assert_eq!(built.result, Ok(()), "{label}");
    assert_same_dump(
        label,
        &without(&reference.dump, ignored),
        &without(&built.dump, ignored),
    );
    assert_eq!(
        built.source_set_digest, reference.source_set_digest,
        "{label}: sourceSetDigest"
    );
    assert!(built.closure.is_empty(), "{label}: {:?}", built.closure);
    coverage.builds += 1;
    coverage.flips += built.flips as u64;
    coverage.slices += built.slices;
    coverage.beside += built.beside;
    if !built.path_walk {
        coverage.recency_builds += 1;
    }
    if adapter.cold_admission() != ColdAdmission::RootTree || built.path_walk {
        // Runtime adapters (and a root-tree runtime walked by path) keep
        // HEAD's ledger byte for byte (I5): the order-sensitive source
        // digest and every non-meta entry, in order.
        assert_eq!(
            built.source_digest, reference.source_digest,
            "{label}: source digest"
        );
        assert_eq!(built.entries, reference.entries, "{label}: entries");
        assert_eq!(built.order, reference.order, "{label}: path order");
        coverage.entry_checked += 1;
    }
}

/// Every walk of one home, held to its reference.
fn sweep(
    label: &str,
    adapter: &'static dyn AgentAdapter,
    home: &Path,
    chunk: u64,
    coverage: &mut Coverage,
) {
    let opts = IngestOptions::default();
    let reference = reference(label, adapter, home, &opts);
    for walk in walks(chunk, &opts) {
        let label = format!("{label} [{}]", walk.name);
        check_walk(&label, adapter, home, &walk.spec, &reference, &[], coverage);
    }
}

/// `dump` without the `ignored` columns' values.
fn without(dump: &Dump, ignored: &[(&str, &str)]) -> Dump {
    if ignored.is_empty() {
        return dump.clone();
    }
    dump.iter()
        .map(|(table, rows)| {
            let mut rows: Vec<String> = rows
                .iter()
                .map(|row| {
                    row.split(" | ")
                        .filter(|field| {
                            !ignored.iter().any(|(ignored_table, column)| {
                                ignored_table == table && field.starts_with(&format!("{column}="))
                            })
                        })
                        .collect::<Vec<_>>()
                        .join(" | ")
                })
                .collect();
            rows.sort();
            (table.clone(), rows)
        })
        .collect()
}

#[test]
fn every_walk_of_every_recorded_fixture_equals_the_path_reference() {
    let mut coverage = Coverage::default();
    let mut scenarios = 0;
    for adapter in adapters() {
        for (scenario, data) in fixture_roots(adapter.kind()) {
            let label = format!("{}/{scenario}", adapter.kind());
            let home = home_with(adapter, &data);
            // Fixture sources are a few KiB: slice them at 512 bytes.
            sweep(&label, adapter, home.path(), 512, &mut coverage);
            scenarios += 1;
        }
    }
    eprintln!("{coverage:?}");
    assert!(scenarios >= 27, "only {scenarios} fixture scenarios");
    assert!(coverage.recency_builds > 0, "{coverage:?}");
    assert!(coverage.flips > 0, "{coverage:?}");
    assert!(coverage.slices > 0, "{coverage:?}");
    assert!(coverage.entry_checked > 0, "{coverage:?}");
}

#[test]
fn every_walk_of_the_generated_corpora_equals_the_path_reference() {
    let mut coverage = Coverage::default();
    for (adapter, agent, seed) in [
        (
            &ClaudeCodeAdapter as &'static dyn AgentAdapter,
            Agent::ClaudeCode,
            73,
        ),
        (&CodexAdapter, Agent::Codex, 19),
    ] {
        let home = TempDir::new().unwrap();
        generated(home.path(), adapter, agent, seed);
        sweep(
            adapter.kind(),
            adapter,
            home.path(),
            64 * KIB,
            &mut coverage,
        );
    }
    eprintln!("{coverage:?}");
    // Lanes interleaved on the writer: an item began beside another lane's
    // item between its slices, and trees flipped before the close.
    assert!(coverage.slices > 0, "{coverage:?}");
    assert!(coverage.beside > 0, "{coverage:?}");
    assert!(coverage.flips > 0, "{coverage:?}");
}

#[test]
fn every_walk_of_the_r1_fixtures_equals_the_path_reference() {
    let mut coverage = Coverage::default();
    let codex = TempDir::new().unwrap();
    codex_nested(codex.path());
    sweep(
        "codex nested",
        &CodexAdapter,
        codex.path(),
        4 * KIB,
        &mut coverage,
    );
    let claude = TempDir::new().unwrap();
    claude_nested(claude.path(), 96 * 1024);
    sweep(
        "claude nested",
        &ClaudeCodeAdapter,
        claude.path(),
        16 * KIB,
        &mut coverage,
    );
    let giant = TempDir::new().unwrap();
    giant_newest(giant.path(), 8, 1024 * 1024, 5);
    sweep(
        "giant newest",
        &ClaudeCodeAdapter,
        giant.path(),
        64 * KIB,
        &mut coverage,
    );
    eprintln!("{coverage:?}");
    assert!(coverage.slices > 0, "{coverage:?}");
    assert!(coverage.flips > 0, "{coverage:?}");
}

/// A two-transcript session whose transcript is rewritten mid-capture heals
/// to the same projection on every walk. Each build needs its own home (the
/// rewrite mutates its source), so checkpoints differ by inode and skimmed
/// session times by mtime.
#[test]
fn a_restarted_two_transcript_session_equals_the_reference_on_every_walk() {
    let mut coverage = Coverage::default();
    let ignored = [("ingest_cursor", "checkpoint"), ("session", "updated_at")];
    let (home, opts) = restarting_home();
    let reference = reference("restart", &ClaudeCodeAdapter, home.path(), &opts);
    for walk in walks(256, &opts) {
        let (home, opts) = restarting_home();
        let spec = BuildSpec { opts, ..walk.spec };
        let label = format!("restart [{}]", walk.name);
        check_walk(
            &label,
            &ClaudeCodeAdapter,
            home.path(),
            &spec,
            &reference,
            &ignored,
            &mut coverage,
        );
    }
    eprintln!("{coverage:?}");
    assert!(coverage.builds >= 46, "{coverage:?}");
    assert!(coverage.slices > 0, "{coverage:?}");
}

/// Records the sessions the writer invalidates after a displacement on a
/// flipped tree (D15).
#[derive(Default)]
struct Invalidations(Vec<String>);

impl WriterHooks for Invalidations {
    fn invalidated(&mut self, sessions: &[String]) {
        self.0.extend(sessions.iter().cloned());
    }
}

/// T1's order-free usage across trees: a fork copying a response (with
/// advisor and compaction iteration samples) converges on the origin's
/// ownership whether the copy's tree lands and flips first — its usage then
/// displaced under readers, and invalidated — or the origin's. (Mutation:
/// the pre-T1 retraction leaves the copy's iteration samples behind when
/// the copy lands first.)
#[test]
fn a_cross_tree_fork_copy_converges_whichever_tree_lands_first() {
    let home = TempDir::new().unwrap();
    claude_fork(home.path());
    let reference = build(
        &ClaudeCodeAdapter,
        home.path(),
        &BuildSpec::reference(),
        &mut cold_store(),
        None,
    );
    assert_eq!(reference.result, Ok(()));
    let landed = |walk_override| {
        let mut hooks = Invalidations::default();
        let spec = BuildSpec {
            walk: ColdWalkOptions {
                walk_override: Some(walk_override),
                ..ColdWalkOptions::default()
            },
            chunk: 4 * KIB,
            ..BuildSpec::declared()
        };
        let built = build(
            &ClaudeCodeAdapter,
            home.path(),
            &spec,
            &mut cold_store(),
            Some(&mut hooks),
        );
        assert_eq!(built.result, Ok(()));
        (built, hooks.0)
    };
    let (copy_first, copy_invalidated) = landed(WalkOverride::Recency);
    let (origin_first, origin_invalidated) = landed(WalkOverride::Reversed);
    assert!(
        copy_first.order[0].ends_with("fork.jsonl"),
        "{:?}",
        copy_first.order
    );
    assert!(
        origin_first.order[0].ends_with("origin.jsonl"),
        "{:?}",
        origin_first.order
    );
    // The copy's tree was readable when the origin displaced its usage.
    let runtime = runtime_id(&copy_first.dump);
    let fork = ids::session_id(&runtime, "fork");
    assert_eq!(copy_invalidated, vec![fork], "copy first");
    assert!(
        origin_invalidated.is_empty(),
        "origin first: {origin_invalidated:?}"
    );
    if let Some(difference) = dump_difference(&copy_first.dump, &origin_first.dump) {
        panic!("copy first and origin first differ: {difference}");
    }
    // The response's iteration samples exist once, on the owner.
    let samples: Vec<&String> = origin_first.dump["session_usage"]
        .iter()
        .filter(|row| row.contains(":iteration:"))
        .collect();
    assert_eq!(samples.len(), 2, "{samples:#?}");
    assert_same_dump("copy first", &reference.dump, &copy_first.dump);
    assert_same_dump("origin first", &reference.dump, &origin_first.dump);
}

fn runtime_id(dump: &Dump) -> String {
    let row = &dump["agent_runtime"][0];
    let id = row
        .split(" | ")
        .find_map(|field| field.strip_prefix("id="))
        .unwrap();
    id.trim_start_matches("Text(\"")
        .trim_end_matches("\")")
        .to_owned()
}

/// Every `ORDER BY` over message rowids in product code, with why each stays
/// within one tree: a rowid follows commit order, which a recency walk and
/// the writer's lane interleaving change, so a rowid order spanning trees
/// would let walk order reach a read. Keyed by file and clause, with counts.
const ROWID_ORDERS: [(&str, &str, usize); 7] = [
    // FTS documents: rowids order only the detached search sidecar's own
    // document stream, never a reply.
    ("crates/store/src/queries.rs", "ORDER BY m.id", 1),
    // Child runtime profiles (PROFILES, PROFILES_ADMITTED): the children of
    // one parent, which are one tree.
    ("crates/store/src/queries/workflows.rs", "ORDER BY m.id", 2),
    // One session's task and plan tools, after their ordinal.
    (
        "crates/store/src/timeline_meta.rs",
        "ORDER BY m.ordinal, m.id",
        1,
    ),
    // The renumber within one session, after path and offset.
    (
        "crates/store/src/apply.rs",
        "ORDER BY COALESCE(c.path, ''), COALESCE(m.src_offset, m.id), m.id",
        2,
    ),
    // One cursor's rows: one source of one session.
    ("crates/store/src/apply.rs", "ORDER BY id", 1),
    ("crates/store/src/apply/rows.rs", "ORDER BY id", 1),
    // The profiler's column sample of one row, never a read route.
    ("crates/rosettad/src/bench.rs", "ORDER BY id LIMIT 1", 1),
];

/// Product Rust sources: no integration test, no test module file.
fn product_sources(root: &Path) -> Vec<std::path::PathBuf> {
    support::harness::walkdir(&root.join("crates"))
        .into_iter()
        .filter(|path| {
            let name = path.file_name().unwrap().to_string_lossy();
            path.extension().is_some_and(|ext| ext == "rs")
                && !path.components().any(|part| part.as_os_str() == "tests")
                && !name.ends_with("tests.rs")
                && !name.starts_with("tests")
        })
        .collect()
}

/// The `ORDER BY` clauses of `text` over message rowids: one naming `m.id`,
/// or ordering by a bare `id` in a statement that reads `FROM message`
/// (within the five lines before it).
fn rowid_orders(text: &str) -> Vec<String> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let Some(at) = line.find("ORDER BY") else {
            continue;
        };
        let clause = line[at..].trim_end_matches(['"', ',', ';', ')', ' ']);
        let words: Vec<&str> = clause
            .trim_start_matches("ORDER BY")
            .split([',', ' ', '(', ')'])
            .filter(|word| !word.is_empty())
            .collect();
        let message_rowid = words
            .iter()
            .any(|word| *word == "m.id" || *word == "message.id");
        let bare_id = clause
            .trim_start_matches("ORDER BY")
            .split(',')
            .any(|key| key.split_whitespace().next() == Some("id"));
        let reads_messages = lines[index.saturating_sub(5)..=index]
            .iter()
            .any(|line| line.contains("FROM message ") || line.ends_with("FROM message"));
        if message_rowid || (bare_id && reads_messages) {
            out.push(clause.to_owned());
        }
    }
    out
}

#[test]
fn every_rowid_order_over_messages_is_known_and_stays_within_one_tree() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut found: BTreeMap<(String, String), usize> = BTreeMap::new();
    for path in product_sources(&root) {
        let file = path
            .strip_prefix(&root)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        for clause in rowid_orders(&std::fs::read_to_string(&path).unwrap()) {
            *found.entry((file.clone(), clause)).or_default() += 1;
        }
    }
    let known: BTreeMap<(String, String), usize> = ROWID_ORDERS
        .iter()
        .map(|(file, clause, count)| ((file.to_string(), clause.to_string()), *count))
        .collect();
    assert_eq!(
        found, known,
        "an ORDER BY over message rowids changed: list it in ROWID_ORDERS with why it stays \
         within one tree, or order by a key no walk can change"
    );
}

/// One build of a crossing fixture.
fn crossing_build(home: &Path, pool: Option<u64>) -> Built {
    let spec = BuildSpec {
        split: Split::Bytes(CROSSING_SPLIT),
        pool,
        chunk: 4 * KIB,
        schedule: Schedule::Seeded(pool.unwrap_or(0)),
        ..BuildSpec::declared()
    };
    build(&CROSSING, home, &spec, &mut cold_store(), None)
}

fn schedules() -> Vec<Option<u64>> {
    std::iter::once(None).chain((0..20).map(Some)).collect()
}

/// D21: a lane-S source naming a session of a big-lane tree refuses the
/// build on every schedule with the same error — same path, session, tree
/// and lane — and counts one crossing. (Mutation: binding a cross-lane
/// reference while its target tree is still open lets some seeds publish.)
#[test]
fn a_cross_lane_reference_refuses_identically_on_every_schedule() {
    let home = TempDir::new().unwrap();
    cross_lane(home.path());
    let mut outcomes = BTreeMap::new();
    for pool in schedules() {
        let built = crossing_build(home.path(), pool);
        assert_eq!(built.report.cross_tree_crossings, 1, "{pool:?}");
        // The harness's own closure audit (I7) sees the injected op too, so
        // its silence on every other corpus is not vacuous.
        assert_eq!(built.closure.len(), 1, "{pool:?}: {:?}", built.closure);
        assert!(
            built.closure[0].starts_with("projects/-w-small/small.jsonl writes "),
            "{:?}",
            built.closure
        );
        outcomes
            .entry(built.result)
            .or_insert_with(Vec::new)
            .push(pool);
    }
    assert_eq!(
        outcomes.len(),
        1,
        "one outcome on every schedule: {outcomes:#?}"
    );
    let error = outcomes
        .keys()
        .next()
        .unwrap()
        .as_ref()
        .expect_err("a cross-lane reference refuses");
    assert!(
        error.contains("cold source projects/-w-small/small.jsonl addresses session")
            && error.contains("in lane B0"),
        "{error}"
    );
}

/// D21: a forward reference within lane S binds both trees, and the build
/// publishes on every schedule with both admitted at the runtime's close,
/// equal to the reference.
#[test]
fn a_same_lane_forward_reference_publishes_on_every_schedule() {
    let home = TempDir::new().unwrap();
    same_lane(home.path());
    let reference = build(
        &CROSSING,
        home.path(),
        &BuildSpec::reference(),
        &mut cold_store(),
        None,
    );
    assert_eq!(reference.result, Ok(()));
    let runtime = runtime_id(&reference.dump);
    let (newer, older) = (
        ids::session_id(&runtime, "newer"),
        ids::session_id(&runtime, "older"),
    );
    for pool in schedules() {
        let built = crossing_build(home.path(), pool);
        assert_eq!(built.result, Ok(()), "{pool:?}");
        assert_eq!(built.report.cross_tree_crossings, 1, "{pool:?}");
        assert_eq!(built.report.trees_bound, 2, "{pool:?}");
        assert_eq!(built.closure.len(), 1, "{pool:?}: {:?}", built.closure);
        for bound in [&newer, &older] {
            assert!(!built.flipped_sessions.contains(bound), "{pool:?}: {bound}");
            assert!(built.remainder.contains(bound), "{pool:?}: {bound}");
        }
        assert_same_dump(&format!("{pool:?}"), &reference.dump, &built.dump);
    }
}
