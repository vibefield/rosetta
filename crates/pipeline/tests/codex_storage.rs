//! Storage protocol regressions use synthetic native files only.
use rosetta_adapter_codex::CodexAdapter;
use rosetta_capture::{Ledger, scan_ledger};
use rosetta_core::{ids, rows::RuntimeRow};
use rosetta_pipeline::{
    IngestOptions, Instrument, meta_ingest_into, rebuild_runtime, stream_backfill_into,
};
use rosetta_store::{Store, queries};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use tempfile::TempDir;

struct World {
    _dir: TempDir,
    root: PathBuf,
    archive: PathBuf,
    runtime: RuntimeRow,
    store: Store,
}
impl World {
    fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("native");
        let archive = dir.path().join("ledger");
        std::fs::create_dir_all(&root).unwrap();
        let runtime = RuntimeRow {
            id: ids::runtime_id("codex", "~/.codex"),
            kind: "codex".into(),
            data_root: "~/.codex".into(),
            enabled: true,
            account: None,
            config: None,
            skills: None,
            versions_seen: None,
            discovered_at: 0,
            label: None,
            provenance: Default::default(),
        };
        Self {
            _dir: dir,
            root,
            archive,
            runtime,
            store: Store::open_in_memory().unwrap(),
        }
    }
    fn write(&self, id: &str, bytes: &[u8], compressed: bool, archived: bool) -> PathBuf {
        let filename = format!("rollout-2026-11-01T12-00-00-{id}.jsonl");
        let path = self
            .root
            .join(if archived {
                "archived_sessions"
            } else {
                "sessions/2026/11/01"
            })
            .join(if compressed {
                format!("{filename}.zst")
            } else {
                filename
            });
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            if compressed {
                zstd::encode_all(bytes, 3).unwrap()
            } else {
                bytes.to_vec()
            },
        )
        .unwrap();
        path
    }
    fn select(&self, id: &str, path: &Path) {
        let db = rusqlite::Connection::open(self.root.join("state_5.sqlite")).unwrap();
        db.execute_batch("CREATE TABLE IF NOT EXISTS threads (id TEXT PRIMARY KEY, rollout_path TEXT, cwd TEXT, cli_version TEXT, created_at INTEGER, updated_at INTEGER, history_mode TEXT);").unwrap();
        let path = path.to_string_lossy();
        let path = path.strip_suffix(".zst").unwrap_or(&path);
        db.execute("INSERT OR REPLACE INTO threads VALUES (?1,?2,'/test/project','0.167.2',1,2,'paginated')", rusqlite::params![id, path]).unwrap();
    }
    fn ingest(&mut self) -> rosetta_pipeline::StreamReport {
        let opts = IngestOptions {
            audit_files_per_run: 0,
            ..Default::default()
        };
        let instrument = Instrument::new();
        let mut ledger = Ledger::open(&self.archive).unwrap();
        let (_, files) = meta_ingest_into(
            &CodexAdapter,
            &self.runtime,
            &self.root,
            &mut self.store,
            &opts,
            &instrument,
            Some(&mut ledger),
        )
        .unwrap();
        let report = stream_backfill_into(
            &CodexAdapter,
            &self.runtime,
            &self.root,
            &mut self.store,
            &files,
            &opts,
            &instrument,
            Some(&mut ledger),
        )
        .unwrap();
        ledger.flush().unwrap();
        report
    }
    fn texts(&self, native: &str) -> Vec<String> {
        self.store
            .conn()
            .prepare("SELECT content FROM message WHERE session_id=?1 ORDER BY ordinal")
            .unwrap()
            .query_map([ids::session_id(&self.runtime.id, native)], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }
    fn indexed(&self, native: &str) -> bool {
        queries::session(
            self.store.conn(),
            &ids::session_id(&self.runtime.id, native),
        )
        .unwrap()
        .is_some_and(|s| s.stream_state == rosetta_core::rows::StreamState::Indexed)
    }
    fn rebuild(&mut self) {
        std::fs::remove_dir_all(&self.root).unwrap();
        self.store = Store::open_in_memory().unwrap();
        let report = rebuild_runtime(
            &CodexAdapter,
            &self.runtime,
            &self.archive,
            &mut self.store,
            &Instrument::new(),
        )
        .unwrap();
        assert_eq!(report.hard_errors, 0);
    }
    fn raw_count(&self) -> usize {
        scan_ledger(&self.archive)
            .unwrap()
            .entries
            .iter()
            .filter(|e| matches!(e, rosetta_capture::LedgerEntry::Raw { .. }))
            .count()
    }
}
fn line(v: Value) -> Vec<u8> {
    let mut b = serde_json::to_vec(&v).unwrap();
    b.push(b'\n');
    b
}
fn header(id: &str, base: Option<(&str, u64, u64)>) -> Vec<u8> {
    let mut payload = json!({"id":id,"timestamp":"2026-11-01T12:00:00Z","cwd":"/test/project","originator":"codex_cli","cli_version":"0.167.2","source":"cli","model_provider":"openai","history_mode":"paginated"});
    if let Some((parent, end, offset)) = base {
        payload["history_base"] =
            json!({"thread_id":parent,"end_ordinal_exclusive":end,"end_byte_offset":offset});
    }
    line(
        json!({"timestamp":"2026-11-01T12:00:00Z","ordinal":base.map_or(0, |b| b.1),"type":"session_meta","payload":payload}),
    )
}
fn message(ordinal: u64, text: &str) -> Vec<u8> {
    line(
        json!({"timestamp":"2026-11-01T12:00:01Z","ordinal":ordinal,"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":text}]}}),
    )
}
fn transcript(id: &str) -> Vec<u8> {
    [header(id, None), message(1, "first"), message(2, "second")].concat()
}

