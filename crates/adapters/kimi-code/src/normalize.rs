//! Kimi wire records to canonical ingest operations.
//!
//! Kimi has duplicate persistence rails inside one wire file: `turn.prompt`
//! precedes the richer `context.append_message`, and `usage.record` repeats the
//! usage later carried by `step.end`. This projection takes the message rail
//! and the dedicated usage rail exactly once.

use std::collections::BTreeSet;

use rosetta_adapter_support::tool::{
    projection_record_kind, projection_shape_paths, terminal_update, unknown_interaction,
};
use rosetta_adapter_support::{Drift, LineOutcome, LineStatus, NormalizeCtx};
use rosetta_core::rows::{
    DriftFact, MessageKind, MessageRow, Ms, TaskRow, TaskStatus, TokenSnapshot,
};
use rosetta_core::timeline::{AttachmentKind, AttachmentRef, ResourceRef, ToolAction};
use rosetta_core::{IngestOp, ids};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::raw::wire::{
    ConfigUpdateLine, ContentBlock, ContextAppendLoopEventLine, ContextAppendMessageLine,
    ContextApplyCompactionLine, KnownRecord, ProfileBindLine, RecordedUsage, ToolResultBody,
    ToolsUpdateStoreLine, UsageRecordLine,
};
use crate::raw::{ParsedLine, parse_wire_line};
use crate::spec;
use crate::time::epoch_ms;

const UNKNOWN_TYPE: &str = "unknown_type";
const UNKNOWN_SUBTYPE: &str = "unknown_subtype";
const UNKNOWN_FIELDS: &str = "unknown_fields";
const PARSE_FAILED: &str = "parse_failed";
const INVALID_JSON: &str = "invalid_json";
const INVALID_UTF8: &str = "invalid_utf8";
const NO_TYPE: &str = "<none>";
const INVALID_JSON_KIND: &str = "<invalid-json>";
const BINARY_KIND: &str = "<binary>";

#[derive(Debug, PartialEq, Eq)]
enum ContentPartError {
    Undeclared(String),
    UnsupportedDeclared(String),
    Malformed(&'static str),
}

/// Per-wire state. Each agent wire gets its own `NormalizeCtx`, so model,
/// pending usage, transient tool cwd, and ordinals never cross agent sessions.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KimiCodeState {
    /// Execution context for resolving tool paths only. Canonical cwd is meta.
    tool_cwd: Option<String>,
    model: Option<String>,
    effort: Option<String>,
    pending_usage: Option<TokenSnapshot>,
    /// Result records carry no tool name. Remember only reviewed Agent calls
    /// so unrelated successful output cannot masquerade as a child link.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    delegate_calls: BTreeSet<String>,
}

pub fn ingest_line(
    ctx: &mut NormalizeCtx,
    state: &mut KimiCodeState,
    stream_id: &str,
    line: &[u8],
) -> LineOutcome {
    let Ok(text) = std::str::from_utf8(line) else {
        return hard(ctx, stream_id, BINARY_KIND, INVALID_UTF8, &[], None);
    };
    if text.trim().is_empty() {
        return LineOutcome::parsed(Vec::new());
    }

    let parsed = match parse_wire_line(text) {
        Ok(parsed) => parsed,
        Err(_) => return parse_failure(ctx, stream_id, text),
    };
    let record = match parsed {
        ParsedLine::Known(record) => record,
        ParsedLine::UnknownType { rtype, value } => {
            let kind = rtype.as_deref().unwrap_or(NO_TYPE);
            if spec::declared_wire_types().contains(kind) {
                return LineOutcome::parsed(Vec::new());
            }
            let op = drift(
                ctx,
                stream_id,
                kind,
                UNKNOWN_TYPE,
                &top_level_keys(&value),
                value_time(&value),
            );
            return LineOutcome {
                ops: vec![op],
                status: LineStatus::UnknownType,
            };
        }
    };

    let mut ops = Vec::new();
    let paths = record.drift_paths();
    if !paths.is_empty() {
        ops.push(drift(
            ctx,
            stream_id,
            record_kind(&record),
            UNKNOWN_FIELDS,
            &paths,
            record_time(&record),
        ));
    }
    let status = project(ctx, state, stream_id, &record, &mut ops);
    LineOutcome { ops, status }
}

