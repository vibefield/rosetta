//! Fixture-backed canonical projection for Hermes snapshot rows.
//!
//! Regenerate the full canonical-op snapshots with:
//! `UPDATE_GOLDEN=1 cargo test -p rosetta-adapter-hermes --test projection`.

use std::path::{Path, PathBuf};

use rosetta_adapter_hermes::{HermesAdapter, spec, usage};
use rosetta_adapter_support::{AgentAdapter, LineStatus, NormalizeCtx};
use rosetta_capture::{SnapshotOutcome, snapshot_db};
use rosetta_core::timeline::{TimelineBody, TimelineEntry, ToolAction, ToolStatus};
use rosetta_core::{IngestOp, ids};

const VERSION: &str = "0.20.5";
const SCENARIOS: [&str; 3] = ["basic-chat", "resume", "tool-use"];

fn fixture_db(scenario: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
        "../../../groundtruth/hermes/fixtures/{VERSION}/{scenario}/hermes-home/state.db"
    ))
}

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

fn normalize_scenario(scenario: &str) -> Vec<IngestOp> {
    normalize_store(&fixture_db(scenario), spec::STATE_DB)
}

/// Replay one session store through the adapter's plan for `stream`.
fn normalize_store(path: &Path, stream: &str) -> Vec<IngestOp> {
    let adapter = HermesAdapter;
    let plan = adapter.snapshot_plan(stream).expect("snapshot plan");
    let SnapshotOutcome::Ready(snapshot) = snapshot_db(path).unwrap() else {
        panic!("{} did not produce a stable snapshot", path.display());
    };
    let sessions = snapshot.query_rows(&plan.sessions_sql, None).unwrap();
    assert_eq!(sessions.len(), 1, "fixture scenarios contain one session");
    let session: serde_json::Value = serde_json::from_str(&sessions[0]).unwrap();
    let native = session["native_session_id"].as_str().unwrap();
    let runtime_id = ids::runtime_id(adapter.kind(), "~/.hermes");
    let session_id = ids::session_id(&runtime_id, native);
    let mut ctx = NormalizeCtx::new(runtime_id, session_id);
    ctx.src_cursor = Some(7);

    let mut ops = Vec::new();
    for query in &plan.record_queries {
        for (offset, row) in snapshot
            .query_rows(&query.sql, Some(native))
            .unwrap()
            .into_iter()
            .enumerate()
        {
            ctx.src_offset = Some(offset as i64);
            let record = serde_json::json!({
                "query": &query.name,
                "row": serde_json::from_str::<serde_json::Value>(&row).unwrap(),
            });
            let outcome = adapter.ingest_line(&mut ctx, stream, record.to_string().as_bytes());
            assert_eq!(
                outcome.status,
                LineStatus::Parsed,
                "{}/{}: {row}",
                path.display(),
                query.name
            );
            ops.extend(outcome.ops);
        }
    }
    ops
}

/// Ingest synthetic `{"query", "row"}` records through one session context.
fn ingest(
    ctx: &mut NormalizeCtx,
    records: &[serde_json::Value],
) -> Vec<(LineStatus, Vec<IngestOp>)> {
    records
        .iter()
        .map(|record| {
            let outcome =
                HermesAdapter.ingest_line(ctx, spec::STATE_DB, record.to_string().as_bytes());
            (outcome.status, outcome.ops)
        })
        .collect()
}

fn drift_codes(ops: &[IngestOp]) -> Vec<(String, String)> {
    ops.iter()
        .filter_map(|op| match op {
            IngestOp::Drift(fact) => Some((fact.record_kind.clone(), fact.error_code.clone())),
            _ => None,
        })
        .collect()
}

fn usage_rows(ops: &[IngestOp]) -> Vec<&rosetta_core::rows::SessionUsageRow> {
    ops.iter()
        .filter_map(|op| match op {
            IngestOp::SessionUsage(row) => Some(row),
            _ => None,
        })
        .collect()
}

/// A minimal `sessions` row shaped like the 0.20.5 fixture columns.
fn session_record(id: &str, model_config: Option<&str>) -> serde_json::Value {
    serde_json::json!({
        "query": "sessions",
        "row": {
            "id": id,
            "source": "cli",
            "model": "synthetic-model",
            "model_config": model_config,
            "started_at": 1.0,
            "git_metadata_generation": 0,
            "compression_fallback_streak": 0,
            "compression_ineffective_count": 0,
            "rewind_count": 0,
            "archived": 0,
            "pinned": 0,
            "hidden": 0
        }
    })
}

