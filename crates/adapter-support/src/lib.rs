//! Small shared primitives for agent parsers and their fixture replay tests.
//!
//! Ground-truth document construction is intentionally implemented by the
//! Python tooling under `tools/groundtruth`; this crate contains only the
//! pieces that production Rust parsers need.

pub mod adapter;
pub mod time;

pub use adapter::{
    AgentAdapter, FileStat, LineOutcome, LineStatus, NormalizeCtx, SkimResult, SnapshotPlan,
    SnapshotQuery, StreamDriver,
};
pub use time::rfc3339_ms;

use std::collections::BTreeMap;

use serde::Deserialize;

/// Flatten target for permissive raw records. Unknown fields are drift, not a
/// parse failure.
pub type Unknown = BTreeMap<String, serde_json::Value>;

/// Collect unknown-field paths from a parsed raw record.
pub trait Drift {
    fn drift(&self, prefix: &str, out: &mut Vec<String>);

    fn drift_paths(&self) -> Vec<String> {
        let mut out = Vec::new();
        self.drift("", &mut out);
        out
    }
}

/// Append every key in an [`Unknown`] map as `<prefix>.<key>`.
pub fn note_unknown(unknown: &Unknown, prefix: &str, out: &mut Vec<String>) {
    for key in unknown.keys() {
        out.push(format!("{prefix}.{key}"));
    }
}

/// The layout subset of a canonical `groundtruth.json` used by replay tests.
#[derive(Debug, Deserialize)]
pub struct GroundTruthDoc {
    pub agent: String,
    pub data_root: String,
    pub streams: Vec<StreamSpec>,
}

#[derive(Debug, Deserialize)]
pub struct StreamSpec {
    pub id: String,
    pub glob: String,
    pub file_format: FileFormat,
    pub discipline: FileDiscipline,
    pub evidence: Evidence,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileFormat {
    Jsonl,
    Json,
    Toml,
    Markdown,
    Sqlite,
    Text,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileDiscipline {
    AppendOnly,
    RewriteInPlace,
    Database,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Evidence {
    Writer,
    Fixture,
    Observed,
    Research,
}

impl GroundTruthDoc {
    pub fn parse(text: &str) -> serde_json::Result<Self> {
        serde_json::from_str(text)
    }

    pub fn stream(&self, id: &str) -> Option<&StreamSpec> {
        self.streams.iter().find(|stream| stream.id == id)
    }

    pub fn matching_stream(&self, relative_path: &str) -> Option<&StreamSpec> {
        self.streams
            .iter()
            .find(|stream| glob_match(&stream.glob, relative_path))
    }
}

/// Match `/`-separated paths using the ground-truth layout's small glob
/// language: `*` within a segment and `**` across segments.
pub fn glob_match(pattern: &str, path: &str) -> bool {
    let pattern: Vec<&str> = pattern.split('/').collect();
    let path: Vec<&str> = path.split('/').collect();
    match_segments(&pattern, &path)
}

fn match_segments(pattern: &[&str], path: &[&str]) -> bool {
    match (pattern.first(), path.first()) {
        (None, None) => true,
        (Some(&"**"), _) => {
            match_segments(&pattern[1..], path)
                || (!path.is_empty() && match_segments(pattern, &path[1..]))
        }
        (Some(head), Some(segment)) => {
            match_segment(head, segment) && match_segments(&pattern[1..], &path[1..])
        }
        _ => false,
    }
}

fn match_segment(pattern: &str, segment: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == segment;
    }
    if !segment.starts_with(parts[0]) || !segment.ends_with(parts.last().copied().unwrap_or("")) {
        return false;
    }

    let mut cursor = parts[0].len();
    let final_part = parts.len() - 1;
    for part in &parts[1..final_part] {
        let Some(offset) = segment[cursor..].find(part) else {
            return false;
        };
        cursor += offset + part.len();
    }
    cursor <= segment.len() - parts[final_part].len()
}

#[cfg(test)]
mod tests {
    use super::glob_match;

    #[test]
    fn layout_globs() {
        assert!(glob_match("projects/*/*.jsonl", "projects/p/a.jsonl"));
        assert!(!glob_match("projects/*/*.jsonl", "projects/a.jsonl"));
        assert!(glob_match(
            "projects/*/*/subagents/**/agent-*.jsonl",
            "projects/p/s/subagents/x/y/agent-7.jsonl"
        ));
        assert!(glob_match("**/*.jsonl", "a/b/c.jsonl"));
        assert!(glob_match("**/*.jsonl", "c.jsonl"));
        assert!(glob_match("todos/*-agent-*.json", "todos/1-agent-2.json"));
        assert!(!glob_match("agent-*-*.jsonl", "agent-.jsonl"));
    }
}
