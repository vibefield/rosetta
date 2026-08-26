use std::io::Write;
use std::path::{Path, PathBuf};

use rosetta_adapter_claude_code::ClaudeCodeAdapter;
use rosetta_adapter_codex::CodexAdapter;
use rosetta_adapter_support::AgentAdapter;
use rosetta_bench_corpus::{Agent, CorpusSpec, generate};
use rosetta_core::ids;
use rosetta_core::rows::{RuntimeRow, SessionRole, StreamState};
use rosetta_pipeline::{IngestOptions, Instrument, meta_ingest, stream_backfill};
use rosetta_store::{Store, queries};
use serde_json::{Value, json};
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

fn opts() -> IngestOptions {
    IngestOptions {
        audit_files_per_run: 0,
        ..IngestOptions::default()
    }
}

fn write_lines(path: &Path, lines: &[Value]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let text = lines
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(path, format!("{text}\n")).unwrap();
}

fn claude_user(session: &str, uuid: &str, content: &str, cwd: &str) -> Value {
    json!({
        "parentUuid": null,
        "isSidechain": false,
        "type": "user",
        "message": {"role": "user", "content": content},
        "uuid": uuid,
        "timestamp": "2026-08-25T02:35:19.054Z",
        "userType": "external",
        "cwd": cwd,
        "sessionId": session,
        "version": "2.1.241",
        "gitBranch": "main"
    })
}

fn claude_edit(session: &str, uuid: &str, path: &str) -> Value {
    json!({
        "parentUuid": null,
        "isSidechain": false,
        "message": {
            "content": [{
                "id": format!("tool-{uuid}"),
                "input": {"file_path": path, "old_string": "a", "new_string": "b"},
                "name": "Edit",
                "type": "tool_use"
            }],
            "id": format!("msg-{uuid}"),
            "model": "claude-opus-5",
            "role": "assistant",
            "stop_reason": "tool_use",
            "stop_sequence": null,
            "type": "message"
        },
        "type": "assistant",
        "uuid": uuid,
        "timestamp": "2026-08-25T02:35:20.054Z",
        "userType": "external",
        "cwd": "/w/project",
        "sessionId": session,
        "version": "2.1.241",
        "gitBranch": "main"
    })
}

fn only_path(root: &Path, suffix: &str) -> PathBuf {
    walkdir::WalkDir::new(root)
        .into_iter()
        .filter_map(Result::ok)
        .map(|entry| entry.into_path())
        .find(|path| path.to_string_lossy().ends_with(suffix))
        .unwrap()
}

#[test]
fn layout_inventory_never_becomes_a_transcript_queue() {
    let root = TempDir::new().unwrap();
    std::fs::write(root.path().join(".claude.json"), "{\"projects\":{}}\n").unwrap();
    write_lines(
        &root.path().join("projects/p/session-1.jsonl"),
        &[claude_user("session-1", "u1", "hello", "/w/project")],
    );

    let mut store = Store::open_in_memory().unwrap();
    let runtime = runtime(&ClaudeCodeAdapter);
    let (meta, files) = meta_ingest(
        &ClaudeCodeAdapter,
        &runtime,
        root.path(),
        &mut store,
        &opts(),
        &Instrument::new(),
    )
    .unwrap();
    assert_eq!(meta.files_matched, 2);
    assert_eq!(meta.files_inventory_only, 1);
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].stream_id, "transcript");

    let stream = stream_backfill(
        &ClaudeCodeAdapter,
        &runtime,
        root.path(),
        &mut store,
        &files,
        &opts(),
        &Instrument::new(),
    )
    .unwrap();
    assert_eq!(stream.hard_errors, 0);
    assert_eq!(stream.records, 1);
}