#[test]
fn compressed_archived_restore_append_and_offline_rebuild() {
    let mut w = World::new();
    let bytes = transcript("one");
    let cold = w.write("one", &bytes, true, true);
    w.select("one", &cold);
    assert_eq!(w.ingest().hard_errors, 0);
    assert_eq!(w.texts("one"), ["first", "second"]);
    assert!(w.indexed("one"));
    let count = w.raw_count();
    assert_eq!(w.ingest().records, 0);
    assert_eq!(w.raw_count(), count);
    let hot = w.write("one", &bytes, false, false);
    std::fs::remove_file(cold).unwrap();
    w.select("one", &hot);
    let restored = w.ingest();
    assert_eq!(
        restored.generations_restarted, 1,
        "only the changed DB selector is a replacement"
    );
    assert_eq!(
        restored.records, 1,
        "only changed native DB selection is captured"
    );
    let appended = [bytes, message(3, "third")].concat();
    std::fs::write(hot, appended).unwrap();
    assert_eq!(w.ingest().generations_restarted, 0);
    assert_eq!(w.texts("one"), ["first", "second", "third"]);
    w.rebuild();
    assert_eq!(w.texts("one"), ["first", "second", "third"]);
}

#[test]
fn both_representations_prefer_plain_and_ignore_temporary_files() {
    let mut w = World::new();
    w.write("one", &transcript("one"), true, false);
    let hot = w.write(
        "one",
        &[transcript("one"), message(3, "plain wins")].concat(),
        false,
        false,
    );
    std::fs::write(hot.with_extension("jsonl.zst.tmp"), b"not a transcript").unwrap();
    w.ingest();
    assert_eq!(w.texts("one"), ["first", "second", "plain wins"]);
}

#[test]
fn nested_history_cuts_do_not_grow_with_parents() {
    let mut w = World::new();
    let parent = transcript("parent");
    let first_cut = (header("parent", None).len() + message(1, "first").len()) as u64;
    w.write("parent", &parent, true, false);
    let child = [
        header("child", Some(("parent", 2, first_cut))),
        message(3, "child own"),
    ]
    .concat();
    w.write("child", &child, false, true);
    let grandchild = [
        header("grandchild", Some(("child", 4, child.len() as u64))),
        message(5, "grandchild own"),
    ]
    .concat();
    w.write("grandchild", &grandchild, true, false);
    assert_eq!(w.ingest().hard_errors, 0);
    assert_eq!(w.texts("child"), ["first", "child own"]);
    assert_eq!(
        w.texts("grandchild"),
        ["first", "child own", "grandchild own"]
    );
    w.write(
        "parent",
        &[parent, message(3, "later parent")].concat(),
        true,
        false,
    );
    w.ingest();
    assert_eq!(w.texts("child"), ["first", "child own"]);
    w.rebuild();
    assert_eq!(
        w.texts("grandchild"),
        ["first", "child own", "grandchild own"]
    );
}

