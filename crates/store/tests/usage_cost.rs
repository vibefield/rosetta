//! List-price cost in the usage rollup (docs/ARCHITECTURE.md §4.1.3): every
//! usage record is priced once by the compiled catalog, the live bucket
//! rebuild and the cold set-based fold agree exactly, unpriced usage is never
//! zero dollars, and a catalog change re-prices from stored facts.

use rosetta_core::rows::{
    MessageKind, MessageRow, ProjectRow, RuntimeRow, ServiceTier, SessionRole, SessionRow,
    SessionUsageRow, StreamState, TokenSnapshot, UsageScope,
};
use rosetta_core::{IngestOp, TimelineEntry, UsageCost};
use rosetta_pricing::{Request, list_cost, pricing_version};
use rosetta_store::{Batch, Store, pricing, queries};

const AUG_20: i64 = 1_787_227_200_000;
const AUG_21: i64 = 1_787_313_600_000;
const SEP_01: i64 = 1_788_264_000_000;

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
        label: None,
        provenance: Default::default(),
    }
}

fn session() -> SessionRow {
    SessionRow {
        id: "s1".into(),
        native_session_id: "native-s1".into(),
        runtime_id: "rt1".into(),
        project_id: Some("p1".into()),
        cwd: Some("/w/rosetta".into()),
        parent_session_id: None,
        delegation: None,
        root_session_id: "s1".into(),
        role: SessionRole::Main,
        title: None,
        summary: None,
        started_at: 10_000,
        updated_at: 10_000,
        activity: None,
        stream_state: StreamState::Streaming,
    }
}

fn agent(timeline_id: &str, ordinal: i64, ts: i64, model: &str, usage: TokenSnapshot) -> IngestOp {
    agent_in("s1", timeline_id, ordinal, ts, model, usage)
}

fn agent_in(
    session_id: &str,
    timeline_id: &str,
    ordinal: i64,
    ts: i64,
    model: &str,
    usage: TokenSnapshot,
) -> IngestOp {
    let row = MessageRow {
        timeline_id: timeline_id.into(),
        session_id: session_id.into(),
        ordinal,
        kind: MessageKind::Agent,
        subtype: None,
        uuid: None,
        parent_uuid: None,
        ts: Some(ts),
        model: Some(model.into()),
        effort: None,
        content: Some(format!("reply {timeline_id}")),
        tool: None,
        attachments: Vec::new(),
        citations: Vec::new(),
        group: None,
        response: None,
        block_index: None,
        branch_tool_id: None,
        usage: Some(usage),
        agent_version: None,
        src_cursor: None,
        src_offset: None,
    };
    let mut ops = IngestOp::project_message(row);
    assert_eq!(ops.len(), 1);
    ops.pop().unwrap()
}

fn sample(key: &str, ts: i64, model: &str, scope: UsageScope, tokens: TokenSnapshot) -> IngestOp {
    IngestOp::SessionUsage(SessionUsageRow {
        identity: None,
        session_id: "s1".into(),
        key: key.into(),
        ts,
        model: Some(model.into()),
        tokens: TokenSnapshot { scope, ..tokens },
    })
}

fn opus_usage() -> TokenSnapshot {
    TokenSnapshot {
        input: Some(1_000),
        output: Some(400),
        cache_read: Some(10_000),
        cache_write: Some(2_000),
        thinking: Some(600),
        cache_write_1h: Some(3_000),
        web_searches: Some(2),
        service_tier: Some(ServiceTier::Standard),
        route: None,
        billed_pico_usd: None,
        scope: UsageScope::Request,
        requests: None,
    }
}

fn io(input: i64, output: i64) -> TokenSnapshot {
    TokenSnapshot {
        input: Some(input),
        output: Some(output),
        ..TokenSnapshot::default()
    }
}

