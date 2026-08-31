//! Deliberately structure-free cold-ingest controls.
//!
//! This is not an alternate Rosetta format. It measures the physical floor
//! under the real source roster: read every admitted JSONL byte into RAM, then
//! put those bytes into a nearly schema-free SQLite file in one transaction.

use std::fs::{self, File};
use std::io::Read;
use std::path::PathBuf;
use std::time::Instant;

#[cfg(target_os = "macos")]
use std::os::fd::AsRawFd;

use rosetta_adapter_support::{AgentAdapter, FileFormat, StreamDriver};
use rosetta_pipeline::{IngestOptions, Instrument, discover};
use rusqlite::{Connection, params};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Granularity {
    File,
    Line,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Durability {
    /// SQLite and filesystem durability both disabled: absolute write floor.
    None,
    /// Private scratch semantics: no journal, then one explicit final fsync.
    Fsync,
    /// Ordinary SQLite FULL synchronization for comparison.
    SqliteFull,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SqliteLocation {
    Disk,
    Memory,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SourceCache {
    Default,
    Bypass,
}

struct Flags {
    home: PathBuf,
    db: PathBuf,
    agents: Vec<String>,
    granularity: Granularity,
    durability: Durability,
    page_size: u32,
    sqlite_location: SqliteLocation,
    source_cache: SourceCache,
    quick_check: bool,
    json: bool,
}

struct RawFile {
    agent: &'static str,
    relative_path: String,
    modified_ms: i64,
    expected_bytes: u64,
    bytes: Vec<u8>,
}

#[derive(Default)]
struct Times {
    discovery_ms: f64,
    read_ms: f64,
    schema_ms: f64,
    insert_ms: f64,
    commit_ms: f64,
    materialize_ms: f64,
    close_ms: f64,
    quick_check_ms: f64,
    fsync_ms: f64,
    total_ms: f64,
}

pub fn cmd_brutal_ingest(
    args: &[String],
    registry: Vec<&'static dyn AgentAdapter>,
) -> Result<(), String> {
    let flags = parse(args)?;
    if flags.db.exists() {
        return Err(format!(
            "brutal-ingest refuses existing --db {}; choose a new target",
            flags.db.display()
        ));
    }
    let selected = select(registry, &flags.agents)?;
    let started = Instant::now();
    let mut times = Times::default();

    let stage = Instant::now();
    let discoveries = discover(
        &selected,
        &flags.home,
        &IngestOptions::default(),
        &Instrument::new(),
    )
    .map_err(|error| format!("discovering source roster: {error}"))?;
    times.discovery_ms = elapsed_ms(stage);

    let mut roster = Vec::new();
    let mut expected_bytes = 0_u64;
    for (adapter, discovery) in selected.iter().zip(&discoveries) {
        for file in &discovery.inventory {
            let Some(stream) = adapter.layout().matching_stream(&file.relative_path) else {
                continue;
            };
            if stream.file_format != FileFormat::Jsonl
                || adapter.stream_driver(&stream.id) != Some(StreamDriver::JsonLines)
            {
                continue;
            }
            expected_bytes = expected_bytes.saturating_add(file.size);
            roster.push((
                *adapter,
                discovery.root.join(&file.relative_path),
                file.clone(),
            ));
        }
    }

    let stage = Instant::now();
    let mut raw = Vec::with_capacity(roster.len());
    let mut vanished = 0_u64;
    let mut changed = 0_u64;
    let mut bytes_read = 0_u64;
    for (adapter, absolute_path, file) in roster {
        let mut handle = match File::open(&absolute_path) {
            Ok(handle) => handle,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                vanished += 1;
                continue;
            }
            Err(error) => {
                return Err(format!("opening {}: {error}", absolute_path.display()));
            }
        };
        configure_source_cache(&handle, flags.source_cache, &absolute_path)?;
        let capacity = usize::try_from(file.size).unwrap_or(0);
        let mut bytes = Vec::with_capacity(capacity);
        handle
            .read_to_end(&mut bytes)
            .map_err(|error| format!("reading {}: {error}", absolute_path.display()))?;
        bytes_read = bytes_read.saturating_add(bytes.len() as u64);
        if bytes.len() as u64 != file.size {
            changed += 1;
        }
        raw.push(RawFile {
            agent: adapter.kind(),
            relative_path: file.relative_path,
            modified_ms: file.modified_ms,
            expected_bytes: file.size,
            bytes,
        });
    }
    times.read_ms = elapsed_ms(stage);

    if let Some(parent) = flags.db.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("creating {}: {error}", parent.display()))?;
    }
    let mut connection = match flags.sqlite_location {
        SqliteLocation::Disk => Connection::open(&flags.db)
            .map_err(|error| format!("opening {}: {error}", flags.db.display()))?,
        SqliteLocation::Memory => Connection::open_in_memory()
            .map_err(|error| format!("opening in-memory SQLite control: {error}"))?,
    };

    let stage = Instant::now();
    configure(&connection, flags.page_size, flags.durability)?;
    match flags.granularity {
        Granularity::File => connection
            .execute_batch(
                "CREATE TABLE raw_file (
                    id          INTEGER PRIMARY KEY,
                    agent       TEXT NOT NULL,
                    path        TEXT NOT NULL,
                    modified_ms INTEGER NOT NULL,
                    payload     BLOB NOT NULL
                 );",
            )
            .map_err(|error| format!("creating raw-file table: {error}"))?,
        Granularity::Line => connection
            .execute_batch(
                "CREATE TABLE raw_source (
                    id          INTEGER PRIMARY KEY,
                    agent       TEXT NOT NULL,
                    path        TEXT NOT NULL,
                    modified_ms INTEGER NOT NULL
                 );
                 CREATE TABLE raw_record (
                    id        INTEGER PRIMARY KEY,
                    source_id INTEGER NOT NULL,
                    payload   BLOB NOT NULL
                 );",
            )
            .map_err(|error| format!("creating raw-line tables: {error}"))?,
    }
    times.schema_ms = elapsed_ms(stage);

    let tx = connection
        .transaction()
        .map_err(|error| format!("starting one-shot transaction: {error}"))?;
    let stage = Instant::now();
    let stored_rows = match flags.granularity {
        Granularity::File => {
            let mut insert = tx
                .prepare_cached(
                    "INSERT INTO raw_file(agent, path, modified_ms, payload)
                     VALUES (?1, ?2, ?3, ?4)",
                )
                .map_err(|error| format!("preparing raw-file insert: {error}"))?;
            for file in &raw {
                insert
                    .execute(params![
                        file.agent,
                        file.relative_path,
                        file.modified_ms,
                        file.bytes
                    ])
                    .map_err(|error| {
                        format!("inserting {}/{}: {error}", file.agent, file.relative_path)
                    })?;
            }
            raw.len() as u64
        }
        Granularity::Line => {
            let mut insert_source = tx
                .prepare_cached(
                    "INSERT INTO raw_source(agent, path, modified_ms) VALUES (?1, ?2, ?3)",
                )
                .map_err(|error| format!("preparing raw-source insert: {error}"))?;
            let mut insert_record = tx
                .prepare_cached("INSERT INTO raw_record(source_id, payload) VALUES (?1, ?2)")
                .map_err(|error| format!("preparing raw-record insert: {error}"))?;
            let mut rows = 0_u64;
            for file in &raw {
                insert_source
                    .execute(params![file.agent, file.relative_path, file.modified_ms])
                    .map_err(|error| {
                        format!(
                            "inserting source {}/{}: {error}",
                            file.agent, file.relative_path
                        )
                    })?;
                let source_id = tx.last_insert_rowid();
                for line in file.bytes.split_inclusive(|byte| *byte == b'\n') {
                    if line.is_empty() {
                        continue;
                    }
                    insert_record
                        .execute(params![source_id, line])
                        .map_err(|error| {
                            format!(
                                "inserting record {} from {}/{}: {error}",
                                rows, file.agent, file.relative_path
                            )
                        })?;
                    rows += 1;
                }
            }
            rows
        }
    };
    times.insert_ms = elapsed_ms(stage);

    let stage = Instant::now();
    tx.commit()
        .map_err(|error| format!("committing one-shot transaction: {error}"))?;
    times.commit_ms = elapsed_ms(stage);

    if flags.sqlite_location == SqliteLocation::Memory {
        let stage = Instant::now();
        connection
            .backup(rusqlite::DatabaseName::Main, &flags.db, None)
            .map_err(|error| format!("materializing in-memory SQLite control: {error}"))?;
        times.materialize_ms = elapsed_ms(stage);
    }

    let stage = Instant::now();
    drop(connection);
    times.close_ms = elapsed_ms(stage);

    if flags.durability == Durability::Fsync {
        let stage = Instant::now();
        File::open(&flags.db)
            .and_then(|file| file.sync_all())
            .map_err(|error| format!("syncing {}: {error}", flags.db.display()))?;
        if let Some(parent) = flags.db.parent() {
            File::open(parent)
                .and_then(|directory| directory.sync_all())
                .map_err(|error| format!("syncing {}: {error}", parent.display()))?;
        }
        times.fsync_ms = elapsed_ms(stage);
    }
    if flags.quick_check {
        let stage = Instant::now();
        let checked = Connection::open(&flags.db)
            .map_err(|error| format!("opening raw control database for validation: {error}"))?;
        let quick_check: String = checked
            .query_row("PRAGMA quick_check", [], |row| row.get(0))
            .map_err(|error| format!("checking raw control database: {error}"))?;
        if quick_check != "ok" {
            return Err(format!(
                "raw control database failed quick_check: {quick_check}"
            ));
        }
        times.quick_check_ms = elapsed_ms(stage);
    }
    times.total_ms = elapsed_ms(started);

    let db_bytes = fs::metadata(&flags.db)
        .map_err(|error| format!("statting {}: {error}", flags.db.display()))?
        .len();
    let retained_bytes = raw.iter().fold(0_u64, |total, file| {
        total.saturating_add(file.bytes.len() as u64)
    });
    let expected_retained_bytes = raw.iter().fold(0_u64, |total, file| {
        total.saturating_add(file.expected_bytes)
    });
    let result = serde_json::json!({
        "event": "brutal-ingest",
        "scope": "admitted-jsonl",
        "granularity": match flags.granularity { Granularity::File => "file", Granularity::Line => "line" },
        "durability": match flags.durability { Durability::None => "none", Durability::Fsync => "fsync", Durability::SqliteFull => "sqlite-full" },
        "sqliteLocation": match flags.sqlite_location { SqliteLocation::Disk => "disk", SqliteLocation::Memory => "memory-then-backup" },
        "sourceCache": match flags.source_cache { SourceCache::Default => "default", SourceCache::Bypass => "bypass" },
        "pageSize": flags.page_size,
        "quickCheck": flags.quick_check,
        "sourceFiles": raw.len(),
        "storedRows": stored_rows,
        "inventoryExpectedBytes": expected_bytes,
        "selectedExpectedBytes": expected_retained_bytes,
        "bytesRead": bytes_read,
        "bytesRetainedInRam": retained_bytes,
        "vanishedFiles": vanished,
        "changedWhileReading": changed,
        "databaseBytes": db_bytes,
        "timingMs": {
            "discovery": times.discovery_ms,
            "readAllIntoRam": times.read_ms,
            "schema": times.schema_ms,
            "insertAll": times.insert_ms,
            "commit": times.commit_ms,
            "materializeToDisk": times.materialize_ms,
            "closeAndPagerDrain": times.close_ms,
            "quickCheck": times.quick_check_ms,
            "finalFsync": times.fsync_ms,
            "total": times.total_ms,
        },
        "throughputMiBPerSec": {
            "read": mib_per_sec(bytes_read, times.read_ms),
            "insert": mib_per_sec(bytes_read, times.insert_ms),
            "endToEnd": mib_per_sec(bytes_read, times.total_ms),
        },
        "database": flags.db,
    });
    if flags.json {
        println!("{result}");
    } else {
        println!(
            "brutal ingest: {} JSONL files, {:.2} GiB, {} SQLite rows",
            raw.len(),
            bytes_read as f64 / (1024.0 * 1024.0 * 1024.0),
            stored_rows
        );
        println!(
            "discovery              {:>9.3} s",
            times.discovery_ms / 1_000.0
        );
        println!("read all into RAM      {:>9.3} s", times.read_ms / 1_000.0);
        println!(
            "schema                 {:>9.3} s",
            times.schema_ms / 1_000.0
        );
        println!(
            "insert (one tx)        {:>9.3} s",
            times.insert_ms / 1_000.0
        );
        println!(
            "commit                 {:>9.3} s",
            times.commit_ms / 1_000.0
        );
        println!(
            "materialize to disk    {:>9.3} s",
            times.materialize_ms / 1_000.0
        );
        println!("close / pager drain    {:>9.3} s", times.close_ms / 1_000.0);
        println!(
            "quick_check            {:>9.3} s",
            times.quick_check_ms / 1_000.0
        );
        println!("final fsync            {:>9.3} s", times.fsync_ms / 1_000.0);
        println!("total                  {:>9.3} s", times.total_ms / 1_000.0);
        println!(
            "database               {:.2} GiB",
            db_bytes as f64 / (1024.0 * 1024.0 * 1024.0)
        );
        println!("machine-readable result:\n{result}");
    }
    Ok(())
}

