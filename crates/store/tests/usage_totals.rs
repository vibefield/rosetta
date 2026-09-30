//! Cumulative usage is a floor over a session tree, never another request.
use rosetta_core::{
    IngestOp, ModelUsageTotal, RuntimeRow, SessionRole, SessionRow, SessionUsageRow,
    SessionUsageTotals, StreamState, TokenSnapshot,
};
use rosetta_store::{Batch, CheckpointUpdate, SourceMode, SourceWrite, Store, queries};

const AT: i64 = 1_788_267_600_000;
const MODEL: &str = "claude-haiku-4-5";
const DOLLAR: i64 = 1_000_000_000_000;

fn session(id: &str, parent: Option<&str>) -> IngestOp {
    IngestOp::SessionMeta(SessionRow {
        id: id.into(),
        native_session_id: id.into(),
        runtime_id: "rt".into(),
        project_id: None,
        cwd: None,
        parent_session_id: parent.map(str::to_owned),
        delegation: None,
        root_session_id: "root".into(),
        role: if parent.is_some() {
            SessionRole::Subagent
        } else {
            SessionRole::Main
        },
        title: None,
        summary: None,
        started_at: AT,
        updated_at: AT,
        activity: None,
        stream_state: StreamState::Indexed,
    })
}

fn setup(store: &mut Store) {
    apply(
        store,
        vec![
            IngestOp::Runtime(RuntimeRow {
                id: "rt".into(),
                kind: "fixture".into(),
                data_root: "/fixture".into(),
                enabled: true,
                account: None,
                config: None,
                skills: None,
                versions_seen: None,
                discovered_at: AT,
                label: None,
                provenance: Default::default(),
            }),
            session("root", None),
            session("child", Some("root")),
        ],
    );
}

fn apply(store: &mut Store, ops: Vec<IngestOp>) {
    store
        .apply(&Batch {
            ops,
            ..Batch::default()
        })
        .unwrap();
}

fn request(session: &str, key: &str, tokens: TokenSnapshot) -> IngestOp {
    IngestOp::SessionUsage(SessionUsageRow {
        session_id: session.into(),
        key: key.into(),
        ts: AT,
        model: Some(MODEL.into()),
        tokens,
        identity: None,
    })
}

fn io(input: i64, output: i64) -> TokenSnapshot {
    TokenSnapshot {
        input: Some(input),
        output: Some(output),
        ..Default::default()
    }
}

fn total(input: i64, output: i64, cost: Option<i64>) -> ModelUsageTotal {
    ModelUsageTotal {
        model: MODEL.into(),
        input,
        output,
        thinking: None,
        cache_read: 0,
        cache_write: 0,
        web_searches: 0,
        estimated_pico_usd: cost,
    }
}

fn report(session: &str, models: Vec<ModelUsageTotal>) -> IngestOp {
    IngestOp::SessionUsageTotals(SessionUsageTotals {
        session_id: session.into(),
        ts: AT,
        models,
    })
}

fn sum(store: &Store, expression: &str) -> i64 {
    store
        .conn()
        .query_row(
            &format!("SELECT COALESCE(SUM({expression}),0) FROM usage_bucket"),
            [],
            |r| r.get(0),
        )
        .unwrap()
}

fn tokens(store: &Store) -> i64 {
    sum(
        store,
        "COALESCE(in_tokens,0)+COALESCE(out_tokens,0)+COALESCE(think_tokens,0)+COALESCE(cache_r_tokens,0)+COALESCE(cache_w_tokens,0)+COALESCE(cache_w1h_tokens,0)",
    )
}