fn project(
    ctx: &mut NormalizeCtx,
    state: &mut KimiCodeState,
    stream_id: &str,
    record: &KnownRecord,
    ops: &mut Vec<IngestOp>,
) -> LineStatus {
    match record {
        KnownRecord::ProfileBind(line) => profile_bind(ctx, state, stream_id, line, ops),
        KnownRecord::ContextAppendMessage(line) => context_message(ctx, stream_id, line, ops),
        KnownRecord::ContextAppendLoopEvent(line) => loop_event(ctx, state, stream_id, line, ops),
        KnownRecord::LlmRequest(line) => {
            state.model = Some(line.model.clone());
            state.effort = Some(line.thinking_effort.clone());
            LineStatus::Parsed
        }
        KnownRecord::UsageRecord(line) => usage_record(ctx, state, stream_id, line, ops),
        KnownRecord::ConfigUpdate(line) => config_update(ctx, state, stream_id, line, ops),
        KnownRecord::ContextApplyCompaction(line) => context_compaction(ctx, stream_id, line, ops),
        KnownRecord::ToolsUpdateStore(line) => tools_update_store(ctx, stream_id, line, ops),
        // Declared bookkeeping. `turn.prompt` duplicates the subsequently
        // appended context message; step usage is handled inside the loop-event
        // router and duplicates the preceding usage.record. Prompt acceptance,
        // approval interaction, and compaction lifecycle records carry no
        // additional canonical content beyond the typed records above.
        KnownRecord::Metadata(_)
        | KnownRecord::RuntimeSetBinding(_)
        | KnownRecord::PermissionSetMode(_)
        | KnownRecord::TurnPrompt(_)
        | KnownRecord::PluginSessionStart(_)
        | KnownRecord::LlmToolsSnapshot(_)
        | KnownRecord::TokenCountingMeasured(_)
        | KnownRecord::TurnEnded(_)
        | KnownRecord::TokenCountingTurnRecorded(_)
        | KnownRecord::FullCompactionBegin(_)
        | KnownRecord::FullCompactionComplete(_)
        | KnownRecord::InteractionRequest(_)
        | KnownRecord::InteractionResolved(_)
        | KnownRecord::PermissionRecordApprovalResult(_)
        | KnownRecord::PromptAccepted(_) => LineStatus::Parsed,
    }
}

fn profile_bind(
    ctx: &NormalizeCtx,
    state: &mut KimiCodeState,
    stream_id: &str,
    line: &ProfileBindLine,
    ops: &mut Vec<IngestOp>,
) -> LineStatus {
    let at = match epoch_ms(line.time) {
        Some(at) => at,
        None => {
            return hard_into(
                ctx,
                stream_id,
                "profile.bind",
                "timestamp_out_of_range",
                None,
                ops,
            );
        }
    };
    let Some(cwd) = line.environment_disclosure["cwd"]
        .as_str()
        .filter(|cwd| !cwd.is_empty())
    else {
        return hard_into(ctx, stream_id, "profile.bind", "missing_cwd", Some(at), ops);
    };

    state.model = Some(line.model_alias.clone());
    state.effort = Some(line.thinking_effort.clone());
    state.tool_cwd = Some(cwd.to_owned());
    LineStatus::Parsed
}

fn config_update(
    ctx: &NormalizeCtx,
    state: &mut KimiCodeState,
    stream_id: &str,
    line: &ConfigUpdateLine,
    ops: &mut Vec<IngestOp>,
) -> LineStatus {
    let Some(at) = epoch_ms(line.time) else {
        return hard_into(
            ctx,
            stream_id,
            "config.update",
            "timestamp_out_of_range",
            None,
            ops,
        );
    };
    let cwd = line.environment_disclosure.cwd.trim();
    if cwd.is_empty() {
        return hard_into(
            ctx,
            stream_id,
            "config.update",
            "missing_cwd",
            Some(at),
            ops,
        );
    }
    state.tool_cwd = Some(cwd.to_owned());
    LineStatus::Parsed
}

