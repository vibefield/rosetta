//! rosetta-core — the canonical model: row types mirroring the projection DDL,
//! the [`IngestOp`] stream every adapter normalizes into, deterministic ids,
//! and the pure fold helpers. Zero IO dependencies by design; every function
//! here benches and tests in isolation (docs/ingest-engine.html).

pub mod assets;
pub mod fold;
pub mod ids;
pub mod ops;
pub mod rows;
pub mod timeline;

pub use assets::{ImageAsset, ProjectionAsset};
pub use ops::IngestOp;
pub use rows::{
    DriftFact, EditKind, FileEditRow, GitCommitRow, MessageKind, MessageRow, Ms, PlanRow,
    ProjectRow, RuntimeRow, SessionDelegation, SessionRole, SessionRow, SessionTitleRow,
    SessionUsageRow, StreamState, TaskRow, TaskStatus, TitleKind, TokenSnapshot, ToolCall,
};
pub use timeline::{
    CanonicalError, CanonicalText, CollaborationChannel, CollaborationEvent,
    CollaborationEventKind, CollaborationStatus, CorrelationDerivation, EvidenceOrder, FailureKind,
    GroupDerivation, GroupKind, MessageDetail, NativeToolRef, ParticipantColor, Provenance,
    ResponseGroup, SearchLanes, SourceIdentity, StatusBasis, TaskSnapshot, TaskSnapshotItem,
    TimelineBody, TimelineEntry, TimelineGroup, ToolAction, ToolActionKind, ToolCorrelation,
    ToolFailure, ToolFoldState, ToolInteraction, ToolOutcomePatch, ToolRequestPatch, ToolStatus,
    ToolStatusFact, ToolTiming, ToolUpdatePatch, WorkflowJournalKind, WorkflowJournalObservation,
    WorkflowPhase, WorkflowResultKind, WorkflowRun, WorkflowRunStatus, WorkflowUsage,
    WorkflowWorker, WorkflowWorkerState,
};
