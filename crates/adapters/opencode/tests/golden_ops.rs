//! Golden normalization of OpenCode's SQLite snapshot rows.
//!
//! Regenerate with:
//! `UPDATE_GOLDEN=1 cargo test -p rosetta-adapter-opencode --test golden_ops`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use rosetta_adapter_opencode::{OpenCodeAdapter, spec};
use rosetta_adapter_support::{AgentAdapter, LineStatus, NormalizeCtx};
use rosetta_core::{IngestOp, ids};
use rusqlite::types::Value as SqlValue;
use rusqlite::{Connection, OpenFlags};

const DATA_ROOT: &str = "~/.local/share/opencode";

fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../groundtruth/opencode/fixtures")
}

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

fn scenarios() -> Vec<(String, String, PathBuf)> {
    let root = fixtures_root();
    let mut found: Vec<_> = walkdir::WalkDir::new(&root)
        .sort_by_file_name()
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file() && entry.file_name() == "opencode.db")
        .map(|entry| {
            let path = entry.into_path();
            let relative = path.strip_prefix(&root).unwrap();
            let mut parts = relative.components();
            let version = parts.next().expect("version directory").as_os_str();
            let scenario = parts.next().expect("scenario directory").as_os_str();
            (
                version.to_string_lossy().into_owned(),
                scenario.to_string_lossy().into_owned(),
                path,
            )
        })
        .collect();
    found.sort();
    found
}

fn staging_copy(path: &Path) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "rosetta-opencode-golden-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let dest = dir.join("opencode.db");
    std::fs::copy(path, &dest).unwrap();
    let base = path.to_string_lossy();
    for suffix in ["-wal", "-shm"] {
        let src = PathBuf::from(format!("{base}{suffix}"));
        if src.is_file() {
            std::fs::copy(&src, dir.join(format!("opencode.db{suffix}"))).unwrap();
        }
    }
    dest
}

