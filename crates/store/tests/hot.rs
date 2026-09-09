//! Hot-tier effects through real applies: what the store hands the hub. The
//! load-bearing invariants — effects cost nothing until observed, Append
//! emits rows with apply-time ordinals plus merge patches keyed by
//! timeline_id, activation collapses to a single sweep marker (temporary
//! ordinals never leak), and the mirror diffs are precise.

use rosetta_core::rows::{
    MessageKind, MessageRow, ProjectRow, RuntimeRow, SessionRole, SessionRow, StreamState,
    TokenSnapshot,
};
use rosetta_core::{
    CanonicalText, CorrelationDerivation, EvidenceOrder, IngestOp, NativeToolRef, SourceIdentity,
    StatusBasis, TimelineBody, ToolAction, ToolCorrelation, ToolInteraction, ToolStatus,
    ToolStatusFact, ToolUpdatePatch,
};
use rosetta_store::hot::{Effects, MetaMirror, Mutation};
use rosetta_store::{Batch, CheckpointUpdate, SourceMode, SourceWrite, Store, queries};

fn base_batch() -> Batch {
    Batch {
        ops: vec![
            IngestOp::Runtime(RuntimeRow {
                id: "rt1".into(),
                kind: "claude-code".into(),
                data_root: "~/.claude".into(),
                enabled: true,
                account: None,
                config: None,
                skills: None,
                versions_seen: None,
                discovered_at: 1_000,
            }),
            IngestOp::Project(ProjectRow {
                id: "p1".into(),
                name: "rosetta".into(),
                root_paths: vec!["/w/rosetta".into()],
                git_repo: None,
            }),
            IngestOp::SessionMeta(session("s1")),
        ],
        checkpoints: vec![],
        source: None,
    }
}

fn session(id: &str) -> SessionRow {
    SessionRow {
        id: id.into(),
        native_session_id: format!("native-{id}"),
        runtime_id: "rt1".into(),
        project_id: Some("p1".into()),
        cwd: Some("/w/rosetta".into()),
        parent_session_id: None,
        delegation: None,
        root_session_id: id.into(),
        role: SessionRole::Main,
        title: None,
        summary: None,
        started_at: 10_000,
        updated_at: 10_000,
        stream_state: StreamState::Streaming,
    }
}

fn message(timeline_id: &str, ordinal: i64) -> MessageRow {
    MessageRow {
        timeline_id: timeline_id.into(),
        session_id: "s1".into(),
        ordinal,
        kind: MessageKind::User,
        subtype: None,
        uuid: None,
        parent_uuid: None,
        ts: Some(3_600_000 + ordinal * 1000),
        model: None,
        effort: None,
        content: Some(format!("content of {timeline_id}")),
        tool: None,
        attachments: Vec::new(),
        citations: Vec::new(),
        group: None,
        response: None,
        block_index: None,
        branch_tool_id: None,
        usage: None,
        agent_version: None,
        src_cursor: None,
        src_offset: None,
    }
}

fn message_op(row: MessageRow) -> IngestOp {
    let mut ops = IngestOp::project_message(row);
    assert_eq!(ops.len(), 1);
    ops.pop().unwrap()
}

fn shell_tool(key: &str, command: &str) -> ToolInteraction {
    ToolInteraction {
        correlation: ToolCorrelation {
            key: key.into(),
            derivation: CorrelationDerivation::NativeId,
        },
        native: NativeToolRef {
            id: Some(key.into()),
            namespace: None,
            name: "Bash".into(),
            provider: None,
        },
        status: ToolStatusFact {
            value: ToolStatus::Pending,
            basis: StatusBasis::RequestObserved,
        },
        action: ToolAction::Shell {
            command: CanonicalText::inline(command).unwrap(),
            source_truncated: false,
            cwd: None,
            timeout_ms: None,
            background: false,
            outcome: None,
        },
        failure: None,
        timing: None,
    }
}