#[test]
fn revert_selects_one_branch_and_replaces_the_old_projection() {
    let mut w = World::new();
    let original = transcript("one");
    let old = w.write("one", &original, false, false);
    w.select("one", &old);
    w.ingest();
    let cut = (header("one", None).len() + message(1, "first").len()) as u64;
    let reverted = [
        header("one", Some(("one", 2, cut))),
        message(3, "new branch"),
    ]
    .concat();
    let new = w.write("one_reverted", &reverted, true, false);
    w.select("one", &new);
    w.ingest();
    assert_eq!(w.texts("one"), ["first", "new branch"]);
    w.rebuild();
    assert_eq!(w.texts("one"), ["first", "new branch"]);
}

#[test]
fn missing_selected_rollout_does_not_resurrect_an_old_branch() {
    let mut w = World::new();
    w.write("one", &transcript("one"), false, false);
    let missing = w
        .root
        .join("sessions/2026/11/01/rollout-2026-11-01T12-00-00-one_missing.jsonl");
    w.select("one", &missing);
    w.ingest();
    assert!(w.texts("one").is_empty());
    assert!(!w.indexed("one"));
}

#[test]
fn missing_base_and_cycles_remain_incomplete() {
    let mut w = World::new();
    w.write(
        "one",
        &[
            header("one", Some(("missing", 1, 999))),
            message(2, "must wait"),
        ]
        .concat(),
        false,
        false,
    );
    w.ingest();
    assert!(w.texts("one").is_empty());
    assert!(!w.indexed("one"));
    w.write("one", &header("one", Some(("one", 1, 999))), false, false);
    w.ingest();
    assert!(w.texts("one").is_empty());
    assert!(!w.indexed("one"));
}

#[test]
fn corrupted_replacement_keeps_the_last_complete_projection() {
    let mut w = World::new();
    let cold = w.write("one", &transcript("one"), true, false);
    w.ingest();
    let mut bytes = std::fs::read(&cold).unwrap();
    bytes.truncate(bytes.len() - 4);
    std::fs::write(cold, bytes).unwrap();
    let report = w.ingest();
    assert!(report.retries_exhausted > 0 || report.files_missing > 0);
    assert_eq!(w.texts("one"), ["first", "second"]);
    assert!(!w.indexed("one"));
}

fn runner(w: &mut World) -> rosetta_pipeline::LiveRunner<'static> {
    rosetta_pipeline::LiveRunner::start_with_ledger(
        &CodexAdapter,
        w.runtime.clone(),
        w.root.clone(),
        Box::new(rosetta_capture::ScriptedEvents::new([])),
        &mut w.store,
        IngestOptions {
            audit_files_per_run: 0,
            ..Default::default()
        },
        rosetta_pipeline::LiveOptions {
            tick: std::time::Duration::ZERO,
            poll_interval: std::time::Duration::ZERO,
            max_coalesced: 16,
        },
        Some(Ledger::open(&w.archive).unwrap()),
        &Instrument::new(),
    )
    .unwrap()
}

#[test]
fn quiet_compressed_polls_do_not_redecode_or_append_ledger_records() {
    let mut w = World::new();
    w.write("one", &transcript("one"), true, true);
    w.ingest();
    let count = w.raw_count();
    let mut live = runner(&mut w);
    let instrument = Instrument::new();
    for _ in 0..3 {
        live.catch_up(&mut w.store, &instrument).unwrap();
    }
    let report = live.into_report();
    assert_eq!(report.stream.files, 0);
    assert_eq!(report.admissions, 0);
    assert_eq!(w.raw_count(), count);
}

#[test]
fn dropped_restore_events_recover_appended_records_on_poll() {
    let mut w = World::new();
    let cold = w.write("one", &transcript("one"), true, true);
    w.ingest();
    let mut live = runner(&mut w);
    w.write(
        "one",
        &[transcript("one"), message(3, "restored while offline")].concat(),
        false,
        false,
    );
    std::fs::remove_file(cold).unwrap();
    live.catch_up(&mut w.store, &Instrument::new()).unwrap();
    assert_eq!(
        w.texts("one"),
        ["first", "second", "restored while offline"]
    );
    let report = live.into_report();
    assert_eq!(report.stream.generations_restarted, 0);
}

