//! Shared helpers for the conversation-store tests: run the adapter's declared
//! snapshot plan against a committed fixture database and frame every result
//! row exactly as the capture layer does (`{"query": name, "row": {...}}`,
//! blobs as `{"$base64": "..."}`, keys sorted). The fixture is copied first so
//! SQLite's `-shm` bookkeeping never touches the committed corpus.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use rosetta_adapter_antigravity::raw::base64;
use rosetta_adapter_support::adapter::SnapshotPlan;
use rusqlite::types::ValueRef;
use serde_json::{Map, Value};

pub const DATA_ROOT: &str = "~/.gemini/antigravity-cli";

pub fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../groundtruth/antigravity/fixtures")
}

/// Every committed conversation store as `(version, scenario, path)`.
pub fn conversation_dbs() -> Vec<(String, String, PathBuf)> {
    let root = fixtures_root();
    let mut found: Vec<_> = walkdir::WalkDir::new(&root)
        .sort_by_file_name()
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| entry.into_path())
        .filter(|path| {
            path.extension().is_some_and(|ext| ext == "db")
                && path
                    .parent()
                    .is_some_and(|dir| dir.file_name().is_some_and(|name| name == "conversations"))
        })
        .map(|path| {
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

pub struct FramedStore {
    pub native_session_id: String,
    /// `(query name, framed record bytes)` in plan order.
    pub records: Vec<(String, Vec<u8>)>,
}

/// Snapshot one store through the plan. Panics unless the store names exactly
/// one cascade, which every committed fixture does.
pub fn frame(db: &Path, plan: &SnapshotPlan) -> FramedStore {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let scratch = std::env::temp_dir().join(format!(
        "rosetta-antigravity-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&scratch).unwrap();
    let copy = scratch.join("conversation.db");
    std::fs::copy(db, &copy).unwrap();
    let wal = PathBuf::from(format!("{}-wal", db.display()));
    if wal.is_file() {
        std::fs::copy(&wal, scratch.join("conversation.db-wal")).unwrap();
    }

    let framed = {
        let connection = rusqlite::Connection::open(&copy).unwrap();
        let sessions = rows(&connection, &plan.sessions_sql, None);
        assert_eq!(sessions.len(), 1, "{}: one cascade per store", db.display());
        let native_session_id = sessions[0]["native_session_id"]
            .as_str()
            .expect("cascade id")
            .to_owned();
        let mut records = Vec::new();
        for query in &plan.record_queries {
            for row in rows(&connection, &query.sql, Some(&native_session_id)) {
                let record = format!(
                    "{{\"query\":{},\"row\":{}}}",
                    Value::String(query.name.clone()),
                    Value::Object(row)
                );
                records.push((query.name.clone(), record.into_bytes()));
            }
        }
        FramedStore {
            native_session_id,
            records,
        }
    };
    let _ = std::fs::remove_dir_all(&scratch);
    framed
}

fn rows(
    connection: &rusqlite::Connection,
    sql: &str,
    bind: Option<&str>,
) -> Vec<Map<String, Value>> {
    let mut statement = connection.prepare(sql).unwrap();
    let names: Vec<String> = statement
        .column_names()
        .iter()
        .map(|name| (*name).to_owned())
        .collect();
    let mut result = match bind {
        Some(value) => statement.query(rusqlite::params![value]).unwrap(),
        None => statement.query([]).unwrap(),
    };
    let mut out = Vec::new();
    while let Some(row) = result.next().unwrap() {
        let mut object = Map::new();
        for (index, name) in names.iter().enumerate() {
            let value = match row.get_ref(index).unwrap() {
                ValueRef::Null => Value::Null,
                ValueRef::Integer(value) => Value::from(value),
                ValueRef::Real(value) => Value::from(value),
                ValueRef::Text(text) => Value::from(String::from_utf8_lossy(text).into_owned()),
                ValueRef::Blob(bytes) => {
                    let mut blob = Map::new();
                    blob.insert("$base64".to_owned(), Value::from(base64::encode(bytes)));
                    Value::Object(blob)
                }
            };
            object.insert(name.clone(), value);
        }
        out.push(object);
    }
    out
}
