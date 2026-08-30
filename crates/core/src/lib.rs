//! rosetta-core — the canonical model: row types mirroring the projection DDL,
//! the [`IngestOp`] stream every adapter normalizes into, deterministic ids,
//! and the pure fold helpers. Zero IO dependencies by design; every function
//! here benches and tests in isolation (docs/ingest-engine.html).

pub mod assets;
pub mod fold;
pub mod ids;
pub mod ops;
pub mod rows;

pub use assets::ImageAsset;
pub use ops::IngestOp;
pub use rows::{
    DriftFact, EditKind, FileEditRow, GitCommitRow, MessageKind, MessageRow, Ms, PlanRow,
    ProjectRow, RuntimeRow, SessionRole, SessionRow, SessionTitleRow, SessionUsageRow, StreamState,
    TaskRow, TaskStatus, TitleKind, TokenSnapshot, ToolCall, WorkspaceObsRow,
};
