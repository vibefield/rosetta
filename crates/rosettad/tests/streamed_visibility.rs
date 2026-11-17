//! R1 brief §10 test 2: the per-commit visibility audit (invariants I2, I8).
//!
//! A root-tree runtime is built into fresh serve's shared-cache RAM
//! generation by the harness writer — chunked, flipping trees as they become
//! ready, on serial and pooled walks. A commit observer runs after every
//! committed writer transaction of every kind (sources, Stage and
//! resolved-row slices, flips, the remainder, metadata, finalization) and
//! reads the generation through its own connection, exactly as a reader
//! would between two commits. At every commit:
//!
//! - every session that is not Pending already equals its final self: its
//!   payload rows, its mirror activity and, for a root, its turn spans;
//! - a session once visible is never Pending again;
//! - the one exemption, usage a later displacement changes on a visible
//!   row (R1 D15, decision U-1), is followed by the writer's invalidation
//!   of that session before the next commit.
//!
//! The final selves come from the reference build of the same home.
//! (Mutation: committing the readiness flip before the fold leaves a flipped
//! session without the tool row its fold writes, at that commit.)

mod support;

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::{Arc, Mutex};

use rosetta_adapter_claude_code::ClaudeCodeAdapter;
use rosetta_adapter_codex::CodexAdapter;
use rosetta_adapter_support::AgentAdapter;
use rosetta_bench_corpus::Agent;
use rosetta_pipeline::ColdWalkOptions;
use rosetta_store::{CommitKind, lock_order, queries, turns};
use rusqlite::{Connection, OpenFlags};
use tempfile::TempDir;

use support::crossing::{CROSSING, claude_crossed};
use support::fixtures::{claude_fork, claude_nested, codex_nested, generated};
use support::harness::{cold_store, shared_store};
use support::projection_dump::{View, session_views};
use support::writer::{BuildSpec, Schedule, Split, WriterHooks, build};

const KIB: u64 = 1024;
/// Turn spans over all time, split at gaps over a minute.
const GAP_MS: i64 = 60_000;

/// One session as a reader sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Seen {
    /// Every payload row.
    whole: Vec<String>,
    /// The same rows without their usage.
    without_usage: Vec<String>,
    /// The metadata mirror's row, activity included, behind the barrier.
    mirror: String,
    /// The tree's turn spans, for a root.
    turns: String,
}

/// Every session a reader can see now, and every Pending one.
struct Look {
    visible: BTreeMap<String, Seen>,
    pending: BTreeSet<String>,
}