fn succeeded_update(key: &str) -> IngestOp {
    IngestOp::ToolUpdate {
        session_id: "s1".into(),
        correlation_key: key.into(),
        evidence: EvidenceOrder {
            source_identity: SourceIdentity("test:hot".into()),
            generation: 0,
            record_offset: 1,
            mutation_index: 0,
        },
        patch: Box::new(ToolUpdatePatch {
            status: Some(ToolStatusFact {
                value: ToolStatus::Succeeded,
                basis: StatusBasis::NativeTerminalRecord,
            }),
            ..Default::default()
        }),
        at: Some(3_700_000),
    }
}

fn cursor_id(store: &mut Store) -> i64 {
    store
        .apply(&Batch {
            ops: vec![],
            checkpoints: vec![CheckpointUpdate {
                runtime_id: "rt1".into(),
                stream: "transcript".into(),
                path: "p/x.jsonl".into(),
                session_id: Some("s1".into()),
                expected: None,
                next: "blob-v0".into(),
                updated_at: Some(1),
            }],
            source: None,
        })
        .unwrap();
    queries::cursor_state(store.conn(), "rt1", "p/x.jsonl")
        .unwrap()
        .unwrap()
        .id
}

fn source(cursor: i64, generation: u64, first_sequence: u64, mode: SourceMode) -> SourceWrite {
    SourceWrite {
        cursor_id: cursor,
        session_id: "s1".into(),
        generation,
        first_sequence,
        mode,
    }
}

#[test]
fn effects_are_off_by_default_and_cost_nothing() {
    let mut store = Store::open_in_memory().unwrap();
    store.apply(&base_batch()).unwrap();
    assert_eq!(store.drain_effects(), Effects::default());
}

#[test]
fn append_mode_emits_rows_with_apply_time_ordinals_and_merge_patches() {
    let mut store = Store::open_in_memory().unwrap();
    store.observe_effects();
    store.apply(&base_batch()).unwrap();
    let meta = store.drain_effects();
    assert!(
        meta.mutations.is_empty(),
        "meta batches emit no row mutations"
    );
    assert!(meta.touched_sessions.contains("s1"));
    assert!(meta.touched_projects.contains("p1"));
    assert!(meta.touched_runtimes.contains("rt1"));

    let cursor = cursor_id(&mut store);
    store.drain_effects();

    // Both rows claim ordinal 0; Append assigns the real ordinals, and the
    // mutation log must carry those, not the adapter's claims.
    let mut call = message("m-tool", 0);
    call.kind = MessageKind::ToolUse;
    call.content = None;
    call.tool = Some(shell_tool("toolu_1", "true"));
    store
        .apply(&Batch {
            ops: vec![
                message_op(message("m-user", 0)),
                message_op(call),
                succeeded_update("toolu_1"),
                IngestOp::Usage {
                    session_id: "s1".into(),
                    response: "m-user".into(),
                    ts: 3_600_000,
                    model: Some("claude-opus-5".into()),
                    tokens: TokenSnapshot {
                        input: Some(10),
                        output: Some(5),
                        ..Default::default()
                    },
                },
            ],
            checkpoints: vec![],
            source: Some(source(cursor, 0, 0, SourceMode::Append)),
        })
        .unwrap();

    let effects = store.drain_effects();
    assert!(!effects.overflowed);
    assert!(effects.touched_sessions.contains("s1"));
    assert_eq!(effects.mutations.len(), 4);
    match &effects.mutations[0] {
        Mutation::Message(row) => {
            assert_eq!(row.timeline_id, "m-user");
            assert_eq!(row.ordinal, 0);
        }
        other => panic!("expected message mutation, got {other:?}"),
    }
    match &effects.mutations[1] {
        Mutation::Message(row) => {
            assert_eq!(row.timeline_id, "m-tool");
            assert_eq!(row.ordinal, 1);
        }
        other => panic!("expected message mutation, got {other:?}"),
    }
    match &effects.mutations[2] {
        Mutation::ToolUpdated {
            timeline_id, tool, ..
        } => {
            assert_eq!(timeline_id, "m-tool", "patch keyed by the CALL row");
            assert_eq!(tool.status.value, ToolStatus::Succeeded);
        }
        other => panic!("expected typed tool patch, got {other:?}"),
    }
    match &effects.mutations[3] {
        Mutation::Usage {
            timeline_id,
            model,
            tokens,
            ..
        } => {
            assert_eq!(timeline_id, "m-user");
            assert_eq!(model.as_deref(), Some("claude-opus-5"));
            assert_eq!(tokens.input, Some(10));
        }
        other => panic!("expected usage patch, got {other:?}"),
    }

    // A replacement upsert of an existing row is a Message mutation too.
    let mut edited = message("m-user", 0);
    edited.content = Some("edited content".into());
    store
        .apply(&Batch {
            ops: vec![message_op(edited)],
            checkpoints: vec![],
            source: Some(source(cursor, 0, 0, SourceMode::Append)),
        })
        .unwrap();
    let effects = store.drain_effects();
    assert_eq!(effects.mutations.len(), 1);
    match &effects.mutations[0] {
        Mutation::Message(row) => {
            assert!(matches!(
                &row.body,
                TimelineBody::User { text: Some(text), .. }
                    if text.as_preview() == "edited content"
            ));
            assert_eq!(row.ordinal, 0, "replacement keeps the existing ordinal");
        }
        other => panic!("expected message mutation, got {other:?}"),
    }
}

