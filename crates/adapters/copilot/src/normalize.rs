//! Copilot persisted events to canonical ingest operations.
//!
//! Assistant messages can contain reasoning, visible text, and one or more
//! tool requests, so they split into flat rows. Tool completions merge into
//! the request row by native `toolCallId`. Session shutdown metrics are real
//! running counters; `crate::usage` turns them into per-model intervals and
//! owns the per-call session-store rows.

use std::collections::{BTreeMap, BTreeSet};

use rosetta_adapter_support::Drift;
use rosetta_adapter_support::adapter::{LineOutcome, LineStatus, NormalizeCtx};
use rosetta_adapter_support::tool::{
    projection_record_kind, projection_shape_paths, terminal_update, unknown_interaction,
};
use rosetta_core::rows::{
    DriftFact, EditKind, FileEditRow, MessageKind, MessageRow, Ms, TaskRow, TaskStatus, ToolCall,
};
use rosetta_core::timeline::{CorrelationDerivation, StatusBasis};
use rosetta_core::{IngestOp, ids};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::raw::events::{
    AssistantMessage, EventLine, EventPayload, SessionNotice, SubagentCompleted, SubagentFailed,
    SubagentStarted, ToolExecutionComplete, ToolRequest,
};
use crate::raw::{ParsedLine, parse_line};
use crate::spec;
use crate::time::rfc3339_ms;
use crate::usage::{self, ShutdownTotals};

const UNKNOWN_TYPE: &str = "unknown_type";
const UNKNOWN_FIELDS: &str = "unknown_fields";
const PARSE_FAILED: &str = "parse_failed";
const NO_TYPE: &str = "<none>";
const INVALID_JSON: &str = "<invalid-json>";

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct CopilotState {
    #[serde(default)]
    images: crate::images::Images,
    /// Current execution cwd for resolving relative tool inputs only.
    /// The canonical session cwd and project are fixed during meta ingest.
    tool_cwd: Option<String>,
    model: Option<String>,
    effort: Option<String>,
    /// Tool requests and execution starts describe the same native call. This
    /// set ensures they produce one timeline row across checkpoint resumes.
    seen_tools: BTreeSet<String>,
    #[serde(default)]
    task_content: BTreeMap<String, String>,
    /// Last cumulative `session.shutdown` reading per model. Shutdowns
    /// re-report running session totals (resume ⇒ 128 → 256), so each one
    /// contributes only its interval's delta (RESEARCH_PLAN §5.13).
    #[serde(default)]
    pub(crate) model_totals: BTreeMap<String, ShutdownTotals>,
}

pub fn ingest_line(
    ctx: &mut NormalizeCtx,
    state: &mut CopilotState,
    stream_id: &str,
    line: &[u8],
) -> LineOutcome {
    let Ok(text) = std::str::from_utf8(line) else {
        return hard_error(ctx, stream_id, INVALID_JSON, None);
    };
    if text.trim().is_empty() {
        return LineOutcome::parsed(Vec::new());
    }

    let parsed = match parse_line(text) {
        Ok(parsed) => parsed,
        Err(_) => {
            return match serde_json::from_str::<Value>(text) {
                Ok(value) => {
                    let kind = value["type"].as_str().unwrap_or(NO_TYPE).to_owned();
                    hard_error(ctx, stream_id, &kind, Some(&value))
                }
                Err(_) => hard_error(ctx, stream_id, INVALID_JSON, None),
            };
        }
    };

    let record = match parsed {
        ParsedLine::Known(record) => record,
        ParsedLine::UnknownType { rtype, value } => {
            let kind = rtype.as_deref().unwrap_or(NO_TYPE);
            if spec::declared_event_types().contains(kind) {
                let paths = spec::unexpected_declared_fields(kind, &value);
                if paths.is_empty() {
                    return LineOutcome::parsed(Vec::new());
                }
                return LineOutcome::parsed(vec![drift(
                    ctx,
                    stream_id,
                    kind,
                    UNKNOWN_FIELDS,
                    &paths,
                    record_ts(&value),
                )]);
            }
            return LineOutcome {
                ops: vec![drift(
                    ctx,
                    stream_id,
                    kind,
                    UNKNOWN_TYPE,
                    &top_level_keys(&value),
                    record_ts(&value),
                )],
                status: LineStatus::UnknownType,
            };
        }
    };

    let ts = rfc3339_ms(&record.timestamp);
    let mut ops = Vec::new();
    let paths = record.drift_paths();
    if !paths.is_empty() {
        ops.push(drift(
            ctx,
            stream_id,
            record_kind(&record.payload),
            UNKNOWN_FIELDS,
            &paths,
            ts,
        ));
    }

    project(ctx, state, stream_id, &record, ts, &mut ops);
    LineOutcome::parsed(ops)
}

