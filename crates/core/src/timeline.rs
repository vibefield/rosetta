//! Closed canonical timeline and tool types.
//!
//! Native agent payloads stop before this module.  These types contain only
//! reviewed, user-facing facts and are safe to serialize into the disposable
//! projection and the public protocol.  In particular there is deliberately
//! no `serde_json::Value`, arbitrary map, or generic payload variant here.

use serde::{Deserialize, Serialize};

use crate::assets::{IMAGE_ASSET_URL_PREFIX, ProjectionAsset};
use crate::rows::{Ms, TaskStatus, TokenSnapshot};

mod attachment;
pub use attachment::{AttachmentRef, AttachmentSource, AttachmentUnavailableReason, Dimensions};

pub const INLINE_TEXT_MAX_BYTES: usize = 32 * 1024;
pub const TEXT_PREVIEW_MAX_BYTES: usize = 2 * 1024;
pub const CORE_SCALAR_MAX_BYTES: usize = 16 * 1024;
pub const ID_MAX_BYTES: usize = 512;
pub const FAILURE_SUMMARY_MAX_BYTES: usize = 512;
pub const FAILURE_DIAGNOSTIC_MAX_BYTES: usize = 4 * 1024;
pub const FAILURE_DIAGNOSTIC_MAX_LINES: usize = 40;
/// Commands are supporting evidence in the transcript, not prose. Keep the
/// disposable projection deliberately small; the raw ledger remains the
/// lossless source for an explicit on-demand expansion. The UI reserves the
/// final three characters for its omission marker when source was dropped.
pub const COMMAND_PREVIEW_MAX_CHARACTERS: usize = 42;
const COMMAND_PREVIEW_OMISSION_CHARACTERS: usize = 3;
pub const SEARCH_PATH_LIMIT: usize = 20;
pub const WEB_SOURCE_LIMIT: usize = 10;
pub const CITATION_RANGE_LIMIT: usize = 40;
pub const EXTERNAL_FIELD_LIMIT: usize = 24;
pub const CANONICAL_LIST_LIMIT: usize = 20;
/// Task boards are durable user-visible state rather than a result preview.
/// Keep enough of the board for the UI's explicit "show full tasks" action,
/// while retaining a hard admission ceiling for hostile/native payloads.
pub const TASK_SNAPSHOT_LIMIT: usize = 128;
/// Workflow fan-out is a first-class document rather than an ordinary tool
/// result. Real runs currently top out well below these bounds, but the limits
/// keep a hostile rewrite-in-place sidecar from becoming an unbounded wire
/// payload while still preserving the complete observed roster.
pub const WORKFLOW_PHASE_LIMIT: usize = 64;
pub const WORKFLOW_WORKER_LIMIT: usize = 256;
pub const WORKFLOW_ATTEMPT_LIMIT: usize = 256;
pub const WORKFLOW_JOURNAL_LIMIT: usize = 2_048;
pub const WORKFLOW_LOG_LIMIT: usize = 128;
pub const WORKFLOW_RUNTIME_PROFILE_LIMIT: usize = 16;
pub const EXTERNAL_LABEL_MAX_BYTES: usize = 128;
pub const EXTERNAL_VALUE_MAX_BYTES: usize = 1024;
pub const DOCUMENT_MAX_DEPTH: usize = 4;

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
wire_enum!(WorkflowRunStatus {
    Running,
    Completed,
    Failed,
    Killed,
    Cancelled,
    Stopped,
    Unknown,
});
wire_enum!(WorkflowWorkerState {
    Done,
    Error,
    Progress,
    Start,
    Unknown,
});
wire_enum!(WorkflowJournalKind { Started, Result });
wire_enum!(WorkflowResultKind {
    Object,
    Array,
    String,
    Number,
    Boolean,
    Null,
});
wire_enum!(PlanOp {
    Create,
    Update,
    Clear,
    Show
});
wire_enum!(CollaborationOp {
    Send,
    RequestStop,
    RespondStop,
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
// Native teammate colours are stable participant addresses, not inferred
// state. The closed set is backed by the Claude Code team-config census;
// adapters for runtimes without a native colour simply leave it absent.
wire_enum!(ParticipantColor {
    Blue,
    Green,
    Yellow,
    Purple,
    Orange,
    Pink,
    Cyan,
    Red
});
wire_enum!(CollaborationChannel {
    Coordinator,
    Peer,
    Team
});
wire_enum!(CollaborationEventKind {
    Message,
    Assignment,
    Idle,
    StopRequested,
    StopApproved,
    Terminated,
    Report
});
wire_enum!(CollaborationStatus {
    Available,
    Completed,
    Failed,
    Killed,
    Stopped
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

    /// Preserve canonical text fully while keeping SQLite rows bounded. The
    /// returned projection member must be emitted before the referencing row;
    /// its bytes remain rebuildable from the raw ledger.
    pub fn externalize(text: impl Into<String>) -> (Self, Option<ProjectionAsset>) {
        let text = text.into();
        if text.len() <= INLINE_TEXT_MAX_BYTES {
            return (Self::Inline { text }, None);
        }
        let preview = utf8_prefix(&text, TEXT_PREVIEW_MAX_BYTES).to_owned();
        let asset = ProjectionAsset::text(text);
        let reference = AssetRef {
            url: asset
                .url()
                .expect("canonical text media type has a stable extension"),
            digest: asset.digest.clone(),
            media_type: asset.media_type.clone(),
            byte_len: u64::try_from(asset.bytes.len()).unwrap_or(u64::MAX),
        };
        (
            Self::Member {
                asset: reference,
                preview,
                byte_len: u64::try_from(asset.bytes.len()).unwrap_or(u64::MAX),
            },
            Some(asset),
        )
    }

    pub fn validate(&self) -> Result<(), CanonicalError> {
        match self {
            Self::Inline { text } => bounded("canonical text", text, INLINE_TEXT_MAX_BYTES),
            Self::Member {
                asset,
                preview,
                byte_len,
            } => {
                asset.validate()?;
                bounded("canonical text preview", preview, TEXT_PREVIEW_MAX_BYTES)?;
                if *byte_len != asset.byte_len {
                    return Err(CanonicalError::Invalid(
                        "canonical text member byte lengths disagree".into(),
                    ));
                }
                if asset.media_type != "text/plain; charset=utf-8" {
                    return Err(CanonicalError::Invalid(
                        "canonical text member has a non-text media type".into(),
                    ));
                }
                Ok(())
            }
        }
    }
}

fn utf8_prefix(value: &str, max_bytes: usize) -> &str {
    let mut end = value.len().min(max_bytes);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

/// Keep one meaningful line. When anything is omitted, retain at most 39
/// characters so the UI's three-dot marker still fits inside the 42-character
/// reading contract. `char` counting matches the render helper's Unicode-code-
/// point counting and never slices through UTF-8.
fn command_preview(value: &str) -> (String, bool) {
    let line = value
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .find(|line| !line.trim().is_empty())
        .unwrap_or("");
    let line_characters = line.chars().count();
    let omitted = line != value || line_characters > COMMAND_PREVIEW_MAX_CHARACTERS;
    let limit = if omitted {
        COMMAND_PREVIEW_MAX_CHARACTERS - COMMAND_PREVIEW_OMISSION_CHARACTERS
    } else {
        COMMAND_PREVIEW_MAX_CHARACTERS
    };
    (line.chars().take(limit).collect(), omitted)
}

fn is_false(value: &bool) -> bool {
    !*value
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

impl AssetRef {
    pub fn validate(&self) -> Result<(), CanonicalError> {
        bounded("asset URL", &self.url, CORE_SCALAR_MAX_BYTES)?;
        bounded("asset media type", &self.media_type, ID_MAX_BYTES)?;
        if self.byte_len == 0 {
            return Err(CanonicalError::Invalid(
                "projection asset has zero declared bytes".into(),
            ));
        }
        if self.digest.len() != 64
            || !self
                .digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(CanonicalError::Invalid(
                "projection asset digest is not lowercase BLAKE3 hex".into(),
            ));
        }
        let extension =
            ProjectionAsset::supported_extension(&self.media_type).ok_or_else(|| {
                CanonicalError::Invalid(format!(
                    "unsupported projection asset media type {:?}",
                    self.media_type
                ))
            })?;
        let expected = format!("{IMAGE_ASSET_URL_PREFIX}{}.{extension}", self.digest);
        if self.url != expected {
            return Err(CanonicalError::Invalid(
                "projection asset URL does not match its digest and media type".into(),
            ));
        }
        Ok(())
    }
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

    pub fn validate(&self) -> Result<(), CanonicalError> {
        match self {
            Self::File { path, .. } | Self::Directory { path, .. } => {
                required_bounded("resource path", path, CORE_SCALAR_MAX_BYTES)
            }
            Self::Url { url } => required_bounded("resource URL", url, CORE_SCALAR_MAX_BYTES),
            Self::Session { id } | Self::Task { id } | Self::Process { id } => {
                required_bounded("resource id", id, ID_MAX_BYTES)
            }
            Self::Asset { asset } => asset.validate(),
        }
    }
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

impl CitationRef {
    pub fn validate(&self) -> Result<(), CanonicalError> {
        bounded_option(
            "citation source id",
            self.source_id.as_deref(),
            ID_MAX_BYTES,
        )?;
        bounded_option("citation provider", self.provider.as_deref(), ID_MAX_BYTES)?;
        bounded_option(
            "citation title",
            self.title.as_deref(),
            CORE_SCALAR_MAX_BYTES,
        )?;
        if let Some(source) = &self.source {
            source.validate()?;
        }
        bounded_collection(
            "citation message ranges",
            self.message_ranges.len(),
            CITATION_RANGE_LIMIT,
        )?;
        let mut prior_end = 0;
        for (index, range) in self.message_ranges.iter().enumerate() {
            if range.start >= range.end {
                return Err(CanonicalError::Invalid(
                    "citation range must have positive width".into(),
                ));
            }
            if index > 0 && range.start < prior_end {
                return Err(CanonicalError::Invalid(
                    "citation ranges must be ordered and non-overlapping".into(),
                ));
            }
            prior_end = range.end;
        }
        if self.source_id.is_none()
            && self.provider.is_none()
            && self.title.is_none()
            && self.source.is_none()
        {
            return Err(CanonicalError::Invalid(
                "citation has no source identity".into(),
            ));
        }
        Ok(())
    }
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
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub src_cursor: Option<i64>,
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
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
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
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
        #[cfg_attr(feature = "ts", ts(type = "number | null"))]
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
    CommandOutput {
        output: CanonicalText,
    },
    /// A top-level async task callback whose originating tool is not present
    /// in this branch. Correlated callbacks patch their tool row instead; this
    /// marker keeps an otherwise orphaned machine event out of human prose.
    AsyncTask {
        task_id: Option<String>,
        tool_use_id: Option<String>,
        status: ToolStatus,
        summary: Option<String>,
        #[cfg_attr(feature = "ts", ts(type = "number | null"))]
        duration_ms: Option<u64>,
    },
    RuntimeError {
        kind: RuntimeErrorKind,
        summary: String,
    },
}

impl Notice {
    pub fn validate(&self) -> Result<(), CanonicalError> {
        match self {
            Self::Compaction { reason } => bounded_option(
                "compaction reason",
                reason.as_deref(),
                CORE_SCALAR_MAX_BYTES,
            )?,
            Self::Truncation { .. } | Self::ModeChanged { .. } => {}
            Self::ModelChanged { from, to } => {
                bounded_option("prior model", from.as_deref(), ID_MAX_BYTES)?;
                required_bounded("model", to, ID_MAX_BYTES)?;
            }
            Self::PermissionRequested { request_id, scope } => {
                bounded_option("permission request id", request_id.as_deref(), ID_MAX_BYTES)?;
                bounded_option("permission scope", scope.as_deref(), CORE_SCALAR_MAX_BYTES)?;
            }
            Self::PermissionDecision { scope, .. } => {
                bounded_option("permission scope", scope.as_deref(), CORE_SCALAR_MAX_BYTES)?;
            }
            Self::Handoff {
                to_session_id,
                to_agent,
            } => {
                bounded_option("handoff session id", to_session_id.as_deref(), ID_MAX_BYTES)?;
                bounded_option("handoff agent", to_agent.as_deref(), ID_MAX_BYTES)?;
            }
            Self::CommandOutput { output } => output.validate()?,
            Self::AsyncTask {
                task_id,
                tool_use_id,
                summary,
                ..
            } => {
                bounded_option("async task id", task_id.as_deref(), ID_MAX_BYTES)?;
                bounded_option("async tool-use id", tool_use_id.as_deref(), ID_MAX_BYTES)?;
                bounded_option(
                    "async task summary",
                    summary.as_deref(),
                    FAILURE_SUMMARY_MAX_BYTES,
                )?;
            }
            Self::RuntimeError { summary, .. } => {
                required_bounded("runtime error summary", summary, FAILURE_SUMMARY_MAX_BYTES)?;
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct SourceRange {
    pub unit: RangeUnit,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub start: u64,
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
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
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub line: Option<u64>,
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
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
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub lines_added: Option<u64>,
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
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

/// One delivered collaboration boundary event.  This is deliberately not a
/// native transport envelope: sender-side requests, routing acknowledgements,
/// inbox copies, and recipient envelopes can all describe the same speech.
/// Adapters emit only the user-facing observation they can place in time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct CollaborationEvent {
    pub kind: CollaborationEventKind,
    pub channel: CollaborationChannel,
    pub sender: Option<String>,
    pub recipient: Option<String>,
    pub team: Option<String>,
    pub color: Option<ParticipantColor>,
    pub summary: Option<String>,
    pub body: Option<CanonicalText>,
    pub task_id: Option<String>,
    pub delivery_id: Option<String>,
    pub status: Option<CollaborationStatus>,
    /// Native send time carried inside a delivery envelope. `TimelineEntry.at`
    /// remains the containing transcript's observed-arrival time.
    #[serde(default)]
    pub authored_at: Option<Ms>,
    /// Exact owning session for an optional report-work disclosure.
    #[serde(default)]
    pub source_session_id: Option<String>,
    /// Exact response in that session which produced the task-linked result.
    #[serde(default)]
    pub source_response_id: Option<String>,
    /// Inclusive exact bounds within `source_session_id`. Both or neither.
    #[serde(default)]
    pub work_start_timeline_id: Option<String>,
    #[serde(default)]
    pub work_end_timeline_id: Option<String>,
}

impl CollaborationEvent {
    fn validate(&self) -> Result<(), CanonicalError> {
        bounded_option("collaboration sender", self.sender.as_deref(), ID_MAX_BYTES)?;
        bounded_option(
            "collaboration recipient",
            self.recipient.as_deref(),
            ID_MAX_BYTES,
        )?;
        bounded_option("collaboration team", self.team.as_deref(), ID_MAX_BYTES)?;
        bounded_option(
            "collaboration summary",
            self.summary.as_deref(),
            FAILURE_SUMMARY_MAX_BYTES,
        )?;
        bounded_option(
            "collaboration task id",
            self.task_id.as_deref(),
            ID_MAX_BYTES,
        )?;
        bounded_option(
            "collaboration delivery id",
            self.delivery_id.as_deref(),
            ID_MAX_BYTES,
        )?;
        bounded_option(
            "collaboration source session id",
            self.source_session_id.as_deref(),
            ID_MAX_BYTES,
        )?;
        bounded_option(
            "collaboration source response id",
            self.source_response_id.as_deref(),
            ID_MAX_BYTES,
        )?;
        bounded_option(
            "collaboration work start timeline id",
            self.work_start_timeline_id.as_deref(),
            ID_MAX_BYTES,
        )?;
        bounded_option(
            "collaboration work end timeline id",
            self.work_end_timeline_id.as_deref(),
            ID_MAX_BYTES,
        )?;
        validate_optional_text(self.body.as_ref())?;
        let visible_body = self
            .body
            .as_ref()
            .is_some_and(|body| !body.as_preview().trim().is_empty());
        let visible_summary = self
            .summary
            .as_deref()
            .is_some_and(|summary| !summary.trim().is_empty());
        if matches!(
            self.kind,
            CollaborationEventKind::Message
                | CollaborationEventKind::Assignment
                | CollaborationEventKind::Report
        ) && !visible_body
            && !visible_summary
        {
            return Err(CanonicalError::Invalid(
                "collaboration event has no visible fact".into(),
            ));
        }
        let has_start = self.work_start_timeline_id.is_some();
        let has_end = self.work_end_timeline_id.is_some();
        if has_start != has_end {
            return Err(CanonicalError::Invalid(
                "collaboration report work bounds are incomplete".into(),
            ));
        }
        if (has_start || self.source_session_id.is_some() || self.source_response_id.is_some())
            && self.kind != CollaborationEventKind::Report
        {
            return Err(CanonicalError::Invalid(
                "collaboration work provenance belongs only to reports".into(),
            ));
        }
        if has_start && self.source_session_id.is_none() {
            return Err(CanonicalError::Invalid(
                "collaboration report work has no source session".into(),
            ));
        }
        if self.kind == CollaborationEventKind::Report
            && (self.task_id.is_none()
                || self.source_session_id.is_none()
                || self.source_response_id.is_none())
        {
            return Err(CanonicalError::Invalid(
                "collaboration report has no exact task source".into(),
            ));
        }
        if self.kind == CollaborationEventKind::Report {
            required_bounded(
                "collaboration report task id",
                self.task_id.as_deref().expect("presence checked above"),
                ID_MAX_BYTES,
            )?;
            required_bounded(
                "collaboration report source session id",
                self.source_session_id
                    .as_deref()
                    .expect("presence checked above"),
                ID_MAX_BYTES,
            )?;
            required_bounded(
                "collaboration report source response id",
                self.source_response_id
                    .as_deref()
                    .expect("presence checked above"),
                ID_MAX_BYTES,
            )?;
        }
        if has_start {
            required_bounded(
                "collaboration report work start timeline id",
                self.work_start_timeline_id
                    .as_deref()
                    .expect("paired bounds checked above"),
                ID_MAX_BYTES,
            )?;
            required_bounded(
                "collaboration report work end timeline id",
                self.work_end_timeline_id
                    .as_deref()
                    .expect("paired bounds checked above"),
                ID_MAX_BYTES,
            )?;
        }
        Ok(())
    }
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

/// One task in the bounded, point-in-time board attached to a task action.
/// The raw ledger remains authoritative; this user-facing state makes both a
/// compact preview and an explicit full-board disclosure possible.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct TaskSnapshotItem {
    pub task_id: String,
    pub subject: Option<String>,
    pub status: Option<TaskStatus>,
    pub owner: Option<String>,
    pub blocker_ids: Vec<String>,
}

/// A bounded task board immediately after the action completed. `total_count`
/// stays honest if a native board exceeds the task-specific safety ceiling.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct TaskSnapshot {
    pub tasks: Vec<TaskSnapshotItem>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub total_count: u64,
}

/// Aggregate accounting emitted by one Workflow attempt or its durable run.
/// Missing values stay missing: a cached worker must never claim fresh work.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct WorkflowUsage {
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub duration_ms: Option<u64>,
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub tokens: Option<u64>,
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub tool_calls: Option<u64>,
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub agent_count: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct WorkflowPhase {
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub index: u64,
    pub title: String,
    pub detail: Option<String>,
}

/// One row in the mutable `workflowProgress` roster. Configured runtime facts
/// come from the run snapshot; observed profiles are joined from the durable
/// child transcript at read time and therefore remain separate evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct WorkflowWorker {
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub index: u64,
    pub key: Option<String>,
    pub label: String,
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub phase_index: Option<u64>,
    pub phase_title: Option<String>,
    pub state: WorkflowWorkerState,
    pub cached: bool,
    pub configured_model: Option<String>,
    pub configured_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub observed_models: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub observed_efforts: Vec<String>,
    pub child_session_id: Option<String>,
    pub prompt_preview: Option<String>,
    pub result_preview: Option<String>,
    pub last_tool_name: Option<String>,
    pub last_tool_summary: Option<String>,
    pub queued_at: Option<Ms>,
    pub started_at: Option<Ms>,
    pub last_progress_at: Option<Ms>,
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub duration_ms: Option<u64>,
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub tokens: Option<u64>,
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub tool_calls: Option<u64>,
    pub error: Option<String>,
}

/// Untimestamped coordinator evidence. `observation_id` is derived from the
/// journal source position, so append replay and generation replacement remain
/// deterministic without pretending the journal supplies a clock.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct WorkflowJournalObservation {
    pub observation_id: String,
    pub run_id: String,
    /// Byte position in the append-only journal. It orders observations
    /// without inventing a timestamp the native record does not contain.
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub source_offset: Option<u64>,
    pub kind: WorkflowJournalKind,
    pub worker_key: String,
    pub agent_id: Option<String>,
    pub result_kind: Option<WorkflowResultKind>,
    pub result_preview: Option<String>,
}

/// One chronological invocation of a Workflow run, hydrated from its ordinary
/// tool row. These are read facts, not fields persisted in `workflow_run`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct WorkflowAttempt {
    pub timeline_id: String,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub attempt: u64,
    pub task_id: Option<String>,
    pub resumed: bool,
    pub status: ToolStatus,
    pub at: Option<Ms>,
    pub completed_at: Option<Ms>,
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub duration_ms: Option<u64>,
    pub summary: Option<String>,
}

/// The durable current view of one Workflow run. Each invocation remains a
/// chronological tool row; query hydration attaches the complete ordinary
/// attempt history plus a bounded journal window to the shared run document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct WorkflowRun {
    pub run_id: String,
    pub task_id: Option<String>,
    pub name: Option<String>,
    pub summary: Option<CanonicalText>,
    pub status: WorkflowRunStatus,
    pub started_at: Option<Ms>,
    pub updated_at: Option<Ms>,
    pub usage: WorkflowUsage,
    pub default_model: Option<String>,
    pub arguments: Option<CanonicalText>,
    pub script_path: Option<ResourceRef>,
    pub script: Option<CanonicalText>,
    pub phases: Vec<WorkflowPhase>,
    pub workers: Vec<WorkflowWorker>,
    pub logs: Vec<String>,
    pub result_kind: Option<WorkflowResultKind>,
    pub result: Option<CanonicalText>,
    pub error: Option<CanonicalText>,
    pub resources: Vec<ResourceRef>,
    /// Read-time facts from all Workflow tool rows carrying this run id.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attempts: Vec<WorkflowAttempt>,
    /// Exact count even if a pathological run exceeds the wire safety bound.
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub attempt_total_count: u64,
    /// Read-time journal window and exact aggregate counts. The durable
    /// append-only observations stay in `workflow_journal`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub journal: Vec<WorkflowJournalObservation>,
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub journal_total_count: u64,
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub journal_started_count: u64,
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub journal_result_count: u64,
}

impl WorkflowRun {
    pub(crate) fn externalize_text_members(&mut self) -> Vec<ProjectionAsset> {
        let mut assets = Vec::new();
        externalize_optional_text(&mut self.summary, &mut assets);
        externalize_optional_text(&mut self.arguments, &mut assets);
        externalize_optional_text(&mut self.script, &mut assets);
        externalize_optional_text(&mut self.result, &mut assets);
        externalize_optional_text(&mut self.error, &mut assets);
        assets
    }

    pub fn validate(&self) -> Result<(), CanonicalError> {
        required_bounded("workflow run id", &self.run_id, ID_MAX_BYTES)?;
        bounded_option("workflow task id", self.task_id.as_deref(), ID_MAX_BYTES)?;
        bounded_option("workflow name", self.name.as_deref(), ID_MAX_BYTES)?;
        validate_optional_text(self.summary.as_ref())?;
        bounded_option(
            "workflow default model",
            self.default_model.as_deref(),
            ID_MAX_BYTES,
        )?;
        validate_optional_text(self.arguments.as_ref())?;
        validate_optional_resource(self.script_path.as_ref())?;
        validate_optional_text(self.script.as_ref())?;
        bounded_collection("workflow phases", self.phases.len(), WORKFLOW_PHASE_LIMIT)?;
        for phase in &self.phases {
            required_bounded(
                "workflow phase title",
                &phase.title,
                EXTERNAL_LABEL_MAX_BYTES,
            )?;
            bounded_option(
                "workflow phase detail",
                phase.detail.as_deref(),
                CORE_SCALAR_MAX_BYTES,
            )?;
        }
        bounded_collection(
            "workflow workers",
            self.workers.len(),
            WORKFLOW_WORKER_LIMIT,
        )?;
        for worker in &self.workers {
            bounded_option("workflow worker key", worker.key.as_deref(), ID_MAX_BYTES)?;
            required_bounded(
                "workflow worker label",
                &worker.label,
                EXTERNAL_LABEL_MAX_BYTES,
            )?;
            bounded_option(
                "workflow worker phase",
                worker.phase_title.as_deref(),
                EXTERNAL_LABEL_MAX_BYTES,
            )?;
            bounded_option(
                "workflow configured model",
                worker.configured_model.as_deref(),
                ID_MAX_BYTES,
            )?;
            bounded_option(
                "workflow configured effort",
                worker.configured_effort.as_deref(),
                ID_MAX_BYTES,
            )?;
            bounded_collection(
                "workflow observed models",
                worker.observed_models.len(),
                WORKFLOW_RUNTIME_PROFILE_LIMIT,
            )?;
            bounded_collection(
                "workflow observed efforts",
                worker.observed_efforts.len(),
                WORKFLOW_RUNTIME_PROFILE_LIMIT,
            )?;
            for value in worker
                .observed_models
                .iter()
                .chain(worker.observed_efforts.iter())
            {
                required_bounded("workflow observed runtime", value, ID_MAX_BYTES)?;
            }
            bounded_option(
                "workflow worker session",
                worker.child_session_id.as_deref(),
                ID_MAX_BYTES,
            )?;
            for (field, value) in [
                ("workflow prompt preview", worker.prompt_preview.as_deref()),
                ("workflow result preview", worker.result_preview.as_deref()),
                ("workflow last tool", worker.last_tool_name.as_deref()),
                ("workflow tool summary", worker.last_tool_summary.as_deref()),
                ("workflow worker error", worker.error.as_deref()),
            ] {
                bounded_option(field, value, EXTERNAL_VALUE_MAX_BYTES)?;
            }
        }
        bounded_collection("workflow logs", self.logs.len(), WORKFLOW_LOG_LIMIT)?;
        for log in &self.logs {
            required_bounded("workflow log", log, EXTERNAL_VALUE_MAX_BYTES)?;
        }
        validate_optional_text(self.result.as_ref())?;
        validate_optional_text(self.error.as_ref())?;
        bounded_collection(
            "workflow resources",
            self.resources.len(),
            CANONICAL_LIST_LIMIT,
        )?;
        validate_resources(&self.resources)?;
        bounded_collection(
            "workflow attempts",
            self.attempts.len(),
            WORKFLOW_ATTEMPT_LIMIT,
        )?;
        if u64::try_from(self.attempts.len()).unwrap_or(u64::MAX) > self.attempt_total_count {
            return Err(CanonicalError::Invalid(
                "workflow attempt window exceeds its total count".into(),
            ));
        }
        for attempt in &self.attempts {
            required_bounded(
                "workflow attempt timeline id",
                &attempt.timeline_id,
                ID_MAX_BYTES,
            )?;
            if attempt.attempt == 0 {
                return Err(CanonicalError::Invalid(
                    "workflow attempt number must be positive".into(),
                ));
            }
            bounded_option(
                "workflow attempt task id",
                attempt.task_id.as_deref(),
                ID_MAX_BYTES,
            )?;
            bounded_option(
                "workflow attempt summary",
                attempt.summary.as_deref(),
                FAILURE_SUMMARY_MAX_BYTES,
            )?;
        }
        bounded_collection(
            "workflow journal",
            self.journal.len(),
            WORKFLOW_JOURNAL_LIMIT,
        )?;
        if u64::try_from(self.journal.len()).unwrap_or(u64::MAX) > self.journal_total_count {
            return Err(CanonicalError::Invalid(
                "workflow journal window exceeds its total count".into(),
            ));
        }
        if self
            .journal_started_count
            .saturating_add(self.journal_result_count)
            != self.journal_total_count
        {
            return Err(CanonicalError::Invalid(
                "workflow journal aggregate counts disagree".into(),
            ));
        }
        for observation in &self.journal {
            required_bounded(
                "workflow observation id",
                &observation.observation_id,
                ID_MAX_BYTES,
            )?;
            required_bounded(
                "workflow observation run",
                &observation.run_id,
                ID_MAX_BYTES,
            )?;
            if observation.run_id != self.run_id {
                return Err(CanonicalError::Invalid(
                    "workflow journal observation belongs to another run".into(),
                ));
            }
            required_bounded(
                "workflow observation worker",
                &observation.worker_key,
                ID_MAX_BYTES,
            )?;
            bounded_option(
                "workflow observation agent",
                observation.agent_id.as_deref(),
                ID_MAX_BYTES,
            )?;
            bounded_option(
                "workflow observation result",
                observation.result_preview.as_deref(),
                EXTERNAL_VALUE_MAX_BYTES,
            )?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct MediaMetadata {
    pub media_type: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
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
        /// The command is a bounded projection preview. Its exact source text
        /// remains addressable through the row's raw-ledger provenance.
        #[serde(default, skip_serializing_if = "is_false")]
        source_truncated: bool,
        cwd: Option<ResourceRef>,
        #[cfg_attr(feature = "ts", ts(type = "number | null"))]
        timeout_ms: Option<u64>,
        background: bool,
        outcome: Option<ShellOutcome>,
    },
    CodeExecute {
        language: Option<String>,
        code: CanonicalText,
        /// The code is a bounded projection preview. Its exact source text
        /// remains addressable through the row's raw-ledger provenance.
        #[serde(default, skip_serializing_if = "is_false")]
        source_truncated: bool,
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
        #[cfg_attr(feature = "ts", ts(type = "number | null"))]
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
        /// Present only when native evidence proves this invocation addresses
        /// a persistent team rather than an invocation-owned worker.
        team: Option<String>,
        /// Native participant address colour.  Absence stays absence.
        color: Option<ParticipantColor>,
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
        snapshot: Option<TaskSnapshot>,
    },
    Plan {
        operation: PlanOp,
        plan_id: Option<String>,
        title: Option<String>,
        summary: Option<CanonicalText>,
        #[cfg_attr(feature = "ts", ts(type = "number | null"))]
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
        task_id: Option<String>,
        run_id: Option<String>,
        #[cfg_attr(feature = "ts", ts(type = "number | null"))]
        attempt: Option<u64>,
        callback_summary: Option<String>,
        /// Structured callback result retained as canonical JSON/text when a
        /// run sidecar is delayed or absent. The hydrated run remains the
        /// authoritative current snapshot when both exist.
        callback_result: Option<Box<CanonicalText>>,
        usage: Option<WorkflowUsage>,
        /// Hydrated from the generation-owned workflow sidecar at read time;
        /// the stored tool row keeps this `None`, so every attempt points to
        /// one run document instead of copying it.
        run: Option<Box<WorkflowRun>>,
    },
    Schedule {
        operation: ScheduleOp,
        schedule_id: Option<String>,
        due_at: Option<Ms>,
        #[cfg_attr(feature = "ts", ts(type = "number | null"))]
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

impl ToolActionKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::FileRead => "file_read",
            Self::FileList => "file_list",
            Self::FileSearch => "file_search",
            Self::CodeLookup => "code_lookup",
            Self::FileChange => "file_change",
            Self::Shell => "shell",
            Self::CodeExecute => "code_execute",
            Self::Process => "process",
            Self::Orchestration => "orchestration",
            Self::WebSearch => "web_search",
            Self::WebNavigate => "web_navigate",
            Self::WebFetch => "web_fetch",
            Self::Delegate => "delegate",
            Self::Task => "task",
            Self::Plan => "plan",
            Self::Collaborate => "collaborate",
            Self::Question => "question",
            Self::Skill => "skill",
            Self::Workflow => "workflow",
            Self::Schedule => "schedule",
            Self::Artifact => "artifact",
            Self::ToolDiscovery => "tool_discovery",
            Self::Mode => "mode",
            Self::Finding => "finding",
            Self::Notification => "notification",
            Self::StructuredOutput => "structured_output",
            Self::External => "external",
            Self::Unknown => "unknown",
        }
    }
}

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

    /// Replace a shell command or executable code body with the small text
    /// admitted to SQLite. Returns the digest of a projection member made
    /// redundant by the replacement, so the ingest boundary can discard it.
    ///
    /// This deliberately runs *after* adapter normalization. The adapter's
    /// typed draft is still lossless, which lets the raw-ledger reader reuse
    /// the one native parser when a user asks to expand a command.
    pub fn compact_command_preview(&mut self) -> Option<String> {
        let (text, source_truncated) = match self {
            Self::Shell {
                command,
                source_truncated,
                ..
            } => (command, source_truncated),
            Self::CodeExecute {
                code,
                source_truncated,
                ..
            } => (code, source_truncated),
            _ => return None,
        };

        let was_truncated = *source_truncated;
        let retired_member = match text {
            CanonicalText::Member { asset, .. } => Some(asset.digest.clone()),
            CanonicalText::Inline { .. } => None,
        };
        let source = text.as_preview();
        let (preview, clipped) = command_preview(source);
        // A member's preview is already evidence that bytes were omitted,
        // even when its first logical line happens to fit unchanged.
        // Preserve an earlier true value: both the pipeline boundary and the
        // store's defense-in-depth boundary may compact the same action.
        *source_truncated = was_truncated || clipped || retired_member.is_some();
        if clipped || retired_member.is_some() {
            *text = CanonicalText::Inline { text: preview };
        }
        retired_member
    }

    /// Move every lossless user-facing text field above the inline limit into
    /// a content-addressed projection member. Adapter projectors construct a
    /// typed draft; the central message boundary performs this exhaustive
    /// walk so no adapter can forget to emit the member bytes.
    pub(crate) fn externalize_text_members(&mut self) -> Vec<ProjectionAsset> {
        let mut assets = Vec::new();
        match self {
            Self::FileChange { changes } => {
                for change in changes {
                    externalize_optional_text(&mut change.diff, &mut assets);
                }
            }
            Self::Shell { command, .. } => externalize_text(command, &mut assets),
            Self::CodeExecute { code, .. } => externalize_text(code, &mut assets),
            Self::Process { input, .. } => externalize_optional_text(input, &mut assets),
            Self::Delegate {
                objective,
                orphan_result_summary,
                ..
            } => {
                externalize_text(objective, &mut assets);
                externalize_optional_text(orphan_result_summary, &mut assets);
            }
            Self::Task { description, .. } => externalize_optional_text(description, &mut assets),
            Self::Plan { summary, .. } => externalize_optional_text(summary, &mut assets),
            Self::Collaborate { body, .. } => externalize_optional_text(body, &mut assets),
            Self::Question { questions, answers } => {
                for question in questions {
                    externalize_text(&mut question.prompt, &mut assets);
                }
                for answer in answers {
                    for value in &mut answer.values {
                        externalize_text(value, &mut assets);
                    }
                }
            }
            Self::Skill { arguments, .. } => externalize_optional_text(arguments, &mut assets),
            Self::Workflow {
                arguments,
                objective,
                callback_result,
                run,
                ..
            } => {
                externalize_optional_text(arguments, &mut assets);
                externalize_optional_text(objective, &mut assets);
                if let Some(text) = callback_result {
                    externalize_text(text, &mut assets);
                }
                if let Some(run) = run {
                    assets.extend(run.externalize_text_members());
                }
            }
            Self::Schedule { objective, .. } => externalize_optional_text(objective, &mut assets),
            Self::Artifact { description, .. } => {
                externalize_optional_text(description, &mut assets)
            }
            Self::Finding { findings, .. } => {
                for finding in findings {
                    externalize_optional_text(&mut finding.summary, &mut assets);
                }
            }
            Self::Notification { summary, .. } => externalize_text(summary, &mut assets),
            Self::StructuredOutput { document } => {
                if let CanonicalDocument::Inline { summary, .. } = document {
                    externalize_optional_text(summary, &mut assets);
                }
                externalize_document(document, &mut assets);
            }
            Self::FileRead { .. }
            | Self::FileList { .. }
            | Self::FileSearch { .. }
            | Self::CodeLookup { .. }
            | Self::Orchestration { .. }
            | Self::WebSearch { .. }
            | Self::WebNavigate { .. }
            | Self::WebFetch { .. }
            | Self::ToolDiscovery { .. }
            | Self::Mode { .. }
            | Self::External { .. }
            | Self::Unknown {} => {}
        }
        assets
    }

    pub fn validate(&self) -> Result<(), CanonicalError> {
        match self {
            Self::FileRead { targets, .. } => {
                bounded_collection("file read targets", targets.len(), CANONICAL_LIST_LIMIT)?;
                if targets.is_empty() {
                    return Err(CanonicalError::Invalid("file read has no target".into()));
                }
                for target in targets {
                    target.resource.validate()?;
                    if target.range.as_ref().is_some_and(|range| range.start == 0) {
                        return Err(CanonicalError::Invalid(
                            "source ranges are one-based".into(),
                        ));
                    }
                }
            }
            Self::FileList {
                directory, filter, ..
            } => {
                directory.validate()?;
                bounded_option("file-list filter", filter.as_deref(), CORE_SCALAR_MAX_BYTES)?;
            }
            Self::FileSearch {
                query,
                roots,
                filters,
                outcome,
                ..
            } => {
                required_bounded("file-search query", query, CORE_SCALAR_MAX_BYTES)?;
                bounded_collection("file-search roots", roots.len(), CANONICAL_LIST_LIMIT)?;
                bounded_collection("file-search filters", filters.len(), CANONICAL_LIST_LIMIT)?;
                validate_resources(roots)?;
                for filter in filters {
                    required_bounded("file-search filter", &filter.value, CORE_SCALAR_MAX_BYTES)?;
                }
                if let Some(outcome) = outcome {
                    bounded_collection(
                        "file-search paths",
                        outcome.paths.len(),
                        SEARCH_PATH_LIMIT,
                    )?;
                    reduced_count(
                        "file-search paths",
                        outcome.paths.len(),
                        outcome.total_paths,
                    )?;
                    validate_resources(&outcome.paths)?;
                }
            }
            Self::CodeLookup {
                query,
                path,
                outcome,
                ..
            } => {
                required_bounded("code lookup query", query, CORE_SCALAR_MAX_BYTES)?;
                validate_optional_resource(path.as_ref())?;
                if let Some(outcome) = outcome {
                    bounded_collection(
                        "code locations",
                        outcome.locations.len(),
                        SEARCH_PATH_LIMIT,
                    )?;
                    reduced_count(
                        "code locations",
                        outcome.locations.len(),
                        outcome.total_count,
                    )?;
                    for location in &outcome.locations {
                        location.resource.validate()?;
                        bounded_option(
                            "code location summary",
                            location.summary.as_deref(),
                            CORE_SCALAR_MAX_BYTES,
                        )?;
                    }
                }
            }
            Self::FileChange { changes } => {
                if changes.is_empty() {
                    return Err(CanonicalError::Invalid("file change has no paths".into()));
                }
                for change in changes {
                    change.path.validate()?;
                    validate_optional_resource(change.destination.as_ref())?;
                    validate_optional_text(change.diff.as_ref())?;
                }
            }
            Self::Shell {
                command,
                cwd,
                outcome,
                ..
            } => {
                validate_required_text("shell command", command)?;
                validate_optional_resource(cwd.as_ref())?;
                if let Some(outcome) = outcome {
                    bounded_option("shell signal", outcome.signal.as_deref(), ID_MAX_BYTES)?;
                    bounded_option(
                        "shell process id",
                        outcome.process_id.as_deref(),
                        ID_MAX_BYTES,
                    )?;
                }
            }
            Self::CodeExecute {
                language,
                code,
                cwd,
                outcome,
                ..
            } => {
                bounded_option("code language", language.as_deref(), ID_MAX_BYTES)?;
                validate_required_text("code", code)?;
                validate_optional_resource(cwd.as_ref())?;
                if let Some(outcome) = outcome {
                    bounded_option("code signal", outcome.signal.as_deref(), ID_MAX_BYTES)?;
                    bounded_option(
                        "code process id",
                        outcome.process_id.as_deref(),
                        ID_MAX_BYTES,
                    )?;
                }
            }
            Self::Process {
                process_id, input, ..
            } => {
                required_bounded("process id", process_id, ID_MAX_BYTES)?;
                validate_optional_text(input.as_ref())?;
            }
            Self::Orchestration {
                runtime,
                executed_children,
                executed_child_count,
                ..
            } => {
                bounded_option("orchestration runtime", runtime.as_deref(), ID_MAX_BYTES)?;
                bounded_collection(
                    "executed child links",
                    executed_children.len(),
                    CANONICAL_LIST_LIMIT,
                )?;
                for child in executed_children {
                    required_bounded("executed child id", child, ID_MAX_BYTES)?;
                }
                if let Some(total) = executed_child_count {
                    reduced_count("executed child links", executed_children.len(), *total)?;
                }
            }
            Self::WebSearch {
                query,
                allowed_domains,
                blocked_domains,
                outcome,
            } => {
                bounded_option("web-search query", query.as_deref(), CORE_SCALAR_MAX_BYTES)?;
                validate_scalar_list("allowed domains", allowed_domains, CANONICAL_LIST_LIMIT)?;
                validate_scalar_list("blocked domains", blocked_domains, CANONICAL_LIST_LIMIT)?;
                if let Some(outcome) = outcome {
                    bounded_collection("web sources", outcome.sources.len(), WEB_SOURCE_LIMIT)?;
                    reduced_count("web sources", outcome.sources.len(), outcome.total_count)?;
                    for source in &outcome.sources {
                        bounded_option(
                            "web source title",
                            source.title.as_deref(),
                            CORE_SCALAR_MAX_BYTES,
                        )?;
                        required_bounded("web source URL", &source.url, CORE_SCALAR_MAX_BYTES)?;
                    }
                }
            }
            Self::WebNavigate {
                url,
                pattern,
                outcome,
                ..
            } => {
                validate_optional_resource(url.as_ref())?;
                bounded_option(
                    "find-in-page pattern",
                    pattern.as_deref(),
                    CORE_SCALAR_MAX_BYTES,
                )?;
                validate_fetch_outcome(outcome.as_ref())?;
            }
            Self::WebFetch {
                url,
                purpose,
                outcome,
            } => {
                url.validate()?;
                bounded_option(
                    "web-fetch purpose",
                    purpose.as_deref(),
                    CORE_SCALAR_MAX_BYTES,
                )?;
                validate_fetch_outcome(outcome.as_ref())?;
            }
            Self::Delegate {
                agent_kind,
                name,
                model,
                mode,
                team,
                objective,
                child_session_id,
                resume_id,
                orphan_result_summary,
                ..
            } => {
                bounded_option("delegate agent kind", agent_kind.as_deref(), ID_MAX_BYTES)?;
                bounded_option("delegate name", name.as_deref(), ID_MAX_BYTES)?;
                bounded_option("delegate model", model.as_deref(), ID_MAX_BYTES)?;
                bounded_option("delegate mode", mode.as_deref(), ID_MAX_BYTES)?;
                bounded_option("delegate team", team.as_deref(), ID_MAX_BYTES)?;
                validate_required_text("delegate objective", objective)?;
                bounded_option(
                    "delegate child session",
                    child_session_id.as_deref(),
                    ID_MAX_BYTES,
                )?;
                bounded_option("delegate resume id", resume_id.as_deref(), ID_MAX_BYTES)?;
                validate_optional_text(orphan_result_summary.as_ref())?;
                if child_session_id.is_some() && orphan_result_summary.is_some() {
                    return Err(CanonicalError::Invalid(
                        "delegate result summary duplicates a linked child transcript".into(),
                    ));
                }
            }
            Self::Task {
                task_id,
                subject,
                description,
                owner,
                blocker_ids,
                snapshot,
                ..
            } => {
                bounded_option("task id", task_id.as_deref(), ID_MAX_BYTES)?;
                bounded_option("task subject", subject.as_deref(), CORE_SCALAR_MAX_BYTES)?;
                validate_optional_text(description.as_ref())?;
                bounded_option("task owner", owner.as_deref(), ID_MAX_BYTES)?;
                validate_id_list("task blocker ids", blocker_ids, CANONICAL_LIST_LIMIT)?;
                if let Some(snapshot) = snapshot {
                    bounded_collection(
                        "task snapshot items",
                        snapshot.tasks.len(),
                        TASK_SNAPSHOT_LIMIT,
                    )?;
                    reduced_count(
                        "task snapshot items",
                        snapshot.tasks.len(),
                        snapshot.total_count,
                    )?;
                    for task in &snapshot.tasks {
                        required_bounded("task snapshot id", &task.task_id, ID_MAX_BYTES)?;
                        bounded_option(
                            "task snapshot subject",
                            task.subject.as_deref(),
                            CORE_SCALAR_MAX_BYTES,
                        )?;
                        bounded_option("task snapshot owner", task.owner.as_deref(), ID_MAX_BYTES)?;
                        validate_id_list(
                            "task snapshot blocker ids",
                            &task.blocker_ids,
                            CANONICAL_LIST_LIMIT,
                        )?;
                    }
                }
            }
            Self::Plan {
                plan_id,
                title,
                summary,
                ..
            } => {
                bounded_option("plan id", plan_id.as_deref(), ID_MAX_BYTES)?;
                bounded_option("plan title", title.as_deref(), CORE_SCALAR_MAX_BYTES)?;
                validate_optional_text(summary.as_ref())?;
            }
            Self::Collaborate {
                recipient,
                team,
                summary,
                body,
                linked_session_id,
                delivery_id,
                outcome,
                ..
            } => {
                bounded_option(
                    "collaboration recipient",
                    recipient.as_deref(),
                    ID_MAX_BYTES,
                )?;
                bounded_option("collaboration team", team.as_deref(), ID_MAX_BYTES)?;
                bounded_option(
                    "collaboration summary",
                    summary.as_deref(),
                    CORE_SCALAR_MAX_BYTES,
                )?;
                validate_optional_text(body.as_ref())?;
                bounded_option(
                    "collaboration session id",
                    linked_session_id.as_deref(),
                    ID_MAX_BYTES,
                )?;
                bounded_option(
                    "collaboration delivery id",
                    delivery_id.as_deref(),
                    ID_MAX_BYTES,
                )?;
                if let Some(outcome) = outcome {
                    bounded_collection(
                        "collaboration participants",
                        outcome.participants.len(),
                        CANONICAL_LIST_LIMIT,
                    )?;
                    reduced_count(
                        "collaboration participants",
                        outcome.participants.len(),
                        outcome.total_count,
                    )?;
                    for participant in &outcome.participants {
                        required_bounded("collaborator name", &participant.name, ID_MAX_BYTES)?;
                        bounded_option(
                            "collaborator role",
                            participant.role.as_deref(),
                            ID_MAX_BYTES,
                        )?;
                        bounded_option(
                            "collaborator session id",
                            participant.session_id.as_deref(),
                            ID_MAX_BYTES,
                        )?;
                    }
                }
            }
            Self::Question { questions, answers } => {
                for question in questions {
                    required_bounded("question id", &question.id, ID_MAX_BYTES)?;
                    validate_required_text("question prompt", &question.prompt)?;
                    bounded_option(
                        "question header",
                        question.header.as_deref(),
                        CORE_SCALAR_MAX_BYTES,
                    )?;
                    bounded_collection(
                        "question options",
                        question.options.len(),
                        CANONICAL_LIST_LIMIT,
                    )?;
                    for option in &question.options {
                        required_bounded("question option id", &option.id, ID_MAX_BYTES)?;
                        required_bounded(
                            "question option label",
                            &option.label,
                            CORE_SCALAR_MAX_BYTES,
                        )?;
                        bounded_option(
                            "question option description",
                            option.description.as_deref(),
                            CORE_SCALAR_MAX_BYTES,
                        )?;
                    }
                }
                for answer in answers {
                    required_bounded("answer question id", &answer.question_id, ID_MAX_BYTES)?;
                    bounded_collection("answer values", answer.values.len(), CANONICAL_LIST_LIMIT)?;
                    for value in &answer.values {
                        value.validate()?;
                    }
                }
            }
            Self::Skill { name, arguments } => {
                required_bounded("skill name", name, ID_MAX_BYTES)?;
                validate_optional_text(arguments.as_ref())?;
            }
            Self::Workflow {
                name,
                path,
                arguments,
                objective,
                resume_id,
                task_id,
                run_id,
                callback_summary,
                callback_result,
                run,
                ..
            } => {
                bounded_option("workflow name", name.as_deref(), ID_MAX_BYTES)?;
                validate_optional_resource(path.as_ref())?;
                validate_optional_text(arguments.as_ref())?;
                validate_optional_text(objective.as_ref())?;
                bounded_option("workflow resume id", resume_id.as_deref(), ID_MAX_BYTES)?;
                bounded_option("workflow task id", task_id.as_deref(), ID_MAX_BYTES)?;
                bounded_option("workflow run id", run_id.as_deref(), ID_MAX_BYTES)?;
                bounded_option(
                    "workflow callback summary",
                    callback_summary.as_deref(),
                    FAILURE_SUMMARY_MAX_BYTES,
                )?;
                validate_optional_text(callback_result.as_deref())?;
                if let Some(run) = run {
                    if run_id.as_deref() != Some(&run.run_id) {
                        return Err(CanonicalError::Invalid(
                            "workflow action and hydrated run ids disagree".into(),
                        ));
                    }
                    run.validate()?;
                }
            }
            Self::Schedule {
                schedule_id,
                reason,
                objective,
                ..
            } => {
                bounded_option("schedule id", schedule_id.as_deref(), ID_MAX_BYTES)?;
                bounded_option("schedule reason", reason.as_deref(), CORE_SCALAR_MAX_BYTES)?;
                validate_optional_text(objective.as_ref())?;
            }
            Self::Artifact {
                name,
                description,
                resources,
                media,
                ..
            } => {
                bounded_option("artifact name", name.as_deref(), CORE_SCALAR_MAX_BYTES)?;
                validate_optional_text(description.as_ref())?;
                bounded_collection("artifact resources", resources.len(), CANONICAL_LIST_LIMIT)?;
                validate_resources(resources)?;
                if let Some(media) = media {
                    bounded_option(
                        "artifact media type",
                        media.media_type.as_deref(),
                        ID_MAX_BYTES,
                    )?;
                }
            }
            Self::ToolDiscovery {
                query,
                matches,
                total_count,
            } => {
                required_bounded("tool discovery query", query, CORE_SCALAR_MAX_BYTES)?;
                validate_id_list("tool discovery matches", matches, CANONICAL_LIST_LIMIT)?;
                reduced_count("tool discovery matches", matches.len(), *total_count)?;
            }
            Self::Mode { workspace, .. } => validate_optional_resource(workspace.as_ref())?,
            Self::Finding {
                findings,
                total_count,
            } => {
                bounded_collection("findings", findings.len(), CANONICAL_LIST_LIMIT)?;
                reduced_count("findings", findings.len(), *total_count)?;
                for finding in findings {
                    required_bounded("finding title", &finding.title, CORE_SCALAR_MAX_BYTES)?;
                    validate_optional_text(finding.summary.as_ref())?;
                    bounded_collection(
                        "finding resources",
                        finding.resources.len(),
                        CANONICAL_LIST_LIMIT,
                    )?;
                    validate_resources(&finding.resources)?;
                }
            }
            Self::Notification {
                channel,
                destination,
                summary,
                delivery_id,
            } => {
                bounded_option("notification channel", channel.as_deref(), ID_MAX_BYTES)?;
                bounded_option(
                    "notification destination",
                    destination.as_deref(),
                    CORE_SCALAR_MAX_BYTES,
                )?;
                validate_required_text("notification summary", summary)?;
                bounded_option(
                    "notification delivery id",
                    delivery_id.as_deref(),
                    ID_MAX_BYTES,
                )?;
            }
            Self::StructuredOutput { document } => validate_document(document)?,
            Self::External {
                provider,
                operation,
                subject,
                fields,
                resources,
            } => {
                bounded_option("external provider", provider.as_deref(), ID_MAX_BYTES)?;
                required_bounded("external operation", operation, ID_MAX_BYTES)?;
                bounded_option(
                    "external subject",
                    subject.as_deref(),
                    CORE_SCALAR_MAX_BYTES,
                )?;
                bounded_collection("external fields", fields.len(), EXTERNAL_FIELD_LIMIT)?;
                bounded_collection("external resources", resources.len(), CANONICAL_LIST_LIMIT)?;
                for field in fields {
                    required_bounded(
                        "external field label",
                        &field.label,
                        EXTERNAL_LABEL_MAX_BYTES,
                    )?;
                    validate_display_value(&field.value)?;
                }
                validate_resources(resources)?;
            }
            Self::Unknown {} => {}
        }
        Ok(())
    }
}

fn externalize_optional_text(value: &mut Option<CanonicalText>, assets: &mut Vec<ProjectionAsset>) {
    if let Some(value) = value {
        externalize_text(value, assets);
    }
}

fn externalize_text(value: &mut CanonicalText, assets: &mut Vec<ProjectionAsset>) {
    let CanonicalText::Inline { text } = value else {
        return;
    };
    if text.len() <= INLINE_TEXT_MAX_BYTES {
        return;
    }
    let (replacement, asset) = CanonicalText::externalize(std::mem::take(text));
    *value = replacement;
    if let Some(asset) = asset {
        assets.push(asset);
    }
}

fn externalize_document(document: &mut CanonicalDocument, assets: &mut Vec<ProjectionAsset>) {
    if !matches!(document, CanonicalDocument::Inline { .. }) {
        return;
    }
    let Ok(bytes) = serde_json::to_vec(document) else {
        return;
    };
    if bytes.len() <= INLINE_TEXT_MAX_BYTES {
        return;
    }
    let CanonicalDocument::Inline { title, summary, .. } = document else {
        unreachable!("document kind checked before serialization")
    };
    let preview = summary
        .as_ref()
        .map(CanonicalText::as_preview)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| std::str::from_utf8(&bytes).unwrap_or_default());
    let preview = utf8_prefix(preview, TEXT_PREVIEW_MAX_BYTES).to_owned();
    let title = title.clone();
    let asset = ProjectionAsset::new("application/vnd.rosetta.canonical-document+json", bytes)
        .expect("canonical document JSON is an allowlisted projection member");
    let reference = AssetRef {
        url: asset
            .url()
            .expect("canonical document media type has a stable extension"),
        digest: asset.digest.clone(),
        media_type: asset.media_type.clone(),
        byte_len: u64::try_from(asset.bytes.len()).unwrap_or(u64::MAX),
    };
    *document = CanonicalDocument::Member {
        asset: reference,
        title,
        preview,
    };
    assets.push(asset);
}

fn validate_optional_text(value: Option<&CanonicalText>) -> Result<(), CanonicalError> {
    if let Some(value) = value {
        value.validate()?;
    }
    Ok(())
}

fn validate_required_text(
    field: &'static str,
    value: &CanonicalText,
) -> Result<(), CanonicalError> {
    value.validate()?;
    if value.as_preview().trim().is_empty() {
        return Err(CanonicalError::Invalid(format!("{field} is empty")));
    }
    Ok(())
}

fn validate_optional_resource(value: Option<&ResourceRef>) -> Result<(), CanonicalError> {
    if let Some(value) = value {
        value.validate()?;
    }
    Ok(())
}

fn validate_resources(values: &[ResourceRef]) -> Result<(), CanonicalError> {
    for value in values {
        value.validate()?;
    }
    Ok(())
}

fn validate_scalar_list(
    field: &'static str,
    values: &[String],
    limit: usize,
) -> Result<(), CanonicalError> {
    bounded_collection(field, values.len(), limit)?;
    for value in values {
        required_bounded(field, value, CORE_SCALAR_MAX_BYTES)?;
    }
    Ok(())
}

fn validate_id_list(
    field: &'static str,
    values: &[String],
    limit: usize,
) -> Result<(), CanonicalError> {
    bounded_collection(field, values.len(), limit)?;
    for value in values {
        required_bounded(field, value, ID_MAX_BYTES)?;
    }
    Ok(())
}

fn validate_fetch_outcome(value: Option<&FetchOutcome>) -> Result<(), CanonicalError> {
    if let Some(value) = value {
        bounded_option(
            "fetch final URL",
            value.final_url.as_deref(),
            CORE_SCALAR_MAX_BYTES,
        )?;
        bounded_option("fetch title", value.title.as_deref(), CORE_SCALAR_MAX_BYTES)?;
    }
    Ok(())
}

fn reduced_count(field: &'static str, retained: usize, total: u64) -> Result<(), CanonicalError> {
    if total < u64::try_from(retained).unwrap_or(u64::MAX) {
        return Err(CanonicalError::Invalid(format!(
            "{field} total is smaller than the retained list"
        )));
    }
    Ok(())
}

fn validate_display_scalar(value: &DisplayScalar) -> Result<(), CanonicalError> {
    match value {
        DisplayScalar::Text(value) | DisplayScalar::Number(value) => {
            bounded("external scalar", value, EXTERNAL_VALUE_MAX_BYTES)
        }
        DisplayScalar::Boolean(_) => Ok(()),
    }
}

fn validate_display_value(value: &DisplayValue) -> Result<(), CanonicalError> {
    match value {
        DisplayValue::Text(value) | DisplayValue::Number(value) => {
            bounded("external value", value, EXTERNAL_VALUE_MAX_BYTES)
        }
        DisplayValue::Boolean(_) => Ok(()),
        DisplayValue::Resource(resource) => resource.validate(),
        DisplayValue::List(values) => {
            bounded_collection("external list values", values.len(), CANONICAL_LIST_LIMIT)?;
            for value in values {
                validate_display_scalar(value)?;
            }
            Ok(())
        }
    }
}

fn validate_document(document: &CanonicalDocument) -> Result<(), CanonicalError> {
    match document {
        CanonicalDocument::Inline {
            title,
            summary,
            blocks,
            resources,
        } => {
            bounded_option("document title", title.as_deref(), CORE_SCALAR_MAX_BYTES)?;
            validate_optional_text(summary.as_ref())?;
            validate_document_blocks(blocks, 1)?;
            validate_resources(resources)?;
            let bytes = serde_json::to_vec(document)
                .map_err(|error| CanonicalError::Invalid(error.to_string()))?
                .len();
            if bytes > INLINE_TEXT_MAX_BYTES {
                return Err(CanonicalError::Overflow {
                    field: "canonical document",
                    bytes,
                    limit: INLINE_TEXT_MAX_BYTES,
                });
            }
        }
        CanonicalDocument::Member {
            asset,
            title,
            preview,
        } => {
            asset.validate()?;
            if asset.media_type != "application/vnd.rosetta.canonical-document+json" {
                return Err(CanonicalError::Invalid(
                    "canonical document member has the wrong media type".into(),
                ));
            }
            bounded_option("document title", title.as_deref(), CORE_SCALAR_MAX_BYTES)?;
            bounded("document preview", preview, TEXT_PREVIEW_MAX_BYTES)?;
        }
    }
    Ok(())
}

fn validate_document_blocks(blocks: &[DocumentBlock], depth: usize) -> Result<(), CanonicalError> {
    if depth > DOCUMENT_MAX_DEPTH {
        return Err(CanonicalError::Invalid(
            "canonical document exceeds maximum section depth".into(),
        ));
    }
    for block in blocks {
        match block {
            DocumentBlock::Field { field } => {
                required_bounded(
                    "document field label",
                    &field.label,
                    EXTERNAL_LABEL_MAX_BYTES,
                )?;
                validate_display_value(&field.value)?;
            }
            DocumentBlock::List { heading, items } => {
                bounded_option(
                    "document list heading",
                    heading.as_deref(),
                    CORE_SCALAR_MAX_BYTES,
                )?;
                for item in items {
                    validate_display_scalar(item)?;
                }
            }
            DocumentBlock::Table {
                heading,
                columns,
                rows,
                total_rows,
            } => {
                bounded_option(
                    "document table heading",
                    heading.as_deref(),
                    CORE_SCALAR_MAX_BYTES,
                )?;
                for column in columns {
                    required_bounded("document table column", column, EXTERNAL_LABEL_MAX_BYTES)?;
                }
                reduced_count("document table rows", rows.len(), *total_rows)?;
                for row in rows {
                    if row.len() != columns.len() {
                        return Err(CanonicalError::Invalid(
                            "document table row width does not match its columns".into(),
                        ));
                    }
                    for value in row {
                        validate_display_scalar(value)?;
                    }
                }
            }
            DocumentBlock::Section { heading, blocks } => {
                required_bounded("document section heading", heading, CORE_SCALAR_MAX_BYTES)?;
                validate_document_blocks(blocks, depth + 1)?;
            }
        }
    }
    Ok(())
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
        required_bounded("tool correlation key", &self.correlation.key, ID_MAX_BYTES)?;
        required_bounded("tool name", &self.native.name, ID_MAX_BYTES)?;
        bounded_option("tool native id", self.native.id.as_deref(), ID_MAX_BYTES)?;
        bounded_option(
            "tool namespace",
            self.native.namespace.as_deref(),
            ID_MAX_BYTES,
        )?;
        bounded_option(
            "tool provider",
            self.native.provider.as_deref(),
            ID_MAX_BYTES,
        )?;
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
        if let Some(timing) = self.timing
            && timing
                .completed_at
                .zip(timing.started_at)
                .is_some_and(|(end, start)| end < start)
        {
            return Err(CanonicalError::Invalid(
                "tool completion precedes tool start".into(),
            ));
        }
        self.action.validate()?;
        Ok(())
    }

    /// Merge one closed lifecycle patch using deterministic evidence order.
    /// Lower-authority facts may fill an absent slot but can never overwrite a
    /// stronger fact; byte-identical replay of the same evidence is a no-op.
    pub fn apply_patch(
        &mut self,
        state: &mut ToolFoldState,
        patch: &ToolUpdatePatch,
        evidence: &EvidenceOrder,
    ) -> Result<bool, CanonicalError> {
        let mut changed = false;

        if let Some(native_id) = patch.native_id.as_ref().filter(|id| !id.is_empty()) {
            let stamp = EvidenceStamp {
                rank: 100,
                order: evidence.clone(),
            };
            changed |= merge_optional_value(
                &mut self.native.id,
                &Some(native_id.clone()),
                state,
                ToolField::NativeId,
                &stamp,
            );
        }

        if let Some(request) = &patch.request {
            checked_action_family(&self.action, &request.action)?;
            let stamp = EvidenceStamp {
                rank: 100,
                order: evidence.clone(),
            };
            changed |= merge_action_fields(
                &mut self.action,
                &request.action,
                ToolPatchPhase::Request,
                state,
                &stamp,
            );
        }

        if let Some(outcome) = &patch.outcome {
            checked_action_family(&self.action, &outcome.action)?;
            let stamp = EvidenceStamp {
                rank: 110,
                order: evidence.clone(),
            };
            changed |= merge_action_fields(
                &mut self.action,
                &outcome.action,
                ToolPatchPhase::Outcome,
                state,
                &stamp,
            );
        }

        if let Some(child_session_id) = patch
            .delegate_child_session_id
            .as_ref()
            .filter(|id| !id.is_empty())
        {
            let ToolAction::Delegate {
                child_session_id: current,
                ..
            } = &mut self.action
            else {
                return Err(CanonicalError::Invalid(
                    "delegate child link applied to a non-delegate action".into(),
                ));
            };
            let stamp = EvidenceStamp {
                rank: 110,
                order: evidence.clone(),
            };
            changed |= merge_optional_value(
                current,
                &Some(child_session_id.clone()),
                state,
                ToolField::ChildSessionId,
                &stamp,
            );
        }

        if let Some(status) = patch.status {
            let stamp = EvidenceStamp {
                rank: status_rank(status.basis),
                order: evidence.clone(),
            };
            changed |= merge_required_value(
                &mut self.status,
                &status,
                state,
                ToolField::LifecycleStatus,
                &stamp,
            );
        }

        if let Some(failure) = &patch.failure {
            let stamp = EvidenceStamp {
                rank: patch.status.map_or(100, |status| status_rank(status.basis)),
                order: evidence.clone(),
            };
            let mut merged = self.failure.clone().unwrap_or(ToolFailure {
                kind: failure.kind,
                summary: None,
                diagnostic: None,
            });
            let mut failure_changed = merge_required_value(
                &mut merged.kind,
                &failure.kind,
                state,
                ToolField::FailureKind,
                &stamp,
            );
            failure_changed |= merge_optional_value(
                &mut merged.summary,
                &failure.summary,
                state,
                ToolField::FailureSummary,
                &stamp,
            );
            failure_changed |= merge_optional_value(
                &mut merged.diagnostic,
                &failure.diagnostic,
                state,
                ToolField::FailureDiagnostic,
                &stamp,
            );
            if failure_changed {
                self.failure = Some(merged);
                changed = true;
            }
        }

        if let Some(timing) = patch.timing {
            let stamp = EvidenceStamp {
                rank: patch.status.map_or(100, |status| status_rank(status.basis)),
                order: evidence.clone(),
            };
            let mut merged = self.timing.unwrap_or(ToolTiming {
                started_at: None,
                completed_at: None,
                duration_ms: None,
            });
            let mut timing_changed = merge_optional_value(
                &mut merged.started_at,
                &timing.started_at,
                state,
                ToolField::StartedAt,
                &stamp,
            );
            timing_changed |= merge_optional_value(
                &mut merged.completed_at,
                &timing.completed_at,
                state,
                ToolField::CompletedAt,
                &stamp,
            );
            timing_changed |= merge_optional_value(
                &mut merged.duration_ms,
                &timing.duration_ms,
                state,
                ToolField::DurationMs,
                &stamp,
            );
            if timing_changed {
                self.timing = Some(merged);
                changed = true;
            }
        }

        self.validate()?;
        Ok(changed)
    }

    /// Exhaustive semantic extraction for a tool row. Known tools index the
    /// canonical family; only an unknown action exposes its bounded native
    /// namespace/name as the audit label promised by the public contract.
    pub fn search_lanes(&self) -> SearchLanes {
        let mut lanes = SearchLanes::default();
        match &self.action {
            // Codex's opaque outer `exec` envelope proves only that a batch
            // ran.  With no authoritative child trace it has no user-facing
            // target, intent, change, or useful tool label to search.
            ToolAction::Orchestration {
                trace: ChildTrace::Unavailable,
                executed_children,
                executed_child_count,
                ..
            } if executed_children.is_empty()
                && executed_child_count.is_none_or(|count| count == 0) => {}
            ToolAction::Unknown {} => {
                let label = self.native.namespace.as_deref().map_or_else(
                    || self.native.name.clone(),
                    |namespace| format!("{namespace}:{}", self.native.name),
                );
                SearchLanes::push(&mut lanes.tool, &label);
            }
            _ => {
                SearchLanes::push(&mut lanes.tool, self.action.kind().as_str());
                self.action.extract_search(&mut lanes);
            }
        }
        lanes
    }
}

fn checked_action_family(
    current: &ToolAction,
    incoming: &ToolAction,
) -> Result<(), CanonicalError> {
    if current.kind() != ToolActionKind::Unknown && current.kind() != incoming.kind() {
        return Err(CanonicalError::Invalid(format!(
            "tool action family changed from {} to {}",
            current.kind().as_str(),
            incoming.kind().as_str()
        )));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolPatchPhase {
    Request,
    Outcome,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum ToolField {
    NativeId,
    LifecycleStatus,
    FailureKind,
    FailureSummary,
    FailureDiagnostic,
    StartedAt,
    CompletedAt,
    DurationMs,
    Targets,
    Outcome,
    Directory,
    Depth,
    Filter,
    Query,
    Mode,
    Roots,
    Filters,
    Operation,
    Path,
    Changes,
    Command,
    SourceTruncated,
    Cwd,
    TimeoutMs,
    Background,
    Language,
    Code,
    ProcessId,
    Input,
    Runtime,
    Trace,
    ExecutedChildren,
    ExecutedChildCount,
    AllowedDomains,
    BlockedDomains,
    Url,
    Pattern,
    Purpose,
    AgentKind,
    Name,
    Model,
    ParticipantColor,
    Objective,
    ChildSessionId,
    ResumeId,
    OrphanResultSummary,
    TaskId,
    Subject,
    Description,
    TaskStatus,
    Owner,
    BlockerIds,
    TaskSnapshot,
    PlanId,
    Title,
    Summary,
    StepCount,
    Recipient,
    Team,
    Body,
    LinkedSessionId,
    DeliveryId,
    Questions,
    Answers,
    Arguments,
    RunId,
    Attempt,
    CallbackSummary,
    CallbackResult,
    WorkflowUsage,
    WorkflowRun,
    ScheduleId,
    DueAt,
    DelayMs,
    Reason,
    ArtifactKind,
    Resources,
    Media,
    Matches,
    TotalCount,
    Workspace,
    Findings,
    Channel,
    Destination,
    Document,
    Provider,
    Fields,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ToolFieldStamp {
    field: ToolField,
    stamp: EvidenceStamp,
}

fn stamp_wins(current: Option<&EvidenceStamp>, incoming: &EvidenceStamp) -> bool {
    current.is_none_or(|current| {
        incoming.rank > current.rank
            || (incoming.rank == current.rank && incoming.order > current.order)
    })
}

fn update_field_stamp(
    state: &mut ToolFoldState,
    field: ToolField,
    incoming: &EvidenceStamp,
) -> bool {
    match state
        .fields
        .binary_search_by_key(&field, |entry| entry.field)
    {
        Ok(index) if stamp_wins(Some(&state.fields[index].stamp), incoming) => {
            state.fields[index].stamp = incoming.clone();
            true
        }
        Ok(_) => false,
        Err(index) => {
            state.fields.insert(
                index,
                ToolFieldStamp {
                    field,
                    stamp: incoming.clone(),
                },
            );
            true
        }
    }
}

fn merge_required_value<T: Clone + PartialEq>(
    current: &mut T,
    incoming: &T,
    state: &mut ToolFoldState,
    field: ToolField,
    stamp: &EvidenceStamp,
) -> bool {
    if !update_field_stamp(state, field, stamp) {
        return false;
    }
    if current != incoming {
        current.clone_from(incoming);
    }
    true
}

fn merge_optional_value<T: Clone + PartialEq>(
    current: &mut Option<T>,
    incoming: &Option<T>,
    state: &mut ToolFoldState,
    field: ToolField,
    stamp: &EvidenceStamp,
) -> bool {
    let Some(incoming) = incoming else {
        return false;
    };
    if !update_field_stamp(state, field, stamp) {
        return false;
    }
    if current.as_ref() != Some(incoming) {
        *current = Some(incoming.clone());
    }
    true
}

fn merge_action_fields(
    current: &mut ToolAction,
    incoming: &ToolAction,
    phase: ToolPatchPhase,
    state: &mut ToolFoldState,
    stamp: &EvidenceStamp,
) -> bool {
    if current.kind() == ToolActionKind::Unknown && incoming.kind() != ToolActionKind::Unknown {
        *current = incoming.clone();
    }

    let mut changed = false;
    macro_rules! required {
        ($field:ident, $current:expr, $incoming:expr) => {
            changed |= merge_required_value($current, $incoming, state, ToolField::$field, stamp)
        };
    }
    macro_rules! optional {
        ($field:ident, $current:expr, $incoming:expr) => {
            changed |= merge_optional_value($current, $incoming, state, ToolField::$field, stamp)
        };
    }

    match (current, incoming) {
        (
            ToolAction::FileRead {
                targets: current_targets,
                outcome: current_outcome,
            },
            ToolAction::FileRead {
                targets: incoming_targets,
                outcome: incoming_outcome,
            },
        ) => match phase {
            ToolPatchPhase::Request => required!(Targets, current_targets, incoming_targets),
            ToolPatchPhase::Outcome => optional!(Outcome, current_outcome, incoming_outcome),
        },
        (
            ToolAction::FileList {
                directory: current_directory,
                depth: current_depth,
                filter: current_filter,
                outcome: current_outcome,
            },
            ToolAction::FileList {
                directory: incoming_directory,
                depth: incoming_depth,
                filter: incoming_filter,
                outcome: incoming_outcome,
            },
        ) => match phase {
            ToolPatchPhase::Request => {
                required!(Directory, current_directory, incoming_directory);
                optional!(Depth, current_depth, incoming_depth);
                optional!(Filter, current_filter, incoming_filter);
            }
            ToolPatchPhase::Outcome => optional!(Outcome, current_outcome, incoming_outcome),
        },
        (
            ToolAction::FileSearch {
                query: current_query,
                mode: current_mode,
                roots: current_roots,
                filters: current_filters,
                outcome: current_outcome,
            },
            ToolAction::FileSearch {
                query: incoming_query,
                mode: incoming_mode,
                roots: incoming_roots,
                filters: incoming_filters,
                outcome: incoming_outcome,
            },
        ) => match phase {
            ToolPatchPhase::Request => {
                required!(Query, current_query, incoming_query);
                required!(Mode, current_mode, incoming_mode);
                required!(Roots, current_roots, incoming_roots);
                required!(Filters, current_filters, incoming_filters);
            }
            ToolPatchPhase::Outcome => optional!(Outcome, current_outcome, incoming_outcome),
        },
        (
            ToolAction::CodeLookup {
                operation: current_operation,
                query: current_query,
                path: current_path,
                outcome: current_outcome,
            },
            ToolAction::CodeLookup {
                operation: incoming_operation,
                query: incoming_query,
                path: incoming_path,
                outcome: incoming_outcome,
            },
        ) => match phase {
            ToolPatchPhase::Request => {
                required!(Operation, current_operation, incoming_operation);
                required!(Query, current_query, incoming_query);
                optional!(Path, current_path, incoming_path);
            }
            ToolPatchPhase::Outcome => optional!(Outcome, current_outcome, incoming_outcome),
        },
        (
            ToolAction::FileChange {
                changes: current_changes,
            },
            ToolAction::FileChange {
                changes: incoming_changes,
            },
        ) => required!(Changes, current_changes, incoming_changes),
        (
            ToolAction::Shell {
                command: current_command,
                source_truncated: current_source_truncated,
                cwd: current_cwd,
                timeout_ms: current_timeout,
                background: current_background,
                outcome: current_outcome,
            },
            ToolAction::Shell {
                command: incoming_command,
                source_truncated: incoming_source_truncated,
                cwd: incoming_cwd,
                timeout_ms: incoming_timeout,
                background: incoming_background,
                outcome: incoming_outcome,
            },
        ) => match phase {
            ToolPatchPhase::Request => {
                required!(Command, current_command, incoming_command);
                required!(
                    SourceTruncated,
                    current_source_truncated,
                    incoming_source_truncated
                );
                optional!(Cwd, current_cwd, incoming_cwd);
                optional!(TimeoutMs, current_timeout, incoming_timeout);
                required!(Background, current_background, incoming_background);
            }
            ToolPatchPhase::Outcome => optional!(Outcome, current_outcome, incoming_outcome),
        },
        (
            ToolAction::CodeExecute {
                language: current_language,
                code: current_code,
                source_truncated: current_source_truncated,
                cwd: current_cwd,
                outcome: current_outcome,
            },
            ToolAction::CodeExecute {
                language: incoming_language,
                code: incoming_code,
                source_truncated: incoming_source_truncated,
                cwd: incoming_cwd,
                outcome: incoming_outcome,
            },
        ) => match phase {
            ToolPatchPhase::Request => {
                optional!(Language, current_language, incoming_language);
                required!(Code, current_code, incoming_code);
                required!(
                    SourceTruncated,
                    current_source_truncated,
                    incoming_source_truncated
                );
                optional!(Cwd, current_cwd, incoming_cwd);
            }
            ToolPatchPhase::Outcome => optional!(Outcome, current_outcome, incoming_outcome),
        },
        (
            ToolAction::Process {
                operation: current_operation,
                process_id: current_process_id,
                input: current_input,
                outcome: current_outcome,
            },
            ToolAction::Process {
                operation: incoming_operation,
                process_id: incoming_process_id,
                input: incoming_input,
                outcome: incoming_outcome,
            },
        ) => match phase {
            ToolPatchPhase::Request => {
                required!(Operation, current_operation, incoming_operation);
                required!(ProcessId, current_process_id, incoming_process_id);
                optional!(Input, current_input, incoming_input);
            }
            ToolPatchPhase::Outcome => optional!(Outcome, current_outcome, incoming_outcome),
        },
        (
            ToolAction::Orchestration {
                runtime: current_runtime,
                trace: current_trace,
                executed_children: current_children,
                executed_child_count: current_count,
            },
            ToolAction::Orchestration {
                runtime: incoming_runtime,
                trace: incoming_trace,
                executed_children: incoming_children,
                executed_child_count: incoming_count,
            },
        ) => match phase {
            ToolPatchPhase::Request => {
                optional!(Runtime, current_runtime, incoming_runtime);
                required!(Trace, current_trace, incoming_trace);
            }
            ToolPatchPhase::Outcome => {
                required!(ExecutedChildren, current_children, incoming_children);
                optional!(ExecutedChildCount, current_count, incoming_count);
            }
        },
        (
            ToolAction::WebSearch {
                query: current_query,
                allowed_domains: current_allowed,
                blocked_domains: current_blocked,
                outcome: current_outcome,
            },
            ToolAction::WebSearch {
                query: incoming_query,
                allowed_domains: incoming_allowed,
                blocked_domains: incoming_blocked,
                outcome: incoming_outcome,
            },
        ) => match phase {
            ToolPatchPhase::Request => {
                optional!(Query, current_query, incoming_query);
                required!(AllowedDomains, current_allowed, incoming_allowed);
                required!(BlockedDomains, current_blocked, incoming_blocked);
            }
            ToolPatchPhase::Outcome => optional!(Outcome, current_outcome, incoming_outcome),
        },
        (
            ToolAction::WebNavigate {
                operation: current_operation,
                url: current_url,
                pattern: current_pattern,
                outcome: current_outcome,
            },
            ToolAction::WebNavigate {
                operation: incoming_operation,
                url: incoming_url,
                pattern: incoming_pattern,
                outcome: incoming_outcome,
            },
        ) => match phase {
            ToolPatchPhase::Request => {
                required!(Operation, current_operation, incoming_operation);
                optional!(Url, current_url, incoming_url);
                optional!(Pattern, current_pattern, incoming_pattern);
            }
            ToolPatchPhase::Outcome => optional!(Outcome, current_outcome, incoming_outcome),
        },
        (
            ToolAction::WebFetch {
                url: current_url,
                purpose: current_purpose,
                outcome: current_outcome,
            },
            ToolAction::WebFetch {
                url: incoming_url,
                purpose: incoming_purpose,
                outcome: incoming_outcome,
            },
        ) => match phase {
            ToolPatchPhase::Request => {
                required!(Url, current_url, incoming_url);
                optional!(Purpose, current_purpose, incoming_purpose);
            }
            ToolPatchPhase::Outcome => optional!(Outcome, current_outcome, incoming_outcome),
        },
        (
            ToolAction::Delegate {
                agent_kind: current_agent_kind,
                name: current_name,
                model: current_model,
                mode: current_mode,
                team: current_team,
                color: current_color,
                objective: current_objective,
                child_session_id: current_child_session,
                resume_id: current_resume_id,
                background: current_background,
                orphan_result_summary: current_orphan_summary,
            },
            ToolAction::Delegate {
                agent_kind: incoming_agent_kind,
                name: incoming_name,
                model: incoming_model,
                mode: incoming_mode,
                team: incoming_team,
                color: incoming_color,
                objective: incoming_objective,
                child_session_id: incoming_child_session,
                resume_id: incoming_resume_id,
                background: incoming_background,
                orphan_result_summary: incoming_orphan_summary,
            },
        ) => match phase {
            ToolPatchPhase::Request => {
                optional!(AgentKind, current_agent_kind, incoming_agent_kind);
                optional!(Name, current_name, incoming_name);
                optional!(Model, current_model, incoming_model);
                optional!(Mode, current_mode, incoming_mode);
                required!(Objective, current_objective, incoming_objective);
                optional!(ResumeId, current_resume_id, incoming_resume_id);
                required!(Background, current_background, incoming_background);
            }
            ToolPatchPhase::Outcome => {
                optional!(AgentKind, current_agent_kind, incoming_agent_kind);
                optional!(Name, current_name, incoming_name);
                optional!(Model, current_model, incoming_model);
                optional!(Team, current_team, incoming_team);
                optional!(ParticipantColor, current_color, incoming_color);
                optional!(
                    ChildSessionId,
                    current_child_session,
                    incoming_child_session
                );
                optional!(
                    OrphanResultSummary,
                    current_orphan_summary,
                    incoming_orphan_summary
                );
            }
        },
        (
            ToolAction::Task {
                operation: current_operation,
                task_id: current_task_id,
                subject: current_subject,
                description: current_description,
                status: current_status,
                owner: current_owner,
                blocker_ids: current_blockers,
                snapshot: current_snapshot,
            },
            ToolAction::Task {
                operation: incoming_operation,
                task_id: incoming_task_id,
                subject: incoming_subject,
                description: incoming_description,
                status: incoming_status,
                owner: incoming_owner,
                blocker_ids: incoming_blockers,
                snapshot: incoming_snapshot,
            },
        ) => match phase {
            ToolPatchPhase::Request => {
                required!(Operation, current_operation, incoming_operation);
                optional!(TaskId, current_task_id, incoming_task_id);
                optional!(Subject, current_subject, incoming_subject);
                optional!(Description, current_description, incoming_description);
                optional!(Owner, current_owner, incoming_owner);
                required!(BlockerIds, current_blockers, incoming_blockers);
                optional!(TaskSnapshot, current_snapshot, incoming_snapshot);
            }
            ToolPatchPhase::Outcome => optional!(TaskStatus, current_status, incoming_status),
        },
        (
            ToolAction::Plan {
                operation: current_operation,
                plan_id: current_plan_id,
                title: current_title,
                summary: current_summary,
                step_count: current_step_count,
            },
            ToolAction::Plan {
                operation: incoming_operation,
                plan_id: incoming_plan_id,
                title: incoming_title,
                summary: incoming_summary,
                step_count: incoming_step_count,
            },
        ) => {
            if phase == ToolPatchPhase::Request {
                required!(Operation, current_operation, incoming_operation);
                optional!(PlanId, current_plan_id, incoming_plan_id);
                optional!(Title, current_title, incoming_title);
                optional!(Summary, current_summary, incoming_summary);
                optional!(StepCount, current_step_count, incoming_step_count);
            }
        }
        (
            ToolAction::Collaborate {
                operation: current_operation,
                recipient: current_recipient,
                team: current_team,
                summary: current_summary,
                body: current_body,
                linked_session_id: current_linked_session,
                delivery_id: current_delivery_id,
                outcome: current_outcome,
            },
            ToolAction::Collaborate {
                operation: incoming_operation,
                recipient: incoming_recipient,
                team: incoming_team,
                summary: incoming_summary,
                body: incoming_body,
                linked_session_id: incoming_linked_session,
                delivery_id: incoming_delivery_id,
                outcome: incoming_outcome,
            },
        ) => match phase {
            ToolPatchPhase::Request => {
                required!(Operation, current_operation, incoming_operation);
                optional!(Recipient, current_recipient, incoming_recipient);
                optional!(Team, current_team, incoming_team);
                optional!(Summary, current_summary, incoming_summary);
                optional!(Body, current_body, incoming_body);
                optional!(
                    LinkedSessionId,
                    current_linked_session,
                    incoming_linked_session
                );
            }
            ToolPatchPhase::Outcome => {
                optional!(DeliveryId, current_delivery_id, incoming_delivery_id);
                optional!(Outcome, current_outcome, incoming_outcome);
            }
        },
        (
            ToolAction::Question {
                questions: current_questions,
                answers: current_answers,
            },
            ToolAction::Question {
                questions: incoming_questions,
                answers: incoming_answers,
            },
        ) => match phase {
            ToolPatchPhase::Request => {
                required!(Questions, current_questions, incoming_questions)
            }
            ToolPatchPhase::Outcome => required!(Answers, current_answers, incoming_answers),
        },
        (
            ToolAction::Skill {
                name: current_name,
                arguments: current_arguments,
            },
            ToolAction::Skill {
                name: incoming_name,
                arguments: incoming_arguments,
            },
        ) => {
            if phase == ToolPatchPhase::Request {
                required!(Name, current_name, incoming_name);
                optional!(Arguments, current_arguments, incoming_arguments);
            }
        }
        (
            ToolAction::Workflow {
                name: current_name,
                path: current_path,
                arguments: current_arguments,
                objective: current_objective,
                resume_id: current_resume_id,
                background: current_background,
                task_id: current_task_id,
                run_id: current_run_id,
                attempt: current_attempt,
                callback_summary: current_callback_summary,
                callback_result: current_callback_result,
                usage: current_usage,
                run: current_run,
            },
            ToolAction::Workflow {
                name: incoming_name,
                path: incoming_path,
                arguments: incoming_arguments,
                objective: incoming_objective,
                resume_id: incoming_resume_id,
                background: incoming_background,
                task_id: incoming_task_id,
                run_id: incoming_run_id,
                attempt: incoming_attempt,
                callback_summary: incoming_callback_summary,
                callback_result: incoming_callback_result,
                usage: incoming_usage,
                run: incoming_run,
            },
        ) => {
            if phase == ToolPatchPhase::Request {
                optional!(Name, current_name, incoming_name);
                optional!(Path, current_path, incoming_path);
                optional!(Arguments, current_arguments, incoming_arguments);
                optional!(Objective, current_objective, incoming_objective);
                optional!(ResumeId, current_resume_id, incoming_resume_id);
                required!(Background, current_background, incoming_background);
                optional!(TaskId, current_task_id, incoming_task_id);
                optional!(RunId, current_run_id, incoming_run_id);
                optional!(Attempt, current_attempt, incoming_attempt);
                optional!(
                    CallbackSummary,
                    current_callback_summary,
                    incoming_callback_summary
                );
                optional!(
                    CallbackResult,
                    current_callback_result,
                    incoming_callback_result
                );
                optional!(WorkflowUsage, current_usage, incoming_usage);
                optional!(WorkflowRun, current_run, incoming_run);
            }
        }
        (
            ToolAction::Schedule {
                operation: current_operation,
                schedule_id: current_schedule_id,
                due_at: current_due_at,
                delay_ms: current_delay,
                reason: current_reason,
                objective: current_objective,
            },
            ToolAction::Schedule {
                operation: incoming_operation,
                schedule_id: incoming_schedule_id,
                due_at: incoming_due_at,
                delay_ms: incoming_delay,
                reason: incoming_reason,
                objective: incoming_objective,
            },
        ) => {
            if phase == ToolPatchPhase::Request {
                required!(Operation, current_operation, incoming_operation);
                optional!(ScheduleId, current_schedule_id, incoming_schedule_id);
                optional!(DueAt, current_due_at, incoming_due_at);
                optional!(DelayMs, current_delay, incoming_delay);
                optional!(Reason, current_reason, incoming_reason);
                optional!(Objective, current_objective, incoming_objective);
            }
        }
        (
            ToolAction::Artifact {
                operation: current_operation,
                artifact_kind: current_kind,
                name: current_name,
                description: current_description,
                resources: current_resources,
                media: current_media,
            },
            ToolAction::Artifact {
                operation: incoming_operation,
                artifact_kind: incoming_kind,
                name: incoming_name,
                description: incoming_description,
                resources: incoming_resources,
                media: incoming_media,
            },
        ) => match phase {
            ToolPatchPhase::Request => {
                required!(Operation, current_operation, incoming_operation);
                required!(ArtifactKind, current_kind, incoming_kind);
                optional!(Name, current_name, incoming_name);
                optional!(Description, current_description, incoming_description);
            }
            ToolPatchPhase::Outcome => {
                required!(Resources, current_resources, incoming_resources);
                optional!(Media, current_media, incoming_media);
            }
        },
        (
            ToolAction::ToolDiscovery {
                query: current_query,
                matches: current_matches,
                total_count: current_total,
            },
            ToolAction::ToolDiscovery {
                query: incoming_query,
                matches: incoming_matches,
                total_count: incoming_total,
            },
        ) => match phase {
            ToolPatchPhase::Request => required!(Query, current_query, incoming_query),
            ToolPatchPhase::Outcome => {
                required!(Matches, current_matches, incoming_matches);
                required!(TotalCount, current_total, incoming_total);
            }
        },
        (
            ToolAction::Mode {
                operation: current_operation,
                mode: current_mode,
                workspace: current_workspace,
            },
            ToolAction::Mode {
                operation: incoming_operation,
                mode: incoming_mode,
                workspace: incoming_workspace,
            },
        ) => {
            if phase == ToolPatchPhase::Request {
                required!(Operation, current_operation, incoming_operation);
                required!(Mode, current_mode, incoming_mode);
                optional!(Workspace, current_workspace, incoming_workspace);
            }
        }
        (
            ToolAction::Finding {
                findings: current_findings,
                total_count: current_total,
            },
            ToolAction::Finding {
                findings: incoming_findings,
                total_count: incoming_total,
            },
        ) => {
            if phase == ToolPatchPhase::Outcome {
                required!(Findings, current_findings, incoming_findings);
                required!(TotalCount, current_total, incoming_total);
            }
        }
        (
            ToolAction::Notification {
                channel: current_channel,
                destination: current_destination,
                summary: current_summary,
                delivery_id: current_delivery_id,
            },
            ToolAction::Notification {
                channel: incoming_channel,
                destination: incoming_destination,
                summary: incoming_summary,
                delivery_id: incoming_delivery_id,
            },
        ) => match phase {
            ToolPatchPhase::Request => {
                optional!(Channel, current_channel, incoming_channel);
                optional!(Destination, current_destination, incoming_destination);
                required!(Summary, current_summary, incoming_summary);
            }
            ToolPatchPhase::Outcome => {
                optional!(DeliveryId, current_delivery_id, incoming_delivery_id)
            }
        },
        (
            ToolAction::StructuredOutput {
                document: current_document,
            },
            ToolAction::StructuredOutput {
                document: incoming_document,
            },
        ) => {
            if phase == ToolPatchPhase::Outcome {
                required!(Document, current_document, incoming_document);
            }
        }
        (
            ToolAction::External {
                provider: current_provider,
                operation: current_operation,
                subject: current_subject,
                fields: current_fields,
                resources: current_resources,
            },
            ToolAction::External {
                provider: incoming_provider,
                operation: incoming_operation,
                subject: incoming_subject,
                fields: incoming_fields,
                resources: incoming_resources,
            },
        ) => match phase {
            ToolPatchPhase::Request => {
                optional!(Provider, current_provider, incoming_provider);
                required!(Operation, current_operation, incoming_operation);
                optional!(Subject, current_subject, incoming_subject);
            }
            ToolPatchPhase::Outcome => {
                required!(Fields, current_fields, incoming_fields);
                required!(Resources, current_resources, incoming_resources);
            }
        },
        (ToolAction::Unknown {}, ToolAction::Unknown {}) => {}
        _ => unreachable!("tool action family was checked before field merge"),
    }
    changed
}

fn status_rank(basis: StatusBasis) -> u16 {
    match basis {
        StatusBasis::RequestObserved => 10,
        StatusBasis::SourceFinalization => 20,
        StatusBasis::NativeTerminalRecord => 100,
        StatusBasis::NativeState => 110,
        StatusBasis::GroundTruthProtocol => 120,
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
    Collaboration {
        event: Box<CollaborationEvent>,
    },
    ToolUse {
        tool: Box<ToolInteraction>,
    },
    Notice {
        notice: Notice,
    },
    Summary {
        purpose: SummaryPurpose,
        text: CanonicalText,
    },
}

/// Closed non-tool payload stored in `message.detail`. Inline prose remains
/// in the indexed `content` column; this carries only typed references and
/// lifecycle meaning, never a native object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MessageDetail {
    Prose {
        text_member: Option<AssetRef>,
        attachments: Vec<AttachmentRef>,
        citations: Vec<CitationRef>,
    },
    Thinking {
        text_member: Option<AssetRef>,
    },
    Collaboration {
        /// `body` is cleared while encoded because its searchable preview is
        /// stored in `message.content`; `body_member` restores a large body.
        event: Box<CollaborationEvent>,
        body_member: Option<AssetRef>,
    },
    Notice {
        notice: Notice,
    },
    Summary {
        purpose: SummaryPurpose,
        text_member: Option<AssetRef>,
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
    /// Native tool invocation that owns a spawned child-session entry.
    pub parent_tool_native_id: Option<String>,
    pub usage: TokenSnapshot,
    pub provenance: Provenance,
}

impl TimelineEntry {
    pub fn validate(&self) -> Result<(), CanonicalError> {
        required_bounded("timeline id", &self.timeline_id, ID_MAX_BYTES)?;
        required_bounded("session id", &self.session_id, ID_MAX_BYTES)?;
        required_bounded("group id", &self.group.id, ID_MAX_BYTES)?;
        if self.ordinal < 0 {
            return Err(CanonicalError::Invalid(
                "timeline ordinal must be non-negative".into(),
            ));
        }
        if let Some(response) = &self.response {
            required_bounded("response id", &response.id, ID_MAX_BYTES)?;
        }
        bounded_option("native message id", self.native_id.as_deref(), ID_MAX_BYTES)?;
        bounded_option(
            "parent native message id",
            self.parent_native_id.as_deref(),
            ID_MAX_BYTES,
        )?;
        bounded_option("model", self.model.as_deref(), ID_MAX_BYTES)?;
        bounded_option("effort", self.effort.as_deref(), ID_MAX_BYTES)?;
        bounded_option(
            "parent tool native id",
            self.parent_tool_native_id.as_deref(),
            ID_MAX_BYTES,
        )?;
        bounded_option(
            "agent version",
            self.provenance.agent_version.as_deref(),
            ID_MAX_BYTES,
        )?;
        if [
            self.usage.input,
            self.usage.output,
            self.usage.cache_read,
            self.usage.cache_write,
            self.usage.thinking,
        ]
        .into_iter()
        .flatten()
        .any(|value| value < 0)
        {
            return Err(CanonicalError::Invalid(
                "token snapshots cannot be negative".into(),
            ));
        }
        if self.response.is_some() != self.block_index.is_some() {
            return Err(CanonicalError::Invalid(
                "response and block_index must be present together".into(),
            ));
        }
        if self.parent_tool_native_id.is_some()
            && !matches!(&self.body, TimelineBody::ToolUse { .. })
        {
            return Err(CanonicalError::Invalid(
                "only tool-use entries may anchor a child session".into(),
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
            TimelineBody::User {
                text: Some(text), ..
            }
            | TimelineBody::Agent {
                text: Some(text), ..
            }
            | TimelineBody::Thinking { text }
            | TimelineBody::Summary { text, .. } => text.validate()?,
            TimelineBody::Collaboration { event } => event.validate()?,
            TimelineBody::ToolUse { tool } => tool.validate()?,
            TimelineBody::Notice { notice } => notice.validate()?,
            _ => {}
        }
        match &self.body {
            TimelineBody::User { text, attachments } => {
                attachment::validate_attachments(attachments, text.as_ref())?;
            }
            TimelineBody::Agent {
                text,
                attachments,
                citations,
            } => {
                attachment::validate_attachments(attachments, text.as_ref())?;
                bounded_collection("agent citations", citations.len(), CANONICAL_LIST_LIMIT)?;
                for citation in citations {
                    citation.validate()?;
                }
            }
            TimelineBody::ToolUse { .. } => {}
            TimelineBody::Thinking { .. }
            | TimelineBody::Collaboration { .. }
            | TimelineBody::Notice { .. }
            | TimelineBody::Summary { .. } => {}
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct EvidenceStamp {
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
    /// Sparse, user-facing linkage proven by a native spawn/result record.
    /// Applied only to [`ToolAction::Delegate`], so adapters need not invent
    /// request fields merely to attach a child after the opener was emitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegate_child_session_id: Option<String>,
    pub status: Option<ToolStatusFact>,
    pub failure: Option<ToolFailure>,
    pub timing: Option<ToolTiming>,
}

/// Process-local deterministic merge stamps. Values remain in the closed
/// user-facing action; this state stores one evidence stamp per canonical
/// field and intentionally implements no serialization trait.
#[doc(hidden)]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolFoldState {
    fields: Vec<ToolFieldStamp>,
}

impl ToolFoldState {
    /// Merge state for a row RELOADED from the projection, where the live
    /// fold's per-field stamps are gone by design — the only provenance the
    /// store keeps is the winning status basis. Every present status, timing,
    /// and failure field is assumed to carry that basis at the earliest
    /// possible order, so a later cross-batch patch must OUTRANK the row (or
    /// match its rank, the same-rank-correction rule) to move a proven field,
    /// while absent fields stay open for any evidence — mirroring what one
    /// uninterrupted fold over the same records produces. Action sub-fields
    /// are not seeded: a known-family change is rejected at the store
    /// boundary before merge state matters.
    pub fn assume_resolved(tool: &ToolInteraction) -> Self {
        let stamp = EvidenceStamp {
            rank: status_rank(tool.status.basis),
            order: EvidenceOrder {
                source_identity: SourceIdentity(String::new()),
                generation: 0,
                record_offset: 0,
                mutation_index: 0,
            },
        };
        let mut state = Self::default();
        let mut seed = |field: ToolField, present: bool| {
            if present {
                update_field_stamp(&mut state, field, &stamp);
            }
        };
        seed(ToolField::LifecycleStatus, true);
        let timing = tool.timing.as_ref();
        seed(
            ToolField::StartedAt,
            timing.is_some_and(|timing| timing.started_at.is_some()),
        );
        seed(
            ToolField::CompletedAt,
            timing.is_some_and(|timing| timing.completed_at.is_some()),
        );
        seed(
            ToolField::DurationMs,
            timing.is_some_and(|timing| timing.duration_ms.is_some()),
        );
        let failure = tool.failure.as_ref();
        seed(ToolField::FailureKind, failure.is_some());
        seed(
            ToolField::FailureSummary,
            failure.is_some_and(|failure| failure.summary.is_some()),
        );
        seed(
            ToolField::FailureDiagnostic,
            failure.is_some_and(|failure| failure.diagnostic.is_some()),
        );
        state
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchLane {
    Prose,
    Target,
    Intent,
    Change,
    Tool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchMember {
    pub lane: SearchLane,
    pub asset: AssetRef,
    pub preview: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchLanes {
    pub prose: String,
    pub target: String,
    pub intent: String,
    pub change: String,
    pub tool: String,
    pub members: Vec<SearchMember>,
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

    fn push_text(&mut self, lane: SearchLane, value: &CanonicalText) {
        let target = match lane {
            SearchLane::Prose => &mut self.prose,
            SearchLane::Target => &mut self.target,
            SearchLane::Intent => &mut self.intent,
            SearchLane::Change => &mut self.change,
            SearchLane::Tool => &mut self.tool,
        };
        Self::push(target, value.as_preview());
        if let CanonicalText::Member { asset, preview, .. } = value {
            self.members.push(SearchMember {
                lane,
                asset: asset.clone(),
                preview: preview.clone(),
            });
        }
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
                    lanes.push_text(SearchLane::Prose, text);
                }
                for attachment in attachments {
                    if let Some(name) = &attachment.name {
                        SearchLanes::push(&mut lanes.target, name);
                    }
                }
            }
            TimelineBody::Agent {
                text,
                attachments,
                citations,
            } => {
                if let Some(text) = text {
                    lanes.push_text(SearchLane::Prose, text);
                }
                for attachment in attachments {
                    if let Some(name) = &attachment.name {
                        SearchLanes::push(&mut lanes.target, name);
                    }
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
                lanes.push_text(SearchLane::Prose, text);
            }
            TimelineBody::Collaboration { event } => {
                if let Some(body) = &event.body {
                    lanes.push_text(SearchLane::Prose, body);
                }
                for value in [
                    event.sender.as_deref(),
                    event.recipient.as_deref(),
                    event.team.as_deref(),
                ]
                .into_iter()
                .flatten()
                {
                    SearchLanes::push(&mut lanes.target, value);
                }
                if let Some(summary) = &event.summary {
                    SearchLanes::push(&mut lanes.intent, summary);
                }
                if let Some(task_id) = &event.task_id {
                    SearchLanes::push(&mut lanes.target, task_id);
                }
            }
            TimelineBody::Notice {
                notice: Notice::CommandOutput { output },
            } => lanes.push_text(SearchLane::Prose, output),
            TimelineBody::Notice {
                notice:
                    Notice::AsyncTask {
                        task_id,
                        tool_use_id,
                        summary,
                        ..
                    },
            } => {
                for value in [task_id.as_deref(), tool_use_id.as_deref()]
                    .into_iter()
                    .flatten()
                {
                    SearchLanes::push(&mut lanes.target, value);
                }
                if let Some(summary) = summary {
                    SearchLanes::push(&mut lanes.intent, summary);
                }
            }
            TimelineBody::Notice { .. } => {}
            TimelineBody::ToolUse { tool } => {
                let tool_lanes = tool.search_lanes();
                SearchLanes::push(&mut lanes.target, &tool_lanes.target);
                SearchLanes::push(&mut lanes.intent, &tool_lanes.intent);
                SearchLanes::push(&mut lanes.change, &tool_lanes.change);
                SearchLanes::push(&mut lanes.tool, &tool_lanes.tool);
                lanes.members.extend(tool_lanes.members);
            }
        }
        lanes
    }
}

impl ToolAction {
    fn extract_search(&self, lanes: &mut SearchLanes) {
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
                        lanes.push_text(SearchLane::Change, v);
                    }
                }
            }
            Self::Shell { command, cwd, .. } => {
                lanes.push_text(SearchLane::Intent, command);
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
                lanes.push_text(SearchLane::Intent, code);
                if let Some(v) = cwd {
                    resource(lanes, v);
                }
            }
            Self::Process {
                process_id, input, ..
            } => {
                SearchLanes::push(&mut lanes.target, process_id);
                if let Some(v) = input {
                    lanes.push_text(SearchLane::Intent, v);
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
                team,
                objective,
                ..
            } => {
                if let Some(v) = agent_kind {
                    SearchLanes::push(&mut lanes.tool, v);
                }
                if let Some(v) = name {
                    SearchLanes::push(&mut lanes.tool, v);
                }
                if let Some(v) = team {
                    SearchLanes::push(&mut lanes.target, v);
                }
                lanes.push_text(SearchLane::Intent, objective);
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
                    lanes.push_text(SearchLane::Intent, v);
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
                    lanes.push_text(SearchLane::Intent, v);
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
                    lanes.push_text(SearchLane::Intent, v);
                }
            }
            Self::Question { questions, answers } => {
                for v in questions {
                    lanes.push_text(SearchLane::Intent, &v.prompt);
                }
                for v in answers {
                    for answer in &v.values {
                        lanes.push_text(SearchLane::Intent, answer);
                    }
                }
            }
            Self::Skill { name, arguments } => {
                SearchLanes::push(&mut lanes.tool, name);
                if let Some(v) = arguments {
                    lanes.push_text(SearchLane::Intent, v);
                }
            }
            Self::Workflow {
                name,
                path,
                arguments,
                objective,
                callback_summary,
                callback_result,
                run,
                ..
            } => {
                if let Some(v) = name {
                    SearchLanes::push(&mut lanes.tool, v);
                }
                if let Some(v) = path {
                    resource(lanes, v);
                }
                if let Some(v) = arguments {
                    lanes.push_text(SearchLane::Intent, v);
                }
                if let Some(v) = objective {
                    lanes.push_text(SearchLane::Intent, v);
                }
                if let Some(v) = callback_summary {
                    SearchLanes::push(&mut lanes.prose, v);
                }
                if let Some(v) = callback_result {
                    lanes.push_text(SearchLane::Prose, v);
                }
                if let Some(run) = run {
                    if let Some(v) = &run.name {
                        SearchLanes::push(&mut lanes.tool, v);
                    }
                    if let Some(v) = &run.summary {
                        lanes.push_text(SearchLane::Intent, v);
                    }
                    if let Some(v) = &run.result {
                        lanes.push_text(SearchLane::Prose, v);
                    }
                    if let Some(v) = &run.error {
                        lanes.push_text(SearchLane::Prose, v);
                    }
                    for resource_ref in &run.resources {
                        resource(lanes, resource_ref);
                    }
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
                    lanes.push_text(SearchLane::Intent, v);
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
                    lanes.push_text(SearchLane::Intent, v);
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
                        lanes.push_text(SearchLane::Intent, v);
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
                lanes.push_text(SearchLane::Intent, summary);
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
                    lanes.push_text(SearchLane::Prose, v);
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

fn required_bounded(field: &'static str, value: &str, limit: usize) -> Result<(), CanonicalError> {
    if value.trim().is_empty() {
        return Err(CanonicalError::Invalid(format!("{field} is empty")));
    }
    bounded(field, value, limit)
}

fn bounded_option(
    field: &'static str,
    value: Option<&str>,
    limit: usize,
) -> Result<(), CanonicalError> {
    if let Some(value) = value {
        bounded(field, value, limit)?;
    }
    Ok(())
}

fn bounded_collection(
    field: &'static str,
    count: usize,
    limit: usize,
) -> Result<(), CanonicalError> {
    if count > limit {
        return Err(CanonicalError::Invalid(format!(
            "{field} has {count} entries; limit is {limit}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn evidence(source: &str, offset: u64) -> EvidenceOrder {
        EvidenceOrder {
            source_identity: SourceIdentity(source.into()),
            generation: 1,
            record_offset: offset,
            mutation_index: 0,
        }
    }

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
            parent_tool_native_id: None,
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
    fn attachment_references_must_match_their_content_addressed_member() {
        let asset = ProjectionAsset::new("image/png", b"\x89PNG\r\n\x1a\nbody".to_vec()).unwrap();
        let reference = AssetRef {
            url: asset.url().unwrap(),
            digest: asset.digest,
            media_type: asset.media_type,
            byte_len: asset.bytes.len() as u64,
        };
        let mut row = base(TimelineBody::User {
            text: None,
            attachments: vec![AttachmentRef {
                id: "image-0".into(),
                label: None,
                message_ranges: Vec::new(),
                kind: AttachmentKind::Image,
                name: None,
                media_type: Some("image/png".into()),
                byte_len: Some(reference.byte_len),
                source: AttachmentSource::Asset { asset: reference },
                dimensions: None,
            }],
        });
        assert!(row.validate().is_ok());
        let TimelineBody::User { attachments, .. } = &mut row.body else {
            unreachable!()
        };
        attachments[0].byte_len = Some(1);
        assert!(row.validate().is_err());
    }

    #[test]
    fn citation_ranges_are_bounded_ordered_unicode_spans() {
        let mut row = base(TimelineBody::Agent {
            text: None,
            attachments: Vec::new(),
            citations: vec![CitationRef {
                source_id: Some("source-1".into()),
                provider: None,
                title: None,
                source: None,
                message_ranges: vec![TextRange { start: 3, end: 7 }],
            }],
        });
        assert!(row.validate().is_ok());
        let TimelineBody::Agent { citations, .. } = &mut row.body else {
            unreachable!()
        };
        citations[0]
            .message_ranges
            .push(TextRange { start: 6, end: 9 });
        assert!(row.validate().is_err());
    }

    #[test]
    fn nested_tool_collections_cannot_bypass_the_canonical_bounds() {
        let tool = ToolInteraction {
            action: ToolAction::WebSearch {
                query: Some("bounded query".into()),
                allowed_domains: (0..=CANONICAL_LIST_LIMIT)
                    .map(|index| format!("{index}.example"))
                    .collect(),
                blocked_domains: Vec::new(),
                outcome: None,
            },
            ..ToolInteraction::unknown("t1", CorrelationDerivation::NativeId, "web_search")
        };
        let row = base(TimelineBody::ToolUse {
            tool: Box::new(tool),
        });
        assert!(row.validate().is_err());
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
    fn collaboration_lifecycle_kind_is_itself_a_visible_fact() {
        let row = base(TimelineBody::Collaboration {
            event: Box::new(CollaborationEvent {
                kind: CollaborationEventKind::StopApproved,
                channel: CollaborationChannel::Team,
                sender: Some("w1".into()),
                recipient: None,
                team: Some("review".into()),
                color: None,
                summary: None,
                body: None,
                task_id: None,
                delivery_id: Some("shutdown-1".into()),
                status: None,
                authored_at: None,
                source_session_id: None,
                source_response_id: None,
                work_start_timeline_id: None,
                work_end_timeline_id: None,
            }),
        });
        assert!(row.validate().is_ok());
    }

    #[test]
    fn collaboration_prose_requires_non_whitespace_content() {
        let row = base(TimelineBody::Collaboration {
            event: Box::new(CollaborationEvent {
                kind: CollaborationEventKind::Message,
                channel: CollaborationChannel::Team,
                sender: Some("w1".into()),
                recipient: None,
                team: Some("review".into()),
                color: None,
                summary: Some("   ".into()),
                body: None,
                task_id: None,
                delivery_id: None,
                status: None,
                authored_at: None,
                source_session_id: None,
                source_response_id: None,
                work_start_timeline_id: None,
                work_end_timeline_id: None,
            }),
        });
        assert!(row.validate().is_err());
    }

    #[test]
    fn collaboration_report_work_requires_exact_paired_bounds() {
        let mut row = base(TimelineBody::Collaboration {
            event: Box::new(CollaborationEvent {
                kind: CollaborationEventKind::Report,
                channel: CollaborationChannel::Team,
                sender: Some("w1".into()),
                recipient: Some("coordinator".into()),
                team: Some("review".into()),
                color: None,
                summary: Some("done".into()),
                body: None,
                task_id: Some("task-1".into()),
                delivery_id: None,
                status: Some(CollaborationStatus::Completed),
                authored_at: None,
                source_session_id: Some("member-1".into()),
                source_response_id: Some("response-1".into()),
                work_start_timeline_id: Some("work-1".into()),
                work_end_timeline_id: None,
            }),
        });
        assert!(row.validate().is_err());
        if let TimelineBody::Collaboration { event } = &mut row.body {
            event.work_end_timeline_id = Some("work-2".into());
        }
        assert!(row.validate().is_ok());
        if let TimelineBody::Collaboration { event } = &mut row.body {
            event.source_response_id = Some("   ".into());
        }
        assert!(row.validate().is_err());
    }

    #[test]
    fn lifecycle_merge_is_order_independent_and_replay_idempotent() {
        let opener = ToolInteraction::unknown("t1", CorrelationDerivation::NativeId, "future_tool");
        let weak = ToolUpdatePatch {
            status: Some(ToolStatusFact {
                value: ToolStatus::Unknown,
                basis: StatusBasis::SourceFinalization,
            }),
            ..Default::default()
        };
        let terminal = ToolUpdatePatch {
            status: Some(ToolStatusFact {
                value: ToolStatus::Succeeded,
                basis: StatusBasis::NativeTerminalRecord,
            }),
            ..Default::default()
        };

        let mut forward = opener.clone();
        let mut forward_state = ToolFoldState::default();
        assert!(
            forward
                .apply_patch(&mut forward_state, &weak, &evidence("b", 9))
                .unwrap()
        );
        assert!(
            forward
                .apply_patch(&mut forward_state, &terminal, &evidence("a", 1))
                .unwrap()
        );
        assert!(
            !forward
                .apply_patch(&mut forward_state, &terminal, &evidence("a", 1))
                .unwrap()
        );

        let mut reverse = opener;
        let mut reverse_state = ToolFoldState::default();
        reverse
            .apply_patch(&mut reverse_state, &terminal, &evidence("a", 1))
            .unwrap();
        assert!(
            !reverse
                .apply_patch(&mut reverse_state, &weak, &evidence("b", 9))
                .unwrap()
        );

        assert_eq!(forward, reverse);
        assert_eq!(forward_state, reverse_state);
        assert_eq!(forward.status.value, ToolStatus::Succeeded);
    }

    #[test]
    fn lifecycle_merge_rejects_known_action_family_changes() {
        let mut tool = ToolInteraction {
            action: ToolAction::Shell {
                command: CanonicalText::inline("true").unwrap(),
                source_truncated: false,
                cwd: None,
                timeout_ms: None,
                background: false,
                outcome: None,
            },
            ..ToolInteraction::unknown("t1", CorrelationDerivation::NativeId, "bash")
        };
        let patch = ToolUpdatePatch {
            request: Some(ToolRequestPatch {
                action: ToolAction::FileRead {
                    targets: Vec::new(),
                    outcome: None,
                },
            }),
            ..Default::default()
        };
        assert!(
            tool.apply_patch(&mut ToolFoldState::default(), &patch, &evidence("a", 1))
                .is_err()
        );
    }

    #[test]
    fn sparse_child_link_only_applies_to_delegate_actions() {
        let mut delegate = ToolInteraction {
            action: ToolAction::Delegate {
                agent_kind: None,
                name: None,
                model: None,
                mode: None,
                team: None,
                color: None,
                objective: CanonicalText::inline("inspect the store").unwrap(),
                child_session_id: None,
                resume_id: None,
                background: false,
                orphan_result_summary: None,
            },
            ..ToolInteraction::unknown("t1", CorrelationDerivation::NativeId, "delegate")
        };
        let patch = ToolUpdatePatch {
            delegate_child_session_id: Some("child-1".into()),
            ..Default::default()
        };
        assert!(
            delegate
                .apply_patch(&mut ToolFoldState::default(), &patch, &evidence("a", 1))
                .unwrap()
        );
        assert!(matches!(
            delegate.action,
            ToolAction::Delegate {
                child_session_id: Some(ref id),
                ..
            } if id == "child-1"
        ));

        let mut shell = ToolInteraction {
            action: ToolAction::Shell {
                command: CanonicalText::inline("true").unwrap(),
                source_truncated: false,
                cwd: None,
                timeout_ms: None,
                background: false,
                outcome: None,
            },
            ..ToolInteraction::unknown("t2", CorrelationDerivation::NativeId, "shell")
        };
        assert!(
            shell
                .apply_patch(&mut ToolFoldState::default(), &patch, &evidence("a", 1))
                .is_err()
        );
    }

    #[test]
    fn lifecycle_request_fields_converge_independently() {
        fn opener() -> ToolInteraction {
            ToolInteraction {
                action: ToolAction::Shell {
                    command: CanonicalText::inline("opener").unwrap(),
                    source_truncated: false,
                    cwd: None,
                    timeout_ms: None,
                    background: false,
                    outcome: None,
                },
                ..ToolInteraction::unknown("t1", CorrelationDerivation::NativeId, "bash")
            }
        }
        fn request(
            command: &str,
            cwd: Option<&str>,
            timeout_ms: Option<u64>,
            background: bool,
        ) -> ToolUpdatePatch {
            ToolUpdatePatch {
                request: Some(ToolRequestPatch {
                    action: ToolAction::Shell {
                        command: CanonicalText::inline(command).unwrap(),
                        source_truncated: false,
                        cwd: cwd.map(|path| ResourceRef::Directory {
                            path: path.into(),
                            resolved: true,
                        }),
                        timeout_ms,
                        background,
                        outcome: None,
                    },
                }),
                ..Default::default()
            }
        }

        let older = request("older command", Some("/repo"), None, false);
        let newer = request("newer command", None, Some(30_000), true);

        let mut forward = opener();
        let mut forward_state = ToolFoldState::default();
        forward
            .apply_patch(&mut forward_state, &newer, &evidence("a", 10))
            .unwrap();
        forward
            .apply_patch(&mut forward_state, &older, &evidence("a", 5))
            .unwrap();

        let mut reverse = opener();
        let mut reverse_state = ToolFoldState::default();
        reverse
            .apply_patch(&mut reverse_state, &older, &evidence("a", 5))
            .unwrap();
        reverse
            .apply_patch(&mut reverse_state, &newer, &evidence("a", 10))
            .unwrap();

        assert_eq!(forward, reverse);
        assert_eq!(forward_state, reverse_state);
        let ToolAction::Shell {
            command,
            cwd,
            timeout_ms,
            background,
            ..
        } = &forward.action
        else {
            panic!("shell action");
        };
        assert_eq!(command.as_preview(), "newer command");
        assert_eq!(cwd.as_ref().map(ResourceRef::display_value), Some("/repo"));
        assert_eq!(*timeout_ms, Some(30_000));
        assert!(*background);
    }

    #[test]
    fn lifecycle_outcome_patch_cannot_replace_request_fields() {
        let mut tool = ToolInteraction {
            action: ToolAction::Shell {
                command: CanonicalText::inline("keep request").unwrap(),
                source_truncated: false,
                cwd: Some(ResourceRef::Directory {
                    path: "/repo".into(),
                    resolved: true,
                }),
                timeout_ms: Some(10_000),
                background: false,
                outcome: None,
            },
            ..ToolInteraction::unknown("t1", CorrelationDerivation::NativeId, "bash")
        };
        let patch = ToolUpdatePatch {
            outcome: Some(ToolOutcomePatch {
                action: ToolAction::Shell {
                    command: CanonicalText::inline("native result copy").unwrap(),
                    source_truncated: false,
                    cwd: None,
                    timeout_ms: None,
                    background: true,
                    outcome: Some(ShellOutcome {
                        exit_code: Some(0),
                        signal: None,
                        process_id: None,
                    }),
                },
            }),
            ..Default::default()
        };
        tool.apply_patch(
            &mut ToolFoldState::default(),
            &patch,
            &evidence("result", 1),
        )
        .unwrap();

        let ToolAction::Shell {
            command,
            cwd,
            timeout_ms,
            background,
            outcome,
            ..
        } = &tool.action
        else {
            panic!("shell action");
        };
        assert_eq!(command.as_preview(), "keep request");
        assert_eq!(cwd.as_ref().map(ResourceRef::display_value), Some("/repo"));
        assert_eq!(*timeout_ms, Some(10_000));
        assert!(!background);
        assert_eq!(outcome.as_ref().and_then(|value| value.exit_code), Some(0));
    }

    #[test]
    fn search_extracts_only_declared_fields() {
        let row = base(TimelineBody::ToolUse {
            tool: Box::new(ToolInteraction {
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
                    source_truncated: false,
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
            }),
        });
        let lanes = row.search_lanes();
        assert_eq!(lanes.intent, "git status");
        assert_eq!(lanes.target, "/repo");
        assert_eq!(lanes.tool, "shell");
        assert!(lanes.change.is_empty());
    }

    #[test]
    fn collaboration_search_keeps_team_and_task_addresses() {
        let delegate = base(TimelineBody::ToolUse {
            tool: Box::new(ToolInteraction {
                action: ToolAction::Delegate {
                    agent_kind: Some("general-purpose".into()),
                    name: Some("w2-docs".into()),
                    model: None,
                    mode: None,
                    team: Some("refinement".into()),
                    color: Some(ParticipantColor::Green),
                    objective: CanonicalText::inline("land the design deltas").unwrap(),
                    child_session_id: Some("member-2".into()),
                    resume_id: None,
                    background: true,
                    orphan_result_summary: None,
                },
                ..ToolInteraction::unknown("delegate-1", CorrelationDerivation::NativeId, "Agent")
            }),
        });
        let lanes = delegate.search_lanes();
        assert_eq!(lanes.target, "refinement");
        assert!(lanes.tool.contains("w2-docs"));

        let message = base(TimelineBody::Collaboration {
            event: Box::new(CollaborationEvent {
                kind: CollaborationEventKind::Message,
                channel: CollaborationChannel::Peer,
                sender: Some("w2-docs".into()),
                recipient: Some("coordinator".into()),
                team: Some("refinement".into()),
                color: Some(ParticipantColor::Green),
                summary: None,
                body: Some(CanonicalText::inline("docs are ready").unwrap()),
                task_id: Some("task-22".into()),
                delivery_id: Some("delivery-22".into()),
                status: None,
                authored_at: None,
                source_session_id: None,
                source_response_id: None,
                work_start_timeline_id: None,
                work_end_timeline_id: None,
            }),
        });
        let lanes = message.search_lanes();
        assert!(lanes.prose.contains("docs are ready"));
        assert!(lanes.target.contains("w2-docs"));
        assert!(lanes.target.contains("refinement"));
        assert!(lanes.target.contains("task-22"));
    }

    #[test]
    fn command_output_is_searchable_as_prose() {
        let row = base(TimelineBody::Notice {
            notice: Notice::CommandOutput {
                output: CanonicalText::inline("Set model to Fable 5.1").unwrap(),
            },
        });
        let lanes = row.search_lanes();
        assert_eq!(lanes.prose, "Set model to Fable 5.1");
        assert!(lanes.target.is_empty());
        assert!(lanes.intent.is_empty());
        assert!(lanes.change.is_empty());
        assert!(lanes.tool.is_empty());
    }

    #[test]
    fn opaque_orchestration_is_not_searchable() {
        let row = base(TimelineBody::ToolUse {
            tool: Box::new(ToolInteraction {
                correlation: ToolCorrelation {
                    key: "opaque".into(),
                    derivation: CorrelationDerivation::Synthetic,
                },
                native: NativeToolRef {
                    id: None,
                    namespace: None,
                    name: "exec".into(),
                    provider: None,
                },
                status: ToolStatusFact {
                    value: ToolStatus::Succeeded,
                    basis: StatusBasis::GroundTruthProtocol,
                },
                action: ToolAction::Orchestration {
                    runtime: Some("node".into()),
                    trace: ChildTrace::Unavailable,
                    executed_children: Vec::new(),
                    executed_child_count: None,
                },
                failure: None,
                timing: None,
            }),
        });
        assert_eq!(row.search_lanes(), SearchLanes::default());
    }

    #[test]
    fn unknown_tools_index_only_the_native_name() {
        let row = base(TimelineBody::ToolUse {
            tool: Box::new(ToolInteraction::unknown(
                "t1",
                CorrelationDerivation::Synthetic,
                "mystery",
            )),
        });
        let lanes = row.search_lanes();
        assert_eq!(lanes.tool, "mystery");
        assert!(lanes.intent.is_empty());
        assert!(lanes.target.is_empty());
    }
}
