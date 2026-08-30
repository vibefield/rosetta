//! Rollout records → canonical ops.
//!
//! Every projection here cites the ground-truth path it rests on
//! (`groundtruth/codex/groundtruth.json`, stream `rollout`). The rule for the
//! two open payload vocabularies is in [`crate::spec::declared`]: declared and
//! unprojected is silence, undeclared is drift.
//!
//! Pure by construction — no IO, no clock, no randomness. Ids are deterministic
//! blake3 over the session and the envelope `ordinal` (or the source byte
//! offset for legacy writers), so replaying a file produces byte-identical ops.

use rosetta_adapter_support::Drift;
use rosetta_adapter_support::adapter::{LineOutcome, LineStatus, NormalizeCtx};
use rosetta_adapter_support::tool::{
    projection_record_kind, projection_shape_paths, terminal_update, unknown_interaction,
};
use rosetta_core::rows::{
    DriftFact, EditKind, FileEditRow, MessageKind, MessageRow, Ms, SessionRole, SessionRow,
    StreamState, TokenSnapshot, WorkspaceObsRow,
};
use rosetta_core::timeline::{CorrelationDerivation, StatusBasis};
use rosetta_core::{IngestOp, fold, ids};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::raw::rollout::{
    Compacted, ContentBlock, EventMsg, InterAgentCommunication, RealtimeItem, ResponseItem,
    RolloutLine, RolloutPayload, SessionMeta, TurnContext,
};
use crate::raw::{ParsedLine, parse_line};
use crate::spec;
use crate::time::rfc3339_ms;

/// The only tool whose arguments carry a patch body. Everything else — the
/// canonical declares `exec_command` and nothing more — projects as a plain
/// tool call.
const APPLY_PATCH: &str = "apply_patch";

/// Per-file adapter state ([`NormalizeCtx::state`]).
///
/// `model`/`effort` are carried forward because codex stamps them on
/// `turn_context`, not on the rows they describe; `response` and
/// `response_total` are the machinery for the cumulative token counters.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct CodexState {
    /// Last observed working directory — a change emits a `workspace_obs`.
    cwd: Option<String>,
    model: Option<String>,
    effort: Option<String>,
    /// timeline_id of the most recent assistant-authored row.
    response: Option<String>,
    /// Running per-response total, in deltas absorbed from the session counters.
    response_total: TokenSnapshot,
}

/// Ingest one rollout line. `stream_id` names the layout stream for drift
/// bookkeeping only — row identity never depends on it.
pub fn ingest_line(
    ctx: &mut NormalizeCtx,
    state: &mut CodexState,
    stream_id: &str,
    line: &[u8],
) -> LineOutcome {
    let Ok(text) = std::str::from_utf8(line) else {
        return hard(ctx, stream_id, "<binary>", "invalid_utf8", &[], None);
    };
    // Rollout files are append-only; a torn write shows up as a blank line.
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
            let kind = rtype.unwrap_or_else(|| "<untyped>".to_owned());
            let at = stamp(&value);
            let op = drift(ctx, stream_id, &kind, "unknown_type", &keys(&value), at);
            return LineOutcome {
                ops: vec![op],
                status: LineStatus::UnknownType,
            };
        }
    };

    let ts = rfc3339_ms(&record.timestamp);
    let mut ops = Vec::new();
    // Permissive parsing means novel fields land in the flatten maps instead of
    // failing the line — that is the whole point, but it must still be visible.
    let paths = record.drift_paths();
    if !paths.is_empty() {
        ops.push(drift(
            ctx,
            stream_id,
            &record_kind(&record.payload),
            "unknown_fields",
            &paths,
            ts,
        ));
    }

    let status = project(ctx, state, stream_id, &record, ts, &mut ops);
    LineOutcome { ops, status }
}

/// Route one parsed record to its projection.
fn project(
    ctx: &mut NormalizeCtx,
    state: &mut CodexState,
    stream_id: &str,
    record: &RolloutLine,
    ts: Option<Ms>,
    ops: &mut Vec<IngestOp>,
) -> LineStatus {
    match &record.payload {
        RolloutPayload::SessionMeta(meta) => {
            session_meta(ctx, state, meta, ts, ops);
            LineStatus::Parsed
        }
        RolloutPayload::TurnContext(turn) => {
            turn_context(ctx, state, turn, ts, ops);
            LineStatus::Parsed
        }
        RolloutPayload::ResponseItem(item) => {
            let Some(discriminator) = record_discriminator(record, ctx) else {
                return hard_into(
                    ctx,
                    stream_id,
                    "response_item",
                    "missing_record_discriminator",
                    ts,
                    ops,
                );
            };
            response_item(ctx, state, stream_id, item, &discriminator, ts, ops)
        }
        RolloutPayload::EventMsg(event) => event_msg(ctx, state, stream_id, event, ts, ops),
        RolloutPayload::Compacted(item) => {
            let Some(discriminator) = record_discriminator(record, ctx) else {
                return hard_into(
                    ctx,
                    stream_id,
                    "compacted",
                    "missing_record_discriminator",
                    ts,
                    ops,
                );
            };
            compacted(ctx, item, &discriminator, ts, ops);
            LineStatus::Parsed
        }
        RolloutPayload::InterAgentCommunication(item) => {
            let Some(discriminator) = record_discriminator(record, ctx) else {
                return hard_into(
                    ctx,
                    stream_id,
                    "inter_agent_communication",
                    "missing_record_discriminator",
                    ts,
                    ops,
                );
            };
            inter_agent_communication(ctx, item, &discriminator, ts, ops);
            LineStatus::Parsed
        }
        RolloutPayload::RealtimeItem(item) => {
            let Some(discriminator) = record_discriminator(record, ctx) else {
                return hard_into(
                    ctx,
                    stream_id,
                    "realtime_item",
                    "missing_record_discriminator",
                    ts,
                    ops,
                );
            };
            realtime_item(ctx, item, &discriminator, ts, ops);
            LineStatus::Parsed
        }
        // Declared and unprojected: the world-state snapshot is a full copy of
        // the prompt context, and its cwd is already carried by turn_context.
        RolloutPayload::WorldState(_)
        | RolloutPayload::InterAgentCommunicationMetadata(_)
        | RolloutPayload::SecurityRiskScore(_) => LineStatus::Parsed,
    }
}

