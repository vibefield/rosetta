//! Stable snapshots of agent-owned SQLite databases — the source primitive
//! behind `StreamDriver::SqliteSnapshot` (opencode, hermes).
//!
//! A live database cannot be tailed or copied as unrelated `db`/`-wal` files.
//! SQLite's online backup API supplies the consistency boundary: it reads one
//! committed source snapshot into a private database while the agent may keep
//! writing. Agent crates never see the connection and the source is read-only.
//!
//! Queries are DECLARED by adapters and EXECUTED here: agent crates never
//! see a database handle, and every result row is serialized to one dense
//! JSON object — a record the ordinary `ingest_line` path (and therefore
//! the ledger and rebuild) consumes with no SQLite involvement at all.

use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::OpenFlags;
use rusqlite::backup::{Backup, StepResult};
use rusqlite::types::ValueRef;
use serde_json::{Map, Value};

use crate::{CaptureError, io_error};

/// Diagnostic metadata for a database file set (db + -wal). It is retained in
/// checkpoints for observability only; equality is never treated as content
/// identity or as proof that a snapshot is stable.
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

/// The snapshot attempt's outcome. `Unstable` means SQLite could not obtain
/// backup progress within a bounded number of lock retries.
pub enum SnapshotOutcome {
    Missing,
    Unstable,
    Ready(Box<DbSnapshot>),
}

pub fn snapshot(db_path: &Path) -> Result<SnapshotOutcome, CaptureError> {
    let Some(stamp) = db_stamp(db_path)? else {
        return Ok(SnapshotOutcome::Missing);
    };
    let dir = tempfile::TempDir::new()
        .map_err(|error| io_error("creating snapshot dir for", db_path, error))?;
    let copy = dir.path().join("snapshot.db");
    let source = match rusqlite::Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) {
        Ok(connection) => connection,
        Err(_) if !db_path.exists() => return Ok(SnapshotOutcome::Missing),
        Err(error) => {
            return Err(CaptureError::InvalidConfig(format!(
                "opening source snapshot {}: {error}",
                db_path.display()
            )));
        }
    };
    source
        .busy_timeout(Duration::from_secs(2))
        .map_err(|error| CaptureError::InvalidConfig(format!("configuring snapshot: {error}")))?;
    let mut conn = rusqlite::Connection::open(&copy)
        .map_err(|error| CaptureError::InvalidConfig(format!("opening snapshot: {error}")))?;
    {
        let backup = Backup::new(&source, &mut conn)
            .map_err(|error| CaptureError::InvalidConfig(format!("starting backup: {error}")))?;
        let mut stalled = 0_u32;
        loop {
            match backup
                .step(256)
                .map_err(|error| CaptureError::InvalidConfig(format!("backing up: {error}")))?
            {
                StepResult::Done => break,
                StepResult::More => stalled = 0,
                StepResult::Busy | StepResult::Locked => {
                    stalled += 1;
                    if stalled > 400 {
                        return Ok(SnapshotOutcome::Unstable);
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                _ => return Ok(SnapshotOutcome::Unstable),
            }
        }
    }
    Ok(SnapshotOutcome::Ready(Box::new(DbSnapshot {
        _dir: dir,
        conn,
        stamp,
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
