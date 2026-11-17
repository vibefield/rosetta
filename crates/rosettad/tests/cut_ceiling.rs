//! Cold reads bounded at the metadata cut (R1 T3, brief v3 §9.2).
//!
//! A runtime admitted per root tree reads each JSONL transcript only up to
//! the length its metadata skim saw (`SessionFile::cut_len`): a session is
//! complete as of the metadata it was listed with, and anything appended
//! after the skim belongs to the live runner, which lands it exactly once
//! after publication. A rewrite, every runtime-admission adapter, the kill
//! switch and a fallen-back runtime read to the end, as before.
//!
//! The library harness below (`plan`, the streamed build, the projection
//! dump) is copied from `streamed_cold.rs` until it moves to
//! `tests/support/`.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use rosetta_adapter_claude_code::ClaudeCodeAdapter;
use rosetta_adapter_kimi_code::KimiCodeAdapter;
use rosetta_adapter_pi::PiAdapter;
use rosetta_adapter_support::{AgentAdapter, ColdAdmission, StreamDriver};
use rosetta_capture::{Ledger, LedgerEntry, ScriptedEvents, SegmentCodec, scan_ledger};
use rosetta_core::rows::RuntimeRow;
use rosetta_pipeline::progress::ColdProgressObserver;
use rosetta_pipeline::{
    BlockingEmit, CapturePool, ClosedSource, ColdWalkOptions, IngestOptions, Instrument,
    LiveOptions, LiveRunner, PollEvidence, PoolBudgets, SessionFile, StreamReport,
    StreamedPreparer, StreamedRuntime, discover, expand_data_root, meta_ingest_inventory_into,
    stream_closed_sources_into, stream_closed_sources_pooled, streamed_setup_for,
};
use rosetta_store::Store;
use rusqlite::types::Value;
use serde_json::json;
use tempfile::TempDir;

// ── harness ─────────────────────────────────────────────────────────────────

struct Planned {
    adapter: &'static dyn AgentAdapter,
    runtime: RuntimeRow,
    root: PathBuf,
    planner: Store,
    files: Vec<SessionFile>,
    truncated: bool,
}

fn cold_store() -> Store {
    let mut store = Store::open_in_memory_for_rebuild().unwrap();
    store.use_precomputed_bulk_ordinals().unwrap();
    store
}

/// Discover, open a cold planner, skim metadata — which fixes each
/// transcript's cut — and publish the metadata: fresh serve's steps before
/// capture.
fn plan(adapter: &'static dyn AgentAdapter, home: &Path, canonical: &mut Store) -> Planned {
    plan_with(adapter, home, canonical, &IngestOptions::default())
}

fn plan_with(
    adapter: &'static dyn AgentAdapter,
    home: &Path,
    canonical: &mut Store,
    opts: &IngestOptions,
) -> Planned {
    let instrument = Instrument::new();
    let discovery = discover(&[adapter], home, opts, &instrument)
        .unwrap()
        .pop()
        .unwrap();
    let mut runtime = discovery.runtime;
    runtime.enabled = true;
    let mut planner = Store::open_in_memory_for_cold_planning().unwrap();
    let (meta, files) = meta_ingest_inventory_into(
        adapter,
        &runtime,
        &discovery.root,
        &mut planner,
        &discovery.inventory,
        discovery.walk_truncated,
        opts,
        &instrument,
        None,
    )
    .unwrap();
    for batch in planner.planned_metadata_batches().unwrap() {
        canonical.apply(&batch).unwrap();
    }
    Planned {
        adapter,
        runtime,
        root: discovery.root,
        planner,
        files,
        truncated: discovery.walk_truncated || meta.walk_truncated,
    }
}

/// How the closed sources are captured: the serial walk, or a capture pool
/// of this many workers.
#[derive(Debug, Clone, Copy)]
enum Walk {
    Serial,
    Pooled(usize),
}

/// What a runtime's cold build leaves for its live runner.
struct Built {
    adapter: &'static dyn AgentAdapter,
    runtime: RuntimeRow,
    root: PathBuf,
    files: Vec<SessionFile>,
    evidence: HashMap<String, PollEvidence>,
    report: StreamReport,
    per_tree: bool,
}

