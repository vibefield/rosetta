//! Closed canonical timeline and tool types.
//!
//! Native agent payloads stop before this module.  These types contain only
//! reviewed, user-facing facts and are safe to serialize into the disposable
//! projection and the public protocol.  In particular there is deliberately
//! no `serde_json::Value`, arbitrary map, or generic payload variant here.

use serde::{Deserialize, Serialize};

use crate::rows::{Ms, TaskStatus, TokenSnapshot};

pub const INLINE_TEXT_MAX_BYTES: usize = 32 * 1024;
pub const TEXT_PREVIEW_MAX_BYTES: usize = 2 * 1024;
pub const CORE_SCALAR_MAX_BYTES: usize = 16 * 1024;
pub const ID_MAX_BYTES: usize = 512;
pub const FAILURE_SUMMARY_MAX_BYTES: usize = 512;
pub const FAILURE_DIAGNOSTIC_MAX_BYTES: usize = 4 * 1024;
pub const FAILURE_DIAGNOSTIC_MAX_LINES: usize = 40;
pub const SEARCH_PATH_LIMIT: usize = 20;
pub const WEB_SOURCE_LIMIT: usize = 10;
pub const CITATION_RANGE_LIMIT: usize = 40;
pub const EXTERNAL_FIELD_LIMIT: usize = 24;

