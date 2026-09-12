//! Internal sessions are captured losslessly without becoming canonical work.

use std::io::Write;
use std::path::{Path, PathBuf};

use rosetta_adapter_codex::CodexAdapter;
use rosetta_capture::{Ledger, LedgerEntry, scan_ledger};
use rosetta_core::rows::RuntimeRow;
use rosetta_core::{IngestOp, ids};
use rosetta_pipeline::{
    IngestOptions, IngestPlan, Instrument, RebuildOptions, apply_prepared_runtime,
    meta_ingest_into, prepare_captured_runtime, prepare_runtime, rebuild_runtime,
    rebuild_runtime_with_options, stream_backfill_into,
};
use rosetta_store::{Store, queries};
use serde_json::{Value, json};
use tempfile::TempDir;

struct Fixture {
    _temp: TempDir,
    root: PathBuf,
    ledger_dir: PathBuf,
    projection: PathBuf,
    runtime: RuntimeRow,
    visible: usize,
}

fn record(kind: &str, ordinal: u64, payload: Value) -> String {
    json!({"timestamp": "2026-09-09T12:00:00Z", "ordinal": ordinal, "type": kind, "payload": payload}).to_string() + "\n"
}

fn message(ordinal: u64, role: &str, text: &str) -> String {
    record(
        "response_item",
        ordinal,
        json!({
            "type": "message", "id": format!("message-{ordinal}"), "role": role,
            "content": [{"type": if role == "user" { "input_text" } else { "output_text" }, "text": text}],
        }),
    )
}

fn rollout(root: &Path, id: &str, source: &Value, thread_source: &str, private: bool) {
    let path = root.join(format!("sessions/2026/09/09/rollout-{id}.jsonl"));
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut payload = json!({
        "id": id, "timestamp": "2026-09-09T12:00:00Z", "cwd": "/fixture/project",
        "originator": "codex_exec", "cli_version": "0.150.1", "source": source,
        "thread_source": thread_source, "model_provider": "openai",
    });
    if id != "parent" {
        payload["parent_thread_id"] = json!("parent");
    }
    let text = if private {
        "private-review-sentinel ".repeat(4000)
    } else {
        "visible work about the guardian implementation".into()
    };
    let mut lines = record("session_meta", 0, payload);
    lines.push_str(&message(1, "user", &text));
    lines.push_str(&message(
        2,
        "assistant",
        if private {
            r#"{"outcome":"allow"}"#
        } else {
            "visible result"
        },
    ));
    if private {
        lines.push_str(&record("future_private_event", 3, json!({"private": text})));
    }
    std::fs::write(path, lines).unwrap();
}

fn fixture(include_visible: bool) -> Fixture {
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("codex");
    std::fs::create_dir_all(&root).unwrap();
    let state = rusqlite::Connection::open(root.join("state_5.sqlite")).unwrap();
    state.execute_batch("CREATE TABLE threads(id TEXT PRIMARY KEY, source TEXT, thread_source TEXT, cwd TEXT, title TEXT, cli_version TEXT, created_at INTEGER, updated_at INTEGER);").unwrap();
    let mut threads = vec![
        (
            "legacy-guardian",
            json!({"subagent":{"other":"guardian"}}),
            "subagent",
            true,
        ),
        (
            "modern-guardian",
            json!({"subagent":{"other":"guardian"}}),
            "guardian_review",
            true,
        ),
    ];
    if include_visible {
        threads.extend([
            ("parent", json!("cli"), "user", false),
            (
                "worker",
                json!({"subagent":{"other":"review"}}),
                "subagent",
                false,
            ),
        ]);
    }
    for (id, source, kind, private) in threads {
        rollout(&root, id, &source, kind, private);
        let encoded = source
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| source.to_string());
        state.execute("INSERT INTO threads VALUES (?1,?2,?3,'/fixture/project','Review guardian code','0.150.1',1,2)", rusqlite::params![id,encoded,kind]).unwrap();
    }
    drop(state);
    let runtime = RuntimeRow {
        id: ids::runtime_id("codex", root.to_str().unwrap()),
        kind: "codex".into(),
        data_root: root.to_string_lossy().into_owned(),
        enabled: true,
        account: None,
        config: None,
        skills: None,
        versions_seen: None,
        discovered_at: 1,
        label: None,
        provenance: Default::default(),
    };
    Fixture {
        ledger_dir: temp.path().join("ledger"),
        projection: temp.path().join("projection.sqlite"),
        _temp: temp,
        root,
        runtime,
        visible: if include_visible { 2 } else { 0 },
    }
}