/// The streamed build as fresh serve runs it, with the runtime asking for
/// `admission`: closed sources applied as they are emitted, then admission.
/// Publication (`finish_bulk_load`) is the caller's.
fn build(
    planned: Planned,
    canonical: &mut Store,
    admission: ColdAdmission,
    walk: Walk,
    opts: &IngestOptions,
    ledger: Option<&mut Ledger>,
) -> Built {
    let Planned {
        adapter,
        runtime,
        root,
        mut planner,
        files,
        truncated,
    } = planned;
    let instrument = Instrument::new();
    let setup = streamed_setup_for(&files, &mut planner, admission, truncated).unwrap();
    let trees = setup.trees().cloned();
    let mut writer =
        StreamedRuntime::open(adapter, &runtime, setup, canonical, &instrument).unwrap();
    let mut preparer = StreamedPreparer::new(&files);
    let mut emit = BlockingEmit(|_, mut sources: Vec<ClosedSource>| {
        preparer.prepare(&mut sources)?;
        writer.apply(canonical, sources, &instrument).map(|_| ())
    });
    let walk_opts = ColdWalkOptions::default();
    let report = match walk {
        Walk::Serial => stream_closed_sources_into(
            adapter,
            &runtime,
            &root,
            &mut planner,
            &files,
            opts,
            &instrument,
            ledger,
            trees.as_deref(),
            &walk_opts,
            &mut emit,
        ),
        Walk::Pooled(workers) => {
            let pool = CapturePool::new(workers, PoolBudgets::DEFAULT);
            pool.scoped(|| {
                stream_closed_sources_pooled(
                    &pool,
                    adapter,
                    &runtime,
                    &root,
                    &mut planner,
                    &files,
                    opts,
                    &instrument,
                    ledger,
                    trees.as_deref(),
                    &walk_opts,
                    &mut emit,
                )
            })
        }
    }
    .unwrap();
    let evidence = writer.take_poll_evidence();
    let prepared = preparer.finish().unwrap();
    writer.admit(canonical, &instrument, &prepared).unwrap();
    Built {
        adapter,
        runtime,
        root,
        files,
        evidence,
        report,
        per_tree: trees.is_some(),
    }
}

/// After publication: the live runner, seeded with the writer's evidence,
/// runs its startup catch-up, appending to `ledger` when given one (dropping
/// the runner closes it).
fn catch_up(built: Built, canonical: &mut Store, ledger: Option<Ledger>) {
    let instrument = Instrument::new();
    let mut runner = LiveRunner::resume_from_roster(
        built.adapter,
        built.runtime,
        built.root,
        Box::new(ScriptedEvents::default()),
        built.files,
        IngestOptions::default(),
        LiveOptions {
            tick: Duration::ZERO,
            poll_interval: Duration::from_secs(3600),
            max_coalesced: 8,
        },
        ledger,
    )
    .with_poll_evidence(built.evidence);
    runner.catch_up(canonical, &instrument).unwrap();
}

/// A fresh root-tree build of `home` as it is now, published and caught up.
fn fresh_build(adapter: &'static dyn AgentAdapter, home: &Path) -> Store {
    let mut store = cold_store();
    let planned = plan(adapter, home, &mut store);
    let built = build(
        planned,
        &mut store,
        adapter.cold_admission(),
        Walk::Serial,
        &IngestOptions::default(),
        None,
    );
    store.finish_bulk_load().unwrap();
    catch_up(built, &mut store, None);
    store
}