fn project(
    ctx: &mut NormalizeCtx,
    state: &mut CopilotState,
    stream_id: &str,
    record: &EventLine,
    ts: Option<Ms>,
    ops: &mut Vec<IngestOp>,
) {
    match &record.payload {
        EventPayload::SessionBinaryAsset(asset) => state.images.binary(ctx, asset, ops),
        EventPayload::Abort(abort) => {
            system_row(ctx, record, "abort", abort.reason.clone(), ts, ops)
        }
        EventPayload::AssistantReasoning(reasoning) => {
            let mut thinking = row(
                ctx,
                record,
                &format!("event:{}#reasoning", record.id),
                MessageKind::Thinking,
                ts,
            );
            thinking.uuid = Some(reasoning.reasoning_id.clone());
            thinking.model = state.model.clone();
            thinking.effort = state.effort.clone();
            thinking.content = Some(reasoning.content.clone());
            IngestOp::push_message(ops, thinking);
        }
        EventPayload::ExternalToolRequested(request) => {
            if state.seen_tools.insert(request.tool_call_id.clone()) {
                tool_event(
                    ctx,
                    state,
                    stream_id,
                    record,
                    ToolEvent {
                        call_id: &request.tool_call_id,
                        name: &request.tool_name,
                        namespace: None,
                        input: request.arguments.clone(),
                        ts,
                    },
                    ops,
                );
            }
            if let Some(cwd) = request.working_directory.as_deref() {
                remember_tool_cwd(state, cwd);
            }
        }
        EventPayload::ExternalToolCompleted(_) => {}
        EventPayload::SessionStart(start) => {
            ctx.agent_version = Some(start.copilot_version.clone());
            state.model = start.selected_model.clone();
            state.effort = start.reasoning_effort.clone();

            remember_context_cwd(state, start.context.as_ref());
        }
        EventPayload::SessionResume(resume) => {
            if let Some(model) = &resume.selected_model {
                state.model = Some(model.clone());
            }
            if let Some(effort) = &resume.reasoning_effort {
                state.effort = Some(effort.clone());
            }
            remember_context_cwd(state, resume.context.as_ref());
        }
        EventPayload::SessionModelChange(change) => {
            state.model = Some(change.new_model.clone());
            if let Some(effort) = &change.reasoning_effort {
                state.effort = effort.as_str().map(str::to_owned);
            }
        }
        EventPayload::AssistantTurnStart(turn) => {
            if let Some(model) = &turn.model {
                state.model = Some(model.clone());
            }
        }
        EventPayload::AssistantTurnEnd(turn) => {
            if let Some(model) = &turn.model {
                state.model = Some(model.clone());
            }
        }
        EventPayload::SystemMessage(message) => {
            let mut row = row(
                ctx,
                record,
                &format!("event:{}", record.id),
                MessageKind::System,
                ts,
            );
            row.subtype = message
                .name
                .clone()
                .or_else(|| (message.role != "system").then(|| message.role.clone()));
            row.content = Some(message.content.clone());
            IngestOp::push_message(ops, row);
        }
        EventPayload::UserMessage(message) => {
            let mut row = row(
                ctx,
                record,
                &format!("event:{}", record.id),
                MessageKind::User,
                ts,
            );
            row.content = Some(message.content.clone());
            state
                .images
                .user(row, message.attachments.as_deref().unwrap_or_default(), ops);
        }
        EventPayload::AssistantMessage(message) => {
            assistant_message(ctx, state, stream_id, record, message, ts, ops);
        }
        EventPayload::ToolExecutionStart(start) => {
            if let Some(model) = &start.model {
                state.model = Some(model.clone());
            }
            if state.seen_tools.insert(start.tool_call_id.clone()) {
                let mut tool_row = row(
                    ctx,
                    record,
                    &tool_discriminator(&start.tool_call_id),
                    MessageKind::ToolUse,
                    ts,
                );
                tool_row.model = start.model.clone().or_else(|| state.model.clone());
                tool_row.effort = state.effort.clone();
                let name = start.mcp_tool_name.as_deref().unwrap_or(&start.tool_name);
                tool_row.tool = Some(project_tool(
                    ctx,
                    state,
                    stream_id,
                    &start.tool_call_id,
                    name,
                    start.mcp_server_name.as_deref(),
                    start.arguments.as_ref(),
                    ts,
                    ops,
                ));
                IngestOp::push_message(ops, tool_row);
            }
        }
        EventPayload::ToolExecutionComplete(complete) => {
            if let Some(result) = complete.result.as_ref() {
                state
                    .images
                    .tool(ctx, &complete.tool_call_id, result, ts, ops);
            }
            tool_result(ctx, complete, ts, ops);
        }
        EventPayload::SessionShutdown(shutdown) => {
            if let Some(model) = &shutdown.current_model {
                state.model = Some(model.clone());
            }
            usage::shutdown_usage(ctx, state, &record.id, shutdown, ts, ops);
        }
        EventPayload::SessionContextChanged(context) => remember_tool_cwd(state, &context.cwd),
        EventPayload::SessionContextCleared(cleared) => {
            let content = cleared.initial_message.clone().unwrap_or_else(|| {
                format!(
                    "Cleared {} messages from context.",
                    cleared.messages_cleared
                )
            });
            summary_row(ctx, record, "context_cleared", content, ts, ops);
        }
        EventPayload::SessionCompactionComplete(compaction) => {
            if let Some(summary) = compaction
                .summary_content
                .clone()
                .filter(|summary| !summary.trim().is_empty())
            {
                summary_row(ctx, record, "compaction", summary, ts, ops);
            } else if !compaction.success
                && let Some(error) = compaction.error.clone()
            {
                system_row(ctx, record, "compaction_error", error, ts, ops);
            }
        }
        EventPayload::SessionHandoff(handoff) => {
            if let Some(content) = handoff
                .summary
                .clone()
                .or_else(|| handoff.context.clone())
                .filter(|content| !content.trim().is_empty())
            {
                summary_row(ctx, record, "handoff", content, ts, ops);
            }
        }
        EventPayload::SessionInfo(notice) => notice_row(ctx, record, "info", notice, ts, ops),
        EventPayload::SessionWarning(notice) => notice_row(ctx, record, "warning", notice, ts, ops),
        EventPayload::SessionError(notice) => notice_row(ctx, record, "error", notice, ts, ops),
        EventPayload::SessionTaskComplete(task) => {
            let Some(native_id) = task.objective_id.map(|id| format!("objective:{id}")) else {
                if let Some(summary) = task.summary.clone() {
                    summary_row(ctx, record, "task_complete", summary, ts, ops);
                }
                return;
            };
            let content = task
                .summary
                .clone()
                .or_else(|| task.reason.clone())
                .or_else(|| task.outcome.clone())
                .unwrap_or_else(|| "Autopilot objective".to_owned());
            task_row(
                ctx,
                state,
                TaskFact {
                    native_id,
                    content,
                    status: if task.success == Some(false) {
                        TaskStatus::Failed
                    } else {
                        TaskStatus::Completed
                    },
                    created_at: None,
                    updated_at: ts,
                },
                ops,
            );
        }
        EventPayload::SessionScheduleCreated(schedule) => {
            task_row(
                ctx,
                state,
                TaskFact {
                    native_id: format!("schedule:{}", schedule.id),
                    content: schedule
                        .display_prompt
                        .clone()
                        .unwrap_or_else(|| schedule.prompt.clone()),
                    status: TaskStatus::Pending,
                    created_at: ts,
                    updated_at: ts,
                },
                ops,
            );
        }
        EventPayload::SessionScheduleCancelled(schedule) => task_update(
            ctx,
            state,
            &format!("schedule:{}", schedule.id),
            TaskStatus::Cancelled,
            ts,
            ops,
        ),
        EventPayload::SessionScheduleRearmed(schedule) => task_update(
            ctx,
            state,
            &format!("schedule:{}", schedule.id),
            TaskStatus::Pending,
            ts,
            ops,
        ),
        EventPayload::SessionWorkspaceFileChanged(change) => {
            ops.push(IngestOp::FileEdit(FileEditRow {
                session_id: ctx.session_id.clone(),
                message_timeline_id: None,
                path: change.path.clone(),
                kind: match change.operation.as_str() {
                    "created" | "create" | "added" | "add" => EditKind::Write,
                    _ => EditKind::Edit,
                },
                snapshot_ref: None,
                lines_added: None,
                lines_removed: None,
                at: ts,
            }));
        }
        EventPayload::SkillInvoked(skill) => system_row(
            ctx,
            record,
            &format!("skill:{}", skill.name),
            skill.content.clone(),
            ts,
            ops,
        ),
        EventPayload::SubagentStarted(subagent) => subagent_started(ctx, state, subagent, ts, ops),
        EventPayload::SubagentCompleted(subagent) => {
            subagent_completed(ctx, state, subagent, ts, ops)
        }
        EventPayload::SubagentFailed(subagent) => subagent_failed(ctx, state, subagent, ts, ops),
        EventPayload::SystemNotification(notification) => system_row(
            ctx,
            record,
            "notification",
            notification.content.clone(),
            ts,
            ops,
        ),
        EventPayload::ToolUserRequested(request) => {
            if state.seen_tools.insert(request.tool_call_id.clone()) {
                tool_event(
                    ctx,
                    state,
                    stream_id,
                    record,
                    ToolEvent {
                        call_id: &request.tool_call_id,
                        name: &request.tool_name,
                        namespace: None,
                        input: request.arguments.clone(),
                        ts,
                    },
                    ops,
                );
            }
        }
        EventPayload::SessionAutopilotObjectiveChanged(_)
        | EventPayload::SessionUsageCheckpoint(_) => {}
        // Permission-mode changes are session settings, not timeline content.
        EventPayload::SessionPermissionsChanged(_) => {}
    }
}

