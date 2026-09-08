//! The disposable-projection proof: ingest a corpus WITH the ledger, delete
//! the agent's files entirely (GC), drop the projection — and rebuild an
//! equivalent projection from the ledger alone.

use std::io::Write;
use std::path::PathBuf;

use rosetta_adapter_claude_code::ClaudeCodeAdapter;
use rosetta_adapter_support::AgentAdapter;
use rosetta_bench_corpus::{Agent, CorpusSpec, generate};
use rosetta_capture::{Ledger, LedgerEntry, scan_ledger};
use rosetta_core::rows::RuntimeRow;
use rosetta_pipeline::{
    IngestOptions, Instrument, discover, expand_data_root, meta_ingest_into, rebuild_runtime,
    stream_backfill_into,
};
use rosetta_store::{Store, queries};
use tempfile::TempDir;

#[derive(Debug, PartialEq)]
struct Snapshot {
    sessions: usize,
    messages: i64,
    tool_rows: i64,
    fts_hits: usize,
    tokens: i64,
    projects: i64,
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
        messages: one("SELECT COUNT(*) FROM message"),
        tool_rows: one("SELECT COUNT(*) FROM message WHERE tool_result IS NOT NULL"),
        fts_hits: queries::search(store.conn(), "synthetic", 500)
            .unwrap()
            .len(),
        tokens: one("SELECT SUM(COALESCE(in_tokens,0) + COALESCE(out_tokens,0)) FROM usage_bucket"),
        projects: one("SELECT COUNT(*) FROM project"),
    }
}

struct Ledgered {
    _home: TempDir,
    root: PathBuf,
    ledger_dir: PathBuf,
    runtime: RuntimeRow,
    original: Snapshot,
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
    let line = serde_json::json!({
        "type": "user", "sessionId": native,
        "uuid": "00000000-0000-4000-8000-00000000feed",
        "parentUuid": null, "isSidechain": false, "userType": "external",
        "timestamp": "2026-05-01T00:00:00.000Z", "cwd": "/tmp/bench/project-0",
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
    let scanned = scan_ledger(&world.ledger_dir).unwrap();
    let (source, generation) = scanned
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
        .unwrap();
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
    assert_eq!(
        rebuilt
            .conn()
            .query_row("SELECT COUNT(*) FROM source_op", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0,
        "abandoned rebuild staging must be pruned"
    );
}