/// `(messages, stream_state)` of the session with `native` id.
fn session(store: &Store, native: &str) -> (i64, String) {
    store
        .conn()
        .query_row(
            "SELECT (SELECT COUNT(*) FROM message m WHERE m.session_id = s.id), s.stream_state
             FROM session s WHERE s.native_session_id = ?1",
            [native],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap()
}

fn message_count(store: &Store) -> i64 {
    store
        .conn()
        .query_row("SELECT COUNT(*) FROM message", [], |row| row.get(0))
        .unwrap()
}

/// Every canonical table with surrogate ids mapped to stable identities —
/// the same normalization as `streamed_cold.rs` — less the bookkeeping of
/// a write after publication, which a fresh build never makes:
/// `ingest_cursor.updated_at` (the census mtime a live pass stamps when it
/// moves a cursor; the cold writer leaves it NULL), the search repair it
/// records (`projection_state.search_revision`, `search_dirty`), and the
/// library revision (`projection_state.artifacts_revision`, schema 30), which
/// counts writer transactions and so differs between a live and a fresh
/// build of one ledger. The checkpoints, and every projection row, must be
/// equal.
fn dump(store: &Store) -> BTreeMap<String, Vec<String>> {
    let conn = store.conn();
    let cursors: BTreeMap<i64, String> = conn
        .prepare("SELECT id, runtime_id, path FROM ingest_cursor")
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                format!("{}|{}", row.get::<_, String>(1)?, row.get::<_, String>(2)?),
            ))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let messages: BTreeMap<i64, String> = conn
        .prepare("SELECT id, timeline_id FROM message")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let tables: Vec<String> = conn
        .prepare(
            "SELECT name FROM sqlite_master
             WHERE type = 'table' AND name NOT LIKE 'sqlite_%'
               AND name NOT IN ('source_audit', 'search_dirty')
             ORDER BY name",
        )
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let mut out = BTreeMap::new();
    for table in tables {
        let columns: Vec<String> = conn
            .prepare(&format!("PRAGMA table_info(\"{table}\")"))
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
            .into_iter()
            .filter(|column| {
                !((table == "message" || table == "ingest_cursor") && column == "id")
                    && !(table == "projection_state"
                        && (column == "projection_epoch"
                            || column == "search_revision"
                            || column == "artifacts_revision"))
                    && !(table == "agent_runtime" && column == "discovered_at")
                    && !(table == "ingest_cursor" && column == "updated_at")
            })
            .collect();
        let select = columns
            .iter()
            .map(|column| format!("\"{column}\""))
            .collect::<Vec<_>>()
            .join(",");
        let mut statement = conn
            .prepare(&format!("SELECT {select} FROM \"{table}\""))
            .unwrap();
        let mut rows = statement
            .query_map([], |row| {
                let mut fields = Vec::with_capacity(columns.len());
                for (index, column) in columns.iter().enumerate() {
                    let value: Value = row.get(index)?;
                    let text = match (&value, column.as_str()) {
                        (Value::Integer(id), name) if name.ends_with("cursor") => cursors
                            .get(id)
                            .cloned()
                            .unwrap_or_else(|| format!("<dangling cursor {id}>")),
                        (Value::Integer(id), "message_id") => messages
                            .get(id)
                            .cloned()
                            .unwrap_or_else(|| format!("<dangling message {id}>")),
                        _ => format!("{value:?}"),
                    };
                    fields.push(format!("{column}={text}"));
                }
                Ok(fields.join(" | "))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        rows.sort();
        out.insert(table, rows);
    }
    out
}

fn assert_same_projection(label: &str, reference: &Store, candidate: &Store) {
    let (reference, candidate) = (dump(reference), dump(candidate));
    assert_eq!(
        reference.keys().collect::<Vec<_>>(),
        candidate.keys().collect::<Vec<_>>(),
        "{label}: table sets differ"
    );
    for (table, expected) in &reference {
        let actual = &candidate[table];
        if expected != actual {
            let missing = expected
                .iter()
                .filter(|row| !actual.contains(row))
                .take(3)
                .collect::<Vec<_>>();
            let extra = actual
                .iter()
                .filter(|row| !expected.contains(row))
                .take(3)
                .collect::<Vec<_>>();
            panic!(
                "{label}: table {table} differs ({} fresh rows, {} cut rows)\n\
                 only in the fresh build: {missing:#?}\nonly after the cut: {extra:#?}",
                expected.len(),
                actual.len()
            );
        }
    }
}

// ── a Claude Code tree ──────────────────────────────────────────────────────

const PROJECT: &str = "projects/-w-cut";
/// The newest tree's main transcript, which grows after the skim.
const MAIN: &str = "projects/-w-cut/m.jsonl";

fn claude_user(session: &str, agent: Option<&str>, index: usize, text: &str) -> String {
    let mut record = json!({
        "parentUuid": null,
        "isSidechain": agent.is_some(),
        "type": "user",
        "message": {"role": "user", "content": text},
        "uuid": format!("{session}-{}-u{index}", agent.unwrap_or("main")),
        "timestamp": format!("2026-10-30T10:{:02}:00.000Z", index % 60),
        "userType": "external",
        "cwd": "/w/cut",
        "sessionId": session,
        "version": "2.1.241",
        "gitBranch": "main"
    });
    if let Some(agent) = agent {
        record["agentId"] = json!(agent);
    }
    record.to_string()
}

fn claude_assistant(session: &str, agent: Option<&str>, index: usize, text: &str) -> String {
    let tag = agent.unwrap_or("main");
    let mut record = json!({
        "parentUuid": format!("{session}-{tag}-u{index}"),
        "isSidechain": agent.is_some(),
        "type": "assistant",
        "message": {
            "id": format!("msg_{session}_{tag}_{index}"),
            "type": "message",
            "role": "assistant",
            "model": "claude-opus-5",
            "content": [{"type": "text", "text": text}],
            "stop_reason": "end_turn",
            "stop_sequence": null,
            "usage": {"input_tokens": 100 + index, "output_tokens": 10 + index,
                      "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0,
                      "cache_creation": {"ephemeral_1h_input_tokens": 0,
                                         "ephemeral_5m_input_tokens": 0},
                      "service_tier": "standard"}
        },
        "uuid": format!("{session}-{tag}-a{index}"),
        "timestamp": format!("2026-10-30T10:{:02}:30.000Z", index % 60),
        "userType": "external",
        "cwd": "/w/cut",
        "sessionId": session,
        "version": "2.1.241",
        "gitBranch": "main"
    });
    if let Some(agent) = agent {
        record["agentId"] = json!(agent);
    }
    record.to_string()
}

/// Turn `index` of a transcript: a user record, then the reply.
fn turn(session: &str, agent: Option<&str>, index: usize) -> [String; 2] {
    [
        claude_user(session, agent, index, &format!("question {index}")),
        claude_assistant(session, agent, index, &format!("answer {index}")),
    ]
}

fn lines(records: &[String]) -> String {
    records.iter().map(|record| format!("{record}\n")).collect()
}

/// The main transcript's records: `turns` turns of session `m`.
fn main_records(turns: usize) -> Vec<String> {
    (0..turns)
        .flat_map(|index| turn("m", None, index))
        .collect()
}

/// A Claude Code home with two root trees: `m` (its main transcript holding
/// `main`, plus subagent `a` with its sidecar) and an older `n`.
fn claude_tree(main: &str) -> TempDir {
    let home = TempDir::new().unwrap();
    let project =
        expand_data_root(&ClaudeCodeAdapter.layout().data_root, home.path()).join(PROJECT);
    let subagents = project.join("m/subagents");
    std::fs::create_dir_all(&subagents).unwrap();
    std::fs::write(
        project.join("n.jsonl"),
        lines(
            &(0..2)
                .flat_map(|index| turn("n", None, index))
                .collect::<Vec<_>>(),
        ),
    )
    .unwrap();
    std::fs::write(
        subagents.join("agent-a.jsonl"),
        lines(
            &(0..2)
                .flat_map(|index| turn("m", Some("a"), index))
                .collect::<Vec<_>>(),
        ),
    )
    .unwrap();
    std::fs::write(
        subagents.join("agent-a.meta.json"),
        json!({"agentType": "Explore"}).to_string(),
    )
    .unwrap();
    write_main(home.path(), main);
    home
}

/// Write the main transcript with its modification time pinned, so a fresh
/// build of the grown file and the cut build that grew see one mtime (the
/// session's `updated_at` is metadata, taken from the census).
fn write_main(home: &Path, text: &str) {
    let path = main_path(home);
    std::fs::write(&path, text).unwrap();
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(std::time::UNIX_EPOCH + Duration::from_secs(1_793_000_000))
        .unwrap();
}

fn main_path(home: &Path) -> PathBuf {
    expand_data_root(&ClaudeCodeAdapter.layout().data_root, home).join(MAIN)
}

fn roster_file<'a>(files: &'a mut [SessionFile], path: &str) -> &'a mut SessionFile {
    files
        .iter_mut()
        .find(|file| file.relative_path == path)
        .unwrap_or_else(|| panic!("{path} is not in the roster"))
}

