//! Fixture replay: every committed SQLite JSON blob and row must parse into
//! the raw types with zero drift. This test IS the schema claim — if a fixture
//! regeneration (new agent version) makes it fail, that failure is the drift
//! report.

use rosetta_adapter_opencode::raw::{
    ParsedEvent, ParsedMessage, ParsedPart, ParsedProject, ParsedSession, parse_event,
    parse_message, parse_part, parse_project, parse_session,
};
use rosetta_adapter_support::{Drift, Evidence, FileFormat, GroundTruthDoc, glob_match};
use rusqlite::{Connection, OpenFlags, types::Value as SqlValue};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../groundtruth/opencode/fixtures")
}

fn ground_truth() -> GroundTruthDoc {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../groundtruth/opencode/groundtruth.json");
    GroundTruthDoc::parse(&std::fs::read_to_string(&path).unwrap())
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn database_files() -> Vec<PathBuf> {
    walkdir::WalkDir::new(fixtures_root())
        .sort_by_file_name()
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| entry.into_path())
        .filter(|path| {
            path.file_name().is_some_and(|name| name == "opencode.db")
                && path.to_string_lossy().contains("opencode-home/")
        })
        .collect()
}

fn staging_copy(path: &Path) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "rosetta-opencode-replay-{}-{}",
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

fn open_readonly(path: &Path) -> Connection {
    let dest = staging_copy(path);
    Connection::open_with_flags(&dest, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn json_column(connection: &Connection, table: &str) -> Vec<(String, String)> {
    let mut statement = connection
        .prepare(&format!("SELECT id, data FROM {table} ORDER BY id"))
        .unwrap();
    statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .unwrap()
        .map(|row| row.unwrap())
        .collect()
}

fn sql_to_json(value: SqlValue) -> serde_json::Value {
    match value {
        SqlValue::Null => serde_json::Value::Null,
        SqlValue::Integer(value) => serde_json::Value::Number(value.into()),
        SqlValue::Real(value) => serde_json::Number::from_f64(value)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        SqlValue::Text(text) => {
            let trimmed = text.trim_start();
            if trimmed.starts_with('{') || trimmed.starts_with('[') {
                serde_json::from_str(&text).unwrap_or(serde_json::Value::String(text))
            } else {
                serde_json::Value::String(text)
            }
        }
        SqlValue::Blob(bytes) => serde_json::Value::String(String::from_utf8_lossy(&bytes).into()),
    }
}

fn table_objects(connection: &Connection, table: &str) -> Vec<serde_json::Value> {
    let mut statement = connection
        .prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))
        .unwrap();
    let names: Vec<String> = statement
        .column_names()
        .into_iter()
        .map(str::to_owned)
        .collect();
    let mut rows = statement.query([]).unwrap();
    let mut out = Vec::new();
    while let Some(row) = rows.next().unwrap() {
        let mut object = serde_json::Map::new();
        for (index, name) in names.iter().enumerate() {
            object.insert(
                name.clone(),
                sql_to_json(row.get::<_, SqlValue>(index).unwrap()),
            );
        }
        out.push(serde_json::Value::Object(object));
    }
    out
}

fn event_rows(connection: &Connection) -> Vec<(String, String)> {
    let mut statement = connection
        .prepare("SELECT type, data FROM event ORDER BY seq, id")
        .unwrap();
    statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .unwrap()
        .map(|row| row.unwrap())
        .collect()
}

#[test]
fn corpus_exists() {
    assert!(
        !database_files().is_empty(),
        "no opencode.db fixtures under {} — run: python3 -m tools.groundtruth farm opencode",
        fixtures_root().display()
    );
}