#[test]
fn changed_database_selection_streams_the_reverted_view_live() {
    let mut w = World::new();
    let old = w.write("one", &transcript("one"), true, false);
    w.select("one", &old);
    w.ingest();
    let mut live = runner(&mut w);
    // Seed the DB stat gate before changing selection.
    live.catch_up(&mut w.store, &Instrument::new()).unwrap();
    let cut = (header("one", None).len() + message(1, "first").len()) as u64;
    let new = w.write(
        "one_revert",
        &[
            header("one", Some(("one", 2, cut))),
            message(3, "live branch"),
        ]
        .concat(),
        true,
        false,
    );
    w.select("one", &new);
    live.catch_up(&mut w.store, &Instrument::new()).unwrap();
    assert_eq!(w.texts("one"), ["first", "live branch"]);
}

#[test]
fn same_size_same_mtime_rewrite_and_legacy_to_shared_migration() {
    let mut w = World::new();
    let path = w.write("one", &transcript("one"), false, false);
    w.ingest();
    let stamp = std::fs::metadata(&path).unwrap().modified().unwrap();
    let changed = [
        header("one", None),
        message(1, "FIRST"),
        message(2, "second"),
    ]
    .concat();
    std::fs::write(&path, changed).unwrap();
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(stamp))
        .unwrap();
    let mut live = runner(&mut w);
    live.catch_up(&mut w.store, &Instrument::new()).unwrap();
    assert_eq!(w.texts("one"), ["FIRST", "second"]);
    drop(live);
    let parent = transcript("parent");
    w.write("parent", &parent, true, true);
    w.write(
        "one",
        &[
            header("one", Some(("parent", 3, parent.len() as u64))),
            message(4, "migrated"),
        ]
        .concat(),
        false,
        false,
    );
    let mut live = runner(&mut w);
    live.catch_up(&mut w.store, &Instrument::new()).unwrap();
    assert_eq!(w.texts("one"), ["first", "second", "migrated"]);
}

#[test]
fn reference_requires_matching_ordinal_and_newline_byte_boundaries() {
    for bad in 0..3 {
        let mut w = World::new();
        let parent = transcript("parent");
        w.write("parent", &parent, true, false);
        let (end, offset) = match bad {
            0 => (3, parent.len() as u64 - 1),
            1 => (4, parent.len() as u64),
            _ => (3, parent.len() as u64 + 100),
        };
        w.write(
            "child",
            &[
                header("child", Some(("parent", end, offset))),
                message(end + 1, "withheld"),
            ]
            .concat(),
            true,
            false,
        );
        w.ingest();
        assert!(w.texts("child").is_empty());
        assert!(!w.indexed("child"));
    }
}

#[test]
fn missing_base_appearing_later_is_admitted_by_poll() {
    let mut w = World::new();
    let parent = transcript("parent");
    w.write(
        "child",
        &[
            header("child", Some(("parent", 3, parent.len() as u64))),
            message(4, "child"),
        ]
        .concat(),
        true,
        false,
    );
    w.ingest();
    assert!(!w.indexed("child"));
    let mut live = runner(&mut w);
    live.catch_up(&mut w.store, &Instrument::new()).unwrap();
    w.write("parent", &parent, true, true);
    live.catch_up(&mut w.store, &Instrument::new()).unwrap();
    assert_eq!(w.texts("child"), ["first", "second", "child"]);
    assert!(w.indexed("child"));
}

#[test]
fn relocated_native_state_paths_bind_only_inside_the_enrolled_root() {
    let mut w = World::new();
    let file = w.write("one", &transcript("one"), true, false);
    let relative = file.strip_prefix(&w.root).unwrap();
    w.select("one", &Path::new("/old-codex-home").join(relative));
    w.ingest();
    assert_eq!(w.texts("one"), ["first", "second"]);
}

#[test]
fn offline_rebuild_keeps_a_missing_selected_transcript_incomplete() {
    let mut w = World::new();
    let missing = w
        .root
        .join("sessions/2026/11/01/rollout-2026-11-01T12-00-00-one.jsonl");
    w.select("one", &missing);
    w.ingest();
    assert!(!w.indexed("one"));
    w.rebuild();
    assert!(!w.indexed("one"));
}

