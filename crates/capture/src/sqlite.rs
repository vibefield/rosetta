//! Stable snapshots of agent-owned SQLite databases — the source primitive
//! behind `StreamDriver::SqliteSnapshot` (opencode, hermes).
//!
//! A live database cannot be tailed and must never be opened in place: even
//! a read-only open takes locks in the agent's directory and needs the
//! `-shm` file. Instead: stamp the file set, COPY `db` + `-wal` to a private
//! temp dir, re-stamp, and only trust the copy when both stamps agree
//! (spaghetti's stable-read discipline). The copy is ours, so opening it
//! read-write and letting SQLite recover the WAL is safe and yields the
//! writer's latest committed state.
//!
//! Queries are DECLARED by adapters and EXECUTED here: agent crates never
//! see a database handle, and every result row is serialized to one dense
//! JSON object — a record the ordinary `ingest_line` path (and therefore
//! the ledger and rebuild) consumes with no SQLite involvement at all.

use std::path::{Path, PathBuf};

use rusqlite::types::ValueRef;
use serde_json::{Map, Value};

use crate::{CaptureError, io_error};

/// Cheap change signature for a database file set (db + -wal), from
/// metadata only. Equal stamps mean "no need to copy anything".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbStamp(String);

impl DbStamp {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn from_encoded(text: &str) -> Self {
        Self(text.to_string())
    }
}

