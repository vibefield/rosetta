//! Fixture replay: every committed primary JSON/JSONL record must parse into
//! the raw types with zero drift. This test IS the schema claim — if a fixture
//! regeneration (new agent version) makes it fail, that failure is the drift
//! report.

use rosetta_adapter_kimi_code::raw::{
    ParsedLine, parse_session_index_line, parse_session_state, parse_wire_line, parse_workspaces,
};
use rosetta_adapter_support::{Drift, Evidence, GroundTruthDoc, glob_match};
use std::path::{Path, PathBuf};

fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../groundtruth/kimi-code/fixtures")
}

fn ground_truth() -> GroundTruthDoc {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../groundtruth/kimi-code/groundtruth.json");
    GroundTruthDoc::parse(&std::fs::read_to_string(&path).unwrap())
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn home_files(name: &str) -> Vec<PathBuf> {
    walkdir::WalkDir::new(fixtures_root())
        .sort_by_file_name()
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .map(|e| e.into_path())
        .filter(|p| {
            p.file_name().is_some_and(|file| file == name)
                && p.to_string_lossy().contains("kimi-home/")
        })
        .collect()
}

fn wire_files() -> Vec<PathBuf> {
    walkdir::WalkDir::new(fixtures_root())
        .sort_by_file_name()
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .map(|e| e.into_path())
        .filter(|p| {
            p.file_name().is_some_and(|name| name == "wire.jsonl")
                && p.to_string_lossy().contains("kimi-home/sessions/")
        })
        .collect()
}

#[test]
fn corpus_exists() {
    assert!(
        !wire_files().is_empty(),
        "no wire fixtures under {} — run: python3 -m tools.groundtruth farm kimi-code",
        fixtures_root().display()
    );
    assert!(
        wire_files()
            .iter()
            .any(|p| p.to_string_lossy().contains("/agents/agent-")),
        "subagent scenario did not harvest agents/agent-N/wire.jsonl"
    );
}

#[test]
fn every_wire_line_parses_without_drift() {
    let mut lines = 0usize;
    let mut unknown_types = Vec::new();
    let mut drift = Vec::new();

    for file in wire_files() {
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
            let parsed = parse_wire_line(line)
                .unwrap_or_else(|e| panic!("{short}:{}: hard parse failure: {e}", no + 1));
            match parsed {
                ParsedLine::Known(rec) => {
                    let paths = rec.drift_paths();
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
fn json_streams_parse_without_drift() {
    let mut drift = Vec::new();

    for file in home_files("state.json") {
        let text = std::fs::read_to_string(&file).unwrap();
        let parsed = parse_session_state(&text)
            .unwrap_or_else(|e| panic!("{}: hard parse failure: {e}", file.display()));
        for path in parsed.drift_paths() {
            drift.push(format!("{}: {path}", file.display()));
        }
    }
    for file in home_files("session_index.jsonl") {
        let text = std::fs::read_to_string(&file).unwrap();
        for (no, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let parsed = parse_session_index_line(line).unwrap_or_else(|e| {
                panic!("{}:{}: hard parse failure: {e}", file.display(), no + 1)
            });
            for path in parsed.drift_paths() {
                drift.push(format!("{}:{}: {path}", file.display(), no + 1));
            }
        }
    }
    for file in home_files("workspaces.json") {
        let text = std::fs::read_to_string(&file).unwrap();
        let parsed = parse_workspaces(&text)
            .unwrap_or_else(|e| panic!("{}: hard parse failure: {e}", file.display()));
        for path in parsed.drift_paths() {
            drift.push(format!("{}: {path}", file.display()));
        }
    }

    assert!(
        drift.is_empty(),
        "unknown fields in JSON streams:\n{}",
        drift.join("\n")
    );
}

#[test]
fn records_round_trip_losslessly() {
    for file in wire_files() {
        let text = std::fs::read_to_string(&file).unwrap();
        for (no, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let original: serde_json::Value = serde_json::from_str(line).unwrap();
            if let ParsedLine::Known(rec) = parse_wire_line(line).unwrap() {
                let back = serde_json::to_value(&*rec).unwrap();
                assert_eq!(
                    back,
                    original,
                    "lossy wire round-trip at {}:{}",
                    file.display(),
                    no + 1
                );
            }
        }
    }
    for file in home_files("state.json") {
        let original: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        let parsed = parse_session_state(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(
            serde_json::to_value(&parsed).unwrap(),
            original,
            "lossy state round-trip at {}",
            file.display()
        );
    }
    for file in home_files("session_index.jsonl") {
        let text = std::fs::read_to_string(&file).unwrap();
        for (no, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let original: serde_json::Value = serde_json::from_str(line).unwrap();
            let parsed = parse_session_index_line(line).unwrap();
            assert_eq!(
                serde_json::to_value(&parsed).unwrap(),
                original,
                "lossy session-index round-trip at {}:{}",
                file.display(),
                no + 1
            );
        }
    }
    for file in home_files("workspaces.json") {
        let original: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        let parsed = parse_workspaces(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(
            serde_json::to_value(&parsed).unwrap(),
            original,
            "lossy workspaces round-trip at {}",
            file.display()
        );
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
        let Some((_, rel)) = path.split_once("kimi-home/") else {
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
                p.split_once("kimi-home/")
                    .is_some_and(|(_, rel)| glob_match(&stream.glob, rel))
            });
        assert!(
            covered,
            "stream `{}` claims Fixture evidence but no fixture file matches `{}`",
            stream.id, stream.glob
        );
    }
}