fn buckets(store: &Store) -> Vec<Vec<rusqlite::types::Value>> {
    let mut stmt = store
        .conn()
        .prepare("SELECT * FROM usage_bucket ORDER BY session_id, hour_ts, model, source")
        .unwrap();
    let columns = stmt.column_count();
    stmt.query_map([], |r| (0..columns).map(|i| r.get(i)).collect())
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

#[test]
fn duplicates_and_late_children_never_add_the_cumulative_total_twice() {
    let mut store = Store::open_in_memory().unwrap();
    setup(&mut store);
    let snapshot = report("root", vec![total(100, 200, Some(DOLLAR))]);
    apply(
        &mut store,
        vec![snapshot.clone(), request("root", "r1", io(40, 50))],
    );
    assert_eq!(tokens(&store), 300);
    apply(
        &mut store,
        vec![
            request("child", "r2", io(60, 100)),
            snapshot.clone(),
            snapshot,
        ],
    );
    assert_eq!(tokens(&store), 300);
    assert_eq!(sum(&store, "cost_pusd"), DOLLAR);
    assert_eq!(sum(&store, "message_count"), 0);
    let contribution = queries::contribution_for_scope(store.conn(), None).unwrap();
    assert_eq!(contribution.len(), 1);
    assert_eq!(contribution[0].tokens, 300);
    assert_eq!(contribution[0].spend.cost_micro_usd, Some(1_000_000));
    // 100 input + 150 output at Haiku list prices = $0.00085.
    assert_eq!(
        contribution[0].spend.reported_cost_adjustment_micro_usd,
        999_150
    );
    assert_eq!(contribution[0].spend.billed_micro_usd, None);
    assert_eq!(
        store
            .conn()
            .query_row("SELECT COUNT(*) FROM session_usage_total", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn the_calculated_estimate_can_win_and_a_stale_report_is_only_a_floor() {
    let mut store = Store::open_in_memory().unwrap();
    setup(&mut store);
    apply(
        &mut store,
        vec![report("root", vec![total(100, 200, Some(DOLLAR))])],
    );
    apply(
        &mut store,
        vec![request("child", "r", io(2_000_000, 1_000_000))],
    );
    assert_eq!(sum(&store, "cost_pusd"), 7 * DOLLAR);
    assert_eq!(tokens(&store), 3_000_000);
    assert_eq!(
        queries::contribution_for_scope(store.conn(), None).unwrap()[0]
            .spend
            .reported_cost_adjustment_micro_usd,
        0
    );
}

#[test]
fn inclusive_output_and_cache_writes_preserve_existing_subsets() {
    let mut store = Store::open_in_memory().unwrap();
    setup(&mut store);
    let mut model = total(10, 100, Some(DOLLAR));
    model.thinking = Some(90);
    model.cache_write = 400;
    model.cache_read = 500;
    apply(
        &mut store,
        vec![
            request(
                "child",
                "r",
                TokenSnapshot {
                    thinking: Some(20),
                    cache_write: Some(100),
                    cache_write_1h: Some(300),
                    cache_read: Some(500),
                    ..io(10, 50)
                },
            ),
            report("root", vec![model]),
        ],
    );
    assert_eq!(tokens(&store), 1010);
    // Only 30 missing output tokens can be supplemented. An improved thinking
    // split cannot manufacture 70 more tokens on top of known response output.
    assert_eq!(sum(&store, "out_tokens"), 50);
    assert_eq!(sum(&store, "think_tokens"), 50);
    assert_eq!(sum(&store, "cache_w1h_tokens"), 300);
    assert_eq!(sum(&store, "cache_w_tokens"), 100);
}

#[test]
fn dated_model_aliases_compare_against_the_same_requests() {
    let mut store = Store::open_in_memory().unwrap();
    setup(&mut store);
    let mut model = total(100, 200, Some(DOLLAR));
    model.model = "claude-haiku-4-5-20251001".into();
    apply(
        &mut store,
        vec![
            request("child", "r", io(100, 200)),
            report("root", vec![model]),
        ],
    );
    assert_eq!(tokens(&store), 300);
    assert_eq!(sum(&store, "cost_pusd"), DOLLAR);
}

#[test]
fn nested_totals_reconcile_from_the_leaves_without_overlapping() {
    let mut store = Store::open_in_memory().unwrap();
    setup(&mut store);
    apply(
        &mut store,
        vec![
            session("grandchild", Some("child")),
            report("root", vec![total(100, 200, Some(2 * DOLLAR))]),
            report("child", vec![total(50, 100, Some(DOLLAR))]),
            request("grandchild", "r", io(25, 50)),
        ],
    );
    assert_eq!(tokens(&store), 300);
    assert_eq!(sum(&store, "cost_pusd"), 2 * DOLLAR);
}

#[test]
fn an_unpriced_native_total_retains_tokens_and_coverage() {
    let mut store = Store::open_in_memory().unwrap();
    setup(&mut store);
    let mut model = total(100, 200, None);
    model.model = "unknown-model".into();
    apply(&mut store, vec![report("root", vec![model])]);
    assert_eq!(tokens(&store), 300);
    assert_eq!(sum(&store, "unpriced_tokens"), 300);
    let spend = &queries::contribution_for_scope(store.conn(), None).unwrap()[0].spend;
    assert_eq!(spend.cost_micro_usd, None);
}

#[test]
fn root_membership_covers_a_child_whose_direct_parent_is_unavailable() {
    let mut store = Store::open_in_memory().unwrap();
    setup(&mut store);
    let IngestOp::SessionMeta(mut child) = session("child", Some("root")) else {
        panic!()
    };
    child.parent_session_id = None;
    apply(
        &mut store,
        vec![
            IngestOp::SessionMeta(child),
            request("child", "r", io(100, 200)),
            report("root", vec![total(100, 200, Some(DOLLAR))]),
        ],
    );
    assert_eq!(tokens(&store), 300);
    assert_eq!(sum(&store, "cost_pusd"), DOLLAR);
}

#[test]
fn model_aliases_with_partly_unknown_estimates_keep_coverage_partial() {
    let mut store = Store::open_in_memory().unwrap();
    setup(&mut store);
    let known = total(100, 200, Some(DOLLAR));
    let mut unknown = total(100, 200, None);
    unknown.model = "claude-haiku-4-5-20251001".into();
    apply(&mut store, vec![report("root", vec![known, unknown])]);
    assert_eq!(tokens(&store), 600);
    assert_eq!(sum(&store, "cost_pusd"), DOLLAR);
    assert_eq!(sum(&store, "unpriced_tokens"), 300);
}

#[test]
fn live_cold_and_repricing_use_the_same_selected_totals() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("projection.sqlite");
    let mut live = Store::open(&path).unwrap();
    setup(&mut live);
    let mut cold = Store::open_in_memory_for_rebuild().unwrap();
    setup(&mut cold);
    let ops = vec![
        report("root", vec![total(100, 200, Some(DOLLAR))]),
        request("root", "r1", io(40, 50)),
        request("child", "r2", io(60, 100)),
    ];
    apply(&mut live, ops.clone());
    apply(&mut cold, ops);
    cold.finish_bulk_load().unwrap();
    let expected = buckets(&live);
    assert_eq!(buckets(&cold), expected);
    live.conn().execute_batch("UPDATE usage_bucket SET cost_pusd = 1; UPDATE projection_state SET pricing_version = 'old'").unwrap();
    drop(live);
    let mut reopened = Store::open(&path).unwrap();
    assert_eq!(buckets(&reopened), expected);
    apply(
        &mut reopened,
        vec![request("child", "new", io(0, 1_000_000))],
    );
    assert_eq!(sum(&reopened, "cost_pusd"), 5_000_850_000_000);
    assert_eq!(tokens(&reopened), 1_000_250);
}

#[test]
fn replacing_a_snapshot_removes_models_it_no_longer_reports() {
    let mut store = Store::open_in_memory().unwrap();
    setup(&mut store);
    apply(
        &mut store,
        vec![report("root", vec![total(100, 200, Some(DOLLAR))])],
    );
    let mut other = total(3, 4, Some(1));
    other.model = "another-model".into();
    apply(&mut store, vec![report("root", vec![other])]);
    assert_eq!(tokens(&store), 7);
    assert_eq!(sum(&store, "cost_pusd"), 1);
    apply(&mut store, vec![report("root", vec![])]);
    assert_eq!(tokens(&store), 0);
    assert_eq!(sum(&store, "cost_pusd"), 0);
}

#[test]
fn a_source_rewrite_retracts_its_total_and_the_derived_difference() {
    let mut store = Store::open_in_memory().unwrap();
    setup(&mut store);
    store
        .apply(&Batch {
            checkpoints: vec![CheckpointUpdate {
                runtime_id: "rt".into(),
                stream: "transcript".into(),
                path: "root.jsonl".into(),
                session_id: Some("root".into()),
                expected: None,
                next: "{}".into(),
                updated_at: None,
            }],
            ..Batch::default()
        })
        .unwrap();
    let cursor = queries::cursor_state(store.conn(), "rt", "root.jsonl")
        .unwrap()
        .unwrap();
    let source = |generation, mode| {
        Some(SourceWrite {
            cursor_id: cursor.id,
            session_id: "root".into(),
            generation,
            first_sequence: 0,
            mode,
        })
    };
    apply(&mut store, vec![request("child", "r", io(40, 50))]);
    store
        .apply(&Batch {
            source: source(1, SourceMode::Activate),
            ops: vec![report("root", vec![total(100, 200, Some(DOLLAR))])],
            ..Batch::default()
        })
        .unwrap();
    assert_eq!(tokens(&store), 300);
    // Abandoned replacements leave the selected total intact.
    store
        .apply(&Batch {
            source: source(2, SourceMode::Stage),
            ops: vec![report("root", vec![total(1_000, 2_000, Some(2 * DOLLAR))])],
            ..Batch::default()
        })
        .unwrap();
    assert_eq!(tokens(&store), 300);
    store
        .apply(&Batch {
            source: source(2, SourceMode::Abandon),
            ..Batch::default()
        })
        .unwrap();
    assert_eq!(tokens(&store), 300);
    store
        .apply(&Batch {
            source: source(3, SourceMode::Activate),
            ..Batch::default()
        })
        .unwrap();
    assert_eq!(tokens(&store), 90);
    assert_eq!(sum(&store, "cost_pusd"), 290_000_000);
}