// ── tests ───────────────────────────────────────────────────────────────────

/// A transcript appended to between the skim and capture publishes at the
/// cut — exactly its pre-growth records, Indexed when the cut is a record
/// boundary and Streaming when it falls inside a record its writer had not
/// finished — and the live runner's catch-up lands the growth exactly once:
/// the result equals a fresh build of the grown file, and the raw ledger
/// holds each of its records once. Serial and pooled, in one batch and in
/// batches of three records, so the cut also binds a pass's later reads.
#[test]
fn growth_after_the_skim_publishes_at_the_cut() {
    const TURNS: usize = 4;
    let grown = lines(&main_records(TURNS + 2));
    let boundary = lines(&main_records(TURNS));
    // Mid-record: the skim sees the opening bytes of turn TURNS's user record.
    let mid_record = format!("{boundary}{}", &grown[boundary.len()..boundary.len() + 40]);
    let mut small_batches = rosetta_capture::TailConfig::json_lines();
    small_batches.max_records_per_batch = 3;
    for (variant, at_skim, expected_state) in [
        ("boundary cut", boundary.as_str(), "indexed"),
        ("mid-record cut", mid_record.as_str(), "streaming"),
    ] {
        for walk in [Walk::Serial, Walk::Pooled(1), Walk::Pooled(4)] {
            for tail in [None, Some(&small_batches)] {
                let opts = IngestOptions {
                    tail: tail
                        .cloned()
                        .unwrap_or_else(|| IngestOptions::default().tail),
                    ..IngestOptions::default()
                };
                let label = format!(
                    "{variant}, {walk:?}, {} records a batch",
                    opts.tail.max_records_per_batch
                );
                let home = claude_tree(at_skim);
                let mut canonical = cold_store();
                let planned = plan(&ClaudeCodeAdapter, home.path(), &mut canonical);
                assert_eq!(
                    roster_cut(&planned.files, MAIN),
                    at_skim.len() as u64,
                    "{label}: the skim fixed the cut"
                );
                // The writer keeps writing after the skim: it finishes the
                // open record and appends more.
                write_main(home.path(), &grown);

                let ledger_dir = TempDir::new().unwrap();
                let mut ledger = Ledger::open(ledger_dir.path())
                    .unwrap()
                    .with_codec(SegmentCodec::Plain);
                let built = build(
                    planned,
                    &mut canonical,
                    ColdAdmission::RootTree,
                    walk,
                    &opts,
                    Some(&mut ledger),
                );
                assert!(built.per_tree, "{label}");
                // Before publication: exactly the pre-growth records.
                assert_eq!(
                    session(&canonical, "m"),
                    (2 * TURNS as i64, expected_state.to_string()),
                    "{label}"
                );
                assert_eq!(session(&canonical, "n"), (4, "indexed".into()), "{label}");
                ledger.flush().unwrap();
                assert_eq!(
                    raw_records(ledger_dir.path(), MAIN),
                    record_spans(&boundary),
                    "{label}: the cold build ledgers only what the cut holds"
                );
                canonical.finish_bulk_load().unwrap();
                catch_up(built, &mut canonical, Some(ledger));
                assert_eq!(
                    session(&canonical, "m"),
                    (2 * (TURNS as i64 + 2), "indexed".into()),
                    "{label}: catch-up lands the growth once"
                );
                assert_eq!(
                    raw_records(ledger_dir.path(), MAIN),
                    record_spans(&grown),
                    "{label}: every record of the grown file is ledgered once"
                );

                let fresh = fresh_build(&ClaudeCodeAdapter, home.path());
                assert_eq!(
                    session(&fresh, "m"),
                    (2 * (TURNS as i64 + 2), "indexed".into())
                );
                assert_same_projection(&label, &fresh, &canonical);
            }
        }
    }
}

