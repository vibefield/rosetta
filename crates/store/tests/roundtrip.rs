//! Store round-trip: ops in, queries out — proving the merge, replacement,
//! unknownness, drift-aggregation, and checkpoint-CAS semantics end to end.

use rosetta_core::IngestOp;
use rosetta_core::rows::{
    DriftFact, EditKind, FileEditRow, MessageKind, MessageRow, ProjectRow, RuntimeRow, SessionRole,
    SessionRow, SessionTitleRow, SessionUsageRow, StreamState, TitleKind, TokenSnapshot, ToolCall,
};
use rosetta_store::{Batch, CheckpointUpdate, SourceMode, SourceWrite, Store, StoreError, queries};

fn runtime() -> RuntimeRow {
    RuntimeRow {
        id: "rt1".into(),
        kind: "claude-code".into(),
        data_root: "~/.claude".into(),
        enabled: true,
        account: None,
        config: None,
        skills: None,
        versions_seen: None,
        discovered_at: 1_000,
    }
}

fn project() -> ProjectRow {
    ProjectRow {
        id: "p1".into(),
        name: "rosetta".into(),
        root_paths: vec!["/w/rosetta".into()],
        git_repo: None,
    }
}

fn session(id: &str) -> SessionRow {
    SessionRow {
        id: id.into(),
        native_session_id: format!("native-{id}"),
        runtime_id: "rt1".into(),
        project_id: Some("p1".into()),
        parent_session_id: None,
        root_session_id: id.into(),
        role: SessionRole::Main,
        title: None,
        summary: None,
        started_at: 10_000,
        updated_at: 10_000,
        stream_state: StreamState::Streaming,
    }
}

fn message(timeline_id: &str, ordinal: i64, kind: MessageKind) -> MessageRow {
    MessageRow {
        timeline_id: timeline_id.into(),
        session_id: "s1".into(),
        ordinal,
        kind,
        subtype: None,
        uuid: None,
        parent_uuid: None,
        ts: Some(3_600_000 + ordinal * 1000),
        model: Some("claude-opus-5".into()),
        effort: None,
        content: None,
        tool: None,
        branch_tool_id: None,
        usage: None,
        agent_version: None,
        src_cursor: None,
        src_offset: None,
    }
}

fn base_batch() -> Batch {
    Batch {
        ops: vec![
            IngestOp::Runtime(runtime()),
            IngestOp::Project(project()),
            IngestOp::SessionMeta(session("s1")),
        ],
        checkpoints: vec![],
        source: None,
    }
}

#[test]
fn streamed_title_singletons_refine_meta_with_custom_precedence() {
    let mut store = Store::open_in_memory().unwrap();
    store.apply(&base_batch()).unwrap();
    store
        .apply(&Batch {
            ops: vec![IngestOp::SessionTitle(SessionTitleRow {
                session_id: "s1".into(),
                title: "bounded meta title".into(),
                kind: TitleKind::Generated,
                at: 9_000,
            })],
            checkpoints: vec![],
            source: None,
        })
        .unwrap();

    let mut generated = message("generated-title", 0, MessageKind::Summary);
    generated.subtype = Some("ai_title".into());
    generated.content = Some("generated stream title".into());
    store
        .apply(&Batch {
            ops: vec![IngestOp::Message(Box::new(generated.clone()))],
            checkpoints: vec![],
            source: None,
        })
        .unwrap();
    assert_eq!(
        queries::session(store.conn(), "s1")
            .unwrap()
            .unwrap()
            .title
            .as_deref(),
        Some("generated stream title")
    );

    let mut custom = message("custom-title", 1, MessageKind::Summary);
    custom.subtype = Some("custom_title".into());
    custom.content = Some("explicit title".into());
    store
        .apply(&Batch {
            ops: vec![IngestOp::Message(Box::new(custom))],
            checkpoints: vec![],
            source: None,
        })
        .unwrap();

    // A later generated-title replacement cannot beat the custom singleton.
    generated.content = Some("later generated title".into());
    store
        .apply(&Batch {
            ops: vec![IngestOp::Message(Box::new(generated))],
            checkpoints: vec![],
            source: None,
        })
        .unwrap();
    assert_eq!(
        queries::session(store.conn(), "s1")
            .unwrap()
            .unwrap()
            .title
            .as_deref(),
        Some("explicit title")
    );
}

#[test]
fn explicit_meta_title_outranks_a_newer_stream_fallback() {
    let mut store = Store::open_in_memory().unwrap();
    store.apply(&base_batch()).unwrap();
    store
        .apply(&Batch {
            ops: vec![
                IngestOp::SessionTitle(SessionTitleRow {
                    session_id: "s1".into(),
                    title: "Checkpoint objective".into(),
                    kind: TitleKind::Fallback,
                    at: 11_000,
                }),
                IngestOp::SessionTitle(SessionTitleRow {
                    session_id: "s1".into(),
                    title: "Explicit native title".into(),
                    kind: TitleKind::Explicit,
                    at: 10_000,
                }),
            ],
            checkpoints: vec![],
            source: None,
        })
        .unwrap();

    let mut fallback = message("checkpoint-title", 4, MessageKind::Summary);
    fallback.subtype = Some("title_fallback".into());
    fallback.content = Some("Newer checkpoint objective".into());
    fallback.ts = Some(12_000);
    store
        .apply(&Batch {
            ops: vec![IngestOp::Message(Box::new(fallback))],
            checkpoints: vec![],
            source: None,
        })
        .unwrap();

    assert_eq!(
        queries::session(store.conn(), "s1")
            .unwrap()
            .unwrap()
            .title
            .as_deref(),
        Some("Explicit native title")
    );
}

#[test]
fn search_breaks_equal_rank_ties_by_timeline_identity_before_limit() {
    let mut store = Store::open_in_memory().unwrap();
    store.apply(&base_batch()).unwrap();

    let rows = ["timeline-zeta", "timeline-alpha", "timeline-middle"]
        .into_iter()
        .enumerate()
        .map(|(ordinal, timeline_id)| {
            let mut row = message(timeline_id, ordinal as i64, MessageKind::Agent);
            row.content = Some("identical equilibrium phrase".into());
            IngestOp::Message(Box::new(row))
        })
        .collect();
    store
        .apply(&Batch {
            ops: rows,
            checkpoints: vec![],
            source: None,
        })
        .unwrap();

    let hits = queries::search(store.conn(), "equilibrium", 2).unwrap();
    assert_eq!(hits[0].score, hits[1].score);
    assert_eq!(
        hits.iter()
            .map(|hit| hit.timeline_id.as_str())
            .collect::<Vec<_>>(),
        ["timeline-alpha", "timeline-middle"]
    );
}