/// A `session_model_usage` running total with the given route and task.
fn usage_record(
    session: &str,
    provider: &str,
    mode: &str,
    task: &str,
    calls: i64,
) -> serde_json::Value {
    serde_json::json!({
        "query": "session_model_usage",
        "row": {
            "session_id": session,
            "model": "synthetic-model",
            "billing_provider": provider,
            "billing_base_url": "",
            "billing_mode": mode,
            "task": task,
            "api_call_count": calls,
            "input_tokens": 100 * calls,
            "output_tokens": 10 * calls,
            "cache_read_tokens": 0,
            "cache_write_tokens": 0,
            "reasoning_tokens": 0,
            "estimated_cost_usd": 0.0,
            "actual_cost_usd": 0.0,
            "cost_status": "included",
            "cost_source": "none",
            "first_seen": 1.0,
            "last_seen": 2.0
        }
    })
}

fn messages(ops: &[IngestOp]) -> Vec<&TimelineEntry> {
    ops.iter()
        .filter_map(|op| match op {
            IngestOp::Message(row) => Some(row.as_ref()),
            _ => None,
        })
        .collect()
}

fn body_kind(row: &TimelineEntry) -> &'static str {
    match &row.body {
        TimelineBody::User { .. } => "user",
        TimelineBody::Agent { .. } => "agent",
        TimelineBody::Thinking { .. } => "thinking",
        TimelineBody::Collaboration { .. } => "collaboration",
        TimelineBody::ToolUse { .. } => "tool_use",
        TimelineBody::Notice { .. } => "notice",
        TimelineBody::Summary { .. } => "summary",
    }
}

fn text(row: &TimelineEntry) -> Option<&str> {
    match &row.body {
        TimelineBody::User { text, .. } | TimelineBody::Agent { text, .. } => {
            text.as_ref().map(|text| text.as_preview())
        }
        TimelineBody::Thinking { text } | TimelineBody::Summary { text, .. } => {
            Some(text.as_preview())
        }
        TimelineBody::Collaboration { event } => event.body.as_ref().map(|text| text.as_preview()),
        TimelineBody::ToolUse { .. } | TimelineBody::Notice { .. } => None,
    }
}

#[test]
fn basic_chat_drops_system_prompt_and_projects_flat_timeline() {
    let ops = normalize_scenario("basic-chat");
    let rows = messages(&ops);
    assert_eq!(
        rows.iter().map(|row| body_kind(row)).collect::<Vec<_>>(),
        ["user", "agent"]
    );
    assert_eq!(
        rows.iter().map(|row| row.ordinal).collect::<Vec<_>>(),
        [1, 2]
    );
    assert!(rows.iter().all(|row| row.usage.is_empty()));
    let usage: Vec<_> = ops
        .iter()
        .filter_map(|op| match op {
            IngestOp::SessionUsage(row) => Some(row),
            _ => None,
        })
        .collect();
    assert_eq!(usage.len(), 2);
    assert_eq!(
        usage
            .iter()
            .map(|row| row.tokens.input)
            .sum::<Option<i64>>(),
        Some(256)
    );
    assert!(!ops.iter().any(|op| matches!(op, IngestOp::Drift(_))));
}

#[test]
fn tool_result_merges_without_consuming_an_ordinal() {
    let ops = normalize_scenario("tool-use");
    let rows = messages(&ops);
    assert_eq!(
        rows.iter().map(|row| body_kind(row)).collect::<Vec<_>>(),
        ["user", "tool_use", "agent"]
    );
    assert_eq!(
        rows.iter().map(|row| row.ordinal).collect::<Vec<_>>(),
        [1, 2, 3]
    );

    let call = rows
        .iter()
        .find_map(|row| match &row.body {
            TimelineBody::ToolUse { tool } => Some(tool),
            _ => None,
        })
        .expect("tool call");
    assert_eq!(call.native.id.as_deref(), Some("call_mock_0002"));
    assert_eq!(call.native.name, "terminal");
    let ToolAction::Shell { command, .. } = &call.action else {
        panic!("terminal must project as shell")
    };
    assert_eq!(command.as_preview(), "echo rosetta-mock");

    let result = ops.iter().find_map(|op| match op {
        IngestOp::ToolUpdate {
            correlation_key,
            patch,
            ..
        } => Some((correlation_key, patch)),
        _ => None,
    });
    let (correlation_key, patch) = result.expect("tool result merge");
    assert_eq!(correlation_key, "call_mock_0002");
    assert_eq!(
        patch.status.as_ref().map(|status| status.value),
        Some(ToolStatus::Succeeded)
    );
    assert!(
        patch.failure.is_none(),
        "successful output is intentionally discarded"
    );
}

