//! Fixture replay: every committed primary state.db row must parse into the
//! raw types with zero drift. This test IS the schema claim — if a fixture
//! regeneration (new agent version) makes it fail, that failure is the drift
//! report.

use rosetta_adapter_hermes::raw::{KnownRecord, ParsedRow, parse_row, sqlite_row_to_value};
use rosetta_adapter_support::{Drift, Evidence, FileFormat, GroundTruthDoc, glob_match};
use rusqlite::{Connection, OpenFlags};
use std::path::{Path, PathBuf};

fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../groundtruth/hermes/fixtures")
}

fn ground_truth() -> GroundTruthDoc {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../groundtruth/hermes/groundtruth.json");
    GroundTruthDoc::parse(&std::fs::read_to_string(&path).unwrap())
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn state_db_files() -> Vec<PathBuf> {
    walkdir::WalkDir::new(fixtures_root())
        .sort_by_file_name()
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .map(|e| e.into_path())
        .filter(|p| p.file_name().is_some_and(|name| name == "state.db"))
        .collect()
}

fn export_jsonl_files() -> Vec<(String, PathBuf)> {
    const TABLES: [&str; 6] = [
        "sessions",
        "messages",
        "session_model_usage",
        "schema_version",
        "state_meta",
        "system_prompts",
    ];
    walkdir::WalkDir::new(fixtures_root())
        .sort_by_file_name()
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .map(|e| e.into_path())
        .filter_map(|path| {
            let name = path.file_name()?.to_str()?;
            let table = TABLES
                .iter()
                .copied()
                .find(|table| name == format!("{table}.jsonl"))?;
            Some((table.to_owned(), path))
        })
        .collect()
}

#[test]
fn corpus_exists() {
    assert!(
        !state_db_files().is_empty(),
        "no state.db fixtures under {} — run: python3 -m tools.groundtruth farm hermes",
        fixtures_root().display()
    );
    assert!(
        !export_jsonl_files().is_empty(),
        "no SQLite JSONL exports under {}",
        fixtures_root().display()
    );
}

#[test]
fn every_primary_row_parses_without_drift() {
    let mut rows = 0usize;
    let mut unknown_tables = Vec::new();
    let mut drift = Vec::new();

    for (table, file) in export_jsonl_files() {
        let text = std::fs::read_to_string(&file).unwrap();
        let short = file
            .strip_prefix(fixtures_root())
            .unwrap()
            .display()
            .to_string();
        for (no, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            rows += 1;
            let value: serde_json::Value = serde_json::from_str(line).unwrap();
            let parsed = parse_row(&table, value)
                .unwrap_or_else(|e| panic!("{short}:{}: hard parse failure: {e}", no + 1));
            match parsed {
                ParsedRow::Known(rec) => {
                    let paths = rec.drift_paths();
                    if !paths.is_empty() {
                        drift.push(format!("{short}:{}: {}", no + 1, paths.join(", ")));
                    }
                }
                ParsedRow::UnknownTable { table, .. } => {
                    unknown_tables.push(format!("{short}:{}: table={table}", no + 1));
                }
            }
        }
    }

    assert!(rows > 0, "corpus is empty");
    assert!(
        unknown_tables.is_empty(),
        "unknown tables in committed primary exports:\n{}",
        unknown_tables.join("\n")
    );
    assert!(
        drift.is_empty(),
        "unknown fields in committed corpus (extend raw types with this evidence):\n{}",
        drift.join("\n")
    );
}

#[test]
fn exports_round_trip_losslessly() {
    for (table, file) in export_jsonl_files() {
        let text = std::fs::read_to_string(&file).unwrap();
        for (no, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let original: serde_json::Value = serde_json::from_str(line).unwrap();
            if let ParsedRow::Known(rec) = parse_row(&table, original.clone()).unwrap() {
                let back = match &*rec {
                    KnownRecord::Session(row) => serde_json::to_value(row).unwrap(),
                    KnownRecord::Message(row) => serde_json::to_value(row).unwrap(),
                    KnownRecord::SessionModelUsage(row) => serde_json::to_value(row).unwrap(),
                    KnownRecord::SchemaVersion(row) => serde_json::to_value(row).unwrap(),
                    KnownRecord::StateMeta(row) => serde_json::to_value(row).unwrap(),
                    KnownRecord::SystemPrompt(row) => serde_json::to_value(row).unwrap(),
                };
                assert_eq!(
                    back,
                    original,
                    "lossy round-trip at {}:{}",
                    file.display(),
                    no + 1
                );
            }
        }
    }
}

#[test]
fn sqlite_files_parse_with_the_same_types() {
    for file in state_db_files() {
        let connection =
            Connection::open_with_flags(&file, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let tables: Vec<String> = connection
            .prepare(
                "SELECT name FROM sqlite_master \
                 WHERE type='table' AND name NOT LIKE 'sqlite_%' \
                 AND name NOT LIKE '%fts%' ORDER BY name",
            )
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        for table in tables {
            let mut statement = connection
                .prepare(&format!("SELECT * FROM \"{table}\""))
                .unwrap();
            let mut rows = statement.query([]).unwrap();
            while let Some(row) = rows.next().unwrap() {
                let value = sqlite_row_to_value(row).unwrap();
                parse_row(&table, value)
                    .unwrap_or_else(|error| panic!("{} table {table}: {error}", file.display()));
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
        let Some((_, rel)) = path.split_once("hermes-home/") else {
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
        .filter(|s| s.evidence == Evidence::Fixture && !s.id.starts_with("farm:"))
    {
        let covered = walkdir::WalkDir::new(fixtures_root())
            .into_iter()
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_file())
            .any(|e| {
                let p = e.path().to_string_lossy().into_owned();
                p.split_once("hermes-home/")
                    .is_some_and(|(_, rel)| glob_match(&stream.glob, rel))
            });
        assert!(
            covered,
            "stream `{}` claims Fixture evidence but no fixture file matches `{}`",
            stream.id, stream.glob
        );
    }
    let dbs = ground_truth.stream("state-db").expect("state-db stream");
    assert_eq!(dbs.file_format, FileFormat::Sqlite);
}