/// Records spanning a dated price change, an unknown model, a billed-only
/// model, and aggregate samples on both sides of a long-context tier.
fn usage_ops() -> Vec<IngestOp> {
    vec![
        IngestOp::Runtime(runtime()),
        IngestOp::Project(ProjectRow {
            id: "p1".into(),
            name: "rosetta".into(),
            root_paths: vec!["/w/rosetta".into()],
            git_repo: None,
        }),
        IngestOp::SessionMeta(session()),
        agent("opus", 0, SEP_01, "claude-opus-4-8", opus_usage()),
        agent("sol-before", 1, AUG_20, "gpt-5.6-sol", io(100_000, 10_000)),
        agent("sol-after", 2, AUG_21, "gpt-5.6-sol", io(100_000, 10_000)),
        agent("pickle", 3, SEP_01 + 1_000, "big-pickle", io(100, 10)),
        agent(
            "grok",
            4,
            SEP_01 + 2_000,
            "grok-4.9-build",
            TokenSnapshot {
                billed_pico_usd: Some(5_000_000_000),
                ..io(1_000, 50)
            },
        ),
        sample(
            "small",
            SEP_01 + 3_000,
            "gpt-5.4",
            UsageScope::Aggregate,
            io(75_177, 5_956),
        ),
        sample(
            "huge",
            SEP_01 + 4_000,
            "gpt-5.4",
            UsageScope::Aggregate,
            io(900_000, 1_000),
        ),
        sample(
            "counted",
            SEP_01 + 5_000,
            "gpt-5.4",
            UsageScope::Aggregate,
            counted_usage(),
        ),
    ]
}

/// The same total as `huge`, counted as four requests: each averages 225K
/// prompt tokens, below gpt-5.4's 272K tier.
fn counted_usage() -> TokenSnapshot {
    TokenSnapshot {
        requests: Some(4),
        ..io(900_000, 1_000)
    }
}

fn expected_cost(model: &str, at_ms: i64, scope: UsageScope, usage: &TokenSnapshot) -> Option<i64> {
    let usage = TokenSnapshot {
        scope,
        ..usage.clone()
    };
    list_cost(&Request::from_usage(model, Some(at_ms), &usage)).ok()
}

#[derive(Debug, PartialEq)]
struct Totals {
    cost: Option<i64>,
    billed: Option<i64>,
    unpriced: i64,
    one_hour: Option<i64>,
    searches: Option<i64>,
}

fn totals(store: &Store) -> Totals {
    store
        .conn()
        .query_row(
            "SELECT SUM(cost_pusd), SUM(billed_pusd), SUM(unpriced_tokens),
                    SUM(cache_w1h_tokens), SUM(web_searches)
             FROM usage_bucket",
            [],
            |row| {
                Ok(Totals {
                    cost: row.get(0)?,
                    billed: row.get(1)?,
                    unpriced: row.get(2)?,
                    one_hour: row.get(3)?,
                    searches: row.get(4)?,
                })
            },
        )
        .unwrap()
}

