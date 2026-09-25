//! Capture progress for an omitted source is not a canonical session.

use rosetta_core::rows::{
    RuntimeRow, SessionRole, SessionRow, SessionUsageRow, StreamState, TokenSnapshot,
};
use rosetta_core::{IngestOp, SessionTitleRow, TitleKind};
use rosetta_store::{Batch, CheckpointUpdate, SourceMode, SourceWrite, Store, queries};

#[test]
fn ledger_only_cursor_advances_without_projection_or_search_effects() {
    let mut store = Store::open_in_memory().unwrap();
    store
        .apply(&Batch {
            ops: vec![IngestOp::Runtime(RuntimeRow {
                id: "runtime".into(),
                kind: "fixture".into(),
                data_root: "/fixture".into(),
                enabled: true,
                account: None,
                config: None,
                skills: None,
                versions_seen: None,
                discovered_at: 0,
                label: None,
                provenance: Default::default(),
            })],
            checkpoints: vec![CheckpointUpdate {
                runtime_id: "runtime".into(),
                stream: "rollout".into(),
                path: "internal.jsonl".into(),
                session_id: None,
                expected: None,
                next: "{}".into(),
                updated_at: None,
            }],
            source: None,
        })
        .unwrap();
    let cursor = queries::cursor_state(store.conn(), "runtime", "internal.jsonl")
        .unwrap()
        .unwrap();
    assert_eq!(cursor.session_id, None);
    let source = |mode| SourceWrite {
        cursor_id: cursor.id,
        session_id: "internal-native-identity".into(),
        generation: 1,
        first_sequence: 0,
        mode,
    };
    for mode in [SourceMode::Stage, SourceMode::Activate, SourceMode::Append] {
        store
            .apply(&Batch {
                source: Some(source(mode)),
                ..Batch::default()
            })
            .unwrap();
    }
    let error = store
        .apply(&Batch {
            source: Some(source(SourceMode::Append)),
            ops: vec![IngestOp::SessionTitle(SessionTitleRow {
                session_id: "internal-native-identity".into(),
                title: "must not escape".into(),
                kind: TitleKind::Explicit,
                at: 0,
            })],
            checkpoints: vec![],
        })
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("ledger-only source cannot project operations"),
        "{error}"
    );
    let cursor = queries::cursor_state(store.conn(), "runtime", "internal.jsonl")
        .unwrap()
        .unwrap();
    assert_eq!(cursor.active_generation, 1);
    assert_eq!(cursor.highest_generation, 1);
    for table in ["session", "message", "usage_bucket", "search_dirty"] {
        let rows: i64 = store
            .conn()
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(rows, 0, "{table}");
    }
}

