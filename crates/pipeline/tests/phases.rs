//! Whole-phase integration: a bench-corpus temp dir in, a queryable
//! projection out — discovery → meta ingest → stream backfill, then the
//! adversarial follow-ups (idempotent re-run, live append, truncation).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use rosetta_adapter_claude_code::ClaudeCodeAdapter;
use rosetta_adapter_codex::CodexAdapter;
use rosetta_adapter_support::AgentAdapter;
use rosetta_bench_corpus::{Agent, CorpusSpec, generate};
use rosetta_core::rows::{RuntimeRow, StreamState};
use rosetta_pipeline::{
    IngestOptions, Instrument, discover, expand_data_root, meta_ingest, stream_backfill,
};
use rosetta_store::{Store, queries};
use tempfile::TempDir;

struct Harness {
    _home: TempDir,
    home: PathBuf,
    root: PathBuf,
    runtime: RuntimeRow,
    store: Store,
    opts: IngestOptions,
    instrument: Instrument,
}

fn harness(adapter: &dyn AgentAdapter, agent: Agent, sessions: u64, lines: u64) -> Harness {
    let home = TempDir::new().unwrap();
    let root = expand_data_root(&adapter.layout().data_root, home.path());
    std::fs::create_dir_all(&root).unwrap();
    generate(&CorpusSpec::new(agent, sessions, lines, 42), &root).unwrap();

    // Individual tests opt into the rotating audit lane explicitly; the phase
    // baseline exercises quiet incremental tails.
    let opts = IngestOptions {
        audit_files_per_run: 0,
        ..IngestOptions::default()
    };
    let instrument = Instrument::new();
    let discoveries = discover(&[adapter], home.path(), &opts, &instrument).unwrap();
    assert_eq!(discoveries.len(), 1);
    let discovery = &discoveries[0];
    assert!(discovery.installed, "generated corpus must be discovered");
    assert!(discovery.file_count > 0 && discovery.total_bytes > 0);

    let mut runtime = discovery.runtime.clone();
    runtime.enabled = true; // the test plays the consenting user

    Harness {
        home: home.path().to_path_buf(),
        _home: home,
        root: discovery.root.clone(),
        runtime,
        store: Store::open_in_memory().unwrap(),
        opts,
        instrument,
    }
}

#[test]
fn cancelled_cold_stream_stops_before_publishing_payload_rows() {
    let adapter = &ClaudeCodeAdapter;
    let mut h = harness(adapter, Agent::ClaudeCode, 2, 50);
    let (_, files) = meta_ingest(
        adapter,
        &h.runtime,
        &h.root,
        &mut h.store,
        &h.opts,
        &h.instrument,
    )
    .unwrap();
    let opts = IngestOptions {
        cancel: Some(Arc::new(AtomicBool::new(true))),
        ..h.opts.clone()
    };
    let error = stream_backfill(
        adapter,
        &h.runtime,
        &h.root,
        &mut h.store,
        &files,
        &opts,
        &h.instrument,
    )
    .unwrap_err();
    assert!(matches!(error, rosetta_pipeline::PipelineError::Cancelled));
    let messages: i64 = h
        .store
        .conn()
        .query_row("SELECT COUNT(*) FROM message", [], |row| row.get(0))
        .unwrap();
    assert_eq!(messages, 0);
}

#[test]
fn progress_observes_batches_within_a_source_without_counting_read_amplification() {
    use rosetta_pipeline::{Metric, SessionFile, progress::ColdProgressObserver};
    use std::sync::Mutex;
    use std::sync::atomic::Ordering;

    #[derive(Debug, Default)]
    struct Observer {
        batches: Mutex<Vec<(String, u64)>>,
        reading_finished: AtomicBool,
    }
    impl ColdProgressObserver for Observer {
        fn source_bytes(&self, file: &SessionFile, bytes: u64) {
            if bytes > 0 {
                self.batches
                    .lock()
                    .unwrap()
                    .push((file.relative_path.clone(), bytes));
            }
        }
        fn reading_finished(&self) {
            self.reading_finished.store(true, Ordering::Relaxed);
        }
    }
    let adapter = &ClaudeCodeAdapter;
    let mut h = harness(adapter, Agent::ClaudeCode, 1, 500);
    let (meta, files) = meta_ingest(
        adapter,
        &h.runtime,
        &h.root,
        &mut h.store,
        &h.opts,
        &h.instrument,
    )
    .unwrap();
    assert_eq!(
        h.instrument.snapshot().get(Metric::SessionsDiscovered),
        meta.sessions as u64
    );
    let observer = Arc::new(Observer::default());
    h.opts.progress = Some(observer.clone());
    h.opts.tail.max_records_per_batch = 64;
    let report = stream_backfill(
        adapter,
        &h.runtime,
        &h.root,
        &mut h.store,
        &files,
        &h.opts,
        &h.instrument,
    )
    .unwrap();
    let batches = observer.batches.lock().unwrap();
    assert!(
        batches.len() >= 7,
        "one large source needs continuous observations"
    );
    assert!(
        batches
            .iter()
            .all(|(path, bytes)| path == &files[0].relative_path && *bytes > 0)
    );
    assert_eq!(
        batches.iter().map(|(_, bytes)| bytes).sum::<u64>(),
        report.payload_bytes
    );
    assert_eq!(
        h.instrument.snapshot().get(Metric::PayloadBytes),
        report.payload_bytes
    );
    assert!(h.instrument.snapshot().get(Metric::BytesRead) > report.payload_bytes);
    assert!(observer.reading_finished.load(Ordering::Relaxed));
    let previous_count = batches.len();
    drop(batches);
    let quiet = stream_backfill(
        adapter,
        &h.runtime,
        &h.root,
        &mut h.store,
        &files,
        &h.opts,
        &h.instrument,
    )
    .unwrap();
    assert_eq!(quiet.payload_bytes, 0);
    assert_eq!(observer.batches.lock().unwrap().len(), previous_count);
}

