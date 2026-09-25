//! The session store's `assistant_usage_events` table through the real
//! snapshot plan: a hermetic database built from the runtime's own DDL
//! (1.0.80, plus the 1.0.88 migration), filled with numbers only, snapshotted
//! by the capture layer and normalized as the pipeline would. A native
//! capture of the store is pending (the farm strips it at harvest); the DDL
//! is quoted from `prebuilds/darwin-arm64/runtime.node` of each version, and
//! the 1.0.80 rows carry one real session's numbers, whose five rows sum to
//! its shutdown reading exactly. Regenerate the goldens with
//! `UPDATE_GOLDEN=1 cargo test -p rosetta-adapter-copilot --test store_usage`.

use std::path::{Path, PathBuf};

use rosetta_adapter_copilot::{CopilotAdapter, spec};
use rosetta_adapter_support::adapter::{AgentAdapter, LineStatus, NormalizeCtx};
use rosetta_capture::{SnapshotOutcome, snapshot_db};
use rosetta_core::rows::{DriftFact, ServiceTier, SessionUsageRow, UsageScope};
use rosetta_core::{IngestOp, ids};

const SESSION: &str = "11111111-2222-4333-8444-555555555555";
const BYOK_SESSION: &str = "66666666-7777-4888-9999-000000000000";

/// runtime.node 1.0.80 (`schema_version` 6), structure only.
const DDL_1_0_80: &str = "
CREATE TABLE schema_version (version INTEGER NOT NULL);
CREATE TABLE sessions (
    id TEXT PRIMARY KEY,
    cwd TEXT,
    repository TEXT,
    host_type TEXT,
    branch TEXT,
    summary TEXT,
    created_at TEXT DEFAULT (datetime('now')),
    updated_at TEXT DEFAULT (datetime('now'))
);
CREATE TABLE assistant_usage_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL REFERENCES sessions(id),
    turn_index INTEGER,
    agent_id TEXT,
    parent_tool_call_id TEXT,
    model TEXT NOT NULL,
    input_tokens INTEGER,
    output_tokens INTEGER,
    cache_read_tokens INTEGER,
    cache_write_tokens INTEGER,
    reasoning_tokens INTEGER,
    total_nano_aiu INTEGER,
    request_multiplier REAL,
    duration_ms INTEGER,
    time_to_first_token_ms INTEGER,
    inter_token_latency_ms INTEGER,
    initiator TEXT,
    api_endpoint TEXT,
    reasoning_effort TEXT,
    finish_reason TEXT,
    content_filter_triggered INTEGER,
    token_details_json TEXT,
    created_at TEXT DEFAULT (datetime('now'))
);
CREATE INDEX idx_assistant_usage_events_session ON assistant_usage_events(session_id, id);
INSERT INTO schema_version (version) VALUES (6);
";

/// runtime.node 1.0.88: two `ALTER TABLE … ADD COLUMN` migrations.
const MIGRATION_1_0_88: &str = "
ALTER TABLE assistant_usage_events ADD COLUMN output_ttft_ms REAL;
ALTER TABLE assistant_usage_events ADD COLUMN copilot_usage_model TEXT;
";

/// A real 1.0.80 session's five gpt-5.6-terra calls, numbers only.
struct RealRow {
    id: i64,
    input: i64,
    output: i64,
    cache_read: i64,
    cache_write: i64,
    reasoning: i64,
    nano_aiu: i64,
    created_at: &'static str,
}