/// Session header: identity, provenance, and the opening working directory.
///
/// The session row is emitted HERE rather than from `skim` because deriving its
/// id needs a runtime id, which the meta phase does not hand the adapter (see
/// `CodexAdapter::skim`). `IngestOp::SessionMeta` is an upsert, so a pipeline
/// that later learns more about the session refines this row instead of
/// fighting it.
fn session_meta(
    ctx: &mut NormalizeCtx,
    state: &mut CodexState,
    meta: &SessionMeta,
    ts: Option<Ms>,
    ops: &mut Vec<IngestOp>,
) {
    ctx.agent_version = Some(meta.cli_version.clone());
    // payload.timestamp is when the thread opened; the envelope stamp is when
    // the record was written. Prefer the former, fall back to the latter.
    let started = rfc3339_ms(&meta.timestamp).or(ts).unwrap_or_default();

    let parent_session_id = meta
        .parent_thread_id
        .as_deref()
        .map(|native| ids::session_id(&ctx.runtime_id, native));
    let (root_session_id, role) = match &parent_session_id {
        Some(parent) => (parent.clone(), SessionRole::Subagent),
        None => (ctx.session_id.clone(), SessionRole::Main),
    };

    ops.push(IngestOp::SessionMeta(SessionRow {
        id: ctx.session_id.clone(),
        native_session_id: meta.id.clone(),
        runtime_id: ctx.runtime_id.clone(),
        // Left to the pipeline: `workspace_obs` is the documented input to
        // project assignment, so the adapter reports cwd and claims nothing.
        project_id: None,
        parent_session_id,
        root_session_id,
        role,
        title: None,
        summary: None,
        started_at: started,
        updated_at: ts.unwrap_or(started),
        stream_state: StreamState::Streaming,
    }));

    state.cwd = Some(meta.cwd.clone());
    ops.push(IngestOp::WorkspaceObs(WorkspaceObsRow {
        session_id: ctx.session_id.clone(),
        at: started,
        cwd: meta.cwd.clone(),
        git_branch: None,
        git_repo: None,
    }));
}