#[test]
fn every_message_and_part_parses_without_drift() {
    let mut messages = 0usize;
    let mut parts = 0usize;
    let mut unknown_types = Vec::new();
    let mut drift = Vec::new();

    for file in database_files() {
        let short = file
            .strip_prefix(fixtures_root())
            .unwrap()
            .display()
            .to_string();
        let connection = open_readonly(&file);
        for (id, data) in json_column(&connection, "message") {
            messages += 1;
            let parsed = parse_message(&data).unwrap_or_else(|error| {
                panic!("{short} message {id}: hard parse failure: {error}")
            });
            match parsed {
                ParsedMessage::Known(record) => {
                    let paths = record.drift_paths();
                    if !paths.is_empty() {
                        drift.push(format!("{short} message {id}: {}", paths.join(", ")));
                    }
                }
                ParsedMessage::UnknownType { role, .. } => {
                    unknown_types.push(format!("{short} message {id}: role={role:?}"));
                }
            }
        }
        for (id, data) in json_column(&connection, "part") {
            parts += 1;
            let parsed = parse_part(&data)
                .unwrap_or_else(|error| panic!("{short} part {id}: hard parse failure: {error}"));
            match parsed {
                ParsedPart::Known(record) => {
                    let paths = record.drift_paths();
                    if !paths.is_empty() {
                        drift.push(format!("{short} part {id}: {}", paths.join(", ")));
                    }
                }
                ParsedPart::UnknownType { rtype, .. } => {
                    unknown_types.push(format!("{short} part {id}: type={rtype:?}"));
                }
            }
        }
    }

    assert!(messages > 0, "no message rows in corpus");
    assert!(parts > 0, "no part rows in corpus");
    assert!(
        unknown_types.is_empty(),
        "unknown record types in committed corpus (extend raw types with this evidence):\n{}",
        unknown_types.join("\n")
    );
    assert!(
        drift.is_empty(),
        "unknown fields in committed corpus (extend raw types with this evidence):\n{}",
        drift.join("\n")
    );
}

#[test]
fn every_session_project_and_event_parses_without_drift() {
    let mut sessions = 0usize;
    let mut projects = 0usize;
    let mut events = 0usize;
    let mut unknown_types = Vec::new();
    let mut drift = Vec::new();

    for file in database_files() {
        let short = file
            .strip_prefix(fixtures_root())
            .unwrap()
            .display()
            .to_string();
        let connection = open_readonly(&file);
        for value in table_objects(&connection, "session") {
            sessions += 1;
            match parse_session(value)
                .unwrap_or_else(|error| panic!("{short} session: hard parse failure: {error}"))
            {
                ParsedSession::Known(record) => {
                    let paths = record.drift_paths();
                    if !paths.is_empty() {
                        drift.push(format!("{short} session: {}", paths.join(", ")));
                    }
                }
            }
        }
        for value in table_objects(&connection, "project") {
            projects += 1;
            match parse_project(value)
                .unwrap_or_else(|error| panic!("{short} project: hard parse failure: {error}"))
            {
                ParsedProject::Known(record) => {
                    let paths = record.drift_paths();
                    if !paths.is_empty() {
                        drift.push(format!("{short} project: {}", paths.join(", ")));
                    }
                }
            }
        }
        for (event_type, data) in event_rows(&connection) {
            events += 1;
            let parsed = parse_event(&event_type, &data).unwrap_or_else(|error| {
                panic!("{short} event {event_type}: hard parse failure: {error}")
            });
            match parsed {
                ParsedEvent::Known(record) => {
                    let paths = record.drift_paths();
                    if !paths.is_empty() {
                        drift.push(format!("{short} event {event_type}: {}", paths.join(", ")));
                    }
                }
                ParsedEvent::UnknownType { rtype, .. } => {
                    unknown_types.push(format!("{short} event type={rtype:?}"));
                }
            }
        }
    }

    assert!(sessions > 0, "no session rows");
    assert!(projects > 0, "no project rows");
    assert!(events > 0, "no event rows");
    assert!(
        unknown_types.is_empty(),
        "unknown event types in committed corpus:\n{}",
        unknown_types.join("\n")
    );
    assert!(
        drift.is_empty(),
        "unknown fields in session/project/event corpus:\n{}",
        drift.join("\n")
    );
}

