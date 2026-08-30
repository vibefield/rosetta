//! The central op type: adapters end here, the writer starts here. All
//! agent-specific weirdness stops at this boundary (docs/ingest-engine.html).

use serde::{Deserialize, Serialize};

use crate::assets::ImageAsset;
use crate::rows::{
    DriftFact, FileEditRow, GitCommitRow, MessageRow, Ms, PlanRow, ProjectRow, RuntimeRow,
    SessionRow, SessionTitleRow, SessionUsageRow, TaskRow, TokenSnapshot, WorkspaceObsRow,
};
use crate::timeline::ToolUpdatePatch;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum IngestOp {
    /// A large decoded image that belongs to the canonical projection but is
    /// materialized outside SQLite. The matching tool payload carries its
    /// stable `/api/assets/<digest>.<ext>` URL.
    ImageAsset {
        asset: ImageAsset,
    },
    Runtime(RuntimeRow),
    Project(ProjectRow),
    /// Meta-phase upsert; stream-phase ops for the same session refine it.
    SessionMeta(SessionRow),
    /// Ranked title fact recovered during bounded metadata admission.
    SessionTitle(SessionTitleRow),
    /// One flattened timeline row (upsert by timeline_id). Boxed: it is by
    /// far the largest variant and ops move through channels in bulk.
    Message(Box<MessageRow>),
    /// A typed merge op, not a row: folds into its tool_use row by the
    /// adapter's deterministic correlation key.  Native result bodies never
    /// cross this boundary.
    ToolUpdate {
        session_id: String,
        correlation_key: String,
        patch: ToolUpdatePatch,
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
    /// Usage that the native agent reports only at session/task granularity.
    /// It is an additive canonical sample, not a synthetic response message.
    SessionUsage(SessionUsageRow),
    WorkspaceObs(WorkspaceObsRow),
    FileEdit(FileEditRow),
    Plan(PlanRow),
    Task(TaskRow),
    GitCommit(GitCommitRow),
    /// Lands aggregated in drift_agg — never one row per bad record.
    Drift(DriftFact),
}
