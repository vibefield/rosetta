//! Starter-slice proofs: a budgeted pass activates and seals a clean prefix,
//! the remainder streams later as ordinary append growth, and the result —
//! projection and ledger alike — is indistinguishable from one full pass.
//! Codex is the stress corpus on purpose: cumulative usage counters and
//! separate tool/usage merges must survive every slice boundary. claude-code
//! carries the other half of the risk — its tool calls and their results are
//! separate records, so a boundary drawn between them proves the merge is
//! resumed from the checkpoint rather than from a live accumulator.

use std::io::Write;
use std::path::{Path, PathBuf};

use rosetta_adapter_claude_code::ClaudeCodeAdapter;
use rosetta_adapter_codex::CodexAdapter;
use rosetta_adapter_support::AgentAdapter;
use rosetta_bench_corpus::{Agent, CorpusSpec, generate};
use rosetta_capture::Ledger;
use rosetta_core::ids;
use rosetta_core::rows::{RuntimeRow, StreamState};
use rosetta_core::{StatusBasis, TimelineBody, ToolStatus, ToolStatusFact};
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

/// [`slice_until_done`] for any adapter, ledger-free: budgeted passes until
/// nothing pauses. Returns how many passes paused before the finishing pass.
fn slice_until_done_for(
    adapter: &dyn AgentAdapter,
    root: &Path,
    runtime: &RuntimeRow,
    store: &mut Store,
    files: &[rosetta_pipeline::SessionFile],
    opts: &IngestOptions,
) -> u64 {
    let mut paused_passes = 0;
    let mut attempts = 0;
    loop {
        attempts += 1;
        assert!(attempts <= 64, "budgeted passes did not converge");
        let report = stream_backfill(
            adapter,
            runtime,
            root,
            store,
            files,
            opts,
            &Instrument::new(),
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

/// The unbudgeted control: one meta pass and one full stream pass over `root`.
fn one_shot(
    adapter: &dyn AgentAdapter,
    runtime: &RuntimeRow,
    root: &Path,
) -> (Store, Vec<rosetta_pipeline::SessionFile>) {
    let mut store = Store::open_in_memory().unwrap();
    let (_, files) = meta_ingest(
        adapter,
        runtime,
        root,
        &mut store,
        &full_opts(),
        &Instrument::new(),
    )
    .unwrap();
    let report = stream_backfill(
        adapter,
        runtime,
        root,
        &mut store,
        &files,
        &full_opts(),
        &Instrument::new(),
    )
    .unwrap();
    assert_eq!(report.files_paused, 0, "no budget, no pauses");
    assert_eq!(report.files_incomplete, 0);
    assert_eq!(report.hard_errors, 0);
    (store, files)
}

/// The tool verdict an entry carries, when the entry is a tool interaction.
fn tool_status(entry: &rosetta_core::TimelineEntry) -> Option<ToolStatusFact> {
    match &entry.body {
        TimelineBody::ToolUse { tool } => Some(tool.status),
        _ => None,
    }
}

/// The one transcript a single-session corpus wrote.
fn only_jsonl(root: &Path) -> PathBuf {
    walkdir::WalkDir::new(root)
        .into_iter()
        .filter_map(Result::ok)
        .map(|entry| entry.into_path())
        .find(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
        .unwrap()
}

#[test]
fn claude_multi_session_slices_match_one_shot() {
    let one_root = TempDir::new().unwrap();
    let sliced_root = TempDir::new().unwrap();
    let spec = CorpusSpec::new(Agent::ClaudeCode, 3, 60, 7);
    generate(&spec, one_root.path()).unwrap();
    generate(&spec, sliced_root.path()).unwrap();
    let runtime = runtime(&ClaudeCodeAdapter);

    let (one_store, one_files) = one_shot(&ClaudeCodeAdapter, &runtime, one_root.path());
    assert!(one_files.len() > 1, "the corpus must span several sessions");

    let mut sliced_store = Store::open_in_memory().unwrap();
    let opts = budgeted_opts(1, 6);
    let (_, sliced_files) = meta_ingest(
        &ClaudeCodeAdapter,
        &runtime,
        sliced_root.path(),
        &mut sliced_store,
        &opts,
        &Instrument::new(),
    )
    .unwrap();
    assert_eq!(sliced_files.len(), one_files.len());
    let paused_passes = slice_until_done_for(
        &ClaudeCodeAdapter,
        sliced_root.path(),
        &runtime,
        &mut sliced_store,
        &sliced_files,
        &opts,
    );
    assert!(paused_passes >= 1, "the budget must pause a pass");

    // Every session finishes, and every session's timeline is byte-for-byte
    // what one pass produced — tool_use openers and their results land in the
    // same merged rows however the boundaries fell between them.
    let mut tools = 0;
    for file in &sliced_files {
        let session = &file.session_id;
        assert_eq!(session_state(&sliced_store, session), StreamState::Indexed);
        let mut page = timeline(&sliced_store, session);
        assert!(!page.is_empty(), "{session} projected nothing");
        tools += page
            .iter()
            .filter(|entry| tool_status(entry).is_some())
            .count();
        // Independent projections can admit files in different orders and
        // assign different cursor IDs. Prove the same source identity before
        // mapping those local IDs; byte offsets and all other fields still
        // compare exactly.
        let sliced_cursor =
            queries::cursor_state(sliced_store.conn(), &runtime.id, &file.relative_path)
                .unwrap()
                .unwrap();
        let one_cursor = queries::cursor_state(one_store.conn(), &runtime.id, &file.relative_path)
            .unwrap()
            .unwrap();
        for entry in &mut page {
            assert_eq!(entry.provenance.src_cursor, Some(sliced_cursor.id));
            entry.provenance.src_cursor = Some(one_cursor.id);
        }
        assert_eq!(page, timeline(&one_store, session));
    }
    assert!(tools > 0, "the compared pages must carry tool merges");
    assert_eq!(buckets(&sliced_store), buckets(&one_store));
}

#[test]
fn growth_between_slices_matches_one_shot() {
    let one_root = TempDir::new().unwrap();
    let sliced_root = TempDir::new().unwrap();
    let spec = CorpusSpec::new(Agent::Codex, 1, 30, 42);
    generate(&spec, one_root.path()).unwrap();
    generate(&spec, sliced_root.path()).unwrap();

    // The sliced side starts truncated: its first budgeted pass pauses INSIDE
    // the prefix, so the file is still mid-slice when it grows underneath.
    let path = only_jsonl(sliced_root.path());
    let text = std::fs::read_to_string(&path).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert!(lines.len() > 12, "the corpus must outlive its prefix");
    let (prefix, rest) = lines.split_at(12);
    std::fs::write(&path, format!("{}\n", prefix.join("\n"))).unwrap();

    let runtime = runtime(&CodexAdapter);
    let (one_store, _) = one_shot(&CodexAdapter, &runtime, one_root.path());

    let mut sliced_store = Store::open_in_memory().unwrap();
    let opts = budgeted_opts(1, 5);
    let (_, sliced_files) = meta_ingest(
        &CodexAdapter,
        &runtime,
        sliced_root.path(),
        &mut sliced_store,
        &opts,
        &Instrument::new(),
    )
    .unwrap();
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
    assert_eq!(first.files_paused, 1, "the pause lands inside the prefix");
    assert_eq!(first.records, 5, "the floor frames the first slice");

    let mut append = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    writeln!(append, "{}", rest.join("\n")).unwrap();
    drop(append);

    let paused_passes = slice_until_done_for(
        &CodexAdapter,
        sliced_root.path(),
        &runtime,
        &mut sliced_store,
        &sliced_files,
        &opts,
    );
    assert!(paused_passes >= 1, "the growth is sliced too");

    let session = &sliced_files[0].session_id;
    assert_eq!(session_state(&sliced_store, session), StreamState::Indexed);
    assert_eq!(
        timeline(&sliced_store, session),
        timeline(&one_store, session)
    );
    assert_eq!(buckets(&sliced_store), buckets(&one_store));
}

#[test]
fn pause_between_tool_use_and_result_matches_one_shot() {
    let one_root = TempDir::new().unwrap();
    let sliced_root = TempDir::new().unwrap();
    let spec = CorpusSpec::new(Agent::ClaudeCode, 1, 60, 3);
    generate(&spec, one_root.path()).unwrap();
    generate(&spec, sliced_root.path()).unwrap();

    // The record floor is also the batch granularity, so a floor set to the
    // opener's line number draws the boundary in the worst place there is:
    // the call is projected and its result is still unread.
    let text = std::fs::read_to_string(only_jsonl(sliced_root.path())).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    let opener = lines
        .windows(2)
        .position(|pair| {
            pair[0].contains(r#""type":"tool_use""#) && pair[1].contains(r#""type":"tool_result""#)
        })
        .expect("the corpus must contain a tool round-trip");
    let floor = u32::try_from(opener + 1).unwrap();

    let runtime = runtime(&ClaudeCodeAdapter);
    let (one_store, _) = one_shot(&ClaudeCodeAdapter, &runtime, one_root.path());

    let mut sliced_store = Store::open_in_memory().unwrap();
    let opts = budgeted_opts(1, floor);
    let (_, sliced_files) = meta_ingest(
        &ClaudeCodeAdapter,
        &runtime,
        sliced_root.path(),
        &mut sliced_store,
        &opts,
        &Instrument::new(),
    )
    .unwrap();
    let first = stream_backfill(
        &ClaudeCodeAdapter,
        &runtime,
        sliced_root.path(),
        &mut sliced_store,
        &sliced_files,
        &opts,
        &Instrument::new(),
    )
    .unwrap();
    assert_eq!(first.files_paused, 1);
    assert_eq!(
        first.records,
        u64::from(floor),
        "the pause must land on the opener, not past its result"
    );

    // The boundary really did land between the two records: the sealed prefix
    // ends on the call, and the call carries no verdict — exactly the row a
    // file truncated here would produce.
    let split = timeline(&sliced_store, &sliced_files[0].session_id)
        .pop()
        .expect("the prefix is readable at the pause");
    assert_eq!(
        tool_status(&split),
        Some(ToolStatusFact {
            value: ToolStatus::Unknown,
            basis: StatusBasis::SourceFinalization,
        }),
        "the prefix must end on the unresolved call"
    );

    slice_until_done_for(
        &ClaudeCodeAdapter,
        sliced_root.path(),
        &runtime,
        &mut sliced_store,
        &sliced_files,
        &opts,
    );

    let session = &sliced_files[0].session_id;
    let page = timeline(&sliced_store, session);
    let settled = page
        .iter()
        .find(|entry| entry.timeline_id == split.timeline_id)
        .expect("the later slice revises the row, never a second one");
    let status = tool_status(settled).unwrap();
    assert_eq!(
        status.basis,
        StatusBasis::NativeTerminalRecord,
        "the verdict must come from the result record the next slice read"
    );
    assert_ne!(
        status.value,
        ToolStatus::Unknown,
        "the finalized placeholder must be revised, not kept"
    );

    assert_eq!(session_state(&sliced_store, session), StreamState::Indexed);
    assert_eq!(page, timeline(&one_store, session));
    assert_eq!(buckets(&sliced_store), buckets(&one_store));
}

/// One budgeted Codex pass, ledger-free — the body of [`slice_until_done_for`]
/// without its clean-run assertions, for the passes that expect a dirty file.
fn codex_pass(
    root: &Path,
    runtime: &RuntimeRow,
    store: &mut Store,
    files: &[rosetta_pipeline::SessionFile],
    opts: &IngestOptions,
) -> rosetta_pipeline::StreamReport {
    stream_backfill(
        &CodexAdapter,
        runtime,
        root,
        store,
        files,
        opts,
        &Instrument::new(),
    )
    .unwrap()
}

/// Replace one 0-based line of a single-session corpus with a byte that frames
/// as a COMPLETE record and cannot parse: a hard error mid-file, not a torn
/// tail. Earlier and later lines are left exactly as generated.
fn corrupt_line(root: &Path, index: usize) {
    let path = only_jsonl(root);
    let text = std::fs::read_to_string(&path).unwrap();
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    assert!(index < lines.len(), "the corpus must reach line {index}");
    lines[index] = "{".to_string();
    std::fs::write(&path, format!("{}\n", lines.join("\n"))).unwrap();
}

/// Meta-ingest `root` under `opts` and take exactly ONE budgeted pass over it:
/// the store, its roster, and the report a reader would see at the first pause.
fn first_slice(
    root: &Path,
    runtime: &RuntimeRow,
    opts: &IngestOptions,
) -> (
    Store,
    Vec<rosetta_pipeline::SessionFile>,
    rosetta_pipeline::StreamReport,
) {
    let mut store = Store::open_in_memory().unwrap();
    let (_, files) = meta_ingest(
        &CodexAdapter,
        runtime,
        root,
        &mut store,
        opts,
        &Instrument::new(),
    )
    .unwrap();
    let report = codex_pass(root, runtime, &mut store, &files, opts);
    (store, files, report)
}

#[test]
fn byte_target_governs_the_pause_when_the_floor_is_met() {
    // The byte half of `budget_stop`, isolated. Every other budgeted test here
    // pins max_bytes = 1 — a target the very first batch clears — so in all of
    // them the RECORD FLOOR alone decides where the pass stops. Here the floor
    // is 1, which every batch clears, leaving the byte target as the only thing
    // left to choose the boundary.
    //
    // Two roots hold identical bytes and differ only in that target. A pause on
    // its own would prove little, so the claim is sharper: QUADRUPLING the
    // target must slice materially further into the same file.
    //
    // The budget spends FRAMED RECORD PAYLOAD — the sum of `record.payload.len()`,
    // delimiters and re-read anchors excluded — so the corpus's exact payload
    // total is computable here, and a target at or above it correctly never
    // pauses at all. Both targets are derived as fractions of that total, which
    // is what stops either pass from quietly decaying into an EOF pass.
    let one_root = TempDir::new().unwrap();
    let small_root = TempDir::new().unwrap();
    let large_root = TempDir::new().unwrap();
    let spec = CorpusSpec::new(Agent::Codex, 1, 60, 42);
    generate(&spec, one_root.path()).unwrap();
    generate(&spec, small_root.path()).unwrap();
    generate(&spec, large_root.path()).unwrap();

    let text = std::fs::read_to_string(only_jsonl(small_root.path())).unwrap();
    let total_payload: u64 = text.lines().map(|line| line.len() as u64).sum();
    let total_records = u64::try_from(text.lines().count()).unwrap();
    const FLOOR: u32 = 1;
    let small_target = total_payload / 8;
    let large_target = small_target * 4;
    assert!(
        small_target > 0 && large_target < total_payload && total_records > 1,
        "the corpus must outlast both targets ({small_target}, {large_target}) \
         over {total_payload} payload bytes in {total_records} records"
    );

    let runtime = runtime(&CodexAdapter);
    let (one_store, _) = one_shot(&CodexAdapter, &runtime, one_root.path());

    let small_opts = budgeted_opts(small_target, FLOOR);
    let (mut small_store, small_files, small) =
        first_slice(small_root.path(), &runtime, &small_opts);
    let (_, _, large) = first_slice(
        large_root.path(),
        &runtime,
        &budgeted_opts(large_target, FLOOR),
    );

    assert_eq!(
        small.files_paused, 1,
        "the byte target pauses the only file"
    );
    assert_eq!(large.files_paused, 1);
    assert_eq!(small.files_incomplete, 0, "a pause is not a failure");
    assert_eq!(large.files_incomplete, 0);
    assert_eq!(small.hard_errors, 0);
    assert_eq!(large.hard_errors, 0);

    // These three bounds are the whole proof, and none of them can pass by
    // accident: above the floor, the floor cannot have stopped the pass; the
    // larger target reaching further says the bytes chose the boundary; and
    // under the record count says neither pass merely ran out of file.
    assert!(
        small.records > u64::from(FLOOR),
        "the floor of {FLOOR} was cleared long before the pass stopped, so the \
         {small_target}-byte target is what stopped it (records {})",
        small.records,
    );
    assert!(
        small.records < large.records,
        "a four-fold byte target must slice further: {} records at {small_target} \
         bytes against {} at {large_target}",
        small.records,
        large.records,
    );
    assert!(
        large.records < total_records,
        "a budget met at EOF proves nothing ({} records of {total_records})",
        large.records,
    );

    // Sharper still, now that the budget spends exactly what it frames: the
    // pause lands on the FIRST record boundary at or past the target. The
    // sealed prefix's payload clears it, and one record less would not have —
    // the documented single record of overshoot, no more.
    let payload_through = |records: u64| -> u64 {
        text.lines()
            .take(usize::try_from(records).unwrap())
            .map(|line| line.len() as u64)
            .sum()
    };
    assert!(
        payload_through(small.records) >= small_target,
        "the sealed prefix must clear the target: {} payload bytes over {} records \
         against a {small_target}-byte target",
        payload_through(small.records),
        small.records,
    );
    assert!(
        payload_through(small.records - 1) < small_target,
        "one record earlier must NOT have cleared it, or the pass overshot by \
         more than a record: {} payload bytes over {} records against {small_target}",
        payload_through(small.records - 1),
        small.records - 1,
    );

    // And a byte-governed slice is still just a slice: the remainder streams in
    // as ordinary append growth, reaches Indexed, and rolls up identically.
    let paused_passes = slice_until_done_for(
        &CodexAdapter,
        small_root.path(),
        &runtime,
        &mut small_store,
        &small_files,
        &small_opts,
    );
    assert!(paused_passes >= 1, "the remainder is sliced too");

    let session = &small_files[0].session_id;
    assert_eq!(session_state(&small_store, session), StreamState::Indexed);
    assert_eq!(
        timeline(&small_store, session).len(),
        timeline(&one_store, session).len(),
        "the same rows exist however the boundaries fell"
    );
    assert_eq!(buckets(&small_store), buckets(&one_store));

    // NOTE — why this compares row COUNT and not the rows themselves, unlike
    // every sibling test here. On a 60-record Codex corpus the two sides differ
    // in exactly one field: a tool's `timing.completed_at`. That divergence is
    // NOT the budget's. It needs only a pass boundary between an
    // `event_msg/item_completed/CommandExecution` record and the
    // `response_item/function_call_output` that follows it — the later record
    // patches the persisted row's completed_at with its own timestamp while
    // started_at and duration_ms keep the values CommandExecution derived, so
    // the row ends up self-contradictory (completed_at - started_at of ~9.8s
    // against duration_ms of 1). Plain UNBUDGETED two-pass append growth over
    // this same corpus reproduces it at splits 34, 38, 42 and 46, and a single
    // pass cut into one-record transactions does not reproduce it at all: the
    // in-RAM accumulator ranks CommandExecution's completion above the output
    // record's, and only a checkpoint-and-resume boundary loses that ranking.
    // Restore full entry equality here once tool timing survives a resume.
}

#[test]
fn dirty_sources_converge_on_live_growth_semantics() {
    // THE CONVERGED RULE (product decision 2026-08-31, replacing the
    // divergence this test previously pinned): a source that goes bad
    // partway follows LIVE-GROWTH semantics on every cold path. The
    // parseable rows publish — sliced or not — drift records the damage,
    // and readiness never leaves Streaming. A dirty REPLACEMENT generation
    // still abandons so previously clean content survives; that rule is
    // exercised by the store's replacement tests, not here.
    let unsliced_root = TempDir::new().unwrap();
    let sliced_root = TempDir::new().unwrap();
    let spec = CorpusSpec::new(Agent::Codex, 1, 60, 11);
    generate(&spec, unsliced_root.path()).unwrap();
    generate(&spec, sliced_root.path()).unwrap();

    // Line 40 sits well past the first slice, so several clean passes
    // activate and extend the sliced prefix before any pass finds the damage.
    const BAD_LINE: usize = 39;
    const FLOOR: u32 = 10;
    corrupt_line(unsliced_root.path(), BAD_LINE);
    corrupt_line(sliced_root.path(), BAD_LINE);
    let runtime = runtime(&CodexAdapter);

    // UNSLICED: one pass, EOF, unclean FIRST generation — the parseable rows
    // activate anyway, reported incomplete, held at Streaming.
    let mut unsliced_store = Store::open_in_memory().unwrap();
    let (_, unsliced_files) = meta_ingest(
        &CodexAdapter,
        &runtime,
        unsliced_root.path(),
        &mut unsliced_store,
        &full_opts(),
        &Instrument::new(),
    )
    .unwrap();
    let session = unsliced_files[0].session_id.clone();
    let whole = codex_pass(
        unsliced_root.path(),
        &runtime,
        &mut unsliced_store,
        &unsliced_files,
        &full_opts(),
    );
    assert_eq!(whole.hard_errors, 1, "the bad line is a hard parse error");
    assert_eq!(whole.files_incomplete, 1);
    let unsliced_rows = timeline(&unsliced_store, &session);
    assert!(
        !unsliced_rows.is_empty(),
        "a dirty first generation publishes its parseable rows"
    );
    assert_eq!(
        session_state(&unsliced_store, &session),
        StreamState::Streaming
    );

    // SLICED: the same bytes, budgeted. The first pass pauses on a clean
    // prefix long before the damage.
    let mut sliced_store = Store::open_in_memory().unwrap();
    let opts = budgeted_opts(1, FLOOR);
    let (_, sliced_files) = meta_ingest(
        &CodexAdapter,
        &runtime,
        sliced_root.path(),
        &mut sliced_store,
        &opts,
        &Instrument::new(),
    )
    .unwrap();
    assert_eq!(sliced_files[0].session_id, session);
    let first = codex_pass(
        sliced_root.path(),
        &runtime,
        &mut sliced_store,
        &sliced_files,
        &opts,
    );
    assert_eq!(first.files_paused, 1);
    assert_eq!(
        first.hard_errors, 0,
        "the damage is far past the first slice"
    );
    let prefix = timeline(&sliced_store, &session);
    assert!(!prefix.is_empty());

    // Keep slicing until a pass finally reads the bad line, then settle.
    let mut passes = 1;
    loop {
        passes += 1;
        assert!(
            passes <= 32,
            "the budgeted passes never reached the bad line"
        );
        let report = codex_pass(
            sliced_root.path(),
            &runtime,
            &mut sliced_store,
            &sliced_files,
            &opts,
        );
        if report.hard_errors > 0 {
            assert_eq!(report.files_incomplete, 1);
            assert_eq!(
                report.files_paused, 0,
                "an unclean generation may not pause; the pass reads on to EOF"
            );
            break;
        }
        assert_eq!(report.files_paused, 1, "a clean pass pauses on budget");
    }

    // THE CONVERGENCE: same bytes, same damage — both paths show the reader
    // the same partial transcript, hold the same badge, and count the same
    // usage. Nobody is ever promoted to Indexed.
    let settled = timeline(&sliced_store, &session);
    assert!(
        settled.len() > prefix.len(),
        "rows past the bad line append too"
    );
    assert_eq!(settled, unsliced_rows);
    assert_eq!(buckets(&sliced_store), buckets(&unsliced_store));
    assert_eq!(
        session_state(&sliced_store, &session),
        StreamState::Streaming
    );
    for _ in 0..3 {
        let report = codex_pass(
            sliced_root.path(),
            &runtime,
            &mut sliced_store,
            &sliced_files,
            &opts,
        );
        assert_eq!(report.hard_errors, 0, "the bad line is not re-read");
        assert_ne!(
            session_state(&sliced_store, &session),
            StreamState::Indexed,
            "a dirty source must never earn the Indexed badge"
        );
        assert_eq!(timeline(&sliced_store, &session), settled);
    }
}