#[test]
fn tool_result_merges_into_its_call_row_and_fts_sees_it() {
    let mut store = Store::open_in_memory().unwrap();
    store.apply(&base_batch()).unwrap();

    let mut call = message("m-tool", 0, MessageKind::ToolUse);
    call.tool = Some(ToolCall {
        tool_id: Some("toolu_1".into()),
        name: "Bash".into(),
        input: serde_json::json!({"command": "cargo test"}),
        result: None,
        payload: None,
        is_error: None,
    });
    let stats = store
        .apply(&Batch {
            ops: vec![IngestOp::Message(Box::new(call))],
            checkpoints: vec![],
            source: None,
        })
        .unwrap();
    assert_eq!(stats.messages_inserted, 1);
    assert_eq!(stats.profile.message_attempts, 1);
    assert_eq!(stats.profile.pending_gate_checks, 1);
    assert_eq!(stats.profile.pending_lookup_skips, 1);
    assert_eq!(stats.profile.pending_lookups, 0);
    assert_eq!(stats.profile.session_touch_attempts, 1);

    let stats = store
        .apply(&Batch {
            ops: vec![IngestOp::ToolResult {
                session_id: "s1".into(),
                tool_id: "toolu_1".into(),
                result: Some("everything is quetzalcoatl green".into()),
                payload: Some(serde_json::json!({"exit": 0})),
                is_error: Some(false),
                at: Some(3_700_000),
            }],
            checkpoints: vec![],
            source: None,
        })
        .unwrap();
    assert_eq!(stats.tool_results_merged, 1);
    assert_eq!(stats.profile.tool_result_attempts, 1);
    assert_eq!(stats.profile.session_touch_attempts, 1);

    let page = queries::timeline_page(store.conn(), "s1", None, 10).unwrap();
    let tool = page[0].tool.as_ref().unwrap();
    assert_eq!(
        tool.result.as_deref(),
        Some("everything is quetzalcoatl green")
    );
    assert_eq!(tool.is_error, Some(false));

    // The merged result is searchable, and so is the input.
    let hits = queries::search(store.conn(), "quetzalcoatl", 5).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].timeline_id, "m-tool");
    assert_eq!(queries::search(store.conn(), "cargo", 5).unwrap().len(), 1);

    // A result with no matching call is retained until the call arrives.
    let stats = store
        .apply(&Batch {
            ops: vec![IngestOp::ToolResult {
                session_id: "s1".into(),
                tool_id: "toolu_missing".into(),
                result: Some("orphan".into()),
                payload: None,
                is_error: None,
                at: None,
            }],
            checkpoints: vec![],
            source: None,
        })
        .unwrap();
    assert_eq!(stats.tool_results_unmatched, 1);
    assert_eq!(stats.profile.tool_result_attempts, 1);
    assert_eq!(stats.profile.pending_queues, 1);
    let pending: i64 = store
        .conn()
        .query_row("SELECT COUNT(*) FROM pending_merge", [], |row| row.get(0))
        .unwrap();
    assert_eq!(pending, 1);

    let mut late_call = message("m-tool-late", 1, MessageKind::ToolUse);
    late_call.tool = Some(ToolCall {
        tool_id: Some("toolu_missing".into()),
        name: "Bash".into(),
        input: serde_json::json!({"command": "true"}),
        result: None,
        payload: None,
        is_error: None,
    });
    let stats = store
        .apply(&Batch {
            ops: vec![IngestOp::Message(Box::new(late_call))],
            checkpoints: vec![],
            source: None,
        })
        .unwrap();
    assert_eq!(stats.tool_results_merged, 1);
    assert_eq!(stats.profile.message_attempts, 1);
    assert_eq!(stats.profile.pending_gate_checks, 1);
    assert_eq!(stats.profile.pending_lookup_skips, 0);
    assert_eq!(stats.profile.pending_lookups, 1);
    assert_eq!(stats.profile.pending_rows, 1);
    assert_eq!(stats.profile.tool_result_attempts, 1);
    assert_eq!(
        store
            .conn()
            .query_row("SELECT COUNT(*) FROM pending_merge", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    let page = queries::timeline_page(store.conn(), "s1", None, 10).unwrap();
    let late = page
        .iter()
        .find(|row| row.timeline_id == "m-tool-late")
        .unwrap();
    assert_eq!(
        late.tool.as_ref().unwrap().result.as_deref(),
        Some("orphan")
    );
}

#[test]
fn empty_pending_table_is_checked_once_for_the_whole_batch() {
    let mut store = Store::open_in_memory().unwrap();
    store.apply(&base_batch()).unwrap();
    let ops = (0..16)
        .map(|ordinal| {
            IngestOp::Message(Box::new(message(
                &format!("empty-gate-{ordinal}"),
                ordinal,
                MessageKind::User,
            )))
        })
        .collect();

    let stats = store
        .apply(&Batch {
            ops,
            checkpoints: vec![],
            source: None,
        })
        .unwrap();

    assert_eq!(stats.profile.pending_gate_checks, 1);
    assert_eq!(stats.profile.pending_lookup_skips, 16);
    assert_eq!(stats.profile.pending_lookups, 0);
}

#[test]
fn queued_result_reopens_the_pending_gate_inside_the_same_batch() {
    let mut store = Store::open_in_memory().unwrap();
    store.apply(&base_batch()).unwrap();
    let mut call = message("same-batch-call", 0, MessageKind::ToolUse);
    call.tool = Some(ToolCall {
        tool_id: Some("same-batch-tool".into()),
        name: "Read".into(),
        input: serde_json::json!({"path": "fixture.txt"}),
        result: None,
        payload: None,
        is_error: None,
    });

    let stats = store
        .apply(&Batch {
            ops: vec![
                IngestOp::ToolResult {
                    session_id: "s1".into(),
                    tool_id: "same-batch-tool".into(),
                    result: Some("same transaction".into()),
                    payload: None,
                    is_error: Some(false),
                    at: Some(3_700_000),
                },
                IngestOp::Message(Box::new(call)),
            ],
            checkpoints: vec![],
            source: None,
        })
        .unwrap();

    assert_eq!(stats.tool_results_unmatched, 1);
    assert_eq!(stats.tool_results_merged, 1);
    assert_eq!(stats.profile.pending_queues, 1);
    assert_eq!(stats.profile.pending_gate_checks, 0);
    assert_eq!(stats.profile.pending_lookups, 1);
    assert_eq!(stats.profile.pending_rows, 1);
    assert_eq!(
        store
            .conn()
            .query_row("SELECT COUNT(*) FROM pending_merge", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        0
    );
    let page = queries::timeline_page(store.conn(), "s1", None, 10).unwrap();
    assert_eq!(
        page[0].tool.as_ref().unwrap().result.as_deref(),
        Some("same transaction")
    );
}

#[test]
fn unrelated_pending_target_does_not_force_message_lookups() {
    let mut store = Store::open_in_memory().unwrap();
    store.apply(&base_batch()).unwrap();
    store
        .apply(&Batch {
            ops: vec![IngestOp::ToolResult {
                session_id: "s1".into(),
                tool_id: "still-missing".into(),
                result: Some("orphan".into()),
                payload: None,
                is_error: None,
                at: None,
            }],
            checkpoints: vec![],
            source: None,
        })
        .unwrap();
    let ops = (0..16)
        .map(|ordinal| {
            IngestOp::Message(Box::new(message(
                &format!("unrelated-{ordinal}"),
                ordinal,
                MessageKind::User,
            )))
        })
        .collect();

    let stats = store
        .apply(&Batch {
            ops,
            checkpoints: vec![],
            source: None,
        })
        .unwrap();

    assert_eq!(stats.profile.pending_gate_checks, 1);
    assert_eq!(stats.profile.pending_lookup_skips, 16);
    assert_eq!(stats.profile.pending_lookups, 0);
    assert_eq!(
        store
            .conn()
            .query_row("SELECT COUNT(*) FROM pending_merge", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn pending_target_in_one_session_does_not_poison_another() {
    let mut store = Store::open_in_memory().unwrap();
    let mut setup = base_batch();
    setup.ops.push(IngestOp::SessionMeta(session("s2")));
    store.apply(&setup).unwrap();
    store
        .apply(&Batch {
            ops: vec![IngestOp::Usage {
                session_id: "s1".into(),
                response: "missing-response".into(),
                ts: 0,
                model: None,
                tokens: TokenSnapshot::default(),
            }],
            checkpoints: vec![],
            source: None,
        })
        .unwrap();
    let ops = (0..8)
        .map(|ordinal| {
            let mut row = message(
                &format!("other-session-{ordinal}"),
                ordinal,
                MessageKind::User,
            );
            row.session_id = "s2".into();
            IngestOp::Message(Box::new(row))
        })
        .collect();

    let stats = store
        .apply(&Batch {
            ops,
            checkpoints: vec![],
            source: None,
        })
        .unwrap();

    assert_eq!(stats.profile.pending_gate_checks, 1);
    assert_eq!(stats.profile.pending_lookup_skips, 8);
    assert_eq!(stats.profile.pending_lookups, 0);
}

#[test]
fn usage_replacement_never_double_counts_and_preserves_unknownness() {
    let mut store = Store::open_in_memory().unwrap();
    store.apply(&base_batch()).unwrap();

    let mut reply = message("m-agent", 0, MessageKind::Agent);
    reply.usage = Some(TokenSnapshot {
        input: Some(100),
        output: Some(20),
        ..Default::default()
    });
    store
        .apply(&Batch {
            ops: vec![IngestOp::Message(Box::new(reply))],
            checkpoints: vec![],
            source: None,
        })
        .unwrap();

    // Re-reported snapshot for the same response: bucket takes (new − old).
    let stats = store
        .apply(&Batch {
            ops: vec![IngestOp::Usage {
                session_id: "s1".into(),
                response: "m-agent".into(),
                ts: 3_601_000,
                model: None,
                tokens: TokenSnapshot {
                    input: Some(150),
                    output: Some(20),
                    cache_read: Some(30),
                    ..Default::default()
                },
            }],
            checkpoints: vec![],
            source: None,
        })
        .unwrap();
    assert_eq!(stats.usage_applied, 1);

    let (input, output, cache_read, cache_write): (i64, i64, i64, Option<i64>) = store
        .conn()
        .query_row(
            "SELECT in_tokens, out_tokens, cache_r_tokens, cache_w_tokens FROM usage_bucket
             WHERE session_id = 's1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!((input, output, cache_read), (150, 20, 30));
    // Never reported → still NULL, not zero.
    assert_eq!(cache_write, None);

    // Heatmap sees the corrected totals exactly once.
    let days = queries::heatmap_days(store.conn(), "p1", 0).unwrap();
    assert_eq!(days.len(), 1);
    assert_eq!(days[0].day, "1970-01-01");
    assert_eq!(days[0].tokens, 200);
    let west = queries::heatmap_days_in_timezone(store.conn(), "p1", 0, -120).unwrap();
    assert_eq!(west[0].day, "1969-12-31");
}

#[test]
fn model_days_and_hour_profile_are_views_of_the_heatmap_sum() {
    let mut store = Store::open_in_memory().unwrap();
    store.apply(&base_batch()).unwrap();

    // Two models in the same day, hours 1 and 3 — distinct responses.
    let mut a = message("m-a", 1, MessageKind::Agent);
    a.ts = Some(3_600_000);
    a.model = Some("opus-5".into());
    a.usage = Some(TokenSnapshot {
        input: Some(100),
        output: Some(20),
        ..Default::default()
    });
    let mut b = message("m-b", 2, MessageKind::Agent);
    b.ts = Some(3_600_000 * 3);
    b.model = Some("haiku-4-5".into());
    b.usage = Some(TokenSnapshot {
        output: Some(7),
        ..Default::default()
    });
    store
        .apply(&Batch {
            ops: vec![
                IngestOp::Message(Box::new(a)),
                IngestOp::Message(Box::new(b)),
            ],
            checkpoints: vec![],
            source: None,
        })
        .unwrap();

    let models = queries::model_days_in_timezone(store.conn(), "p1", 0, 0).unwrap();
    assert_eq!(models.len(), 2);
    assert_eq!(
        (
            models[0].day.as_str(),
            models[0].model.as_str(),
            models[0].tokens
        ),
        ("1970-01-01", "haiku-4-5", 7)
    );
    assert_eq!(
        (
            models[1].day.as_str(),
            models[1].model.as_str(),
            models[1].tokens
        ),
        ("1970-01-01", "opus-5", 120)
    );

    // The coherence contract: per-model days sum back to the heatmap's day.
    let days = queries::heatmap_days(store.conn(), "p1", 0).unwrap();
    assert_eq!(days[0].tokens, models.iter().map(|m| m.tokens).sum::<i64>());

    // Hour profile in UTC, then shifted two hours west: same totals, moved.
    let hours = queries::hour_profile_in_timezone(store.conn(), "p1", 0, 0).unwrap();
    assert_eq!(hours.len(), 2);
    assert_eq!((hours[0].hour, hours[0].tokens), (1, 120));
    assert_eq!((hours[1].hour, hours[1].tokens), (3, 7));
    let west = queries::hour_profile_in_timezone(store.conn(), "p1", 0, -120).unwrap();
    assert_eq!((west[0].hour, west[0].tokens), (1, 7));
    assert_eq!((west[1].hour, west[1].tokens), (23, 120));
}

#[test]
fn contribution_folds_subagents_into_their_root() {
    let mut store = Store::open_in_memory().unwrap();
    let mut batch = base_batch();
    let mut sub = session("s2");
    sub.parent_session_id = Some("s1".into());
    sub.root_session_id = "s1".into();
    sub.role = SessionRole::Subagent;
    batch.ops.push(IngestOp::SessionMeta(sub));
    store.apply(&batch).unwrap();

    let mut root_msg = message("m1", 0, MessageKind::Agent);
    root_msg.usage = Some(TokenSnapshot {
        output: Some(10),
        ..Default::default()
    });
    let mut sub_msg = message("m2", 0, MessageKind::Agent);
    sub_msg.session_id = "s2".into();
    sub_msg.usage = Some(TokenSnapshot {
        output: Some(5),
        ..Default::default()
    });
    store
        .apply(&Batch {
            ops: vec![
                IngestOp::Message(Box::new(root_msg)),
                IngestOp::Message(Box::new(sub_msg)),
                IngestOp::FileEdit(FileEditRow {
                    session_id: "s2".into(),
                    message_timeline_id: None,
                    path: "/w/rosetta/src/lib.rs".into(),
                    kind: EditKind::Edit,
                    snapshot_ref: None,
                    lines_added: Some(4),
                    lines_removed: Some(1),
                    at: Some(3_600_000),
                }),
            ],
            checkpoints: vec![],
            source: None,
        })
        .unwrap();

    let rows = queries::contribution(store.conn(), "p1").unwrap();
    assert_eq!(
        rows.len(),
        1,
        "subagent must not appear as its own contributor"
    );
    assert_eq!(rows[0].session_id, "s1");
    assert_eq!(rows[0].tokens, 15);
    assert_eq!(rows[0].edits, 1);
    assert_eq!(rows[0].loc, 5);
}

#[test]
fn drift_aggregates_distinct_occurrences_and_deduplicates_replay() {
    let mut store = Store::open_in_memory().unwrap();
    store.apply(&base_batch()).unwrap();

    let fact = DriftFact {
        runtime_id: "rt1".into(),
        agent_version: "2.1.241".into(),
        stream: "transcript".into(),
        record_kind: "assistant".into(),
        error_code: "missing_field".into(),
        shape_hash: "abc".into(),
        at: Some(5_000),
        sample_cursor: None,
        sample_offset: Some(42),
    };
    let mut later = fact.clone();
    later.at = Some(9_000);
    let batch = Batch {
        ops: vec![
            IngestOp::Drift(fact.clone()),
            IngestOp::Drift(later),
            IngestOp::Drift(fact),
        ],
        checkpoints: vec![],
        source: None,
    };
    store.apply(&batch).unwrap();
    store.apply(&batch).unwrap();

    let (count, first, last): (i64, i64, i64) = store
        .conn()
        .query_row(
            "SELECT count, first_seen, last_seen FROM drift_agg",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!((count, first, last), (2, 5_000, 9_000));
}

#[test]
fn checkpoint_cas_rolls_back_the_whole_batch_on_conflict() {
    let mut store = Store::open_in_memory().unwrap();
    store.apply(&base_batch()).unwrap();

    store
        .apply(&Batch {
            ops: vec![],
            checkpoints: vec![CheckpointUpdate {
                runtime_id: "rt1".into(),
                stream: "transcript".into(),
                path: "projects/p/s1.jsonl".into(),
                session_id: "s1".into(),
                expected: None,
                next: "ck-v1".into(),
                updated_at: Some(1),
            }],
            source: None,
        })
        .unwrap();
    assert_eq!(
        queries::checkpoint(store.conn(), "rt1", "projects/p/s1.jsonl")
            .unwrap()
            .unwrap()
            .1,
        "ck-v1"
    );

    // Stale expected state: the CAS fails AND the batch's row never lands.
    let result = store.apply(&Batch {
        ops: vec![IngestOp::Message(Box::new(message(
            "m-torn",
            0,
            MessageKind::User,
        )))],
        checkpoints: vec![CheckpointUpdate {
            runtime_id: "rt1".into(),
            stream: "transcript".into(),
            path: "projects/p/s1.jsonl".into(),
            session_id: "s1".into(),
            expected: Some("ck-v0-stale".into()),
            next: "ck-v2".into(),
            updated_at: Some(2),
        }],
        source: None,
    });
    assert!(matches!(result, Err(StoreError::CheckpointConflict { .. })));
    let count: i64 = store
        .conn()
        .query_row("SELECT COUNT(*) FROM message", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 0, "a failed CAS must roll back the entire batch");
}

#[test]
fn reingesting_the_same_message_is_idempotent() {
    let mut store = Store::open_in_memory().unwrap();
    store.apply(&base_batch()).unwrap();

    let mut reply = message("m-idem", 0, MessageKind::Agent);
    reply.usage = Some(TokenSnapshot {
        input: Some(7),
        output: Some(3),
        ..Default::default()
    });
    reply.content = Some("hello".into());
    let batch = Batch {
        ops: vec![IngestOp::Message(Box::new(reply))],
        checkpoints: vec![],
        source: None,
    };
    store.apply(&batch).unwrap();
    store.apply(&batch).unwrap(); // replay — deterministic ids make this a no-op

    let (input, messages): (i64, i64) = store
        .conn()
        .query_row(
            "SELECT in_tokens, message_count FROM usage_bucket WHERE session_id = 's1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(input, 7, "usage must not double-count on replay");
    assert_eq!(messages, 1, "message_count counts inserts, not replays");
    assert_eq!(queries::search(store.conn(), "hello", 5).unwrap().len(), 1);
}

#[test]
fn session_usage_replaces_exactly_and_does_not_invent_messages() {
    let mut store = Store::open_in_memory().unwrap();
    store.apply(&base_batch()).unwrap();
    let mut sample = SessionUsageRow {
        session_id: "s1".into(),
        key: "native-model-task".into(),
        ts: 3_600_000,
        model: Some("model-a".into()),
        tokens: TokenSnapshot {
            input: Some(40),
            output: Some(10),
            ..Default::default()
        },
    };
    store
        .apply(&Batch {
            ops: vec![IngestOp::SessionUsage(sample.clone())],
            ..Batch::default()
        })
        .unwrap();
    store
        .apply(&Batch {
            ops: vec![IngestOp::SessionUsage(sample.clone())],
            ..Batch::default()
        })
        .unwrap();
    let first: (i64, i64, i64) = store
        .conn()
        .query_row(
            "SELECT in_tokens, message_count, tool_call_count FROM usage_bucket
             WHERE session_id = 's1' AND model = 'model-a'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(first, (40, 0, 0));
    assert_eq!(
        store
            .conn()
            .query_row("SELECT COUNT(*) FROM message", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );

    sample.ts = 7_200_000;
    sample.model = Some("model-b".into());
    sample.tokens.input = Some(7);
    store
        .apply(&Batch {
            ops: vec![IngestOp::SessionUsage(sample)],
            ..Batch::default()
        })
        .unwrap();
    assert_eq!(
        store
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM usage_bucket WHERE model = 'model-a'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
    let replacement: (i64, i64) = store
        .conn()
        .query_row(
            "SELECT in_tokens, out_tokens FROM usage_bucket WHERE model = 'model-b'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(replacement, (7, 10));
}

#[test]
fn message_replacement_moves_counts_between_bucket_keys_and_clears_unknowns() {
    let mut store = Store::open_in_memory().unwrap();
    store.apply(&base_batch()).unwrap();

    let mut row = message("m-move", 0, MessageKind::ToolUse);
    row.usage = Some(TokenSnapshot {
        input: Some(40),
        output: Some(5),
        ..Default::default()
    });
    store
        .apply(&Batch {
            ops: vec![IngestOp::Message(Box::new(row.clone()))],
            checkpoints: vec![],
            source: None,
        })
        .unwrap();

    row.ts = Some(2 * 3_600_000);
    row.model = Some("claude-sonnet-5".into());
    row.kind = MessageKind::Agent;
    row.usage = None;
    store
        .apply(&Batch {
            ops: vec![IngestOp::Message(Box::new(row))],
            checkpoints: vec![],
            source: None,
        })
        .unwrap();

    let old_count: i64 = store
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM usage_bucket
             WHERE session_id = 's1' AND hour_ts = 3600000 AND model = 'claude-opus-5'",
            [],
            |record| record.get(0),
        )
        .unwrap();
    assert_eq!(old_count, 0, "the old bucket must be emptied");
    let (input, messages, tools): (Option<i64>, i64, i64) = store
        .conn()
        .query_row(
            "SELECT in_tokens, message_count, tool_call_count FROM usage_bucket
             WHERE session_id = 's1' AND hour_ts = 7200000 AND model = 'claude-sonnet-5'",
            [],
            |record| Ok((record.get(0)?, record.get(1)?, record.get(2)?)),
        )
        .unwrap();
    assert_eq!(input, None, "a cleared full replacement returns to unknown");
    assert_eq!((messages, tools), (1, 0));
}

#[test]
fn project_upserts_union_root_paths() {
    let mut store = Store::open_in_memory().unwrap();
    let mut first = project();
    let mut second = project();
    first.root_paths = vec!["/w/one".into()];
    second.root_paths = vec!["/w/two".into()];
    store
        .apply(&Batch {
            ops: vec![IngestOp::Project(first), IngestOp::Project(second)],
            checkpoints: vec![],
            source: None,
        })
        .unwrap();
    let json: String = store
        .conn()
        .query_row(
            "SELECT root_paths FROM project WHERE id = 'p1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Vec<String>>(&json).unwrap(),
        ["/w/one", "/w/two"]
    );
}

#[test]
fn metadata_merges_are_independent_of_arrival_order() {
    fn projected(reverse: bool) -> (String, String, SessionRow, String, i64) {
        let mut store = Store::open_in_memory().unwrap();
        store.apply(&base_batch()).unwrap();

        let mut alpha = project();
        alpha.name = "alpha".into();
        alpha.root_paths = vec!["/w/alpha".into()];
        let mut zulu = project();
        zulu.name = "zulu".into();
        zulu.root_paths = vec!["/w/zulu".into()];

        let placeholder = session("s2");
        let mut child = session("s2");
        child.parent_session_id = Some("s1".into());
        child.root_session_id = "s1".into();
        child.role = SessionRole::Subagent;
        let alpha_title = SessionTitleRow {
            session_id: "s2".into(),
            title: "Alpha title".into(),
            kind: TitleKind::Explicit,
            at: 20_000,
        };
        let zulu_title = SessionTitleRow {
            session_id: "s2".into(),
            title: "Zulu title".into(),
            kind: TitleKind::Explicit,
            at: 20_000,
        };

        let ops = if reverse {
            vec![
                IngestOp::Project(zulu),
                IngestOp::Project(alpha),
                IngestOp::SessionMeta(child),
                IngestOp::SessionMeta(placeholder),
            ]
        } else {
            vec![
                IngestOp::Project(alpha),
                IngestOp::Project(zulu),
                IngestOp::SessionMeta(placeholder),
                IngestOp::SessionMeta(child),
            ]
        };
        store
            .apply(&Batch {
                ops,
                checkpoints: vec![],
                source: None,
            })
            .unwrap();
        let titles = if reverse {
            vec![
                IngestOp::SessionTitle(zulu_title),
                IngestOp::SessionTitle(alpha_title),
            ]
        } else {
            vec![
                IngestOp::SessionTitle(alpha_title),
                IngestOp::SessionTitle(zulu_title),
            ]
        };
        store
            .apply(&Batch {
                ops: titles,
                checkpoints: vec![],
                source: None,
            })
            .unwrap();

        let (name, roots): (String, String) = store
            .conn()
            .query_row(
                "SELECT name, root_paths FROM project WHERE id = 'p1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        let child = queries::session(store.conn(), "s2").unwrap().unwrap();
        let (meta_title, meta_title_at): (String, i64) = store
            .conn()
            .query_row(
                "SELECT meta_title, meta_title_at FROM session WHERE id = 's2'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        (name, roots, child, meta_title, meta_title_at)
    }

    let forward = projected(false);
    let reverse = projected(true);
    assert_eq!(forward, reverse);
    assert_eq!(forward.0, "alpha");
    assert_eq!(forward.2.parent_session_id.as_deref(), Some("s1"));
    assert_eq!(forward.2.root_session_id, "s1");
    assert_eq!(forward.2.role, SessionRole::Subagent);
    assert_eq!((forward.3.as_str(), forward.4), ("Alpha title", 20_000));
}

#[test]
fn drift_dedupe_identity_uses_stable_source_path_not_cursor_rowid() {
    fn event_key(reverse: bool) -> String {
        let mut store = Store::open_in_memory().unwrap();
        store.apply(&base_batch()).unwrap();
        let checkpoint = |path: &str| CheckpointUpdate {
            runtime_id: "rt1".into(),
            stream: "transcript".into(),
            path: path.into(),
            session_id: "s1".into(),
            expected: None,
            next: format!("checkpoint-{path}"),
            updated_at: Some(1),
        };
        let mut checkpoints = vec![checkpoint("other.jsonl"), checkpoint("target.jsonl")];
        if reverse {
            checkpoints.reverse();
        }
        store
            .apply(&Batch {
                ops: vec![],
                checkpoints,
                source: None,
            })
            .unwrap();
        let cursor = queries::cursor_state(store.conn(), "rt1", "target.jsonl")
            .unwrap()
            .unwrap();
        store
            .apply(&Batch {
                ops: vec![IngestOp::Drift(DriftFact {
                    runtime_id: "rt1".into(),
                    agent_version: "1.0".into(),
                    stream: "transcript".into(),
                    record_kind: "event".into(),
                    error_code: "unknown".into(),
                    shape_hash: "shape".into(),
                    at: Some(10),
                    sample_cursor: Some(cursor.id),
                    sample_offset: Some(20),
                })],
                checkpoints: vec![],
                source: None,
            })
            .unwrap();
        store
            .conn()
            .query_row("SELECT event_key FROM drift_seen", [], |row| row.get(0))
            .unwrap()
    }

    assert_eq!(event_key(false), event_key(true));
}

#[test]
fn stream_state_updates_do_not_feed_projected_titles_back_as_evidence() {
    let mut store = Store::open_in_memory().unwrap();
    store.apply(&base_batch()).unwrap();
    store
        .apply(&Batch {
            ops: vec![IngestOp::SessionTitle(SessionTitleRow {
                session_id: "s1".into(),
                title: "Generated title".into(),
                kind: TitleKind::Generated,
                at: 12_345,
            })],
            checkpoints: vec![],
            source: None,
        })
        .unwrap();

    assert!(
        store
            .set_session_stream_state("s1", StreamState::Indexed)
            .unwrap()
    );
    let (title, rank, at, state): (String, i64, i64, String) = store
        .conn()
        .query_row(
            "SELECT meta_title, meta_title_rank, meta_title_at, stream_state
             FROM session WHERE id = 's1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!((title.as_str(), rank, at), ("Generated title", 2, 12_345));
    assert_eq!(state, "indexed");
}

#[test]
fn schema_mismatch_fails_closed_without_dropping_existing_data() {
    let db = tempfile::NamedTempFile::new().unwrap();
    let conn = rusqlite::Connection::open(db.path()).unwrap();
    conn.execute("CREATE TABLE sentinel (value TEXT NOT NULL)", [])
        .unwrap();
    conn.execute("INSERT INTO sentinel VALUES ('keep-me')", [])
        .unwrap();
    conn.pragma_update(None, "user_version", 1).unwrap();
    drop(conn);

    assert!(matches!(
        Store::open(db.path()),
        Err(StoreError::SchemaVersionMismatch {
            found: 1,
            expected: rosetta_store::schema::SCHEMA_VERSION,
        })
    ));
    let conn = rusqlite::Connection::open(db.path()).unwrap();
    let value: String = conn
        .query_row("SELECT value FROM sentinel", [], |row| row.get(0))
        .unwrap();
    assert_eq!(value, "keep-me");
}

/// The staged-only invariant, pinned: the journal holds ONLY not-yet-active
/// staged generations (empty in steady state), appends never journal, and a
/// swap sweeps EVERY artifact kind an old generation projected — rows, FTS,
/// small tables, bucket residue. A new table added without provenance fails
/// here instead of silently corrupting swaps.
#[test]
fn activation_sweeps_every_artifact_kind_and_leaves_the_journal_empty() {
    use rosetta_core::rows::{
        EditKind, FileEditRow, PlanRow, SessionUsageRow, TaskRow, TaskStatus, WorkspaceObsRow,
    };

    let mut store = Store::open_in_memory().unwrap();
    store.apply(&base_batch()).unwrap();
    store
        .apply(&Batch {
            ops: vec![],
            checkpoints: vec![CheckpointUpdate {
                runtime_id: "rt1".into(),
                stream: "transcript".into(),
                path: "p/x.jsonl".into(),
                session_id: "s1".into(),
                expected: None,
                next: "blob-v0".into(),
                updated_at: Some(1),
            }],
            source: None,
        })
        .unwrap();
    let cursor = queries::cursor_state(store.conn(), "rt1", "p/x.jsonl")
        .unwrap()
        .unwrap();

    let journal_rows = |store: &Store| -> i64 {
        store
            .conn()
            .query_row("SELECT COUNT(*) FROM source_op", [], |row| row.get(0))
            .unwrap()
    };
    let count = |store: &Store, sql: &str| -> i64 {
        store.conn().query_row(sql, [], |row| row.get(0)).unwrap()
    };

    // Generation 1: one of every artifact kind.
    let mut m1 = message("g1-user", 0, MessageKind::User);
    m1.content = Some("generation-one alpha".into());
    m1.usage = Some(TokenSnapshot {
        input: Some(10),
        output: Some(5),
        ..Default::default()
    });
    m1.src_cursor = Some(cursor.id);
    m1.src_offset = Some(0);
    let mut m2 = message("g1-tool", 1, MessageKind::ToolUse);
    m2.tool = Some(ToolCall {
        tool_id: Some("toolu_g1".into()),
        name: "Bash".into(),
        input: serde_json::json!({"command": "true"}),
        result: None,
        payload: None,
        is_error: None,
    });
    m2.src_cursor = Some(cursor.id);
    m2.src_offset = Some(100);
    let gen1: Vec<IngestOp> = vec![
        IngestOp::Message(Box::new(m1)),
        IngestOp::Message(Box::new(m2)),
        IngestOp::ToolResult {
            session_id: "s1".into(),
            tool_id: "toolu_g1".into(),
            result: Some("gen-one result".into()),
            payload: None,
            is_error: Some(false),
            at: Some(3_700_000),
        },
        IngestOp::WorkspaceObs(WorkspaceObsRow {
            session_id: "s1".into(),
            at: 3_600_000,
            cwd: "/w/rosetta".into(),
            git_branch: Some("main".into()),
            git_repo: None,
        }),
        IngestOp::FileEdit(FileEditRow {
            session_id: "s1".into(),
            message_timeline_id: Some("g1-tool".into()),
            path: "/w/rosetta/a.rs".into(),
            kind: EditKind::Edit,
            snapshot_ref: None,
            lines_added: Some(3),
            lines_removed: Some(1),
            at: Some(3_600_000),
        }),
        IngestOp::Plan(PlanRow {
            session_id: "s1".into(),
            revision: 1,
            content: "gen-one plan".into(),
            updated_at: 3_600_000,
        }),
        IngestOp::Task(TaskRow {
            id: "t-g1".into(),
            session_id: "s1".into(),
            status: TaskStatus::Completed,
            content: "gen-one task".into(),
            created_at: Some(3_600_000),
            updated_at: Some(3_600_500),
        }),
        IngestOp::SessionUsage(SessionUsageRow {
            session_id: "s1".into(),
            key: "g1-usage".into(),
            ts: 3_600_000,
            model: Some("claude-opus-5".into()),
            tokens: TokenSnapshot {
                input: Some(100),
                output: Some(20),
                ..Default::default()
            },
        }),
    ];
    let staged = gen1.len() as u64;
    store
        .apply(&Batch {
            ops: gen1,
            checkpoints: vec![],
            source: Some(SourceWrite {
                cursor_id: cursor.id,
                session_id: "s1".into(),
                generation: 1,
                first_sequence: 0,
                mode: SourceMode::Stage,
            }),
        })
        .unwrap();
    // Staged: journaled, projected NOTHING.
    assert_eq!(journal_rows(&store), staged as i64);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM message"), 0);

    store
        .apply(&Batch {
            ops: vec![],
            checkpoints: vec![],
            source: Some(SourceWrite {
                cursor_id: cursor.id,
                session_id: "s1".into(),
                generation: 1,
                first_sequence: staged,
                mode: SourceMode::Activate,
            }),
        })
        .unwrap();
    assert_eq!(journal_rows(&store), 0, "activation must prune the journal");
    assert_eq!(count(&store, "SELECT COUNT(*) FROM message"), 2);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM file_edit"), 1);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM workspace_obs"), 1);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM plan"), 1);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM task"), 1);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM session_usage"), 1);
    assert_eq!(queries::search(store.conn(), "alpha", 5).unwrap().len(), 1);

    // Generation 2 rewrite: a single, different message. Everything from
    // generation 1 must vanish — rows, FTS, small tables, bucket residue.
    let mut m3 = message("g2-user", 0, MessageKind::User);
    m3.content = Some("generation-two omega".into());
    m3.usage = Some(TokenSnapshot {
        input: Some(7),
        ..Default::default()
    });
    m3.src_cursor = Some(cursor.id);
    m3.src_offset = Some(0);
    store
        .apply(&Batch {
            ops: vec![IngestOp::Message(Box::new(m3))],
            checkpoints: vec![],
            source: Some(SourceWrite {
                cursor_id: cursor.id,
                session_id: "s1".into(),
                generation: 2,
                first_sequence: 0,
                mode: SourceMode::Stage,
            }),
        })
        .unwrap();
    // Mid-stage: old generation still fully visible.
    assert_eq!(count(&store, "SELECT COUNT(*) FROM message"), 2);
    assert_eq!(journal_rows(&store), 1);

    store
        .apply(&Batch {
            ops: vec![],
            checkpoints: vec![],
            source: Some(SourceWrite {
                cursor_id: cursor.id,
                session_id: "s1".into(),
                generation: 2,
                first_sequence: 1,
                mode: SourceMode::Activate,
            }),
        })
        .unwrap();
    assert_eq!(journal_rows(&store), 0);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM message"), 1);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM file_edit"), 0);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM workspace_obs"), 0);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM plan"), 0);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM task"), 0);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM session_usage"), 0);
    assert_eq!(queries::search(store.conn(), "alpha", 5).unwrap().len(), 0);
    assert_eq!(queries::search(store.conn(), "omega", 5).unwrap().len(), 1);
    let (buckets, tokens): (i64, i64) = store
        .conn()
        .query_row(
            "SELECT COUNT(*), COALESCE(SUM(COALESCE(in_tokens,0) + COALESCE(out_tokens,0)), 0)
             FROM usage_bucket WHERE session_id = 's1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((buckets, tokens), (1, 7), "no bucket residue from gen one");
    let (rows, min_ord, max_ord): (i64, i64, i64) = store
        .conn()
        .query_row(
            "SELECT COUNT(*), MIN(ordinal), MAX(ordinal) FROM message WHERE session_id = 's1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!((min_ord, max_ord), (0, rows - 1), "ordinals stay dense");

    // The hot path: appends project without journaling.
    let mut m4 = message("g2-append", 9, MessageKind::User);
    m4.content = Some("appended".into());
    m4.src_cursor = Some(cursor.id);
    m4.src_offset = Some(500);
    store
        .apply(&Batch {
            ops: vec![IngestOp::Message(Box::new(m4))],
            checkpoints: vec![],
            source: Some(SourceWrite {
                cursor_id: cursor.id,
                session_id: "s1".into(),
                generation: 2,
                first_sequence: 1,
                mode: SourceMode::Append,
            }),
        })
        .unwrap();
    assert_eq!(journal_rows(&store), 0, "appends never journal");
    assert_eq!(count(&store, "SELECT COUNT(*) FROM message"), 2);
}

#[test]
fn abandoning_an_unclean_generation_keeps_the_active_projection_and_drift() {
    let mut store = Store::open_in_memory().unwrap();
    store.apply(&base_batch()).unwrap();
    store
        .apply(&Batch {
            ops: vec![],
            checkpoints: vec![CheckpointUpdate {
                runtime_id: "rt1".into(),
                stream: "transcript".into(),
                path: "p/unclean.jsonl".into(),
                session_id: "s1".into(),
                expected: None,
                next: "fresh".into(),
                updated_at: Some(1),
            }],
            source: None,
        })
        .unwrap();
    let cursor = queries::cursor_state(store.conn(), "rt1", "p/unclean.jsonl")
        .unwrap()
        .unwrap();

    let mut clean = message("clean", 0, MessageKind::User);
    clean.content = Some("last clean projection".into());
    clean.src_cursor = Some(cursor.id);
    store
        .apply(&Batch {
            ops: vec![IngestOp::Message(Box::new(clean))],
            checkpoints: vec![],
            source: Some(SourceWrite {
                cursor_id: cursor.id,
                session_id: "s1".into(),
                generation: 1,
                first_sequence: 0,
                mode: SourceMode::Activate,
            }),
        })
        .unwrap();

    let mut partial = message("partial", 0, MessageKind::User);
    partial.content = Some("must stay hidden".into());
    partial.src_cursor = Some(cursor.id);
    store
        .apply(&Batch {
            ops: vec![
                IngestOp::Message(Box::new(partial)),
                IngestOp::Drift(DriftFact {
                    runtime_id: "rt1".into(),
                    agent_version: "1".into(),
                    stream: "transcript".into(),
                    record_kind: "known".into(),
                    error_code: "parse_failed".into(),
                    shape_hash: "shape".into(),
                    at: Some(2),
                    sample_cursor: Some(cursor.id),
                    sample_offset: Some(0),
                }),
            ],
            checkpoints: vec![],
            source: Some(SourceWrite {
                cursor_id: cursor.id,
                session_id: "s1".into(),
                generation: 2,
                first_sequence: 0,
                mode: SourceMode::Stage,
            }),
        })
        .unwrap();
    store
        .apply(&Batch {
            ops: vec![],
            checkpoints: vec![],
            source: Some(SourceWrite {
                cursor_id: cursor.id,
                session_id: "s1".into(),
                generation: 2,
                first_sequence: 2,
                mode: SourceMode::Abandon,
            }),
        })
        .unwrap();

    assert_eq!(
        queries::search(store.conn(), "last clean", 5)
            .unwrap()
            .len(),
        1
    );
    assert!(
        queries::search(store.conn(), "must stay hidden", 5)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        store
            .conn()
            .query_row("SELECT COUNT(*) FROM source_op", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        store
            .conn()
            .query_row("SELECT count FROM drift_agg", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    let cursor = queries::cursor_state(store.conn(), "rt1", "p/unclean.jsonl")
        .unwrap()
        .unwrap();
    assert_eq!(
        (cursor.active_generation, cursor.highest_generation),
        (1, 2)
    );
}

#[test]
fn workspace_changes_after_session_start_do_not_move_the_project() {
    use rosetta_core::rows::WorkspaceObsRow;

    let mut store = Store::open_in_memory().unwrap();
    store.apply(&base_batch()).unwrap();
    store
        .apply(&Batch {
            ops: vec![IngestOp::WorkspaceObs(WorkspaceObsRow {
                session_id: "s1".into(),
                at: 20_000,
                cwd: "/w/other".into(),
                git_branch: None,
                git_repo: None,
            })],
            checkpoints: vec![],
            source: None,
        })
        .unwrap();
    assert_eq!(
        queries::session(store.conn(), "s1")
            .unwrap()
            .unwrap()
            .project_id
            .as_deref(),
        Some("p1")
    );

    store
        .apply(&Batch {
            ops: vec![IngestOp::WorkspaceObs(WorkspaceObsRow {
                session_id: "s1".into(),
                at: 9_000,
                cwd: "/w/earlier".into(),
                git_branch: None,
                git_repo: None,
            })],
            checkpoints: vec![],
            source: None,
        })
        .unwrap();
    let expected = rosetta_core::ids::project_id("/w/earlier", None);
    assert_eq!(
        queries::session(store.conn(), "s1")
            .unwrap()
            .unwrap()
            .project_id,
        Some(expected)
    );
}

#[test]
fn message_timestamps_only_write_when_the_session_clock_advances() {
    let mut store = Store::open_in_memory_for_rebuild().unwrap();
    store.apply(&base_batch()).unwrap();
    let updated_at = |store: &Store| {
        store
            .conn()
            .query_row(
                "SELECT updated_at FROM session WHERE id = 's1'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap()
    };

    let mut older = message("m-older", 0, MessageKind::User);
    older.ts = Some(5_000);
    let before = store.conn().total_changes();
    store
        .apply(&Batch {
            ops: vec![IngestOp::Message(Box::new(older))],
            checkpoints: vec![],
            source: None,
        })
        .unwrap();
    let older_changes = store.conn().total_changes() - before;
    assert_eq!(older_changes, 1, "only the message row is new");
    assert_eq!(updated_at(&store), 10_000);

    let mut newer = message("m-newer", 1, MessageKind::User);
    newer.ts = Some(20_000);
    let before = store.conn().total_changes();
    store
        .apply(&Batch {
            ops: vec![IngestOp::Message(Box::new(newer))],
            checkpoints: vec![],
            source: None,
        })
        .unwrap();
    let newer_changes = store.conn().total_changes() - before;
    assert_eq!(newer_changes, 2, "the message and session rows both change");
    assert_eq!(updated_at(&store), 20_000);
}
