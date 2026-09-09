//! The disposable-projection proof: ingest a corpus WITH the ledger, delete
//! the agent's files entirely (GC), drop the projection — and rebuild an
//! equivalent projection from the ledger alone.

use std::io::Write;
use std::path::PathBuf;

use rosetta_adapter_claude_code::ClaudeCodeAdapter;
use rosetta_adapter_codex::CodexAdapter;
use rosetta_adapter_support::AgentAdapter;
use rosetta_bench_corpus::{Agent, CorpusSpec, generate};
use rosetta_capture::{Ledger, LedgerEntry, SourceDef, scan_ledger};
use rosetta_core::ids;
use rosetta_core::rows::RuntimeRow;
use rosetta_pipeline::{
    IngestOptions, Instrument, RebuildOptions, SliceBudget, apply_prepared_runtime, discover,
    expand_data_root, meta_ingest, meta_ingest_into, prepare_captured_runtime, rebuild_runtime,
    rebuild_runtime_with_options, reserve_unsealed_generation, stream_backfill,
    stream_backfill_into,
};
use rosetta_store::{Store, queries};
use tempfile::TempDir;

#[derive(Debug, PartialEq)]
struct Snapshot {
    sessions: usize,
    indexed_sessions: i64,
    messages: i64,
    tool_rows: i64,
    fts_hits: usize,
    tokens: i64,
    projects: i64,
    session_cwds: i64,
}