const REAL_ROWS: [RealRow; 5] = [
    RealRow {
        id: 1,
        input: 19_918,
        output: 305,
        cache_read: 0,
        cache_write: 19_915,
        reasoning: 51,
        nano_aiu: 5_345_350_000,
        created_at: "2026-08-27T01:28:08.091Z",
    },
    RealRow {
        id: 2,
        input: 20_993,
        output: 177,
        cache_read: 19_850,
        cache_write: 1_140,
        reasoning: 28,
        nano_aiu: 895_000_000,
        created_at: "2026-08-27T01:31:57.018Z",
    },
    RealRow {
        id: 3,
        input: 22_713,
        output: 393,
        cache_read: 20_990,
        cache_write: 1_720,
        reasoning: 59,
        nano_aiu: 1_322_000_000,
        created_at: "2026-08-27T01:32:07.532Z",
    },
    RealRow {
        id: 4,
        input: 35_317,
        output: 288,
        cache_read: 0,
        cache_write: 35_314,
        reasoning: 102,
        nano_aiu: 9_174_700_000,
        created_at: "2026-08-27T02:48:15.244Z",
    },
    RealRow {
        id: 5,
        input: 37_126,
        output: 631,
        cache_read: 35_314,
        cache_write: 1_809,
        reasoning: 122,
        nano_aiu: 1_916_330_000,
        created_at: "2026-08-27T03:05:22.547Z",
    },
];

struct Row {
    id: i64,
    session: &'static str,
    model: &'static str,
    billing_model: Option<&'static str>,
    input: i64,
    output: i64,
    cache_read: i64,
    cache_write: i64,
    reasoning: Option<i64>,
    nano_aiu: Option<i64>,
    details: Option<String>,
    created_at: Option<&'static str>,
}

impl Row {
    fn metered(id: i64, session: &'static str, model: &'static str, nano_aiu: i64) -> Self {
        Self {
            id,
            session,
            model,
            billing_model: None,
            input: 1_000,
            output: 100,
            cache_read: 600,
            cache_write: 300,
            reasoning: Some(40),
            nano_aiu: Some(nano_aiu),
            details: None,
            created_at: Some("2026-08-27T01:28:08.091Z"),
        }
    }
}

/// CAPI's line items as 1.0.80 stores them (gpt-5.6-terra rates).
fn line_items(input: i64, cache_read: i64, cache_write: i64, output: i64) -> String {
    format!(
        concat!(
            "[{{\"batchSize\":1000000,\"costPerBatch\":200000000000,\"tokenCount\":{},\"tokenType\":\"input\"}},",
            "{{\"batchSize\":1000000,\"costPerBatch\":20000000000,\"tokenCount\":{},\"tokenType\":\"cache_read\"}},",
            "{{\"batchSize\":1000000,\"costPerBatch\":250000000000,\"tokenCount\":{},\"tokenType\":\"cache_write\"}},",
            "{{\"batchSize\":1000000,\"costPerBatch\":1200000000000,\"tokenCount\":{},\"tokenType\":\"output\"}}]"
        ),
        input, cache_read, cache_write, output
    )
}

fn real_rows() -> Vec<Row> {
    REAL_ROWS
        .iter()
        .map(|row| Row {
            id: row.id,
            session: SESSION,
            model: "gpt-5.6-terra",
            billing_model: None,
            input: row.input,
            output: row.output,
            cache_read: row.cache_read,
            cache_write: row.cache_write,
            reasoning: Some(row.reasoning),
            nano_aiu: Some(row.nano_aiu),
            details: Some(line_items(
                row.input - row.cache_read - row.cache_write,
                row.cache_read,
                row.cache_write,
                row.output,
            )),
            created_at: Some(row.created_at),
        })
        .collect()
}

struct StoreDb {
    _dir: tempfile::TempDir,
    path: PathBuf,
}