#[test]
fn activation_collapses_to_a_single_sweep_marker() {
    let mut store = Store::open_in_memory().unwrap();
    store.observe_effects();
    store.apply(&base_batch()).unwrap();
    let cursor = cursor_id(&mut store);
    store.drain_effects();

    let staged: Vec<IngestOp> = vec![
        message_op(message("g1-a", 0)),
        message_op(message("g1-b", 1)),
    ];
    let count = staged.len() as u64;
    store
        .apply(&Batch {
            ops: staged,
            checkpoints: vec![],
            source: Some(source(cursor, 1, 0, SourceMode::Stage)),
        })
        .unwrap();
    assert!(
        store.drain_effects().is_empty(),
        "staging projects nothing and must emit nothing"
    );

    store
        .apply(&Batch {
            ops: vec![],
            checkpoints: vec![],
            source: Some(source(cursor, 1, count, SourceMode::Activate)),
        })
        .unwrap();
    let effects = store.drain_effects();
    assert_eq!(
        effects.mutations,
        vec![Mutation::SessionSwept {
            session_id: "s1".into()
        }],
        "a swap is ONE mutation-log entry; per-row applies with temporary \
         ordinals must never leak"
    );
    assert!(effects.touched_sessions.contains("s1"));
}

#[test]
fn mirror_diffs_are_precise() {
    let mut store = Store::open_in_memory().unwrap();
    store.observe_effects();
    store.apply(&base_batch()).unwrap();

    let mut mirror = MetaMirror::default();
    let events = mirror.warm(store.conn()).unwrap();
    assert_eq!(events.len(), 3, "runtime + project + session");
    assert!(
        mirror.warm(store.conn()).unwrap().is_empty(),
        "warm again: no diffs"
    );
    store.drain_effects();

    // A titled upsert changes the session row; the diff carries the new row.
    let mut titled = session("s1");
    titled.title = Some("Named".into());
    store
        .apply(&Batch {
            ops: vec![IngestOp::SessionMeta(titled)],
            checkpoints: vec![],
            source: None,
        })
        .unwrap();
    let effects = store.drain_effects();
    let events = mirror.refresh(store.conn(), &effects).unwrap();
    assert_eq!(events.len(), 1);
    match &events[0] {
        Mutation::Session(row) => assert_eq!(row.title.as_deref(), Some("Named")),
        other => panic!("expected session diff, got {other:?}"),
    }
    assert_eq!(
        mirror.session("s1").unwrap().title.as_deref(),
        Some("Named")
    );

    // The identical upsert touches the same ids but changes no row: the
    // refresh reloads them and emits nothing.
    let mut titled = session("s1");
    titled.title = Some("Named".into());
    store
        .apply(&Batch {
            ops: vec![IngestOp::SessionMeta(titled)],
            checkpoints: vec![],
            source: None,
        })
        .unwrap();
    let effects = store.drain_effects();
    assert!(effects.touched_sessions.contains("s1"));
    assert!(mirror.refresh(store.conn(), &effects).unwrap().is_empty());
}
