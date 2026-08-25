//! Raw types for the `session-index` stream (`session_index.jsonl`).
//!
//! Evidence: THE CANONICAL `groundtruth.json` — fixtures at 0.38.0.
//! Homogeneous records; there is no type discriminant.

use rosetta_adapter_support::{Drift, Unknown, note_unknown};
use serde::{Deserialize, Serialize};

/// Parse one `session_index.jsonl` line.
pub fn parse_line(line: &str) -> Result<SessionIndexLine, serde_json::Error> {
    serde_json::from_str(line)
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionIndexLine {
    pub session_id: String,
    pub session_dir: String,
    pub work_dir: String,
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl Drift for SessionIndexLine {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        note_unknown(&self.unknown, prefix, out);
    }
}
