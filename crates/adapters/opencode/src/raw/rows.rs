//! Raw types for SQLite *rows* (`session`, `project`).
//!
//! JSON TEXT columns (`model`, `permission`, `sandboxes`, …) are parsed into
//! values before these structs are deserialized. SQL NULL becomes JSON null
//! and is modeled as `Option` without `skip_serializing_if`, so round-trip
//! preserves null vs absent.

use rosetta_adapter_support::{Drift, Unknown, note_unknown};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::event::SessionModel;

#[derive(Debug)]
pub enum ParsedSession {
    Known(Box<SessionRow>),
}

#[derive(Debug)]
pub enum ParsedProject {
    Known(Box<ProjectRow>),
}

pub fn parse_session(value: Value) -> Result<ParsedSession, serde_json::Error> {
    Ok(ParsedSession::Known(Box::new(serde_json::from_value(
        value,
    )?)))
}

pub fn parse_project(value: Value) -> Result<ParsedProject, serde_json::Error> {
    Ok(ParsedProject::Known(Box::new(serde_json::from_value(
        value,
    )?)))
}

#[derive(Debug, Deserialize, Serialize)]
pub struct SessionRow {
    pub id: String,
    pub project_id: String,
    pub workspace_id: Option<String>,
    pub parent_id: Option<String>,
    pub slug: String,
    pub directory: String,
    pub path: Option<String>,
    pub title: String,
    pub version: String,
    pub share_url: Option<String>,
    pub summary_additions: Option<i64>,
    pub summary_deletions: Option<i64>,
    pub summary_files: Option<i64>,
    pub summary_diffs: Option<Value>,
    pub metadata: Option<Value>,
    pub cost: serde_json::Number,
    pub tokens_input: i64,
    pub tokens_output: i64,
    pub tokens_reasoning: i64,
    pub tokens_cache_read: i64,
    pub tokens_cache_write: i64,
    pub revert: Option<Value>,
    pub permission: Option<Value>,
    pub agent: Option<String>,
    pub model: Option<SessionModel>,
    pub time_created: i64,
    pub time_updated: i64,
    pub time_compacting: Option<i64>,
    pub time_archived: Option<i64>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl Drift for SessionRow {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        note_unknown(&self.unknown, prefix, out);
        if let Some(model) = &self.model {
            note_unknown(&model.unknown, &format!("{prefix}.model"), out);
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ProjectRow {
    pub id: String,
    pub worktree: String,
    pub vcs: Option<String>,
    pub name: Option<String>,
    pub icon_url: Option<String>,
    pub icon_url_override: Option<String>,
    pub icon_color: Option<String>,
    pub time_created: i64,
    pub time_updated: i64,
    pub time_initialized: Option<i64>,
    pub sandboxes: Value,
    pub commands: Option<Value>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl Drift for ProjectRow {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        note_unknown(&self.unknown, prefix, out);
    }
}
