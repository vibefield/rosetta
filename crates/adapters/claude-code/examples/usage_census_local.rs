//! Corroboration census: replay every transcript in a live `~/.claude` tree
//! (or `$CLAUDE_CONFIG_DIR`) through the adapter and count where each
//! response's usage lands — rows, `response:` samples, retractions,
//! unrecorded output, cross-file identities. Numbers only, never content.
//! Local data corroborates, never proves (CLAUDE.md rule #3).
//!
//! Run: cargo run -p rosetta-adapter-claude-code --example usage_census_local

use std::collections::{BTreeMap, BTreeSet};

use rosetta_adapter_claude_code::ClaudeCodeAdapter;
use rosetta_adapter_support::{AgentAdapter, NormalizeCtx};
use rosetta_core::rows::TokenSnapshot;
use rosetta_core::{IngestOp, ids};

#[derive(Default)]
struct Census {
    files: u64,
    lines: u64,
    usage_ops: u64,
    restated_ops: u64,
    row_facts: u64,
    row_facts_output_unrecorded: u64,
    row_tokens: i64,
    response_samples: u64,
    response_sample_tokens: i64,
    timeline_samples: u64,
    iteration_samples: u64,
    retractions: u64,
    facts_without_identity: u64,
}

fn total(tokens: &TokenSnapshot) -> i64 {
    tokens.total_tokens().unwrap_or(0)
}

fn main() {
    let root = std::env::var("CLAUDE_CONFIG_DIR")
        .unwrap_or_else(|_| format!("{}/.claude", std::env::var("HOME").expect("HOME")));
    let projects = std::path::Path::new(&root).join("projects");
    let adapter = ClaudeCodeAdapter;
    let runtime_id = ids::runtime_id(adapter.kind(), &adapter.layout().data_root);

    let mut census = Census::default();
    // identity -> files it was emitted from (row or sample facts only).
    let mut identity_files: BTreeMap<String, BTreeSet<u64>> = BTreeMap::new();
    let mut stack = vec![projects];
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
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let text = path.to_string_lossy();
            let stream = if text.contains("/subagents/") {
                if name.starts_with("agent-") && name.ends_with(".jsonl") {
                    "subagent-transcript"
                } else {
                    continue;
                }
            } else if name.ends_with(".jsonl") {
                "transcript"
            } else {
                continue;
            };
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            census.files += 1;
            let file_index = census.files;
            let native = name.trim_end_matches(".jsonl");
            let mut ctx = NormalizeCtx::new(&runtime_id, ids::session_id(&runtime_id, native));
            ctx.state = adapter.init_state();
            let mut rows: BTreeMap<String, TokenSnapshot> = BTreeMap::new();
            let mut samples: BTreeMap<String, TokenSnapshot> = BTreeMap::new();
            for line in text.lines().filter(|l| !l.trim().is_empty()) {
                census.lines += 1;
                let outcome = adapter.ingest_line(&mut ctx, stream, line.as_bytes());
                for op in outcome.ops {
                    match op {
                        IngestOp::Usage {
                            response,
                            tokens,
                            identity,
                            ..
                        } => {
                            census.usage_ops += 1;
                            if rows.get(&response) == Some(&tokens) {
                                census.restated_ops += 1;
                            }
                            rows.insert(response, tokens);
                            match identity {
                                Some(identity) => {
                                    identity_files
                                        .entry(identity)
                                        .or_default()
                                        .insert(file_index);
                                }
                                None => census.facts_without_identity += 1,
                            }
                        }
                        IngestOp::SessionUsage(sample) => {
                            if sample.tokens.is_empty() {
                                census.retractions += 1;
                            } else if let Some(identity) = &sample.identity {
                                identity_files
                                    .entry(identity.clone())
                                    .or_default()
                                    .insert(file_index);
                            } else {
                                census.facts_without_identity += 1;
                            }
                            samples.insert(sample.key, sample.tokens);
                        }
                        _ => {}
                    }
                }
            }
            for tokens in rows.values() {
                census.row_facts += 1;
                census.row_tokens += total(tokens);
                if tokens.output.is_none() {
                    census.row_facts_output_unrecorded += 1;
                }
            }
            for (key, tokens) in &samples {
                if tokens.is_empty() {
                    continue;
                }
                if key.contains(":iteration:") {
                    census.iteration_samples += 1;
                } else if key.starts_with("response:") {
                    census.response_samples += 1;
                    census.response_sample_tokens += total(tokens);
                } else {
                    census.timeline_samples += 1;
                }
            }
        }
    }
    let copied = identity_files
        .values()
        .filter(|files| files.len() > 1)
        .count();
    let extra_copies: usize = identity_files
        .values()
        .filter(|files| files.len() > 1)
        .map(|files| files.len() - 1)
        .sum();
    println!("files: {}", census.files);
    println!("lines: {}", census.lines);
    println!("usage ops to rows: {}", census.usage_ops);
    println!(
        "  of which re-statements of the same fact: {}",
        census.restated_ops
    );
    println!("responses whose usage rests on a row: {}", census.row_facts);
    println!(
        "  with unrecorded output: {}",
        census.row_facts_output_unrecorded
    );
    println!("  tokens: {}", census.row_tokens);
    println!(
        "responses whose usage rests on a response: sample: {}",
        census.response_samples
    );
    println!("  tokens: {}", census.response_sample_tokens);
    println!(
        "timeline: samples (dropped owners): {}",
        census.timeline_samples
    );
    println!("iteration samples: {}", census.iteration_samples);
    println!("retractions emitted: {}", census.retractions);
    println!("facts without identity: {}", census.facts_without_identity);
    println!("distinct identities: {}", identity_files.len());
    println!("identities emitted from more than one file: {copied} (extra copies: {extra_copies})");
}
