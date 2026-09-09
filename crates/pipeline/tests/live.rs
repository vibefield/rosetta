//! Live-lane integration: scripted event sources drive ticks against a
//! bench-corpus temp dir, proving the recovery rules — hints are free,
//! polling is the fallback, admission is idempotent meta, and every write
//! goes through the same generation-safe stream phase as backfill.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rosetta_adapter_claude_code::ClaudeCodeAdapter;
use rosetta_adapter_support::AgentAdapter;
use rosetta_bench_corpus::{Agent, CorpusSpec, generate};
use rosetta_capture::{ScriptedEvents, SourceEvent, SourceEvents};
use rosetta_core::rows::RuntimeRow;
use rosetta_pipeline::{
    IngestOptions, Instrument, LiveOptions, LiveRunner, PipeCheckpoint, discover, expand_data_root,
    meta_ingest, stream_backfill,
};
use rosetta_store::{Store, queries};
use tempfile::TempDir;

struct Harness {
    _home: TempDir,
    root: PathBuf,
    runtime: RuntimeRow,
    store: Store,
    opts: IngestOptions,
    instrument: Instrument,
    files: Vec<rosetta_pipeline::SessionFile>,
}

/// Corpus generated, meta + backfill complete — the state a `watch` command
/// reaches before its first live tick.
fn backfilled_harness() -> Harness {
    let adapter = &ClaudeCodeAdapter;
    let home = TempDir::new().unwrap();
    let root = expand_data_root(&adapter.layout().data_root, home.path());
    std::fs::create_dir_all(&root).unwrap();
    generate(&CorpusSpec::new(Agent::ClaudeCode, 4, 120, 42), &root).unwrap();

    let opts = IngestOptions::default();
    let instrument = Instrument::new();
    let discoveries = discover(&[adapter], home.path(), &opts, &instrument).unwrap();
    let mut runtime = discoveries[0].runtime.clone();
    runtime.enabled = true;

    let mut store = Store::open_in_memory().unwrap();
    let (_, files) = meta_ingest(adapter, &runtime, &root, &mut store, &opts, &instrument).unwrap();
    stream_backfill(
        adapter,
        &runtime,
        &root,
        &mut store,
        &files,
        &opts,
        &instrument,
    )
    .unwrap();

    Harness {
        _home: home,
        root,
        runtime,
        store,
        opts,
        instrument,
        files,
    }
}

/// A live tick loop should never poll unless asked: quiet watcher, huge
/// poll interval.
fn hot_live() -> LiveOptions {
    LiveOptions {
        tick: Duration::ZERO,
        poll_interval: Duration::from_secs(3600),
        max_coalesced: 16,
    }
}

#[test]
fn rescanning_fresh_file_dates_does_not_change_recorded_conversation_activity() {
    let mut h = backfilled_harness();
    let before: Vec<_> = queries::all_sessions(h.store.conn()).unwrap().into_iter()
        .map(|row| (row.id, row.activity)).collect();
    assert!(before.iter().any(|(_, activity)| activity.is_some()));
    for source in &h.files {
        let path = h.root.join(&source.relative_path);
        let file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        file.set_times(std::fs::FileTimes::new().set_modified(
            std::time::UNIX_EPOCH + Duration::from_secs(2_000_000_000)
        )).unwrap();
    }
    meta_ingest(&ClaudeCodeAdapter, &h.runtime, &h.root, &mut h.store, &h.opts, &h.instrument).unwrap();
    let after: Vec<_> = queries::all_sessions(h.store.conn()).unwrap().into_iter()
        .map(|row| (row.id, row.activity)).collect();
    assert_eq!(after, before, "file freshness must never become conversation recency");

    let target = h.files[0].clone();
    append_user_line(&h.root.join(&target.relative_path), &target.native_session_id, "new activity");
    let mut events = ScriptedEvents::default();
    events.push_changed(&target.relative_path);
    let mut runner = resumed_runner(&h, &ClaudeCodeAdapter, events, hot_live());
    runner.tick(&mut h.store, &h.instrument).unwrap();
    let rows = queries::timeline_page(h.store.conn(), &target.session_id, None, 10_000).unwrap();
    let message_at = rows.iter().map(|row| row.at).max().flatten().unwrap();
    assert_eq!(queries::session(h.store.conn(), &target.session_id).unwrap().unwrap()
        .activity.unwrap().last_at, message_at);
}

