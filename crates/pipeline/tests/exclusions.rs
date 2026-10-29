//! Folder policy at the actual metadata → ledger → live-capture boundary.

use std::path::{Path, PathBuf};
use std::time::Duration;

use rosetta_adapter_codex::CodexAdapter;
use rosetta_capture::{Ledger, LedgerEntry, ScriptedEvents, scan_ledger};
use rosetta_core::{IngestOp, ids, rows::RuntimeRow};
use rosetta_pipeline::{
    CwdExclusions, IngestOptions, Instrument, LiveOptions, LiveRunner, Metric, discover,
    meta_ingest_into, rebuild_runtime, stream_backfill_into,
};
use rosetta_store::{Store, queries};
use serde_json::{Value, json};

struct Harness {
    _temp: tempfile::TempDir,
    root: PathBuf,
    ledger_dir: PathBuf,
    runtime: RuntimeRow,
    store: Store,
    ledger: Ledger,
    opts: IngestOptions,
    instrument: Instrument,
}

impl Harness {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join(".codex");
        std::fs::create_dir_all(&root).unwrap();
        let opts = IngestOptions {
            audit_files_per_run: 0,
            ..IngestOptions::default()
        };
        let instrument = Instrument::new();
        let mut runtime = discover(&[&CodexAdapter], temp.path(), &opts, &instrument)
            .unwrap()
            .remove(0)
            .runtime;
        runtime.enabled = true;
        let ledger_dir = temp.path().join("ledger");
        let ledger = Ledger::open(&ledger_dir).unwrap();
        Self {
            _temp: temp,
            root,
            ledger_dir,
            runtime,
            store: Store::open_in_memory().unwrap(),
            ledger,
            opts,
            instrument,
        }
    }

    fn exclude(&mut self, folder: &str) {
        self.opts.exclude_cwds = CwdExclusions::new([folder.to_owned()]).unwrap();
    }

    fn ingest(
        &mut self,
    ) -> (
        rosetta_pipeline::MetaReport,
        Vec<rosetta_pipeline::SessionFile>,
    ) {
        let (meta, files) = meta_ingest_into(
            &CodexAdapter,
            &self.runtime,
            &self.root,
            &mut self.store,
            &self.opts,
            &self.instrument,
            Some(&mut self.ledger),
        )
        .unwrap();
        stream_backfill_into(
            &CodexAdapter,
            &self.runtime,
            &self.root,
            &mut self.store,
            &files,
            &self.opts,
            &self.instrument,
            Some(&mut self.ledger),
        )
        .unwrap();
        self.ledger.flush().unwrap();
        (meta, files)
    }

    fn id(&self, native: &str) -> String {
        ids::session_id(&self.runtime.id, native)
    }

    fn native_sessions(&self) -> Vec<String> {
        let mut ids: Vec<_> = queries::all_sessions(self.store.conn())
            .unwrap()
            .into_iter()
            .map(|row| row.native_session_id)
            .collect();
        ids.sort();
        ids
    }
}

fn record(kind: &str, ordinal: u64, payload: Value) -> String {
    json!({"timestamp":"2026-09-09T12:00:00Z","ordinal":ordinal,"type":kind,"payload":payload})
        .to_string()
        + "\n"
}