/// Per-turn execution context: the model/effort stamp for the rows that follow,
/// and the second place a working directory can change.
fn turn_context(
    ctx: &NormalizeCtx,
    state: &mut CodexState,
    turn: &TurnContext,
    ts: Option<Ms>,
    ops: &mut Vec<IngestOp>,
) {
    let settings = turn
        .collaboration_mode
        .as_ref()
        .and_then(|mode| mode.get("settings"));
    // The collaboration mode's setting is the effective one; payload.model is
    // the fallback the canonical marks required.
    if let Some(model) = settings
        .and_then(|settings| settings.get("model"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| turn.model.clone())
    {
        state.model = Some(model);
    }
    if let Some(effort) = settings
        .and_then(|settings| settings.get("reasoning_effort"))
        .and_then(Value::as_str)
        .or_else(|| turn.effort.as_ref().and_then(Value::as_str))
    {
        state.effort = Some(effort.to_owned());
    }

    if let Some(cwd) = turn.cwd.as_deref()
        && state.cwd.as_deref() != Some(cwd)
    {
        state.cwd = Some(cwd.to_owned());
        ops.push(IngestOp::WorkspaceObs(WorkspaceObsRow {
            session_id: ctx.session_id.clone(),
            at: ts.unwrap_or_default(),
            cwd: cwd.to_owned(),
            git_branch: None,
            git_repo: None,
        }));
    }
}

/// Conversation items — the timeline rows.
fn response_item(
    ctx: &mut NormalizeCtx,
    state: &mut CodexState,
    stream_id: &str,
    item: &ResponseItem,
    discriminator: &str,
    ts: Option<Ms>,
    ops: &mut Vec<IngestOp>,
) -> LineStatus {
    match item.item_type.as_str() {
        "message" => {
            let (kind, subtype) = match item.role.as_deref() {
                Some("user") => (MessageKind::User, None),
                Some("assistant") => (MessageKind::Agent, None),
                // `developer` carries the injected skills/instructions preamble:
                // real content, but not something a user said.
                Some(other) => (MessageKind::System, Some(other.to_owned())),
                None => (MessageKind::System, None),
            };
            let mut row = row(ctx, discriminator, kind, ts);
            row.subtype = item.phase.clone().or(subtype);
            row.uuid = item.id.clone();
            row.content = block_text(item.content.as_deref());
            if kind == MessageKind::Agent {
                stamp_model(&mut row, state);
                begin_response(state, &row.timeline_id);
            }
            IngestOp::push_message(ops, row);
            LineStatus::Parsed
        }
        "reasoning" => {
            let mut row = row(ctx, discriminator, MessageKind::Thinking, ts);
            row.uuid = item.id.clone();
            row.content =
                block_text(item.summary.as_deref()).or_else(|| block_text(item.content.as_deref()));
            stamp_model(&mut row, state);
            begin_response(state, &row.timeline_id);
            IngestOp::push_message(ops, row);
            LineStatus::Parsed
        }
        "agent_message" => {
            let mut row = row(ctx, discriminator, MessageKind::Agent, ts);
            row.uuid = item.id.clone();
            row.subtype = Some("agent_message".to_owned());
            row.content = block_text(item.content.as_deref());
            stamp_model(&mut row, state);
            begin_response(state, &row.timeline_id);
            IngestOp::push_message(ops, row);
            LineStatus::Parsed
        }
        "function_call" | "custom_tool_call" => {
            let custom = item.item_type == "custom_tool_call";
            let record_kind = if custom {
                "response_item/custom_tool_call"
            } else {
                "response_item/function_call"
            };
            let Some(name) = item.name.clone() else {
                // The writer marks `name` required; a call without one cannot
                // become a tool row at all.
                return hard_into(ctx, stream_id, record_kind, "missing_name", ts, ops);
            };
            let input = if custom {
                let Some(input) = item.input.as_ref() else {
                    return hard_into(ctx, stream_id, record_kind, "missing_input", ts, ops);
                };
                Value::String(input.clone())
            } else {
                tool_input(item.arguments.as_ref())
            };
            let Some(tool_id) = item.call_id.clone() else {
                return hard_into(ctx, stream_id, record_kind, "missing_call_id", ts, ops);
            };
            let mut row = row(ctx, discriminator, MessageKind::ToolUse, ts);
            row.uuid = item.id.clone();
            stamp_model(&mut row, state);
            let projected = crate::tool::project_request(
                ctx.agent_version.as_deref(),
                &name,
                &input,
                state.cwd.as_deref(),
                &tool_id,
                Some(&tool_id),
                item.namespace.as_deref(),
            );
            row.tool = Some(match projected {
                Ok(tool) => tool,
                Err(error) => {
                    ops.push(drift(
                        ctx,
                        stream_id,
                        &projection_record_kind("request", item.namespace.as_deref(), &name),
                        error.drift_code(),
                        &projection_shape_paths(&input),
                        ts,
                    ));
                    unknown_interaction(
                        &tool_id,
                        CorrelationDerivation::NativeId,
                        Some(tool_id.clone()),
                        item.namespace.clone(),
                        &name,
                        item.namespace.clone(),
                    )
                }
            });
            // A tool call IS a response: the model produced it, and the
            // token_count that follows belongs to it.
            begin_response(state, &row.timeline_id);
            let timeline_id = row.timeline_id.clone();
            IngestOp::push_message(ops, row);

            for edit in patch_body(&name, &input)
                .map(patch_edits)
                .unwrap_or_default()
            {
                ops.push(IngestOp::FileEdit(FileEditRow {
                    session_id: ctx.session_id.clone(),
                    message_timeline_id: Some(timeline_id.clone()),
                    path: edit.path,
                    kind: edit.kind,
                    snapshot_ref: None,
                    lines_added: edit.added,
                    lines_removed: edit.removed,
                    at: ts,
                }));
            }
            LineStatus::Parsed
        }
        "function_call_output" | "custom_tool_call_output" => {
            let record_kind = format!("response_item/{}", item.item_type);
            let Some(tool_id) = item.call_id.clone() else {
                // Without the correlation id the result can never reach its call.
                return hard_into(ctx, stream_id, &record_kind, "missing_call_id", ts, ops);
            };
            let is_error = status_error(item.status.as_deref());
            let output = is_error
                .is_some_and(|value| value)
                .then(|| tool_output_text(item.output.as_ref()))
                .flatten();
            ops.push(IngestOp::ToolUpdate {
                session_id: ctx.session_id.clone(),
                correlation_key: tool_id,
                evidence: ctx.evidence(0),
                // The record declares no status field — success stays UNKNOWN
                // rather than being sniffed out of its text.
                patch: Box::new(terminal_update(
                    is_error,
                    false,
                    StatusBasis::NativeTerminalRecord,
                    ts,
                    output.as_deref(),
                )),
                at: ts,
            });
            LineStatus::Parsed
        }
        "tool_search_call" => {
            let mut row = row(ctx, discriminator, MessageKind::ToolUse, ts);
            row.uuid = item.id.clone();
            stamp_model(&mut row, state);
            let native_id = item.call_id.as_deref().or(item.id.as_deref());
            let correlation_key = native_id.unwrap_or(discriminator);
            let mut input = tool_input(item.arguments.as_ref());
            if let Some(execution) = item.execution.as_ref()
                && let Some(map) = input.as_object_mut()
            {
                map.entry("execution")
                    .or_insert_with(|| Value::String(execution.clone()));
            }
            let projected = crate::tool::project_request(
                ctx.agent_version.as_deref(),
                "tool_search_call",
                &input,
                state.cwd.as_deref(),
                correlation_key,
                native_id,
                item.namespace.as_deref(),
            );
            row.tool = Some(match projected {
                Ok(tool) => tool,
                Err(error) => {
                    ops.push(drift(
                        ctx,
                        stream_id,
                        &projection_record_kind(
                            "request",
                            item.namespace.as_deref(),
                            "tool_search_call",
                        ),
                        error.drift_code(),
                        &projection_shape_paths(&input),
                        ts,
                    ));
                    unknown_interaction(
                        correlation_key,
                        if native_id.is_some() {
                            CorrelationDerivation::NativeId
                        } else {
                            CorrelationDerivation::Synthetic
                        },
                        native_id.map(str::to_owned),
                        item.namespace.clone(),
                        "tool_search_call",
                        item.namespace.clone(),
                    )
                }
            });
            begin_response(state, &row.timeline_id);
            IngestOp::push_message(ops, row);
            LineStatus::Parsed
        }
        "tool_search_output" => {
            let Some(tool_id) = item.call_id.clone() else {
                // The current writer permits a missing call id. Keep the raw
                // record, but do not fabricate a correlation key.
                return LineStatus::Parsed;
            };
            ops.push(IngestOp::ToolUpdate {
                session_id: ctx.session_id.clone(),
                correlation_key: tool_id,
                evidence: ctx.evidence(0),
                patch: Box::new(terminal_update(
                    status_error(item.status.as_deref()),
                    false,
                    StatusBasis::NativeState,
                    ts,
                    None,
                )),
                at: ts,
            });
            LineStatus::Parsed
        }
        "local_shell_call" | "web_search_call" | "image_generation_call" => {
            let name = item.item_type.as_str();
            let input = item.action.clone().unwrap_or_else(|| {
                item.revised_prompt
                    .as_ref()
                    .map_or(Value::Null, |prompt| serde_json::json!({"prompt": prompt}))
            });
            let mut row = row(ctx, discriminator, MessageKind::ToolUse, ts);
            row.uuid = item.id.clone();
            stamp_model(&mut row, state);
            let native_id = item.call_id.as_deref().or(item.id.as_deref());
            let correlation_key = native_id.unwrap_or(discriminator);
            let projected = crate::tool::project_request(
                ctx.agent_version.as_deref(),
                name,
                &input,
                state.cwd.as_deref(),
                correlation_key,
                native_id,
                item.namespace.as_deref(),
            );
            let mut tool = match projected {
                Ok(tool) => tool,
                Err(error) => {
                    ops.push(drift(
                        ctx,
                        stream_id,
                        &projection_record_kind("request", item.namespace.as_deref(), name),
                        error.drift_code(),
                        &projection_shape_paths(&input),
                        ts,
                    ));
                    unknown_interaction(
                        correlation_key,
                        if native_id.is_some() {
                            CorrelationDerivation::NativeId
                        } else {
                            CorrelationDerivation::Synthetic
                        },
                        native_id.map(str::to_owned),
                        item.namespace.clone(),
                        name,
                        item.namespace.clone(),
                    )
                }
            };
            let is_error = status_error(item.status.as_deref());
            let patch = terminal_update(
                is_error,
                false,
                StatusBasis::NativeState,
                ts,
                is_error
                    .is_some_and(|value| value)
                    .then_some(item.result.as_deref())
                    .flatten(),
            );
            if let Some(status) = patch.status {
                tool.status = status;
            }
            tool.failure = patch.failure;
            tool.timing = patch.timing;
            row.tool = Some(tool);
            begin_response(state, &row.timeline_id);
            IngestOp::push_message(ops, row);
            LineStatus::Parsed
        }
        other => undeclared(
            ctx,
            stream_id,
            spec::declared_response_items().contains(other),
            &format!("response_item/{other}"),
            ts,
            ops,
        ),
    }
}

/// A top-level compaction checkpoint contributes exactly its replacement
/// summary. Embedded `replacement_history` is a copy of earlier records and is
/// intentionally not expanded.
fn compacted(
    ctx: &mut NormalizeCtx,
    item: &Compacted,
    discriminator: &str,
    ts: Option<Ms>,
    ops: &mut Vec<IngestOp>,
) {
    if item.message.trim().is_empty() {
        return;
    }
    let mut row = row(ctx, discriminator, MessageKind::Summary, ts);
    row.subtype = Some("compacted".to_owned());
    row.content = Some(item.message.clone());
    IngestOp::push_message(ops, row);
}

fn inter_agent_communication(
    ctx: &mut NormalizeCtx,
    item: &InterAgentCommunication,
    discriminator: &str,
    ts: Option<Ms>,
    ops: &mut Vec<IngestOp>,
) {
    if item.content.trim().is_empty() {
        return;
    }
    let mut row = row(ctx, discriminator, MessageKind::Agent, ts);
    row.uuid = item.id.clone();
    row.subtype = Some("inter_agent_communication".to_owned());
    row.content = Some(item.content.clone());
    IngestOp::push_message(ops, row);
}

fn realtime_item(
    ctx: &mut NormalizeCtx,
    item: &RealtimeItem,
    discriminator: &str,
    ts: Option<Ms>,
    ops: &mut Vec<IngestOp>,
) {
    if item.item_type != "transcript_segment" {
        return;
    }
    let Some(content) = item.text.clone().filter(|text| !text.trim().is_empty()) else {
        return;
    };
    let kind = match item.role.as_deref() {
        Some("user") => MessageKind::User,
        Some("assistant") => MessageKind::Agent,
        _ => MessageKind::System,
    };
    let mut row = row(ctx, discriminator, kind, ts);
    row.uuid = Some(item.id.clone());
    row.subtype = Some("realtime_transcript".to_owned());
    row.content = Some(content);
    IngestOp::push_message(ops, row);
}

/// Lifecycle and telemetry events. Token counters project as usage; completed
/// command items project the native status/exit-code fact that the later
/// text-only `function_call_output` record cannot provide.
fn event_msg(
    ctx: &mut NormalizeCtx,
    state: &mut CodexState,
    stream_id: &str,
    event: &EventMsg,
    ts: Option<Ms>,
    ops: &mut Vec<IngestOp>,
) -> LineStatus {
    match event.event_type.as_str() {
        "token_count" => {
            token_count(ctx, state, event, ts, ops);
            LineStatus::Parsed
        }
        "item_completed" => item_completed(ctx, stream_id, event, ts, ops),
        _ => undeclared(
            ctx,
            stream_id,
            spec::declared_event_subtypes().contains(&event.event_type),
            &format!("event_msg/{}", event.event_type),
            ts,
            ops,
        ),
    }
}

fn item_completed(
    ctx: &NormalizeCtx,
    stream_id: &str,
    event: &EventMsg,
    ts: Option<Ms>,
    ops: &mut Vec<IngestOp>,
) -> LineStatus {
    let Some(item) = event.item.as_ref() else {
        return hard_into(
            ctx,
            stream_id,
            "event_msg/item_completed",
            "missing_item",
            ts,
            ops,
        );
    };
    let Some(item_type) = item.get("type").and_then(Value::as_str) else {
        return hard_into(
            ctx,
            stream_id,
            "event_msg/item_completed",
            "missing_item_type",
            ts,
            ops,
        );
    };

    // AgentMessage/UserMessage are redundant delivery telemetry: their
    // canonical prose comes from response_item, which carries richer response
    // and provenance identity. CommandExecution is the one reviewed lifecycle
    // fact in the current writer schema.
    if item_type != "CommandExecution" {
        return LineStatus::Parsed;
    }

    let Some(correlation_key) = item.get("id").and_then(Value::as_str) else {
        return hard_into(
            ctx,
            stream_id,
            "event_msg/item_completed/CommandExecution",
            "missing_item_id",
            ts,
            ops,
        );
    };
    let completed_at = event
        .completed_at_ms
        .and_then(|value| i64::try_from(value).ok())
        .or(ts);
    let is_error = item
        .get("exit_code")
        .and_then(Value::as_i64)
        .map(|code| code != 0)
        .or_else(|| {
            item.get("status")
                .and_then(Value::as_str)
                .and_then(|status| status_error(Some(status)))
        });
    let diagnostic = is_error.is_some_and(|value| value).then(|| {
        item.get("stderr")
            .and_then(Value::as_str)
            .filter(|text| !text.trim().is_empty())
            .or_else(|| item.get("aggregated_output").and_then(Value::as_str))
    });
    let mut patch = terminal_update(
        is_error,
        false,
        StatusBasis::NativeState,
        completed_at,
        diagnostic.flatten(),
    );
    if let Some(timing) = patch.timing.as_mut() {
        timing.duration_ms = item.get("duration").and_then(command_duration_ms);
        timing.started_at = timing.duration_ms.and_then(|duration| {
            i64::try_from(duration)
                .ok()
                .and_then(|duration| completed_at.map(|at| at.saturating_sub(duration)))
        });
    }
    ops.push(IngestOp::ToolUpdate {
        session_id: ctx.session_id.clone(),
        correlation_key: correlation_key.to_owned(),
        evidence: ctx.evidence(0),
        patch: Box::new(patch),
        at: completed_at,
    });
    LineStatus::Parsed
}

fn command_duration_ms(value: &Value) -> Option<u64> {
    let seconds = value.get("secs").and_then(Value::as_u64)?;
    let nanos = value.get("nanos").and_then(Value::as_u64).unwrap_or(0);
    seconds
        .checked_mul(1_000)?
        .checked_add(nanos.saturating_add(999_999) / 1_000_000)
}

/// THE CODEX TRAP (docs/canonical-model.html, `usage_bucket`): "codex token
/// counters are cumulative, claude-code's are per-message — normalize to deltas
/// once, at ingest, or the cache double-counts".
///
/// `info.total_token_usage` is a running SESSION total, re-reported after every
/// turn; `info.last_token_usage` is the same numbers for one turn only. The
/// totals are the safe source — a missed line self-corrects on the next
/// reading, where a missed delta would be lost forever — so the cumulative
/// figures go through `ctx.cumulative`, which subtracts the previous reading
/// (and treats a counter that walks backwards as a reset, never a negative).
///
/// The resulting delta accumulates into a per-response running total, and the
/// op carries that TOTAL rather than the delta: `IngestOp::Usage` is
/// replacement-keyed on the response row, so the writer applies (new − old) and
/// several token_counts for one response can never double-count.
fn token_count(
    ctx: &mut NormalizeCtx,
    state: &mut CodexState,
    event: &EventMsg,
    ts: Option<Ms>,
    ops: &mut Vec<IngestOp>,
) {
    let Some(totals) = event.info.as_ref().map(|info| &info["total_token_usage"]) else {
        return;
    };
    let cached = number(totals, "cached_input_tokens");
    let reading = TokenSnapshot {
        // Codex `input_tokens` INCLUDES the cached part; the canonical model
        // keeps fresh input and cache reads apart, so split by subtraction.
        input: fold::split_inclusive_input(number(totals, "input_tokens"), cached),
        output: number(totals, "output_tokens"),
        cache_read: cached,
        cache_write: number(totals, "cache_write_input_tokens"),
        thinking: number(totals, "reasoning_output_tokens"),
    };
    if reading.is_empty() {
        return;
    }

    let delta = ctx.cumulative.absorb(&reading);
    state.response_total = fold::bucket_apply(&state.response_total, &delta);
    // Nothing to key the snapshot to yet: the total is kept, not dropped, and
    // the first response row that appears carries it (see `begin_response`).
    let Some(response) = state.response.clone() else {
        return;
    };
    ops.push(IngestOp::Usage {
        session_id: ctx.session_id.clone(),
        response,
        ts: ts.unwrap_or_default(),
        model: state.model.clone(),
        tokens: state.response_total,
    });
}

/// Point later `token_count` readings at a newly emitted assistant row.
///
/// A codex turn ends in a message OR a tool call, and the token_count lands
/// after whichever it was — so the "response row" a usage snapshot belongs to is
/// the most recent ASSISTANT-authored row, tool calls included. The committed
/// tool-use fixture is exactly this case: its first token_count (ordinal 11)
/// follows a function_call and precedes the first agent message, and keying
/// usage to agent rows alone would drop half the session's tokens.
///
/// The running total resets only once something has been attributed, so a
/// reading that arrives before any response row is carried onto the next one.
fn begin_response(state: &mut CodexState, timeline_id: &str) {
    if state.response.is_some() {
        state.response_total = TokenSnapshot::default();
    }
    state.response = Some(timeline_id.to_owned());
}

// ————— row construction —————————————————————————————————————————————

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

/// Model and effort are per-row observations that codex only ever writes on
/// `turn_context`, so assistant-authored rows inherit the last one seen. User
/// rows get none: no model produced them.
fn stamp_model(row: &mut MessageRow, state: &CodexState) {
    row.model = state.model.clone();
    row.effort = state.effort.clone();
}

fn block_text(blocks: Option<&[ContentBlock]>) -> Option<String> {
    let joined = blocks?
        .iter()
        .filter_map(|block| block.text.as_deref())
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    (!joined.is_empty()).then_some(joined)
}

fn status_error(status: Option<&str>) -> Option<bool> {
    match status {
        Some("failed" | "error" | "incomplete") => Some(true),
        Some("completed") => Some(false),
        _ => None,
    }
}

/// `arguments` is a JSON-encoded STRING (canonical: `function_call.arguments`).
/// A body that will not parse is kept verbatim rather than dropped.
fn tool_input(arguments: Option<&Value>) -> Value {
    let Some(arguments) = arguments else {
        return Value::Null;
    };
    match arguments {
        Value::String(arguments) => {
            serde_json::from_str(arguments).unwrap_or_else(|_| Value::String(arguments.to_owned()))
        }
        value => value.clone(),
    }
}

/// Select only a human-readable failure diagnostic. Successful output never
/// calls this helper and no native object crosses the canonical boundary.
fn tool_output_text(output: Option<&Value>) -> Option<String> {
    let output = output?;
    match output {
        Value::String(text) => Some(text.clone()),
        Value::Array(blocks) => {
            let text = blocks
                .iter()
                .filter_map(|block| block.get("text").and_then(Value::as_str))
                .filter(|text| !text.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            (!text.is_empty()).then_some(text)
        }
        Value::Object(map) => map
            .get("text")
            .or_else(|| map.get("message"))
            .or_else(|| map.get("error"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        Value::Number(_) | Value::Bool(_) => Some(output.to_string()),
        Value::Null => None,
    }
}

/// The envelope ordinal is the preferred within-session key. Writers before
/// it was introduced are still append-only, so the record's byte offset is an
/// equally deterministic fallback supplied by the pipeline.
fn record_discriminator(record: &RolloutLine, ctx: &NormalizeCtx) -> Option<String> {
    record
        .ordinal
        .map(|ordinal| format!("rollout#{ordinal}"))
        .or_else(|| {
            ctx.src_offset
                .map(|offset| format!("rollout-offset#{offset}"))
        })
}

fn number(value: &Value, key: &str) -> Option<i64> {
    value.get(key).and_then(Value::as_i64)
}

// ————— apply_patch → file edits ——————————————————————————————————————

struct PatchEdit {
    path: String,
    kind: EditKind,
    added: Option<i64>,
    removed: Option<i64>,
}

/// The patch body of an edit-shaped call, if the arguments carry one.
///
/// NOT fixture-backed at 0.149.1: the canonical's `tool-call-arguments` facet
/// declares `exec_command` and nothing else, so this stays deliberately narrow —
/// one tool name, and no rows at all unless an explicit patch header is present.
fn patch_body<'a>(name: &str, input: &'a Value) -> Option<&'a str> {
    if name != APPLY_PATCH {
        return None;
    }
    let body = input
        .get("input")
        .or_else(|| input.get("patch"))
        .and_then(Value::as_str)
        .or_else(|| input.as_str())?;
    body.contains("*** ").then_some(body)
}

/// Count added/removed lines per file in an apply_patch body. Files whose
/// content the patch does not carry (a delete) report unknown counts rather
/// than zero.
fn patch_edits(patch: &str) -> Vec<PatchEdit> {
    let mut edits = Vec::new();
    let mut open: Option<OpenFile> = None;
    for line in patch.lines() {
        if let Some(header) = line.strip_prefix("*** ") {
            // A rename header does not open a new file — the hunks that follow
            // still belong to the section above it.
            if header.starts_with("Move to:") {
                continue;
            }
            if let Some(file) = open.take() {
                edits.push(file.finish());
            }
            open = OpenFile::start(header);
            continue;
        }
        let Some(file) = open.as_mut() else {
            continue;
        };
        if let Some(text) = line.strip_prefix('+') {
            file.added.push(text);
        } else if let Some(text) = line.strip_prefix('-') {
            file.removed.push(text);
        }
    }
    if let Some(file) = open.take() {
        edits.push(file.finish());
    }
    edits
}

struct OpenFile<'a> {
    path: String,
    kind: EditKind,
    /// Whether the patch carries the file's lines at all.
    counted: bool,
    added: Vec<&'a str>,
    removed: Vec<&'a str>,
}

impl<'a> OpenFile<'a> {
    fn start(header: &str) -> Option<Self> {
        let (verb, path) = header.split_once(": ")?;
        let (kind, counted) = match verb {
            "Add File" => (EditKind::Write, true),
            "Update File" => (EditKind::Edit, true),
            "Delete File" => (EditKind::Edit, false),
            // "Begin Patch" / "End Patch" carry no colon and never reach here.
            _ => return None,
        };
        Some(Self {
            path: path.trim().to_owned(),
            kind,
            counted,
            added: Vec::new(),
            removed: Vec::new(),
        })
    }

    fn finish(self) -> PatchEdit {
        let added = self.added.join("\n");
        let (lines_added, lines_removed) = if !self.counted {
            (None, None)
        } else if self.kind == EditKind::Write {
            // A whole-file add: everything is new, nothing is known to be gone.
            fold::line_delta(None, &added)
        } else {
            fold::line_delta(Some(&self.removed.join("\n")), &added)
        };
        PatchEdit {
            path: self.path,
            kind: self.kind,
            added: lines_added,
            removed: lines_removed,
        }
    }
}

// ————— drift bookkeeping ————————————————————————————————————————————

/// A payload subtype the canonical does not declare: soft drift, because these
/// vocabularies grow with every codex release. A declared one is silence.
fn undeclared(
    ctx: &NormalizeCtx,
    stream_id: &str,
    declared: bool,
    record_kind: &str,
    ts: Option<Ms>,
    ops: &mut Vec<IngestOp>,
) -> LineStatus {
    if declared {
        return LineStatus::Parsed;
    }
    ops.push(drift(
        ctx,
        stream_id,
        record_kind,
        "unknown_subtype",
        &[],
        ts,
    ));
    LineStatus::UnknownType
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

/// A KNOWN record kind that could not be projected — the drift alarm.
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

/// Malformed JSON and a declared record whose shape moved are different
/// failures: only the second is the alarm this adapter exists to raise.
fn parse_failure(ctx: &NormalizeCtx, stream_id: &str, text: &str) -> LineOutcome {
    let value: Option<Value> = serde_json::from_str(text).ok();
    let Some(value) = value else {
        return hard(ctx, stream_id, "<invalid-json>", "invalid_json", &[], None);
    };
    let kind = value["type"].as_str().unwrap_or("<untyped>").to_owned();
    hard(
        ctx,
        stream_id,
        &kind,
        "parse_failed",
        &keys(&value),
        stamp(&value),
    )
}

fn record_kind(payload: &RolloutPayload) -> String {
    match payload {
        RolloutPayload::SessionMeta(_) => "session_meta".to_owned(),
        RolloutPayload::TurnContext(_) => "turn_context".to_owned(),
        RolloutPayload::WorldState(_) => "world_state".to_owned(),
        RolloutPayload::Compacted(_) => "compacted".to_owned(),
        RolloutPayload::InterAgentCommunication(_) => "inter_agent_communication".to_owned(),
        RolloutPayload::InterAgentCommunicationMetadata(_) => {
            "inter_agent_communication_metadata".to_owned()
        }
        RolloutPayload::SecurityRiskScore(_) => "security_risk_score".to_owned(),
        RolloutPayload::RealtimeItem(_) => "realtime_item".to_owned(),
        RolloutPayload::EventMsg(event) => format!("event_msg/{}", event.event_type),
        RolloutPayload::ResponseItem(item) => format!("response_item/{}", item.item_type),
    }
}

fn stamp(value: &Value) -> Option<Ms> {
    value["timestamp"].as_str().and_then(rfc3339_ms)
}

/// Field paths for drift aggregation — top-level keys are enough to tell one
/// unrecognized shape from another.
fn keys(value: &Value) -> Vec<String> {
    match value.as_object() {
        Some(map) => map.keys().cloned().collect(),
        None => vec!["<non-object>".to_owned()],
    }
}

#[cfg(test)]
mod tests {
    use super::{APPLY_PATCH, patch_body, patch_edits, tool_input, tool_output_text};
    use rosetta_core::rows::EditKind;
    use serde_json::{Value, json};

    const PATCH: &str = "*** Begin Patch\n\
        *** Update File: src/main.rs\n\
        @@ fn main() {\n\
        -    println!(\"old\");\n\
        +    println!(\"new\");\n\
        +    println!(\"and more\");\n\
        *** Add File: src/added.rs\n\
        +fn added() {}\n\
        *** Delete File: src/gone.rs\n\
        *** End Patch";

    #[test]
    fn tool_arguments_are_decoded_json_strings() {
        assert_eq!(
            tool_input(Some(&json!(r#"{"cmd":"echo rosetta-mock"}"#))),
            json!({"cmd": "echo rosetta-mock"})
        );
        // A body that will not parse is kept verbatim, never dropped.
        assert_eq!(tool_input(Some(&json!("not json"))), json!("not json"));
        assert_eq!(
            tool_input(Some(&json!({"query": "tools"}))),
            json!({"query": "tools"})
        );
        assert_eq!(tool_input(None), Value::Null);
    }

    #[test]
    fn block_output_reduces_to_a_failure_diagnostic() {
        let output = json!([
            {"type": "input_text", "text": "first"},
            {"type": "input_text", "text": "second"},
        ]);
        assert_eq!(
            tool_output_text(Some(&output)).as_deref(),
            Some("first\nsecond")
        );
        assert_eq!(
            tool_output_text(Some(&json!({"error": "boom"}))).as_deref(),
            Some("boom")
        );
    }

    #[test]
    fn patch_bodies_are_recognized_narrowly() {
        let args = json!({"input": PATCH});
        assert!(patch_body(APPLY_PATCH, &args).is_some());
        // Another tool's arguments are never read as a patch...
        assert!(patch_body("exec_command", &args).is_none());
        // ...and neither is an apply_patch call with no patch header.
        assert!(patch_body(APPLY_PATCH, &json!({"input": "nope"})).is_none());
    }

    #[test]
    fn patch_line_counts_are_per_file() {
        let edits = patch_edits(PATCH);
        assert_eq!(edits.len(), 3);

        assert_eq!(edits[0].path, "src/main.rs");
        assert_eq!(edits[0].kind, EditKind::Edit);
        assert_eq!((edits[0].added, edits[0].removed), (Some(2), Some(1)));

        assert_eq!(edits[1].path, "src/added.rs");
        assert_eq!(edits[1].kind, EditKind::Write);
        // A whole-file add: nothing is KNOWN to have been removed.
        assert_eq!((edits[1].added, edits[1].removed), (Some(1), None));

        assert_eq!(edits[2].path, "src/gone.rs");
        assert_eq!((edits[2].added, edits[2].removed), (None, None));
    }

    #[test]
    fn rename_headers_keep_their_hunks() {
        let edits = patch_edits(
            "*** Begin Patch\n\
             *** Update File: a.rs\n\
             *** Move to: b.rs\n\
             -gone\n\
             +kept\n\
             *** End Patch",
        );
        assert_eq!(edits.len(), 1);
        assert_eq!((edits[0].added, edits[0].removed), (Some(1), Some(1)));
    }
}
