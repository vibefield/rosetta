//! Pi session entries → canonical operations.
//!
//! Pi's JSONL order is presentation order. The `id`/`parentId` links form an
//! independent entry tree: they are preserved as `uuid`/`parent_uuid` and are
//! used to inherit branch-local settings, but they never sort rows or assign
//! ordinals. Every emitted row takes the next ordinal directly from the replay
//! context.

use std::collections::BTreeMap;

use rosetta_adapter_support::Drift;
use rosetta_adapter_support::adapter::{LineOutcome, LineStatus, NormalizeCtx};
use rosetta_adapter_support::tool::{
    projection_record_kind, projection_shape_paths, terminal_update, unknown_interaction,
};
use rosetta_core::rows::{
    DriftFact, MessageKind, MessageRow, Ms, SessionTitleRow, SessionUsageRow, TitleKind,
    TokenSnapshot, UsageScope,
};
use rosetta_core::timeline::{CorrelationDerivation, StatusBasis};
use rosetta_core::{IngestOp, fold, ids};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::raw::session::{
    AgentMessage, AssistantMessage, BashExecutionMessage, BranchSummaryEntry, BranchSummaryMessage,
    CompactionEntry, CompactionSummaryMessage, ContentBlock, CustomMessageEntry, CustomRoleMessage,
    KnownContentBlock, KnownMessage, KnownRecord, MessageEntry, SessionHeader, ToolResultMessage,
    Usage, UserContent,
};
use crate::raw::{ParsedLine, parse_line};
use crate::spec;
use crate::time::rfc3339_ms;

const INVALID_JSON: &str = "<invalid-json>";
const NO_TYPE: &str = "<untyped>";