fn store(migrated: bool, rows: &[Row]) -> StoreDb {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("session-store.db");
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection.execute_batch(DDL_1_0_80).unwrap();
    if migrated {
        connection.execute_batch(MIGRATION_1_0_88).unwrap();
        connection
            .execute("UPDATE schema_version SET version = 7", [])
            .unwrap();
    }
    for session in [SESSION, BYOK_SESSION] {
        connection
            .execute("INSERT INTO sessions (id) VALUES (?1)", [session])
            .unwrap();
    }
    for row in rows {
        let mut columns = vec![
            "id",
            "session_id",
            "model",
            "input_tokens",
            "output_tokens",
            "cache_read_tokens",
            "cache_write_tokens",
            "reasoning_tokens",
            "total_nano_aiu",
            "request_multiplier",
            "api_endpoint",
            "finish_reason",
            "token_details_json",
            "created_at",
        ];
        let mut values: Vec<Box<dyn rusqlite::ToSql>> = vec![
            Box::new(row.id),
            Box::new(row.session),
            Box::new(row.model),
            Box::new(row.input),
            Box::new(row.output),
            Box::new(row.cache_read),
            Box::new(row.cache_write),
            Box::new(row.reasoning),
            Box::new(row.nano_aiu),
            Box::new(1.0_f64),
            Box::new("ws:/responses"),
            Box::new("stop"),
            Box::new(row.details.clone()),
            Box::new(row.created_at),
        ];
        if let Some(billing_model) = row.billing_model {
            assert!(migrated, "copilot_usage_model exists from 1.0.88");
            columns.push("copilot_usage_model");
            values.push(Box::new(billing_model));
        }
        let placeholders = (1..=columns.len())
            .map(|index| format!("?{index}"))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "INSERT INTO assistant_usage_events ({}) VALUES ({placeholders})",
            columns.join(", ")
        );
        let params: Vec<&dyn rusqlite::ToSql> = values.iter().map(|value| value.as_ref()).collect();
        connection.execute(&sql, params.as_slice()).unwrap();
    }
    drop(connection);
    StoreDb { _dir: dir, path }
}

fn sessions(db: &Path) -> Vec<serde_json::Value> {
    let plan = CopilotAdapter
        .snapshot_plan(spec::SESSION_STORE)
        .expect("store plan");
    let SnapshotOutcome::Ready(snapshot) = snapshot_db(db).unwrap() else {
        panic!("{} did not produce a stable snapshot", db.display());
    };
    snapshot
        .query_rows(&plan.sessions_sql, None)
        .unwrap()
        .iter()
        .map(|row| serde_json::from_str(row).unwrap())
        .collect()
}

fn normalize(db: &Path, native: &str) -> Vec<IngestOp> {
    let adapter = CopilotAdapter;
    let plan = adapter
        .snapshot_plan(spec::SESSION_STORE)
        .expect("store plan");
    let SnapshotOutcome::Ready(snapshot) = snapshot_db(db).unwrap() else {
        panic!("{} did not produce a stable snapshot", db.display());
    };
    let runtime_id = ids::runtime_id(adapter.kind(), "~/.copilot");
    let session_id = ids::session_id(&runtime_id, native);
    let mut ctx = NormalizeCtx::new(runtime_id, session_id);
    ctx.native_session_id = Some(native.to_owned());
    ctx.src_cursor = Some(3);

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
            let outcome =
                adapter.ingest_line(&mut ctx, spec::SESSION_STORE, record.to_string().as_bytes());
            assert_eq!(outcome.status, LineStatus::Parsed, "{}: {row}", query.name);
            ops.extend(outcome.ops);
        }
    }
    ops
}

fn samples(ops: &[IngestOp]) -> Vec<&SessionUsageRow> {
    ops.iter()
        .filter_map(|op| match op {
            IngestOp::SessionUsage(row) => Some(row),
            _ => None,
        })
        .collect()
}

fn drift_codes(ops: &[IngestOp]) -> Vec<&str> {
    ops.iter()
        .filter_map(|op| match op {
            IngestOp::Drift(DriftFact { error_code, .. }) => Some(error_code.as_str()),
            _ => None,
        })
        .collect()
}

fn check_golden(name: &str, ops: &[IngestOp]) {
    let golden =
        Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("tests/golden/store/{name}.json"));
    let rendered = format!("{}\n", serde_json::to_string_pretty(ops).unwrap());
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::create_dir_all(golden.parent().unwrap()).unwrap();
        std::fs::write(&golden, &rendered).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&golden).unwrap_or_else(|error| {
        panic!(
            "{}: {error} — regenerate with UPDATE_GOLDEN=1 cargo test -p rosetta-adapter-copilot",
            golden.display()
        )
    });
    assert_eq!(
        rendered, expected,
        "store ops changed for {name}; review then bless"
    );
}