/// ARCHITECTURE §4.1.3, counting rule 5: a hidden internal session (a Codex
/// guardian review) contributes its requests to the thread that ran it as
/// `session_usage` samples scoped to the review's own cursor, and nothing
/// else.
#[test]
fn a_ledger_only_source_contributes_usage_samples_to_a_proven_session_and_nothing_else() {
    let mut store = Store::open_in_memory().unwrap();
    store
        .apply(&Batch {
            ops: vec![
                IngestOp::Runtime(RuntimeRow {
                    id: "runtime".into(),
                    kind: "codex".into(),
                    data_root: "/fixture".into(),
                    enabled: true,
                    account: None,
                    config: None,
                    skills: None,
                    versions_seen: None,
                    discovered_at: 0,
                    label: None,
                    provenance: Default::default(),
                }),
                IngestOp::SessionMeta(SessionRow {
                    id: "parent".into(),
                    native_session_id: "parent-thread".into(),
                    runtime_id: "runtime".into(),
                    project_id: None,
                    cwd: None,
                    parent_session_id: None,
                    delegation: None,
                    root_session_id: "parent".into(),
                    role: SessionRole::Main,
                    title: None,
                    summary: None,
                    started_at: 1_000,
                    updated_at: 1_000,
                    activity: None,
                    stream_state: StreamState::Streaming,
                }),
            ],
            checkpoints: vec![CheckpointUpdate {
                runtime_id: "runtime".into(),
                stream: "rollout".into(),
                path: "guardian.jsonl".into(),
                session_id: None,
                expected: None,
                next: "{}".into(),
                updated_at: None,
            }],
            source: None,
        })
        .unwrap();
    let cursor = queries::cursor_state(store.conn(), "runtime", "guardian.jsonl")
        .unwrap()
        .unwrap();
    assert_eq!(cursor.session_id, None);
    fn count(store: &Store, table: &str) -> i64 {
        store
            .conn()
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap()
    }
    // Admitting the parent session is the only search repair work here.
    let search_dirty_after_setup = count(&store, "search_dirty");
    let source = |generation, mode| SourceWrite {
        cursor_id: cursor.id,
        session_id: "guardian-native-identity".into(),
        generation,
        first_sequence: 0,
        mode,
    };
    let sample = |session: &str, key: &str, input: i64| {
        IngestOp::SessionUsage(SessionUsageRow {
            identity: None,
            session_id: session.into(),
            key: key.into(),
            ts: 3_600_000,
            model: Some("codex-auto-review".into()),
            tokens: TokenSnapshot {
                input: Some(input),
                output: Some(5),
                ..Default::default()
            },
        })
    };
    fn samples(store: &Store) -> Vec<(String, String, i64)> {
        store
            .conn()
            .prepare("SELECT session_id, sample_key, src_cursor FROM session_usage ORDER BY 2")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    }
    fn parent_input(store: &Store) -> Option<i64> {
        store
            .conn()
            .query_row(
                "SELECT SUM(in_tokens) FROM usage_bucket WHERE session_id = 'parent'",
                [],
                |row| row.get(0),
            )
            .unwrap()
    }

    // A live generation stages in RAM and lands whole at activation.
    store
        .apply(&Batch {
            source: Some(source(1, SourceMode::Stage)),
            ops: vec![sample("parent", "review:1", 100)],
            checkpoints: vec![],
        })
        .unwrap();
    assert_eq!(samples(&store), vec![]);
    store
        .apply(&Batch {
            source: Some(source(1, SourceMode::Activate)),
            ops: vec![],
            checkpoints: vec![],
        })
        .unwrap();
    assert_eq!(
        samples(&store),
        vec![("parent".to_string(), "review:1".to_string(), cursor.id)]
    );
    assert_eq!(parent_input(&store), Some(100));

    // A parent that was never captured is out of coverage, not a failure.
    let stats = store
        .apply(&Batch {
            source: Some(source(1, SourceMode::Append)),
            ops: vec![sample("ghost", "review:2", 7)],
            checkpoints: vec![],
        })
        .unwrap();
    assert_eq!(stats.usage_unmatched, 1);
    assert_eq!(samples(&store).len(), 1);

    // Anything but samples and drift is still refused.
    let error = store
        .apply(&Batch {
            source: Some(source(1, SourceMode::Append)),
            ops: vec![IngestOp::SessionTitle(SessionTitleRow {
                session_id: "parent".into(),
                title: "must not escape".into(),
                kind: TitleKind::Explicit,
                at: 0,
            })],
            checkpoints: vec![],
        })
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("ledger-only source cannot project operations"),
        "{error}"
    );

    // A replacement generation retracts the previous generation's samples.
    store
        .apply(&Batch {
            source: Some(source(2, SourceMode::Stage)),
            ops: vec![sample("parent", "review:3", 40)],
            checkpoints: vec![],
        })
        .unwrap();
    assert_eq!(parent_input(&store), Some(100), "staging changes nothing");
    let stats = store
        .apply(&Batch {
            source: Some(source(2, SourceMode::Activate)),
            ops: vec![],
            checkpoints: vec![],
        })
        .unwrap();
    assert_eq!(stats.source_sweeps, 1);
    assert_eq!(
        samples(&store),
        vec![("parent".to_string(), "review:3".to_string(), cursor.id)]
    );
    assert_eq!(parent_input(&store), Some(40));
    let cursor = queries::cursor_state(store.conn(), "runtime", "guardian.jsonl")
        .unwrap()
        .unwrap();
    assert_eq!(cursor.session_id, None);
    assert_eq!(cursor.active_generation, 2);
    assert_eq!(count(&store, "message"), 0);
    assert_eq!(
        count(&store, "search_dirty"),
        search_dirty_after_setup,
        "samples and their sweeps are not search repair work"
    );
}
