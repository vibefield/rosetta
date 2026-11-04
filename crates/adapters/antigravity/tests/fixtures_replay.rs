//! Fixture replay: every committed transcript line must parse into the raw types
//! with zero drift. This test IS the schema claim — if a fixture regeneration
//! (new agent version) makes it fail, that failure is the drift report.

use rosetta_adapter_antigravity::raw::{ParsedLine, parse_line};
use rosetta_adapter_support::{Drift, Evidence, GroundTruthDoc, glob_match};
use std::path::{Path, PathBuf};

fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../groundtruth/antigravity/fixtures")
}

fn ground_truth() -> GroundTruthDoc {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../groundtruth/antigravity/groundtruth.json");
    GroundTruthDoc::parse(&std::fs::read_to_string(&path).unwrap())
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn transcript_files() -> Vec<PathBuf> {
    walkdir::WalkDir::new(fixtures_root())
        .sort_by_file_name()
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| entry.into_path())
        .filter(|path| {
            path.extension().is_some_and(|ext| ext == "jsonl")
                && path.to_string_lossy().contains("antigravity-cli/brain/")
                && path.to_string_lossy().contains("/.system_generated/logs/")
        })
        .collect()
}

fn primary_transcript_files() -> Vec<PathBuf> {
    transcript_files()
        .into_iter()
        .filter(|path| {
            path.to_string_lossy()
                .ends_with("/.system_generated/logs/transcript.jsonl")
        })
        .collect()
}

#[test]
fn corpus_exists() {
    assert!(
        !transcript_files().is_empty(),
        "no transcript fixtures under {} — run: python3 -m tools.groundtruth farm antigravity",
        fixtures_root().display()
    );
}

#[test]
fn every_transcript_line_parses_without_drift() {
    let mut lines = 0usize;
    let mut unknown_types = Vec::new();
    let mut drift = Vec::new();

    for file in transcript_files() {
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
fn transcripts_round_trip_losslessly() {
    for file in transcript_files() {
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
fn overlapping_transcript_representations_are_exact_copies() {
    // This is the committed ownership evidence. There is one chunk generation
    // (`00000000`) in each family and it reproduces its monolithic file byte
    // for byte. The corpus therefore establishes duplication, but no later
    // rotation boundary or truncation behavior.
    for primary in primary_transcript_files() {
        let logs = primary.parent().expect("logs directory");
        for (monolith, chunk) in [
            ("transcript.jsonl", "chunks/transcript/00000000.jsonl"),
            (
                "transcript_full.jsonl",
                "chunks/transcript_full/00000000.jsonl",
            ),
        ] {
            assert_eq!(
                std::fs::read(logs.join(chunk)).unwrap(),
                std::fs::read(logs.join(monolith)).unwrap(),
                "{chunk} differs from {monolith} in {}",
                logs.display()
            );
        }

        for family in ["transcript", "transcript_full"] {
            let directory = logs.join("chunks").join(family);
            let mut names: Vec<_> = std::fs::read_dir(&directory)
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect();
            names.sort();
            assert_eq!(
                names,
                [std::ffi::OsString::from("00000000.jsonl")],
                "committed evidence unexpectedly gained a rotation in {}",
                directory.display()
            );
        }
    }
}

/// `transcript_full.jsonl` carries the primary's records with one difference:
/// the primary stores every `tool_calls[].args` value as its JSON encoding
/// (1.4.12: `"CommandLine":"\"echo x\""`, `"WaitMsBeforeAsync":"5000"`), the
/// full file stores the value itself. Without tool calls, and in every 1.1.20
/// fixture (whose primary does not encode its args), the two are byte-identical.
#[test]
fn transcript_full_is_the_primary_with_typed_tool_args() {
    let mut decoded_args = 0usize;
    for primary in primary_transcript_files() {
        let full = primary.with_file_name("transcript_full.jsonl");
        let primary_text = std::fs::read_to_string(&primary).unwrap();
        let full_text = std::fs::read_to_string(&full).unwrap();
        let primary_lines: Vec<_> = primary_text.lines().collect();
        let full_lines: Vec<_> = full_text.lines().collect();
        assert_eq!(
            primary_lines.len(),
            full_lines.len(),
            "{}",
            primary.display()
        );
        for (no, (encoded, typed)) in primary_lines.iter().zip(&full_lines).enumerate() {
            if encoded == typed {
                continue;
            }
            let mut encoded: serde_json::Value = serde_json::from_str(encoded).unwrap();
            let typed: serde_json::Value = serde_json::from_str(typed).unwrap();
            let calls = encoded["tool_calls"].as_array_mut().unwrap_or_else(|| {
                panic!(
                    "{}:{}: differs without tool calls",
                    primary.display(),
                    no + 1
                )
            });
            for call in calls {
                for value in call["args"]
                    .as_object_mut()
                    .expect("args object")
                    .values_mut()
                {
                    let text = value
                        .as_str()
                        .expect("primary arg is a JSON-encoded string");
                    *value = serde_json::from_str(text).expect("arg decodes as JSON");
                    decoded_args += 1;
                }
            }
            assert_eq!(
                encoded,
                typed,
                "{}:{}: differs beyond tool-call arg encoding",
                primary.display(),
                no + 1
            );
        }
    }
    assert!(
        decoded_args > 0,
        "the corpus no longer exercises encoded args"
    );
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
        let Some((_, rel)) = path.split_once("antigravity-cli/") else {
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
                path.split_once("antigravity-cli/")
                    .is_some_and(|(_, rel)| glob_match(&stream.glob, rel))
            });
        assert!(
            covered,
            "stream `{}` claims Fixture evidence but no fixture file matches `{}`",
            stream.id, stream.glob
        );
    }
}