fn runner<'a>(
    h: &mut Harness,
    adapter: &'a ClaudeCodeAdapter,
    events: ScriptedEvents,
    live: LiveOptions,
) -> LiveRunner<'a> {
    LiveRunner::start(
        adapter,
        h.runtime.clone(),
        h.root.clone(),
        Box::new(events),
        &mut h.store,
        h.opts.clone(),
        live,
        &h.instrument,
    )
    .unwrap()
}

fn resumed_runner<'a>(
    h: &Harness,
    adapter: &'a ClaudeCodeAdapter,
    events: ScriptedEvents,
    live: LiveOptions,
) -> LiveRunner<'a> {
    LiveRunner::resume_from_roster(
        adapter,
        h.runtime.clone(),
        h.root.clone(),
        Box::new(events),
        h.files.clone(),
        h.opts.clone(),
        live,
        None,
    )
}

#[test]
fn ready_pass_reaches_the_last_runtime_without_waiting_on_quiet_ones() {
    struct ReadyOnly(ScriptedEvents);
    impl SourceEvents for ReadyOnly {
        fn next_event(&mut self, timeout: Duration) -> Option<SourceEvent> {
            assert_eq!(timeout, Duration::ZERO, "shared passes must never wait");
            self.0.next_event(timeout)
        }
    }

    let mut h = backfilled_harness();
    let target = h.files[0].clone();
    append_user_line(
        &h.root.join(&target.relative_path),
        &target.native_session_id,
        "ready after eight quiet runtimes",
    );
    let mut runners = (0..9)
        .map(|index| {
            let mut events = ScriptedEvents::default();
            if index == 8 {
                events.push_changed(&target.relative_path);
            }
            LiveRunner::resume_from_roster(
                &ClaudeCodeAdapter,
                h.runtime.clone(),
                h.root.clone(),
                Box::new(ReadyOnly(events)),
                if index == 8 { h.files.clone() } else { vec![] },
                h.opts.clone(),
                LiveOptions {
                    tick: Duration::from_secs(60),
                    ..hot_live()
                },
                None,
            )
        })
        .collect::<Vec<_>>();
    for runner in &mut runners {
        assert!(!runner.tick_ready(&mut h.store, &h.instrument).unwrap());
    }
    assert_eq!(runners[8].report().stream.stats.messages_inserted, 1);
    assert_eq!(runners[8].report().polls, 0);
}

#[test]
fn exhausted_ready_budget_requests_another_pass_until_the_queue_is_empty() {
    let mut h = backfilled_harness();
    let mut events = ScriptedEvents::default();
    for file in &h.files[..2] {
        append_user_line(
            &h.root.join(&file.relative_path),
            &file.native_session_id,
            "bounded ready drain",
        );
        events.push_changed(&file.relative_path);
    }
    let mut runner = resumed_runner(
        &h,
        &ClaudeCodeAdapter,
        events,
        LiveOptions {
            max_coalesced: 1,
            ..hot_live()
        },
    );
    assert!(runner.tick_ready(&mut h.store, &h.instrument).unwrap());
    assert_eq!(runner.report().stream.stats.messages_inserted, 1);
    assert!(runner.tick_ready(&mut h.store, &h.instrument).unwrap());
    assert_eq!(runner.report().stream.stats.messages_inserted, 2);
    assert!(!runner.tick_ready(&mut h.store, &h.instrument).unwrap());
    assert_eq!(runner.report().events, 2);
}

#[test]
fn resume_from_backfill_roster_preserves_queued_hints_without_reskimming() {
    let adapter = &ClaudeCodeAdapter;
    let mut h = backfilled_harness();
    let target = h.files[0].clone();
    append_user_line(
        &h.root.join(&target.relative_path),
        &target.native_session_id,
        "quetzalcoatl queued during transition",
    );
    let mut events = ScriptedEvents::default();
    events.push_changed(&target.relative_path);
    let skimmed_before = h
        .instrument
        .snapshot()
        .get(rosetta_pipeline::Metric::FilesSkimmed);

    let mut runner = resumed_runner(&h, adapter, events, hot_live());
    assert_eq!(
        h.instrument
            .snapshot()
            .get(rosetta_pipeline::Metric::FilesSkimmed),
        skimmed_before,
        "resuming a fresh roster must not repeat meta ingest"
    );
    runner.tick(&mut h.store, &h.instrument).unwrap();
    assert_eq!(runner.report().stream.stats.messages_inserted, 1);
}

