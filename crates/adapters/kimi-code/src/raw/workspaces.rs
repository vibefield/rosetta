//! Raw types for the `workspaces` stream (`workspaces.json`).
//!
//! Evidence: THE CANONICAL `groundtruth.json` — fixtures at 0.38.0.
//! `workspaces` is keyed by `workDirKey` (`wd_<slug>_<sha256-12>`).

use std::collections::BTreeMap;

use rosetta_adapter_support::{Drift, Unknown, note_unknown};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Parse a `workspaces.json` document.
pub fn parse_document(text: &str) -> Result<WorkspacesFile, serde_json::Error> {
    serde_json::from_str(text)
}

#[derive(Debug, Deserialize, Serialize)]
pub struct WorkspacesFile {
    pub version: u64,
    pub workspaces: BTreeMap<String, WorkspaceEntry>,
    pub deleted_workspace_ids: Vec<Value>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl Drift for WorkspacesFile {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        note_unknown(&self.unknown, prefix, out);
        for (key, entry) in &self.workspaces {
            entry.drift(&format!("{prefix}.workspaces.{key}"), out);
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct WorkspaceEntry {
    pub root: String,
    pub name: String,
    pub created_at: String,
    pub last_opened_at: String,
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl Drift for WorkspaceEntry {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        note_unknown(&self.unknown, prefix, out);
    }
}
