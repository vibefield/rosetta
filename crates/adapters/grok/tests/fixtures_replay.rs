//! Fixture replay: every committed primary JSONL line must parse into the raw
//! types with zero drift. This test IS the schema claim — if a fixture
//! regeneration (new agent version) makes it fail, that failure is the drift
//! report.

use rosetta_adapter_grok::raw::{
    ChatHistoryLine, EventLine, UpdateLine, parse_chat_history_line, parse_event_line,
    parse_prompt_history, parse_rewind_point, parse_signals, parse_summary, parse_update_line,
};
use rosetta_adapter_support::{Drift, Evidence, GroundTruthDoc, glob_match};
use std::path::{Path, PathBuf};

fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../groundtruth/grok/fixtures")
}

fn ground_truth() -> GroundTruthDoc {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../groundtruth/grok/groundtruth.json");
    GroundTruthDoc::parse(&std::fs::read_to_string(&path).unwrap())
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn jsonl_files(name: &str) -> Vec<PathBuf> {
    walkdir::WalkDir::new(fixtures_root())
        .sort_by_file_name()
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| entry.into_path())
        .filter(|path| {
            path.file_name().is_some_and(|file| file == name)
                && path.to_string_lossy().contains("grok-home/")
        })
        .collect()
}

enum Parsed {
    Chat(ChatHistoryLine),
    Update(UpdateLine),
    Event(EventLine),
}

fn parse_named(name: &str, line: &str) -> Result<Parsed, serde_json::Error> {
    match name {
        "chat_history.jsonl" => parse_chat_history_line(line).map(Parsed::Chat),
        "updates.jsonl" => parse_update_line(line).map(Parsed::Update),
        "events.jsonl" => parse_event_line(line).map(Parsed::Event),
        _ => unreachable!(),
    }
}

fn drift_of(parsed: &Parsed) -> Vec<String> {
    match parsed {
        Parsed::Chat(ChatHistoryLine::Known(record)) => record.drift_paths(),
        Parsed::Update(UpdateLine::Known(record)) => record.drift_paths(),
        Parsed::Event(EventLine::Known(record)) => record.drift_paths(),
        Parsed::Chat(ChatHistoryLine::UnknownType { rtype, .. })
        | Parsed::Update(UpdateLine::UnknownType { rtype, .. })
        | Parsed::Event(EventLine::UnknownType { rtype, .. }) => {
            vec![format!("unknown type={rtype:?}")]
        }
    }
}

fn round_trip_value(parsed: &Parsed) -> Option<serde_json::Value> {
    match parsed {
        Parsed::Chat(ChatHistoryLine::Known(record)) => serde_json::to_value(record).ok(),
        Parsed::Update(UpdateLine::Known(record)) => serde_json::to_value(record).ok(),
        Parsed::Event(EventLine::Known(record)) => serde_json::to_value(record).ok(),
        _ => None,
    }
}

#[test]
fn corpus_exists() {
    assert!(
        !jsonl_files("chat_history.jsonl").is_empty(),
        "no chat_history fixtures under {} — run: python3 -m tools.groundtruth farm grok",
        fixtures_root().display()
    );
    assert!(!jsonl_files("updates.jsonl").is_empty());
    assert!(!jsonl_files("events.jsonl").is_empty());
}

#[test]
fn every_primary_line_parses_without_drift() {
    let mut lines = 0usize;
    let mut problems = Vec::new();

    for name in ["chat_history.jsonl", "updates.jsonl", "events.jsonl"] {
        for file in jsonl_files(name) {
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
                match parse_named(name, line) {
                    Err(error) => problems.push(format!("{short}:{}: hard parse: {error}", no + 1)),
                    Ok(parsed) => {
                        for path in drift_of(&parsed) {
                            problems.push(format!("{short}:{}: {path}", no + 1));
                        }
                    }
                }
            }
        }
    }

    assert!(lines > 0, "corpus is empty");
    assert!(
        problems.is_empty(),
        "unknown types or fields in committed corpus:\n{}",
        problems.join("\n")
    );
}

#[test]
fn sidecar_documents_parse_without_drift() {
    let mut problems = Vec::new();
    for (name, kind) in [
        ("summary.json", "summary"),
        ("signals.json", "signals"),
        ("rewind_points.jsonl", "rewind"),
        ("prompt_history.jsonl", "prompt-history"),
    ] {
        for file in jsonl_files(name) {
            let text = std::fs::read_to_string(&file).unwrap();
            let short = file
                .strip_prefix(fixtures_root())
                .unwrap()
                .display()
                .to_string();
            if kind == "summary" || kind == "signals" {
                let result = if kind == "summary" {
                    parse_summary(&text).map(|rec| {
                        let drift = rec.drift_paths();
                        let back = serde_json::to_value(&rec).unwrap();
                        (drift, back)
                    })
                } else {
                    parse_signals(&text).map(|rec| {
                        let drift = rec.drift_paths();
                        let back = serde_json::to_value(&rec).unwrap();
                        (drift, back)
                    })
                };
                match result {
                    Err(error) => problems.push(format!("{short}: hard parse: {error}")),
                    Ok((drift, back)) => {
                        for path in drift {
                            problems.push(format!("{short}: {path}"));
                        }
                        let original: serde_json::Value = serde_json::from_str(&text).unwrap();
                        if back != original {
                            problems.push(format!("{short}: lossy round-trip"));
                        }
                    }
                }
                continue;
            }
            for (no, line) in text.lines().enumerate() {
                if line.trim().is_empty() {
                    continue;
                }
                let parsed = if kind == "rewind" {
                    parse_rewind_point(line)
                        .map(|rec| (rec.drift_paths(), serde_json::to_value(&rec).unwrap()))
                } else {
                    parse_prompt_history(line)
                        .map(|rec| (rec.drift_paths(), serde_json::to_value(&rec).unwrap()))
                };
                match parsed {
                    Err(error) => problems.push(format!("{short}:{}: hard parse: {error}", no + 1)),
                    Ok((drift, back)) => {
                        for path in drift {
                            problems.push(format!("{short}:{}: {path}", no + 1));
                        }
                        let original: serde_json::Value = serde_json::from_str(line).unwrap();
                        if back != original {
                            problems.push(format!("{short}:{}: lossy round-trip", no + 1));
                        }
                    }
                }
            }
        }
    }
    assert!(
        problems.is_empty(),
        "sidecar parse problems:\n{}",
        problems.join("\n")
    );
}

#[test]
fn primary_streams_round_trip_losslessly() {
    for name in ["chat_history.jsonl", "updates.jsonl", "events.jsonl"] {
        for file in jsonl_files(name) {
            let text = std::fs::read_to_string(&file).unwrap();
            for (no, line) in text.lines().enumerate() {
                if line.trim().is_empty() {
                    continue;
                }
                let original: serde_json::Value = serde_json::from_str(line).unwrap();
                if let Some(back) = round_trip_value(&parse_named(name, line).unwrap()) {
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
        let Some((_, rel)) = path.split_once("grok-home/") else {
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
                path.split_once("grok-home/")
                    .is_some_and(|(_, rel)| glob_match(&stream.glob, rel))
            });
        assert!(
            covered,
            "stream `{}` claims Fixture evidence but no fixture file matches `{}`",
            stream.id, stream.glob
        );
    }
}
