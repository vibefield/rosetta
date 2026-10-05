use std::io::Write;
use std::path::{Path, PathBuf};

use rosetta_adapter_antigravity::AntigravityAdapter;
use rosetta_adapter_claude_code::ClaudeCodeAdapter;
use rosetta_adapter_codex::CodexAdapter;
use rosetta_adapter_copilot::CopilotAdapter;
use rosetta_adapter_support::AgentAdapter;
use rosetta_bench_corpus::{Agent, CorpusSpec, generate};
use rosetta_capture::Ledger;
use rosetta_core::ids;
use rosetta_core::rows::{RuntimeRow, SessionRole, StreamState};
use rosetta_core::timeline::TimelineBody;
use rosetta_pipeline::{
    IngestOptions, Instrument, Metric, meta_ingest, meta_ingest_into, rebuild_runtime,
    stream_backfill, stream_backfill_into,
};
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
        label: None,
        provenance: Default::default(),
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

fn claude_assistant_block(session: &str, response: &str, uuid: &str, content: Value) -> Value {
    json!({
        "parentUuid": null,
        "isSidechain": false,
        "message": {
            "content": [content],
            "id": response,
            "model": "claude-opus-5",
            "role": "assistant",
            "stop_reason": "tool_use",
            "stop_sequence": null,
            "type": "message",
            "usage": {
                "input_tokens": 100,
                "output_tokens": 20,
                "cache_creation_input_tokens": 7,
                "cache_read_input_tokens": 9,
                "cache_creation": {},
                "inference_geo": null,
                "service_tier": "standard",
                "output_tokens_details": {"thinking_tokens": 5}
            }
        },
        "requestId": "req-split-response",
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
fn claude_split_response_survives_incremental_checkpoint_without_usage_duplication() {
    let root = TempDir::new().unwrap();
    let path = root.path().join("projects/p/session-1.jsonl");
    let response = "msg-shared-response";
    write_lines(
        &path,
        &[claude_assistant_block(
            "session-1",
            response,
            "11111111-1111-1111-1111-111111111101",
            json!({"type": "thinking", "thinking": "first", "signature": "sig"}),
        )],
    );

    let runtime = runtime(&ClaudeCodeAdapter);
    let options = opts();
    let mut store = Store::open_in_memory().unwrap();
    let (_, files) = meta_ingest(
        &ClaudeCodeAdapter,
        &runtime,
        root.path(),
        &mut store,
        &options,
        &Instrument::new(),
    )
    .unwrap();
    let first = stream_backfill(
        &ClaudeCodeAdapter,
        &runtime,
        root.path(),
        &mut store,
        &files,
        &options,
        &Instrument::new(),
    )
    .unwrap();
    assert_eq!(first.hard_errors, 0);
    assert_eq!(first.stats.messages_inserted, 1);

    let mut transcript = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    for line in [
        claude_assistant_block(
            "session-1",
            response,
            "11111111-1111-1111-1111-111111111102",
            json!({"type": "text", "text": "second"}),
        ),
        claude_assistant_block(
            "session-1",
            response,
            "11111111-1111-1111-1111-111111111103",
            json!({"type": "tool_use", "id": "toolu-split", "name": "Bash",
                   "input": {"command": "true"}}),
        ),
    ] {
        writeln!(transcript, "{line}").unwrap();
    }
    drop(transcript);

    // A new stream pass restores adapter state from the persisted checkpoint.
    let appended = stream_backfill(
        &ClaudeCodeAdapter,
        &runtime,
        root.path(),
        &mut store,
        &files,
        &options,
        &Instrument::new(),
    )
    .unwrap();
    assert_eq!(appended.hard_errors, 0);
    assert_eq!(appended.stats.messages_inserted, 2);

    let mut statement = store
        .conn()
        .prepare(
            "SELECT response_id, block_index, type, in_tokens, out_tokens
             FROM message WHERE session_id = ?1 ORDER BY ordinal",
        )
        .unwrap();
    let rows = statement
        .query_map([&files[0].session_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<i64>>(3)?,
                row.get::<_, Option<i64>>(4)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(
        rows,
        [
            (
                response.to_owned(),
                0,
                "thinking".to_owned(),
                Some(100),
                // Anthropic's 20 output tokens include the 5 thinking tokens
                // Claude Code breaks out; the buckets are exclusive.
                Some(15)
            ),
            (response.to_owned(), 1, "agent".to_owned(), None, None),
            (response.to_owned(), 2, "tool_use".to_owned(), None, None),
        ]
    );
    let bucket: (i64, i64, i64) = store
        .conn()
        .query_row(
            "SELECT SUM(in_tokens), SUM(out_tokens), SUM(think_tokens) FROM usage_bucket",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    // One response's usage once: 100 in, 15 visible out, 5 thinking.
    assert_eq!(bucket, (100, 15, 5));
}

#[test]
fn claude_replayed_branch_coalesces_one_tool_without_reusing_block_indexes() {
    let root = TempDir::new().unwrap();
    let path = root.path().join("projects/p/session-1.jsonl");
    let response = "msg-revisited-response";
    let repeated_uuid = "11111111-1111-1111-1111-111111111202";
    let repeated_tool = json!({
        "type": "tool_use",
        "id": "toolu-revisited",
        "name": "Bash",
        "input": {"command": "true"}
    });
    let mut replayed =
        claude_assistant_block("session-1", response, repeated_uuid, repeated_tool.clone());
    replayed["parentUuid"] = json!("new-branch-parent");
    write_lines(
        &path,
        &[
            claude_assistant_block(
                "session-1",
                response,
                "11111111-1111-1111-1111-111111111201",
                json!({"type": "text", "text": "first block"}),
            ),
            claude_assistant_block("session-1", response, repeated_uuid, repeated_tool),
            claude_assistant_block(
                "session-1",
                "msg-interrupting-response",
                "11111111-1111-1111-1111-111111111203",
                json!({"type": "text", "text": "branch work"}),
            ),
            replayed,
            claude_assistant_block(
                "session-1",
                response,
                "11111111-1111-1111-1111-111111111204",
                json!({"type": "tool_use", "id": "toolu-late", "name": "Read",
                       "input": {"file_path": "/w/project/src/lib.rs"}}),
            ),
        ],
    );

    let runtime = runtime(&ClaudeCodeAdapter);
    let options = opts();
    let mut store = Store::open_in_memory_for_rebuild().unwrap();
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
    assert_eq!(report.hard_errors, 0);

    let mut statement = store
        .conn()
        .prepare(
            "SELECT block_index, type, tool_native_id, parent_uuid
             FROM message
             WHERE session_id = ?1 AND response_id = ?2
             ORDER BY block_index",
        )
        .unwrap();
    let rows = statement
        .query_map(rusqlite::params![files[0].session_id, response], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(
        rows,
        [
            (0, "agent".into(), None, None),
            (
                1,
                "tool_use".into(),
                Some("toolu-revisited".into()),
                Some("new-branch-parent".into())
            ),
            (2, "tool_use".into(), Some("toolu-late".into()), None),
        ]
    );
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
fn codex_state_titles_merge_with_rollouts_and_update_incrementally() {
    let root = TempDir::new().unwrap();
    let native = "01a036c5-6e18-73f3-8c07-d4430763840b";
    write_lines(
        &root
            .path()
            .join("sessions/2026/08/25/rollout-session.jsonl"),
        &[json!({
            "timestamp": "2026-08-25T02:35:03.577Z",
            "type": "session_meta",
            "payload": {
                "id": native,
                "timestamp": "2026-08-25T02:35:03.577Z",
                "cwd": "/w/project",
                "originator": "codex_exec",
                "cli_version": "0.150.1",
                "source": "exec",
                "model_provider": "openai"
            }
        })],
    );
    let db = rusqlite::Connection::open(root.path().join("state_5.sqlite")).unwrap();
    db.execute_batch(
        "CREATE TABLE threads (
           id TEXT PRIMARY KEY, rollout_path TEXT NOT NULL,
           created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL,
           source TEXT NOT NULL, cwd TEXT NOT NULL, title TEXT NOT NULL,
           cli_version TEXT NOT NULL DEFAULT '', name TEXT
         );",
    )
    .unwrap();
    db.execute(
        "INSERT INTO threads
           (id, rollout_path, created_at, updated_at, source, cwd, title, cli_version, name)
         VALUES (?1, 'rollout', 10, 11, 'exec', '/w/project', ?2, '0.150.1', ?3)",
        rusqlite::params![
            native,
            "a very long first prompt that is only a fallback",
            "Review Rosetta codebase"
        ],
    )
    .unwrap();
    drop(db);

    let runtime = runtime(&CodexAdapter);
    let options = opts();
    let mut store = Store::open_in_memory().unwrap();
    let (meta, files) = meta_ingest(
        &CodexAdapter,
        &runtime,
        root.path(),
        &mut store,
        &options,
        &Instrument::new(),
    )
    .unwrap();
    assert_eq!(meta.sessions, 1, "database and rollout share one native id");
    assert_eq!(files.len(), 2, "one rollout plus one virtual state source");
    let session_id = ids::session_id(&runtime.id, native);
    assert_eq!(
        queries::session(store.conn(), &session_id)
            .unwrap()
            .unwrap()
            .title
            .as_deref(),
        Some("Review Rosetta codebase")
    );

    // Rollout normalization emits a title-less SessionMeta. It must not
    // regress the richer state-database metadata.
    stream_backfill(
        &CodexAdapter,
        &runtime,
        root.path(),
        &mut store,
        &files,
        &options,
        &Instrument::new(),
    )
    .unwrap();
    assert_eq!(
        queries::session(store.conn(), &session_id)
            .unwrap()
            .unwrap()
            .title
            .as_deref(),
        Some("Review Rosetta codebase")
    );

    let db = rusqlite::Connection::open(root.path().join("state_5.sqlite")).unwrap();
    db.execute(
        "UPDATE threads SET name = 'Onboard to Rosetta codebase', updated_at = 12 WHERE id = ?1",
        [native],
    )
    .unwrap();
    drop(db);
    let (_, updated_files) = meta_ingest(
        &CodexAdapter,
        &runtime,
        root.path(),
        &mut store,
        &options,
        &Instrument::new(),
    )
    .unwrap();
    let codex_title_meta: (Option<String>, i64) = store
        .conn()
        .query_row(
            "SELECT title, title_rank FROM session WHERE id = ?1",
            [&session_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        codex_title_meta,
        (Some("Onboard to Rosetta codebase".into()), 3)
    );
    assert_eq!(
        queries::session(store.conn(), &session_id)
            .unwrap()
            .unwrap()
            .title
            .as_deref(),
        Some("Onboard to Rosetta codebase")
    );
    stream_backfill(
        &CodexAdapter,
        &runtime,
        root.path(),
        &mut store,
        &updated_files,
        &options,
        &Instrument::new(),
    )
    .unwrap();
}

#[test]
fn copilot_workspace_title_merges_updates_and_survives_events() {
    let root = TempDir::new().unwrap();
    let native = "79e2bd43-58b9-43ef-8e18-664897f8f0a9";
    let session_root = root.path().join("session-state").join(native);
    write_lines(
        &session_root.join("events.jsonl"),
        &[json!({
            "id": "event-1",
            "timestamp": "2026-08-25T05:27:58.278Z",
            "parentId": null,
            "type": "session.start",
            "data": {
                "sessionId": native,
                "version": 1,
                "producer": "copilot-agent",
                "copilotVersion": "1.0.80",
                "startTime": "2026-08-25T05:27:58.248Z",
                "context": {"cwd": "/w/project"}
            }
        })],
    );
    std::fs::write(
        session_root.join("workspace.yaml"),
        format!(
            "id: {native}\ncwd: /w/project\nname: Codebase Overview Session\n\
             user_named: false\ncreated_at: 2026-08-25T05:27:58.262Z\n\
             updated_at: 2026-08-25T05:27:58.417Z\n"
        ),
    )
    .unwrap();

    let runtime = runtime(&CopilotAdapter);
    let options = opts();
    let mut store = Store::open_in_memory().unwrap();
    let (meta, files) = meta_ingest(
        &CopilotAdapter,
        &runtime,
        root.path(),
        &mut store,
        &options,
        &Instrument::new(),
    )
    .unwrap();
    assert_eq!(meta.sessions, 1);
    assert_eq!(files.len(), 2);
    let session_id = ids::session_id(&runtime.id, native);
    assert_eq!(
        queries::session(store.conn(), &session_id)
            .unwrap()
            .unwrap()
            .title
            .as_deref(),
        Some("Codebase Overview Session")
    );

    stream_backfill(
        &CopilotAdapter,
        &runtime,
        root.path(),
        &mut store,
        &files,
        &options,
        &Instrument::new(),
    )
    .unwrap();
    assert_eq!(
        queries::session(store.conn(), &session_id)
            .unwrap()
            .unwrap()
            .title
            .as_deref(),
        Some("Codebase Overview Session")
    );

    std::fs::write(
        session_root.join("workspace.yaml"),
        format!(
            "id: {native}\ncwd: /w/project\nname: Explicit renamed session\n\
             user_named: true\nupdated_at: 2026-08-25T05:28:58.417Z\n"
        ),
    )
    .unwrap();
    let (_, updated_files) = meta_ingest(
        &CopilotAdapter,
        &runtime,
        root.path(),
        &mut store,
        &options,
        &Instrument::new(),
    )
    .unwrap();
    let copilot_title_meta: (Option<String>, i64) = store
        .conn()
        .query_row(
            "SELECT title, title_rank FROM session WHERE id = ?1",
            [&session_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        copilot_title_meta,
        (Some("Explicit renamed session".into()), 3)
    );
    assert_eq!(
        queries::session(store.conn(), &session_id)
            .unwrap()
            .unwrap()
            .title
            .as_deref(),
        Some("Explicit renamed session")
    );
    stream_backfill(
        &CopilotAdapter,
        &runtime,
        root.path(),
        &mut store,
        &updated_files,
        &options,
        &Instrument::new(),
    )
    .unwrap();

    // A partially written companion is unidentified, but cannot erase the
    // last committed title or make the title-less event source authoritative.
    std::fs::write(session_root.join("workspace.yaml"), "id: '").unwrap();
    let (malformed, _) = meta_ingest(
        &CopilotAdapter,
        &runtime,
        root.path(),
        &mut store,
        &options,
        &Instrument::new(),
    )
    .unwrap();
    assert_eq!(malformed.files_unidentified, 1);
    assert_eq!(
        queries::session(store.conn(), &session_id)
            .unwrap()
            .unwrap()
            .title
            .as_deref(),
        Some("Explicit renamed session")
    );
}

#[test]
fn antigravity_checkpoint_title_is_bounded_ranked_and_replay_stable() {
    let root = TempDir::new().unwrap();
    let native = "3d4449c2-023a-41bf-9c73-5ef10ffab9b6";
    let transcript = root
        .path()
        .join("brain")
        .join(native)
        .join(".system_generated/logs/transcript.jsonl");
    let checkpoint = |step: u64, objective: &str| {
        json!({
            "step_index": step,
            "source": "SYSTEM",
            "type": "CHECKPOINT",
            "status": "DONE",
            "created_at": format!("2026-08-25T05:10:{step:02}Z"),
            "content": format!(
                "{{{{ CHECKPOINT {step} }}}}\n# USER Objective:\n{objective}\n\n# User Requests\nnot a title"
            )
        })
    };
    write_lines(
        &transcript,
        &[
            json!({
                "step_index": 0,
                "source": "USER_EXPLICIT",
                "type": "USER_INPUT",
                "status": "DONE",
                "created_at": "2026-08-25T05:10:00Z",
                "content": "x".repeat(66_000)
            }),
            checkpoint(1, "First generated objective"),
            checkpoint(2, "Latest generated objective"),
            json!({
                "step_index": 3,
                "source": "MODEL",
                "type": "GENERIC",
                "status": "DONE",
                "created_at": "2026-08-25T05:10:03Z",
                "content": "y".repeat(20_000)
            }),
        ],
    );

    let db_path = root.path().join("conversation_summaries.db");
    let db = rusqlite::Connection::open(&db_path).unwrap();
    db.execute_batch(
        "CREATE TABLE conversation_summaries (
           conversation_id TEXT PRIMARY KEY,
           title TEXT NOT NULL,
           preview TEXT NOT NULL,
           last_modified_time TEXT NOT NULL,
           workspace_uris TEXT NOT NULL,
           parent_conversation_id TEXT NOT NULL
         );",
    )
    .unwrap();
    db.execute(
        "INSERT INTO conversation_summaries VALUES
           (?1, '', 'Preview stays summary', '2026-08-25 05:10:02+00:00',
            'file:///w/project', '')",
        [native],
    )
    .unwrap();
    drop(db);

    let runtime = runtime(&AntigravityAdapter);
    let options = opts();

    // Both objectives sit after the 64 KiB head and before the 16 KiB tail.
    // With fallback disabled meta remains honest, and complete streaming still
    // repairs the title.
    let mut no_fallback = options.clone();
    no_fallback.skim_fallback_bytes = 0;
    let mut stream_repairs = Store::open_in_memory().unwrap();
    let (_, repair_files) = meta_ingest(
        &AntigravityAdapter,
        &runtime,
        root.path(),
        &mut stream_repairs,
        &no_fallback,
        &Instrument::new(),
    )
    .unwrap();
    let session_id = ids::session_id(&runtime.id, native);
    assert_eq!(
        queries::session(stream_repairs.conn(), &session_id)
            .unwrap()
            .unwrap()
            .title,
        None
    );
    stream_backfill(
        &AntigravityAdapter,
        &runtime,
        root.path(),
        &mut stream_repairs,
        &repair_files,
        &no_fallback,
        &Instrument::new(),
    )
    .unwrap();
    assert_eq!(
        queries::session(stream_repairs.conn(), &session_id)
            .unwrap()
            .unwrap()
            .title
            .as_deref(),
        Some("Latest generated objective")
    );

    let instrument = Instrument::new();
    let ledger_dir = root.path().join("ledger");
    let mut ledger = Ledger::open(&ledger_dir).unwrap();
    let mut store = Store::open_in_memory().unwrap();
    let (_, files) = meta_ingest_into(
        &AntigravityAdapter,
        &runtime,
        root.path(),
        &mut store,
        &options,
        &instrument,
        Some(&mut ledger),
    )
    .unwrap();
    let session = queries::session(store.conn(), &session_id)
        .unwrap()
        .unwrap();
    assert_eq!(session.title.as_deref(), Some("Latest generated objective"));
    assert_eq!(session.summary.as_deref(), Some("Preview stays summary"));
    assert!(instrument.get(Metric::SkimFallbackBytes) > 0);
    assert!(instrument.get(Metric::SkimFallbackBytes) <= options.skim_fallback_bytes as u64);

    stream_backfill_into(
        &AntigravityAdapter,
        &runtime,
        root.path(),
        &mut store,
        &files,
        &options,
        &instrument,
        Some(&mut ledger),
    )
    .unwrap();
    assert_eq!(
        queries::session(store.conn(), &session_id)
            .unwrap()
            .unwrap()
            .title
            .as_deref(),
        Some("Latest generated objective")
    );

    // The sparse summary index's explicit title is stronger than any
    // checkpoint fallback, even when a newer checkpoint arrives later.
    let db = rusqlite::Connection::open(&db_path).unwrap();
    db.execute(
        "UPDATE conversation_summaries
         SET title = 'Explicit summary title',
             last_modified_time = '2026-08-25 05:10:03+00:00'
         WHERE conversation_id = ?1",
        [native],
    )
    .unwrap();
    drop(db);
    let (_, updated_files) = meta_ingest_into(
        &AntigravityAdapter,
        &runtime,
        root.path(),
        &mut store,
        &options,
        &instrument,
        Some(&mut ledger),
    )
    .unwrap();
    assert_eq!(
        queries::session(store.conn(), &session_id)
            .unwrap()
            .unwrap()
            .title
            .as_deref(),
        Some("Explicit summary title")
    );

    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&transcript)
        .unwrap();
    writeln!(
        file,
        "{}",
        checkpoint(4, "Newest but lower-confidence objective")
    )
    .unwrap();
    stream_backfill_into(
        &AntigravityAdapter,
        &runtime,
        root.path(),
        &mut store,
        &updated_files,
        &options,
        &instrument,
        Some(&mut ledger),
    )
    .unwrap();
    assert_eq!(
        queries::session(store.conn(), &session_id)
            .unwrap()
            .unwrap()
            .title
            .as_deref(),
        Some("Explicit summary title")
    );

    drop(ledger);
    let mut rebuilt = Store::open_in_memory().unwrap();
    let report = rebuild_runtime(
        &AntigravityAdapter,
        &runtime,
        &ledger_dir,
        &mut rebuilt,
        &Instrument::new(),
    )
    .unwrap();
    assert_eq!(report.hard_errors, 0);
    let replayed = queries::session(rebuilt.conn(), &session_id)
        .unwrap()
        .unwrap();
    assert_eq!(replayed.title.as_deref(), Some("Explicit summary title"));
    assert_eq!(replayed.summary.as_deref(), Some("Preview stays summary"));
}

#[test]
fn claude_title_between_skim_windows_is_bounded_then_stream_reconciled() {
    let root = TempDir::new().unwrap();
    let path = root.path().join("projects/p/session-1.jsonl");
    write_lines(
        &path,
        &[
            claude_user("session-1", "opening", &"x".repeat(66_000), "/w/project"),
            json!({
                "type": "custom-title",
                "customTitle": "Explicit boundary title",
                "sessionId": "session-1"
            }),
            json!({
                "type": "ai-title",
                "aiTitle": "Later generated title",
                "sessionId": "session-1"
            }),
            claude_user("session-1", "trailing", &"y".repeat(20_000), "/w/project"),
        ],
    );

    // The first title is beyond 64 KiB and the final title is more than
    // 16 KiB from EOF. The opt-in record scan recovers it without reading the
    // transcript body without limit, and custom outranks generated.
    let runtime = runtime(&ClaudeCodeAdapter);
    let options = opts();
    let instrument = Instrument::new();
    let mut meta_store = Store::open_in_memory().unwrap();
    let (_, files) = meta_ingest(
        &ClaudeCodeAdapter,
        &runtime,
        root.path(),
        &mut meta_store,
        &options,
        &instrument,
    )
    .unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(
        queries::session(meta_store.conn(), &files[0].session_id)
            .unwrap()
            .unwrap()
            .title
            .as_deref(),
        Some("Explicit boundary title")
    );
    assert!(instrument.get(Metric::SkimFallbackBytes) > 0);
    assert!(instrument.get(Metric::SkimFallbackBytes) <= options.skim_fallback_bytes as u64);

    // Bounded metadata is best-effort. Deliberately disable the fallback and
    // prove that complete streaming still converges on the same title.
    let mut no_fallback = options.clone();
    no_fallback.skim_fallback_bytes = 0;
    let mut stream_store = Store::open_in_memory().unwrap();
    let (_, files) = meta_ingest(
        &ClaudeCodeAdapter,
        &runtime,
        root.path(),
        &mut stream_store,
        &no_fallback,
        &Instrument::new(),
    )
    .unwrap();
    assert_eq!(
        queries::session(stream_store.conn(), &files[0].session_id)
            .unwrap()
            .unwrap()
            .title,
        None
    );
    stream_backfill(
        &ClaudeCodeAdapter,
        &runtime,
        root.path(),
        &mut stream_store,
        &files,
        &no_fallback,
        &Instrument::new(),
    )
    .unwrap();
    assert_eq!(
        queries::session(stream_store.conn(), &files[0].session_id)
            .unwrap()
            .unwrap()
            .title
            .as_deref(),
        Some("Explicit boundary title")
    );
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
fn expired_checkpoint_replays_past_a_lost_ram_generation() {
    let root = TempDir::new().unwrap();
    let relative = "projects/p/session-1.jsonl";
    let path = root.path().join(relative);
    write_lines(
        &path,
        &[claude_user("session-1", "old-u", "old", "/w/project")],
    );
    let projection = root.path().join("projection.sqlite");
    let mut store = Store::open(&projection).unwrap();
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

    // Simulate a process exit (which deliberately loses RAM staging) plus an
    // expired normalizer checkpoint. Recovery must replay from raw input and
    // allocate generation 3 rather than pretending SQLite retained gen 2.
    store
        .conn()
        .execute(
            "UPDATE ingest_cursor SET checkpoint = 'expired' WHERE id = ?1",
            [staged.id],
        )
        .unwrap();
    drop(store);
    let mut store = Store::open(&projection).unwrap();
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
    let parent = queries::session(store.conn(), &parent_id).unwrap().unwrap();
    assert_eq!(child.parent_session_id.as_deref(), Some(parent_id.as_str()));
    assert_eq!(child.root_session_id, parent_id);
    assert_eq!(child.role, SessionRole::Subagent);
    assert_eq!(child.cwd, parent.cwd);
    assert_eq!(child.project_id, parent.project_id);
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
fn claude_metadata_orders_nested_parents_after_sidecar_refinement() {
    for warm in [false, true] {
        let root = TempDir::new().unwrap();
        write_lines(
            &root.path().join("projects/p/root.jsonl"),
            &[claude_user("root", "root-u", "root", "/w/project")],
        );
        let mut store = Store::open_in_memory().unwrap();
        let runtime = runtime(&ClaudeCodeAdapter);
        let options = opts();
        if warm {
            meta_ingest(
                &ClaudeCodeAdapter,
                &runtime,
                root.path(),
                &mut store,
                &options,
                &Instrument::new(),
            )
            .unwrap();
        }

        // The transcript admits the child with its directory's root as a
        // fallback parent. Later sidecars refine it into a deeper tree, so
        // first-merge order puts the child before its newly named ancestors.
        let subagents = root.path().join("projects/p/root/subagents");
        let mut child = claude_user("root", "child-u", "child", "/w/project");
        child["agentId"] = json!("a-child");
        child["isSidechain"] = json!(true);
        write_lines(&subagents.join("agent-a-child.jsonl"), &[child]);
        for (id, parent) in [("a-child", "z-parent"), ("z-parent", "zz-grandparent")] {
            std::fs::write(
                subagents.join(format!("agent-{id}.meta.json")),
                json!({"agentType": "Explore", "parentAgentId": parent}).to_string(),
            )
            .unwrap();
        }

        let (meta, _) = meta_ingest(
            &ClaudeCodeAdapter,
            &runtime,
            root.path(),
            &mut store,
            &options,
            &Instrument::new(),
        )
        .unwrap_or_else(|error| panic!("warm={warm}: {error}"));
        assert_eq!(meta.sessions, 4);
        let root_id = ids::session_id(&runtime.id, "root");
        for (id, parent) in [
            ("a-child", "z-parent"),
            ("z-parent", "zz-grandparent"),
            ("zz-grandparent", "root"),
        ] {
            let row = queries::session(store.conn(), &ids::session_id(&runtime.id, id))
                .unwrap()
                .unwrap();
            assert_eq!(
                row.parent_session_id,
                Some(ids::session_id(&runtime.id, parent))
            );
            assert_eq!(row.root_session_id, root_id);
            assert_eq!(row.role, SessionRole::Subagent);
        }
    }
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
fn forced_replay_is_idempotent_for_edits_and_drift() {
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
                "SELECT (SELECT COUNT(*) FROM file_edit),
                        (SELECT COALESCE(SUM(count),0) FROM drift_agg)",
                [],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )
            .unwrap()
    };
    assert_eq!(counts(&store), (1, 1));
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
    assert_eq!(counts(&store), (1, 1));
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

/// One assistant `tool_use` opener plus its `tool_result`, stamped with an
/// explicit writer version.
fn claude_bash_round(session: &str, version: &str) -> Vec<Value> {
    vec![
        json!({
            "parentUuid": null,
            "isSidechain": false,
            "message": {
                "content": [{
                    "id": "msg_line_0001_tool",
                    "input": {"command": "echo rosetta-line"},
                    "name": "Bash",
                    "type": "tool_use"
                }],
                "id": "msg_line_0001",
                "model": "claude-opus-5",
                "role": "assistant",
                "stop_reason": "tool_use",
                "stop_sequence": null,
                "type": "message"
            },
            "type": "assistant",
            "uuid": "741ae4e4-7d74-4293-859d-a8d892fd4001",
            "timestamp": "2026-08-25T02:35:19.072Z",
            "userType": "external",
            "cwd": "/w/project",
            "sessionId": session,
            "version": version,
            "gitBranch": "main"
        }),
        json!({
            "parentUuid": "741ae4e4-7d74-4293-859d-a8d892fd4001",
            "isSidechain": false,
            "type": "user",
            "message": {"role": "user", "content": [{
                "tool_use_id": "msg_line_0001_tool",
                "type": "tool_result",
                "content": "rosetta-line",
                "is_error": false
            }]},
            "uuid": "54b4d4ef-7e2f-4b5c-bf99-3eac2383701a",
            "timestamp": "2026-08-25T02:35:19.141Z",
            "toolUseResult": {"stdout": "rosetta-line", "stderr": "", "interrupted": false},
            "userType": "external",
            "cwd": "/w/project",
            "sessionId": session,
            "version": version,
            "gitBranch": "main"
        }),
    ]
}

/// A Codex rollout whose session meta declares an explicit CLI version, then
/// runs one shell call to completion.
fn codex_exec_round(native: &str, cli_version: &str) -> Vec<Value> {
    vec![
        json!({
            "timestamp": "2026-08-25T02:35:03.577Z",
            "type": "session_meta",
            "payload": {
                "id": native,
                "timestamp": "2026-08-25T02:35:03.577Z",
                "cwd": "/w/project",
                "originator": "codex_exec",
                "cli_version": cli_version,
                "source": "exec",
                "model_provider": "openai"
            }
        }),
        json!({
            "timestamp": "2026-08-25T02:35:04.102Z",
            "ordinal": 8,
            "type": "response_item",
            "payload": {
                "type": "function_call",
                "id": "fc_line_0001",
                "name": "exec_command",
                "arguments": "{\"cmd\":\"echo rosetta-line\"}",
                "call_id": "call_line_0001"
            }
        }),
        json!({
            "timestamp": "2026-08-25T02:35:04.177Z",
            "ordinal": 9,
            "type": "event_msg",
            "payload": {
                "type": "item_completed",
                "thread_id": native,
                "item": {
                    "type": "CommandExecution",
                    "id": "call_line_0001",
                    "command": ["/bin/zsh", "-lc", "echo rosetta-line"],
                    "cwd": "file:///w/project",
                    "status": "completed",
                    "exit_code": 0
                },
                "started_at_ms": 1_787_625_304_176_i64,
                "completed_at_ms": 1_787_625_304_177_i64
            }
        }),
    ]
}

/// The single tool action kind a projected session published, plus the number
/// of `projection_unregistered` drift facts the run recorded.
fn published_tool_kind(store: &Store, session_id: &str) -> (String, i64) {
    let entry = queries::timeline_page(store.conn(), session_id, None, 100)
        .unwrap()
        .into_iter()
        .find(|entry| matches!(entry.body, TimelineBody::ToolUse { .. }))
        .expect("the session must publish a tool entry");
    let TimelineBody::ToolUse { tool } = entry.body else {
        unreachable!("filtered above")
    };
    let unregistered: i64 = store
        .conn()
        .query_row(
            "SELECT COALESCE(SUM(count), 0) FROM drift_agg
             WHERE error_code = 'projection_unregistered'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    (tool.action.kind().as_str().to_owned(), unregistered)
}

/// Typed tool actions were lost to a writer-version gate, not to reconciliation.
/// The projector registry accepted only the release its fixtures were farmed
/// from, so every transcript written by any other patch release published
/// `unknown` with a `projection_unregistered` drift fact — 131 of 131 tool rows
/// in one observed Claude Code session, 82 of 82 in one Codex session. The whole
/// release line must stay typed end to end; the next line must still fail closed.
#[test]
fn claude_tool_actions_stay_typed_across_the_writer_release_line() {
    for (version, kind, unregistered) in [
        ("2.1.37", "shell", 0),  // census floor
        ("2.1.241", "shell", 0), // the pinned fixture writer
        ("2.1.258", "shell", 0), // a later patch of the same line
        ("2.2.0", "unknown", 1), // a new line still fails closed
    ] {
        let root = TempDir::new().unwrap();
        write_lines(
            &root.path().join("projects/p/session-line.jsonl"),
            &claude_bash_round("session-line", version),
        );
        let runtime = runtime(&ClaudeCodeAdapter);
        let mut store = Store::open_in_memory().unwrap();
        let (_, files) = meta_ingest(
            &ClaudeCodeAdapter,
            &runtime,
            root.path(),
            &mut store,
            &opts(),
            &Instrument::new(),
        )
        .unwrap();
        stream_backfill(
            &ClaudeCodeAdapter,
            &runtime,
            root.path(),
            &mut store,
            &files,
            &opts(),
            &Instrument::new(),
        )
        .unwrap();
        assert_eq!(
            published_tool_kind(&store, &files[0].session_id),
            (kind.to_owned(), unregistered),
            "claude-code writer {version}"
        );
    }
}

#[test]
fn codex_tool_actions_stay_typed_across_the_writer_release_line() {
    let native = "01a036c5-6e18-73f3-8c07-d4430763840b";
    for (version, kind, unregistered) in [
        ("0.144.6", "shell", 0), // oldest writer with this vocabulary
        ("0.149.1", "shell", 0), // the pinned fixture writer
        ("0.152.0", "shell", 0), // a later release of the same line
        ("1.0.0", "unknown", 1), // a new line still fails closed
    ] {
        let root = TempDir::new().unwrap();
        write_lines(
            &root
                .path()
                .join("sessions/2026/08/25/rollout-session-line.jsonl"),
            &codex_exec_round(native, version),
        );
        let runtime = runtime(&CodexAdapter);
        let mut store = Store::open_in_memory().unwrap();
        let (_, files) = meta_ingest(
            &CodexAdapter,
            &runtime,
            root.path(),
            &mut store,
            &opts(),
            &Instrument::new(),
        )
        .unwrap();
        stream_backfill(
            &CodexAdapter,
            &runtime,
            root.path(),
            &mut store,
            &files,
            &opts(),
            &Instrument::new(),
        )
        .unwrap();
        assert_eq!(
            published_tool_kind(&store, &files[0].session_id),
            (kind.to_owned(), unregistered),
            "codex writer {version}"
        );
    }
}