fn context_compaction(
    ctx: &mut NormalizeCtx,
    stream_id: &str,
    line: &ContextApplyCompactionLine,
    ops: &mut Vec<IngestOp>,
) -> LineStatus {
    let Some(ts) = epoch_ms(line.time) else {
        return hard_into(
            ctx,
            stream_id,
            "context.apply_compaction",
            "timestamp_out_of_range",
            None,
            ops,
        );
    };
    let content = if line.context_summary.trim().is_empty() {
        line.summary.trim()
    } else {
        line.context_summary.trim()
    };
    if content.is_empty() {
        return hard_into(
            ctx,
            stream_id,
            "context.apply_compaction",
            "missing_summary",
            Some(ts),
            ops,
        );
    }
    let discriminator = source_discriminator(ctx, "context.apply_compaction");
    let mut summary = row(ctx, &discriminator, MessageKind::Summary, Some(ts));
    summary.subtype = Some("context.apply_compaction".to_owned());
    summary.content = Some(content.to_owned());
    IngestOp::push_message(ops, summary);
    LineStatus::Parsed
}

fn tools_update_store(
    ctx: &NormalizeCtx,
    stream_id: &str,
    line: &ToolsUpdateStoreLine,
    ops: &mut Vec<IngestOp>,
) -> LineStatus {
    let Some(at) = epoch_ms(line.time) else {
        return hard_into(
            ctx,
            stream_id,
            "tools.update_store",
            "timestamp_out_of_range",
            None,
            ops,
        );
    };
    let mut status = LineStatus::Parsed;
    for (index, item) in line.value.iter().enumerate() {
        let task_status = match item.status.as_str() {
            "pending" => TaskStatus::Pending,
            "in_progress" => TaskStatus::InProgress,
            "done" | "completed" => TaskStatus::Completed,
            other => {
                ops.push(drift(
                    ctx,
                    stream_id,
                    &format!("tools.update_store/status/{other}"),
                    UNKNOWN_SUBTYPE,
                    &[],
                    Some(at),
                ));
                status = LineStatus::UnknownType;
                TaskStatus::Pending
            }
        };
        let title = item.title.trim();
        if title.is_empty() {
            return hard_into(
                ctx,
                stream_id,
                "tools.update_store",
                "missing_task_title",
                Some(at),
                ops,
            );
        }
        // Kimi exposes no native todo id. The store key and title are the
        // stable facts; the ordinal only disambiguates duplicate titles in a
        // single snapshot.
        let duplicate = line.value[..index]
            .iter()
            .filter(|prior| prior.title.trim() == title)
            .count();
        let native_id = format!("{}:{title}:{duplicate}", line.key);
        ops.push(IngestOp::Task(TaskRow {
            id: ids::task_id(&ctx.session_id, &native_id),
            session_id: ctx.session_id.clone(),
            status: task_status,
            content: title.to_owned(),
            created_at: None,
            updated_at: Some(at),
        }));
    }
    status
}

fn context_message(
    ctx: &mut NormalizeCtx,
    stream_id: &str,
    line: &ContextAppendMessageLine,
    ops: &mut Vec<IngestOp>,
) -> LineStatus {
    let ts = match epoch_ms(line.time) {
        Some(ts) => ts,
        None => {
            return hard_into(
                ctx,
                stream_id,
                "context.append_message",
                "timestamp_out_of_range",
                None,
                ops,
            );
        }
    };
    if line.message.role != "user" {
        return hard_into(
            ctx,
            stream_id,
            "context.append_message",
            "unsupported_role",
            Some(ts),
            ops,
        );
    }
    let content = match content_text(&line.message.content) {
        Ok(content) => content,
        Err(error) => {
            return content_part_failure(
                ctx,
                stream_id,
                "context.append_message/content",
                error,
                Some(ts),
                ops,
            );
        }
    };
    let (kind, subtype) = match line.message.origin.kind.as_str() {
        "user" => (MessageKind::User, None),
        "injection" => (
            MessageKind::System,
            line.message
                .origin
                .variant
                .clone()
                .or_else(|| Some("injection".to_owned())),
        ),
        "system_trigger" => (
            MessageKind::System,
            line.message
                .origin
                .name
                .clone()
                .or_else(|| Some("system_trigger".to_owned())),
        ),
        other => {
            ops.push(drift(
                ctx,
                stream_id,
                &format!("context.append_message/origin/{other}"),
                UNKNOWN_SUBTYPE,
                &[],
                Some(ts),
            ));
            return LineStatus::UnknownType;
        }
    };
    let discriminator = match line.message.id.as_deref() {
        Some(id) => format!("message:{id}"),
        None => ctx.src_offset.map_or_else(
            || {
                format!(
                    "context.append_message:{}:{}:{}:{}",
                    line.time,
                    line.message.origin.kind,
                    line.message.origin.variant.as_deref().unwrap_or(""),
                    line.message.origin.name.as_deref().unwrap_or("")
                )
            },
            |offset| format!("wire@{offset}:context.append_message"),
        ),
    };
    let mut row = row(ctx, &discriminator, kind, Some(ts));
    row.subtype = subtype;
    row.uuid.clone_from(&line.message.id);
    row.content = content;
    row.attachments = content_attachments(&line.message.content);
    IngestOp::push_message(ops, row);
    LineStatus::Parsed
}