#[test]
fn payloads_round_trip_losslessly() {
    for file in database_files() {
        let connection = open_readonly(&file);
        for (id, data) in json_column(&connection, "message") {
            let original: serde_json::Value = serde_json::from_str(&data).unwrap();
            if let ParsedMessage::Known(record) = parse_message(&data).unwrap() {
                assert_eq!(
                    serde_json::to_value(&*record).unwrap(),
                    original,
                    "lossy message round-trip at {}: {id}",
                    file.display()
                );
            }
        }
        for (id, data) in json_column(&connection, "part") {
            let original: serde_json::Value = serde_json::from_str(&data).unwrap();
            if let ParsedPart::Known(record) = parse_part(&data).unwrap() {
                assert_eq!(
                    serde_json::to_value(&*record).unwrap(),
                    original,
                    "lossy part round-trip at {}: {id}",
                    file.display()
                );
            }
        }
        for value in table_objects(&connection, "session") {
            let ParsedSession::Known(record) = parse_session(value.clone()).unwrap();
            assert_eq!(
                serde_json::to_value(&*record).unwrap(),
                value,
                "lossy session round-trip at {}",
                file.display()
            );
        }
        for value in table_objects(&connection, "project") {
            let ParsedProject::Known(record) = parse_project(value.clone()).unwrap();
            assert_eq!(
                serde_json::to_value(&*record).unwrap(),
                value,
                "lossy project round-trip at {}",
                file.display()
            );
        }
        for (event_type, data) in event_rows(&connection) {
            let original: serde_json::Value = serde_json::from_str(&data).unwrap();
            if let ParsedEvent::Known(record) = parse_event(&event_type, &data).unwrap() {
                assert_eq!(
                    serde_json::to_value(&*record).unwrap(),
                    original,
                    "lossy event round-trip at {} ({event_type})",
                    file.display()
                );
            }
        }
    }
}

#[test]
fn harvested_files_match_layout_manifest() {
    let ground_truth = ground_truth();
    let mut unmatched = Vec::new();
    for entry in walkdir::WalkDir::new(fixtures_root()).sort_by_file_name() {
        let entry = entry.unwrap();
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path().to_string_lossy().into_owned();
        let Some((_, rel)) = path.split_once("opencode-home/") else {
            continue;
        };
        if ground_truth.matching_stream(rel).is_none() {
            unmatched.push(rel.to_string());
        }
    }
    unmatched.sort();
    unmatched.dedup();
    assert!(
        unmatched.is_empty(),
        "files outside the layout manifest (add streams with this evidence):\n{}",
        unmatched.join("\n")
    );
}

#[test]
fn fixture_backed_streams_have_coverage() {
    let ground_truth = ground_truth();
    for stream in ground_truth
        .streams
        .iter()
        .filter(|stream| stream.evidence == Evidence::Fixture && !stream.id.starts_with("farm:"))
    {
        let covered = walkdir::WalkDir::new(fixtures_root())
            .into_iter()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_file())
            .any(|entry| {
                let path = entry.path().to_string_lossy().into_owned();
                path.split_once("opencode-home/")
                    .is_some_and(|(_, rel)| glob_match(&stream.glob, rel))
            });
        assert!(
            covered,
            "stream `{}` claims Fixture evidence but no fixture file matches `{}`",
            stream.id, stream.glob
        );
    }
    let database = ground_truth
        .stream("opencode-db")
        .expect("opencode-db stream");
    assert_eq!(database.file_format, FileFormat::Sqlite);
    let git_projects = database_files()
        .iter()
        .map(|path| {
            open_readonly(path)
                .query_row(
                    "SELECT count(*) FROM project WHERE id != 'global'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap()
        })
        .sum::<i64>();
    let global_projects = database_files()
        .iter()
        .map(|path| {
            open_readonly(path)
                .query_row(
                    "SELECT count(*) FROM project WHERE id = 'global'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap()
        })
        .sum::<i64>();
    assert!(
        git_projects > 0,
        "expected a git worktree project (tool-use/resume) in the corpus"
    );
    assert!(
        global_projects > 0,
        "expected the non-git `global` project (basic-chat) in the corpus"
    );
}