fn strings(conn: &Connection, sql: &str) -> BTreeSet<String> {
    conn.prepare(sql)
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

/// Read the generation as one reader transaction (R1's prologue first).
fn look(conn: &Connection) -> Look {
    conn.execute_batch("BEGIN DEFERRED").unwrap();
    lock_order::reader_prologue(conn).unwrap();
    let visible = strings(
        conn,
        "SELECT id FROM session WHERE stream_state != 'pending'",
    );
    let pending = strings(
        conn,
        "SELECT id FROM session WHERE stream_state = 'pending'",
    );
    let roots = strings(conn, "SELECT id FROM session WHERE id = root_session_id");
    let mut whole = session_views(conn, &visible, View::Whole);
    let mut without_usage = session_views(conn, &visible, View::WithoutUsage);
    let mirror: BTreeMap<String, String> = queries::all_sessions_visible(conn, true)
        .unwrap()
        .into_iter()
        .map(|row| (row.id.clone(), format!("{row:?}")))
        .collect();
    let visible = visible
        .into_iter()
        .map(|id| {
            let turns = if roots.contains(&id) {
                let spans =
                    turns::turn_spans_visible(conn, &id, i64::MIN, i64::MAX, GAP_MS, true).unwrap();
                format!("{spans:?}")
            } else {
                String::new()
            };
            let seen = Seen {
                whole: whole.remove(&id).unwrap_or_default(),
                without_usage: without_usage.remove(&id).unwrap_or_default(),
                mirror: mirror.get(&id).cloned().unwrap_or_default(),
                turns,
            };
            (id, seen)
        })
        .collect();
    conn.execute_batch("COMMIT").unwrap();
    Look { visible, pending }
}

/// What the audit log holds, in commit order.
#[derive(Debug)]
enum Event {
    Commit {
        index: u64,
        kind: CommitKind,
        /// Each visible session's whole rows, digested.
        whole: BTreeMap<String, String>,
        pending: BTreeSet<String>,
    },
    /// The writer invalidated these sessions (D15).
    Invalidate(Vec<String>),
}

type Log = Arc<Mutex<Vec<Event>>>;

struct Audit(Log);

impl WriterHooks for Audit {
    fn invalidated(&mut self, sessions: &[String]) {
        if !sessions.is_empty() {
            self.0
                .lock()
                .unwrap()
                .push(Event::Invalidate(sessions.to_vec()));
        }
    }
}

fn digest(rows: &[String]) -> String {
    blake3::hash(rows.join("\n").as_bytes())
        .to_hex()
        .to_string()
}

/// Everything one audited build saw.
#[derive(Debug, Default)]
struct Audited {
    commits: BTreeMap<String, u64>,
    /// Commits at which some session was visible while another was not.
    partial: u64,
    /// Usage changes on a visible session, each followed by its
    /// invalidation.
    usage_changes: u64,
    /// Sessions whose tool rows the fold of their tree's flip wrote.
    crossed: u64,
}

/// Build `home` audited at every commit against `expected` (its reference
/// build's final selves).
fn audit(
    label: &str,
    adapter: &'static dyn AgentAdapter,
    home: &Path,
    spec: &BuildSpec,
    expected: &Arc<BTreeMap<String, Seen>>,
) -> Audited {
    let dir = TempDir::new().unwrap();
    let (mut store, uri) = shared_store(dir.path(), "visibility");
    let reader = Connection::open_with_flags(
        &uri,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_URI,
    )
    .unwrap();
    let log: Log = Arc::default();
    let mismatches: Arc<Mutex<Vec<String>>> = Arc::default();
    {
        let (log, mismatches, expected) = (log.clone(), mismatches.clone(), expected.clone());
        let mut index = 0_u64;
        store.set_commit_observer(Some(Box::new(move |kind| {
            index += 1;
            let now = look(&reader);
            let mut whole = BTreeMap::new();
            for (id, seen) in &now.visible {
                let Some(last) = expected.get(id) else {
                    mismatches
                        .lock()
                        .unwrap()
                        .push(format!("commit {index} ({kind:?}): {id} has no final self"));
                    continue;
                };
                for (part, now, then) in [
                    ("rows", &seen.without_usage, &last.without_usage),
                    (
                        "mirror",
                        &vec![seen.mirror.clone()],
                        &vec![last.mirror.clone()],
                    ),
                    (
                        "turns",
                        &vec![seen.turns.clone()],
                        &vec![last.turns.clone()],
                    ),
                ] {
                    if now != then {
                        let missing: Vec<_> = then
                            .iter()
                            .filter(|row| !now.contains(row))
                            .take(2)
                            .collect();
                        let extra: Vec<_> = now
                            .iter()
                            .filter(|row| !then.contains(row))
                            .take(2)
                            .collect();
                        mismatches.lock().unwrap().push(format!(
                            "commit {index} ({kind:?}): visible {id} differs from its final \
                             {part}\nmissing: {missing:#?}\nextra: {extra:#?}"
                        ));
                    }
                }
                whole.insert(id.clone(), digest(&seen.whole));
            }
            log.lock().unwrap().push(Event::Commit {
                index,
                kind,
                whole,
                pending: now.pending,
            });
        })));
    }
    let mut hooks = Audit(log.clone());
    let built = build(adapter, home, spec, &mut store, Some(&mut hooks));
    assert_eq!(built.result, Ok(()), "{label}");
    let mismatches = std::mem::take(&mut *mismatches.lock().unwrap());
    assert!(
        mismatches.is_empty(),
        "{label}: {} visibility breaches, first: {}",
        mismatches.len(),
        mismatches[0]
    );
    store.set_commit_observer(None);
    let finished = look(
        &Connection::open_with_flags(
            &uri,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_URI,
        )
        .unwrap(),
    );
    assert_eq!(
        &finished.visible,
        expected.as_ref(),
        "{label}: final selves"
    );
    let mut audited = check_log(label, &std::mem::take(&mut *log.lock().unwrap()), expected);
    if built.flips > 0 {
        audited.crossed = built.report.crossed_sessions;
    }
    audited
}

/// I8 and the usage exemption over the whole log.
fn check_log(label: &str, log: &[Event], expected: &BTreeMap<String, Seen>) -> Audited {
    let mut audited = Audited::default();
    let mut ever_visible: BTreeSet<&str> = BTreeSet::new();
    let commits: Vec<usize> = log
        .iter()
        .enumerate()
        .filter(|(_, event)| matches!(event, Event::Commit { .. }))
        .map(|(position, _)| position)
        .collect();
    for (nth, position) in commits.iter().enumerate() {
        let Event::Commit {
            index,
            kind,
            whole,
            pending,
        } = &log[*position]
        else {
            unreachable!()
        };
        *audited.commits.entry(format!("{kind:?}")).or_default() += 1;
        if !whole.is_empty() && !pending.is_empty() {
            audited.partial += 1;
        }
        for id in pending {
            assert!(
                !ever_visible.contains(id.as_str()),
                "{label}: commit {index} ({kind:?}) retracted {id} (I8)"
            );
        }
        ever_visible.extend(whole.keys().map(String::as_str));
        let Some(previous) = nth.checked_sub(1).map(|earlier| &log[commits[earlier]]) else {
            continue;
        };
        let Event::Commit { whole: before, .. } = previous else {
            unreachable!()
        };
        // The writer's invalidations after this commit, before the next.
        let end = commits.get(nth + 1).copied().unwrap_or(log.len());
        let invalidated: BTreeSet<&str> = log[*position + 1..end]
            .iter()
            .flat_map(|event| match event {
                Event::Invalidate(sessions) => sessions.as_slice(),
                Event::Commit { .. } => &[],
            })
            .map(String::as_str)
            .collect();
        for (id, now) in whole {
            if before.get(id).is_some_and(|then| then != now) {
                // Only usage may move under a reader (the rows without it
                // already equal their final), and the writer says so.
                assert!(
                    invalidated.contains(id.as_str()),
                    "{label}: commit {index} ({kind:?}) changed visible {id}'s usage without \
                     an invalidation before the next commit"
                );
                audited.usage_changes += 1;
            }
        }
    }
    assert_eq!(
        ever_visible.len(),
        expected.len(),
        "{label}: every session became visible"
    );
    audited
}

/// The final selves of `home`'s reference build.
fn final_selves(adapter: &'static dyn AgentAdapter, home: &Path) -> Arc<BTreeMap<String, Seen>> {
    let mut store = cold_store();
    let reference = build(adapter, home, &BuildSpec::reference(), &mut store, None);
    assert_eq!(reference.result, Ok(()));
    // Metadata-only placeholders stay Pending until publication lifts the
    // barrier; every other session is visible.
    Arc::new(look(store.conn()).visible)
}

/// The audited builds of one home: serial and pooled, the writer draining
/// each emission or seeded, chunked at `chunk`, two lanes.
fn audit_home(
    label: &str,
    adapter: &'static dyn AgentAdapter,
    home: &Path,
    chunk: u64,
    totals: &mut Audited,
) {
    let expected = final_selves(adapter, home);
    for (name, pool, schedule) in [
        ("serial drain", None, Schedule::Drain),
        ("serial seeded", None, Schedule::Seeded(3)),
        ("pooled seeded", Some(7), Schedule::Seeded(7)),
    ] {
        let spec = BuildSpec {
            walk: ColdWalkOptions::default(),
            split: Split::Median,
            pool,
            chunk,
            schedule,
            ..BuildSpec::declared()
        };
        let audited = audit(
            &format!("{label} [{name}]"),
            adapter,
            home,
            &spec,
            &expected,
        );
        for (kind, count) in audited.commits {
            *totals.commits.entry(kind).or_default() += count;
        }
        totals.partial += audited.partial;
        totals.usage_changes += audited.usage_changes;
        totals.crossed += audited.crossed;
    }
}

#[test]
fn no_commit_ever_shows_a_half_reconciled_session() {
    let mut totals = Audited::default();
    let crossed = TempDir::new().unwrap();
    claude_crossed(crossed.path());
    audit_home(
        "claude crossed",
        &CROSSING,
        crossed.path(),
        4 * KIB,
        &mut totals,
    );
    let codex = TempDir::new().unwrap();
    codex_nested(codex.path());
    audit_home(
        "codex nested",
        &CodexAdapter,
        codex.path(),
        KIB,
        &mut totals,
    );
    let claude = TempDir::new().unwrap();
    claude_nested(claude.path(), 96 * 1024);
    audit_home(
        "claude nested",
        &ClaudeCodeAdapter,
        claude.path(),
        16 * KIB,
        &mut totals,
    );
    let fork = TempDir::new().unwrap();
    claude_fork(fork.path());
    audit_home(
        "claude fork",
        &ClaudeCodeAdapter,
        fork.path(),
        KIB,
        &mut totals,
    );
    for (adapter, agent, seed) in [
        (
            &ClaudeCodeAdapter as &'static dyn AgentAdapter,
            Agent::ClaudeCode,
            41,
        ),
        (&CodexAdapter, Agent::Codex, 43),
    ] {
        let home = TempDir::new().unwrap();
        generated(home.path(), adapter, agent, seed);
        audit_home(adapter.kind(), adapter, home.path(), 64 * KIB, &mut totals);
    }
    eprintln!("{totals:?}");
    // Every kind of writer transaction was audited, trees were readable
    // while others were not, and a displacement moved a visible row.
    for kind in [
        "Apply",
        "Sources",
        "StageSlice",
        "ResolvedSlice",
        "Admission",
        "Finalize",
        "FinalizeIndexes",
    ] {
        assert!(
            totals.commits.get(kind).is_some_and(|count| *count > 0),
            "{kind}: {totals:?}"
        );
    }
    assert!(totals.partial > 0, "{totals:?}");
    assert!(totals.usage_changes > 0, "{totals:?}");
    // A flip folded the tool rows of crossed sessions.
    assert!(totals.crossed >= 2, "{totals:?}");
}