fn count(store: &Store, sql: &str) -> i64 {
    store.conn().query_row(sql, [], |row| row.get(0)).unwrap()
}

fn assert_projection(f: &Fixture, store: &Store) {
    let sessions = queries::all_sessions(store.conn()).unwrap();
    assert_eq!(
        sessions.len(),
        f.visible,
        "no reviewer or parent placeholder should leak"
    );
    assert!(
        sessions
            .iter()
            .all(|s| matches!(s.native_session_id.as_str(), "parent" | "worker"))
    );
    assert_eq!(
        count(store, "SELECT count(*) FROM message"),
        (f.visible * 2) as i64
    );
    assert_eq!(
        count(
            store,
            "SELECT count(*) FROM ingest_cursor WHERE session_id IS NULL"
        ),
        4
    );
    assert_eq!(count(store, "SELECT count(*) FROM drift_agg"), 0);
    assert_eq!(
        count(
            store,
            "SELECT count(*) FROM message WHERE content LIKE '%private-review-sentinel%'"
        ),
        0
    );
    for native in ["legacy-guardian", "modern-guardian"] {
        let id = ids::session_id(&f.runtime.id, native);
        for table in [
            "usage_bucket",
            "session_usage",
            "file_edit",
            "git_commit",
            "task",
            "plan",
        ] {
            let rows: i64 = store
                .conn()
                .query_row(
                    &format!("SELECT count(*) FROM {table} WHERE session_id=?1"),
                    [&id],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(rows, 0, "{table}");
        }
        let dirty: i64 = store
            .conn()
            .query_row(
                "SELECT count(*) FROM search_dirty WHERE key=?1",
                [&id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(dirty, 0, "ledger-only appends must not dirty search");
        let mut documents = 0;
        queries::visit_session_search_documents(store.conn(), &id, |_| {
            documents += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(documents, 0);
    }
    let assets = f.projection.with_file_name("projection.sqlite.assets");
    assert!(
        std::fs::read_dir(assets).map_or(true, |mut files| files.next().is_none()),
        "discarded large prompts must not produce text members"
    );
    let fk = store
        .conn()
        .prepare("PRAGMA foreign_key_check")
        .unwrap()
        .query([])
        .unwrap()
        .next()
        .unwrap()
        .is_some();
    assert!(!fk);
    assert_eq!(
        store
            .conn()
            .query_row("PRAGMA quick_check", [], |row| row.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
}

fn options() -> IngestOptions {
    IngestOptions {
        audit_files_per_run: 0,
        ..IngestOptions::default()
    }
}

#[test]
fn fresh_ingest_resume_and_ledger_replay_keep_only_user_facing_sessions() {
    let f = fixture(true);
    let opts = options();
    let instrument = Instrument::new();
    let mut store = Store::open(&f.projection).unwrap();
    let mut ledger = Ledger::open(&f.ledger_dir).unwrap();
    let (meta, files) = meta_ingest_into(
        &CodexAdapter,
        &f.runtime,
        &f.root,
        &mut store,
        &opts,
        &instrument,
        Some(&mut ledger),
    )
    .unwrap();
    assert_eq!(meta.files_unidentified, 0);
    assert_eq!(files.iter().filter(|file| file.ledger_only).count(), 4);
    let first = stream_backfill_into(
        &CodexAdapter,
        &f.runtime,
        &f.root,
        &mut store,
        &files,
        &opts,
        &instrument,
        Some(&mut ledger),
    )
    .unwrap();
    assert_eq!(first.hard_errors, 0);
    assert_eq!(first.files_incomplete, 0);
    assert_projection(&f, &store);
    let path = f
        .root
        .join("sessions/2026/09/09/rollout-legacy-guardian.jsonl");
    std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .unwrap()
        .write_all(message(4, "user", "private-review-sentinel resumed").as_bytes())
        .unwrap();
    // A new call restores adapter state from the durable cursor checkpoint.
    let second = stream_backfill_into(
        &CodexAdapter,
        &f.runtime,
        &f.root,
        &mut store,
        &files,
        &opts,
        &instrument,
        Some(&mut ledger),
    )
    .unwrap();
    assert_eq!(second.records, 1);
    assert_projection(&f, &store);
    ledger.flush().unwrap();
    drop(ledger);
    let scanned = scan_ledger(&f.ledger_dir).unwrap();
    assert!(scanned.entries.iter().any(|entry| matches!(entry, LedgerEntry::Raw { source, payload, .. } if source.ledger_only && String::from_utf8_lossy(payload).contains("private-review-sentinel resumed"))));
    for entry in &scanned.entries {
        if let LedgerEntry::Meta { payload, .. } = entry {
            let ops: Vec<IngestOp> = serde_json::from_str(payload).unwrap();
            assert!(ops.iter().all(|op| !matches!(op, IngestOp::SessionMeta(s) if s.native_session_id.ends_with("guardian"))));
        }
    }
    std::fs::remove_dir_all(&f.root).unwrap();
    let mut replay = Store::open_in_memory_for_rebuild().unwrap();
    let report = rebuild_runtime(
        &CodexAdapter,
        &f.runtime,
        &f.ledger_dir,
        &mut replay,
        &instrument,
    )
    .unwrap();
    assert_eq!(report.sessions_stubbed, 0);
    replay.finish_bulk_load().unwrap();
    assert_projection(&f, &replay);

    // The cached/parallel replay path must preserve the raw source policy too.
    let cache = f.ledger_dir.join("election.json");
    let mut elected = Store::open_in_memory_for_rebuild().unwrap();
    rebuild_runtime_with_options(
        &CodexAdapter,
        &f.runtime,
        &f.ledger_dir,
        &mut elected,
        &instrument,
        RebuildOptions {
            election_only: true,
            election_cache: Some(cache.clone()),
            ..RebuildOptions::default()
        },
    )
    .unwrap();
    let prepared = prepare_runtime(
        &CodexAdapter,
        &f.runtime,
        &f.ledger_dir,
        &instrument,
        RebuildOptions {
            election_cache: Some(cache),
            ..RebuildOptions::default()
        },
    )
    .unwrap();
    let mut parallel = Store::open_in_memory_for_rebuild().unwrap();
    let report = apply_prepared_runtime(
        &CodexAdapter,
        &f.runtime,
        prepared,
        &mut parallel,
        &instrument,
    )
    .unwrap();
    assert_eq!(report.sessions_stubbed, 0);
    parallel.finish_bulk_load().unwrap();
    assert_projection(&f, &parallel);
}

#[test]
fn fresh_parallel_planning_does_not_recreate_omitted_sessions() {
    let f = fixture(false);
    let opts = options();
    let instrument = Instrument::new();
    let mut planner = Store::open_in_memory_for_cold_planning().unwrap();
    let mut ledger = Ledger::open(&f.ledger_dir).unwrap();
    let (meta, files) = meta_ingest_into(
        &CodexAdapter,
        &f.runtime,
        &f.root,
        &mut planner,
        &opts,
        &instrument,
        Some(&mut ledger),
    )
    .unwrap();
    assert_eq!(meta.sessions, 0);
    assert_eq!(meta.projects, 0);
    let plan = IngestPlan::settle(std::slice::from_ref(&files), &Default::default());
    assert_eq!(plan.sessions().count(), 0);
    let streamed = stream_backfill_into(
        &CodexAdapter,
        &f.runtime,
        &f.root,
        &mut planner,
        &files,
        &opts,
        &instrument,
        Some(&mut ledger),
    )
    .unwrap();
    assert_eq!(streamed.files_incomplete, 0);
    let prepared =
        prepare_captured_runtime(&CodexAdapter, &f.runtime, &files, &mut planner, &streamed)
            .unwrap();
    let mut store = Store::open_in_memory_for_rebuild_with_assets(&f.projection).unwrap();
    let applied =
        apply_prepared_runtime(&CodexAdapter, &f.runtime, prepared, &mut store, &instrument)
            .unwrap();
    assert_eq!(applied.sessions_stubbed, 0);
    store.finish_bulk_load().unwrap();
    assert_projection(&f, &store);
}

#[test]
fn old_archives_rebuild_without_empty_review_headers_and_resume_capture() {
    let f = fixture(true);
    let opts = options();
    let instrument = Instrument::new();
    let mut captured = Store::open_in_memory().unwrap();
    let mut ledger = Ledger::open(&f.ledger_dir).unwrap();
    let (_, files) = meta_ingest_into(
        &CodexAdapter,
        &f.runtime,
        &f.root,
        &mut captured,
        &opts,
        &instrument,
        Some(&mut ledger),
    )
    .unwrap();
    stream_backfill_into(
        &CodexAdapter,
        &f.runtime,
        &f.root,
        &mut captured,
        &files,
        &opts,
        &instrument,
        Some(&mut ledger),
    )
    .unwrap();
    ledger.flush().unwrap();
    drop(ledger);

    // Reproduce the archive written before internal-source admission existed:
    // public source definitions and both early and late title/header batches.
    let mut internal_meta = Vec::new();
    let parent = queries::session(captured.conn(), &ids::session_id(&f.runtime.id, "parent"))
        .unwrap()
        .unwrap();
    for native in ["legacy-guardian", "modern-guardian"] {
        let mut row = parent.clone();
        row.id = ids::session_id(&f.runtime.id, native);
        row.native_session_id = native.into();
        row.parent_session_id = Some(parent.id.clone());
        row.role = rosetta_core::rows::SessionRole::Subagent;
        row.title = Some("The following is the Codex agent hi…".into());
        internal_meta.push(IngestOp::SessionTitle(
            rosetta_core::rows::SessionTitleRow {
                session_id: row.id.clone(),
                title: row.title.clone().unwrap(),
                kind: rosetta_core::rows::TitleKind::Fallback,
                at: 1,
            },
        ));
        internal_meta.push(IngestOp::SessionMeta(row));
    }
    let old_dir = f._temp.path().join("old-ledger");
    let mut old = Ledger::open(&old_dir).unwrap();
    for entry in scan_ledger(&f.ledger_dir).unwrap().entries {
        match entry {
            LedgerEntry::Meta {
                schema_version,
                payload,
            } => {
                let mut ops: Vec<IngestOp> = serde_json::from_str(&payload).unwrap();
                ops.extend(internal_meta.clone());
                old.append_meta(schema_version, &serde_json::to_string(&ops).unwrap())
                    .unwrap();
            }
            LedgerEntry::Raw {
                mut source,
                generation,
                start_offset,
                payload,
            } => {
                source.ledger_only = false;
                old.append_raw(&source, generation, [(start_offset, payload)])
                    .unwrap();
            }
            LedgerEntry::GenerationComplete {
                mut source,
                generation,
                ..
            } => {
                source.ledger_only = false;
                old.complete_generation(&source, generation, 13, true)
                    .unwrap();
            }
        }
    }
    old.append_meta(
        rosetta_pipeline::LEDGER_META_SCHEMA_VERSION,
        &serde_json::to_string(&internal_meta).unwrap(),
    )
    .unwrap();
    old.flush().unwrap();
    drop(old);

    // Rebuild must work from the archive even when the native files are gone.
    let offline = f.root.with_extension("offline");
    std::fs::rename(&f.root, &offline).unwrap();
    let cache = old_dir.join("election.json");
    let mut replay = Store::open_in_memory_for_rebuild().unwrap();
    let report = rebuild_runtime_with_options(
        &CodexAdapter,
        &f.runtime,
        &old_dir,
        &mut replay,
        &instrument,
        RebuildOptions {
            election_cache: Some(cache.clone()),
            ..RebuildOptions::default()
        },
    )
    .unwrap();
    assert_eq!(report.sessions_stubbed, 0);
    replay.finish_bulk_load().unwrap();
    assert_projection(&f, &replay);

    let prepared = prepare_runtime(
        &CodexAdapter,
        &f.runtime,
        &old_dir,
        &instrument,
        RebuildOptions {
            election_cache: Some(cache),
            ..RebuildOptions::default()
        },
    )
    .unwrap();
    let mut parallel = Store::open_in_memory_for_rebuild().unwrap();
    let report = apply_prepared_runtime(
        &CodexAdapter,
        &f.runtime,
        prepared,
        &mut parallel,
        &instrument,
    )
    .unwrap();
    assert_eq!(report.sessions_stubbed, 0);
    parallel.finish_bulk_load().unwrap();
    assert_projection(&f, &parallel);
    assert!(
        scan_ledger(&old_dir)
            .unwrap()
            .entries
            .iter()
            .all(|entry| match entry {
                LedgerEntry::Raw { source, .. }
                | LedgerEntry::GenerationComplete { source, .. } => !source.ledger_only,
                _ => true,
            }),
        "replay never rewrites archived source definitions"
    );

    // After publication the cursor's new admission must agree with a current
    // metadata skim, and a suffix must remain resumable and raw-ledger-only.
    std::fs::rename(&offline, &f.root).unwrap();
    std::fs::OpenOptions::new()
        .append(true)
        .open(
            f.root
                .join("sessions/2026/09/09/rollout-legacy-guardian.jsonl"),
        )
        .unwrap()
        .write_all(message(4, "user", "private-review-sentinel after rebuild").as_bytes())
        .unwrap();
    let mut old = Ledger::open(&old_dir).unwrap();
    let (_, files) = meta_ingest_into(
        &CodexAdapter,
        &f.runtime,
        &f.root,
        &mut replay,
        &opts,
        &instrument,
        Some(&mut old),
    )
    .unwrap();
    assert_eq!(files.iter().filter(|file| file.ledger_only).count(), 4);
    let report = stream_backfill_into(
        &CodexAdapter,
        &f.runtime,
        &f.root,
        &mut replay,
        &files,
        &opts,
        &instrument,
        Some(&mut old),
    )
    .unwrap();
    assert_eq!(report.hard_errors, 0);
    assert_projection(&f, &replay);
    old.flush().unwrap();
    drop(old);
    assert!(
        scan_ledger(&old_dir)
            .unwrap()
            .entries
            .iter()
            .any(|entry| matches!(entry,
                LedgerEntry::Raw { source, payload, .. }
                if source.ledger_only && String::from_utf8_lossy(payload).contains("after rebuild")
            ))
    );
    let mut resumed = Store::open_in_memory_for_rebuild().unwrap();
    rebuild_runtime(
        &CodexAdapter,
        &f.runtime,
        &old_dir,
        &mut resumed,
        &instrument,
    )
    .unwrap();
    resumed.finish_bulk_load().unwrap();
    assert_projection(&f, &resumed);
}