fn system_row(
    ctx: &mut NormalizeCtx,
    record: &EventLine,
    subtype: &str,
    content: String,
    ts: Option<Ms>,
    ops: &mut Vec<IngestOp>,
) {
    let mut message = row(
        ctx,
        record,
        &format!("event:{}#system", record.id),
        MessageKind::System,
        ts,
    );
    message.subtype = Some(subtype.to_owned());
    message.content = Some(content);
    IngestOp::push_message(ops, message);
}

fn summary_row(
    ctx: &mut NormalizeCtx,
    record: &EventLine,
    subtype: &str,
    content: String,
    ts: Option<Ms>,
    ops: &mut Vec<IngestOp>,
) {
    let mut message = row(
        ctx,
        record,
        &format!("event:{}#summary", record.id),
        MessageKind::Summary,
        ts,
    );
    message.subtype = Some(subtype.to_owned());
    message.content = Some(content);
    IngestOp::push_message(ops, message);
}

fn notice_row(
    ctx: &mut NormalizeCtx,
    record: &EventLine,
    class: &str,
    notice: &SessionNotice,
    ts: Option<Ms>,
    ops: &mut Vec<IngestOp>,
) {
    let detail = notice
        .info_type
        .as_deref()
        .or(notice.warning_type.as_deref())
        .or(notice.error_type.as_deref());
    let subtype = detail.map_or_else(|| class.to_owned(), |detail| format!("{class}:{detail}"));
    system_row(ctx, record, &subtype, notice.message.clone(), ts, ops);
}

