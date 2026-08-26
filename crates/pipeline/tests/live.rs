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
use rosetta_capture::ScriptedEvents;
use rosetta_core::rows::RuntimeRow;
use rosetta_pipeline::{
    IngestOptions, Instrument, LiveOptions, LiveRunner, discover, expand_data_root, meta_ingest,
    stream_backfill,
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
    runner.tick(&mut h.store, &h.instrument).unwrap();

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
    runner.tick(&mut h.store, &h.instrument).unwrap();
    runner.tick(&mut h.store, &h.instrument).unwrap();

    let report = runner.report();
    assert_eq!(report.polls, 2);
    assert_eq!(
        report.stream.files_audited, 2,
        "each poll tick advances the rotating sweep by audit_files_per_run"
    );
    // Audited replays go through the generation swap — never duplicates.
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