#[test]
fn codex_large_opening_record_gets_a_bounded_identity_retry() {
    let root = TempDir::new().unwrap();
    generate(&CorpusSpec::new(Agent::Codex, 1, 30, 42), root.path()).unwrap();
    let rollout = only_path(root.path(), ".jsonl");
    let text = std::fs::read_to_string(&rollout).unwrap();
    let mut lines: Vec<Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    lines[0]["payload"]["base_instructions"] = Value::String("x".repeat(100_000));
    write_lines(&rollout, &lines);

    let mut options = opts();
    options.skim_head_bytes = 1_024;
    let mut store = Store::open_in_memory().unwrap();
    let runtime = runtime(&CodexAdapter);
    let (meta, files) = meta_ingest(
        &CodexAdapter,
        &runtime,
        root.path(),
        &mut store,
        &options,
        &Instrument::new(),
    )
    .unwrap();
    assert_eq!(meta.files_unidentified, 0);
    assert_eq!(meta.sessions, 1);
    assert_eq!(files.len(), 1);
}

#[test]
fn unidentified_streams_are_reported_and_never_silently_visited() {
    let root = TempDir::new().unwrap();
    write_lines(
        &root
            .path()
            .join("sessions/2026/08/25/rollout-no-header.jsonl"),
        &[json!({
            "timestamp": "2026-08-25T02:35:03.577Z",
            "ordinal": 0,
            "type": "event_msg",
            "payload": {"type": "task_started"}
        })],
    );
    let mut store = Store::open_in_memory().unwrap();
    let runtime = runtime(&CodexAdapter);
    let (meta, files) = meta_ingest(
        &CodexAdapter,
        &runtime,
        root.path(),
        &mut store,
        &opts(),
        &Instrument::new(),
    )
    .unwrap();
    assert_eq!(meta.files_unidentified, 1);
    assert!(files.is_empty());
    let cursors: i64 = store
        .conn()
        .query_row("SELECT COUNT(*) FROM ingest_cursor", [], |row| row.get(0))
        .unwrap();
    assert_eq!(cursors, 0);
}

