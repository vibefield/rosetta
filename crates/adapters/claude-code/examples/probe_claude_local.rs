//! Corroboration probe: parse every transcript line in a live ~/.claude tree
//! (or $CLAUDE_CONFIG_DIR) through the raw types and report totals — hard
//! failures, unknown record types, drift paths. Local data corroborates, never
//! proves (CLAUDE.md rule #3); this tool exists to answer "would the raw types
//! survive real data?" without committing anything.
//!
//! Run: cargo run -p rosetta-adapter-claude-code --example probe_claude_local

use rosetta_adapter_claude_code::raw::{ParsedLine, parse_line};
use rosetta_adapter_support::Drift;
use std::collections::BTreeMap;

fn main() {
    let root = std::env::var("CLAUDE_CONFIG_DIR")
        .unwrap_or_else(|_| format!("{}/.claude", std::env::var("HOME").expect("HOME")));
    let projects = std::path::Path::new(&root).join("projects");

    let mut lines = 0u64;
    let mut hard_failures = 0u64;
    let mut hard_samples: Vec<String> = Vec::new();
    let mut unknown_types: BTreeMap<String, u64> = BTreeMap::new();
    let mut drift_paths: BTreeMap<String, u64> = BTreeMap::new();

    let mut stack = vec![projects];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "jsonl") {
                let Ok(text) = std::fs::read_to_string(&path) else {
                    continue;
                };
                for line in text.lines().filter(|l| !l.trim().is_empty()) {
                    lines += 1;
                    match parse_line(line) {
                        Err(e) => {
                            hard_failures += 1;
                            if hard_samples.len() < 10 {
                                hard_samples.push(format!("{}: {e}", path.display()));
                            }
                        }
                        Ok(ParsedLine::UnknownType { rtype, .. }) => {
                            *unknown_types
                                .entry(rtype.unwrap_or_else(|| "<none>".into()))
                                .or_default() += 1;
                        }
                        Ok(ParsedLine::Known(rec)) => {
                            for p in rec.drift_paths() {
                                *drift_paths.entry(p).or_default() += 1;
                            }
                        }
                    }
                }
            }
        }
    }

    println!("lines: {lines}");
    println!("hard failures: {hard_failures}");
    for s in &hard_samples {
        println!("  {s}");
    }
    println!("unknown record types (soft, expected for untyped kinds):");
    for (t, n) in &unknown_types {
        println!("  {t}: {n}");
    }
    println!("drift paths on KNOWN types (top 40):");
    let mut d: Vec<_> = drift_paths.into_iter().collect();
    d.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    for (p, n) in d.into_iter().take(40) {
        println!("  {p}: {n}");
    }
}
