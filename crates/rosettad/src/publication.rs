//! Staging a RAM projection on disk for atomic publication.

use std::path::Path;

use rusqlite::Connection;
use rusqlite::backup::{Backup, StepResult};

/// The writer holds this projection's exclusive lock. A killed backup can
/// leave part of a database on disk; replace just this projection's private
/// scratch files on the next attempt. A pause/resume uses the same name, so
/// the footprint sampler continues to measure the current backup.
pub(crate) fn cold_copy(db: &Path) -> Result<tempfile::TempPath, String> {
    let name = db.file_name().ok_or("projection needs a file name")?;
    let key = blake3::hash(name.as_encoded_bytes()).to_hex();
    let basename = format!(".rosetta-backfill-{key}.tmp");
    let parent = crate::projection_parent(db);
    let copy = parent.join(&basename);
    for path in [
        crate::sidecar(&copy, "-wal"),
        crate::sidecar(&copy, "-shm"),
        crate::sidecar(&copy, "-journal"),
        copy,
    ] {
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(format!(
                    "clearing private projection {}: {e}",
                    path.display()
                ));
            }
        }
    }
    tempfile::Builder::new()
        .prefix(&basename)
        .rand_bytes(0)
        .tempfile_in(parent)
        .map(|file| file.into_temp_path())
        .map_err(|e| format!("creating private projection for {}: {e}", db.display()))
}

/// Pages copied per backup step. A step holds the source's shared-cache lock,
/// so readers of the RAM projection wait at most one step; steps this large
/// cost nothing measurable over a single whole-database step.
const STEP_PAGES: i32 = 1024;

/// Copy a RAM projection into the empty private file that will be installed.
/// The file stays private until the caller syncs it and renames it into
/// place, so the copy needs neither a rollback journal nor syncs of its own:
/// an interrupted copy leaves a staging file that is never installed. The
/// pages are the ones the default backup writes.
pub(crate) fn stage_ram_projection(
    source: &Connection,
    staging: &Path,
    should_stop: impl Fn() -> bool,
) -> Result<(), String> {
    let mut destination = Connection::open(staging).map_err(|error| error.to_string())?;
    destination
        .execute_batch("PRAGMA journal_mode = OFF; PRAGMA synchronous = OFF;")
        .map_err(|error| error.to_string())?;
    let backup = Backup::new(source, &mut destination).map_err(|error| error.to_string())?;
    loop {
        // The destination is still private. A shutdown here keeps the staged
        // ledger for recovery and avoids spending the host's grace period on
        // pages that can be reconstructed from that ledger on restart.
        if should_stop() {
            return Err("private projection copy interrupted by shutdown".into());
        }
        // sqlite3_backup_step returns its own result code. Until backup_finish,
        // the destination handle can still describe SQLITE_OK ("not an error").
        let step = backup.step(STEP_PAGES).map_err(|error| match &error {
            rusqlite::Error::SqliteFailure(code, _) => code.to_string(),
            _ => error.to_string(),
        })?;
        match step {
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
    fn restarting_replaces_only_this_projections_staging_file() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("first.sqlite");
        let other = super::cold_copy(&dir.path().join("second.sqlite")).unwrap();
        std::fs::write(&other, b"other writer").unwrap();
        let partial = super::cold_copy(&db).unwrap().keep().unwrap();
        std::fs::write(&partial, b"incomplete backup").unwrap();
        let journal = crate::sidecar(&partial, "-journal");
        std::fs::write(&journal, b"partial journal").unwrap();
        let restarted = super::cold_copy(&db).unwrap();
        assert_eq!(&*restarted, partial);
        assert_eq!(std::fs::metadata(&restarted).unwrap().len(), 0);
        assert!(!journal.exists());
        assert_eq!(std::fs::read(&other).unwrap(), b"other writer");
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
        let steps = std::cell::Cell::new(0);
        let error = stage_ram_projection(&source, &staged, || {
            steps.set(steps.get() + 1);
            steps.get() == 2
        })
        .unwrap_err();
        assert!(error.contains("interrupted by shutdown"));
        assert_eq!(steps.get(), 2);
        // Recreate the private copy the way a new writer does on restart.
        std::fs::File::create(&staged).unwrap();
        stage_ram_projection(&source, &staged, || false).unwrap();
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
