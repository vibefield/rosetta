//! Copilot persisted events to canonical ingest operations.
//!
//! Assistant messages can contain reasoning, visible text, and one or more
//! tool requests, so they split into flat rows. Tool completions merge into
//! the request row by native `toolCallId`. Session shutdown metrics are real
//! running counters and therefore pass through `NormalizeCtx::cumulative`.

use std::collections::{BTreeMap, BTreeSet};

use rosetta_adapter_support::Drift;
use rosetta_adapter_support::adapter::{LineOutcome, LineStatus, NormalizeCtx};
use rosetta_core::rows::{
    DriftFact, EditKind, FileEditRow, MessageKind, MessageRow, Ms, SessionRole, SessionRow,
    StreamState, TaskRow, TaskStatus, TokenSnapshot, ToolCall, WorkspaceObsRow,
};
use rosetta_core::{IngestOp, ids};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::raw::events::{
    AssistantMessage, EventLine, EventPayload, SessionNotice, SessionShutdown, SubagentCompleted,
    SubagentFailed, SubagentStarted, ToolExecutionComplete, ToolRequest,
};
use crate::raw::{ParsedLine, parse_line};
use crate::spec;
use crate::time::rfc3339_ms;

const UNKNOWN_TYPE: &str = "unknown_type";
const UNKNOWN_FIELDS: &str = "unknown_fields";
const PARSE_FAILED: &str = "parse_failed";
const NO_TYPE: &str = "<none>";
const INVALID_JSON: &str = "<invalid-json>";

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct CopilotState {
    workspace: Option<WorkspaceState>,
    model: Option<String>,
    effort: Option<String>,
    /// Tool requests and execution starts describe the same native call. This
    /// set ensures they produce one timeline row across checkpoint resumes.
    seen_tools: BTreeSet<String>,
    #[serde(default)]
    task_content: BTreeMap<String, String>,
    /// Most recent assistant-authored row, used as the replacement key for a
    /// later cumulative shutdown usage snapshot.
    response: Option<String>,
    response_usage: TokenSnapshot,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
struct WorkspaceState {
    cwd: String,
    branch: Option<String>,
    repository: Option<String>,
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

    project(ctx, state, &record, ts, &mut ops);
    LineOutcome::parsed(ops)
}

fn project(
    ctx: &mut NormalizeCtx,
    state: &mut CopilotState,
    record: &EventLine,
    ts: Option<Ms>,
    ops: &mut Vec<IngestOp>,
) {
    match &record.payload {
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
            state.response = Some(thinking.timeline_id.clone());
            state.response_usage = TokenSnapshot::default();
            ops.push(IngestOp::Message(Box::new(thinking)));
        }
        EventPayload::ExternalToolRequested(request) => {
            if state.seen_tools.insert(request.tool_call_id.clone()) {
                tool_event(
                    ctx,
                    state,
                    record,
                    ToolEvent {
                        call_id: &request.tool_call_id,
                        name: &request.tool_name,
                        input: request.arguments.clone(),
                        ts,
                    },
                    ops,
                );
            }
            if let Some(cwd) = request.working_directory.as_deref() {
                observe_workspace_parts(ctx, state, cwd, None, None, ts, ops);
            }
        }
        EventPayload::ExternalToolCompleted(_) => {}
        EventPayload::SessionStart(start) => {
            ctx.agent_version = Some(start.copilot_version.clone());
            state.model = start.selected_model.clone();
            state.effort = start.reasoning_effort.clone();

            let parent_session_id = start
                .detached_from_spawning_parent_session_id
                .as_deref()
                .map(|native| ids::session_id(&ctx.runtime_id, native));
            let role = parent_session_id
                .as_ref()
                .map_or(SessionRole::Main, |_| SessionRole::Subagent);
            let started_at = rfc3339_ms(&start.start_time).or(ts).unwrap_or_default();
            ops.push(IngestOp::SessionMeta(SessionRow {
                id: ctx.session_id.clone(),
                native_session_id: start.session_id.clone(),
                runtime_id: ctx.runtime_id.clone(),
                project_id: None,
                parent_session_id: parent_session_id.clone(),
                root_session_id: parent_session_id.unwrap_or_else(|| ctx.session_id.clone()),
                role,
                title: None,
                summary: None,
                started_at,
                updated_at: ts.unwrap_or(started_at),
                stream_state: StreamState::Streaming,
            }));
            observe_workspace(
                ctx,
                state,
                start.context.as_ref(),
                ts.or(Some(started_at)),
                ops,
            );
        }
        EventPayload::SessionResume(resume) => {
            if let Some(model) = &resume.selected_model {
                state.model = Some(model.clone());
            }
            if let Some(effort) = &resume.reasoning_effort {
                state.effort = Some(effort.clone());
            }
            observe_workspace(ctx, state, resume.context.as_ref(), ts, ops);
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
            ops.push(IngestOp::Message(Box::new(row)));
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
            ops.push(IngestOp::Message(Box::new(row)));
        }
        EventPayload::AssistantMessage(message) => {
            assistant_message(ctx, state, record, message, ts, ops);
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
                tool_row.tool = Some(ToolCall {
                    tool_id: Some(start.tool_call_id.clone()),
                    name: start.tool_name.clone(),
                    input: start.arguments.clone().unwrap_or(Value::Null),
                    result: None,
                    payload: None,
                    is_error: None,
                });
                state.response = Some(tool_row.timeline_id.clone());
                state.response_usage = TokenSnapshot::default();
                ops.push(IngestOp::Message(Box::new(tool_row)));
            }
        }
        EventPayload::ToolExecutionComplete(complete) => {
            tool_result(ctx, complete, ts, ops);
        }
        EventPayload::SessionShutdown(shutdown) => {
            if let Some(model) = &shutdown.current_model {
                state.model = Some(model.clone());
            }
            shutdown_usage(ctx, state, shutdown, ts, ops);
        }
        EventPayload::SessionContextChanged(context) => observe_workspace_parts(
            ctx,
            state,
            &context.cwd,
            context.branch.as_deref(),
            context
                .repository
                .as_deref()
                .or(context.git_root.as_deref()),
            ts,
            ops,
        ),
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
                    record,
                    ToolEvent {
                        call_id: &request.tool_call_id,
                        name: &request.tool_name,
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
    ops.push(IngestOp::Message(Box::new(message)));
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
    ops.push(IngestOp::Message(Box::new(message)));
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
    input: Option<Value>,
    ts: Option<Ms>,
}

fn tool_event(
    ctx: &mut NormalizeCtx,
    state: &mut CopilotState,
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
    tool.tool = Some(ToolCall {
        tool_id: Some(event.call_id.to_owned()),
        name: event.name.to_owned(),
        input: event.input.unwrap_or(Value::Null),
        result: None,
        payload: None,
        is_error: None,
    });
    state.response = Some(tool.timeline_id.clone());
    state.response_usage = TokenSnapshot::default();
    ops.push(IngestOp::Message(Box::new(tool)));
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
    ops.push(IngestOp::Task(TaskRow {
        id: ids::task_id(&ctx.session_id, &task.native_id),
        session_id: ctx.session_id.clone(),
        status: task.status,
        content: task.content,
        created_at: task.created_at,
        updated_at: task.updated_at,
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
                record,
                request,
                effective_model.clone(),
                ts,
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

    let target = rows
        .iter()
        .position(|candidate| candidate.kind == MessageKind::Agent)
        .or_else(|| {
            rows.iter()
                .position(|candidate| candidate.kind == MessageKind::ToolUse)
        })
        .unwrap_or(0);
    let usage = TokenSnapshot {
        output: message
            .output_tokens
            .and_then(|value| i64::try_from(value).ok()),
        ..TokenSnapshot::default()
    };
    if !usage.is_empty() {
        rows[target].usage = Some(usage);
    }
    state.response = Some(rows[target].timeline_id.clone());
    state.response_usage = usage;

    ops.extend(
        rows.into_iter()
            .map(|message| IngestOp::Message(Box::new(message))),
    );
}

fn tool_request_row(
    ctx: &mut NormalizeCtx,
    state: &CopilotState,
    record: &EventLine,
    request: &ToolRequest,
    model: Option<String>,
    ts: Option<Ms>,
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
    tool_row.tool = Some(ToolCall {
        tool_id: Some(request.tool_call_id.clone()),
        name: request.name.clone(),
        input: request.arguments.clone().unwrap_or(Value::Null),
        result: None,
        payload: None,
        is_error: None,
    });
    tool_row
}

fn tool_result(
    ctx: &NormalizeCtx,
    complete: &ToolExecutionComplete,
    ts: Option<Ms>,
    ops: &mut Vec<IngestOp>,
) {
    let payload = complete.result.clone().or_else(|| complete.error.clone());
    let result = complete
        .result
        .as_ref()
        .and_then(display_text)
        .or_else(|| complete.error.as_ref().and_then(display_text));
    ops.push(IngestOp::ToolResult {
        session_id: ctx.session_id.clone(),
        tool_id: complete.tool_call_id.clone(),
        result,
        payload,
        is_error: Some(!complete.success),
        at: ts,
    });
}

/// `modelMetrics.*.usage` is cumulative for the session and is re-reported on
/// resume shutdown. Its delta supplies the components absent from the
/// per-assistant-message `outputTokens` observation.
fn shutdown_usage(
    ctx: &mut NormalizeCtx,
    state: &mut CopilotState,
    shutdown: &SessionShutdown,
    ts: Option<Ms>,
    ops: &mut Vec<IngestOp>,
) {
    let Some((reading, metrics_model)) = model_metrics(&shutdown.model_metrics) else {
        return;
    };
    let delta = ctx.cumulative.absorb(&reading);
    if all_reported_components_zero(&delta) {
        return;
    }
    let Some(response) = state.response.clone() else {
        return;
    };
    let tokens = TokenSnapshot {
        input: delta.input,
        // Per-message output is more precise when the session had several
        // model responses before one shutdown snapshot.
        output: state.response_usage.output.or(delta.output),
        cache_read: delta.cache_read,
        cache_write: delta.cache_write,
        thinking: delta.thinking,
    };
    let at = ts.unwrap_or_default();
    ops.push(IngestOp::Usage {
        session_id: ctx.session_id.clone(),
        response,
        ts: at,
        model: metrics_model.or_else(|| state.model.clone()),
        tokens,
    });
}

fn model_metrics(metrics: &Value) -> Option<(TokenSnapshot, Option<String>)> {
    let object = metrics.as_object()?;
    let mut total = TokenSnapshot::default();
    let mut models = Vec::new();
    for (model, metric) in object {
        let Some(usage) = metric.get("usage").and_then(Value::as_object) else {
            continue;
        };
        models.push(model.clone());
        add(&mut total.input, number(usage.get("inputTokens")));
        add(&mut total.output, number(usage.get("outputTokens")));
        add(&mut total.cache_read, number(usage.get("cacheReadTokens")));
        add(
            &mut total.cache_write,
            number(usage.get("cacheWriteTokens")),
        );
        add(&mut total.thinking, number(usage.get("reasoningTokens")));
    }
    if total.is_empty() {
        return None;
    }
    let model = (models.len() == 1).then(|| models.remove(0));
    Some((total, model))
}

fn number(value: Option<&Value>) -> Option<i64> {
    value.and_then(Value::as_i64)
}

fn add(total: &mut Option<i64>, value: Option<i64>) {
    if let Some(value) = value {
        *total = Some(total.unwrap_or(0) + value);
    }
}

fn all_reported_components_zero(snapshot: &TokenSnapshot) -> bool {
    let values = [
        snapshot.input,
        snapshot.output,
        snapshot.cache_read,
        snapshot.cache_write,
        snapshot.thinking,
    ];
    values.iter().any(Option::is_some) && values.into_iter().flatten().all(|value| value == 0)
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

fn observe_workspace(
    ctx: &NormalizeCtx,
    state: &mut CopilotState,
    context: Option<&Value>,
    at: Option<Ms>,
    ops: &mut Vec<IngestOp>,
) {
    let (Some(context), Some(at)) = (context.and_then(Value::as_object), at) else {
        return;
    };
    let Some(cwd) = context
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|cwd| !cwd.is_empty())
    else {
        return;
    };
    observe_workspace_parts(
        ctx,
        state,
        cwd,
        context.get("branch").and_then(Value::as_str),
        context.get("repository").and_then(Value::as_str),
        Some(at),
        ops,
    );
}

fn observe_workspace_parts(
    ctx: &NormalizeCtx,
    state: &mut CopilotState,
    cwd: &str,
    branch: Option<&str>,
    repository: Option<&str>,
    at: Option<Ms>,
    ops: &mut Vec<IngestOp>,
) {
    let Some(at) = at else { return };
    if cwd.is_empty() {
        return;
    }
    let observation = WorkspaceState {
        cwd: cwd.to_owned(),
        branch: branch.filter(|value| !value.is_empty()).map(str::to_owned),
        repository: repository
            .filter(|value| !value.is_empty())
            .map(str::to_owned),
    };
    if state.workspace.as_ref() == Some(&observation) {
        return;
    }
    ops.push(IngestOp::WorkspaceObs(WorkspaceObsRow {
        session_id: ctx.session_id.clone(),
        at,
        cwd: observation.cwd.clone(),
        git_branch: observation.branch.clone(),
        git_repo: observation.repository.clone(),
    }));
    state.workspace = Some(observation);
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
    use super::{
        CopilotState, all_reported_components_zero, display_text, ingest_line, model_metrics,
    };
    use rosetta_adapter_support::adapter::{LineStatus, NormalizeCtx};
    use rosetta_core::IngestOp;
    use rosetta_core::rows::{MessageKind, TaskStatus, TokenSnapshot};
    use serde_json::json;

    fn context() -> NormalizeCtx {
        NormalizeCtx::new("runtime", "session")
    }

    #[test]
    fn model_metrics_preserve_reported_zeroes() {
        let metrics = json!({
            "mock-model": {
                "usage": {
                    "inputTokens": 128,
                    "outputTokens": 32,
                    "cacheReadTokens": 0,
                    "cacheWriteTokens": 0,
                    "reasoningTokens": 0
                }
            }
        });
        let (tokens, model) = model_metrics(&metrics).expect("usage metrics");
        assert_eq!(model.as_deref(), Some("mock-model"));
        assert_eq!(tokens.input, Some(128));
        assert_eq!(tokens.cache_read, Some(0));
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
    fn repeated_cumulative_snapshot_is_a_no_op() {
        assert!(all_reported_components_zero(&TokenSnapshot {
            input: Some(0),
            output: Some(0),
            cache_read: Some(0),
            cache_write: Some(0),
            thinking: Some(0),
        }));
        assert!(!all_reported_components_zero(&TokenSnapshot::default()));
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
                assert_eq!(row.kind, MessageKind::Thinking);
                assert_eq!(row.content.as_deref(), Some("Check the tree."));
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
