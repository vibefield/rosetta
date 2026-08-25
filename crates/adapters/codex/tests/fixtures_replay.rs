//! Fixture replay: every committed rollout line must parse into the raw types
//! with zero drift. This test IS the schema claim — if a fixture regeneration
//! (new agent version) makes it fail, that failure is the drift report.

use rosetta_adapter_codex::raw::{ParsedLine, parse_line};
use rosetta_adapter_support::{Drift, Evidence, FileFormat, GroundTruthDoc, glob_match};
use std::path::{Path, PathBuf};

fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../groundtruth/codex/fixtures")
}

fn ground_truth() -> GroundTruthDoc {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../groundtruth/codex/groundtruth.json");
    GroundTruthDoc::parse(&std::fs::read_to_string(&path).unwrap())
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn rollout_files() -> Vec<PathBuf> {
    walkdir::WalkDir::new(fixtures_root())
        .sort_by_file_name()
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .map(|e| e.into_path())
        .filter(|p| {
            p.extension().is_some_and(|x| x == "jsonl")
                && p.to_string_lossy().contains("codex-home/sessions/")
        })
        .collect()
}

#[test]
fn corpus_exists() {
    assert!(
        !rollout_files().is_empty(),
        "no rollout fixtures under {} — run: python3 -m tools.groundtruth farm codex",
        fixtures_root().display()
    );
}

#[test]
fn every_rollout_line_parses_without_drift() {
    let mut lines = 0usize;
    let mut unknown_types = Vec::new();
    let mut drift = Vec::new();

    for file in rollout_files() {
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
fn rollouts_round_trip_losslessly() {
    // Serialize(parse(line)) must equal the original JSON — proves the flatten
    // maps captured everything and no data is silently dropped.
    for file in rollout_files() {
        let text = std::fs::read_to_string(&file).unwrap();
        for (no, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let original: serde_json::Value = serde_json::from_str(line).unwrap();
            if let ParsedLine::Known(rec) = parse_line(line).unwrap() {
                let back = serde_json::to_value(&*rec).unwrap();
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
    // Every file the agent wrote in the fixtures must match a declared stream —
    // an unmatched file is exactly the UnknownFile drift the manifest exists to
    // catch. (auth.json, .tmp/, and skills/ are stripped at harvest; stdout/
    // stderr/manifest are farm artifacts living outside codex-home.)
    let ground_truth = ground_truth();
    let mut unmatched = Vec::new();
    for entry in walkdir::WalkDir::new(fixtures_root()).sort_by_file_name() {
        let entry = entry.unwrap();
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path().to_string_lossy().into_owned();
        let Some((_, rel)) = path.split_once("codex-home/") else {
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
    // Every stream claiming Evidence::Fixture must actually appear in the corpus.
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
                p.split_once("codex-home/")
                    .is_some_and(|(_, rel)| glob_match(&stream.glob, rel))
            });
        assert!(
            covered,
            "stream `{}` claims Fixture evidence but no fixture file matches `{}`",
            stream.id, stream.glob
        );
    }
    // Sanity: the sqlite stream really is declared Database/Sqlite (the schema
    // dump lane keys off this).
    let dbs = ground_truth.stream("state-dbs").expect("state-dbs stream");
    assert_eq!(dbs.file_format, FileFormat::Sqlite);
}
