//! Staging a RAM projection on disk for atomic publication.

use std::path::Path;

use rusqlite::Connection;
use rusqlite::backup::{Backup, StepResult};

/// Pages copied per backup step. A step holds the source's shared-cache lock,
/// so readers of the RAM projection wait at most one step; steps this large
/// cost nothing measurable over a single whole-database step.
const STEP_PAGES: i32 = 1024;

/// Copy a RAM projection into the empty private file that will be installed.
/// The file stays private until the caller syncs it and renames it into
/// place, so the copy needs neither a rollback journal nor syncs of its own:
/// an interrupted copy leaves a staging file that is never installed. The
/// pages are the ones the default backup writes.
pub(crate) fn stage_ram_projection(source: &Connection, staging: &Path) -> Result<(), String> {
    let mut destination = Connection::open(staging).map_err(|error| error.to_string())?;
    destination
        .execute_batch("PRAGMA journal_mode = OFF; PRAGMA synchronous = OFF;")
        .map_err(|error| error.to_string())?;
    let backup = Backup::new(source, &mut destination).map_err(|error| error.to_string())?;
    loop {
        match backup.step(STEP_PAGES).map_err(|error| error.to_string())? {
            StepResult::More => {}
            StepResult::Done => return Ok(()),
            blocked => return Err(format!("backup step returned {blocked:?}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quick_check(conn: &Connection) -> String {
        conn.query_row("PRAGMA quick_check", [], |row| row.get(0))
            .unwrap()
    }

    #[test]
    fn staged_projection_matches_a_default_backup_byte_for_byte() {
        let dir = tempfile::tempdir().unwrap();
        let source = Connection::open_with_flags(
            "file:stage-ram-projection?mode=memory&cache=shared",
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
                | rusqlite::OpenFlags::SQLITE_OPEN_CREATE
                | rusqlite::OpenFlags::SQLITE_OPEN_URI,
        )
        .unwrap();
        // More pages than one step, with overflow chains and an index.
        source
            .execute_batch(
                "CREATE TABLE t (id INTEGER PRIMARY KEY, body TEXT NOT NULL);
                 CREATE INDEX t_body ON t(substr(body, 1, 16));
                 WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 3000)
                 INSERT INTO t SELECT i, printf('%d-', i) || hex(randomblob(1500 + i % 7000))
                 FROM n;",
            )
            .unwrap();
        let pages: i64 = source
            .query_row("PRAGMA page_count", [], |row| row.get(0))
            .unwrap();
        assert!(pages > i64::from(STEP_PAGES) * 2, "{pages} pages");

        let staged = dir.path().join("staged.sqlite");
        std::fs::File::create(&staged).unwrap();
        stage_ram_projection(&source, &staged).unwrap();
        let reference = dir.path().join("reference.sqlite");
        source
            .backup(rusqlite::DatabaseName::Main, &reference, None)
            .unwrap();
        assert_eq!(
            std::fs::read(&staged).unwrap(),
            std::fs::read(&reference).unwrap()
        );

        let opened = Connection::open(&staged).unwrap();
        assert_eq!(quick_check(&opened), "ok");
        let mode: String = opened
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "delete");
        let rows: i64 = opened
            .query_row("SELECT COUNT(*) FROM t", [], |row| row.get(0))
            .unwrap();
        assert_eq!(rows, 3000);
    }
}