fn open_fixture(path: &Path) -> Connection {
    Connection::open_with_flags(staging_copy(path), OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap()
}

fn sql_value(value: SqlValue) -> serde_json::Value {
    match value {
        SqlValue::Null => serde_json::Value::Null,
        SqlValue::Integer(value) => value.into(),
        SqlValue::Real(value) => serde_json::Value::from(value),
        SqlValue::Text(value) => serde_json::Value::String(value),
        SqlValue::Blob(_) => panic!("snapshot plan unexpectedly selected a blob"),
    }
}

fn query_rows(connection: &Connection, sql: &str, bind: Option<&str>) -> Vec<String> {
    let mut statement = connection.prepare(sql).unwrap();
    let names: Vec<_> = statement
        .column_names()
        .iter()
        .map(|name| (*name).to_owned())
        .collect();
    let mut rows = match bind {
        Some(value) => statement.query([value]),
        None => statement.query([]),
    }
    .unwrap();
    let mut output = Vec::new();
    while let Some(row) = rows.next().unwrap() {
        let mut object = serde_json::Map::new();
        for (index, name) in names.iter().enumerate() {
            object.insert(name.clone(), sql_value(row.get(index).unwrap()));
        }
        output.push(serde_json::Value::Object(object).to_string());
    }
    output
}

fn records(path: &Path) -> (String, String, Vec<Vec<u8>>) {
    let connection = open_fixture(path);
    let plan = OpenCodeAdapter
        .snapshot_plan(spec::DATABASE)
        .expect("snapshot plan");
    let sessions = query_rows(&connection, &plan.sessions_sql, None);
    assert_eq!(sessions.len(), 1, "fixture scenarios contain one session");
    let session: serde_json::Value = serde_json::from_str(&sessions[0]).unwrap();
    let native = session["native_session_id"].as_str().unwrap().to_owned();
    let version = session["agent_version"].as_str().unwrap().to_owned();

    let mut records = Vec::new();
    for query in &plan.record_queries {
        for row in query_rows(&connection, &query.sql, Some(&native)) {
            records.push(
                serde_json::to_vec(&serde_json::json!({
                    "query": query.name,
                    "row": serde_json::from_str::<serde_json::Value>(&row).unwrap()
                }))
                .unwrap(),
            );
        }
    }
    (native, version, records)
}

fn ingest_records(
    ctx: &mut NormalizeCtx,
    records: &[Vec<u8>],
    range: std::ops::Range<usize>,
) -> (Vec<IngestOp>, Vec<String>) {
    let mut ops = Vec::new();
    let mut unhealthy = Vec::new();
    for index in range {
        ctx.src_cursor = Some(1);
        ctx.src_offset = Some(index as i64);
        let outcome = OpenCodeAdapter.ingest_line(ctx, spec::DATABASE, &records[index]);
        if outcome.status != LineStatus::Parsed {
            unhealthy.push(format!("record {}: {:?}", index + 1, outcome.status));
        }
        ops.extend(outcome.ops);
    }
    (ops, unhealthy)
}

fn normalize(path: &Path) -> (Vec<IngestOp>, Vec<String>) {
    let (native, version, records) = records(path);
    let runtime = ids::runtime_id("opencode", DATA_ROOT);
    let session = ids::session_id(&runtime, &native);
    let mut ctx = NormalizeCtx::new(runtime, session);
    ctx.agent_version = Some(version);
    ctx.state = OpenCodeAdapter.init_state();
    ingest_records(&mut ctx, &records, 0..records.len())
}

#[test]
fn corpus_exists() {
    assert!(!scenarios().is_empty());
}

#[test]
fn snapshot_queries_and_normalization_match_goldens() {
    let update = std::env::var_os("UPDATE_GOLDEN").is_some();
    for (version, scenario, path) in scenarios() {
        let (ops, unhealthy) = normalize(&path);
        assert!(
            unhealthy.is_empty(),
            "{version}/{scenario} did not fully normalize:\n{}",
            unhealthy.join("\n")
        );
        assert!(!ops.is_empty(), "{version}/{scenario} produced no ops");
        assert!(
            !ops.iter().any(|op| matches!(op, IngestOp::Drift(_))),
            "{version}/{scenario} produced drift: {ops:#?}"
        );

        let golden = golden_dir().join(&version).join(format!("{scenario}.json"));
        let rendered = format!("{}\n", serde_json::to_string_pretty(&ops).unwrap());
        if update {
            std::fs::create_dir_all(golden.parent().unwrap()).unwrap();
            std::fs::write(&golden, &rendered).unwrap();
            continue;
        }
        let expected = std::fs::read_to_string(&golden).unwrap_or_else(|error| {
            panic!(
                "{}: {error} — regenerate with UPDATE_GOLDEN=1 cargo test \
                 -p rosetta-adapter-opencode --test golden_ops",
                golden.display()
            )
        });
        assert_eq!(rendered, expected, "changed {version}/{scenario}");
    }
}

#[test]
fn replay_and_checkpoint_restore_are_deterministic() {
    let adapter = OpenCodeAdapter;
    for (version, scenario, path) in scenarios() {
        assert_eq!(normalize(&path).0, normalize(&path).0);

        let one_shot = normalize(&path).0;
        let (native, agent_version, records) = records(&path);
        let runtime = ids::runtime_id("opencode", DATA_ROOT);
        let session = ids::session_id(&runtime, &native);
        for split in 0..=records.len() {
            let mut before = NormalizeCtx::new(runtime.clone(), session.clone());
            before.agent_version = Some(agent_version.clone());
            before.state = adapter.init_state();
            let (mut split_ops, unhealthy) = ingest_records(&mut before, &records, 0..split);
            assert!(unhealthy.is_empty());

            let checkpoint = adapter
                .checkpoint_state(before.state.as_deref())
                .unwrap()
                .expect("adapter state");
            let mut after = NormalizeCtx::new(runtime.clone(), session.clone());
            after.agent_version = Some(agent_version.clone());
            after.next_ordinal = before.next_ordinal;
            after.state = adapter.restore_state(Some(&checkpoint)).unwrap();
            let (tail_ops, unhealthy) = ingest_records(&mut after, &records, split..records.len());
            assert!(unhealthy.is_empty());
            split_ops.extend(tail_ops);
            assert_eq!(
                split_ops, one_shot,
                "{version}/{scenario} differed at split {split}"
            );
        }
    }
}

#[test]
fn goldens_carry_no_personal_paths() {
    for entry in walkdir::WalkDir::new(golden_dir())
        .sort_by_file_name()
        .into_iter()
        .filter_map(Result::ok)
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let text = std::fs::read_to_string(entry.path()).unwrap();
        for needle in [concat!("james", "yong"), concat!("/", "Users/")] {
            assert!(
                !text.contains(needle),
                "{} contains personal data ({needle})",
                entry.path().display()
            );
        }
    }
}