#[test]
fn claude_code_phases_end_to_end() {
    let adapter = &ClaudeCodeAdapter;
    let mut h = harness(adapter, Agent::ClaudeCode, 8, 300);

    // Meta: every generated session appears before any transcript is read.
    let (meta, files) = meta_ingest(
        adapter,
        &h.runtime,
        &h.root,
        &mut h.store,
        &h.opts,
        &h.instrument,
    )
    .unwrap();
    assert_eq!(meta.sessions, 8);
    assert_eq!(meta.files_unidentified, 0);
    let listed = queries::session_list(h.store.conn(), None, 100).unwrap();
    assert_eq!(listed.len(), 8);
    assert!(listed.iter().all(|session| session.cwd.is_some()));
    assert!(listed.iter().all(|session| session.project_id.is_some()));
    assert!(
        listed
            .iter()
            .all(|session| session.stream_state == StreamState::Pending)
    );

    // Stream: transcripts land, FTS works, sessions flip to Indexed.
    let stream = stream_backfill(
        adapter,
        &h.runtime,
        &h.root,
        &mut h.store,
        &files,
        &h.opts,
        &h.instrument,
    )
    .unwrap();
    assert_eq!(
        h.store
            .conn()
            .query_row("SELECT COUNT(*) FROM project", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        meta.projects as i64,
        "streaming must not manufacture projects",
    );
    assert_eq!(stream.files, files.len());
    assert!(stream.parsed > 0, "corpus lines must parse");
    // The corpus's odd types (atis-latch, mode, …) are DECLARED no-ops, not
    // drift: a clean corpus must raise no alarms at all.
    assert_eq!(stream.unknown, 0);
    assert_eq!(stream.hard_errors, 0);
    assert_eq!(stream.conflicts, 0);
    assert!(stream.stats.messages_inserted > 0);
    assert_eq!(stream.sessions_indexed, 8);
    let listed = queries::session_list(h.store.conn(), None, 100).unwrap();
    assert!(
        listed
            .iter()
            .all(|session| session.stream_state == StreamState::Indexed)
    );
    let page = queries::timeline_page(h.store.conn(), &files[0].session_id, None, 10).unwrap();
    assert!(!page.is_empty());
    assert!(
        !queries::search(h.store.conn(), "synthetic", 10)
            .unwrap()
            .is_empty()
    );

    // Idempotent re-run: meta upserts, stream writes nothing at all — and the
    // meta re-skim must not regress Indexed sessions to Pending.
    let (meta_again, files_again) = meta_ingest(
        adapter,
        &h.runtime,
        &h.root,
        &mut h.store,
        &h.opts,
        &h.instrument,
    )
    .unwrap();
    assert_eq!(meta_again.sessions, 8);
    let quiet = stream_backfill(
        adapter,
        &h.runtime,
        &h.root,
        &mut h.store,
        &files_again,
        &h.opts,
        &h.instrument,
    )
    .unwrap();
    assert_eq!(quiet.batches, 0, "a quiet file must not produce write txs");
    assert_eq!(quiet.stats.messages_inserted, 0);
    assert_eq!(quiet.generations_restarted, 0);
    assert_eq!(quiet.sessions_indexed, 0);

    // Live append: only the tail is framed and ingested, and it is
    // searchable. The undeclared type must land as aggregated drift.
    let target = &files_again[0];
    let target_path = h.root.join(&target.relative_path);
    append_user_line(
        &target_path,
        &target.native_session_id,
        "quetzalcoatl appended fixture line",
    );
    append_raw_line(
        &target_path,
        &serde_json::json!({
            "type": "zzz-from-the-future",
            "sessionId": target.native_session_id,
        }),
    );
    let appended = stream_backfill(
        adapter,
        &h.runtime,
        &h.root,
        &mut h.store,
        &files_again,
        &h.opts,
        &h.instrument,
    )
    .unwrap();
    assert_eq!(appended.records, 2, "only the appended records are re-read");
    assert_eq!(appended.stats.messages_inserted, 1);
    assert_eq!(appended.unknown, 1);
    assert_eq!(appended.generations_restarted, 0);
    let hits = queries::search(h.store.conn(), "quetzalcoatl", 5).unwrap();
    assert_eq!(hits.len(), 1);
    let drift_rows: i64 = h
        .store
        .conn()
        .query_row("SELECT COUNT(*) FROM drift_agg", [], |row| row.get(0))
        .unwrap();
    assert!(drift_rows > 0, "an undeclared type must aggregate as drift");

    // Truncation: the file shrinks → a staged replacement generation. Removed
    // rows are tombstoned when the clean generation atomically activates.
    let victim = &files_again[1];
    let before: i64 = h
        .store
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM message WHERE session_id = ?1",
            [&victim.session_id],
            |row| row.get(0),
        )
        .unwrap();
    let path = h.root.join(&victim.relative_path);
    let text = std::fs::read_to_string(&path).unwrap();
    let kept: Vec<&str> = text.lines().take(3).collect();
    std::fs::write(&path, format!("{}\n", kept.join("\n"))).unwrap();
    let truncated = stream_backfill(
        adapter,
        &h.runtime,
        &h.root,
        &mut h.store,
        &files_again,
        &h.opts,
        &h.instrument,
    )
    .unwrap();
    assert_eq!(truncated.generations_restarted, 1);
    let after: i64 = h
        .store
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM message WHERE session_id = ?1",
            [&victim.session_id],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        after < before,
        "rows absent from the replacement must be removed"
    );

    // The instrument saw the whole run.
    assert!(h.instrument.get(rosetta_pipeline::Metric::BytesRead) > 0);
    assert!(h.instrument.report().contains("records framed"));
    let _ = h.home;
}