fn loop_event(
    ctx: &mut NormalizeCtx,
    state: &mut KimiCodeState,
    stream_id: &str,
    line: &ContextAppendLoopEventLine,
    ops: &mut Vec<IngestOp>,
) -> LineStatus {
    match line.event.event_type.as_str() {
        "content.part" => content_part(ctx, state, stream_id, line, ops),
        "tool.call" => tool_call(ctx, state, stream_id, line, ops),
        "tool.result" => tool_result(ctx, state, stream_id, line, ops),
        declared if spec::declared_loop_event_types().contains(declared) => LineStatus::Parsed,
        other => {
            ops.push(drift(
                ctx,
                stream_id,
                &format!("context.append_loop_event/{other}"),
                UNKNOWN_SUBTYPE,
                &[],
                epoch_ms(line.time),
            ));
            LineStatus::UnknownType
        }
    }
}

fn content_part(
    ctx: &mut NormalizeCtx,
    state: &mut KimiCodeState,
    stream_id: &str,
    line: &ContextAppendLoopEventLine,
    ops: &mut Vec<IngestOp>,
) -> LineStatus {
    let Some(ts) = epoch_ms(line.time) else {
        return hard_into(
            ctx,
            stream_id,
            "context.append_loop_event/content.part",
            "timestamp_out_of_range",
            None,
            ops,
        );
    };
    let Some(part) = line.event.part.as_ref() else {
        return hard_into(
            ctx,
            stream_id,
            "context.append_loop_event/content.part",
            "missing_part",
            Some(ts),
            ops,
        );
    };
    if let Err(error) = validate_content_part(part) {
        return content_part_failure(
            ctx,
            stream_id,
            "context.append_loop_event/content.part",
            error,
            Some(ts),
            ops,
        );
    }
    let (kind, content) = match part.block_type.as_str() {
        "text" => (MessageKind::Agent, part.text.clone()),
        "think" => (MessageKind::Thinking, part.think.clone()),
        "image_url" | "audio_url" | "video_url" => (MessageKind::Agent, None),
        other => {
            return content_part_failure(
                ctx,
                stream_id,
                "context.append_loop_event/content.part",
                ContentPartError::UnsupportedDeclared(other.to_owned()),
                Some(ts),
                ops,
            );
        }
    };

    let discriminator = line.event.uuid.as_deref().map_or_else(
        || source_discriminator(ctx, "content.part"),
        |uuid| format!("loop:{uuid}"),
    );
    let mut row = row(ctx, &discriminator, kind, Some(ts));
    row.uuid.clone_from(&line.event.uuid);
    row.parent_uuid.clone_from(&line.event.parent_uuid);
    row.content = content;
    row.attachments = content_attachments(std::slice::from_ref(part));
    stamp_response(&mut row, state);
    IngestOp::push_message(ops, row);
    LineStatus::Parsed
}