struct ToolEvent<'a> {
    call_id: &'a str,
    name: &'a str,
    namespace: Option<&'a str>,
    input: Option<Value>,
    ts: Option<Ms>,
}

fn tool_event(
    ctx: &mut NormalizeCtx,
    state: &mut CopilotState,
    stream_id: &str,
    record: &EventLine,
    event: ToolEvent<'_>,
    ops: &mut Vec<IngestOp>,
) {
    let mut tool = row(
        ctx,
        record,
        &tool_discriminator(event.call_id),
        MessageKind::ToolUse,
        event.ts,
    );
    tool.model = state.model.clone();
    tool.effort = state.effort.clone();
    tool.tool = Some(project_tool(
        ctx,
        state,
        stream_id,
        event.call_id,
        event.name,
        event.namespace,
        event.input.as_ref(),
        event.ts,
        ops,
    ));
    IngestOp::push_message(ops, tool);
}

struct TaskFact {
    native_id: String,
    content: String,
    status: TaskStatus,
    created_at: Option<Ms>,
    updated_at: Option<Ms>,
}

fn task_row(ctx: &NormalizeCtx, state: &mut CopilotState, task: TaskFact, ops: &mut Vec<IngestOp>) {
    state
        .task_content
        .insert(task.native_id.clone(), task.content.clone());
    let event_at = task.updated_at.or(task.created_at);
    let started_at = (task.status == TaskStatus::InProgress)
        .then_some(event_at)
        .flatten();
    let terminal_at = matches!(
        task.status,
        TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Cancelled
    )
    .then_some(task.updated_at)
    .flatten();
    ops.push(IngestOp::Task(TaskRow {
        id: ids::task_id(&ctx.session_id, &task.native_id),
        session_id: ctx.session_id.clone(),
        status: task.status,
        content: task.content,
        created_at: task.created_at,
        updated_at: task.updated_at,
        started_at,
        terminal_at,
    }));
}