#[test]
fn containing_directory_hint_nominates_the_changed_source() {
    let adapter = &ClaudeCodeAdapter;
    let mut h = backfilled_harness();
    let target = h.files[0].clone();
    append_user_line(
        &h.root.join(&target.relative_path),
        &target.native_session_id,
        "xipe totec directory-level hint",
    );
    let directory = Path::new(&target.relative_path)
        .parent()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let mut events = ScriptedEvents::default();
    events.push_changed(&directory);

    let mut runner = resumed_runner(&h, adapter, events, hot_live());
    runner.tick(&mut h.store, &h.instrument).unwrap();
    assert_eq!(runner.report().stream.stats.messages_inserted, 1);
}

#[test]
fn startup_catch_up_forces_one_silent_cursor_relative_poll() {
    let adapter = &ClaudeCodeAdapter;
    let mut h = backfilled_harness();
    let target = h.files[0].clone();
    append_user_line(
        &h.root.join(&target.relative_path),
        &target.native_session_id,
        "tlaloc offline append",
    );
    let mut runner = resumed_runner(&h, adapter, ScriptedEvents::default(), hot_live());

    runner.catch_up(&mut h.store, &h.instrument).unwrap();
    assert_eq!(runner.report().polls, 1);
    assert_eq!(runner.report().stream.stats.messages_inserted, 1);
}

#[test]
fn startup_catch_up_replays_a_stale_normalizer_projection() {
    let adapter = &ClaudeCodeAdapter;
    let mut h = backfilled_harness();
    let target = h.files[0].clone();
    let before = queries::cursor_state(h.store.conn(), &h.runtime.id, &target.relative_path)
        .unwrap()
        .unwrap();
    let mut checkpoint = PipeCheckpoint::decode(&before.checkpoint).unwrap();
    checkpoint.normalizer_version = adapter.normalizer_version() - 1;
    h.store
        .conn()
        .execute(
            "UPDATE ingest_cursor SET checkpoint = ?1 WHERE id = ?2",
            rusqlite::params![checkpoint.encode(), before.id],
        )
        .unwrap();

    let mut runner = resumed_runner(&h, adapter, ScriptedEvents::default(), hot_live());
    runner.catch_up(&mut h.store, &h.instrument).unwrap();

    let after = queries::cursor_state(h.store.conn(), &h.runtime.id, &target.relative_path)
        .unwrap()
        .unwrap();
    let checkpoint = PipeCheckpoint::decode(&after.checkpoint).unwrap();
    assert_eq!(checkpoint.normalizer_version, adapter.normalizer_version());
    assert!(after.active_generation > before.active_generation);
    assert_eq!(runner.report().stream.files, 1);
}