fn tool_call(
    ctx: &mut NormalizeCtx,
    state: &mut KimiCodeState,
    stream_id: &str,
    line: &ContextAppendLoopEventLine,
    ops: &mut Vec<IngestOp>,
) -> LineStatus {
    let Some(ts) = epoch_ms(line.time) else {
        return hard_into(
            ctx,
            stream_id,
            "context.append_loop_event/tool.call",
            "timestamp_out_of_range",
            None,
            ops,
        );
    };
    let Some(tool_id) = line.event.tool_call_id.as_deref() else {
        return hard_into(
            ctx,
            stream_id,
            "context.append_loop_event/tool.call",
            "missing_tool_call_id",
            Some(ts),
            ops,
        );
    };
    let Some(name) = line.event.name.as_deref() else {
        return hard_into(
            ctx,
            stream_id,
            "context.append_loop_event/tool.call",
            "missing_name",
            Some(ts),
            ops,
        );
    };
    let input = line.event.args.clone().unwrap_or(Value::Null);

    let discriminator = line
        .event
        .uuid
        .as_deref()
        .map_or_else(|| format!("tool:{tool_id}"), |uuid| format!("loop:{uuid}"));
    let mut row = row(ctx, &discriminator, MessageKind::ToolUse, Some(ts));
    row.uuid.clone_from(&line.event.uuid);
    row.parent_uuid.clone_from(&line.event.parent_uuid);
    let projected = crate::tool::project_request(
        ctx.agent_version.as_deref(),
        name,
        &input,
        state.tool_cwd.as_deref(),
        tool_id,
        Some(tool_id),
    );
    let (tool, is_delegate) = match projected {
        Ok(tool) => {
            let is_delegate = matches!(&tool.action, ToolAction::Delegate { .. });
            (tool, is_delegate)
        }
        Err(error) => {
            ops.push(drift(
                ctx,
                stream_id,
                &projection_record_kind("request", None, name),
                error.drift_code(),
                &projection_shape_paths(&input),
                Some(ts),
            ));
            (
                unknown_interaction(
                    tool_id.to_owned(),
                    rosetta_core::CorrelationDerivation::NativeId,
                    Some(tool_id.to_owned()),
                    None,
                    name.to_owned(),
                    None,
                ),
                false,
            )
        }
    };
    row.tool = Some(tool);
    // Native naming cannot establish a canonical action. If the projector
    // rejected this writer version or request shape, the Unknown opener must
    // not later receive a Delegate-only child-session patch.
    if is_delegate {
        row.branch_tool_id = Some(tool_id.to_owned());
        state.delegate_calls.insert(tool_id.to_owned());
    }
    stamp_response(&mut row, state);
    IngestOp::push_message(ops, row);
    LineStatus::Parsed
}

fn tool_result(
    ctx: &NormalizeCtx,
    state: &mut KimiCodeState,
    stream_id: &str,
    line: &ContextAppendLoopEventLine,
    ops: &mut Vec<IngestOp>,
) -> LineStatus {
    let Some(ts) = epoch_ms(line.time) else {
        return hard_into(
            ctx,
            stream_id,
            "context.append_loop_event/tool.result",
            "timestamp_out_of_range",
            None,
            ops,
        );
    };
    let Some(tool_id) = line.event.tool_call_id.as_deref() else {
        return hard_into(
            ctx,
            stream_id,
            "context.append_loop_event/tool.result",
            "missing_tool_call_id",
            Some(ts),
            ops,
        );
    };
    let Some(result) = line.event.result.as_ref() else {
        return hard_into(
            ctx,
            stream_id,
            "context.append_loop_event/tool.result",
            "missing_result",
            Some(ts),
            ops,
        );
    };
    let text = match result_text(result) {
        Ok(text) => text,
        Err(error) => {
            return content_part_failure(
                ctx,
                stream_id,
                "context.append_loop_event/tool.result",
                error,
                Some(ts),
                ops,
            );
        }
    };
    let delegate_child_session_id = state
        .delegate_calls
        .remove(tool_id)
        .then(|| text.as_deref().and_then(delegate_agent_id))
        .flatten()
        .zip(ctx.native_session_id.as_deref())
        .map(|(agent_id, native_session_id)| {
            let root = native_session_id
                .split_once("/agents/")
                .map_or(native_session_id, |(root, _)| root);
            ids::session_id(&ctx.runtime_id, &format!("{root}/agents/{agent_id}"))
        });
    let mut patch = terminal_update(
        result.is_error,
        true,
        rosetta_core::StatusBasis::NativeTerminalRecord,
        Some(ts),
        text.as_deref(),
    );
    patch.delegate_child_session_id = delegate_child_session_id;
    ops.push(IngestOp::ToolUpdate {
        session_id: ctx.session_id.clone(),
        correlation_key: tool_id.to_owned(),
        evidence: ctx.evidence(0),
        patch: Box::new(patch),
        at: Some(ts),
    });
    LineStatus::Parsed
}

/// Kimi's reviewed Agent result protocol emits one stable `agent_id:` line.
/// Successful command output is otherwise discarded at normalization.
fn delegate_agent_id(text: &str) -> Option<&str> {
    text.lines().find_map(|line| {
        line.strip_prefix("agent_id:")
            .map(str::trim)
            .filter(|id| !id.is_empty() && !id.chars().any(char::is_whitespace))
    })
}