/// Stat the database file set. `None` if the main db file is missing.
pub fn db_stamp(db_path: &Path) -> Result<Option<DbStamp>, CaptureError> {
    let mut parts = Vec::new();
    for path in [db_path.to_path_buf(), wal_path(db_path)] {
        match std::fs::metadata(&path) {
            Ok(metadata) => {
                parts.push(format!(
                    "{}:{}",
                    metadata.len(),
                    crate::discovery::modified_ms(&metadata)
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if path == db_path {
                    return Ok(None);
                }
                parts.push("absent".to_string());
            }
            Err(error) => return Err(io_error("stating", &path, error)),
        }
    }
    Ok(Some(DbStamp(parts.join("|"))))
}

fn wal_path(db_path: &Path) -> PathBuf {
    let mut name = db_path.as_os_str().to_owned();
    name.push("-wal");
    PathBuf::from(name)
}

/// Is this walked path a database sidecar (`-wal`, `-shm`, `-journal`)
/// rather than a database itself?
pub fn is_db_sidecar(relative_path: &str) -> bool {
    relative_path.ends_with("-wal")
        || relative_path.ends_with("-shm")
        || relative_path.ends_with("-journal")
}

/// A stable private copy of one database, queryable until dropped.
pub struct DbSnapshot {
    _dir: tempfile::TempDir,
    conn: rusqlite::Connection,
    stamp: DbStamp,
}

/// The snapshot attempt's outcome. `Unstable` means the writer raced the
/// copy — retry later, exactly like a torn tail read.
pub enum SnapshotOutcome {
    Missing,
    Unstable,
    Ready(Box<DbSnapshot>),
}

pub fn snapshot(db_path: &Path) -> Result<SnapshotOutcome, CaptureError> {
    let Some(before) = db_stamp(db_path)? else {
        return Ok(SnapshotOutcome::Missing);
    };
    let dir = tempfile::TempDir::new()
        .map_err(|error| io_error("creating snapshot dir for", db_path, error))?;
    let copy = dir.path().join("snapshot.db");
    if let Err(error) = std::fs::copy(db_path, &copy) {
        return if error.kind() == std::io::ErrorKind::NotFound {
            Ok(SnapshotOutcome::Missing)
        } else {
            Err(io_error("copying", db_path, error))
        };
    }
    match std::fs::copy(wal_path(db_path), wal_path(&copy)) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(io_error("copying wal for", db_path, error)),
    }
    // The writer may have raced the two copies; equal stamps prove it didn't.
    if db_stamp(db_path)? != Some(before.clone()) {
        return Ok(SnapshotOutcome::Unstable);
    }

    // Our private copy: read-write open lets SQLite recover the WAL into
    // the main file, which is exactly the committed state we want.
    let conn = rusqlite::Connection::open(&copy)
        .map_err(|error| CaptureError::InvalidConfig(format!("opening snapshot: {error}")))?;
    Ok(SnapshotOutcome::Ready(Box::new(DbSnapshot {
        _dir: dir,
        conn,
        stamp: before,
    })))
}

impl DbSnapshot {
    pub fn stamp(&self) -> &DbStamp {
        &self.stamp
    }

    /// Run one declared query with an optional single text binding, and
    /// serialize each result row to a dense JSON object keyed by column
    /// name. Blobs become `{"$base64": "..."}`; everything else maps to its
    /// natural JSON type. Serialization is deterministic: serde_json orders
    /// object keys, so identical rows always produce identical bytes — the
    /// property the per-session revision hash depends on.
    pub fn query_rows(&self, sql: &str, bind: Option<&str>) -> Result<Vec<String>, CaptureError> {
        let mut stmt = self
            .conn
            .prepare(sql)
            .map_err(|error| CaptureError::InvalidConfig(format!("preparing query: {error}")))?;
        let column_names: Vec<String> = stmt
            .column_names()
            .iter()
            .map(|name| name.to_string())
            .collect();
        let mut rows = match bind {
            Some(value) => stmt.query(rusqlite::params![value]),
            None => stmt.query([]),
        }
        .map_err(|error| CaptureError::InvalidConfig(format!("running query: {error}")))?;

        let mut out = Vec::new();
        while let Some(row) = rows
            .next()
            .map_err(|error| CaptureError::InvalidConfig(format!("reading row: {error}")))?
        {
            let mut object = Map::with_capacity(column_names.len());
            for (index, name) in column_names.iter().enumerate() {
                let value = match row.get_ref(index).map_err(|error| {
                    CaptureError::InvalidConfig(format!("reading column {name}: {error}"))
                })? {
                    ValueRef::Null => Value::Null,
                    ValueRef::Integer(value) => Value::from(value),
                    ValueRef::Real(value) => Value::from(value),
                    ValueRef::Text(text) => Value::from(String::from_utf8_lossy(text).into_owned()),
                    ValueRef::Blob(bytes) => {
                        let mut object = Map::with_capacity(1);
                        object.insert("$base64".to_string(), Value::from(base64(bytes)));
                        Value::Object(object)
                    }
                };
                object.insert(name.clone(), value);
            }
            out.push(Value::Object(object).to_string());
        }
        Ok(out)
    }
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for at in 0..4 {
            if at <= chunk.len() {
                out.push(ALPHABET[((n >> (18 - 6 * at)) & 0x3F) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    fn seed_db(path: &Path, wal: bool) -> rusqlite::Connection {
        let conn = rusqlite::Connection::open(path).unwrap();
        if wal {
            conn.pragma_update(None, "journal_mode", "WAL").unwrap();
        }
        conn.execute_batch(
            "CREATE TABLE msg (id INTEGER PRIMARY KEY, session TEXT NOT NULL, body TEXT,
                               data BLOB, score REAL);
             INSERT INTO msg VALUES (1, 's-a', 'hello snapshot', x'0102', 0.5);
             INSERT INTO msg VALUES (2, 's-b', NULL, NULL, NULL);",
        )
        .unwrap();
        conn
    }

    #[test]
    fn snapshot_serializes_rows_deterministically() {
        let dir = TempDir::new().unwrap();
        let db = dir.path().join("agent.db");
        drop(seed_db(&db, false));

        let SnapshotOutcome::Ready(snapshot) = snapshot(&db).unwrap() else {
            panic!("expected a ready snapshot");
        };
        let rows = snapshot
            .query_rows(
                "SELECT * FROM msg WHERE session = ?1 ORDER BY id",
                Some("s-a"),
            )
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0],
            r#"{"body":"hello snapshot","data":{"$base64":"AQI="},"id":1,"score":0.5,"session":"s-a"}"#
        );
        let all = snapshot
            .query_rows("SELECT id FROM msg ORDER BY id", None)
            .unwrap();
        assert_eq!(all, [r#"{"id":1}"#, r#"{"id":2}"#]);
    }

    #[test]
    fn wal_content_not_yet_checkpointed_is_visible() {
        let dir = TempDir::new().unwrap();
        let db = dir.path().join("agent.db");
        let conn = seed_db(&db, true);
        conn.execute(
            "INSERT INTO msg VALUES (3, 's-a', 'lives only in the wal', NULL, NULL)",
            [],
        )
        .unwrap();
        // Deliberately no checkpoint and the writer stays open.
        let SnapshotOutcome::Ready(snapshot) = snapshot(&db).unwrap() else {
            panic!("expected a ready snapshot");
        };
        let rows = snapshot
            .query_rows("SELECT COUNT(*) AS n FROM msg", None)
            .unwrap();
        assert_eq!(rows, [r#"{"n":3}"#]);
        drop(conn);
    }

    #[test]
    fn stamps_detect_change_and_missing_dbs_are_reported() {
        let dir = TempDir::new().unwrap();
        let db = dir.path().join("agent.db");
        assert!(matches!(snapshot(&db).unwrap(), SnapshotOutcome::Missing));
        assert!(db_stamp(&db).unwrap().is_none());

        let conn = seed_db(&db, false);
        let before = db_stamp(&db).unwrap().unwrap();
        assert_eq!(
            DbStamp::from_encoded(before.as_str()).as_str(),
            before.as_str()
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
        conn.execute("INSERT INTO msg VALUES (9, 's-c', 'x', NULL, NULL)", [])
            .unwrap();
        drop(conn);
        let after = db_stamp(&db).unwrap().unwrap();
        assert_ne!(before, after, "a write must change the stamp");
    }

    #[test]
    fn sidecars_are_recognized() {
        assert!(is_db_sidecar("state.db-wal"));
        assert!(is_db_sidecar("state.db-shm"));
        assert!(is_db_sidecar("old.db-journal"));
        assert!(!is_db_sidecar("state.db"));
        assert!(!is_db_sidecar("opencode.db"));
    }
}
