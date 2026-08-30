//! Starter-slice proofs: a budgeted pass activates and seals a clean prefix,
//! the remainder streams later as ordinary append growth, and the result —
//! projection and ledger alike — is indistinguishable from one full pass.
//! Codex is the stress corpus on purpose: cumulative usage counters and
//! separate tool/usage merges must survive every slice boundary.

use std::path::Path;

use rosetta_adapter_codex::CodexAdapter;
use rosetta_adapter_support::AgentAdapter;
use rosetta_bench_corpus::{Agent, CorpusSpec, generate};
use rosetta_capture::Ledger;
use rosetta_core::ids;
use rosetta_core::rows::{RuntimeRow, StreamState};
use rosetta_pipeline::{
    IngestOptions, Instrument, SliceBudget, meta_ingest, meta_ingest_into, rebuild_runtime,
    stream_backfill, stream_backfill_into,
};
use rosetta_store::{Store, queries};
use tempfile::TempDir;

fn runtime(adapter: &dyn AgentAdapter) -> RuntimeRow {
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

fn full_opts() -> IngestOptions {
    IngestOptions {
        audit_files_per_run: 0,
        ..IngestOptions::default()
    }
}

fn budgeted_opts(max_bytes: u64, min_records: u32) -> IngestOptions {
    IngestOptions {
        starter_budget: Some(SliceBudget {
            max_bytes,
            min_records,
        }),
        ..full_opts()
    }
}

fn timeline(store: &Store, session: &str) -> Vec<rosetta_core::TimelineEntry> {
    queries::timeline_page(store.conn(), session, None, 10_000).unwrap()
}

type BucketRow = (i64, String, Option<i64>, Option<i64>, i64, i64);

fn buckets(store: &Store) -> Vec<BucketRow> {
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

fn session_state(store: &Store, session: &str) -> StreamState {
    queries::session(store.conn(), session)
        .unwrap()
        .unwrap()
        .stream_state
}

/// Slice a corpus to completion: budgeted passes until nothing pauses.
/// Returns how many passes paused before the finishing pass.
fn slice_until_done(
    root: &Path,
    runtime: &RuntimeRow,
    store: &mut Store,
    files: &[rosetta_pipeline::SessionFile],
    opts: &IngestOptions,
    ledger: Option<&mut Ledger>,
) -> u64 {
    let mut ledger = ledger;
    let mut paused_passes = 0;
    let mut attempts = 0;
    loop {
        attempts += 1;
        assert!(attempts <= 64, "budgeted passes did not converge");
        let report = stream_backfill_into(
            &CodexAdapter,
            runtime,
            root,
            store,
            files,
            opts,
            &Instrument::new(),
            ledger.as_deref_mut(),
        )
        .unwrap();
        assert_eq!(report.files_incomplete, 0);
        assert_eq!(report.hard_errors, 0);
        if report.files_paused == 0 {
            return paused_passes;
        }
        paused_passes += 1;
    }
}

#[test]
fn budgeted_slices_then_completion_match_one_shot() {
    let one_root = TempDir::new().unwrap();
    let sliced_root = TempDir::new().unwrap();
    let spec = CorpusSpec::new(Agent::Codex, 1, 30, 42);
    generate(&spec, one_root.path()).unwrap();
    generate(&spec, sliced_root.path()).unwrap();
    let runtime = runtime(&CodexAdapter);

    let mut one_store = Store::open_in_memory().unwrap();
    let (_, one_files) = meta_ingest(
        &CodexAdapter,
        &runtime,
        one_root.path(),
        &mut one_store,
        &full_opts(),
        &Instrument::new(),
    )
    .unwrap();
    let one_report = stream_backfill(
        &CodexAdapter,
        &runtime,
        one_root.path(),
        &mut one_store,
        &one_files,
        &full_opts(),
        &Instrument::new(),
    )
    .unwrap();
    assert_eq!(one_report.files_paused, 0, "no budget, no pauses");

    // Sliced side lives on disk so the first pause can cross a "restart".
    let db_dir = TempDir::new().unwrap();
    let db = db_dir.path().join("sliced.sqlite");
    let mut sliced_store = Store::open(&db).unwrap();
    let opts = budgeted_opts(1, 10);
    let (_, sliced_files) = meta_ingest(
        &CodexAdapter,
        &runtime,
        sliced_root.path(),
        &mut sliced_store,
        &opts,
        &Instrument::new(),
    )
    .unwrap();
    let session = sliced_files[0].session_id.clone();

    let first = stream_backfill(
        &CodexAdapter,
        &runtime,
        sliced_root.path(),
        &mut sliced_store,
        &sliced_files,
        &opts,
        &Instrument::new(),
    )
    .unwrap();
    assert_eq!(first.files_paused, 1, "the budget pauses the only file");
    assert_eq!(first.files_incomplete, 0, "a pause is not a failure");
    assert_eq!(first.sessions_indexed, 0);
    assert_eq!(
        session_state(&sliced_store, &session),
        StreamState::Streaming
    );
    let prefix = timeline(&sliced_store, &session);
    assert!(
        !prefix.is_empty(),
        "the activated prefix is readable immediately"
    );

    // The pause boundary survives a process restart: the checkpoint resumes
    // the ACTIVE generation instead of replaying from byte zero.
    drop(sliced_store);
    let mut sliced_store = Store::open(&db).unwrap();
    let paused_passes = slice_until_done(
        sliced_root.path(),
        &runtime,
        &mut sliced_store,
        &sliced_files,
        &opts,
        None,
    );
    assert!(
        paused_passes >= 1,
        "append growth pauses at least once more"
    );

    assert_eq!(session_state(&sliced_store, &session), StreamState::Indexed);
    let full = timeline(&sliced_store, &session);
    assert!(prefix.len() < full.len());
    assert_eq!(full, timeline(&one_store, &session));
    assert_eq!(buckets(&sliced_store), buckets(&one_store));
}

#[test]
fn sliced_ledger_rebuilds_the_same_projection() {
    let root = TempDir::new().unwrap();
    let spec = CorpusSpec::new(Agent::Codex, 1, 30, 42);
    generate(&spec, root.path()).unwrap();
    let runtime = runtime(&CodexAdapter);
    let ledger_dir = TempDir::new().unwrap();
    let mut ledger = Ledger::open(ledger_dir.path()).unwrap();

    let mut live_store = Store::open_in_memory().unwrap();
    let opts = budgeted_opts(1, 8);
    let (_, files) = meta_ingest_into(
        &CodexAdapter,
        &runtime,
        root.path(),
        &mut live_store,
        &opts,
        &Instrument::new(),
        Some(&mut ledger),
    )
    .unwrap();
    let paused_passes = slice_until_done(
        root.path(),
        &runtime,
        &mut live_store,
        &files,
        &opts,
        Some(&mut ledger),
    );
    assert!(
        paused_passes >= 1,
        "the ledger holds a sealed slice boundary"
    );
    ledger.flush().unwrap();
    drop(ledger);

    // The critical claim: a slice-then-append ledger is seal-complete, so
    // rebuild accepts it (no raw-without-seal refusal) and converges on the
    // same user-facing projection.
    let mut rebuilt = Store::open_in_memory().unwrap();
    let report = rebuild_runtime(
        &CodexAdapter,
        &runtime,
        ledger_dir.path(),
        &mut rebuilt,
        &Instrument::new(),
    )
    .unwrap();
    assert!(report.raw_records > 0);

    let session = &files[0].session_id;
    assert_eq!(timeline(&rebuilt, session), timeline(&live_store, session));
    assert_eq!(buckets(&rebuilt), buckets(&live_store));
}

#[test]
fn record_floor_governs_the_first_pause() {
    let root = TempDir::new().unwrap();
    let spec = CorpusSpec::new(Agent::Codex, 1, 30, 7);
    generate(&spec, root.path()).unwrap();
    let runtime = runtime(&CodexAdapter);

    let mut store = Store::open_in_memory().unwrap();
    let opts = budgeted_opts(1, 7);
    let (_, files) = meta_ingest(
        &CodexAdapter,
        &runtime,
        root.path(),
        &mut store,
        &opts,
        &Instrument::new(),
    )
    .unwrap();
    let report = stream_backfill(
        &CodexAdapter,
        &runtime,
        root.path(),
        &mut store,
        &files,
        &opts,
        &Instrument::new(),
    )
    .unwrap();
    assert_eq!(
        report.records, 7,
        "one byte of budget still frames the record floor, exactly"
    );
    assert_eq!(report.files_paused, 1);
    assert_eq!(report.files_incomplete, 0);
}