#[test]
fn metered_sessions_are_listed_with_their_request_span() {
    let mut rows = real_rows();
    rows.push(Row::metered(6, BYOK_SESSION, "mock-model", 0));
    let db = store(false, &rows);
    let listed = sessions(&db.path);
    assert_eq!(
        listed.len(),
        1,
        "an unmetered session is not a store session"
    );
    assert_eq!(listed[0]["native_session_id"], SESSION);
    let skim = CopilotAdapter.snapshot_session(spec::SESSION_STORE, &listed[0]);
    assert_eq!(skim.session_id.as_deref(), Some(SESSION));
    assert_eq!(skim.started_at, Some(1_787_794_088_091));
    assert_eq!(skim.updated_at, Some(1_787_799_922_547));
    assert!(skim.cwd.is_none() && skim.title.is_none() && skim.role.is_none());
}

#[test]
fn per_call_rows_are_exact_request_samples_that_sum_to_the_shutdown() {
    let db = store(false, &real_rows());
    let ops = normalize(&db.path, SESSION);
    assert!(drift_codes(&ops).is_empty(), "{:?}", drift_codes(&ops));
    let rows = samples(&ops);
    assert_eq!(rows.len(), 5);

    let first = rows[0];
    assert_eq!(first.model.as_deref(), Some("gpt-5.6-terra"));
    assert_eq!(first.ts, 1_787_794_088_091);
    // Inclusive input 19,918 = 3 uncached + 0 read + 19,915 written.
    assert_eq!(first.tokens.input, Some(3));
    assert_eq!(first.tokens.cache_read, Some(0));
    assert_eq!(first.tokens.cache_write, Some(19_915));
    assert_eq!(first.tokens.cache_write_1h, None);
    // Inclusive output 305 = 254 + 51 reasoning.
    assert_eq!(
        (first.tokens.output, first.tokens.thinking),
        (Some(254), Some(51))
    );
    assert_eq!(first.tokens.billed_pico_usd, Some(53_453_500_000));
    assert_eq!(
        (first.tokens.scope, first.tokens.requests),
        (UsageScope::Request, None)
    );
    assert_eq!(first.tokens.service_tier, None);

    let sum = |field: fn(&SessionUsageRow) -> Option<i64>| {
        rows.iter().map(|row| field(row).unwrap()).sum::<i64>()
    };
    // The session's shutdown reported 136,067 / 1,794 / 76,154 / 59,898 / 362
    // over 5 requests and 18,653,380,000 nano-AIU: the rows are that reading.
    assert_eq!(sum(|row| row.tokens.input), 15);
    assert_eq!(sum(|row| row.tokens.cache_read), 76_154);
    assert_eq!(sum(|row| row.tokens.cache_write), 59_898);
    assert_eq!(sum(|row| row.tokens.output), 1_432);
    assert_eq!(sum(|row| row.tokens.thinking), 362);
    assert_eq!(sum(|row| row.tokens.billed_pico_usd), 186_533_800_000);
    assert!(
        rows.iter()
            .all(|row| row.tokens.scope == UsageScope::Request)
    );
    assert!(rows.windows(2).all(|pair| pair[0].ts < pair[1].ts));
    let mut keys: Vec<_> = rows.iter().map(|row| row.key.as_str()).collect();
    keys.sort_unstable();
    keys.dedup();
    assert_eq!(keys.len(), 5, "one replacement key per call");
    check_golden("1.0.80", &ops);
}

#[test]
fn unmetered_rows_stay_out_of_canonical_usage() {
    let mut rows = real_rows();
    rows.push(Row::metered(6, BYOK_SESSION, "mock-model", 0));
    rows.push(Row {
        nano_aiu: None,
        ..Row::metered(7, BYOK_SESSION, "mock-model", 0)
    });
    let db = store(false, &rows);
    let ops = normalize(&db.path, BYOK_SESSION);
    assert!(
        samples(&ops).is_empty(),
        "BYOK calls are accounted from the shutdown"
    );
    assert!(drift_codes(&ops).is_empty());
}

