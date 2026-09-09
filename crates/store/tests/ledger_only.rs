//! Capture progress for an omitted source is not a canonical session.

use rosetta_core::rows::RuntimeRow;
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