macro_rules! wire_enum {
    ($name:ident { $($variant:ident),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        #[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
        pub enum $name { $($variant),+ }
    };
}

wire_enum!(GroupKind {
    HumanTurn,
    AgentCycle,
    ParentInvocation,
    SystemCycle,
    Synthetic,
});
wire_enum!(GroupDerivation {
    Native,
    ParentLinked,
    Ordered,
    Synthetic,
});
wire_enum!(CorrelationDerivation {
    NativeId,
    ParentLinked,
    Positional,
    Synthetic,
});
wire_enum!(ToolStatus {
    Pending,
    Running,
    Succeeded,
    Failed,
    Denied,
    Cancelled,
    TimedOut,
    Interrupted,
    Unknown,
});
wire_enum!(StatusBasis {
    RequestObserved,
    NativeState,
    NativeTerminalRecord,
    GroundTruthProtocol,
    SourceFinalization,
});
wire_enum!(FailureKind {
    Permission,
    NotFound,
    InvalidInput,
    ExitNonZero,
    Timeout,
    Cancelled,
    Provider,
    Unknown,
});
wire_enum!(SummaryPurpose {
    Compaction,
    Branch,
    Handoff,
    Session,
});
wire_enum!(AttachmentKind {
    Image,
    Document,
    Audio,
    Video,
    File,
});
wire_enum!(RangeUnit { Line, Page });
wire_enum!(FileSearchMode {
    Name,
    Content,
    Glob,
    Regex,
    Mixed,
});
wire_enum!(SearchFilterKind {
    Glob,
    FileType,
    Language,
    Exclude
});
wire_enum!(CodeLookupOp {
    Definition,
    References,
    Symbols,
    Diagnostics,
    Hover,
});
wire_enum!(FileChangeOp {
    Create,
    Update,
    Delete,
    Move
});
wire_enum!(ProcessOp {
    Wait,
    Input,
    Stop,
    Inspect
});
wire_enum!(ProcessState {
    Pending,
    Running,
    Exited,
    Failed,
    Stopped,
    Unknown,
});
wire_enum!(ChildTrace {
    Unavailable,
    Partial,
    Complete
});
wire_enum!(WebNavigationOp {
    OpenPage,
    FindInPage
});
wire_enum!(TaskOp {
    Create,
    Update,
    List,
    Get,
    Stop,
    Output
});
wire_enum!(PlanOp {
    Create,
    Update,
    Clear,
    Show
});
wire_enum!(CollaborationOp {
    Send,
    Create,
    Delete,
    List
});
wire_enum!(ParticipantState {
    Active,
    Idle,
    Completed,
    Failed,
    Unknown
});
wire_enum!(ScheduleOp {
    Create,
    Delete,
    List,
    Wake
});
wire_enum!(ArtifactOp { Create, Edit, Send });
wire_enum!(ArtifactKind {
    Image,
    Video,
    Audio,
    Document,
    File,
    Other
});
wire_enum!(ModeOp { Enter, Exit });
wire_enum!(ExecutionMode {
    Plan,
    Review,
    Worktree,
    Normal
});
wire_enum!(FindingSeverity {
    Info,
    Low,
    Medium,
    High,
    Critical
});
wire_enum!(FieldRole {
    Subject,
    Target,
    Filter,
    Parameter,
    Outcome
});
wire_enum!(TruncationScope {
    Message,
    ToolResult,
    Context,
    Session
});
wire_enum!(PermissionOutcome {
    Allowed,
    Denied,
    Cancelled
});
wire_enum!(RuntimeErrorKind {
    Authentication,
    Configuration,
    Network,
    Provider,
    Tool,
    Unknown,
});

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub enum CanonicalText {
    Inline {
        text: String,
    },
    Member {
        asset: AssetRef,
        preview: String,
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        byte_len: u64,
    },
}

impl CanonicalText {
    pub fn inline(text: impl Into<String>) -> Result<Self, CanonicalError> {
        let text = text.into();
        bounded("canonical text", &text, INLINE_TEXT_MAX_BYTES)?;
        Ok(Self::Inline { text })
    }

    pub fn as_preview(&self) -> &str {
        match self {
            Self::Inline { text } => text,
            Self::Member { preview, .. } => preview,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct AssetRef {
    pub url: String,
    pub digest: String,
    pub media_type: String,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub byte_len: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub enum ResourceRef {
    File { path: String, resolved: bool },
    Directory { path: String, resolved: bool },
    Url { url: String },
    Session { id: String },
    Task { id: String },
    Process { id: String },
    Asset { asset: AssetRef },
}

impl ResourceRef {
    pub fn display_value(&self) -> &str {
        match self {
            Self::File { path, .. } | Self::Directory { path, .. } => path,
            Self::Url { url } => url,
            Self::Session { id } | Self::Task { id } | Self::Process { id } => id,
            Self::Asset { asset } => &asset.url,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct Dimensions {
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct AttachmentRef {
    pub kind: AttachmentKind,
    pub name: Option<String>,
    pub media_type: Option<String>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub byte_len: Option<u64>,
    pub source: ResourceRef,
    pub dimensions: Option<Dimensions>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct TextRange {
    pub start: u32,
    pub end: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct CitationRef {
    pub source_id: Option<String>,
    pub provider: Option<String>,
    pub title: Option<String>,
    pub source: Option<ResourceRef>,
    pub message_ranges: Vec<TextRange>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct TimelineGroup {
    pub id: String,
    pub kind: GroupKind,
    pub derivation: GroupDerivation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct ResponseGroup {
    pub id: String,
    pub derivation: GroupDerivation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct Provenance {
    pub agent_version: Option<String>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub src_cursor: Option<i64>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub src_offset: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct ToolCorrelation {
    pub key: String,
    pub derivation: CorrelationDerivation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct NativeToolRef {
    pub id: Option<String>,
    pub namespace: Option<String>,
    pub name: String,
    pub provider: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct ToolStatusFact {
    pub value: ToolStatus,
    pub basis: StatusBasis,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct ToolFailure {
    pub kind: FailureKind,
    pub summary: Option<String>,
    pub diagnostic: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct ToolTiming {
    pub started_at: Option<Ms>,
    pub completed_at: Option<Ms>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub enum Notice {
    Compaction {
        reason: Option<String>,
    },
    Truncation {
        scope: TruncationScope,
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        omitted_count: Option<u64>,
    },
    ModelChanged {
        from: Option<String>,
        to: String,
    },
    ModeChanged {
        from: Option<ExecutionMode>,
        to: ExecutionMode,
    },
    WorkspaceChanged {
        cwd: String,
        git_branch: Option<String>,
    },
    PermissionRequested {
        request_id: Option<String>,
        scope: Option<String>,
    },
    PermissionDecision {
        decision: PermissionOutcome,
        scope: Option<String>,
    },
    Handoff {
        to_session_id: Option<String>,
        to_agent: Option<String>,
    },
    RuntimeError {
        kind: RuntimeErrorKind,
        summary: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct SourceRange {
    pub unit: RangeUnit,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub start: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub count: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct ReadTarget {
    pub resource: ResourceRef,
    pub range: Option<SourceRange>,
    pub media_kind: Option<AttachmentKind>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct ReadOutcome {
    pub truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct ListOutcome {
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub entry_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct SearchFilter {
    pub kind: SearchFilterKind,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct SearchOutcome {
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub match_count: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub file_count: u64,
    pub paths: Vec<ResourceRef>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub total_paths: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct SourceLocation {
    pub resource: ResourceRef,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub line: Option<u64>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub column: Option<u64>,
    pub summary: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct LocationOutcome {
    pub locations: Vec<SourceLocation>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub total_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct FileDelta {
    pub operation: Option<FileChangeOp>,
    pub path: ResourceRef,
    pub destination: Option<ResourceRef>,
    pub diff: Option<CanonicalText>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub lines_added: Option<u64>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub lines_removed: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct ShellOutcome {
    pub exit_code: Option<i32>,
    pub signal: Option<String>,
    pub process_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct ProcessOutcome {
    pub state: ProcessState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct WebSource {
    pub title: Option<String>,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct WebSearchOutcome {
    pub sources: Vec<WebSource>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub total_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct FetchOutcome {
    pub final_url: Option<String>,
    pub title: Option<String>,
    pub http_status: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct CollaboratorRef {
    pub name: String,
    pub role: Option<String>,
    pub session_id: Option<String>,
    pub state: ParticipantState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct CollaborationOutcome {
    pub participants: Vec<CollaboratorRef>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub total_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct QuestionOption {
    pub id: String,
    pub label: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct UserQuestion {
    pub id: String,
    pub prompt: CanonicalText,
    pub header: Option<String>,
    pub options: Vec<QuestionOption>,
    pub allow_multiple: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct UserAnswer {
    pub question_id: String,
    pub values: Vec<CanonicalText>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct MediaMetadata {
    pub media_type: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct FindingItem {
    pub severity: FindingSeverity,
    pub title: String,
    pub summary: Option<CanonicalText>,
    pub resources: Vec<ResourceRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct DisplayField {
    pub role: FieldRole,
    pub label: String,
    pub value: DisplayValue,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub enum DisplayValue {
    Text(String),
    Number(String),
    Boolean(bool),
    Resource(ResourceRef),
    List(Vec<DisplayScalar>),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub enum DisplayScalar {
    Text(String),
    Number(String),
    Boolean(bool),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub enum DocumentBlock {
    Field {
        field: DisplayField,
    },
    List {
        heading: Option<String>,
        items: Vec<DisplayScalar>,
    },
    Table {
        heading: Option<String>,
        columns: Vec<String>,
        rows: Vec<Vec<DisplayScalar>>,
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        total_rows: u64,
    },
    Section {
        heading: String,
        blocks: Vec<DocumentBlock>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub enum CanonicalDocument {
    Inline {
        title: Option<String>,
        summary: Option<CanonicalText>,
        blocks: Vec<DocumentBlock>,
        resources: Vec<ResourceRef>,
    },
    Member {
        asset: AssetRef,
        title: Option<String>,
        preview: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub enum ToolAction {
    FileRead {
        targets: Vec<ReadTarget>,
        outcome: Option<ReadOutcome>,
    },
    FileList {
        directory: ResourceRef,
        depth: Option<u32>,
        filter: Option<String>,
        outcome: Option<ListOutcome>,
    },
    FileSearch {
        query: String,
        mode: FileSearchMode,
        roots: Vec<ResourceRef>,
        filters: Vec<SearchFilter>,
        outcome: Option<SearchOutcome>,
    },
    CodeLookup {
        operation: CodeLookupOp,
        query: String,
        path: Option<ResourceRef>,
        outcome: Option<LocationOutcome>,
    },
    FileChange {
        changes: Vec<FileDelta>,
    },
    Shell {
        command: CanonicalText,
        cwd: Option<ResourceRef>,
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        timeout_ms: Option<u64>,
        background: bool,
        outcome: Option<ShellOutcome>,
    },
    CodeExecute {
        language: Option<String>,
        code: CanonicalText,
        cwd: Option<ResourceRef>,
        outcome: Option<ShellOutcome>,
    },
    Process {
        operation: ProcessOp,
        process_id: String,
        input: Option<CanonicalText>,
        outcome: Option<ProcessOutcome>,
    },
    Orchestration {
        runtime: Option<String>,
        trace: ChildTrace,
        executed_children: Vec<String>,
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        executed_child_count: Option<u64>,
    },
    WebSearch {
        query: Option<String>,
        allowed_domains: Vec<String>,
        blocked_domains: Vec<String>,
        outcome: Option<WebSearchOutcome>,
    },
    WebNavigate {
        operation: WebNavigationOp,
        url: Option<ResourceRef>,
        pattern: Option<String>,
        outcome: Option<FetchOutcome>,
    },
    WebFetch {
        url: ResourceRef,
        purpose: Option<String>,
        outcome: Option<FetchOutcome>,
    },
    Delegate {
        agent_kind: Option<String>,
        name: Option<String>,
        model: Option<String>,
        mode: Option<String>,
        objective: CanonicalText,
        child_session_id: Option<String>,
        resume_id: Option<String>,
        background: bool,
        orphan_result_summary: Option<CanonicalText>,
    },
    Task {
        operation: TaskOp,
        task_id: Option<String>,
        subject: Option<String>,
        description: Option<CanonicalText>,
        status: Option<TaskStatus>,
        owner: Option<String>,
        blocker_ids: Vec<String>,
    },
    Plan {
        operation: PlanOp,
        plan_id: Option<String>,
        title: Option<String>,
        summary: Option<CanonicalText>,
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        step_count: Option<u64>,
    },
    Collaborate {
        operation: CollaborationOp,
        recipient: Option<String>,
        team: Option<String>,
        summary: Option<String>,
        body: Option<CanonicalText>,
        linked_session_id: Option<String>,
        delivery_id: Option<String>,
        outcome: Option<CollaborationOutcome>,
    },
    Question {
        questions: Vec<UserQuestion>,
        answers: Vec<UserAnswer>,
    },
    Skill {
        name: String,
        arguments: Option<CanonicalText>,
    },
    Workflow {
        name: Option<String>,
        path: Option<ResourceRef>,
        arguments: Option<CanonicalText>,
        objective: Option<CanonicalText>,
        resume_id: Option<String>,
        background: bool,
    },
    Schedule {
        operation: ScheduleOp,
        schedule_id: Option<String>,
        due_at: Option<Ms>,
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        delay_ms: Option<u64>,
        reason: Option<String>,
        objective: Option<CanonicalText>,
    },
    Artifact {
        operation: ArtifactOp,
        artifact_kind: ArtifactKind,
        name: Option<String>,
        description: Option<CanonicalText>,
        resources: Vec<ResourceRef>,
        media: Option<MediaMetadata>,
    },
    ToolDiscovery {
        query: String,
        matches: Vec<String>,
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        total_count: u64,
    },
    Mode {
        operation: ModeOp,
        mode: ExecutionMode,
        workspace: Option<ResourceRef>,
    },
    Finding {
        findings: Vec<FindingItem>,
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        total_count: u64,
    },
    Notification {
        channel: Option<String>,
        destination: Option<String>,
        summary: CanonicalText,
        delivery_id: Option<String>,
    },
    StructuredOutput {
        document: CanonicalDocument,
    },
    External {
        provider: Option<String>,
        operation: String,
        subject: Option<String>,
        fields: Vec<DisplayField>,
        resources: Vec<ResourceRef>,
    },
    Unknown {},
}

wire_enum!(ToolActionKind {
    FileRead,
    FileList,
    FileSearch,
    CodeLookup,
    FileChange,
    Shell,
    CodeExecute,
    Process,
    Orchestration,
    WebSearch,
    WebNavigate,
    WebFetch,
    Delegate,
    Task,
    Plan,
    Collaborate,
    Question,
    Skill,
    Workflow,
    Schedule,
    Artifact,
    ToolDiscovery,
    Mode,
    Finding,
    Notification,
    StructuredOutput,
    External,
    Unknown,
});

impl ToolAction {
    pub fn kind(&self) -> ToolActionKind {
        match self {
            Self::FileRead { .. } => ToolActionKind::FileRead,
            Self::FileList { .. } => ToolActionKind::FileList,
            Self::FileSearch { .. } => ToolActionKind::FileSearch,
            Self::CodeLookup { .. } => ToolActionKind::CodeLookup,
            Self::FileChange { .. } => ToolActionKind::FileChange,
            Self::Shell { .. } => ToolActionKind::Shell,
            Self::CodeExecute { .. } => ToolActionKind::CodeExecute,
            Self::Process { .. } => ToolActionKind::Process,
            Self::Orchestration { .. } => ToolActionKind::Orchestration,
            Self::WebSearch { .. } => ToolActionKind::WebSearch,
            Self::WebNavigate { .. } => ToolActionKind::WebNavigate,
            Self::WebFetch { .. } => ToolActionKind::WebFetch,
            Self::Delegate { .. } => ToolActionKind::Delegate,
            Self::Task { .. } => ToolActionKind::Task,
            Self::Plan { .. } => ToolActionKind::Plan,
            Self::Collaborate { .. } => ToolActionKind::Collaborate,
            Self::Question { .. } => ToolActionKind::Question,
            Self::Skill { .. } => ToolActionKind::Skill,
            Self::Workflow { .. } => ToolActionKind::Workflow,
            Self::Schedule { .. } => ToolActionKind::Schedule,
            Self::Artifact { .. } => ToolActionKind::Artifact,
            Self::ToolDiscovery { .. } => ToolActionKind::ToolDiscovery,
            Self::Mode { .. } => ToolActionKind::Mode,
            Self::Finding { .. } => ToolActionKind::Finding,
            Self::Notification { .. } => ToolActionKind::Notification,
            Self::StructuredOutput { .. } => ToolActionKind::StructuredOutput,
            Self::External { .. } => ToolActionKind::External,
            Self::Unknown {} => ToolActionKind::Unknown,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct ToolInteraction {
    pub correlation: ToolCorrelation,
    pub native: NativeToolRef,
    pub status: ToolStatusFact,
    pub action: ToolAction,
    pub failure: Option<ToolFailure>,
    pub timing: Option<ToolTiming>,
}

impl ToolInteraction {
    pub fn unknown(
        key: impl Into<String>,
        derivation: CorrelationDerivation,
        native_name: impl Into<String>,
    ) -> Self {
        Self {
            correlation: ToolCorrelation {
                key: key.into(),
                derivation,
            },
            native: NativeToolRef {
                id: None,
                namespace: None,
                name: native_name.into(),
                provider: None,
            },
            status: ToolStatusFact {
                value: ToolStatus::Pending,
                basis: StatusBasis::RequestObserved,
            },
            action: ToolAction::Unknown {},
            failure: None,
            timing: None,
        }
    }

    pub fn validate(&self) -> Result<(), CanonicalError> {
        bounded("tool correlation key", &self.correlation.key, ID_MAX_BYTES)?;
        bounded("tool name", &self.native.name, ID_MAX_BYTES)?;
        if self.status.basis == StatusBasis::SourceFinalization
            && self.status.value != ToolStatus::Unknown
        {
            return Err(CanonicalError::Invalid(
                "source finalization may only establish unknown status".into(),
            ));
        }
        if let Some(failure) = &self.failure {
            if let Some(summary) = &failure.summary {
                bounded("failure summary", summary, FAILURE_SUMMARY_MAX_BYTES)?;
            }
            if let Some(diagnostic) = &failure.diagnostic {
                bounded(
                    "failure diagnostic",
                    diagnostic,
                    FAILURE_DIAGNOSTIC_MAX_BYTES,
                )?;
                if diagnostic.lines().count() > FAILURE_DIAGNOSTIC_MAX_LINES {
                    return Err(CanonicalError::Invalid(
                        "failure diagnostic exceeds 40 lines".into(),
                    ));
                }
            }
        }
        if let Some(timing) = self.timing {
            if timing
                .completed_at
                .zip(timing.started_at)
                .is_some_and(|(end, start)| end < start)
            {
                return Err(CanonicalError::Invalid(
                    "tool completion precedes tool start".into(),
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub enum TimelineBody {
    User {
        text: Option<CanonicalText>,
        attachments: Vec<AttachmentRef>,
    },
    Agent {
        text: Option<CanonicalText>,
        attachments: Vec<AttachmentRef>,
        citations: Vec<CitationRef>,
    },
    Thinking {
        text: CanonicalText,
    },
    ToolUse {
        tool: ToolInteraction,
    },
    Notice {
        notice: Notice,
    },
    Summary {
        purpose: SummaryPurpose,
        text: CanonicalText,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct TimelineEntry {
    pub timeline_id: String,
    pub session_id: String,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub ordinal: i64,
    pub group: TimelineGroup,
    pub response: Option<ResponseGroup>,
    pub block_index: Option<u32>,
    pub native_id: Option<String>,
    pub parent_native_id: Option<String>,
    pub at: Option<Ms>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub body: TimelineBody,
    pub usage: TokenSnapshot,
    pub provenance: Provenance,
}

impl TimelineEntry {
    pub fn validate(&self) -> Result<(), CanonicalError> {
        bounded("timeline id", &self.timeline_id, ID_MAX_BYTES)?;
        bounded("session id", &self.session_id, ID_MAX_BYTES)?;
        bounded("group id", &self.group.id, ID_MAX_BYTES)?;
        if self.response.is_some() != self.block_index.is_some() {
            return Err(CanonicalError::Invalid(
                "response and block_index must be present together".into(),
            ));
        }
        match &self.body {
            TimelineBody::User { text, attachments }
                if text.is_none() && attachments.is_empty() =>
            {
                return Err(CanonicalError::Invalid(
                    "user entry has neither text nor attachments".into(),
                ));
            }
            TimelineBody::Agent {
                text,
                attachments,
                citations,
            } if text.is_none() && attachments.is_empty() && citations.is_empty() => {
                return Err(CanonicalError::Invalid(
                    "agent entry has neither text, attachments, nor citations".into(),
                ));
            }
            TimelineBody::ToolUse { tool } => tool.validate()?,
            _ => {}
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct SourceIdentity(pub String);

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct EvidenceOrder {
    pub source_identity: SourceIdentity,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub generation: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub record_offset: u64,
    pub mutation_index: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct EvidenceStamp {
    pub rank: u16,
    pub order: EvidenceOrder,
}

impl PartialOrd for SourceIdentity {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for SourceIdentity {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.cmp(&other.0)
    }
}

/// A closed request correction.  It may replace or enrich only the typed
/// action request; it can never carry a native object or result body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct ToolRequestPatch {
    pub action: ToolAction,
}

/// Reduced outcome facts for a known action.  The action tag is checked
/// against the opener before application; known families never oscillate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct ToolOutcomePatch {
    pub action: ToolAction,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct ToolUpdatePatch {
    pub native_id: Option<String>,
    pub request: Option<ToolRequestPatch>,
    pub outcome: Option<ToolOutcomePatch>,
    pub status: Option<ToolStatusFact>,
    pub failure: Option<ToolFailure>,
    pub timing: Option<ToolTiming>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolMutation {
    Opened {
        entry: Box<TimelineEntry>,
        evidence: EvidenceOrder,
    },
    Updated {
        session_id: String,
        correlation_key: String,
        evidence: EvidenceOrder,
        patch: ToolUpdatePatch,
    },
}

/// Private deterministic merge stamps.  One request/outcome stamp is enough
/// because typed action merging fills absent fields regardless of age and
/// replaces existing fields only when the incoming stamp wins.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolMergeState {
    pub native_id: Option<EvidenceStamp>,
    pub request: Option<EvidenceStamp>,
    pub outcome: Option<EvidenceStamp>,
    pub status: Option<EvidenceStamp>,
    pub failure: Option<EvidenceStamp>,
    pub timing: Option<EvidenceStamp>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchLanes {
    pub prose: String,
    pub target: String,
    pub intent: String,
    pub change: String,
    pub tool: String,
}

impl SearchLanes {
    fn push(lane: &mut String, value: &str) {
        if value.is_empty() {
            return;
        }
        if !lane.is_empty() {
            lane.push('\n');
        }
        lane.push_str(value);
    }
}

impl TimelineEntry {
    /// Exhaustive extraction into semantic search lanes.  Search never walks
    /// serialized JSON and therefore cannot accidentally index a new field.
    pub fn search_lanes(&self) -> SearchLanes {
        let mut lanes = SearchLanes::default();
        match &self.body {
            TimelineBody::User { text, attachments } => {
                if let Some(text) = text {
                    SearchLanes::push(&mut lanes.prose, text.as_preview());
                }
                for attachment in attachments {
                    SearchLanes::push(&mut lanes.target, attachment.source.display_value());
                }
            }
            TimelineBody::Agent {
                text,
                attachments,
                citations,
            } => {
                if let Some(text) = text {
                    SearchLanes::push(&mut lanes.prose, text.as_preview());
                }
                for attachment in attachments {
                    SearchLanes::push(&mut lanes.target, attachment.source.display_value());
                }
                for citation in citations {
                    if let Some(title) = &citation.title {
                        SearchLanes::push(&mut lanes.target, title);
                    }
                    if let Some(source) = &citation.source {
                        SearchLanes::push(&mut lanes.target, source.display_value());
                    }
                }
            }
            TimelineBody::Thinking { text } | TimelineBody::Summary { text, .. } => {
                SearchLanes::push(&mut lanes.prose, text.as_preview());
            }
            TimelineBody::Notice { .. } => {}
            TimelineBody::ToolUse { tool } => {
                tool.action.extract_search(&mut lanes, &tool.native.name)
            }
        }
        lanes
    }
}

impl ToolAction {
    fn extract_search(&self, lanes: &mut SearchLanes, native_name: &str) {
        SearchLanes::push(&mut lanes.tool, native_name);
        let resource = |lanes: &mut SearchLanes, value: &ResourceRef| {
            SearchLanes::push(&mut lanes.target, value.display_value())
        };
        match self {
            Self::FileRead { targets, .. } => {
                targets.iter().for_each(|v| resource(lanes, &v.resource))
            }
            Self::FileList {
                directory, filter, ..
            } => {
                resource(lanes, directory);
                if let Some(v) = filter {
                    SearchLanes::push(&mut lanes.intent, v);
                }
            }
            Self::FileSearch {
                query,
                roots,
                outcome,
                ..
            } => {
                SearchLanes::push(&mut lanes.intent, query);
                roots.iter().for_each(|v| resource(lanes, v));
                if let Some(v) = outcome {
                    v.paths.iter().for_each(|p| resource(lanes, p));
                }
            }
            Self::CodeLookup { query, path, .. } => {
                SearchLanes::push(&mut lanes.intent, query);
                if let Some(v) = path {
                    resource(lanes, v);
                }
            }
            Self::FileChange { changes } => {
                for change in changes {
                    resource(lanes, &change.path);
                    if let Some(v) = &change.destination {
                        resource(lanes, v);
                    }
                    if let Some(v) = &change.diff {
                        SearchLanes::push(&mut lanes.change, v.as_preview());
                    }
                }
            }
            Self::Shell { command, cwd, .. } => {
                SearchLanes::push(&mut lanes.intent, command.as_preview());
                if let Some(v) = cwd {
                    resource(lanes, v);
                }
            }
            Self::CodeExecute {
                language,
                code,
                cwd,
                ..
            } => {
                if let Some(v) = language {
                    SearchLanes::push(&mut lanes.tool, v);
                }
                SearchLanes::push(&mut lanes.intent, code.as_preview());
                if let Some(v) = cwd {
                    resource(lanes, v);
                }
            }
            Self::Process {
                process_id, input, ..
            } => {
                SearchLanes::push(&mut lanes.target, process_id);
                if let Some(v) = input {
                    SearchLanes::push(&mut lanes.intent, v.as_preview());
                }
            }
            Self::Orchestration { .. } => {}
            Self::WebSearch {
                query,
                allowed_domains,
                blocked_domains,
                outcome,
            } => {
                if let Some(v) = query {
                    SearchLanes::push(&mut lanes.intent, v);
                }
                allowed_domains
                    .iter()
                    .chain(blocked_domains)
                    .for_each(|v| SearchLanes::push(&mut lanes.target, v));
                if let Some(v) = outcome {
                    for source in &v.sources {
                        if let Some(title) = &source.title {
                            SearchLanes::push(&mut lanes.target, title);
                        }
                        SearchLanes::push(&mut lanes.target, &source.url);
                    }
                }
            }
            Self::WebNavigate {
                url,
                pattern,
                outcome,
                ..
            } => {
                if let Some(v) = url {
                    resource(lanes, v);
                }
                if let Some(v) = pattern {
                    SearchLanes::push(&mut lanes.intent, v);
                }
                if let Some(v) = outcome {
                    if let Some(url) = &v.final_url {
                        SearchLanes::push(&mut lanes.target, url);
                    }
                    if let Some(title) = &v.title {
                        SearchLanes::push(&mut lanes.target, title);
                    }
                }
            }
            Self::WebFetch {
                url,
                purpose,
                outcome,
            } => {
                resource(lanes, url);
                if let Some(v) = purpose {
                    SearchLanes::push(&mut lanes.intent, v);
                }
                if let Some(v) = outcome {
                    if let Some(url) = &v.final_url {
                        SearchLanes::push(&mut lanes.target, url);
                    }
                    if let Some(title) = &v.title {
                        SearchLanes::push(&mut lanes.target, title);
                    }
                }
            }
            Self::Delegate {
                agent_kind,
                name,
                objective,
                ..
            } => {
                if let Some(v) = agent_kind {
                    SearchLanes::push(&mut lanes.tool, v);
                }
                if let Some(v) = name {
                    SearchLanes::push(&mut lanes.tool, v);
                }
                SearchLanes::push(&mut lanes.intent, objective.as_preview());
            }
            Self::Task {
                subject,
                description,
                owner,
                ..
            } => {
                if let Some(v) = subject {
                    SearchLanes::push(&mut lanes.intent, v);
                }
                if let Some(v) = description {
                    SearchLanes::push(&mut lanes.intent, v.as_preview());
                }
                if let Some(v) = owner {
                    SearchLanes::push(&mut lanes.target, v);
                }
            }
            Self::Plan { title, summary, .. } => {
                if let Some(v) = title {
                    SearchLanes::push(&mut lanes.intent, v);
                }
                if let Some(v) = summary {
                    SearchLanes::push(&mut lanes.intent, v.as_preview());
                }
            }
            Self::Collaborate {
                recipient,
                team,
                summary,
                body,
                ..
            } => {
                if let Some(v) = recipient {
                    SearchLanes::push(&mut lanes.target, v);
                }
                if let Some(v) = team {
                    SearchLanes::push(&mut lanes.target, v);
                }
                if let Some(v) = summary {
                    SearchLanes::push(&mut lanes.intent, v);
                }
                if let Some(v) = body {
                    SearchLanes::push(&mut lanes.intent, v.as_preview());
                }
            }
            Self::Question { questions, answers } => {
                for v in questions {
                    SearchLanes::push(&mut lanes.intent, v.prompt.as_preview());
                }
                for v in answers {
                    for answer in &v.values {
                        SearchLanes::push(&mut lanes.intent, answer.as_preview());
                    }
                }
            }
            Self::Skill { name, arguments } => {
                SearchLanes::push(&mut lanes.tool, name);
                if let Some(v) = arguments {
                    SearchLanes::push(&mut lanes.intent, v.as_preview());
                }
            }
            Self::Workflow {
                name,
                path,
                arguments,
                objective,
                ..
            } => {
                if let Some(v) = name {
                    SearchLanes::push(&mut lanes.tool, v);
                }
                if let Some(v) = path {
                    resource(lanes, v);
                }
                if let Some(v) = arguments {
                    SearchLanes::push(&mut lanes.intent, v.as_preview());
                }
                if let Some(v) = objective {
                    SearchLanes::push(&mut lanes.intent, v.as_preview());
                }
            }
            Self::Schedule {
                schedule_id,
                reason,
                objective,
                ..
            } => {
                if let Some(v) = schedule_id {
                    SearchLanes::push(&mut lanes.target, v);
                }
                if let Some(v) = reason {
                    SearchLanes::push(&mut lanes.intent, v);
                }
                if let Some(v) = objective {
                    SearchLanes::push(&mut lanes.intent, v.as_preview());
                }
            }
            Self::Artifact {
                name,
                description,
                resources,
                ..
            } => {
                if let Some(v) = name {
                    SearchLanes::push(&mut lanes.target, v);
                }
                if let Some(v) = description {
                    SearchLanes::push(&mut lanes.intent, v.as_preview());
                }
                resources.iter().for_each(|v| resource(lanes, v));
            }
            Self::ToolDiscovery { query, matches, .. } => {
                SearchLanes::push(&mut lanes.intent, query);
                matches
                    .iter()
                    .for_each(|v| SearchLanes::push(&mut lanes.tool, v));
            }
            Self::Mode { workspace, .. } => {
                if let Some(v) = workspace {
                    resource(lanes, v);
                }
            }
            Self::Finding { findings, .. } => {
                for finding in findings {
                    SearchLanes::push(&mut lanes.intent, &finding.title);
                    if let Some(v) = &finding.summary {
                        SearchLanes::push(&mut lanes.intent, v.as_preview());
                    }
                    finding.resources.iter().for_each(|v| resource(lanes, v));
                }
            }
            Self::Notification {
                destination,
                summary,
                ..
            } => {
                if let Some(v) = destination {
                    SearchLanes::push(&mut lanes.target, v);
                }
                SearchLanes::push(&mut lanes.intent, summary.as_preview());
            }
            Self::StructuredOutput { document } => document.extract_search(lanes),
            Self::External {
                operation,
                subject,
                fields,
                resources,
                ..
            } => {
                SearchLanes::push(&mut lanes.tool, operation);
                if let Some(v) = subject {
                    SearchLanes::push(&mut lanes.intent, v);
                }
                for field in fields {
                    field.extract_search(lanes);
                }
                resources.iter().for_each(|v| resource(lanes, v));
            }
            Self::Unknown {} => {}
        }
    }
}

impl DisplayField {
    fn extract_search(&self, lanes: &mut SearchLanes) {
        let lane = match self.role {
            FieldRole::Subject | FieldRole::Parameter => &mut lanes.intent,
            FieldRole::Target | FieldRole::Filter => &mut lanes.target,
            FieldRole::Outcome => &mut lanes.change,
        };
        SearchLanes::push(lane, &self.label);
        match &self.value {
            DisplayValue::Text(v) | DisplayValue::Number(v) => SearchLanes::push(lane, v),
            DisplayValue::Boolean(v) => SearchLanes::push(lane, if *v { "true" } else { "false" }),
            DisplayValue::Resource(v) => SearchLanes::push(lane, v.display_value()),
            DisplayValue::List(values) => {
                for value in values {
                    match value {
                        DisplayScalar::Text(v) | DisplayScalar::Number(v) => {
                            SearchLanes::push(lane, v)
                        }
                        DisplayScalar::Boolean(v) => {
                            SearchLanes::push(lane, if *v { "true" } else { "false" })
                        }
                    }
                }
            }
        }
    }
}

impl CanonicalDocument {
    fn extract_search(&self, lanes: &mut SearchLanes) {
        match self {
            Self::Inline {
                title,
                summary,
                blocks,
                resources,
            } => {
                if let Some(v) = title {
                    SearchLanes::push(&mut lanes.prose, v);
                }
                if let Some(v) = summary {
                    SearchLanes::push(&mut lanes.prose, v.as_preview());
                }
                for block in blocks {
                    block.extract_search(lanes);
                }
                for resource in resources {
                    SearchLanes::push(&mut lanes.target, resource.display_value());
                }
            }
            Self::Member {
                title,
                preview,
                asset,
            } => {
                if let Some(v) = title {
                    SearchLanes::push(&mut lanes.prose, v);
                }
                SearchLanes::push(&mut lanes.prose, preview);
                SearchLanes::push(&mut lanes.target, &asset.url);
            }
        }
    }
}

impl DocumentBlock {
    fn extract_search(&self, lanes: &mut SearchLanes) {
        match self {
            Self::Field { field } => field.extract_search(lanes),
            Self::List { heading, items } => {
                if let Some(v) = heading {
                    SearchLanes::push(&mut lanes.prose, v);
                }
                for item in items {
                    match item {
                        DisplayScalar::Text(v) | DisplayScalar::Number(v) => {
                            SearchLanes::push(&mut lanes.prose, v)
                        }
                        DisplayScalar::Boolean(v) => {
                            SearchLanes::push(&mut lanes.prose, if *v { "true" } else { "false" })
                        }
                    }
                }
            }
            Self::Table {
                heading,
                columns,
                rows,
                ..
            } => {
                if let Some(v) = heading {
                    SearchLanes::push(&mut lanes.prose, v);
                }
                columns
                    .iter()
                    .for_each(|v| SearchLanes::push(&mut lanes.prose, v));
                for row in rows {
                    for item in row {
                        match item {
                            DisplayScalar::Text(v) | DisplayScalar::Number(v) => {
                                SearchLanes::push(&mut lanes.prose, v)
                            }
                            DisplayScalar::Boolean(v) => SearchLanes::push(
                                &mut lanes.prose,
                                if *v { "true" } else { "false" },
                            ),
                        }
                    }
                }
            }
            Self::Section { heading, blocks } => {
                SearchLanes::push(&mut lanes.prose, heading);
                for block in blocks {
                    block.extract_search(lanes);
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CanonicalError {
    Overflow {
        field: &'static str,
        bytes: usize,
        limit: usize,
    },
    Invalid(String),
}

impl std::fmt::Display for CanonicalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Overflow {
                field,
                bytes,
                limit,
            } => write!(f, "{field} is {bytes} bytes; limit is {limit}"),
            Self::Invalid(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for CanonicalError {}

fn bounded(field: &'static str, value: &str, limit: usize) -> Result<(), CanonicalError> {
    if value.len() > limit {
        Err(CanonicalError::Overflow {
            field,
            bytes: value.len(),
            limit,
        })
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base(body: TimelineBody) -> TimelineEntry {
        TimelineEntry {
            timeline_id: "m1".into(),
            session_id: "s1".into(),
            ordinal: 0,
            group: TimelineGroup {
                id: "g1".into(),
                kind: GroupKind::AgentCycle,
                derivation: GroupDerivation::Ordered,
            },
            response: None,
            block_index: None,
            native_id: None,
            parent_native_id: None,
            at: None,
            model: None,
            effort: None,
            body,
            usage: TokenSnapshot::default(),
            provenance: Provenance {
                agent_version: None,
                src_cursor: None,
                src_offset: None,
            },
        }
    }

    #[test]
    fn empty_prose_is_rejected_without_an_attachment() {
        let row = base(TimelineBody::User {
            text: None,
            attachments: vec![],
        });
        assert!(row.validate().is_err());
    }

    #[test]
    fn response_and_block_index_are_paired() {
        let mut row = base(TimelineBody::Thinking {
            text: CanonicalText::inline("x").unwrap(),
        });
        row.response = Some(ResponseGroup {
            id: "r1".into(),
            derivation: GroupDerivation::Native,
        });
        assert!(row.validate().is_err());
        row.block_index = Some(0);
        assert!(row.validate().is_ok());
    }

    #[test]
    fn source_finalization_cannot_claim_success() {
        let mut tool =
            ToolInteraction::unknown("t1", CorrelationDerivation::NativeId, "future_tool");
        tool.status = ToolStatusFact {
            value: ToolStatus::Succeeded,
            basis: StatusBasis::SourceFinalization,
        };
        assert!(tool.validate().is_err());
    }

    #[test]
    fn search_extracts_only_declared_fields() {
        let row = base(TimelineBody::ToolUse {
            tool: ToolInteraction {
                correlation: ToolCorrelation {
                    key: "t1".into(),
                    derivation: CorrelationDerivation::NativeId,
                },
                native: NativeToolRef {
                    id: Some("t1".into()),
                    namespace: None,
                    name: "bash".into(),
                    provider: None,
                },
                status: ToolStatusFact {
                    value: ToolStatus::Succeeded,
                    basis: StatusBasis::NativeTerminalRecord,
                },
                action: ToolAction::Shell {
                    command: CanonicalText::inline("git status").unwrap(),
                    cwd: Some(ResourceRef::Directory {
                        path: "/repo".into(),
                        resolved: true,
                    }),
                    timeout_ms: None,
                    background: false,
                    outcome: Some(ShellOutcome {
                        exit_code: Some(0),
                        signal: None,
                        process_id: None,
                    }),
                },
                failure: None,
                timing: None,
            },
        });
        let lanes = row.search_lanes();
        assert_eq!(lanes.intent, "git status");
        assert_eq!(lanes.target, "/repo");
        assert_eq!(lanes.tool, "bash");
        assert!(lanes.change.is_empty());
    }

    #[test]
    fn unknown_tools_index_only_the_native_name() {
        let row = base(TimelineBody::ToolUse {
            tool: ToolInteraction::unknown("t1", CorrelationDerivation::Synthetic, "mystery"),
        });
        let lanes = row.search_lanes();
        assert_eq!(lanes.tool, "mystery");
        assert!(lanes.intent.is_empty());
        assert!(lanes.target.is_empty());
    }
}