#[test]
fn codex_phases_end_to_end() {
    let adapter = &CodexAdapter;
    let mut h = harness(adapter, Agent::Codex, 6, 240);

    let (meta, files) = meta_ingest(
        adapter,
        &h.runtime,
        &h.root,
        &mut h.store,
        &h.opts,
        &h.instrument,
    )
    .unwrap();
    assert_eq!(meta.sessions, 6);
    assert_eq!(
        queries::session_list(h.store.conn(), None, 100)
            .unwrap()
            .len(),
        6
    );

    let stream = stream_backfill(
        adapter,
        &h.runtime,
        &h.root,
        &mut h.store,
        &files,
        &h.opts,
        &h.instrument,
    )
    .unwrap();
    assert!(stream.parsed > 0);
    assert_eq!(stream.hard_errors, 0);
    assert!(stream.stats.messages_inserted > 0);
    assert_eq!(stream.sessions_indexed, 6);

    // Codex reports cumulative token counters — the replacement-aware fold
    // must land them as bucket rows, exactly once.
    let buckets: i64 = h
        .store
        .conn()
        .query_row("SELECT COUNT(*) FROM usage_bucket", [], |row| row.get(0))
        .unwrap();
    assert!(buckets > 0, "codex usage must reach the buckets");

    // Idempotent re-run stays quiet.
    let quiet = stream_backfill(
        adapter,
        &h.runtime,
        &h.root,
        &mut h.store,
        &files,
        &h.opts,
        &h.instrument,
    )
    .unwrap();
    assert_eq!(quiet.batches, 0);
    assert_eq!(quiet.stats.messages_inserted, 0);
}

/// Append one minimal-but-valid claude-code user record. Shape mirrors the
/// generated corpus lines (permissive raw types tolerate the missing rest).
fn append_user_line(path: &Path, native_session_id: &str, content: &str) {
    let line = serde_json::json!({
        "type": "user",
        "sessionId": native_session_id,
        "uuid": "00000000-0000-4000-8000-00000000a99e",
        "parentUuid": null,
        "isSidechain": false,
        "userType": "external",
        "timestamp": "2026-05-01T00:00:00.000Z",
        "cwd": "/tmp/bench/project-0",
        "gitBranch": "main",
        "version": "2.1.240",
        "message": {"role": "user", "content": content},
    });
    append_raw_line(path, &line);
}

fn append_raw_line(path: &Path, line: &serde_json::Value) {
    let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    writeln!(file, "{line}").unwrap();
}