fn task_update(
    ctx: &NormalizeCtx,
    state: &mut CopilotState,
    native_id: &str,
    status: TaskStatus,
    updated_at: Option<Ms>,
    ops: &mut Vec<IngestOp>,
) {
    let content = state
        .task_content
        .get(native_id)
        .cloned()
        .unwrap_or_else(|| native_id.to_owned());
    task_row(
        ctx,
        state,
        TaskFact {
            native_id: native_id.to_owned(),
            content,
            status,
            created_at: None,
            updated_at,
        },
        ops,
    );
}

fn subagent_started(
    ctx: &NormalizeCtx,
    state: &mut CopilotState,
    subagent: &SubagentStarted,
    ts: Option<Ms>,
    ops: &mut Vec<IngestOp>,
) {
    let content = if subagent.agent_description.trim().is_empty() {
        subagent.agent_display_name.clone()
    } else {
        subagent.agent_description.clone()
    };
    task_row(
        ctx,
        state,
        TaskFact {
            native_id: format!("subagent:{}", subagent.tool_call_id),
            content,
            status: TaskStatus::InProgress,
            created_at: ts,
            updated_at: ts,
        },
        ops,
    );
}

fn subagent_completed(
    ctx: &NormalizeCtx,
    state: &mut CopilotState,
    subagent: &SubagentCompleted,
    ts: Option<Ms>,
    ops: &mut Vec<IngestOp>,
) {
    task_update(
        ctx,
        state,
        &format!("subagent:{}", subagent.tool_call_id),
        if subagent.cancelled == Some(true) {
            TaskStatus::Cancelled
        } else {
            TaskStatus::Completed
        },
        ts,
        ops,
    );
}

fn subagent_failed(
    ctx: &NormalizeCtx,
    state: &mut CopilotState,
    subagent: &SubagentFailed,
    ts: Option<Ms>,
    ops: &mut Vec<IngestOp>,
) {
    let native_id = format!("subagent:{}", subagent.tool_call_id);
    let base = state
        .task_content
        .get(&native_id)
        .cloned()
        .unwrap_or_else(|| subagent.agent_display_name.clone());
    task_row(
        ctx,
        state,
        TaskFact {
            native_id,
            content: format!("{base}: {}", subagent.error),
            status: TaskStatus::Failed,
            created_at: None,
            updated_at: ts,
        },
        ops,
    );
}

fn assistant_message(
    ctx: &mut NormalizeCtx,
    state: &mut CopilotState,
    stream_id: &str,
    record: &EventLine,
    message: &AssistantMessage,
    ts: Option<Ms>,
    ops: &mut Vec<IngestOp>,
) {
    if let Some(model) = &message.model {
        state.model = Some(model.clone());
    }
    let effective_model = message.model.clone().or_else(|| state.model.clone());
    let mut rows = Vec::new();

    if let Some(reasoning) = message
        .reasoning_text
        .as_deref()
        .filter(|text| !text.is_empty())
    {
        let mut thinking = row(
            ctx,
            record,
            &format!("event:{}#thinking", record.id),
            MessageKind::Thinking,
            ts,
        );
        thinking.uuid = Some(message.message_id.clone());
        thinking.model = effective_model.clone();
        thinking.effort = state.effort.clone();
        thinking.content = Some(reasoning.to_owned());
        rows.push(thinking);
    }

    if !message.content.is_empty() {
        let mut agent = row(
            ctx,
            record,
            &format!("event:{}#content", record.id),
            MessageKind::Agent,
            ts,
        );
        agent.uuid = Some(message.message_id.clone());
        agent.subtype = message.phase.clone();
        agent.model = effective_model.clone();
        agent.effort = state.effort.clone();
        agent.content = Some(message.content.clone());
        rows.push(agent);
    }

    for request in message.tool_requests.iter().flatten() {
        if state.seen_tools.insert(request.tool_call_id.clone()) {
            rows.push(tool_request_row(
                ctx,
                state,
                stream_id,
                record,
                request,
                effective_model.clone(),
                ts,
                ops,
            ));
        }
    }

    if rows.is_empty() {
        let mut agent = row(
            ctx,
            record,
            &format!("event:{}#empty", record.id),
            MessageKind::Agent,
            ts,
        );
        agent.uuid = Some(message.message_id.clone());
        agent.subtype = message.phase.clone();
        agent.model = effective_model;
        agent.effort = state.effort.clone();
        rows.push(agent);
    }

    // `outputTokens` here includes reasoning the message never splits out;
    // the shutdown totals carry every bucket, so per-message output is not
    // usage of its own (it would count output twice).
    for message in rows {
        IngestOp::push_message(ops, message);
    }
}

