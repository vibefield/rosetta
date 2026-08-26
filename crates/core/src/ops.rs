//! The central op type: adapters end here, the writer starts here. All
//! agent-specific weirdness stops at this boundary (docs/ingest-engine.html).

use serde::{Deserialize, Serialize};

use crate::rows::{
    DriftFact, FileEditRow, GitCommitRow, MessageRow, Ms, PlanRow, ProjectRow, RuntimeRow,
    SessionRow, TaskRow, TokenSnapshot, WorkspaceObsRow,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum IngestOp {
    Runtime(RuntimeRow),
    Project(ProjectRow),
    /// Meta-phase upsert; stream-phase ops for the same session refine it.
    SessionMeta(SessionRow),
    /// One flattened timeline row (upsert by timeline_id). Boxed: it is by
    /// far the largest variant and ops move through channels in bulk.
    Message(Box<MessageRow>),
    /// A merge op, not a row: folds into its tool_use row by (session, tool id).
    ToolResult {
        session_id: String,
        tool_id: String,
        result: Option<String>,
        payload: Option<serde_json::Value>,
        is_error: Option<bool>,
        at: Option<Ms>,
    },
    /// Replacement-keyed usage snapshot for a response row that already exists
    /// (e.g. codex token_count events correcting the prior assistant row).
    /// The writer applies (new − old) to the hour bucket; absent components
    /// stay `None` — never coerced to zero.
    Usage {
        session_id: String,
        /// timeline_id of the response row this snapshot belongs to.
        response: String,
        ts: Ms,
        model: Option<String>,
        tokens: TokenSnapshot,
    },
    WorkspaceObs(WorkspaceObsRow),
    FileEdit(FileEditRow),
    Plan(PlanRow),
    Task(TaskRow),
    GitCommit(GitCommitRow),
    /// Lands aggregated in drift_agg — never one row per bad record.
    Drift(DriftFact),
}