fn roster_cut(files: &[SessionFile], path: &str) -> u64 {
    files
        .iter()
        .find(|file| file.relative_path == path)
        .and_then(|file| file.cut_len)
        .unwrap_or_else(|| panic!("{path} has no cut"))
}

/// `(generation, start, end)` of every raw ledger record of `path`, in
/// ledger order.
fn raw_records(ledger: &Path, path: &str) -> Vec<(u64, u64, u64)> {
    scan_ledger(ledger)
        .unwrap()
        .entries
        .into_iter()
        .filter_map(|entry| match entry {
            LedgerEntry::Raw {
                source,
                generation,
                start_offset,
                payload,
            } if source.relative_path == path => Some((
                generation,
                start_offset,
                start_offset + payload.len() as u64 + 1,
            )),
            _ => None,
        })
        .collect()
}

/// `(1, start, end)` of every line of `text`: one first generation, each
/// record once, in file order.
fn record_spans(text: &str) -> Vec<(u64, u64, u64)> {
    let mut start = 0;
    text.split_inclusive('\n')
        .map(|line| {
            let span = (1, start, start + line.len() as u64);
            start += line.len() as u64;
            span
        })
        .collect()
}

/// The skim's one-record identity fallback opens the file again, so it can
/// name a session from bytes past the cut: the file was empty, or held only
/// part of its opening record, at the skim's fstat. The cut-bounded read
/// then stops before or inside that record. The session publishes Streaming
/// with no rows, and the catch-up completes it to exactly a fresh build.
/// (The race is reproduced by moving the roster's cut back to where that
/// fstat would have put it.)
#[test]
fn a_session_named_past_its_cut_publishes_streaming_and_catch_up_completes_it() {
    let full = lines(&main_records(3));
    for (variant, cut) in [("empty at the fstat", 0), ("inside the opening record", 25)] {
        for walk in [Walk::Serial, Walk::Pooled(2)] {
            let label = format!("{variant}, {walk:?}");
            let home = claude_tree(&full);
            let mut canonical = cold_store();
            let mut planned = plan(&ClaudeCodeAdapter, home.path(), &mut canonical);
            roster_file(&mut planned.files, MAIN).cut_len = Some(cut);

            let built = build(
                planned,
                &mut canonical,
                ColdAdmission::RootTree,
                walk,
                &IngestOptions::default(),
                None,
            );
            assert_eq!(built.report.files_paused, 1, "{label}");
            assert_eq!(session(&canonical, "m"), (0, "streaming".into()), "{label}");
            canonical.finish_bulk_load().unwrap();
            catch_up(built, &mut canonical, None);
            assert_eq!(session(&canonical, "m"), (6, "indexed".into()), "{label}");
            assert_same_projection(
                &label,
                &fresh_build(&ClaudeCodeAdapter, home.path()),
                &canonical,
            );
        }
    }
}