fn snapshot(store: &Store) -> Snapshot {
    let one = |sql: &str| -> i64 {
        store
            .conn()
            .query_row(sql, [], |row| row.get::<_, Option<i64>>(0))
            .unwrap()
            .unwrap_or(0)
    };
    Snapshot {
        sessions: queries::session_list(store.conn(), None, 1000)
            .unwrap()
            .len(),
        indexed_sessions: one("SELECT COUNT(*) FROM session WHERE stream_state = 'indexed'"),
        messages: one("SELECT COUNT(*) FROM message"),
        tool_rows: one("SELECT COUNT(*) FROM message
             WHERE type = 'tool_use' AND tool_status = 'succeeded'"),
        fts_hits: queries::search(store.conn(), "synthetic", 500)
            .unwrap()
            .len(),
        tokens: one("SELECT SUM(COALESCE(in_tokens,0) + COALESCE(out_tokens,0)) FROM usage_bucket"),
        projects: one("SELECT COUNT(*) FROM project"),
        session_cwds: one("SELECT COUNT(*) FROM session WHERE cwd IS NOT NULL"),
    }
}

#[test]
fn resume_replay_ignores_a_never_sealed_source_but_ordinary_rebuild_fails_closed() {
    let temp = TempDir::new().unwrap();
    let ledger_dir = temp.path().join("ledger");
    let source = SourceDef {
        ledger_only: false,
        relative_path: "projects/interrupted.jsonl".into(),
        stream_id: "transcript".into(),
        session_id: "session-interrupted".into(),
        native_session_id: "native-interrupted".into(),
        agent_version: Some("test".into()),
    };
    let mut ledger = Ledger::open(&ledger_dir).unwrap();
    ledger
        .append_raw(&source, 1, [(0, br#"{"type":"partial"}"#.as_slice())])
        .unwrap();
    ledger.flush().unwrap();
    drop(ledger);
    let runtime = RuntimeRow {
        id: "runtime-interrupted".into(),
        kind: "claude-code".into(),
        data_root: "/tmp/interrupted".into(),
        enabled: true,
        account: None,
        config: None,
        skills: None,
        versions_seen: None,
        discovered_at: 1,
    };

    let mut strict = Store::open_in_memory().unwrap();
    let error = rebuild_runtime(
        &ClaudeCodeAdapter,
        &runtime,
        &ledger_dir,
        &mut strict,
        &Instrument::new(),
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("no complete-generation marker"),
        "{error}"
    );

    let mut resumed = Store::open_in_memory().unwrap();
    let report = rebuild_runtime_with_options(
        &ClaudeCodeAdapter,
        &runtime,
        &ledger_dir,
        &mut resumed,
        &Instrument::new(),
        RebuildOptions {
            allow_unsealed_sources: true,
            ..RebuildOptions::default()
        },
    )
    .unwrap();
    assert_eq!(report.raw_records, 0);
    assert_eq!(
        report
            .unsealed_sources
            .get(&source.relative_path)
            .unwrap()
            .0,
        1
    );
    assert!(
        queries::cursor_state(resumed.conn(), &runtime.id, &source.relative_path)
            .unwrap()
            .is_none(),
        "the source pass, not ledger replay, must restart this path"
    );
}

#[test]
fn interrupted_generation_is_reserved_before_source_retry() {
    let adapter = &ClaudeCodeAdapter;
    let home = TempDir::new().unwrap();
    let root = expand_data_root(&adapter.layout().data_root, home.path());
    std::fs::create_dir_all(&root).unwrap();
    generate(&CorpusSpec::new(Agent::ClaudeCode, 1, 8, 91), &root).unwrap();
    let opts = IngestOptions {
        audit_files_per_run: 0,
        ..IngestOptions::default()
    };
    let discovery = discover(&[adapter], home.path(), &opts, &Instrument::new())
        .unwrap()
        .pop()
        .unwrap();
    let mut runtime = discovery.runtime;
    runtime.enabled = true;
    let mut store = Store::open_in_memory_for_rebuild().unwrap();
    let (_, files) = meta_ingest(
        adapter,
        &runtime,
        &root,
        &mut store,
        &opts,
        &Instrument::new(),
    )
    .unwrap();
    let file = files.first().unwrap().clone();
    let source = SourceDef {
        ledger_only: false,
        relative_path: file.relative_path.clone(),
        stream_id: file.stream_id.clone(),
        session_id: file.session_id.clone(),
        native_session_id: file.native_session_id.clone(),
        agent_version: file.agent_version.clone(),
    };
    let ledger_dir = home.path().join("ledger");
    let mut ledger = Ledger::open(&ledger_dir).unwrap();
    ledger
        .append_raw(&source, 7, [(0, br#"{"type":"interrupted"}"#.as_slice())])
        .unwrap();
    ledger.flush().unwrap();
    drop(ledger);

    let report = rebuild_runtime_with_options(
        adapter,
        &runtime,
        &ledger_dir,
        &mut store,
        &Instrument::new(),
        RebuildOptions {
            flush_reconciliation: false,
            allow_unsealed_sources: true,
            ..RebuildOptions::default()
        },
    )
    .unwrap();
    let (generation, _) = report.unsealed_sources.get(&file.relative_path).unwrap();
    assert_eq!(*generation, 7);
    reserve_unsealed_generation(
        adapter,
        &runtime,
        &mut store,
        &file,
        *generation,
        &Instrument::new(),
    )
    .unwrap();
    let reserved = queries::cursor_state(store.conn(), &runtime.id, &file.relative_path)
        .unwrap()
        .unwrap();
    assert_eq!(reserved.active_generation, 0);
    assert_eq!(reserved.highest_generation, 7);

    let mut ledger = Ledger::open(&ledger_dir).unwrap();
    stream_backfill_into(
        adapter,
        &runtime,
        &root,
        &mut store,
        std::slice::from_ref(&file),
        &opts,
        &Instrument::new(),
        Some(&mut ledger),
    )
    .unwrap();
    ledger.flush().unwrap();
    let retried = queries::cursor_state(store.conn(), &runtime.id, &file.relative_path)
        .unwrap()
        .unwrap();
    assert_eq!(retried.active_generation, 8);
    assert_eq!(retried.highest_generation, 8);
    let scan = scan_ledger(&ledger_dir).unwrap();
    assert!(
        scan.entries
            .iter()
            .any(|entry| matches!(entry, LedgerEntry::GenerationComplete { generation: 8, .. }))
    );
}

#[test]
fn captured_ram_plan_matches_ordinary_cold_projection() {
    let adapter = &ClaudeCodeAdapter;
    let home = TempDir::new().unwrap();
    let root = expand_data_root(&adapter.layout().data_root, home.path());
    std::fs::create_dir_all(&root).unwrap();
    generate(&CorpusSpec::new(Agent::ClaudeCode, 5, 220, 73), &root).unwrap();
    let opts = IngestOptions {
        audit_files_per_run: 0,
        ..IngestOptions::default()
    };
    let discovery = discover(&[adapter], home.path(), &opts, &Instrument::new())
        .unwrap()
        .pop()
        .unwrap();
    let mut runtime = discovery.runtime;
    runtime.enabled = true;

    let mut ordinary = Store::open_in_memory_for_rebuild().unwrap();
    let (_, ordinary_files) = meta_ingest(
        adapter,
        &runtime,
        &root,
        &mut ordinary,
        &opts,
        &Instrument::new(),
    )
    .unwrap();
    stream_backfill(
        adapter,
        &runtime,
        &root,
        &mut ordinary,
        &ordinary_files,
        &opts,
        &Instrument::new(),
    )
    .unwrap();
    ordinary.finish_bulk_load().unwrap();

    let planning_instrument = Instrument::new();
    let mut planner = Store::open_in_memory_for_cold_planning().unwrap();
    let (_, files) = meta_ingest(
        adapter,
        &runtime,
        &root,
        &mut planner,
        &opts,
        &planning_instrument,
    )
    .unwrap();
    let stream = stream_backfill(
        adapter,
        &runtime,
        &root,
        &mut planner,
        &files,
        &opts,
        &planning_instrument,
    )
    .unwrap();
    let prepared =
        prepare_captured_runtime(adapter, &runtime, &files, &mut planner, &stream).unwrap();
    let mut planned = Store::open_in_memory_for_rebuild().unwrap();
    planned.use_precomputed_bulk_ordinals().unwrap();
    apply_prepared_runtime(
        adapter,
        &runtime,
        prepared,
        &mut planned,
        &Instrument::new(),
    )
    .unwrap();
    planned.flush_bulk_reconciliation().unwrap();
    planned.finish_bulk_load().unwrap();

    assert_eq!(snapshot(&planned), snapshot(&ordinary));
    let messages = |store: &Store| {
        let mut statement = store
            .conn()
            .prepare(
                "SELECT timeline_id, session_id, ordinal, type, content, detail,
                        tool_correlation_key, tool_name, tool_kind, tool_status,
                        tool_detail, parent_tool_native_id
                 FROM message ORDER BY timeline_id",
            )
            .unwrap();
        statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, Option<String>>(9)?,
                    row.get::<_, Option<String>>(10)?,
                    row.get::<_, Option<String>>(11)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };
    assert_eq!(messages(&planned), messages(&ordinary));
}

struct Ledgered {
    _home: TempDir,
    root: PathBuf,
    ledger_dir: PathBuf,
    runtime: RuntimeRow,
    original: Snapshot,
}

fn latest_clean_source(ledger_dir: &std::path::Path) -> (SourceDef, u64) {
    scan_ledger(ledger_dir)
        .unwrap()
        .entries
        .iter()
        .filter_map(|entry| match entry {
            LedgerEntry::GenerationComplete {
                source,
                generation,
                normalized_clean: true,
                ..
            } => Some((source.clone(), *generation)),
            _ => None,
        })
        .max_by_key(|(_, generation)| *generation)
        .unwrap()
}

/// Ingest a corpus with the ledger attached, snapshot the projection, then
/// destroy the source root — from here on, the ledger is all that's left.
fn ingest_then_lose_the_sources() -> Ledgered {
    let adapter = &ClaudeCodeAdapter;
    let home = TempDir::new().unwrap();
    let root = expand_data_root(&adapter.layout().data_root, home.path());
    std::fs::create_dir_all(&root).unwrap();
    generate(&CorpusSpec::new(Agent::ClaudeCode, 6, 260, 42), &root).unwrap();

    let opts = IngestOptions::default();
    let instrument = Instrument::new();
    let discoveries = discover(&[adapter], home.path(), &opts, &instrument).unwrap();
    let mut runtime = discoveries[0].runtime.clone();
    runtime.enabled = true;

    let ledger_dir = home.path().join("ledger/claude-code");
    let mut ledger = Ledger::open(&ledger_dir).unwrap();
    let mut store = Store::open_in_memory().unwrap();
    let (_, files) = meta_ingest_into(
        adapter,
        &runtime,
        &root,
        &mut store,
        &opts,
        &instrument,
        Some(&mut ledger),
    )
    .unwrap();

    // A mid-history rewrite: the ledger must hold TWO generations for this
    // file and rebuild must keep only the surviving one.
    let victim = &files[1];
    let victim_path = root.join(&victim.relative_path);
    let first_pass = stream_backfill_into(
        adapter,
        &runtime,
        &root,
        &mut store,
        &files,
        &opts,
        &instrument,
        Some(&mut ledger),
    )
    .unwrap();
    assert!(first_pass.stats.messages_inserted > 0);
    let text = std::fs::read_to_string(&victim_path).unwrap();
    let kept: Vec<&str> = text.lines().take(4).collect();
    std::fs::write(&victim_path, format!("{}\n", kept.join("\n"))).unwrap();
    let second_pass = stream_backfill_into(
        adapter,
        &runtime,
        &root,
        &mut store,
        &files,
        &opts,
        &instrument,
        Some(&mut ledger),
    )
    .unwrap();
    assert_eq!(second_pass.generations_restarted, 1);

    // Append after the rewrite so the surviving generation grows further.
    let native = &victim.native_session_id;
    let workspace = root.join("workspace-probe");
    std::fs::create_dir_all(workspace.join(".git")).unwrap();
    std::fs::write(
        workspace.join(".git/config"),
        "[remote \"origin\"]\n  url = git@example.com:acme/replay-probe.git\n",
    )
    .unwrap();
    let line = serde_json::json!({
        "type": "user", "sessionId": native,
        "uuid": "00000000-0000-4000-8000-00000000feed",
        "parentUuid": null, "isSidechain": false, "userType": "external",
        "timestamp": "2026-05-01T00:00:00.000Z", "cwd": workspace.to_string_lossy(),
        "gitBranch": "main", "version": "2.1.240",
        "message": {"role": "user", "content": "citlalicue synthetic rebuild probe"},
    });
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&victim_path)
        .unwrap();
    writeln!(file, "{line}").unwrap();
    stream_backfill_into(
        adapter,
        &runtime,
        &root,
        &mut store,
        &files,
        &opts,
        &instrument,
        Some(&mut ledger),
    )
    .unwrap();

    let original = snapshot(&store);
    assert!(original.messages > 0 && original.fts_hits > 0 && original.tokens > 0);
    assert!(
        original.session_cwds > 0,
        "meta admission must retain session cwd"
    );
    assert_eq!(
        store
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM project WHERE git_repo LIKE '%replay-probe%'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0,
        "a later stream cwd must not create or reassign a project"
    );

    // The agent GC's everything. The projection is about to be dropped too.
    std::fs::remove_dir_all(&root).unwrap();

    Ledgered {
        _home: home,
        root,
        ledger_dir,
        runtime,
        original,
    }
}

#[test]
fn rebuild_from_ledger_alone_reproduces_the_projection() {
    let adapter = &ClaudeCodeAdapter;
    let world = ingest_then_lose_the_sources();
    assert!(!world.root.exists(), "sources must be fully gone");

    // Crash protocol adversaries: an at-least-once retry writes one exact
    // source position twice, then a later rewrite dies before its completion
    // marker. Neither may change the elected projection generation.
    let scanned = scan_ledger(&world.ledger_dir).unwrap();
    let (source, generation, offset, payload) = scanned
        .entries
        .iter()
        .rev()
        .find_map(|entry| match entry {
            LedgerEntry::Raw {
                source,
                generation,
                start_offset,
                payload,
            } if scanned.entries.iter().any(|candidate| {
                matches!(
                    candidate,
                    LedgerEntry::GenerationComplete {
                        source: sealed,
                        generation: sealed_generation,
                        normalized_clean: true,
                        ..
                    } if sealed.relative_path == source.relative_path
                        && sealed_generation == generation
                )
            }) =>
            {
                Some((source.clone(), *generation, *start_offset, payload.clone()))
            }
            _ => None,
        })
        .expect("ledger has one clean raw generation");
    let mut crash_tail = Ledger::open(&world.ledger_dir).unwrap();
    crash_tail
        .append_raw(&source, generation, [(offset, payload.as_slice())])
        .unwrap();
    crash_tail
        .append_raw(&source, generation + 100, [(0, payload.as_slice())])
        .unwrap();
    crash_tail.flush().unwrap();
    drop(crash_tail);

    let instrument = Instrument::new();
    let mut rebuilt_store = Store::open_in_memory().unwrap();
    let report = rebuild_runtime(
        adapter,
        &world.runtime,
        &world.ledger_dir,
        &mut rebuilt_store,
        &instrument,
    )
    .unwrap();

    assert!(report.meta_batches >= 1);
    assert!(report.raw_records > 0);
    assert!(report.duplicate_records_skipped > 0);
    assert!(
        report.stale_records_skipped > 0,
        "the rewritten file's first generation must be skipped"
    );
    assert_eq!(report.hard_errors, 0);
    assert_eq!(report.torn_segments, 0);
    assert_eq!(
        report.sessions_stubbed, 0,
        "every session was named by a journaled meta batch"
    );

    let rebuilt = snapshot(&rebuilt_store);
    assert_eq!(
        rebuilt, world.original,
        "rebuild must reproduce the projection"
    );
    assert_eq!(
        queries::search(rebuilt_store.conn(), "citlalicue", 5)
            .unwrap()
            .len(),
        1,
        "the post-rewrite append survives rebuild"
    );

    // Rebuild is idempotent: replaying into the same projection changes nothing.
    let again = rebuild_runtime(
        adapter,
        &world.runtime,
        &world.ledger_dir,
        &mut rebuilt_store,
        &instrument,
    )
    .unwrap();
    assert_eq!(again.hard_errors, 0);
    assert_eq!(snapshot(&rebuilt_store), world.original);
}

#[test]
fn rebuild_never_activates_a_generation_that_normalizes_with_hard_errors() {
    let adapter = &ClaudeCodeAdapter;
    let world = ingest_then_lose_the_sources();
    let (source, generation) = latest_clean_source(&world.ledger_dir);
    let broken_generation = generation + 100;
    let mut ledger = Ledger::open(&world.ledger_dir).unwrap();
    ledger
        .append_raw(&source, broken_generation, [(0, b"{".as_slice())])
        .unwrap();
    // Even if an old writer incorrectly claimed cleanliness, replay trusts
    // the current normalizer outcome before it exposes the generation.
    ledger
        .complete_generation(
            &source,
            broken_generation,
            adapter.normalizer_version(),
            true,
        )
        .unwrap();
    ledger.flush().unwrap();
    drop(ledger);

    let mut rebuilt = Store::open_in_memory().unwrap();
    let report = rebuild_runtime(
        adapter,
        &world.runtime,
        &world.ledger_dir,
        &mut rebuilt,
        &Instrument::new(),
    )
    .unwrap();
    assert!(report.hard_errors > 0);
    let cursor = queries::cursor_state(rebuilt.conn(), &world.runtime.id, &source.relative_path)
        .unwrap()
        .unwrap();
    assert_eq!(cursor.active_generation, 0);
    let state: String = rebuilt
        .conn()
        .query_row(
            "SELECT stream_state FROM session WHERE id = ?1",
            [&source.session_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(state, "streaming");
    let private_objects: i64 = rebuilt
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM sqlite_schema
             WHERE name IN (
               'source_op', 'pending_merge', 'tool_update_fact', 'session_title_fact'
             )",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(private_objects, 0, "rebuild staging must be RAM-only");
}

#[test]
fn rebuild_retains_the_clean_generation_but_downgrades_an_unclean_latest_seal() {
    let adapter = &ClaudeCodeAdapter;
    let world = ingest_then_lose_the_sources();
    let (source, generation) = latest_clean_source(&world.ledger_dir);
    let broken_generation = generation + 100;
    let mut ledger = Ledger::open(&world.ledger_dir).unwrap();
    ledger
        .append_raw(&source, broken_generation, [(0, b"{".as_slice())])
        .unwrap();
    ledger
        .complete_generation(
            &source,
            broken_generation,
            adapter.normalizer_version(),
            false,
        )
        .unwrap();
    ledger.flush().unwrap();
    drop(ledger);

    let mut rebuilt = Store::open_in_memory().unwrap();
    let report = rebuild_runtime(
        adapter,
        &world.runtime,
        &world.ledger_dir,
        &mut rebuilt,
        &Instrument::new(),
    )
    .unwrap();
    assert_eq!(
        report.hard_errors, 0,
        "the known-bad generation is not replayed"
    );
    assert!(report.stale_records_skipped > 0);
    let cursor = queries::cursor_state(rebuilt.conn(), &world.runtime.id, &source.relative_path)
        .unwrap()
        .unwrap();
    assert_eq!(cursor.active_generation, generation);
    let state: String = rebuilt
        .conn()
        .query_row(
            "SELECT stream_state FROM session WHERE id = ?1",
            [&source.session_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(state, "streaming");
}

// ---------------------------------------------------------------------------
// Dirty-source rebuild parity.
//
// The runtime converged on live-growth semantics for a source that goes bad
// partway through (`dirty_sources_converge_on_live_growth_semantics` in
// tests/slice.rs): the parseable rows publish, sliced or not, and readiness
// never leaves Streaming. Replay owes the reader the SAME projection from the
// ledger alone — a rebuild that abandoned where the runtime activated would
// silently empty a transcript the user was already reading.
//
// Codex is the stress corpus here for the reason it is there: its cumulative
// usage counters have to survive every boundary the ledger draws.

fn codex_runtime() -> RuntimeRow {
    let adapter = &CodexAdapter;
    RuntimeRow {
        id: ids::runtime_id(adapter.kind(), &adapter.layout().data_root),
        kind: adapter.kind().to_owned(),
        data_root: adapter.layout().data_root.clone(),
        enabled: true,
        account: None,
        config: None,
        skills: None,
        versions_seen: None,
        discovered_at: 1,
    }
}

/// Line 39 sits well past the first budgeted slice, so a sliced run activates
/// and seals a clean prefix several passes before any pass reads the damage.
const DIRTY_LINE: usize = 39;

/// A one-session codex corpus with one mid-file line replaced by a byte that
/// frames as a COMPLETE record and cannot parse: a hard error surrounded by
/// good records, not a torn tail.
fn dirty_codex_corpus(root: &std::path::Path) {
    generate(&CorpusSpec::new(Agent::Codex, 1, 60, 11), root).unwrap();
    let path: PathBuf = walkdir::WalkDir::new(root)
        .into_iter()
        .filter_map(Result::ok)
        .map(|entry| entry.into_path())
        .find(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
        .expect("the corpus wrote one jsonl source");
    let text = std::fs::read_to_string(&path).unwrap();
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    assert!(
        DIRTY_LINE < lines.len(),
        "the corpus must reach line {DIRTY_LINE}"
    );
    lines[DIRTY_LINE] = "{".to_string();
    std::fs::write(&path, format!("{}\n", lines.join("\n"))).unwrap();
}

fn timeline_rows(store: &Store) -> Vec<rosetta_core::TimelineEntry> {
    queries::timeline_page(store.conn(), &only_session(store), None, 10_000).unwrap()
}

type BucketRow = (i64, String, Option<i64>, Option<i64>, i64, i64);

fn usage_buckets(store: &Store) -> Vec<BucketRow> {
    let mut stmt = store
        .conn()
        .prepare(
            "SELECT hour_ts, model, in_tokens, out_tokens, message_count, tool_call_count
             FROM usage_bucket ORDER BY hour_ts, model",
        )
        .unwrap();
    stmt.query_map([], |row| {
        Ok((
            row.get(0)?,
            row.get(1)?,
            row.get(2)?,
            row.get(3)?,
            row.get(4)?,
            row.get(5)?,
        ))
    })
    .unwrap()
    .collect::<Result<Vec<_>, _>>()
    .unwrap()
}

/// The single-session corpora below give both stores exactly one session, and
/// reading it back rather than carrying the id in proves the rebuilt store
/// minted the same one.
fn only_session(store: &Store) -> String {
    let sessions = queries::session_list(store.conn(), None, 1000).unwrap();
    assert_eq!(sessions.len(), 1, "the corpus holds one session");
    sessions[0].id.clone()
}

fn session_stream_state(store: &Store) -> String {
    store
        .conn()
        .query_row(
            "SELECT stream_state FROM session WHERE id = ?1",
            [&only_session(store)],
            |row| row.get(0),
        )
        .unwrap()
}

/// Every seal in write order as `(generation, clean)`, plus how many raw
/// records were written AFTER the last of them. Both proofs below assert the
/// shape they claim to exercise, so a runtime change that stops producing it
/// fails here loudly instead of quietly making the proof vacuous.
fn ledger_shape(ledger_dir: &std::path::Path) -> (Vec<(u64, bool)>, usize) {
    let entries = scan_ledger(ledger_dir).unwrap().entries;
    let seals = entries
        .iter()
        .filter_map(|entry| match entry {
            LedgerEntry::GenerationComplete {
                generation,
                normalized_clean,
                ..
            } => Some((*generation, *normalized_clean)),
            _ => None,
        })
        .collect();
    let raw_after_last_seal = entries
        .iter()
        .rposition(|entry| matches!(entry, LedgerEntry::GenerationComplete { .. }))
        .map(|last| {
            entries[last + 1..]
                .iter()
                .filter(|entry| matches!(entry, LedgerEntry::Raw { .. }))
                .count()
        })
        .unwrap_or_default();
    (seals, raw_after_last_seal)
}

#[test]
fn dirty_first_generation_rebuilds_to_its_parseable_rows() {
    let adapter = &CodexAdapter;
    let root = TempDir::new().unwrap();
    dirty_codex_corpus(root.path());
    let runtime = codex_runtime();
    let ledger_dir = TempDir::new().unwrap();
    let mut ledger = Ledger::open(ledger_dir.path()).unwrap();
    let opts = IngestOptions::default();
    let instrument = Instrument::new();

    // One unbudgeted pass: the first generation reaches EOF unclean, so the
    // ledger's ONLY seal for this path is an unclean one.
    let mut live = Store::open_in_memory().unwrap();
    let (_, files) = meta_ingest_into(
        adapter,
        &runtime,
        root.path(),
        &mut live,
        &opts,
        &instrument,
        Some(&mut ledger),
    )
    .unwrap();
    let pass = stream_backfill_into(
        adapter,
        &runtime,
        root.path(),
        &mut live,
        &files,
        &opts,
        &instrument,
        Some(&mut ledger),
    )
    .unwrap();
    assert_eq!(pass.hard_errors, 1, "the corrupt line is one hard error");
    assert_eq!(pass.files_incomplete, 1);
    ledger.flush().unwrap();
    drop(ledger);

    let (seals, raw_after_last_seal) = ledger_shape(ledger_dir.path());
    assert_eq!(
        seals.len(),
        1,
        "one EOF seal, and it is the only one this path ever gets"
    );
    assert!(!seals[0].1, "that only seal is unclean");
    assert_eq!(raw_after_last_seal, 0, "nothing follows the EOF seal");

    let live_rows = timeline_rows(&live);
    assert!(
        !live_rows.is_empty(),
        "a dirty first generation publishes its parseable rows"
    );
    assert_eq!(session_stream_state(&live), "streaming");

    let mut rebuilt = Store::open_in_memory().unwrap();
    let replayed = rebuild_runtime(
        adapter,
        &runtime,
        ledger_dir.path(),
        &mut rebuilt,
        &Instrument::new(),
    )
    .unwrap();
    assert!(
        replayed.hard_errors > 0,
        "the raw ledger carried the damage into replay"
    );

    assert_eq!(
        timeline_rows(&rebuilt),
        live_rows,
        "replay must publish the same parseable rows the runtime did"
    );
    assert_eq!(usage_buckets(&rebuilt), usage_buckets(&live));
    assert_eq!(session_stream_state(&rebuilt), "streaming");

    // The load-bearing bit: no clean seal ever existed on this path, so replay
    // elects the unclean one and ACTIVATES it. Abandoning would leave the
    // cursor parked at generation 0 with nothing published.
    let cursor = queries::cursor_state(rebuilt.conn(), &runtime.id, &files[0].relative_path)
        .unwrap()
        .unwrap();
    assert_ne!(
        cursor.active_generation, 0,
        "the only-dirty generation activated on replay"
    );
}

#[test]
fn sliced_dirty_ledger_rebuilds_to_the_same_projection() {
    let adapter = &CodexAdapter;
    let root = TempDir::new().unwrap();
    dirty_codex_corpus(root.path());
    let runtime = codex_runtime();
    let ledger_dir = TempDir::new().unwrap();
    let mut ledger = Ledger::open(ledger_dir.path()).unwrap();
    let opts = IngestOptions {
        starter_budget: Some(SliceBudget {
            max_bytes: 1,
            min_records: 10,
        }),
        audit_files_per_run: 0,
        ..IngestOptions::default()
    };
    let instrument = Instrument::new();

    let mut live = Store::open_in_memory().unwrap();
    let (_, files) = meta_ingest_into(
        adapter,
        &runtime,
        root.path(),
        &mut live,
        &opts,
        &instrument,
        Some(&mut ledger),
    )
    .unwrap();

    // Budgeted passes until one finally reads the corrupt line. Every pause
    // seals a CLEAN generation; the passes after it are append growth on that
    // same generation, so the damage lands in the ledger as post-seal appends.
    let mut passes = 0;
    loop {
        passes += 1;
        assert!(
            passes <= 32,
            "the budgeted passes never reached the corrupt line"
        );
        let report = stream_backfill_into(
            adapter,
            &runtime,
            root.path(),
            &mut live,
            &files,
            &opts,
            &instrument,
            Some(&mut ledger),
        )
        .unwrap();
        if report.hard_errors > 0 {
            assert_eq!(report.files_incomplete, 1);
            break;
        }
        assert_eq!(report.files_paused, 1, "a clean budgeted pass pauses");
    }
    assert!(
        passes > 1,
        "the seal must land before the damage, not with it"
    );
    ledger.flush().unwrap();
    drop(ledger);

    // The shape this proof exists for: a CLEAN seal, then raw appends that
    // include the damage. Growth on an already-active generation is never
    // resealed, so the seal cannot know what came after it.
    let (seals, raw_after_last_seal) = ledger_shape(ledger_dir.path());
    assert_eq!(seals.len(), 1, "the pause sealed once");
    assert!(seals[0].1, "the sealed prefix was clean");
    assert!(
        raw_after_last_seal > 0,
        "the dirty remainder is post-seal append growth"
    );

    let live_rows = timeline_rows(&live);
    assert!(
        !live_rows.is_empty(),
        "the sliced prefix and its append growth are readable"
    );
    assert_eq!(session_stream_state(&live), "streaming");

    // THE PROOF: a clean seal followed by dirty appends is the live-growth
    // shape. Replay must activate at that seal and keep applying the appends —
    // abandoning on the later dirt would empty a transcript the reader has.
    let mut rebuilt = Store::open_in_memory().unwrap();
    let replayed = rebuild_runtime(
        adapter,
        &runtime,
        ledger_dir.path(),
        &mut rebuilt,
        &Instrument::new(),
    )
    .unwrap();
    assert!(
        replayed.hard_errors > 0,
        "the post-seal appends carried the damage into replay"
    );

    assert_eq!(
        timeline_rows(&rebuilt),
        live_rows,
        "a sliced dirty ledger must rebuild to the sliced projection"
    );
    assert_eq!(usage_buckets(&rebuilt), usage_buckets(&live));
    assert_eq!(session_stream_state(&rebuilt), "streaming");
}