fn content_part_failure(
    ctx: &NormalizeCtx,
    stream_id: &str,
    record_kind: &str,
    error: ContentPartError,
    at: Option<Ms>,
    ops: &mut Vec<IngestOp>,
) -> LineStatus {
    match error {
        ContentPartError::Undeclared(kind) => {
            ops.push(drift(
                ctx,
                stream_id,
                &format!("{record_kind}/{kind}"),
                UNKNOWN_SUBTYPE,
                &[],
                at,
            ));
            LineStatus::UnknownType
        }
        ContentPartError::UnsupportedDeclared(kind) => hard_into(
            ctx,
            stream_id,
            &format!("{record_kind}/{kind}"),
            "unsupported_declared_content_part",
            at,
            ops,
        ),
        ContentPartError::Malformed(code) => hard_into(ctx, stream_id, record_kind, code, at, ops),
    }
}

fn usage_record(
    ctx: &NormalizeCtx,
    state: &mut KimiCodeState,
    stream_id: &str,
    line: &UsageRecordLine,
    ops: &mut Vec<IngestOp>,
) -> LineStatus {
    match line.usage_scope.as_str() {
        "turn" => {
            let Some(tokens) = usage_snapshot(&line.usage) else {
                return hard_into(
                    ctx,
                    stream_id,
                    "usage.record",
                    "usage_out_of_range",
                    epoch_ms(line.time),
                    ops,
                );
            };
            state.pending_usage = Some(tokens);
            LineStatus::Parsed
        }
        // The canonical observes this scope but does not establish whether it
        // is a running counter or a terminal total. It is deliberately not
        // projected as per-response usage.
        "session" => LineStatus::Parsed,
        other => {
            ops.push(drift(
                ctx,
                stream_id,
                &format!("usage.record/{other}"),
                UNKNOWN_SUBTYPE,
                &[],
                epoch_ms(line.time),
            ));
            LineStatus::UnknownType
        }
    }
}

fn stamp_response(row: &mut MessageRow, state: &mut KimiCodeState) {
    row.model.clone_from(&state.model);
    row.effort.clone_from(&state.effort);
    row.usage = state.pending_usage.take();
}