/// Rewrites one source the first time capture reports bytes for it, so its
/// next read sees a changed anchor and restarts the generation mid-pass.
#[derive(Debug)]
struct RewriteOnce {
    path: PathBuf,
    target: String,
    replacement: String,
    done: AtomicBool,
}

impl ColdProgressObserver for RewriteOnce {
    fn source_bytes(&self, file: &SessionFile, _bytes: u64) {
        if file.relative_path == self.target && !self.done.swap(true, Ordering::SeqCst) {
            std::fs::write(&self.path, &self.replacement).unwrap();
        }
    }

    fn reading_finished(&self) {}
}

const HEAL_TRANSCRIPT: &str = "projects/-w-heal/heal-session.jsonl";
const HEAL_JOURNAL: &str =
    "projects/-w-heal/heal-session/subagents/workflows/wf_heal/journal.jsonl";

fn journal_record(kind: &str, worker: usize) -> String {
    match kind {
        "started" => json!({"type": "started", "key": format!("worker-{worker}"),
                            "agentId": format!("agent-{worker}")}),
        _ => json!({"type": "result", "key": format!("worker-{worker}"), "result": {"ok": true}}),
    }
    .to_string()
}

/// A two-transcript session (transcript + workflow journal) whose transcript
/// is rewritten mid-capture: the rewrite drops the transcript's cut, and the
/// restart heals its sibling — a forced replay that keeps the journal's cut,
/// so neither of the journal's generations frames a byte past it.
#[test]
fn sibling_heal_stays_capped() {
    let home = TempDir::new().unwrap();
    let root = expand_data_root(&ClaudeCodeAdapter.layout().data_root, home.path());
    let user = |prefix: &str, index: usize| {
        json!({
            "parentUuid": null, "isSidechain": false, "type": "user",
            "message": {"role": "user", "content": format!("{prefix} line {index}")},
            "uuid": format!("{prefix}-{index}"), "timestamp": "2026-08-25T02:35:19.054Z",
            "userType": "external", "cwd": "/w/heal", "sessionId": "heal-session",
            "version": "2.1.241", "gitBranch": "main"
        })
        .to_string()
    };
    let transcript_lines =
        |prefix: &str| lines(&(1..=5).map(|index| user(prefix, index)).collect::<Vec<_>>());
    let transcript = root.join(HEAL_TRANSCRIPT);
    let journal = root.join(HEAL_JOURNAL);
    std::fs::create_dir_all(journal.parent().unwrap()).unwrap();
    std::fs::write(&transcript, transcript_lines("old")).unwrap();
    let skimmed = lines(&[journal_record("started", 1), journal_record("result", 1)]);
    std::fs::write(&journal, &skimmed).unwrap();

    let mut tail = rosetta_capture::TailConfig::json_lines();
    // Three records a batch: the transcript is rewritten after its first,
    // and the heal's forced replay — one read — could reach past the cut.
    tail.max_records_per_batch = 3;
    let opts = IngestOptions {
        tail,
        progress: Some(Arc::new(RewriteOnce {
            path: transcript.clone(),
            target: HEAL_TRANSCRIPT.into(),
            // Longer than the transcript's cut: the replacement reads whole.
            replacement: transcript_lines("rewritten"),
            done: AtomicBool::new(false),
        })),
        ..IngestOptions::default()
    };
    let mut canonical = cold_store();
    let planned = plan_with(&ClaudeCodeAdapter, home.path(), &mut canonical, &opts);
    let journal_cut = roster_cut(&planned.files, HEAL_JOURNAL);
    let transcript_cut = roster_cut(&planned.files, HEAL_TRANSCRIPT);
    assert_eq!(journal_cut, skimmed.len() as u64);
    // The journal grows after the skim.
    std::fs::write(
        &journal,
        format!(
            "{skimmed}{}",
            lines(&[journal_record("started", 2), journal_record("result", 2)])
        ),
    )
    .unwrap();

    let ledger_dir = TempDir::new().unwrap();
    let mut ledger = Ledger::open(ledger_dir.path())
        .unwrap()
        .with_codec(SegmentCodec::Plain);
    let built = build(
        planned,
        &mut canonical,
        ColdAdmission::RootTree,
        Walk::Serial,
        &opts,
        Some(&mut ledger),
    );
    ledger.flush().unwrap();
    drop(ledger);
    assert_eq!(
        (
            built.report.generations_restarted,
            built.report.siblings_healed
        ),
        (2, 1),
        "the transcript restarts and the journal heals"
    );

    let mut journal_frames: BTreeMap<u64, Vec<u64>> = BTreeMap::new();
    let mut transcript_end = 0;
    for entry in scan_ledger(ledger_dir.path()).unwrap().entries {
        let LedgerEntry::Raw {
            source,
            generation,
            start_offset,
            payload,
        } = entry
        else {
            continue;
        };
        let end = start_offset + payload.len() as u64 + 1;
        if source.relative_path == HEAL_JOURNAL {
            journal_frames.entry(generation).or_default().push(end);
        } else if source.relative_path == HEAL_TRANSCRIPT {
            transcript_end = transcript_end.max(end);
        }
    }
    // Both journal generations — its own pass and the heal — stop at its cut.
    assert_eq!(journal_frames.len(), 2, "{journal_frames:?}");
    for (generation, ends) in &journal_frames {
        assert_eq!(ends.len(), 2, "generation {generation}: {ends:?}");
        assert!(
            ends.iter().all(|end| *end <= journal_cut),
            "generation {generation} framed past the cut {journal_cut}: {ends:?}"
        );
    }
    // The rewritten transcript read past its stale cut, to its new end.
    assert!(transcript_end > transcript_cut);
    assert_eq!(transcript_end, transcript_lines("rewritten").len() as u64);
}