fn write_session(
    root: &Path,
    filename: &str,
    id: &str,
    cwd: Option<&str>,
    parent: Option<&str>,
    text: &str,
) -> String {
    let relative = format!("sessions/2026/09/09/rollout-{filename}.jsonl");
    let path = root.join(&relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut meta = json!({"id":id,"timestamp":"2026-09-09T12:00:00Z", "originator":"codex_exec","cli_version":"0.150.1","source":"cli","thread_source":"user","model_provider":"openai","cwd":cwd.unwrap_or("")});
    if let Some(parent) = parent {
        meta["parent_thread_id"] = json!(parent);
    }
    let mut contents = record("session_meta", 0, meta);
    contents.push_str(&record("response_item", 1, json!({"type":"message","id":format!("m-{filename}"),"role":"user","content":[{"type":"input_text","text":text}]})));
    std::fs::write(path, contents).unwrap();
    relative
}

fn raw_payloads(dir: &Path) -> String {
    scan_ledger(dir)
        .unwrap()
        .entries
        .into_iter()
        .filter_map(|entry| {
            if let LedgerEntry::Raw { payload, .. } = entry {
                Some(String::from_utf8(payload).unwrap())
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn exclusion_removes_every_sibling_child_and_orphan_placeholder_before_journaling() {
    let mut h = Harness::new();
    h.exclude("/work/client/./");
    write_session(
        &h.root,
        "private",
        "private",
        Some("/work/client/sub"),
        None,
        "excludedpayload",
    );
    write_session(
        &h.root,
        "sibling",
        "private",
        None,
        None,
        "excludedpayload sibling",
    );
    write_session(
        &h.root,
        "child",
        "child",
        None,
        Some("private"),
        "excludedpayload child",
    );
    write_session(
        &h.root,
        "outside-child",
        "outside-child",
        Some("/work/elsewhere"),
        Some("private"),
        "excludedpayload descendant",
    );
    write_session(
        &h.root,
        "orphan",
        "orphan",
        Some("/work/client"),
        Some("absent-parent"),
        "excludedpayload orphan",
    );
    write_session(
        &h.root,
        "allowed",
        "allowed",
        Some("/work/client-other"),
        None,
        "allowedpayload",
    );
    let (meta, files) = h.ingest();
    assert_eq!(h.native_sessions(), ["allowed"]);
    assert_eq!(files.len(), 1);
    assert_eq!(meta.sessions, 1);
    assert_eq!(meta.projects, 1);
    assert_eq!(h.instrument.snapshot().get(Metric::SessionsDiscovered), 1);
    assert!(meta.excluded_sessions.contains(&h.id("private")));
    assert!(!raw_payloads(&h.ledger_dir).contains("excludedpayload"));
    for entry in scan_ledger(&h.ledger_dir).unwrap().entries {
        if let LedgerEntry::Meta { payload, .. } = entry {
            for op in serde_json::from_str::<Vec<IngestOp>>(&payload).unwrap() {
                if let IngestOp::SessionMeta(row) = op {
                    assert_eq!(row.native_session_id, "allowed");
                }
            }
        }
    }
    let mut rebuilt = Store::open_in_memory().unwrap();
    rebuild_runtime(
        &CodexAdapter,
        &h.runtime,
        &h.ledger_dir,
        &mut rebuilt,
        &Instrument::new(),
    )
    .unwrap();
    assert_eq!(queries::all_sessions(rebuilt.conn()).unwrap().len(), 1);
}

#[test]
fn retirement_preserves_history_and_cursors_and_removing_the_rule_resumes_capture() {
    let mut h = Harness::new();
    let path = write_session(
        &h.root,
        "private",
        "private",
        Some("/work/client"),
        None,
        "originalpayload",
    );
    h.ingest();
    let id = h.id("private");
    let before = queries::session(h.store.conn(), &id).unwrap().unwrap();
    let cursor = queries::cursor_state(h.store.conn(), &h.runtime.id, &path)
        .unwrap()
        .unwrap();
    h.exclude("/work/client");
    write_session(
        &h.root,
        "private",
        "private",
        Some("/work/client"),
        None,
        "replacementpayload",
    );
    let (_, files) = h.ingest();
    assert!(files.is_empty());
    assert_eq!(
        queries::session(h.store.conn(), &id).unwrap().unwrap(),
        before
    );
    let retired = queries::cursor_state(h.store.conn(), &h.runtime.id, &path)
        .unwrap()
        .unwrap();
    assert_eq!(retired.checkpoint, cursor.checkpoint);
    assert_eq!(
        queries::search(h.store.conn(), "originalpayload", 5)
            .unwrap()
            .len(),
        1
    );
    assert!(!raw_payloads(&h.ledger_dir).contains("replacementpayload"));
    let mut rebuilt = Store::open_in_memory().unwrap();
    rebuild_runtime(
        &CodexAdapter,
        &h.runtime,
        &h.ledger_dir,
        &mut rebuilt,
        &Instrument::new(),
    )
    .unwrap();
    assert_eq!(
        queries::search(rebuilt.conn(), "originalpayload", 5)
            .unwrap()
            .len(),
        1
    );

    h.opts.exclude_cwds = CwdExclusions::default();
    h.ingest();
    assert_eq!(h.native_sessions(), ["private"]);
    assert_eq!(
        queries::search(h.store.conn(), "replacementpayload", 5)
            .unwrap()
            .len(),
        1
    );
    assert!(raw_payloads(&h.ledger_dir).contains("replacementpayload"));
}

#[test]
fn retained_parent_metadata_retires_new_children_after_the_parent_source_is_pruned() {
    let mut h = Harness::new();
    let parent = write_session(
        &h.root,
        "parent",
        "parent",
        Some("/work/client"),
        None,
        "retainedparent",
    );
    h.ingest();
    let before = queries::session(h.store.conn(), &h.id("parent")).unwrap();
    std::fs::remove_file(h.root.join(parent)).unwrap();
    h.exclude("/work/client");
    write_session(
        &h.root,
        "child",
        "child",
        None,
        Some("parent"),
        "excludedchild",
    );
    let (_, files) = h.ingest();
    assert!(files.is_empty());
    assert_eq!(h.native_sessions(), ["parent"]);
    assert_eq!(
        queries::session(h.store.conn(), &h.id("parent")).unwrap(),
        before
    );
    assert!(!raw_payloads(&h.ledger_dir).contains("excludedchild"));
}

#[test]
fn live_creation_obeys_policy_and_quiet_excluded_files_do_not_trigger_repeated_admission() {
    let mut h = Harness::new();
    h.exclude("/work/client");
    let (meta, files) = h.ingest();
    let mut events = ScriptedEvents::default();
    let private = write_session(
        &h.root,
        "private",
        "private",
        Some("/work/client"),
        None,
        "excludedlive",
    );
    let allowed = write_session(
        &h.root,
        "allowed",
        "allowed",
        Some("/work/public"),
        None,
        "allowedlive",
    );
    events.push_changed(&private);
    events.push_changed(&allowed);
    let live = LiveOptions {
        tick: Duration::ZERO,
        poll_interval: Duration::ZERO,
        max_coalesced: 16,
    };
    let mut runner = LiveRunner::resume_from_roster(
        &CodexAdapter,
        h.runtime.clone(),
        h.root.clone(),
        Box::new(events),
        files,
        h.opts.clone(),
        live,
        Some(h.ledger),
    )
    .with_admission_sources(meta.admission_sources);
    runner.tick(&mut h.store, &h.instrument).unwrap();
    assert_eq!(queries::all_sessions(h.store.conn()).unwrap().len(), 1);
    let admissions = runner.report().admissions;
    runner.tick(&mut h.store, &h.instrument).unwrap();
    runner.tick(&mut h.store, &h.instrument).unwrap();
    assert_eq!(runner.report().admissions, admissions);
    drop(runner);
    assert!(!raw_payloads(&h.ledger_dir).contains("excludedlive"));
}

#[test]
fn known_source_rewrites_are_rechecked_on_hints_and_on_stamp_preserving_audits() {
    for audit in [false, true] {
        let mut h = Harness::new();
        h.exclude("/work/secret");
        let path = write_session(
            &h.root,
            "work",
            "work",
            Some("/work/public"),
            None,
            "beforepayload",
        );
        let (meta, files) = h.ingest();
        let before = queries::session(h.store.conn(), &h.id("work")).unwrap();
        let metadata = std::fs::metadata(h.root.join(&path)).unwrap();
        write_session(
            &h.root,
            "work",
            "work",
            Some("/work/secret"),
            None,
            "hiddenpayload",
        );
        assert_eq!(
            std::fs::metadata(h.root.join(&path)).unwrap().len(),
            metadata.len()
        );
        let mut events = ScriptedEvents::default();
        if audit {
            std::fs::File::options()
                .write(true)
                .open(h.root.join(&path))
                .unwrap()
                .set_times(std::fs::FileTimes::new().set_modified(metadata.modified().unwrap()))
                .unwrap();
            h.opts.audit_files_per_run = 1;
        } else {
            events.push_changed(&path);
        }
        let live = LiveOptions {
            tick: Duration::ZERO,
            poll_interval: Duration::ZERO,
            max_coalesced: 16,
        };
        let id = h.id("work");
        let mut runner = LiveRunner::resume_from_roster(
            &CodexAdapter,
            h.runtime.clone(),
            h.root.clone(),
            Box::new(events),
            files,
            h.opts.clone(),
            live,
            Some(h.ledger),
        )
        .with_admission_sources(meta.admission_sources);
        runner.tick(&mut h.store, &h.instrument).unwrap();
        assert_eq!(queries::session(h.store.conn(), &id).unwrap(), before);
        assert!(
            queries::search(h.store.conn(), "hiddenpayload", 5)
                .unwrap()
                .is_empty()
        );
        drop(runner);
        assert!(
            !raw_payloads(&h.ledger_dir).contains("hiddenpayload"),
            "audit={audit}"
        );
    }
}

#[test]
fn audit_rechecks_cwd_bearing_siblings_before_capture() {
    let mut h = Harness::new();
    h.exclude("/work/secret");
    let sibling = write_session(&h.root, "a-sibling", "work", None, None, "beforepayload");
    let owner = write_session(
        &h.root,
        "z-owner",
        "work",
        Some("/work/public"),
        None,
        "beforepayload",
    );
    let (meta, files) = h.ingest();
    let sibling_meta = std::fs::metadata(h.root.join(&sibling)).unwrap();
    let owner_meta = std::fs::metadata(h.root.join(&owner)).unwrap();
    write_session(&h.root, "a-sibling", "work", None, None, "hiddenpayload");
    write_session(
        &h.root,
        "z-owner",
        "work",
        Some("/work/secret"),
        None,
        "hiddenpayload",
    );
    for (path, metadata) in [(&sibling, sibling_meta), (&owner, owner_meta)] {
        assert_eq!(
            std::fs::metadata(h.root.join(path)).unwrap().len(),
            metadata.len()
        );
        std::fs::File::options()
            .write(true)
            .open(h.root.join(path))
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(metadata.modified().unwrap()))
            .unwrap();
    }
    h.opts.audit_files_per_run = 1;
    let mut runner = LiveRunner::resume_from_roster(
        &CodexAdapter,
        h.runtime.clone(),
        h.root.clone(),
        Box::<ScriptedEvents>::default(),
        files,
        h.opts.clone(),
        LiveOptions {
            tick: Duration::ZERO,
            poll_interval: Duration::ZERO,
            max_coalesced: 16,
        },
        Some(h.ledger),
    )
    .with_admission_sources(meta.admission_sources);
    runner.tick(&mut h.store, &h.instrument).unwrap();
    drop(runner);
    assert!(
        !raw_payloads(&h.ledger_dir).contains("hiddenpayload"),
        "audit captured excluded sibling payload"
    );
}

#[test]
fn one_append_does_not_reskim_unrelated_sessions() {
    use std::io::Write;
    for directory_hint in [false, true] {
        let mut h = Harness::new();
        h.exclude("/work/secret");
        let mut active = String::new();
        for index in 0..100 {
            let name = format!("work-{index}");
            active = write_session(
                &h.root,
                &name,
                &name,
                Some("/work/public"),
                None,
                "beforepayload",
            );
        }
        let (meta, files) = h.ingest();
        let before = h.instrument.snapshot().get(Metric::FilesSkimmed);
        let meta_before = scan_ledger(&h.ledger_dir)
            .unwrap()
            .entries
            .into_iter()
            .filter(|entry| matches!(entry, LedgerEntry::Meta { .. }))
            .count();
        std::fs::OpenOptions::new().append(true).open(h.root.join(&active)).unwrap()
        .write_all(record("response_item", 2, json!({"type":"message","id":"append","role":"user","content":[{"type":"input_text","text":"appendedpayload"}]})).as_bytes()).unwrap();
        let mut events = ScriptedEvents::default();
        events.push_changed(if directory_hint {
            "sessions/2026/09/09"
        } else {
            &active
        });
        let mut runner = LiveRunner::resume_from_roster(
            &CodexAdapter,
            h.runtime.clone(),
            h.root.clone(),
            Box::new(events),
            files,
            h.opts.clone(),
            LiveOptions {
                tick: Duration::ZERO,
                poll_interval: Duration::from_secs(30),
                max_coalesced: 16,
            },
            Some(h.ledger),
        )
        .with_admission_sources(meta.admission_sources);
        runner.tick(&mut h.store, &h.instrument).unwrap();
        let skimmed = h.instrument.snapshot().get(Metric::FilesSkimmed) - before;
        assert_eq!(skimmed, 1, "one append re-skimmed {skimmed} sources");
        assert_eq!(runner.report().admissions, 0);
        assert_eq!(
            queries::search(h.store.conn(), "appendedpayload", 5)
                .unwrap()
                .len(),
            1
        );
        drop(runner);
        let meta_after = scan_ledger(&h.ledger_dir)
            .unwrap()
            .entries
            .into_iter()
            .filter(|entry| matches!(entry, LedgerEntry::Meta { .. }))
            .count();
        assert_eq!(
            meta_after, meta_before,
            "an append must not journal another metadata batch"
        );
    }
}

fn restore_modified(path: &Path, before: &std::fs::Metadata) {
    assert_eq!(std::fs::metadata(path).unwrap().len(), before.len());
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(before.modified().unwrap()))
        .unwrap();
}

#[test]
fn an_audited_child_follows_new_ancestry_through_withheld_sources() {
    for ledger_only in [false, true] {
        let mut h = Harness::new();
        h.exclude("/work/secret");
        let child = write_session(
            &h.root,
            "a-child",
            "child",
            None,
            Some("public"),
            "beforepayload",
        );
        write_session(
            &h.root,
            "public",
            "public",
            Some("/work/public"),
            None,
            "parentpayload",
        );
        let secret = write_session(
            &h.root,
            "secret",
            "secret",
            None,
            Some("ancestor"),
            "privateparent",
        );
        write_session(
            &h.root,
            "ancestor",
            "ancestor",
            Some("/work/secret"),
            None,
            "privateancestor",
        );
        if ledger_only {
            let path = h.root.join(secret);
            let source = std::fs::read_to_string(&path).unwrap();
            std::fs::write(
                path,
                source.replace(
                    "\"thread_source\":\"user\"",
                    "\"thread_source\":\"guardian_review\"",
                ),
            )
            .unwrap();
        }
        for i in 0..20 {
            let id = format!("unrelated-{i}");
            write_session(
                &h.root,
                &id,
                &id,
                Some("/work/public"),
                None,
                "unrelatedpayload",
            );
        }
        let (meta, files) = h.ingest();
        assert!(
            !files
                .iter()
                .any(|file| file.native_session_id == "secret"
                    || file.native_session_id == "ancestor")
        );
        let before = std::fs::metadata(h.root.join(&child)).unwrap();
        h.opts.audit_files_per_run = 1;
        let mut runner = LiveRunner::resume_from_roster(
            &CodexAdapter,
            h.runtime.clone(),
            h.root.clone(),
            Box::<ScriptedEvents>::default(),
            files,
            h.opts.clone(),
            LiveOptions {
                tick: Duration::ZERO,
                poll_interval: Duration::ZERO,
                max_coalesced: 16,
            },
            Some(h.ledger),
        )
        .with_admission_sources(meta.admission_sources);
        // Seed the one-shot stat observations of already-withheld sources.
        runner.catch_up(&mut h.store, &h.instrument).unwrap();
        let admissions = runner.report().admissions;
        let skimmed = h.instrument.snapshot().get(Metric::FilesSkimmed);
        write_session(
            &h.root,
            "a-child",
            "child",
            None,
            Some("secret"),
            "hiddenpayload",
        );
        restore_modified(&h.root.join(&child), &before);
        runner.tick(&mut h.store, &h.instrument).unwrap();
        assert_eq!(
            runner.report().admissions,
            admissions,
            "the audit must not fall back to a root skim"
        );
        assert_eq!(
            h.instrument.snapshot().get(Metric::FilesSkimmed) - skimmed,
            4,
            "each relevant source is read once"
        );
        assert!(
            queries::search(h.store.conn(), "hiddenpayload", 5)
                .unwrap()
                .is_empty()
        );
        drop(runner);
        assert!(
            !raw_payloads(&h.ledger_dir).contains("hiddenpayload"),
            "ledger_only={ledger_only}"
        );
    }
}

#[test]
fn a_transcript_hint_rechecks_its_shared_database_metadata() {
    let mut h = Harness::new();
    h.exclude("/work/secret");
    let path = write_session(&h.root, "work", "work", None, None, "beforepayload");
    for i in 0..20 {
        let id = format!("unrelated-{i}");
        write_session(
            &h.root,
            &id,
            &id,
            Some("/work/public"),
            None,
            "unrelatedpayload",
        );
    }
    let db = rusqlite::Connection::open(h.root.join("state_5.sqlite")).unwrap();
    db.execute_batch("CREATE TABLE threads(id TEXT PRIMARY KEY, source TEXT, thread_source TEXT, cwd TEXT, title TEXT, cli_version TEXT, created_at INTEGER, updated_at INTEGER);
        INSERT INTO threads VALUES ('work', 'cli', 'user', '/work/public', 'Work', '0.150.1', 10, 11);").unwrap();
    let (meta, files) = h.ingest();
    let id = h.id("work");
    let before = queries::session(h.store.conn(), &id).unwrap();
    db.execute(
        "UPDATE threads SET cwd = '/work/secret' WHERE id = 'work'",
        [],
    )
    .unwrap();
    write_session(&h.root, "work", "work", None, None, "hiddenpayload");
    let skimmed = h.instrument.snapshot().get(Metric::FilesSkimmed);
    let mut events = ScriptedEvents::default();
    events.push_changed(&path);
    let mut runner = LiveRunner::resume_from_roster(
        &CodexAdapter,
        h.runtime.clone(),
        h.root.clone(),
        Box::new(events),
        files,
        h.opts.clone(),
        LiveOptions {
            tick: Duration::ZERO,
            poll_interval: Duration::from_secs(30),
            max_coalesced: 16,
        },
        Some(h.ledger),
    )
    .with_admission_sources(meta.admission_sources);
    runner.tick(&mut h.store, &h.instrument).unwrap();
    assert_eq!(runner.report().admissions, 0);
    assert_eq!(
        h.instrument.snapshot().get(Metric::FilesSkimmed) - skimmed,
        2
    );
    assert_eq!(queries::session(h.store.conn(), &id).unwrap(), before);
    drop(runner);
    assert!(!raw_payloads(&h.ledger_dir).contains("hiddenpayload"));
}