#[test]
fn shared_history_does_not_duplicate_usage_or_artifact_publications() {
    let mut w = World::new();
    let usage = json!({"input_tokens":10,"cached_input_tokens":0,"output_tokens":2,"reasoning_output_tokens":0,"total_tokens":12});
    let parent = [header("parent",None),
        message(1,"Review [the design](/tmp/rosetta-missing-review.html)"),
        line(json!({"timestamp":"2026-11-01T12:00:01Z","ordinal":2,"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":usage,"last_token_usage":usage}}}))].concat();
    w.write("parent", &parent, true, true);
    w.write(
        "child",
        &[
            header("child", Some(("parent", 3, parent.len() as u64))),
            message(4, "own work"),
        ]
        .concat(),
        true,
        false,
    );
    w.ingest();
    let one = |sql| {
        w.store
            .conn()
            .query_row(sql, [], |r| r.get::<_, i64>(0))
            .unwrap()
    };
    assert_eq!(one("SELECT COUNT(*) FROM html_artifact"), 1);
    assert_eq!(
        one("SELECT SUM(in_tokens + out_tokens) FROM usage_bucket"),
        12
    );
    assert_eq!(w.texts("child").len(), 2);
    w.rebuild();
    assert_eq!(
        w.store
            .conn()
            .query_row(
                "SELECT SUM(in_tokens + out_tokens) FROM usage_bucket",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        12
    );
}

#[test]
fn attached_child_shows_its_own_history_without_parent_context() {
    let mut w = World::new();
    let parent = transcript("parent");
    w.write("parent", &parent, true, false);
    let mut meta: Value =
        serde_json::from_slice(&header("child", Some(("parent", 3, parent.len() as u64)))).unwrap();
    meta["payload"]["attached_thread"] = json!(true);
    w.write(
        "child",
        &[line(meta), message(4, "attached work")].concat(),
        true,
        false,
    );
    w.ingest();
    assert_eq!(w.texts("child"), ["attached work"]);
    w.rebuild();
    assert_eq!(w.texts("child"), ["attached work"]);
}

/// Same frozen decoded bytes, normalizer, ledger codec and logical source
/// identities in both arms. Run explicitly in release mode; fixture creation
/// and compression are outside the measured ingestion interval.
#[test]
#[ignore = "release-only paired ingestion measurement"]
fn frozen_representation_benchmark() {
    let fixtures: Vec<_> = (0..16)
        .map(|session| {
            let id = format!("benchmark-{session}");
            let mut bytes = header(&id, None);
            for ordinal in 1..=500 {
                bytes.extend(message(
                    ordinal,
                    &format!("synthetic {ordinal} {}", "review ".repeat(48)),
                ));
            }
            (id, bytes)
        })
        .collect();
    let decoded_bytes: usize = fixtures.iter().map(|(_, bytes)| bytes.len()).sum();
    let mut digests = Vec::new();
    for compressed in [false, true] {
        let mut w = World::new();
        for (id, bytes) in &fixtures {
            w.write(id, bytes, compressed, false);
        }
        let start = std::time::Instant::now();
        let report = w.ingest();
        let cold_ms = start.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(report.hard_errors, 0);
        assert_eq!(report.files_incomplete, 0);
        let mut hash = blake3::Hasher::new();
        let mut rows = 0;
        for (id, _) in &fixtures {
            for text in w.texts(id) {
                hash.update(text.as_bytes());
                rows += 1;
            }
        }
        digests.push(hash.finalize().to_hex().to_string());
        let mut live = runner(&mut w);
        let start = std::time::Instant::now();
        for _ in 0..20 {
            live.catch_up(&mut w.store, &Instrument::new()).unwrap();
        }
        let poll_ms = start.elapsed().as_secs_f64() * 1000.0;
        let live = live.into_report();
        assert_eq!(live.stream.files, 0);
        println!(
            "representation={} decoded_bytes={decoded_bytes} rows={rows} cold_ms={cold_ms:.2} twenty_polls_ms={poll_ms:.2} quiet_capture_passes={} digest={}",
            if compressed { "zstd" } else { "plain" },
            live.stream.files,
            digests.last().unwrap()
        );
    }
    assert_eq!(digests[0], digests[1]);
}

#[test]
fn live_corruption_keeps_the_previous_view_and_can_recover() {
    let mut w = World::new();
    let cold = w.write("one", &transcript("one"), true, false);
    w.ingest();
    let mut live = runner(&mut w);
    live.catch_up(&mut w.store, &Instrument::new()).unwrap();
    std::fs::write(&cold, b"invalid zstd").unwrap();
    live.catch_up(&mut w.store, &Instrument::new()).unwrap();
    assert_eq!(w.texts("one"), ["first", "second"]);
    assert!(!w.indexed("one"));
    w.write(
        "one",
        &[transcript("one"), message(3, "recovered")].concat(),
        true,
        false,
    );
    live.catch_up(&mut w.store, &Instrument::new()).unwrap();
    assert_eq!(w.texts("one"), ["first", "second", "recovered"]);
    assert!(w.indexed("one"));
}