fn configure(
    connection: &Connection,
    page_size: u32,
    durability: Durability,
) -> Result<(), String> {
    connection
        .execute_batch(&format!(
            "PRAGMA page_size = {page_size};
             PRAGMA temp_store = MEMORY;
             PRAGMA locking_mode = EXCLUSIVE;
             PRAGMA cache_size = -262144;
             PRAGMA auto_vacuum = NONE;"
        ))
        .map_err(|error| format!("configuring raw control database: {error}"))?;
    let durability_pragmas = match durability {
        Durability::None | Durability::Fsync => {
            "PRAGMA journal_mode = OFF; PRAGMA synchronous = OFF;"
        }
        Durability::SqliteFull => "PRAGMA journal_mode = DELETE; PRAGMA synchronous = FULL;",
    };
    connection
        .execute_batch(durability_pragmas)
        .map_err(|error| format!("configuring durability: {error}"))?;
    Ok(())
}

fn parse(args: &[String]) -> Result<Flags, String> {
    let mut home = dirs_home()?;
    let mut db = None;
    let mut agents = Vec::new();
    let mut granularity = Granularity::File;
    let mut durability = Durability::Fsync;
    let mut page_size = 65_536;
    let mut sqlite_location = SqliteLocation::Disk;
    let mut source_cache = SourceCache::Default;
    let mut quick_check = false;
    let mut json = false;
    let mut at = 0;
    while at < args.len() {
        match args[at].as_str() {
            "--home" => home = PathBuf::from(value(args, &mut at)?),
            "--db" => db = Some(PathBuf::from(value(args, &mut at)?)),
            "--agent" => agents.push(value(args, &mut at)?),
            "--granularity" => {
                granularity = match value(args, &mut at)?.as_str() {
                    "file" => Granularity::File,
                    "line" => Granularity::Line,
                    other => return Err(format!("--granularity wants file|line, got {other}")),
                };
            }
            "--durability" => {
                durability = match value(args, &mut at)?.as_str() {
                    "none" => Durability::None,
                    "fsync" => Durability::Fsync,
                    "sqlite-full" => Durability::SqliteFull,
                    other => {
                        return Err(format!(
                            "--durability wants none|fsync|sqlite-full, got {other}"
                        ));
                    }
                };
            }
            "--page-size" => {
                page_size = value(args, &mut at)?
                    .parse()
                    .map_err(|_| "--page-size wants 4096 or 65536".to_string())?;
                if !matches!(page_size, 4_096 | 65_536) {
                    return Err("--page-size wants 4096 or 65536".to_string());
                }
            }
            "--sqlite-location" => {
                sqlite_location = match value(args, &mut at)?.as_str() {
                    "disk" => SqliteLocation::Disk,
                    "memory" => SqliteLocation::Memory,
                    other => {
                        return Err(format!("--sqlite-location wants disk|memory, got {other}"));
                    }
                };
            }
            "--source-cache" => {
                source_cache = match value(args, &mut at)?.as_str() {
                    "default" => SourceCache::Default,
                    "bypass" => SourceCache::Bypass,
                    other => {
                        return Err(format!("--source-cache wants default|bypass, got {other}"));
                    }
                };
            }
            "--quick-check" => {
                quick_check = true;
                at += 1;
            }
            "--json" => {
                json = true;
                at += 1;
            }
            other => return Err(format!("unknown brutal-ingest flag {other}")),
        }
    }
    Ok(Flags {
        home,
        db: db.ok_or("brutal-ingest needs --db PATH")?,
        agents,
        granularity,
        durability,
        page_size,
        sqlite_location,
        source_cache,
        quick_check,
        json,
    })
}