#[test]
fn rewrite_with_new_ids_replaces_the_source_and_tombstones_old_rows() {
    let root = TempDir::new().unwrap();
    let path = root.path().join("projects/p/session-1.jsonl");
    write_lines(
        &path,
        &[
            claude_user("session-1", "old-u", "old searchable marker", "/w/project"),
            claude_edit("session-1", "old-a", "/w/project/old.rs"),
        ],
    );
    let mut store = Store::open_in_memory().unwrap();
    let runtime = runtime(&ClaudeCodeAdapter);
    let options = opts();
    let (_, files) = meta_ingest(
        &ClaudeCodeAdapter,
        &runtime,
        root.path(),
        &mut store,
        &options,
        &Instrument::new(),
    )
    .unwrap();
    stream_backfill(
        &ClaudeCodeAdapter,
        &runtime,
        root.path(),
        &mut store,
        &files,
        &options,
        &Instrument::new(),
    )
    .unwrap();

    write_lines(
        &path,
        &[claude_user(
            "session-1",
            "new-u",
            "new searchable marker",
            "/w/project",
        )],
    );
    let report = stream_backfill(
        &ClaudeCodeAdapter,
        &runtime,
        root.path(),
        &mut store,
        &files,
        &options,
        &Instrument::new(),
    )
    .unwrap();
    assert_eq!(report.generations_restarted, 1);
    assert!(queries::search(store.conn(), "old", 10).unwrap().is_empty());
    assert_eq!(queries::search(store.conn(), "new", 10).unwrap().len(), 1);
    let (messages, edits): (i64, i64) = store
        .conn()
        .query_row(
            "SELECT (SELECT COUNT(*) FROM message), (SELECT COUNT(*) FROM file_edit)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((messages, edits), (1, 0));
}

#[test]
fn incomplete_rewrite_keeps_the_previous_generation_visible_until_swap() {
    let root = TempDir::new().unwrap();
    let path = root.path().join("projects/p/session-1.jsonl");
    write_lines(
        &path,
        &[claude_user(
            "session-1",
            "old-u",
            "oldgeneration",
            "/w/project",
        )],
    );
    let mut store = Store::open_in_memory().unwrap();
    let runtime = runtime(&ClaudeCodeAdapter);
    let options = opts();
    let (_, files) = meta_ingest(
        &ClaudeCodeAdapter,
        &runtime,
        root.path(),
        &mut store,
        &options,
        &Instrument::new(),
    )
    .unwrap();
    stream_backfill(
        &ClaudeCodeAdapter,
        &runtime,
        root.path(),
        &mut store,
        &files,
        &options,
        &Instrument::new(),
    )
    .unwrap();

    std::fs::write(
        &path,
        format!(
            "{}\n{{\"type\":\"future",
            claude_user("session-1", "new-u", "newgeneration", "/w/project")
        ),
    )
    .unwrap();
    let staged = stream_backfill(
        &ClaudeCodeAdapter,
        &runtime,
        root.path(),
        &mut store,
        &files,
        &options,
        &Instrument::new(),
    )
    .unwrap();
    assert_eq!(staged.files_incomplete, 1);
    assert_eq!(
        queries::search(store.conn(), "oldgeneration", 5)
            .unwrap()
            .len(),
        1
    );
    assert!(
        queries::search(store.conn(), "newgeneration", 5)
            .unwrap()
            .is_empty()
    );

    let mut append = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    writeln!(append, "-record\",\"sessionId\":\"session-1\"}}").unwrap();
    let activated = stream_backfill(
        &ClaudeCodeAdapter,
        &runtime,
        root.path(),
        &mut store,
        &files,
        &options,
        &Instrument::new(),
    )
    .unwrap();
    assert_eq!(activated.files_incomplete, 0);
    assert!(
        queries::search(store.conn(), "oldgeneration", 5)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        queries::search(store.conn(), "newgeneration", 5)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn expired_checkpoint_advances_past_an_abandoned_staged_generation() {
    let root = TempDir::new().unwrap();
    let relative = "projects/p/session-1.jsonl";
    let path = root.path().join(relative);
    write_lines(
        &path,
        &[claude_user("session-1", "old-u", "old", "/w/project")],
    );
    let mut store = Store::open_in_memory().unwrap();
    let runtime = runtime(&ClaudeCodeAdapter);
    let options = opts();
    let (_, files) = meta_ingest(
        &ClaudeCodeAdapter,
        &runtime,
        root.path(),
        &mut store,
        &options,
        &Instrument::new(),
    )
    .unwrap();
    stream_backfill(
        &ClaudeCodeAdapter,
        &runtime,
        root.path(),
        &mut store,
        &files,
        &options,
        &Instrument::new(),
    )
    .unwrap();

    std::fs::write(
        &path,
        format!(
            "{}\n{{\"type\":\"torn",
            claude_user("session-1", "staged-u", "staged", "/w/project")
        ),
    )
    .unwrap();
    stream_backfill(
        &ClaudeCodeAdapter,
        &runtime,
        root.path(),
        &mut store,
        &files,
        &options,
        &Instrument::new(),
    )
    .unwrap();
    let staged = queries::cursor_state(store.conn(), &runtime.id, relative)
        .unwrap()
        .unwrap();
    assert_eq!(
        (staged.active_generation, staged.highest_generation),
        (1, 2)
    );

    // Simulate an expired normalizer checkpoint and incompatible abandoned
    // staged output. Recovery must allocate generation 3 rather than trying
    // to reuse and collide with generation 2.
    store
        .conn()
        .execute(
            "UPDATE source_op SET op_json = '{}' WHERE cursor_id = ?1 AND generation = 2",
            [staged.id],
        )
        .unwrap();
    store
        .conn()
        .execute(
            "UPDATE ingest_cursor SET checkpoint = 'expired' WHERE id = ?1",
            [staged.id],
        )
        .unwrap();
    write_lines(
        &path,
        &[claude_user(
            "session-1",
            "recovered-u",
            "recovered",
            "/w/project",
        )],
    );
    stream_backfill(
        &ClaudeCodeAdapter,
        &runtime,
        root.path(),
        &mut store,
        &files,
        &options,
        &Instrument::new(),
    )
    .unwrap();
    let recovered = queries::cursor_state(store.conn(), &runtime.id, relative)
        .unwrap()
        .unwrap();
    assert_eq!(recovered.active_generation, 3);
    assert_eq!(
        queries::search(store.conn(), "recovered", 5).unwrap().len(),
        1
    );
}

#[test]
fn two_source_files_share_one_session_without_ordinal_collisions() {
    let root = TempDir::new().unwrap();
    write_lines(
        &root.path().join("projects/a/one.jsonl"),
        &[claude_user("shared", "u1", "from one", "/w/project")],
    );
    write_lines(
        &root.path().join("projects/b/two.jsonl"),
        &[claude_user("shared", "u2", "from two", "/w/project")],
    );
    let mut store = Store::open_in_memory().unwrap();
    let runtime = runtime(&ClaudeCodeAdapter);
    let options = opts();
    let (meta, files) = meta_ingest(
        &ClaudeCodeAdapter,
        &runtime,
        root.path(),
        &mut store,
        &options,
        &Instrument::new(),
    )
    .unwrap();
    assert_eq!(meta.sessions, 1);
    assert_eq!(files.len(), 2);
    stream_backfill(
        &ClaudeCodeAdapter,
        &runtime,
        root.path(),
        &mut store,
        &files,
        &options,
        &Instrument::new(),
    )
    .unwrap();
    let page = queries::timeline_page(store.conn(), &files[0].session_id, None, 10).unwrap();
    assert_eq!(page.len(), 2);
    assert_eq!(
        page.iter().map(|row| row.ordinal).collect::<Vec<_>>(),
        [0, 1]
    );
}

#[test]
fn claude_subagent_is_a_child_session_not_a_second_parent_file() {
    let root = TempDir::new().unwrap();
    write_lines(
        &root.path().join("projects/p/parent.jsonl"),
        &[claude_user("parent", "parent-u", "parent", "/w/project")],
    );
    let mut child = claude_user("parent", "child-u", "child", "/w/project");
    child["agentId"] = Value::String("child-7".into());
    child["isSidechain"] = Value::Bool(true);
    write_lines(
        &root
            .path()
            .join("projects/p/parent/subagents/task/agent-child-7.jsonl"),
        &[child],
    );
    let mut store = Store::open_in_memory().unwrap();
    let runtime = runtime(&ClaudeCodeAdapter);
    let options = opts();
    let (meta, files) = meta_ingest(
        &ClaudeCodeAdapter,
        &runtime,
        root.path(),
        &mut store,
        &options,
        &Instrument::new(),
    )
    .unwrap();
    assert_eq!(meta.sessions, 2);
    let parent_id = ids::session_id(&runtime.id, "parent");
    let child_id = ids::session_id(&runtime.id, "child-7");
    let child = queries::session(store.conn(), &child_id).unwrap().unwrap();
    assert_eq!(child.parent_session_id.as_deref(), Some(parent_id.as_str()));
    assert_eq!(child.root_session_id, parent_id);
    assert_eq!(child.role, SessionRole::Subagent);
    stream_backfill(
        &ClaudeCodeAdapter,
        &runtime,
        root.path(),
        &mut store,
        &files,
        &options,
        &Instrument::new(),
    )
    .unwrap();
    assert_eq!(
        queries::timeline_page(store.conn(), &child_id, None, 10)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn rotating_audit_detects_same_size_rewrite_outside_the_tail_anchor() {
    let root = TempDir::new().unwrap();
    let path = root.path().join("projects/p/session-1.jsonl");
    let lines: Vec<Value> = (0..100)
        .map(|index| {
            claude_user(
                "session-1",
                &format!("u-{index}"),
                if index == 0 { "oldmarker" } else { "paddingxx" },
                "/w/project",
            )
        })
        .collect();
    write_lines(&path, &lines);
    assert!(std::fs::metadata(&path).unwrap().len() > 16 * 1024);

    let mut store = Store::open_in_memory().unwrap();
    let runtime = runtime(&ClaudeCodeAdapter);
    let options = opts();
    let (_, files) = meta_ingest(
        &ClaudeCodeAdapter,
        &runtime,
        root.path(),
        &mut store,
        &options,
        &Instrument::new(),
    )
    .unwrap();
    stream_backfill(
        &ClaudeCodeAdapter,
        &runtime,
        root.path(),
        &mut store,
        &files,
        &options,
        &Instrument::new(),
    )
    .unwrap();

    let original = std::fs::read_to_string(&path).unwrap();
    let rewritten = original.replacen("oldmarker", "newmarker", 1);
    assert_eq!(original.len(), rewritten.len());
    std::fs::write(&path, rewritten).unwrap();
    let quiet = stream_backfill(
        &ClaudeCodeAdapter,
        &runtime,
        root.path(),
        &mut store,
        &files,
        &options,
        &Instrument::new(),
    )
    .unwrap();
    assert_eq!(quiet.records, 0);
    assert_eq!(
        queries::search(store.conn(), "oldmarker", 5).unwrap().len(),
        1
    );

    let mut audited = options.clone();
    audited.audit_files_per_run = 1;
    let report = stream_backfill(
        &ClaudeCodeAdapter,
        &runtime,
        root.path(),
        &mut store,
        &files,
        &audited,
        &Instrument::new(),
    )
    .unwrap();
    assert_eq!(report.files_audited, 1);
    assert_eq!(report.generations_restarted, 1);
    assert!(
        queries::search(store.conn(), "oldmarker", 5)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        queries::search(store.conn(), "newmarker", 5).unwrap().len(),
        1
    );
}

#[test]
fn forced_replay_is_idempotent_for_edits_observations_and_drift() {
    let root = TempDir::new().unwrap();
    let path = root.path().join("projects/p/session-1.jsonl");
    write_lines(
        &path,
        &[
            claude_user("session-1", "u1", "hello", "/w/project"),
            claude_edit("session-1", "a1", "/w/project/lib.rs"),
            json!({"type": "future-record", "sessionId": "session-1"}),
        ],
    );
    let mut store = Store::open_in_memory().unwrap();
    let runtime = runtime(&ClaudeCodeAdapter);
    let options = opts();
    let (_, files) = meta_ingest(
        &ClaudeCodeAdapter,
        &runtime,
        root.path(),
        &mut store,
        &options,
        &Instrument::new(),
    )
    .unwrap();
    stream_backfill(
        &ClaudeCodeAdapter,
        &runtime,
        root.path(),
        &mut store,
        &files,
        &options,
        &Instrument::new(),
    )
    .unwrap();
    let counts = |store: &Store| {
        store
            .conn()
            .query_row(
                "SELECT (SELECT COUNT(*) FROM workspace_obs),
                        (SELECT COUNT(*) FROM file_edit),
                        (SELECT COALESCE(SUM(count),0) FROM drift_agg)",
                [],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )
            .unwrap()
    };
    assert_eq!(counts(&store), (1, 1, 1));
    let mut audited = options.clone();
    audited.audit_files_per_run = 1;
    stream_backfill(
        &ClaudeCodeAdapter,
        &runtime,
        root.path(),
        &mut store,
        &files,
        &audited,
        &Instrument::new(),
    )
    .unwrap();
    assert_eq!(counts(&store), (1, 1, 1));
}

#[test]
fn incomplete_trailing_record_keeps_the_session_streaming() {
    let root = TempDir::new().unwrap();
    let path = root.path().join("projects/p/session-1.jsonl");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        format!(
            "{}\n{{\"type\":\"user\"",
            claude_user("session-1", "u1", "hello", "/w/project")
        ),
    )
    .unwrap();
    let mut store = Store::open_in_memory().unwrap();
    let runtime = runtime(&ClaudeCodeAdapter);
    let options = opts();
    let (_, files) = meta_ingest(
        &ClaudeCodeAdapter,
        &runtime,
        root.path(),
        &mut store,
        &options,
        &Instrument::new(),
    )
    .unwrap();
    let report = stream_backfill(
        &ClaudeCodeAdapter,
        &runtime,
        root.path(),
        &mut store,
        &files,
        &options,
        &Instrument::new(),
    )
    .unwrap();
    assert_eq!(report.files_incomplete, 1);
    let session = queries::session(store.conn(), &files[0].session_id)
        .unwrap()
        .unwrap();
    assert_eq!(session.stream_state, StreamState::Streaming);
    let messages: i64 = store
        .conn()
        .query_row("SELECT COUNT(*) FROM message", [], |row| row.get(0))
        .unwrap();
    assert_eq!(messages, 0, "an incomplete first generation stays staged");
}

#[test]
fn codex_split_resume_matches_one_shot_projection() {
    let one_root = TempDir::new().unwrap();
    let split_root = TempDir::new().unwrap();
    let spec = CorpusSpec::new(Agent::Codex, 1, 30, 42);
    generate(&spec, one_root.path()).unwrap();
    generate(&spec, split_root.path()).unwrap();
    let one_path = only_path(one_root.path(), ".jsonl");
    let split_path = only_path(split_root.path(), ".jsonl");
    let full = std::fs::read_to_string(&split_path).unwrap();
    let lines: Vec<&str> = full.lines().collect();
    assert!(lines[9].contains("\"role\":\"assistant\""));
    assert!(lines[10].contains("token_count"));
    std::fs::write(&split_path, format!("{}\n", lines[..10].join("\n"))).unwrap();

    let runtime = runtime(&CodexAdapter);
    let options = opts();
    let mut one_store = Store::open_in_memory().unwrap();
    let (_, one_files) = meta_ingest(
        &CodexAdapter,
        &runtime,
        one_root.path(),
        &mut one_store,
        &options,
        &Instrument::new(),
    )
    .unwrap();
    stream_backfill(
        &CodexAdapter,
        &runtime,
        one_root.path(),
        &mut one_store,
        &one_files,
        &options,
        &Instrument::new(),
    )
    .unwrap();

    let mut split_store = Store::open_in_memory().unwrap();
    let (_, split_files) = meta_ingest(
        &CodexAdapter,
        &runtime,
        split_root.path(),
        &mut split_store,
        &options,
        &Instrument::new(),
    )
    .unwrap();
    stream_backfill(
        &CodexAdapter,
        &runtime,
        split_root.path(),
        &mut split_store,
        &split_files,
        &options,
        &Instrument::new(),
    )
    .unwrap();
    let mut append = std::fs::OpenOptions::new()
        .append(true)
        .open(&split_path)
        .unwrap();
    writeln!(append, "{}", lines[10..].join("\n")).unwrap();
    stream_backfill(
        &CodexAdapter,
        &runtime,
        split_root.path(),
        &mut split_store,
        &split_files,
        &options,
        &Instrument::new(),
    )
    .unwrap();

    let one_session = &one_files[0].session_id;
    let split_session = &split_files[0].session_id;
    assert_eq!(one_session, split_session);
    assert_eq!(
        queries::timeline_page(one_store.conn(), one_session, None, 1_000).unwrap(),
        queries::timeline_page(split_store.conn(), split_session, None, 1_000).unwrap(),
    );
    let buckets = |store: &Store| {
        let mut stmt = store
            .conn()
            .prepare(
                "SELECT hour_ts, model, in_tokens, out_tokens, cache_r_tokens,
                        cache_w_tokens, think_tokens, message_count, tool_call_count
                 FROM usage_bucket ORDER BY hour_ts, model",
            )
            .unwrap();
        stmt.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<i64>>(2)?,
                row.get::<_, Option<i64>>(3)?,
                row.get::<_, Option<i64>>(4)?,
                row.get::<_, Option<i64>>(5)?,
                row.get::<_, Option<i64>>(6)?,
                row.get::<_, i64>(7)?,
                row.get::<_, i64>(8)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
    };
    assert_eq!(buckets(&one_store), buckets(&split_store));

    // Keep the source path used above live until all reads are complete.
    assert!(one_path.exists());
}