#[test]
fn event_hint_streams_only_the_appended_tail() {
    let adapter = &ClaudeCodeAdapter;
    let mut h = backfilled_harness();
    let target = h.files[0].clone();

    append_user_line(
        &h.root.join(&target.relative_path),
        &target.native_session_id,
        "xochipilli live append",
    );
    let mut events = ScriptedEvents::default();
    events.push_changed(&target.relative_path);

    let mut runner = runner(&mut h, adapter, events, hot_live());
    runner.tick(&mut h.store, &h.instrument).unwrap();

    let report = runner.report();
    assert_eq!(report.events, 1);
    assert_eq!(report.polls, 0, "a hint tick must not stat-scan the world");
    assert_eq!(report.stream.records, 1, "only the appended record is read");
    assert_eq!(report.stream.stats.messages_inserted, 1);
    assert_eq!(
        queries::search(h.store.conn(), "xochipilli", 5)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn hint_without_change_writes_nothing() {
    let adapter = &ClaudeCodeAdapter;
    let mut h = backfilled_harness();
    let target = h.files[0].relative_path.clone();

    let mut events = ScriptedEvents::default();
    events.push_changed(&target);
    let mut runner = runner(&mut h, adapter, events, hot_live());
    runner.tick(&mut h.store, &h.instrument).unwrap();

    let report = runner.report();
    assert_eq!(report.events, 1);
    assert_eq!(report.stream.batches, 0, "hints are free: no write tx");
    assert_eq!(report.stream.stats.messages_inserted, 0);
}

#[test]
fn unclaimed_hints_are_ignored_noise() {
    let adapter = &ClaudeCodeAdapter;
    let mut h = backfilled_harness();

    let mut events = ScriptedEvents::default();
    events.push_changed("shell-snapshots/zsh-123"); // matched stream, no driver
    events.push_changed("statsig/some-cache-file"); // no stream at all
    let mut runner = runner(&mut h, adapter, events, hot_live());
    runner.tick(&mut h.store, &h.instrument).unwrap();

    let report = runner.report();
    assert_eq!(report.events, 2);
    assert_eq!(report.hints_ignored, 2);
    assert_eq!(report.admissions, 0);
    assert_eq!(report.stream.batches, 0);
}

#[test]
fn poll_fallback_catches_silent_appends() {
    let adapter = &ClaudeCodeAdapter;
    let mut h = backfilled_harness();
    let target = h.files[0].clone();

    append_user_line(
        &h.root.join(&target.relative_path),
        &target.native_session_id,
        "tezcatlipoca silent append",
    );
    // No events at all — only the poll can see it. Audit off: this test
    // isolates the poll's cursor-vs-size catch-up signal.
    h.opts.audit_files_per_run = 0;
    let live = LiveOptions {
        tick: Duration::ZERO,
        poll_interval: Duration::ZERO,
        max_coalesced: 16,
    };
    let mut runner = runner(&mut h, adapter, ScriptedEvents::default(), live);
    runner.tick_ready(&mut h.store, &h.instrument).unwrap();

    let report = runner.report();
    assert_eq!(report.events, 0);
    assert_eq!(report.polls, 1);
    assert_eq!(report.stream.stats.messages_inserted, 1);
    assert_eq!(
        queries::search(h.store.conn(), "tezcatlipoca", 5)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn rescan_hint_forces_a_poll() {
    let adapter = &ClaudeCodeAdapter;
    let mut h = backfilled_harness();
    let target = h.files[0].clone();

    append_user_line(
        &h.root.join(&target.relative_path),
        &target.native_session_id,
        "huitzilopochtli rescan append",
    );
    let mut events = ScriptedEvents::default();
    events.push_rescan();
    h.opts.audit_files_per_run = 0;
    // Poll interval is huge: only the rescan hint can trigger the poll.
    let mut runner = runner(&mut h, adapter, events, hot_live());
    runner.tick(&mut h.store, &h.instrument).unwrap();

    let report = runner.report();
    assert_eq!(report.rescans, 1);
    assert_eq!(report.polls, 1);
    assert_eq!(report.stream.stats.messages_inserted, 1);
}

#[test]
fn duplicate_hints_coalesce_into_one_stream_pass() {
    let adapter = &ClaudeCodeAdapter;
    let mut h = backfilled_harness();
    let target = h.files[0].clone();

    append_user_line(
        &h.root.join(&target.relative_path),
        &target.native_session_id,
        "coatlicue coalesced append",
    );
    let mut events = ScriptedEvents::default();
    for _ in 0..5 {
        events.push_changed(&target.relative_path);
    }
    let mut runner = runner(&mut h, adapter, events, hot_live());
    runner.tick(&mut h.store, &h.instrument).unwrap();

    let report = runner.report();
    assert_eq!(report.events, 5);
    assert_eq!(report.stream.files, 1, "five hints, one ingest pass");
    assert_eq!(report.stream.stats.messages_inserted, 1);
}

#[test]
fn new_session_file_is_admitted_live() {
    let adapter = &ClaudeCodeAdapter;
    let mut h = backfilled_harness();
    let sessions_before = queries::session_list(h.store.conn(), None, 100)
        .unwrap()
        .len();

    let native = "0f000000-1111-4222-8333-444455556666";
    let relative = format!("projects/-tmp-bench-live/{native}.jsonl");

    // The script is loaded up front, but the file appears only AFTER the
    // runner's initial scan — the admission path, not the startup path.
    let mut events = ScriptedEvents::default();
    events.push_changed(&relative);
    let mut runner = runner(&mut h, adapter, events, hot_live());

    let path = h.root.join(&relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::File::create(&path).unwrap();
    append_user_line(&path, native, "yacatecuhtli fresh session");
    runner.tick(&mut h.store, &h.instrument).unwrap();

    let report = runner.report();
    assert_eq!(report.admissions, 1);
    assert_eq!(report.stream.stats.messages_inserted, 1);
    let sessions_after = queries::session_list(h.store.conn(), None, 100)
        .unwrap()
        .len();
    assert_eq!(sessions_after, sessions_before + 1);
    assert_eq!(
        queries::search(h.store.conn(), "yacatecuhtli", 5)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn poll_ticks_carry_the_rotating_audit_sweep() {
    let adapter = &ClaudeCodeAdapter;
    let mut h = backfilled_harness();

    // Quiet root, poll every tick, audit one file per poll.
    let live = LiveOptions {
        tick: Duration::ZERO,
        poll_interval: Duration::ZERO,
        max_coalesced: 16,
    };
    let mut runner = runner(&mut h, adapter, ScriptedEvents::default(), live);
    runner.tick_ready(&mut h.store, &h.instrument).unwrap();
    runner.tick(&mut h.store, &h.instrument).unwrap();

    let report = runner.report();
    assert_eq!(report.polls, 2);
    assert_eq!(
        report.stream.files_audited, 2,
        "each poll tick advances the rotating sweep by audit_files_per_run"
    );
    // A verified audit leaves rows exactly as they were — never duplicates.
    let per_session_max: i64 = h
        .store
        .conn()
        .query_row(
            "SELECT MAX(cnt) FROM (SELECT COUNT(*) AS cnt FROM message GROUP BY session_id, ordinal)",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(per_session_max, 1, "no (session, ordinal) may double up");
}

/// Mirrors the corpus writer's user record: all canonical-required fields.
fn append_user_line(path: &Path, native_session_id: &str, content: &str) {
    let line = serde_json::json!({
        "type": "user",
        "sessionId": native_session_id,
        "uuid": format!("00000000-0000-4000-8000-{:012x}", content.len()),
        "parentUuid": null,
        "isSidechain": false,
        "userType": "external",
        "timestamp": "2026-05-01T00:00:00.000Z",
        "cwd": "/tmp/bench/project-0",
        "gitBranch": "main",
        "version": "2.1.240",
        "message": {"role": "user", "content": content},
    });
    let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    writeln!(file, "{line}").unwrap();
}

/// Total bytes across a raw-ledger directory — the growth probe.
fn ledger_bytes(dir: &Path) -> u64 {
    fn walk(dir: &Path, total: &mut u64) {
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, total);
            } else {
                *total += entry.metadata().map(|m| m.len()).unwrap_or(0);
            }
        }
    }
    let mut total = 0;
    walk(dir, &mut total);
    total
}

#[test]
fn audits_verify_quiet_files_without_ledger_growth_or_generation_churn() {
    let adapter = &ClaudeCodeAdapter;
    let mut h = backfilled_harness();
    let ledger_dir = TempDir::new().unwrap();
    let ledger = rosetta_capture::Ledger::open(ledger_dir.path()).unwrap();

    let live = LiveOptions {
        tick: Duration::ZERO,
        poll_interval: Duration::ZERO,
        max_coalesced: 16,
    };
    let mut runner = LiveRunner::start_with_ledger(
        adapter,
        h.runtime.clone(),
        h.root.clone(),
        Box::new(ScriptedEvents::default()),
        &mut h.store,
        h.opts.clone(),
        live,
        Some(ledger),
        &h.instrument,
    )
    .unwrap();

    // First tick pays the one-shot recovery admissions; measure after it.
    runner.tick(&mut h.store, &h.instrument).unwrap();
    let after_first = ledger_bytes(ledger_dir.path());
    for _ in 0..4 {
        runner.tick(&mut h.store, &h.instrument).unwrap();
    }
    let report = runner.report();
    assert_eq!(report.polls, 5);
    assert_eq!(
        report.stream.files_audited, 5,
        "the rotating sweep keeps advancing"
    );
    assert_eq!(
        report.stream.generations_restarted, 0,
        "a verified quiet file never opens a replacement generation"
    );
    assert_eq!(
        ledger_bytes(ledger_dir.path()),
        after_first,
        "audit rotations append nothing to the append-only ledger"
    );
}