// Tool projection is deliberately passed its complete native envelope rather
// than reading ambient process state.
#[allow(clippy::too_many_arguments)]
fn tool_request_row(
    ctx: &mut NormalizeCtx,
    state: &CopilotState,
    stream_id: &str,
    record: &EventLine,
    request: &ToolRequest,
    model: Option<String>,
    ts: Option<Ms>,
    ops: &mut Vec<IngestOp>,
) -> MessageRow {
    let mut tool_row = row(
        ctx,
        record,
        &tool_discriminator(&request.tool_call_id),
        MessageKind::ToolUse,
        ts,
    );
    tool_row.model = model;
    tool_row.effort = state.effort.clone();
    let name = request.mcp_tool_name.as_deref().unwrap_or(&request.name);
    tool_row.tool = Some(project_tool(
        ctx,
        state,
        stream_id,
        &request.tool_call_id,
        name,
        request.mcp_server_name.as_deref(),
        request.arguments.as_ref(),
        ts,
        ops,
    ));
    tool_row
}

#[allow(clippy::too_many_arguments)]
fn project_tool(
    ctx: &NormalizeCtx,
    state: &CopilotState,
    stream_id: &str,
    correlation_key: &str,
    name: &str,
    namespace: Option<&str>,
    input: Option<&Value>,
    at: Option<Ms>,
    ops: &mut Vec<IngestOp>,
) -> ToolCall {
    let input = input.unwrap_or(&Value::Null);
    let projected = crate::tool::project_request(
        ctx.agent_version.as_deref(),
        name,
        input,
        state.tool_cwd.as_deref(),
        correlation_key,
        Some(correlation_key),
        namespace,
    );
    match projected {
        Ok(tool) => tool,
        Err(error) => {
            ops.push(drift(
                ctx,
                stream_id,
                &projection_record_kind("request", namespace, name),
                error.drift_code(),
                &projection_shape_paths(input),
                at,
            ));
            unknown_interaction(
                correlation_key,
                CorrelationDerivation::NativeId,
                Some(correlation_key.to_owned()),
                namespace.map(str::to_owned),
                name,
                namespace.map(str::to_owned),
            )
        }
    }
}

fn tool_result(
    ctx: &NormalizeCtx,
    complete: &ToolExecutionComplete,
    ts: Option<Ms>,
    ops: &mut Vec<IngestOp>,
) {
    let diagnostic = (!complete.success)
        .then(|| {
            complete
                .error
                .as_ref()
                .and_then(display_text)
                .or_else(|| complete.result.as_ref().and_then(display_text))
        })
        .flatten();
    ops.push(IngestOp::ToolUpdate {
        session_id: ctx.session_id.clone(),
        correlation_key: complete.tool_call_id.clone(),
        evidence: ctx.evidence(0),
        patch: Box::new(terminal_update(
            Some(!complete.success),
            true,
            StatusBasis::NativeTerminalRecord,
            ts,
            diagnostic.as_deref(),
        )),
        at: ts,
    });
}

fn display_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Object(object) => object
            .get("content")
            .or_else(|| object.get("message"))
            .or_else(|| object.get("detailedContent"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        _ => None,
    }
}

fn remember_context_cwd(state: &mut CopilotState, context: Option<&Value>) {
    let Some(context) = context.and_then(Value::as_object) else {
        return;
    };
    let Some(cwd) = context
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|cwd| !cwd.is_empty())
    else {
        return;
    };
    remember_tool_cwd(state, cwd);
}

fn remember_tool_cwd(state: &mut CopilotState, cwd: &str) {
    if cwd.is_empty() {
        return;
    }
    state.tool_cwd = Some(cwd.to_owned());
}