fn bucket_rows(store: &Store) -> Vec<Vec<Option<i64>>> {
    let mut statement = store
        .conn()
        .prepare(
            "SELECT hour_ts, in_tokens, out_tokens, cache_r_tokens, cache_w_tokens,
                    think_tokens, cache_w1h_tokens, web_searches, cost_pusd, billed_pusd,
                    unpriced_tokens, unrecorded_output, message_count, tool_call_count
             FROM usage_bucket ORDER BY session_id, hour_ts, model",
        )
        .unwrap();
    statement
        .query_map([], |row| {
            (0..14)
                .map(|index| row.get::<_, Option<i64>>(index))
                .collect()
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

/// A second session in the same tree, started later than `s1`.
fn subagent(id: &str, started_at: i64) -> SessionRow {
    SessionRow {
        id: id.into(),
        native_session_id: format!("native-{id}"),
        parent_session_id: Some("s1".into()),
        role: SessionRole::Subagent,
        started_at,
        updated_at: started_at,
        ..session()
    }
}

fn usage_with_identity(session_id: &str, response: &str, ts: i64, input: i64) -> IngestOp {
    IngestOp::Usage {
        session_id: session_id.into(),
        response: response.into(),
        ts,
        model: Some("claude-opus-4-8".into()),
        tokens: io(input, 10),
        identity: Some("msg_01:req_01".into()),
    }
}

fn row_usage(store: &Store, timeline_id: &str) -> (Option<i64>, Option<i64>) {
    store
        .conn()
        .query_row(
            "SELECT in_tokens, out_tokens FROM message WHERE timeline_id = ?1",
            [timeline_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap()
}

#[test]
fn a_response_copied_into_another_transcript_counts_once_for_the_earliest_session() {
    let mut store = Store::open_in_memory().unwrap();
    let mut ops = vec![
        IngestOp::Runtime(runtime()),
        IngestOp::Project(ProjectRow {
            id: "p1".into(),
            name: "rosetta".into(),
            root_paths: vec!["/w/rosetta".into()],
            git_repo: None,
        }),
        IngestOp::SessionMeta(session()),
        IngestOp::SessionMeta(subagent("s2", 20_000)),
        IngestOp::SessionMeta(subagent("s3", 30_000)),
    ];
    // Rows without usage in every session; the usage arrives as ops.
    ops.push(agent(
        "orig",
        0,
        SEP_01,
        "claude-opus-4-8",
        TokenSnapshot::default(),
    ));
    ops.push(agent_in(
        "s2",
        "copy",
        0,
        SEP_01,
        "claude-opus-4-8",
        TokenSnapshot::default(),
    ));
    ops.push(agent_in(
        "s3",
        "copy3",
        0,
        SEP_01,
        "claude-opus-4-8",
        TokenSnapshot::default(),
    ));
    // The subagent's copy lands first, then the origin displaces it, then a
    // later copy is dropped on arrival.
    ops.push(usage_with_identity("s2", "copy", SEP_01, 500));
    ops.push(usage_with_identity("s1", "orig", SEP_01, 500));
    ops.push(usage_with_identity("s3", "copy3", SEP_01, 500));
    let stats = store
        .apply(&Batch {
            ops,
            ..Batch::default()
        })
        .unwrap();
    assert_eq!(stats.usage_copies_dropped, 2);
    assert_eq!(row_usage(&store, "orig"), (Some(500), Some(10)));
    assert_eq!(row_usage(&store, "copy"), (None, None));
    assert_eq!(row_usage(&store, "copy3"), (None, None));
    let one = expected_cost("claude-opus-4-8", SEP_01, UsageScope::Request, &io(500, 10)).unwrap();
    assert_eq!(totals(&store).cost, Some(one));
    // A correction from the owner still applies; a copy's sample is inert.
    let mut ops = vec![usage_with_identity("s1", "orig", SEP_01, 700)];
    ops.push(IngestOp::SessionUsage(SessionUsageRow {
        session_id: "s2".into(),
        key: "response:msg_01".into(),
        ts: SEP_01,
        model: Some("claude-opus-4-8".into()),
        tokens: io(700, 10),
        identity: Some("msg_01:req_01".into()),
    }));
    store
        .apply(&Batch {
            ops,
            ..Batch::default()
        })
        .unwrap();
    assert_eq!(row_usage(&store, "orig"), (Some(700), Some(10)));
    let one = expected_cost("claude-opus-4-8", SEP_01, UsageScope::Request, &io(700, 10)).unwrap();
    assert_eq!(totals(&store).cost, Some(one));
    let samples: i64 = store
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM session_usage WHERE in_tokens IS NOT NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(samples, 0);
}

#[test]
fn unrecorded_output_is_counted_and_never_priced_as_zero() {
    let mut store = Store::open_in_memory().unwrap();
    let placeholder = TokenSnapshot {
        input: Some(200),
        cache_read: Some(10_000),
        ..TokenSnapshot::default()
    };
    store
        .apply(&Batch {
            ops: vec![
                IngestOp::Runtime(runtime()),
                IngestOp::Project(ProjectRow {
                    id: "p1".into(),
                    name: "rosetta".into(),
                    root_paths: vec!["/w/rosetta".into()],
                    git_repo: None,
                }),
                IngestOp::SessionMeta(session()),
                agent(
                    "streamed",
                    0,
                    SEP_01,
                    "claude-opus-4-8",
                    placeholder.clone(),
                ),
                agent("final", 1, SEP_01 + 1, "claude-opus-4-8", io(200, 50)),
            ],
            ..Batch::default()
        })
        .unwrap();
    let rows = bucket_rows(&store);
    assert_eq!(rows.len(), 1);
    // The prompt side is priced; the missing output counts as one unrecorded
    // record, so the bucket reads as a lower bound.
    let prompt_only =
        expected_cost("claude-opus-4-8", SEP_01, UsageScope::Request, &placeholder).unwrap();
    let complete =
        expected_cost("claude-opus-4-8", SEP_01, UsageScope::Request, &io(200, 50)).unwrap();
    assert_eq!(rows[0][8], Some(prompt_only + complete));
    assert_eq!(rows[0][10], Some(0), "unpriced tokens");
    assert_eq!(rows[0][11], Some(1), "unrecorded output");
    let entry = queries::timeline_entry(store.conn(), "s1", "streamed")
        .unwrap()
        .unwrap();
    assert_eq!(entry.usage.output, None);
    assert!(matches!(entry.cost, Some(UsageCost::Priced { .. })));
    // A web-search-only record has no prompt, so it is not "unrecorded output".
    let contribution = queries::contribution_for_scope(store.conn(), Some("p1")).unwrap();
    assert_eq!(contribution.len(), 1);
    assert_eq!(contribution[0].spend.unrecorded_output, 1);
}

#[test]
fn every_record_is_priced_once_and_unpriced_usage_is_never_zero() {
    let mut store = Store::open_in_memory().unwrap();
    store
        .apply(&Batch {
            ops: usage_ops(),
            ..Batch::default()
        })
        .unwrap();

    let opus = expected_cost(
        "claude-opus-4-8",
        SEP_01,
        UsageScope::Request,
        &opus_usage(),
    )
    .unwrap();
    let before = expected_cost(
        "gpt-5.6-sol",
        AUG_20,
        UsageScope::Request,
        &io(100_000, 10_000),
    )
    .unwrap();
    let after = expected_cost(
        "gpt-5.6-sol",
        AUG_21,
        UsageScope::Request,
        &io(100_000, 10_000),
    )
    .unwrap();
    let small =
        expected_cost("gpt-5.4", SEP_01, UsageScope::Aggregate, &io(75_177, 5_956)).unwrap();
    let counted =
        expected_cost("gpt-5.4", SEP_01, UsageScope::Aggregate, &counted_usage()).unwrap();
    // Four equal requests below the tier: every token at the base rates.
    assert_eq!(counted, 900_000 * 2_500_000 + 1_000 * 15_000_000);
    // The dated price change is visible per record: $5/$30 then $4/$20.
    assert_eq!(before, 100_000 * 5_000_000 + 10_000 * 30_000_000);
    assert_eq!(after, 100_000 * 4_000_000 + 10_000 * 20_000_000);

    let totals = totals(&store);
    assert_eq!(totals.cost, Some(opus + before + after + small + counted));
    // Unknown model (110), a billed Grok Build model the catalog does not
    // list (1,050), and an uncounted aggregate above gpt-5.4's 272K tier
    // (901,000) are unpriced, never $0.
    assert_eq!(totals.unpriced, 110 + 1_050 + 901_000);
    assert_eq!(totals.billed, Some(5_000_000_000));
    assert_eq!(totals.one_hour, Some(3_000));
    assert_eq!(totals.searches, Some(2));
    assert_eq!(
        pricing::stored_version(store.conn()).unwrap().as_deref(),
        Some(pricing_version())
    );
}

#[test]
fn live_rebuild_and_cold_fold_agree_bucket_for_bucket() {
    let mut live = Store::open_in_memory().unwrap();
    live.apply(&Batch {
        ops: usage_ops(),
        ..Batch::default()
    })
    .unwrap();

    let mut cold = Store::open_in_memory_for_rebuild().unwrap();
    cold.apply(&Batch {
        ops: usage_ops(),
        ..Batch::default()
    })
    .unwrap();
    cold.finish_bulk_load().unwrap();

    assert_eq!(bucket_rows(&live), bucket_rows(&cold));
    assert_eq!(totals(&live), totals(&cold));
    assert_eq!(
        pricing::stored_version(cold.conn()).unwrap().as_deref(),
        Some(pricing_version())
    );
}

#[test]
fn a_catalog_change_reprices_from_stored_facts_on_open() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("projection.sqlite");
    let expected = {
        let mut store = Store::open(&path).unwrap();
        store
            .apply(&Batch {
                ops: usage_ops(),
                ..Batch::default()
            })
            .unwrap();
        let expected = bucket_rows(&store);
        // Simulate a rollup priced by an older catalog.
        store
            .conn()
            .execute_batch(
                "UPDATE usage_bucket SET cost_pusd = 1, unpriced_tokens = 0;
                 UPDATE projection_state SET pricing_version = 'genai-prices@old';",
            )
            .unwrap();
        expected
    };
    let reopened = Store::open(&path).unwrap();
    assert_eq!(bucket_rows(&reopened), expected);
    assert_eq!(
        pricing::stored_version(reopened.conn()).unwrap().as_deref(),
        Some(pricing_version())
    );
}

#[test]
fn an_undated_record_is_ambiguous_on_a_dated_model_never_1970() {
    let mut store = Store::open_in_memory().unwrap();
    store
        .apply(&Batch {
            ops: vec![
                IngestOp::Runtime(runtime()),
                IngestOp::Project(ProjectRow {
                    id: "p1".into(),
                    name: "rosetta".into(),
                    root_paths: vec!["/w/rosetta".into()],
                    git_repo: None,
                }),
                IngestOp::SessionMeta(session()),
                // Usage ops carry ts = 0 when the agent recorded no time.
                agent("undated", 0, 0, "gpt-5.6-sol", io(100_000, 10_000)),
                sample(
                    "undated-sample",
                    0,
                    "gpt-5.6-sol",
                    UsageScope::Request,
                    io(1_000, 100),
                ),
                // A model with one price needs no time.
                agent("flat", 1, 0, "claude-opus-4-8", io(1_000, 100)),
            ],
            ..Batch::default()
        })
        .unwrap();
    let entry: TimelineEntry = queries::timeline_entry(store.conn(), "s1", "undated")
        .unwrap()
        .expect("stored row");
    assert_eq!(
        entry.cost,
        Some(UsageCost::Unpriced {
            reason: "ambiguous_time".into()
        })
    );
    let totals = totals(&store);
    assert_eq!(totals.unpriced, 110_000 + 1_100);
    assert_eq!(
        totals.cost,
        expected_cost(
            "claude-opus-4-8",
            SEP_01,
            UsageScope::Request,
            &io(1_000, 100)
        )
    );
}

#[test]
fn usage_facts_round_trip_through_the_timeline() {
    let mut store = Store::open_in_memory().unwrap();
    let mut usage = opus_usage();
    usage.route = Some("anthropic/us".into());
    usage.billed_pico_usd = Some(42);
    usage.service_tier = Some(ServiceTier::Fast);
    usage.scope = UsageScope::Aggregate;
    usage.requests = Some(3);
    store
        .apply(&Batch {
            ops: vec![
                IngestOp::Runtime(runtime()),
                IngestOp::Project(ProjectRow {
                    id: "p1".into(),
                    name: "rosetta".into(),
                    root_paths: vec!["/w/rosetta".into()],
                    git_repo: None,
                }),
                IngestOp::SessionMeta(session()),
                agent("opus", 0, SEP_01, "claude-opus-4-8", usage.clone()),
            ],
            ..Batch::default()
        })
        .unwrap();
    let entry: TimelineEntry = queries::timeline_entry(store.conn(), "s1", "opus")
        .unwrap()
        .expect("stored row");
    assert_eq!(entry.usage, usage);
    // Fast mode (2x) and US-only inference (1.1x) on Opus 4.8 both have a
    // cited rule, so the record prices through them, counted as 3 requests.
    let priced = expected_cost("claude-opus-4-8", SEP_01, UsageScope::Aggregate, &usage)
        .expect("cited fast and residency rules");
    assert_eq!(totals(&store).cost, Some(priced));
    assert_eq!(totals(&store).unpriced, 0);
    // A residency without a cited premium stays unpriced rather than guessed.
    let mut uncited = usage.clone();
    uncited.route = Some("anthropic/eu".into());
    store
        .apply(&Batch {
            ops: vec![agent(
                "opus-eu",
                1,
                SEP_01 + 1,
                "claude-opus-4-8",
                uncited.clone(),
            )],
            ..Batch::default()
        })
        .unwrap();
    assert_eq!(totals(&store).cost, Some(priced));
    assert_eq!(totals(&store).unpriced, uncited.total_tokens().unwrap());
}

#[test]
fn a_re_emitted_row_without_usage_keeps_the_usage_it_already_has() {
    // Claude Code streams a response's usage as a `Usage` op behind its row;
    // a replayed record re-emits the row with no usage of its own.
    let mut store = Store::open_in_memory().unwrap();
    let base = vec![
        IngestOp::Runtime(runtime()),
        IngestOp::Project(ProjectRow {
            id: "p1".into(),
            name: "rosetta".into(),
            root_paths: vec!["/w/rosetta".into()],
            git_repo: None,
        }),
        IngestOp::SessionMeta(session()),
        agent(
            "opus",
            0,
            SEP_01,
            "claude-opus-4-8",
            TokenSnapshot::default(),
        ),
        usage_with_identity("s1", "opus", SEP_01, 300),
    ];
    store
        .apply(&Batch {
            ops: base.clone(),
            ..Batch::default()
        })
        .unwrap();
    assert_eq!(row_usage(&store, "opus"), (Some(300), Some(10)));
    // The same row again, without usage: nothing is wiped.
    store
        .apply(&Batch {
            ops: vec![agent(
                "opus",
                0,
                SEP_01,
                "claude-opus-4-8",
                TokenSnapshot::default(),
            )],
            ..Batch::default()
        })
        .unwrap();
    assert_eq!(row_usage(&store, "opus"), (Some(300), Some(10)));
    let one = expected_cost("claude-opus-4-8", SEP_01, UsageScope::Request, &io(300, 10)).unwrap();
    assert_eq!(totals(&store).cost, Some(one));
    // A row that does carry usage still replaces it.
    store
        .apply(&Batch {
            ops: vec![agent("opus", 0, SEP_01, "claude-opus-4-8", io(500, 20))],
            ..Batch::default()
        })
        .unwrap();
    assert_eq!(row_usage(&store, "opus"), (Some(500), Some(20)));
}
