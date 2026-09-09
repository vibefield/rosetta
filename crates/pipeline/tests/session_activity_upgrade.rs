//! An existing archive must keep serving activity while a new admission
//! policy starts excluding internal sessions on fresh capture.
use rosetta_adapter_codex::CodexAdapter;
use rosetta_capture::{Ledger, LedgerEntry, scan_ledger};
use rosetta_core::{IngestOp, ids};
use rosetta_pipeline::{IngestOptions, Instrument, meta_ingest_into, stream_backfill_into};
use rosetta_store::{Batch, Store, queries};
use serde_json::json;
use std::io::Write;

#[test]
fn an_archived_public_source_can_resume_after_internal_session_policy_changes() {
    let temp = tempfile::TempDir::new().unwrap();
    let root = temp.path().join("codex");
    let path = root.join("sessions/2026/09/09/rollout-guardian.jsonl");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let record = |kind: &str, payload: serde_json::Value| {
        json!({"timestamp":"2026-09-09T12:00:00Z","type":kind,"payload":payload}).to_string() + "\n"
    };
    std::fs::write(
        &path,
        record(
            "session_meta",
            json!({
                "id":"guardian", "timestamp":"2026-09-09T12:00:00Z", "cwd":"/fixture/project",
                "originator":"codex_exec", "cli_version":"0.150.1", "model_provider":"openai",
                "source":{"subagent":{"other":"guardian"}}, "thread_source":"guardian_review"
            }),
        ),
    )
    .unwrap();
    let runtime = rosetta_core::RuntimeRow {
        id: ids::runtime_id("codex", root.to_str().unwrap()),
        kind: "codex".into(),
        data_root: root.to_string_lossy().into_owned(),
        enabled: true,
        account: None,
        config: None,
        skills: None,
        versions_seen: None,
        discovered_at: 0,
    };
    let mut store = Store::open_in_memory().unwrap();
    let ledger_path = temp.path().join("ledger");
    let mut ledger = Ledger::open(&ledger_path).unwrap();
    let opts = IngestOptions::default();
    let instrument = Instrument::new();
    let (_, mut files) = meta_ingest_into(
        &CodexAdapter,
        &runtime,
        &root,
        &mut store,
        &opts,
        &instrument,
        Some(&mut ledger),
    )
    .unwrap();
    assert!(files[0].ledger_only);
    let id = files[0].session_id.clone();

    // Simulate the persisted source binding written before the new policy.
    store
        .apply(&Batch {
            ops: vec![IngestOp::SessionMeta(
                serde_json::from_value(json!({
                    "id":id, "native_session_id":"guardian", "runtime_id":runtime.id,
                    "parent_session_id":null, "root_session_id":id, "role":"main",
                    "started_at":0, "updated_at":0, "stream_state":"pending"
                }))
                .unwrap(),
            )],
            ..Batch::default()
        })
        .unwrap();
    files[0].ledger_only = false;
    stream_backfill_into(
        &CodexAdapter,
        &runtime,
        &root,
        &mut store,
        &files,
        &opts,
        &instrument,
        Some(&mut ledger),
    )
    .unwrap();

    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(
            record(
                "response_item",
                json!({"type":"message", "role":"user",
            "content":[{"type":"input_text","text":"private appended review"}]}),
            )
            .as_bytes(),
        )
        .unwrap();
    let (_, files) = meta_ingest_into(
        &CodexAdapter,
        &runtime,
        &root,
        &mut store,
        &opts,
        &instrument,
        Some(&mut ledger),
    )
    .unwrap();
    assert!(
        !files[0].ledger_only,
        "keep the recorded binding for this old source"
    );
    let result = stream_backfill_into(
        &CodexAdapter,
        &runtime,
        &root,
        &mut store,
        &files,
        &opts,
        &instrument,
        Some(&mut ledger),
    )
    .unwrap();
    assert_eq!(result.hard_errors, 0);
    assert_eq!(result.records, 1);
    assert_eq!(
        queries::cursor_state(store.conn(), &runtime.id, &files[0].relative_path)
            .unwrap()
            .unwrap()
            .session_id
            .as_deref(),
        Some(id.as_str())
    );
    assert_eq!(
        queries::session(store.conn(), &id)
            .unwrap()
            .unwrap()
            .activity,
        None
    );
    assert!(
        queries::timeline_page(store.conn(), &id, None, 10)
            .unwrap()
            .is_empty()
    );
    ledger.flush().unwrap();
    drop(ledger);
    assert!(scan_ledger(&ledger_path).unwrap().entries.iter().any(|entry|
        matches!(entry, LedgerEntry::Raw { source, payload, .. }
            if !source.ledger_only && String::from_utf8_lossy(payload).contains("private appended review"))));
}