fn row(
    ctx: &mut NormalizeCtx,
    record: &EventLine,
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
        uuid: Some(record.id.clone()),
        parent_uuid: record.parent_id.clone(),
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

fn tool_discriminator(tool_call_id: &str) -> String {
    format!("tool:{tool_call_id}")
}

fn record_kind(payload: &EventPayload) -> &'static str {
    match payload {
        EventPayload::Abort(_) => "abort",
        EventPayload::AssistantReasoning(_) => "assistant.reasoning",
        EventPayload::ExternalToolCompleted(_) => "external_tool.completed",
        EventPayload::ExternalToolRequested(_) => "external_tool.requested",
        EventPayload::SessionStart(_) => "session.start",
        EventPayload::SessionBinaryAsset(_) => "session.binary_asset",
        EventPayload::SessionPermissionsChanged(_) => "session.permissions_changed",
        EventPayload::SessionModelChange(_) => "session.model_change",
        EventPayload::SessionResume(_) => "session.resume",
        EventPayload::SessionShutdown(_) => "session.shutdown",
        EventPayload::SessionAutopilotObjectiveChanged(_) => "session.autopilot_objective_changed",
        EventPayload::SessionCompactionComplete(_) => "session.compaction_complete",
        EventPayload::SessionContextChanged(_) => "session.context_changed",
        EventPayload::SessionContextCleared(_) => "session.context_cleared",
        EventPayload::SessionHandoff(_) => "session.handoff",
        EventPayload::SessionInfo(_) => "session.info",
        EventPayload::SessionScheduleCancelled(_) => "session.schedule_cancelled",
        EventPayload::SessionScheduleCreated(_) => "session.schedule_created",
        EventPayload::SessionScheduleRearmed(_) => "session.schedule_rearmed",
        EventPayload::SessionTaskComplete(_) => "session.task_complete",
        EventPayload::SessionUsageCheckpoint(_) => "session.usage_checkpoint",
        EventPayload::SessionWarning(_) => "session.warning",
        EventPayload::SessionError(_) => "session.error",
        EventPayload::SessionWorkspaceFileChanged(_) => "session.workspace_file_changed",
        EventPayload::SkillInvoked(_) => "skill.invoked",
        EventPayload::SubagentCompleted(_) => "subagent.completed",
        EventPayload::SubagentFailed(_) => "subagent.failed",
        EventPayload::SubagentStarted(_) => "subagent.started",
        EventPayload::SystemMessage(_) => "system.message",
        EventPayload::SystemNotification(_) => "system.notification",
        EventPayload::UserMessage(_) => "user.message",
        EventPayload::AssistantTurnStart(_) => "assistant.turn_start",
        EventPayload::AssistantMessage(_) => "assistant.message",
        EventPayload::AssistantTurnEnd(_) => "assistant.turn_end",
        EventPayload::ToolExecutionStart(_) => "tool.execution_start",
        EventPayload::ToolExecutionComplete(_) => "tool.execution_complete",
        EventPayload::ToolUserRequested(_) => "tool.user_requested",
    }
}

fn hard_error(
    ctx: &NormalizeCtx,
    stream_id: &str,
    record_kind: &str,
    value: Option<&Value>,
) -> LineOutcome {
    LineOutcome {
        ops: vec![drift(
            ctx,
            stream_id,
            record_kind,
            PARSE_FAILED,
            &value.map(top_level_keys).unwrap_or_default(),
            value.and_then(record_ts),
        )],
        status: LineStatus::HardError {
            code: PARSE_FAILED.to_owned(),
        },
    }
}

