//! Fixture replay: every committed session line must parse into the raw types
//! with zero drift. This test IS the schema claim — if a fixture regeneration
//! (new agent version) makes it fail, that failure is the drift report.

use rosetta_adapter_pi::raw::{ParsedLine, parse_line};
use rosetta_adapter_support::{Drift, Evidence, GroundTruthDoc, glob_match};
use std::path::{Path, PathBuf};

fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../groundtruth/pi/fixtures")
}

fn ground_truth() -> GroundTruthDoc {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../groundtruth/pi/groundtruth.json");
    GroundTruthDoc::parse(&std::fs::read_to_string(&path).unwrap())
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn session_files() -> Vec<PathBuf> {
    walkdir::WalkDir::new(fixtures_root())
        .sort_by_file_name()
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| entry.into_path())
        .filter(|path| {
            path.extension().is_some_and(|ext| ext == "jsonl")
                && path.to_string_lossy().contains("pi-home/agent/sessions/")
        })
        .collect()
}

#[test]
fn corpus_exists() {
    assert!(
        !session_files().is_empty(),
        "no session fixtures under {} — run: python3 -m tools.groundtruth farm pi",
        fixtures_root().display()
    );
}

#[test]
fn every_session_line_parses_without_drift() {
    let mut lines = 0usize;
    let mut unknown_types = Vec::new();
    let mut drift = Vec::new();

    for file in session_files() {
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
            lines += 1;
            let parsed = parse_line(line)
                .unwrap_or_else(|error| panic!("{short}:{}: hard parse failure: {error}", no + 1));
            match parsed {
                ParsedLine::Known(record) => {
                    let paths = record.drift_paths();
                    if !paths.is_empty() {
                        drift.push(format!("{short}:{}: {}", no + 1, paths.join(", ")));
                    }
                }
                ParsedLine::UnknownType { rtype, .. } => {
                    unknown_types.push(format!("{short}:{}: type={rtype:?}", no + 1));
                }
            }
        }
    }

    assert!(lines > 0, "corpus is empty");
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
fn sessions_round_trip_losslessly() {
    for file in session_files() {
        let text = std::fs::read_to_string(&file).unwrap();
        for (no, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let original: serde_json::Value = serde_json::from_str(line).unwrap();
            if let ParsedLine::Known(record) = parse_line(line).unwrap() {
                let back = serde_json::to_value(&*record).unwrap();
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
fn harvested_files_match_layout_manifest() {
    let ground_truth = ground_truth();
    let mut unmatched = Vec::new();
    for entry in walkdir::WalkDir::new(fixtures_root()).sort_by_file_name() {
        let entry = entry.unwrap();
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path().to_string_lossy().into_owned();
        let Some((_, rel)) = path.split_once("pi-home/") else {
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
                path.split_once("pi-home/")
                    .is_some_and(|(_, rel)| glob_match(&stream.glob, rel))
            });
        assert!(
            covered,
            "stream `{}` claims Fixture evidence but no fixture file matches `{}`",
            stream.id, stream.glob
        );
    }
}