fn select(
    registry: Vec<&'static dyn AgentAdapter>,
    requested: &[String],
) -> Result<Vec<&'static dyn AgentAdapter>, String> {
    if requested.is_empty() {
        return Ok(registry);
    }
    let mut selected = Vec::with_capacity(requested.len());
    for kind in requested {
        let adapter = registry
            .iter()
            .copied()
            .find(|adapter| adapter.kind() == kind)
            .ok_or_else(|| format!("unknown agent {kind}"))?;
        if !selected
            .iter()
            .any(|selected: &&dyn AgentAdapter| selected.kind() == kind)
        {
            selected.push(adapter);
        }
    }
    Ok(selected)
}

fn value(args: &[String], at: &mut usize) -> Result<String, String> {
    let flag = &args[*at];
    let value = args
        .get(*at + 1)
        .ok_or_else(|| format!("{flag} needs a value"))?;
    *at += 2;
    Ok(value.clone())
}

fn dirs_home() -> Result<PathBuf, String> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or("HOME is not set".to_string())
}

fn elapsed_ms(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1_000.0
}

fn mib_per_sec(bytes: u64, milliseconds: f64) -> f64 {
    if milliseconds <= 0.0 {
        return 0.0;
    }
    bytes as f64 / (1024.0 * 1024.0) / (milliseconds / 1_000.0)
}

#[cfg(target_os = "macos")]
fn configure_source_cache(
    file: &File,
    source_cache: SourceCache,
    path: &std::path::Path,
) -> Result<(), String> {
    if source_cache == SourceCache::Default {
        return Ok(());
    }
    // SAFETY: `file` owns a live descriptor for the duration of this call;
    // F_NOCACHE changes only the kernel caching policy for that descriptor.
    let result = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_NOCACHE, 1) };
    if result == -1 {
        return Err(format!(
            "disabling source cache for {}: {}",
            path.display(),
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn configure_source_cache(
    _file: &File,
    source_cache: SourceCache,
    path: &std::path::Path,
) -> Result<(), String> {
    if source_cache == SourceCache::Bypass {
        return Err(format!(
            "--source-cache bypass is not implemented for {} on this platform",
            path.display()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn inclusive_line_split_preserves_every_byte() {
        let bytes = b"one\ntwo\nthree";
        let lines: Vec<&[u8]> = bytes.split_inclusive(|byte| *byte == b'\n').collect();
        assert_eq!(lines, [b"one\n".as_slice(), b"two\n", b"three"]);
        assert_eq!(lines.concat(), bytes);
    }
}
