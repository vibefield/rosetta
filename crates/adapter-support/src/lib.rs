//! Small shared primitives for agent parsers and their fixture replay tests.
//!
//! Ground-truth document construction is intentionally implemented by the
//! Python tooling under `tools/groundtruth`; this crate contains only the
//! pieces that production Rust parsers need.

pub mod adapter;
mod glob;
pub mod media;
pub mod time;
pub mod tool;

pub use adapter::{
    AgentAdapter, FileStat, LineOutcome, LineStatus, NormalizeCtx, ProjectHint, SkimResult,
    SnapshotPlan, SnapshotQuery, StreamDriver,
};
pub use glob::Glob;
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
#[serde(from = "StreamFields")]
pub struct StreamSpec {
    pub id: String,
    pub glob: String,
    pub file_format: FileFormat,
    pub discipline: FileDiscipline,
    pub evidence: Evidence,
    /// `glob`, compiled once when the layout loads.
    pattern: Glob,
}

#[derive(Deserialize)]
struct StreamFields {
    id: String,
    glob: String,
    file_format: FileFormat,
    discipline: FileDiscipline,
    evidence: Evidence,
}

impl From<StreamFields> for StreamSpec {
    fn from(fields: StreamFields) -> Self {
        Self {
            pattern: Glob::new(&fields.glob),
            id: fields.id,
            glob: fields.glob,
            file_format: fields.file_format,
            discipline: fields.discipline,
            evidence: fields.evidence,
        }
    }
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

    /// The first stream whose glob claims `relative_path`.
    pub fn matching_stream(&self, relative_path: &str) -> Option<&StreamSpec> {
        self.streams
            .iter()
            .find(|stream| stream.pattern.matches(relative_path))
    }

    /// Whether any stream `accept` admits could claim a file somewhere below
    /// the directory `relative_dir`. Walks skip every other subtree, so a
    /// layout prunes exactly what its own globs can never reach.
    pub fn may_claim_below(
        &self,
        relative_dir: &str,
        accept: impl Fn(&StreamSpec) -> bool,
    ) -> bool {
        self.streams
            .iter()
            .any(|stream| accept(stream) && stream.pattern.may_match_below(relative_dir))
    }
}

/// Match `/`-separated paths using the ground-truth layout's small glob
/// language: `*` within a segment and `**` across segments. Compiles the
/// pattern on every call; layouts keep theirs compiled (see [`Glob`]).
pub fn glob_match(pattern: &str, path: &str) -> bool {
    Glob::new(pattern).matches(path)
}