/// Truncate the largest JSONL roster source of `home` to its first half of
/// records, returning its path and full content, so it can grow back after
/// the skim.
fn shrink_largest_transcript(adapter: &'static dyn AgentAdapter, home: &Path) -> (PathBuf, String) {
    let planned = plan(adapter, home, &mut cold_store());
    let file = planned
        .files
        .iter()
        .filter(|file| file.driver == StreamDriver::JsonLines && !file.ledger_only)
        .max_by_key(|file| (file.size, file.relative_path.clone()))
        .expect("a JSONL roster source");
    let path = planned.root.join(&file.relative_path);
    let full = std::fs::read_to_string(&path).unwrap();
    let records: Vec<&str> = full.lines().collect();
    assert!(
        records.len() >= 4,
        "{}: {} records",
        file.relative_path,
        records.len()
    );
    std::fs::write(
        &path,
        lines(
            &records[..records.len() / 2]
                .iter()
                .map(|r| r.to_string())
                .collect::<Vec<_>>(),
        ),
    )
    .unwrap();
    (path, full)
}

/// Every runtime admitted whole reads a transcript that grew after the skim
/// to its end, as at HEAD: a runtime-admission adapter, and a root-tree
/// adapter under the kill switch or after a truncated walk's fallback.
#[test]
fn runtime_adapters_read_to_eof() {
    let fixture = |kind: &str, scenario: &str| {
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../groundtruth")
            .join(kind)
            .join("fixtures");
        let version = std::fs::read_dir(&fixtures)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .min()
            .unwrap();
        let scenario = version.join(scenario);
        std::fs::read_dir(&scenario)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| path.is_dir())
            .unwrap()
    };
    let copy_home = |adapter: &dyn AgentAdapter, data: &Path| {
        let home = TempDir::new().unwrap();
        let to = expand_data_root(&adapter.layout().data_root, home.path());
        for entry in walkdir(data) {
            let relative = entry.strip_prefix(data).unwrap();
            let destination = to.join(relative);
            std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
            std::fs::copy(&entry, destination).unwrap();
        }
        home
    };

    let mut cases: Vec<(
        String,
        &'static dyn AgentAdapter,
        TempDir,
        ColdAdmission,
        bool,
    )> = Vec::new();
    for (adapter, scenario) in [
        (&KimiCodeAdapter as &'static dyn AgentAdapter, "tool-use"),
        (&PiAdapter, "tool-use"),
    ] {
        assert_eq!(adapter.cold_admission(), ColdAdmission::Runtime);
        let home = copy_home(adapter, &fixture(adapter.kind(), scenario));
        cases.push((
            adapter.kind().to_string(),
            adapter,
            home,
            ColdAdmission::Runtime,
            false,
        ));
    }
    let main = lines(&main_records(4));
    // `--experiment-cold-admission runtime` declares runtime admission for
    // every adapter; a truncated walk falls back to it.
    cases.push((
        "claude-code, kill switch".into(),
        &ClaudeCodeAdapter,
        claude_tree(&main),
        ColdAdmission::Runtime,
        false,
    ));
    cases.push((
        "claude-code, fallback".into(),
        &ClaudeCodeAdapter,
        claude_tree(&main),
        ColdAdmission::RootTree,
        true,
    ));

    for (label, adapter, home, admission, truncated) in cases {
        let expected = message_count(&fresh_build(adapter, home.path()));
        let (path, full) = shrink_largest_transcript(adapter, home.path());
        let at_skim = message_count(&fresh_build(adapter, home.path()));
        assert!(at_skim < expected, "{label}: {at_skim} of {expected}");

        let mut canonical = cold_store();
        let mut planned = plan(adapter, home.path(), &mut canonical);
        planned.truncated |= truncated;
        std::fs::write(&path, &full).unwrap();
        let built = build(
            planned,
            &mut canonical,
            admission,
            Walk::Pooled(2),
            &IngestOptions::default(),
            None,
        );
        assert!(!built.per_tree, "{label}");
        assert_eq!(message_count(&canonical), expected, "{label}: read to EOF");
    }
}

/// Every file under `root`, in path order.
fn walkdir(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}