#[test]
fn one_hour_writes_split_and_model_spellings_resolve() {
    let rows = vec![
        Row {
            input: 2_200,
            cache_write: 1_500,
            details: Some(
                r#"[{"tokenType":"cache_write","tokenCount":1000},{"tokenType":"cache_write_1h","tokenCount":500},{"tokenType":"output","tokenCount":100}]"#
                    .to_owned(),
            ),
            ..Row::metered(1, SESSION, "claude-opus-5.5-1m", 7_000_000)
        },
        Row {
            billing_model: Some("claude-opus-4.8-fast"),
            ..Row::metered(2, SESSION, "claude-opus-4.8", 8_000_000)
        },
        Row {
            details: Some(
                r#"[{"tokenType":"output","tokenCount":60,"model":"gpt-5.6-terra"},{"tokenType":"output","tokenCount":40,"model":"gpt-5.6-luna"}]"#
                    .to_owned(),
            ),
            ..Row::metered(3, SESSION, "gpt-5.6-terra", 9_000_000)
        },
        Row {
            created_at: Some("2026-08-27 01:28:08"),
            ..Row::metered(4, SESSION, "gpt-5.6-terra", 10_000_000)
        },
    ];
    let db = store(true, &rows);
    let ops = normalize(&db.path, SESSION);
    let rows = samples(&ops);
    assert_eq!(rows.len(), 4);

    let long_context = rows[0];
    assert_eq!(long_context.model.as_deref(), Some("claude-opus-5.5[1m]"));
    assert_eq!(long_context.tokens.service_tier, None);
    assert_eq!(long_context.tokens.cache_write, Some(1_000));
    assert_eq!(long_context.tokens.cache_write_1h, Some(500));
    // Inclusive input 2,200 = 100 uncached + 600 read + 1,500 written (1,000 + 500).
    assert_eq!(long_context.tokens.input, Some(100));

    let fast = rows[1];
    assert_eq!(fast.model.as_deref(), Some("claude-opus-4.8"));
    assert_eq!(fast.tokens.service_tier, Some(ServiceTier::Fast));
    assert_eq!(fast.tokens.input, Some(100));
    assert_eq!(fast.tokens.cache_write_1h, None);

    let mixed = rows[2];
    assert_eq!(mixed.model.as_deref(), Some("gpt-5.6-terra"));
    assert_eq!(mixed.tokens.output, Some(60));

    let sqlite_dated = rows[3];
    assert_eq!(sqlite_dated.ts, 1_787_794_088_000);

    assert_eq!(drift_codes(&ops), ["usage_mixed_models"]);
    check_golden("1.0.88", &ops);
}

#[test]
fn malformed_rows_are_reported_not_guessed() {
    let rows = vec![
        // Reads larger than the input that contains them: clamped and flagged.
        Row {
            cache_read: 5_000,
            ..Row::metered(1, SESSION, "gpt-5.6-terra", 1_000)
        },
        Row {
            details: Some("not json".to_owned()),
            ..Row::metered(2, SESSION, "gpt-5.6-terra", 1_000)
        },
        Row {
            created_at: None,
            ..Row::metered(3, SESSION, "gpt-5.6-terra", 1_000)
        },
    ];
    let db = store(false, &rows);
    let ops = normalize(&db.path, SESSION);
    let rows = samples(&ops);
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].tokens.input, Some(0));
    assert_eq!(rows[0].tokens.cache_read, Some(5_000));
    assert_eq!(rows[1].tokens.cache_write_1h, None);
    assert_eq!(rows[2].ts, 0);
    assert_eq!(
        drift_codes(&ops),
        [
            "usage_subset_exceeds_total",
            "parse_failed",
            "usage_undated"
        ]
    );
}