#[test]
fn resume_keeps_native_message_id_order() {
    let rows = normalize_scenario("resume");
    let rows = messages(&rows);
    assert_eq!(
        rows.iter().map(|row| body_kind(row)).collect::<Vec<_>>(),
        ["user", "agent", "user", "agent"]
    );
    assert_eq!(
        rows.iter().map(|row| row.ordinal).collect::<Vec<_>>(),
        [1, 2, 3, 4]
    );
    assert!(text(rows[2]).unwrap().contains("goodbye"));
}

#[test]
fn replay_is_deterministic() {
    for scenario in SCENARIOS {
        assert_eq!(
            normalize_scenario(scenario),
            normalize_scenario(scenario),
            "{scenario} replayed differently"
        );
    }
}

#[test]
fn canonical_ops_match_goldens() {
    let update = std::env::var_os("UPDATE_GOLDEN").is_some();
    for scenario in SCENARIOS {
        let ops = normalize_scenario(scenario);
        assert!(!ops.is_empty(), "{scenario} produced no canonical ops");
        assert!(
            !ops.iter().any(|op| matches!(op, IngestOp::Drift(_))),
            "{scenario} produced drift: {ops:#?}"
        );

        let rendered = format!("{}\n", serde_json::to_string_pretty(&ops).unwrap());
        let golden = golden_dir().join(VERSION).join(format!("{scenario}.json"));
        if update {
            std::fs::create_dir_all(golden.parent().unwrap()).unwrap();
            std::fs::write(&golden, rendered).unwrap();
            continue;
        }
        let expected = std::fs::read_to_string(&golden).unwrap_or_else(|error| {
            panic!(
                "{}: {error} — regenerate with UPDATE_GOLDEN=1 cargo test \
                 -p rosetta-adapter-hermes --test projection",
                golden.display()
            )
        });
        assert_eq!(rendered, expected, "changed {VERSION}/{scenario}");
    }
}

#[test]
fn goldens_carry_no_personal_paths() {
    for entry in walkdir::WalkDir::new(golden_dir())
        .sort_by_file_name()
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
    {
        let text = std::fs::read_to_string(entry.path()).unwrap();
        for needle in [concat!("/", "Users/"), "/home/", concat!("C:\\\\", "Users")] {
            assert!(
                !text.contains(needle),
                "{} contains a local path ({needle})",
                entry.path().display()
            );
        }
    }
}

#[test]
fn known_malformed_rows_are_hard_errors_and_new_tables_are_soft_drift() {
    let adapter = HermesAdapter;
    let mut ctx = NormalizeCtx::new("runtime", "session");
    let malformed = serde_json::json!({
        "query": "messages",
        "row": {"id": 1, "session_id": "s", "role": "assistant"},
    });
    let outcome = adapter.ingest_line(&mut ctx, spec::STATE_DB, malformed.to_string().as_bytes());
    assert!(matches!(outcome.status, LineStatus::HardError { .. }));
    assert!(
        outcome
            .ops
            .iter()
            .any(|op| matches!(op, IngestOp::Drift(_)))
    );

    let future = serde_json::json!({
        "query": "schema_inventory",
        "row": {"table_name": "future_table"},
    });
    let outcome = adapter.ingest_line(&mut ctx, spec::STATE_DB, future.to_string().as_bytes());
    assert_eq!(outcome.status, LineStatus::UnknownType);
    assert!(
        outcome
            .ops
            .iter()
            .any(|op| matches!(op, IngestOp::Drift(_)))
    );
}

