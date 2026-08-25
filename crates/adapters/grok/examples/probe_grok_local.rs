//! Corroboration probe: parse live ~/.grok/sessions through the raw types.
//! Local data corroborates, never proves. Run:
//! `cargo run -p rosetta-adapter-grok --example probe_grok_local`

use rosetta_adapter_grok::raw::{
    ChatHistoryLine, EventLine, UpdateLine, parse_chat_history_line, parse_event_line,
    parse_update_line,
};
use rosetta_adapter_support::Drift;
use std::collections::BTreeMap;
use std::path::PathBuf;

fn main() {
    let root = std::env::var("GROK_HOME")
        .unwrap_or_else(|_| format!("{}/.grok", std::env::var("HOME").expect("HOME")));
    let sessions = PathBuf::from(root).join("sessions");

    let mut lines = 0u64;
    let mut hard_failures = 0u64;
    let mut hard_samples: Vec<String> = Vec::new();
    let mut unknown_types: BTreeMap<String, u64> = BTreeMap::new();
    let mut drift_paths: BTreeMap<String, u64> = BTreeMap::new();

    let mut stack = vec![sessions];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let kind = match name {
                "chat_history.jsonl" => Some("chat"),
                "updates.jsonl" => Some("updates"),
                "events.jsonl" => Some("events"),
                _ => None,
            };
            let Some(kind) = kind else {
                continue;
            };
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            for line in text.lines().filter(|l| !l.trim().is_empty()) {
                lines += 1;
                let parsed = match kind {
                    "chat" => parse_chat_history_line(line).map(|p| match p {
                        ChatHistoryLine::Known(rec) => Probe::Known(rec.drift_paths()),
                        ChatHistoryLine::UnknownType { rtype, .. } => Probe::Unknown(rtype),
                    }),
                    "updates" => parse_update_line(line).map(|p| match p {
                        UpdateLine::Known(rec) => Probe::Known(rec.drift_paths()),
                        UpdateLine::UnknownType { rtype, .. } => Probe::Unknown(rtype),
                    }),
                    _ => parse_event_line(line).map(|p| match p {
                        EventLine::Known(rec) => Probe::Known(rec.drift_paths()),
                        EventLine::UnknownType { rtype, .. } => Probe::Unknown(rtype),
                    }),
                };
                match parsed {
                    Err(error) => {
                        hard_failures += 1;
                        if hard_samples.len() < 10 {
                            hard_samples.push(format!("{}: {error}", path.display()));
                        }
                    }
                    Ok(Probe::Unknown(rtype)) => {
                        *unknown_types
                            .entry(rtype.unwrap_or_else(|| "<none>".into()))
                            .or_default() += 1;
                    }
                    Ok(Probe::Known(paths)) => {
                        for path_name in paths {
                            *drift_paths.entry(path_name).or_default() += 1;
                        }
                    }
                }
            }
        }
    }

    println!("lines: {lines}");
    println!("hard failures: {hard_failures}");
    for sample in &hard_samples {
        println!("  {sample}");
    }
    println!("unknown record types:");
    for (rtype, count) in &unknown_types {
        println!("  {rtype}: {count}");
    }
    println!("drift paths on known types (top 40):");
    let mut drift: Vec<_> = drift_paths.into_iter().collect();
    drift.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
    for (path, count) in drift.into_iter().take(40) {
        println!("  {path}: {count}");
    }
}

enum Probe {
    Known(Vec<String>),
    Unknown(Option<String>),
}