/// Cross-line state. Entry contexts are keyed by native ids so settings follow
/// graph ancestry even when alternate branches are interleaved in the file.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct PiState {
    header: Option<HeaderState>,
    entries: BTreeMap<String, EntryContext>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HeaderState {
    cwd: String,
    started_at: Ms,
    /// The file was forked from another session (`parentSession`): entries
    /// older than the header are the parent's, copied usage included.
    #[serde(default)]
    forked: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct EntryContext {
    model: Option<String>,
    effort: Option<String>,
}

impl PiState {
    fn inherited(&self, parent_id: Option<&str>) -> EntryContext {
        parent_id
            .and_then(|parent| self.entries.get(parent))
            .cloned()
            .unwrap_or_default()
    }

    fn remember(&mut self, id: &str, context: EntryContext) {
        self.entries.insert(id.to_owned(), context);
    }

    /// A fork copies its parent's entries with their original timestamps, so
    /// anything older than the fork's own header is inherited history: its
    /// row stays, its usage was already counted in the parent.
    fn inherited_copy(&self, at: Option<Ms>) -> bool {
        match (&self.header, at) {
            (Some(header), Some(at)) => header.forked && at < header.started_at,
            _ => false,
        }
    }
}

pub fn ingest_line(
    ctx: &mut NormalizeCtx,
    state: &mut PiState,
    stream_id: &str,
    line: &[u8],
) -> LineOutcome {
    let Ok(text) = std::str::from_utf8(line) else {
        return hard(ctx, stream_id, NO_TYPE, "invalid_utf8", &[], None);
    };
    if text.trim().is_empty() {
        return LineOutcome::parsed(Vec::new());
    }

    let parsed = match parse_line(text) {
        Ok(parsed) => parsed,
        Err(_) => return parse_failure(ctx, stream_id, text),
    };
    let record = match parsed {
        ParsedLine::Known(record) => record,
        ParsedLine::UnknownType { rtype, value } => {
            let kind = rtype.unwrap_or_else(|| NO_TYPE.to_owned());
            if spec::declared_session_types().contains(&kind) {
                return LineOutcome::parsed(Vec::new());
            }
            return LineOutcome {
                ops: vec![drift(
                    ctx,
                    stream_id,
                    &kind,
                    "unknown_type",
                    &keys(&value),
                    stamp(&value),
                )],
                status: LineStatus::UnknownType,
            };
        }
    };

    let mut ops = Vec::new();
    project(ctx, state, stream_id, &record, &mut ops);

    let paths = record.drift_paths();
    if !paths.is_empty() {
        ops.push(drift(
            ctx,
            stream_id,
            &record_kind(&record),
            "unknown_fields",
            &paths,
            record_timestamp(&record),
        ));
    }
    LineOutcome::parsed(ops)
}

fn project(
    ctx: &mut NormalizeCtx,
    state: &mut PiState,
    stream_id: &str,
    record: &KnownRecord,
    ops: &mut Vec<IngestOp>,
) {
    match record {
        KnownRecord::Session(header) => session_header(state, header),
        KnownRecord::ModelChange(entry) => {
            let mut inherited = state.inherited(entry.parent_id.as_deref());
            inherited.model = Some(entry.model_id.clone());
            state.remember(&entry.id, inherited);
        }
        KnownRecord::ThinkingLevelChange(entry) => {
            let mut inherited = state.inherited(entry.parent_id.as_deref());
            inherited.effort = Some(entry.thinking_level.clone());
            state.remember(&entry.id, inherited);
        }
        KnownRecord::SessionInfo(entry) => {
            let inherited = state.inherited(entry.parent_id.as_deref());
            state.remember(&entry.id, inherited);
            if let Some(name) = entry.name.as_ref().filter(|name| !name.is_empty()) {
                let at = rfc3339_ms(&entry.timestamp)
                    .or_else(|| state.header.as_ref().map(|header| header.started_at))
                    .unwrap_or_default();
                ops.push(IngestOp::SessionTitle(SessionTitleRow {
                    session_id: ctx.session_id.clone(),
                    title: name.clone(),
                    kind: TitleKind::Explicit,
                    at,
                }));
            }
        }
        KnownRecord::Message(entry) => message_entry(ctx, state, stream_id, entry, ops),
        KnownRecord::Compaction(entry) => compaction_entry(ctx, state, entry, ops),
        KnownRecord::BranchSummary(entry) => branch_summary_entry(ctx, state, entry, ops),
        KnownRecord::CustomMessage(entry) => custom_message_entry(ctx, state, entry, ops),
        // These are declared structural extension metadata. Preserve their
        // place in the native tree for descendants, but emit no invented row.
        KnownRecord::Custom(entry) => {
            let inherited = state.inherited(entry.parent_id.as_deref());
            state.remember(&entry.id, inherited);
        }
        KnownRecord::Label(entry) => {
            let inherited = state.inherited(entry.parent_id.as_deref());
            state.remember(&entry.id, inherited);
        }
    }
}

fn session_header(state: &mut PiState, header: &SessionHeader) {
    let started_at = rfc3339_ms(&header.timestamp).unwrap_or_default();
    state.header = Some(HeaderState {
        cwd: header.cwd.clone(),
        started_at,
        forked: header.parent_session.is_some(),
    });
}

fn message_entry(
    ctx: &mut NormalizeCtx,
    state: &mut PiState,
    stream_id: &str,
    entry: &MessageEntry,
    ops: &mut Vec<IngestOp>,
) {
    let inherited = state.inherited(entry.parent_id.as_deref());
    let ts = rfc3339_ms(&entry.timestamp);
    match &entry.message {
        AgentMessage::Known(KnownMessage::User(message)) => {
            let mut row = row(ctx, entry, &entry.id, MessageKind::User, ts, None, None);
            row.content = user_content_text(&message.content);
            if let UserContent::Blocks(blocks) = &message.content {
                for (index, block) in blocks.iter().enumerate() {
                    if let ContentBlock::Known(KnownContentBlock::Image {
                        data, mime_type, ..
                    }) = block
                    {
                        let (attachment, asset) = rosetta_adapter_support::media::from_base64(
                            &format!("image-{index}"),
                            rosetta_core::timeline::AttachmentKind::Image,
                            mime_type,
                            data,
                        );
                        row.attachments.push(attachment);
                        ops.extend(asset.map(|asset| IngestOp::ProjectionAsset { asset }));
                    } else if matches!(block, ContentBlock::Other(value) if value["type"] == "image")
                    {
                        row.attachments
                            .push(rosetta_core::timeline::AttachmentRef::unavailable(
                                format!("image-{index}"),
                                rosetta_core::timeline::AttachmentKind::Image,
                                rosetta_core::timeline::AttachmentUnavailableReason::InvalidData,
                            ));
                    }
                }
            }
            IngestOp::push_message(ops, row);
        }
        AgentMessage::Known(KnownMessage::Assistant(message)) => {
            let copied = state.inherited_copy(ts);
            assistant_message(
                ctx,
                stream_id,
                state.header.as_ref().map(|header| header.cwd.as_str()),
                entry,
                message,
                &inherited,
                ts,
                copied,
                ops,
            );
        }
        AgentMessage::Known(KnownMessage::ToolResult(message)) => {
            tool_result(ctx, message, ts, ops);
            if let Some(usage) = message.usage.as_ref()
                && !state.inherited_copy(ts)
            {
                // The model the tool called is not recorded, so this spend
                // is counted but unpriced.
                let (tokens, clamped) = usage_snapshot(usage, None, UsageScope::Aggregate);
                if clamped {
                    ops.push(usage_drift(ctx, stream_id, "toolResult", ts));
                }
                ops.push(IngestOp::SessionUsage(SessionUsageRow {
                    session_id: ctx.session_id.clone(),
                    key: ids::usage_id(&ctx.session_id, &format!("{}:tool-result", entry.id)),
                    ts: ts.unwrap_or(0),
                    model: None,
                    tokens,
                }));
            }
        }
        AgentMessage::Known(KnownMessage::BashExecution(message)) => {
            bash_execution(ctx, stream_id, entry, message, ts, ops);
        }
        AgentMessage::Known(KnownMessage::Custom(message)) => {
            custom_role_message(ctx, entry, message, ts, ops);
        }
        AgentMessage::Known(KnownMessage::BranchSummary(message)) => {
            branch_summary_message(ctx, entry, message, &inherited, ts, ops);
        }
        AgentMessage::Known(KnownMessage::CompactionSummary(message)) => {
            compaction_summary_message(ctx, entry, message, &inherited, ts, ops);
        }
        // An undeclared nested role is reported by `drift_paths`; the declared
        // top-level `message` record remains successfully parsed.
        AgentMessage::Other(_) => {}
    }
    state.remember(&entry.id, inherited);
}

// A native message needs both inherited branch context and projection output;
// keeping those explicit prevents hidden state from entering normalization.
#[allow(clippy::too_many_arguments)]
fn assistant_message(
    ctx: &mut NormalizeCtx,
    stream_id: &str,
    cwd: Option<&str>,
    entry: &MessageEntry,
    message: &AssistantMessage,
    inherited: &EntryContext,
    ts: Option<Ms>,
    copied: bool,
    ops: &mut Vec<IngestOp>,
) {
    let model = message.response_model.as_deref().unwrap_or(&message.model);
    let mut rows = Vec::new();
    for (index, block) in message.content.iter().enumerate() {
        let discriminator = format!("{}#{index}", entry.id);
        match block {
            ContentBlock::Known(KnownContentBlock::Text { text, .. }) => {
                let mut row = row(
                    ctx,
                    entry,
                    &discriminator,
                    MessageKind::Agent,
                    ts,
                    Some(model),
                    inherited.effort.as_deref(),
                );
                row.content = Some(text.clone());
                rows.push(row);
            }
            ContentBlock::Known(KnownContentBlock::Thinking { thinking, .. }) => {
                let mut row = row(
                    ctx,
                    entry,
                    &discriminator,
                    MessageKind::Thinking,
                    ts,
                    Some(model),
                    inherited.effort.as_deref(),
                );
                row.content = Some(thinking.clone());
                rows.push(row);
            }
            ContentBlock::Known(KnownContentBlock::ToolCall {
                id,
                name,
                arguments,
                ..
            }) => {
                let mut row = row(
                    ctx,
                    entry,
                    &discriminator,
                    MessageKind::ToolUse,
                    ts,
                    Some(model),
                    inherited.effort.as_deref(),
                );
                let projected = crate::tool::project_request(
                    ctx.agent_version.as_deref(),
                    name,
                    arguments,
                    cwd,
                    id,
                    Some(id),
                );
                row.tool = Some(match projected {
                    Ok(tool) => tool,
                    Err(error) => {
                        ops.push(drift(
                            ctx,
                            stream_id,
                            &projection_record_kind("request", None, name),
                            error.drift_code(),
                            &projection_shape_paths(arguments),
                            ts,
                        ));
                        unknown_interaction(
                            id,
                            CorrelationDerivation::NativeId,
                            Some(id.clone()),
                            None,
                            name,
                            None,
                        )
                    }
                });
                rows.push(row);
            }
            // Images have no canonical bitmap row. Unknown blocks are drifted
            // after projection; neither creates a synthetic ordering slot.
            ContentBlock::Known(KnownContentBlock::Image { .. }) | ContentBlock::Other(_) => {}
        }
    }

    if rows.is_empty() {
        rows.push(row(
            ctx,
            entry,
            &format!("{}#0", entry.id),
            MessageKind::Agent,
            ts,
            Some(model),
            inherited.effort.as_deref(),
        ));
    }
    if !copied {
        let (tokens, clamped) =
            usage_snapshot(&message.usage, Some(&message.provider), UsageScope::Request);
        if clamped {
            ops.push(usage_drift(ctx, stream_id, "message/assistant", ts));
        }
        attach_usage(&mut rows, tokens);
    }
    for row in rows {
        IngestOp::push_message(ops, row);
    }
}

fn tool_result(
    ctx: &NormalizeCtx,
    message: &ToolResultMessage,
    ts: Option<Ms>,
    ops: &mut Vec<IngestOp>,
) {
    let diagnostic = message
        .is_error
        .then(|| blocks_text(&message.content))
        .flatten();
    ops.push(IngestOp::ToolUpdate {
        session_id: ctx.session_id.clone(),
        correlation_key: message.tool_call_id.clone(),
        evidence: ctx.evidence(0),
        patch: Box::new(terminal_update(
            Some(message.is_error),
            true,
            StatusBasis::NativeTerminalRecord,
            ts,
            diagnostic.as_deref(),
        )),
        at: ts,
    });
}

fn bash_execution(
    ctx: &mut NormalizeCtx,
    stream_id: &str,
    entry: &MessageEntry,
    message: &BashExecutionMessage,
    ts: Option<Ms>,
    ops: &mut Vec<IngestOp>,
) {
    let is_error = if message.cancelled {
        Some(true)
    } else {
        message.exit_code.map(|code| code != 0)
    };
    let mut row = row(ctx, entry, &entry.id, MessageKind::ToolUse, ts, None, None);
    let cwd = None;
    let input = json!({"command": message.command});
    let projected = crate::tool::project_request(
        ctx.agent_version.as_deref(),
        "BashExecution",
        &input,
        cwd,
        &entry.id,
        None,
    );
    let mut tool = match projected {
        Ok(tool) => tool,
        Err(error) => {
            ops.push(drift(
                ctx,
                stream_id,
                &projection_record_kind("request", None, "BashExecution"),
                error.drift_code(),
                &projection_shape_paths(&input),
                ts,
            ));
            unknown_interaction(
                &entry.id,
                CorrelationDerivation::Synthetic,
                None,
                None,
                "BashExecution",
                None,
            )
        }
    };
    let patch = terminal_update(
        is_error,
        false,
        StatusBasis::NativeState,
        ts,
        is_error
            .is_some_and(|error| error)
            .then_some(message.output.as_str()),
    );
    if let Some(status) = patch.status {
        tool.status = status;
    }
    tool.failure = patch.failure;
    tool.timing = patch.timing;
    row.tool = Some(tool);
    IngestOp::push_message(ops, row);
}

fn custom_role_message(
    ctx: &mut NormalizeCtx,
    entry: &MessageEntry,
    message: &CustomRoleMessage,
    ts: Option<Ms>,
    ops: &mut Vec<IngestOp>,
) {
    let mut row = row(ctx, entry, &entry.id, MessageKind::System, ts, None, None);
    row.subtype = Some(message.custom_type.clone());
    row.content = user_content_text(&message.content);
    IngestOp::push_message(ops, row);
}

fn branch_summary_message(
    ctx: &mut NormalizeCtx,
    entry: &MessageEntry,
    message: &BranchSummaryMessage,
    inherited: &EntryContext,
    ts: Option<Ms>,
    ops: &mut Vec<IngestOp>,
) {
    let mut row = row(
        ctx,
        entry,
        &entry.id,
        MessageKind::Summary,
        ts,
        inherited.model.as_deref(),
        inherited.effort.as_deref(),
    );
    row.subtype = Some("branch_summary".to_owned());
    row.content = Some(message.summary.clone());
    IngestOp::push_message(ops, row);
}

fn compaction_summary_message(
    ctx: &mut NormalizeCtx,
    entry: &MessageEntry,
    message: &CompactionSummaryMessage,
    inherited: &EntryContext,
    ts: Option<Ms>,
    ops: &mut Vec<IngestOp>,
) {
    let mut row = row(
        ctx,
        entry,
        &entry.id,
        MessageKind::Summary,
        ts,
        inherited.model.as_deref(),
        inherited.effort.as_deref(),
    );
    row.subtype = Some("compaction_summary".to_owned());
    row.content = Some(message.summary.clone());
    IngestOp::push_message(ops, row);
}

fn compaction_entry(
    ctx: &mut NormalizeCtx,
    state: &mut PiState,
    entry: &CompactionEntry,
    ops: &mut Vec<IngestOp>,
) {
    let inherited = state.inherited(entry.parent_id.as_deref());
    let mut row = entry_row(
        ctx,
        &entry.id,
        entry.parent_id.as_deref(),
        MessageKind::Summary,
        rfc3339_ms(&entry.timestamp),
        &inherited,
    );
    row.subtype = Some("compaction".to_owned());
    row.content = Some(entry.summary.clone());
    // A split turn summarizes in two requests recorded as one total.
    row.usage = summary_usage(
        ctx,
        state,
        entry.usage.as_ref(),
        UsageScope::Aggregate,
        row.ts,
        ops,
    );
    IngestOp::push_message(ops, row);
    state.remember(&entry.id, inherited);
}

fn branch_summary_entry(
    ctx: &mut NormalizeCtx,
    state: &mut PiState,
    entry: &BranchSummaryEntry,
    ops: &mut Vec<IngestOp>,
) {
    let inherited = state.inherited(entry.parent_id.as_deref());
    let mut row = entry_row(
        ctx,
        &entry.id,
        entry.parent_id.as_deref(),
        MessageKind::Summary,
        rfc3339_ms(&entry.timestamp),
        &inherited,
    );
    row.subtype = Some("branch_summary".to_owned());
    row.content = Some(entry.summary.clone());
    row.usage = summary_usage(
        ctx,
        state,
        entry.usage.as_ref(),
        UsageScope::Request,
        row.ts,
        ops,
    );
    IngestOp::push_message(ops, row);
    state.remember(&entry.id, inherited);
}

fn summary_usage(
    ctx: &NormalizeCtx,
    state: &PiState,
    usage: Option<&Usage>,
    scope: UsageScope,
    at: Option<Ms>,
    ops: &mut Vec<IngestOp>,
) -> Option<TokenSnapshot> {
    if state.inherited_copy(at) {
        return None;
    }
    let (tokens, clamped) = usage_snapshot(usage?, None, scope);
    if clamped {
        ops.push(usage_drift(ctx, "session", "summary", at));
    }
    Some(tokens)
}

fn custom_message_entry(
    ctx: &mut NormalizeCtx,
    state: &mut PiState,
    entry: &CustomMessageEntry,
    ops: &mut Vec<IngestOp>,
) {
    let inherited = state.inherited(entry.parent_id.as_deref());
    let mut row = entry_row(
        ctx,
        &entry.id,
        entry.parent_id.as_deref(),
        MessageKind::System,
        rfc3339_ms(&entry.timestamp),
        &EntryContext::default(),
    );
    row.subtype = Some(entry.custom_type.clone());
    row.content = user_content_text(&entry.content);
    IngestOp::push_message(ops, row);
    state.remember(&entry.id, inherited);
}

fn row(
    ctx: &mut NormalizeCtx,
    entry: &MessageEntry,
    discriminator: &str,
    kind: MessageKind,
    ts: Option<Ms>,
    model: Option<&str>,
    effort: Option<&str>,
) -> MessageRow {
    make_row(
        ctx,
        discriminator,
        kind,
        Some(&entry.id),
        entry.parent_id.as_deref(),
        ts,
        model,
        effort,
    )
}

fn entry_row(
    ctx: &mut NormalizeCtx,
    id: &str,
    parent_id: Option<&str>,
    kind: MessageKind,
    ts: Option<Ms>,
    inherited: &EntryContext,
) -> MessageRow {
    make_row(
        ctx,
        id,
        kind,
        Some(id),
        parent_id,
        ts,
        inherited.model.as_deref(),
        inherited.effort.as_deref(),
    )
}

#[allow(clippy::too_many_arguments)]
fn make_row(
    ctx: &mut NormalizeCtx,
    discriminator: &str,
    kind: MessageKind,
    uuid: Option<&str>,
    parent_uuid: Option<&str>,
    ts: Option<Ms>,
    model: Option<&str>,
    effort: Option<&str>,
) -> MessageRow {
    MessageRow {
        timeline_id: ids::timeline_id(&ctx.session_id, discriminator),
        session_id: ctx.session_id.clone(),
        // Presentation order only — never native depth or branch-local order.
        ordinal: ctx.take_ordinal(),
        kind,
        subtype: None,
        uuid: uuid.map(str::to_owned),
        parent_uuid: parent_uuid.map(str::to_owned),
        ts,
        model: model.map(str::to_owned),
        effort: effort.map(str::to_owned),
        content: None,
        tool: None,
        attachments: Vec::new(),
        citations: Vec::new(),
        group: None,
        response: None,
        block_index: None,
        branch_tool_id: None,
        usage: None,
        agent_version: ctx.agent_version.clone(),
        src_cursor: ctx.src_cursor,
        src_offset: ctx.src_offset,
    }
}

fn attach_usage(rows: &mut [MessageRow], usage: TokenSnapshot) {
    let target = rows
        .iter()
        .position(|row| row.kind == MessageKind::Agent)
        .or_else(|| rows.iter().position(|row| row.kind == MessageKind::ToolUse))
        .unwrap_or(0);
    if let Some(row) = rows.get_mut(target) {
        row.usage = Some(usage);
    }
}

/// Map Pi's usage record into the exclusive buckets (RESEARCH_PLAN §5.13).
/// `input` already excludes cache on every Pi API, `cacheWrite` includes
/// `cacheWrite1h`, and `output` includes `reasoning`. Pi's `cost` is computed
/// from its own price table, so it is never billing. Returns whether a subset
/// exceeded its total (clamped, reported as drift).
fn usage_snapshot(
    usage: &Usage,
    provider: Option<&str>,
    scope: UsageScope,
) -> (TokenSnapshot, bool) {
    let count = |value: u64| i64::try_from(value).ok();
    let (cache_write, cache_write_1h, write_clamped) = fold::split_subset(
        count(usage.cache_write),
        usage.cache_write_1h.and_then(count),
    );
    let (output, thinking, output_clamped) =
        fold::split_subset(count(usage.output), usage.reasoning.and_then(count));
    let tokens = TokenSnapshot {
        input: count(usage.input),
        output,
        cache_read: count(usage.cache_read),
        cache_write,
        cache_write_1h,
        thinking,
        route: provider.and_then(rosetta_core::rows::canonical_route),
        scope,
        ..TokenSnapshot::default()
    };
    (tokens, write_clamped || output_clamped)
}

/// A usage subset (reasoning, 1-hour writes) larger than its total.
fn usage_drift(ctx: &NormalizeCtx, stream_id: &str, record_kind: &str, at: Option<Ms>) -> IngestOp {
    drift(
        ctx,
        stream_id,
        record_kind,
        "usage_subset_exceeds_total",
        &["usage".to_owned()],
        at,
    )
}

fn user_content_text(content: &UserContent) -> Option<String> {
    match content {
        UserContent::Text(text) => Some(text.clone()),
        UserContent::Blocks(blocks) => blocks_text(blocks),
    }
}

fn blocks_text(blocks: &[ContentBlock]) -> Option<String> {
    let mut joined = String::new();
    for block in blocks {
        if let ContentBlock::Known(KnownContentBlock::Text { text, .. }) = block {
            if !joined.is_empty() {
                joined.push('\n');
            }
            joined.push_str(text);
        }
    }
    (!joined.is_empty()).then_some(joined)
}

fn record_kind(record: &KnownRecord) -> String {
    match record {
        KnownRecord::Session(_) => "session".to_owned(),
        KnownRecord::Message(entry) => match &entry.message {
            AgentMessage::Known(KnownMessage::User(_)) => "message/user".to_owned(),
            AgentMessage::Known(KnownMessage::Assistant(_)) => "message/assistant".to_owned(),
            AgentMessage::Known(KnownMessage::ToolResult(_)) => "message/toolResult".to_owned(),
            AgentMessage::Known(KnownMessage::BashExecution(_)) => {
                "message/bashExecution".to_owned()
            }
            AgentMessage::Known(KnownMessage::Custom(_)) => "message/custom".to_owned(),
            AgentMessage::Known(KnownMessage::BranchSummary(_)) => {
                "message/branchSummary".to_owned()
            }
            AgentMessage::Known(KnownMessage::CompactionSummary(_)) => {
                "message/compactionSummary".to_owned()
            }
            AgentMessage::Other(value) => {
                format!("message/{}", value["role"].as_str().unwrap_or("<untyped>"))
            }
        },
        KnownRecord::ModelChange(_) => "model_change".to_owned(),
        KnownRecord::ThinkingLevelChange(_) => "thinking_level_change".to_owned(),
        KnownRecord::SessionInfo(_) => "session_info".to_owned(),
        KnownRecord::Compaction(_) => "compaction".to_owned(),
        KnownRecord::BranchSummary(_) => "branch_summary".to_owned(),
        KnownRecord::Custom(_) => "custom".to_owned(),
        KnownRecord::CustomMessage(_) => "custom_message".to_owned(),
        KnownRecord::Label(_) => "label".to_owned(),
    }
}

fn record_timestamp(record: &KnownRecord) -> Option<Ms> {
    let timestamp = match record {
        KnownRecord::Session(entry) => &entry.timestamp,
        KnownRecord::Message(entry) => &entry.timestamp,
        KnownRecord::ModelChange(entry) => &entry.timestamp,
        KnownRecord::ThinkingLevelChange(entry) => &entry.timestamp,
        KnownRecord::SessionInfo(entry) => &entry.timestamp,
        KnownRecord::Compaction(entry) => &entry.timestamp,
        KnownRecord::BranchSummary(entry) => &entry.timestamp,
        KnownRecord::Custom(entry) => &entry.timestamp,
        KnownRecord::CustomMessage(entry) => &entry.timestamp,
        KnownRecord::Label(entry) => &entry.timestamp,
    };
    rfc3339_ms(timestamp)
}

fn parse_failure(ctx: &NormalizeCtx, stream_id: &str, text: &str) -> LineOutcome {
    let value: Option<Value> = serde_json::from_str(text).ok();
    let Some(value) = value else {
        return hard(ctx, stream_id, INVALID_JSON, "invalid_json", &[], None);
    };
    let kind = value["type"].as_str().unwrap_or(NO_TYPE);
    hard(
        ctx,
        stream_id,
        kind,
        "parse_failed",
        &keys(&value),
        stamp(&value),
    )
}

fn hard(
    ctx: &NormalizeCtx,
    stream_id: &str,
    record_kind: &str,
    code: &str,
    paths: &[String],
    at: Option<Ms>,
) -> LineOutcome {
    LineOutcome {
        ops: vec![drift(ctx, stream_id, record_kind, code, paths, at)],
        status: LineStatus::HardError {
            code: code.to_owned(),
        },
    }
}

fn drift(
    ctx: &NormalizeCtx,
    stream_id: &str,
    record_kind: &str,
    error_code: &str,
    paths: &[String],
    at: Option<Ms>,
) -> IngestOp {
    IngestOp::Drift(DriftFact {
        runtime_id: ctx.runtime_id.clone(),
        agent_version: ctx.agent_version.clone().unwrap_or_default(),
        stream: stream_id.to_owned(),
        record_kind: record_kind.to_owned(),
        error_code: error_code.to_owned(),
        shape_hash: ids::shape_hash(paths),
        at,
        sample_cursor: ctx.src_cursor,
        sample_offset: ctx.src_offset,
    })
}

fn stamp(value: &Value) -> Option<Ms> {
    value["timestamp"].as_str().and_then(rfc3339_ms)
}

fn keys(value: &Value) -> Vec<String> {
    match value.as_object() {
        Some(map) => map.keys().cloned().collect(),
        None => vec!["<non-object>".to_owned()],
    }
}