#[test]
fn tool_specific_and_plain_results_keep_only_typed_lifecycle_facts() {
    fn project(content: &str) -> (LineStatus, Vec<IngestOp>) {
        let adapter = HermesAdapter;
        let mut ctx = NormalizeCtx::new("runtime", "session");
        let record = serde_json::json!({
            "query": "messages",
            "row": {
                "id": 7,
                "session_id": "session",
                "role": "tool",
                "content": content,
                "tool_call_id": "call-7",
                "tool_name": "terminal",
                "timestamp": 1.0,
                "active": 1,
                "compacted": 0
            }
        });
        let outcome = adapter.ingest_line(&mut ctx, spec::STATE_DB, record.to_string().as_bytes());
        (outcome.status, outcome.ops)
    }

    let (status, ops) = project(r#"{"content":"synthetic file body","file_size":19}"#);
    assert_eq!(status, LineStatus::UnknownType);
    assert!(ops.iter().any(|op| matches!(op, IngestOp::Drift(_))));
    let structured = ops.iter().find_map(|op| match op {
        IngestOp::ToolUpdate { patch, .. } => Some(patch),
        _ => None,
    });
    let structured = structured.expect("structured result");
    assert_eq!(
        structured.status.as_ref().map(|status| status.value),
        Some(ToolStatus::Succeeded)
    );
    assert!(structured.failure.is_none());

    let (status, ops) = project("plain synthetic output");
    assert_eq!(status, LineStatus::UnknownType);
    let plain = ops.iter().find_map(|op| match op {
        IngestOp::ToolUpdate { patch, .. } => Some(patch),
        _ => None,
    });
    let plain = plain.expect("plain result");
    assert_eq!(
        plain.status.as_ref().map(|status| status.value),
        Some(ToolStatus::Succeeded)
    );
    assert!(plain.failure.is_none());

    let (status, ops) = project(r#"{"error":"synthetic failure"}"#);
    assert_eq!(status, LineStatus::UnknownType);
    let error = ops.iter().find_map(|op| match op {
        IngestOp::ToolUpdate { patch, .. } => Some(patch),
        _ => None,
    });
    let error = error.expect("error result");
    assert_eq!(
        error.status.as_ref().map(|status| status.value),
        Some(ToolStatus::Failed)
    );
    assert_eq!(
        error
            .failure
            .as_ref()
            .and_then(|failure| failure.diagnostic.as_deref()),
        Some("synthetic failure")
    );
}

/// HERMES-3: with `/codex-runtime` on, Hermes drives the Codex app-server,
/// records only each turn's last request, and the app-server's own rollout
/// lands in `~/.codex/sessions` where the Codex adapter counts every request.
/// The session row's `model_config.codex_thread_id` names that runtime; its
/// main-loop usage is withheld with a drift notice, Hermes-side auxiliary
/// calls stay, and a row that names the runtime itself is withheld even
/// without the session row.
#[test]
fn codex_runtime_sessions_withhold_main_loop_usage_only() {
    let native = "20260923_120000_0123456789ab";
    let runtime_id = ids::runtime_id("hermes", "~/.hermes");
    let session_id = ids::session_id(&runtime_id, native);

    let mut ctx = NormalizeCtx::new(runtime_id.clone(), session_id.clone());
    let outcomes = ingest(
        &mut ctx,
        &[
            session_record(native, Some(r#"{"codex_thread_id":"thr_synthetic"}"#)),
            usage_record(native, "openai-codex", "subscription_included", "", 4),
            usage_record(native, "openai-codex", "", "title_generation", 1),
        ],
    );
    assert!(
        outcomes
            .iter()
            .all(|(status, _)| *status == LineStatus::Parsed)
    );
    let (_, main_loop) = &outcomes[1];
    assert!(
        usage_rows(main_loop).is_empty(),
        "app-server total must not be sampled"
    );
    assert_eq!(
        drift_codes(main_loop),
        vec![(
            "session_model_usage".to_owned(),
            usage::USAGE_RECORDED_BY_CODEX.to_owned()
        )]
    );
    let (_, aux) = &outcomes[2];
    assert_eq!(
        usage_rows(aux).len(),
        1,
        "Hermes-side auxiliary call is kept"
    );
    assert!(drift_codes(aux).is_empty());

    // An empty or cleared binding leaves the ordinary path untouched.
    for config in [
        None,
        Some("{}"),
        Some(r#"{"codex_thread_id":null}"#),
        Some("not json"),
    ] {
        let mut ctx = NormalizeCtx::new(runtime_id.clone(), session_id.clone());
        let outcomes = ingest(
            &mut ctx,
            &[
                session_record(native, config),
                usage_record(native, "openai-codex", "subscription_included", "", 4),
            ],
        );
        let (_, ops) = &outcomes[1];
        assert_eq!(usage_rows(ops).len(), 1, "{config:?}");
        assert!(drift_codes(ops).is_empty(), "{config:?}");
    }

    // A row persisted after a `/model` switch onto the app-server names the
    // runtime in `billing_mode` and needs no session row.
    let mut ctx = NormalizeCtx::new(runtime_id, session_id);
    let outcomes = ingest(
        &mut ctx,
        &[usage_record(native, "openai", "codex_app_server", "", 2)],
    );
    let (status, ops) = &outcomes[0];
    assert_eq!(*status, LineStatus::Parsed);
    assert!(usage_rows(ops).is_empty());
    assert_eq!(drift_codes(ops).len(), 1);
}

/// HERMES-11: the 0.21.x tools the projector registry already maps are not
/// unknown subtypes. Their result payloads stay soft drift until the L1
/// re-pin captures the shapes.
#[test]
fn registered_tool_names_are_known_without_fixture_facets() {
    let adapter = HermesAdapter;
    let mut ctx = NormalizeCtx::new("runtime", "session");
    let call = serde_json::json!({
        "query": "messages",
        "row": {
            "id": 3,
            "session_id": "session",
            "role": "assistant",
            "content": "",
            "tool_calls": r#"[{"id":"call_synthetic_0003","call_id":"call_synthetic_0003","response_item_id":"fc_synthetic","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"notes.txt\"}"}}]"#,
            "timestamp": 3.0,
            "active": 1,
            "compacted": 0
        }
    });
    let outcome = adapter.ingest_line(&mut ctx, spec::STATE_DB, call.to_string().as_bytes());
    assert_eq!(outcome.status, LineStatus::Parsed);
    assert!(
        drift_codes(&outcome.ops).is_empty(),
        "{:?}",
        drift_codes(&outcome.ops)
    );
    let tool = messages(&outcome.ops)
        .into_iter()
        .find_map(|row| match &row.body {
            TimelineBody::ToolUse { tool } => Some(tool.clone()),
            _ => None,
        })
        .expect("projected tool call");
    assert_eq!(tool.native.name, "read_file");
    assert!(matches!(tool.action, ToolAction::FileRead { .. }));

    let result = serde_json::json!({
        "query": "messages",
        "row": {
            "id": 4,
            "session_id": "session",
            "role": "tool",
            "content": r#"{"content":"synthetic file body","file_size":19}"#,
            "tool_call_id": "call_synthetic_0003",
            "tool_name": "read_file",
            "timestamp": 4.0,
            "active": 1,
            "compacted": 0
        }
    });
    let outcome = adapter.ingest_line(&mut ctx, spec::STATE_DB, result.to_string().as_bytes());
    // Only the unfixtured payload shape is drift; the name itself is known.
    assert_eq!(outcome.status, LineStatus::UnknownType);
    assert_eq!(
        drift_codes(&outcome.ops),
        vec![(
            "messages/tool_result".to_owned(),
            "unknown_subtype".to_owned()
        )]
    );

    let unknown = serde_json::json!({
        "query": "messages",
        "row": {
            "id": 5,
            "session_id": "session",
            "role": "tool",
            "content": "plain synthetic output",
            "tool_call_id": "call_synthetic_0005",
            "tool_name": "web_search",
            "timestamp": 5.0,
            "active": 1,
            "compacted": 0
        }
    });
    let outcome = adapter.ingest_line(&mut ctx, spec::STATE_DB, unknown.to_string().as_bytes());
    assert_eq!(outcome.status, LineStatus::UnknownType);
    assert!(drift_codes(&outcome.ops).contains(&(
        "tool_result/web_search".to_owned(),
        "unknown_subtype".to_owned()
    )));
}

/// HERMES-10: a named profile's store (`profiles/<name>/state.db`) replays
/// through the same plan as the default store and yields the same canonical
/// ops, because its session ids are unique across stores by construction.
#[test]
fn profile_store_replays_like_the_default_store() {
    let scratch = std::env::temp_dir().join(format!(
        "rosetta-hermes-profile-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let profile_dir = scratch.join("profiles").join("coder");
    std::fs::create_dir_all(&profile_dir).unwrap();
    let profile_db = profile_dir.join("state.db");
    std::fs::copy(fixture_db("tool-use"), &profile_db).unwrap();

    let from_profile = normalize_store(&profile_db, spec::PROFILE_STATE_DB);
    std::fs::remove_dir_all(&scratch).unwrap();
    assert_eq!(from_profile, normalize_scenario("tool-use"));
    assert!(!usage_rows(&from_profile).is_empty());
}