pub(crate) fn drift(
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

fn top_level_keys(value: &Value) -> Vec<String> {
    value
        .as_object()
        .map(|object| object.keys().cloned().collect())
        .unwrap_or_else(|| vec!["<non-object>".to_owned()])
}

fn record_ts(value: &Value) -> Option<Ms> {
    value["timestamp"].as_str().and_then(rfc3339_ms)
}

#[cfg(test)]
mod tests {
    use super::{CopilotState, display_text, ingest_line};
    use rosetta_adapter_support::adapter::{LineStatus, NormalizeCtx};
    use rosetta_core::rows::TaskStatus;
    use rosetta_core::{IngestOp, TimelineBody};
    use serde_json::json;

    fn context() -> NormalizeCtx {
        NormalizeCtx::new("runtime", "session")
    }

    #[test]
    fn result_display_text_uses_native_fields() {
        assert_eq!(
            display_text(&json!({"content": "visible", "detailedContent": "long"})),
            Some("visible".to_owned())
        );
        assert_eq!(
            display_text(&json!({"message": "denied", "code": "denied"})),
            Some("denied".to_owned())
        );
    }

    #[test]
    fn undeclared_type_is_soft_drift() {
        let mut ctx = context();
        let mut state = CopilotState::default();
        let outcome = ingest_line(
            &mut ctx,
            &mut state,
            "events",
            br#"{"id":"1","timestamp":"2026-08-25T00:00:00Z","parentId":null,"type":"future.event","data":{}}"#,
        );
        assert_eq!(outcome.status, LineStatus::UnknownType);
        assert!(matches!(outcome.ops.as_slice(), [IngestOp::Drift(_)]));
    }

    #[test]
    fn declared_but_unprojected_type_is_a_parsed_no_op() {
        let mut ctx = context();
        let mut state = CopilotState::default();
        let outcome = ingest_line(
            &mut ctx,
            &mut state,
            "events",
            br#"{"id":"1","timestamp":"2026-08-25T00:00:00Z","parentId":null,"type":"session.plan_changed","data":{"operation":"created"}}"#,
        );
        assert_eq!(outcome.status, LineStatus::Parsed);
        assert!(outcome.ops.is_empty());
    }

    #[test]
    fn declared_fallback_reports_only_schema_additions() {
        let mut ctx = context();
        let mut state = CopilotState::default();
        let outcome = ingest_line(
            &mut ctx,
            &mut state,
            "events",
            br#"{"id":"1","timestamp":"2026-08-25T00:00:00Z","parentId":null,"type":"session.plan_changed","data":{"operation":"created","newField":true}}"#,
        );
        assert_eq!(outcome.status, LineStatus::Parsed);
        assert!(matches!(outcome.ops.as_slice(), [IngestOp::Drift(_)]));
    }

    #[test]
    fn reasoning_and_subagent_lifecycle_project_to_canonical_rows() {
        let mut ctx = context();
        let mut state = CopilotState::default();
        let reasoning = ingest_line(
            &mut ctx,
            &mut state,
            "events",
            br#"{"id":"r1","timestamp":"2026-08-25T00:00:00Z","parentId":null,"type":"assistant.reasoning","data":{"content":"Check the tree.","reasoningId":"reasoning-1"}}"#,
        );
        match reasoning.ops.as_slice() {
            [IngestOp::Message(row)] => {
                let TimelineBody::Thinking { text } = &row.body else {
                    panic!("expected thinking body, got {:?}", row.body);
                };
                assert_eq!(text.as_preview(), "Check the tree.");
            }
            other => panic!("expected a thinking row, got {other:?}"),
        }

        let started = ingest_line(
            &mut ctx,
            &mut state,
            "events",
            br#"{"id":"s1","timestamp":"2026-08-25T00:00:01Z","parentId":"r1","type":"subagent.started","data":{"agentDescription":"Audit tests","agentDisplayName":"Worker","agentName":"worker","toolCallId":"call-1"}}"#,
        );
        match started.ops.as_slice() {
            [IngestOp::Task(task)] => {
                assert_eq!(task.status, TaskStatus::InProgress);
                assert_eq!(task.content, "Audit tests");
            }
            other => panic!("expected an in-progress task, got {other:?}"),
        }

        let failed = ingest_line(
            &mut ctx,
            &mut state,
            "events",
            br#"{"id":"s2","timestamp":"2026-08-25T00:00:02Z","parentId":"s1","type":"subagent.failed","data":{"agentDisplayName":"Worker","agentName":"worker","error":"timed out","toolCallId":"call-1"}}"#,
        );
        match failed.ops.as_slice() {
            [IngestOp::Task(task)] => {
                assert_eq!(task.status, TaskStatus::Failed);
                assert!(task.content.contains("Audit tests"));
                assert!(task.content.contains("timed out"));
            }
            other => panic!("expected a failed task, got {other:?}"),
        }
    }

    #[test]
    fn known_type_shape_failure_is_hard_drift() {
        let mut ctx = context();
        let mut state = CopilotState::default();
        let outcome = ingest_line(
            &mut ctx,
            &mut state,
            "events",
            br#"{"id":"1","timestamp":"2026-08-25T00:00:00Z","parentId":null,"type":"user.message","data":{}}"#,
        );
        assert_eq!(
            outcome.status,
            LineStatus::HardError {
                code: "parse_failed".to_owned()
            }
        );
        assert!(matches!(outcome.ops.as_slice(), [IngestOp::Drift(_)]));
    }
}