fn row(
    ctx: &mut NormalizeCtx,
    discriminator: &str,
    kind: MessageKind,
    ts: Option<Ms>,
) -> MessageRow {
    MessageRow {
        timeline_id: ids::timeline_id(&ctx.session_id, discriminator),
        session_id: ctx.session_id.clone(),
        ordinal: ctx.take_ordinal(),
        kind,
        subtype: None,
        uuid: None,
        parent_uuid: None,
        ts,
        model: None,
        effort: None,
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

fn source_discriminator(ctx: &NormalizeCtx, kind: &str) -> String {
    if let Some(offset) = ctx.src_offset {
        format!("wire@{offset}:{kind}")
    } else if let Some(cursor) = ctx.src_cursor {
        format!("wire#{cursor}:{kind}")
    } else {
        format!("ordinal@{}:{kind}", ctx.next_ordinal)
    }
}

fn validate_content_part(block: &ContentBlock) -> Result<(), ContentPartError> {
    if !spec::declared_content_part_types().contains(&block.block_type) {
        return Err(ContentPartError::Undeclared(block.block_type.clone()));
    }
    match block.block_type.as_str() {
        "text" if block.text.is_none() => Err(ContentPartError::Malformed("missing_text")),
        "think" if block.think.is_none() => Err(ContentPartError::Malformed("missing_think")),
        "image_url" if block.image_url.is_none() => {
            Err(ContentPartError::Malformed("missing_image_url"))
        }
        "audio_url" if block.audio_url.is_none() => {
            Err(ContentPartError::Malformed("missing_audio_url"))
        }
        "video_url" if block.video_url.is_none() => {
            Err(ContentPartError::Malformed("missing_video_url"))
        }
        "text" | "think" | "image_url" | "audio_url" | "video_url" => Ok(()),
        other => Err(ContentPartError::UnsupportedDeclared(other.to_owned())),
    }
}

fn content_text(blocks: &[ContentBlock]) -> Result<Option<String>, ContentPartError> {
    let mut text = Vec::with_capacity(blocks.len());
    for block in blocks {
        validate_content_part(block)?;
        if block.block_type == "text" {
            text.push(block.text.as_deref().expect("validated text part"));
        }
    }
    let joined = text.join("\n");
    Ok((!joined.is_empty()).then_some(joined))
}

/// Kimi persists authored media as references, not bytes. Preserve only that
/// user-facing reference and its coarse kind; encrypted/context payloads and
/// tool-result media never call this helper.
fn content_attachments(blocks: &[ContentBlock]) -> Vec<AttachmentRef> {
    blocks
        .iter()
        .filter_map(|block| {
            let (kind, media) = match block.block_type.as_str() {
                "image_url" => (AttachmentKind::Image, block.image_url.as_ref()?),
                "audio_url" => (AttachmentKind::Audio, block.audio_url.as_ref()?),
                "video_url" => (AttachmentKind::Video, block.video_url.as_ref()?),
                _ => return None,
            };
            (!media.url.is_empty()).then(|| AttachmentRef {
                kind,
                name: media.id.clone(),
                media_type: None,
                byte_len: None,
                source: ResourceRef::Url {
                    url: media.url.clone(),
                },
                dimensions: None,
            })
        })
        .collect()
}

fn usage_snapshot(usage: &RecordedUsage) -> Option<TokenSnapshot> {
    Some(TokenSnapshot {
        input: Some(i64::try_from(usage.input_other).ok()?),
        output: Some(i64::try_from(usage.output).ok()?),
        cache_read: Some(i64::try_from(usage.input_cache_read).ok()?),
        cache_write: Some(i64::try_from(usage.input_cache_creation).ok()?),
        // Kimi's native usage shape declares no thinking-token component.
        thinking: None,
    })
}

fn result_text(result: &ToolResultBody) -> Result<Option<String>, ContentPartError> {
    match &result.output {
        Value::String(text) => Ok(Some(text.clone())),
        Value::Array(blocks) => {
            let blocks = serde_json::from_value::<Vec<ContentBlock>>(Value::Array(blocks.clone()))
                .map_err(|_| ContentPartError::Malformed("invalid_result_blocks"))?;
            content_text(&blocks)
        }
        _ => Err(ContentPartError::Malformed("unsupported_result_output")),
    }
}

fn parse_failure(ctx: &NormalizeCtx, stream_id: &str, text: &str) -> LineOutcome {
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        return hard(ctx, stream_id, INVALID_JSON_KIND, INVALID_JSON, &[], None);
    };
    let kind = value["type"].as_str().unwrap_or(NO_TYPE);
    hard(
        ctx,
        stream_id,
        kind,
        PARSE_FAILED,
        &top_level_keys(&value),
        value_time(&value),
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

fn hard_into(
    ctx: &NormalizeCtx,
    stream_id: &str,
    record_kind: &str,
    code: &str,
    at: Option<Ms>,
    ops: &mut Vec<IngestOp>,
) -> LineStatus {
    ops.push(drift(ctx, stream_id, record_kind, code, &[], at));
    LineStatus::HardError {
        code: code.to_owned(),
    }
}

fn drift(
    ctx: &NormalizeCtx,
    stream_id: &str,
    record_kind: &str,
    code: &str,
    paths: &[String],
    at: Option<Ms>,
) -> IngestOp {
    IngestOp::Drift(DriftFact {
        runtime_id: ctx.runtime_id.clone(),
        agent_version: ctx.agent_version.clone().unwrap_or_default(),
        stream: stream_id.to_owned(),
        record_kind: record_kind.to_owned(),
        error_code: code.to_owned(),
        shape_hash: ids::shape_hash(paths),
        at,
        sample_cursor: ctx.src_cursor,
        sample_offset: ctx.src_offset,
    })
}

fn record_kind(record: &KnownRecord) -> &'static str {
    match record {
        KnownRecord::Metadata(_) => "metadata",
        KnownRecord::RuntimeSetBinding(_) => "runtime.set_binding",
        KnownRecord::ProfileBind(_) => "profile.bind",
        KnownRecord::PermissionSetMode(_) => "permission.set_mode",
        KnownRecord::TurnPrompt(_) => "turn.prompt",
        KnownRecord::ContextAppendMessage(_) => "context.append_message",
        KnownRecord::PluginSessionStart(_) => "plugin.session_start",
        KnownRecord::ContextAppendLoopEvent(_) => "context.append_loop_event",
        KnownRecord::LlmToolsSnapshot(_) => "llm.tools_snapshot",
        KnownRecord::LlmRequest(_) => "llm.request",
        KnownRecord::UsageRecord(_) => "usage.record",
        KnownRecord::TokenCountingMeasured(_) => "token_counting.measured",
        KnownRecord::TurnEnded(_) => "turn.ended",
        KnownRecord::TokenCountingTurnRecorded(_) => "token_counting.turn_recorded",
        KnownRecord::ConfigUpdate(_) => "config.update",
        KnownRecord::ContextApplyCompaction(_) => "context.apply_compaction",
        KnownRecord::FullCompactionBegin(_) => "full_compaction.begin",
        KnownRecord::FullCompactionComplete(_) => "full_compaction.complete",
        KnownRecord::InteractionRequest(_) => "interaction.request",
        KnownRecord::InteractionResolved(_) => "interaction.resolved",
        KnownRecord::PermissionRecordApprovalResult(_) => "permission.record_approval_result",
        KnownRecord::PromptAccepted(_) => "prompt.accepted",
        KnownRecord::ToolsUpdateStore(_) => "tools.update_store",
    }
}

fn record_time(record: &KnownRecord) -> Option<Ms> {
    let time = match record {
        KnownRecord::Metadata(line) => line.created_at,
        KnownRecord::RuntimeSetBinding(line) => line.time,
        KnownRecord::ProfileBind(line) => line.time,
        KnownRecord::PermissionSetMode(line) => line.time,
        KnownRecord::TurnPrompt(line) => line.time,
        KnownRecord::ContextAppendMessage(line) => line.time,
        KnownRecord::PluginSessionStart(line) => line.time,
        KnownRecord::ContextAppendLoopEvent(line) => line.time,
        KnownRecord::LlmToolsSnapshot(line) => line.time,
        KnownRecord::LlmRequest(line) => line.time,
        KnownRecord::UsageRecord(line) => line.time,
        KnownRecord::TokenCountingMeasured(line) => line.time,
        KnownRecord::TurnEnded(line) => line.time,
        KnownRecord::TokenCountingTurnRecorded(line) => line.time,
        KnownRecord::ConfigUpdate(line) => line.time,
        KnownRecord::ContextApplyCompaction(line) => line.time,
        KnownRecord::FullCompactionBegin(line) => line.time,
        KnownRecord::FullCompactionComplete(line) => line.time,
        KnownRecord::InteractionRequest(line) => line.time,
        KnownRecord::InteractionResolved(line) => line.time,
        KnownRecord::PermissionRecordApprovalResult(line) => line.time,
        KnownRecord::PromptAccepted(line) => line.time,
        KnownRecord::ToolsUpdateStore(line) => line.time,
    };
    epoch_ms(time)
}

fn value_time(value: &Value) -> Option<Ms> {
    value["time"]
        .as_u64()
        .or_else(|| value["created_at"].as_u64())
        .and_then(epoch_ms)
}

fn top_level_keys(value: &Value) -> Vec<String> {
    value
        .as_object()
        .map(|object| object.keys().cloned().collect())
        .unwrap_or_else(|| vec!["<non-object>".to_owned()])
}

#[cfg(test)]
mod tests {
    use super::{result_text, usage_snapshot};
    use crate::raw::wire::{RecordedUsage, ToolResultBody};
    use rosetta_adapter_support::Unknown;
    use serde_json::json;

    #[test]
    fn reported_zero_usage_stays_reported() {
        let usage = RecordedUsage {
            input_other: 0,
            output: 0,
            input_cache_read: 0,
            input_cache_creation: 0,
            unknown: Unknown::new(),
        };
        let tokens = usage_snapshot(&usage).expect("representable usage");
        assert_eq!(tokens.input, Some(0));
        assert_eq!(tokens.output, Some(0));
        assert_eq!(tokens.cache_read, Some(0));
        assert_eq!(tokens.cache_write, Some(0));
        assert_eq!(tokens.thinking, None);
    }

    #[test]
    fn result_output_preserves_string_and_text_blocks() {
        let body = ToolResultBody {
            output: json!("plain"),
            is_error: None,
            note: None,
            unknown: Unknown::new(),
        };
        assert_eq!(result_text(&body), Ok(Some("plain".to_owned())));

        let body = ToolResultBody {
            output: json!([
                {"type": "text", "text": "first"},
                {"type": "text", "text": "second"}
            ]),
            is_error: Some(false),
            note: None,
            unknown: Unknown::new(),
        };
        assert_eq!(result_text(&body), Ok(Some("first\nsecond".to_owned())));
    }
}
