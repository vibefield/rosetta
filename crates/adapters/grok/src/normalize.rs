//! Grok `updates` records → canonical operations.
//!
//! `updates.jsonl` is the authoritative ACP conversation log in the canonical
//! ground truth. Message chunks are accumulated into stable logical rows,
//! tool results merge by `toolCallId`, and `turn_completed.usage` refines the
//! best assistant-authored row for that prompt. The overlapping raw-model and
//! telemetry rails are excluded by `GrokAdapter::stream_driver`.

use std::collections::BTreeMap;

use rosetta_adapter_support::Drift;
use rosetta_adapter_support::adapter::{LineOutcome, LineStatus, NormalizeCtx};
use rosetta_adapter_support::tool::{
    projection_record_kind, projection_shape_paths, terminal_update, unknown_interaction,
};
use rosetta_core::rows::{
    DriftFact, EditKind, FileEditRow, MessageKind, MessageRow, Ms, PlanRow, SessionUsageRow,
    TaskRow, TaskStatus, TokenSnapshot, UsageScope,
};
use rosetta_core::timeline::{CorrelationDerivation, StatusBasis, ToolAction, ToolUpdatePatch};
use rosetta_core::{IngestOp, fold, ids};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::raw::updates::{ParsedLine, SessionUpdate, UpdateLine, parse_line};
use crate::spec;
use crate::time::{seconds_ms, system_time_ms, value_ms};

const UNKNOWN_TYPE: &str = "unknown_type";
const UNKNOWN_FIELDS: &str = "unknown_fields";
const PARSE_FAILED: &str = "parse_failed";
const INVALID_JSON: &str = "<invalid-json>";
const UNTYPED: &str = "<untyped>";

/// Checkpointed per-`updates.jsonl` state.
///
/// Grok emits streaming chunks, reuses its envelope event counter after a
/// resume, reports the model before the response, and reports usage after it.
/// All four facts require cross-line state to normalize without duplicate rows.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GrokState {
    messages: BTreeMap<String, MessageRow>,
    current_model: Option<String>,
    responses: BTreeMap<String, ResponseTarget>,
    /// Description → native spawn call ids. Grok's authoritative spawn event
    /// repeats the request description and child session id but omits the
    /// tool-call id. Only a unique exact description is strong enough to join.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pending_delegates: BTreeMap<String, Vec<String>>,
    plan_revision: i64,
}

impl GrokState {
    /// Serialize only restart-portable state. `src_cursor` is a surrogate id
    /// owned by one SQLite projection; carrying it through a ledger rebuild
    /// made otherwise identical checkpoints depend on adapter scheduling.
    /// The next chunk always stamps the current cursor before emitting the
    /// cached message, so retaining that local rowid has no normalization
    /// value and can create a dangling provenance reference after restore.
    pub(crate) fn checkpoint_value(&self) -> serde_json::Result<Value> {
        let mut checkpoint = self.clone();
        for message in checkpoint.messages.values_mut() {
            message.src_cursor = None;
        }
        serde_json::to_value(checkpoint)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ResponseTarget {
    timeline_id: String,
    /// agent text > tool call > thinking; the first row at the best rank wins.
    rank: u8,
}

pub fn ingest_line(
    ctx: &mut NormalizeCtx,
    state: &mut GrokState,
    stream_id: &str,
    line: &[u8],
) -> LineOutcome {
    let Ok(text) = std::str::from_utf8(line) else {
        return hard(ctx, stream_id, INVALID_JSON, None, &[]);
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
        ParsedLine::UnknownType { value, .. } => {
            let kind = update_kind(&value).unwrap_or(UNTYPED);
            if spec::declared_update_types().contains(kind) {
                return hard(ctx, stream_id, kind, stamp_value(&value), &keys(&value));
            }
            return unknown(ctx, stream_id, kind, stamp_value(&value), &keys(&value));
        }
    };

    let kind = record.params.update.session_update.as_str();
    if !spec::declared_update_types().contains(kind) {
        return unknown(ctx, stream_id, kind, stamp(&record), &[]);
    }

    let mut ops = Vec::new();
    let paths = record.drift_paths();
    if !paths.is_empty() {
        ops.push(drift(
            ctx,
            stream_id,
            kind,
            UNKNOWN_FIELDS,
            &paths,
            stamp(&record),
        ));
    }

    let status = project(ctx, state, stream_id, &record, &mut ops);
    LineOutcome { ops, status }
}

fn project(
    ctx: &mut NormalizeCtx,
    state: &mut GrokState,
    stream_id: &str,
    line: &UpdateLine,
    ops: &mut Vec<IngestOp>,
) -> LineStatus {
    let update = &line.params.update;
    let result = match update.session_update.as_str() {
        "user_message_chunk" => user_chunk(ctx, state, line, ops),
        "agent_message_chunk" => response_chunk(ctx, state, line, MessageKind::Agent, ops),
        "agent_thought_chunk" => response_chunk(ctx, state, line, MessageKind::Thinking, ops),
        "tool_call" => tool_call(ctx, state, stream_id, line, ops),
        "tool_call_update" => tool_result(ctx, line, ops),
        "turn_completed" => turn_completed(ctx, state, line, ops),
        "auto_compact_started" => simple_message(
            ctx,
            line,
            MessageKind::System,
            "auto_compact_started",
            update.reason.clone(),
            ops,
        ),
        "auto_compact_completed" => simple_message(
            ctx,
            line,
            MessageKind::System,
            "auto_compact_completed",
            None,
            ops,
        ),
        "current_mode_update" => simple_message(
            ctx,
            line,
            MessageKind::System,
            "mode_change",
            update.current_mode_id.clone(),
            ops,
        ),
        "image_compressed" => simple_message(
            ctx,
            line,
            MessageKind::System,
            "image_compressed",
            update.message.clone(),
            ops,
        ),
        "image_dropped" => simple_message(
            ctx,
            line,
            MessageKind::System,
            "image_dropped",
            update.notes.as_ref().and_then(value_text),
            ops,
        ),
        "retry_state" => simple_message(
            ctx,
            line,
            MessageKind::System,
            "retry_state",
            update.message.clone().or_else(|| update.reason.clone()),
            ops,
        ),
        "session_recap" => simple_message(
            ctx,
            line,
            MessageKind::Summary,
            "session_recap",
            update.summary.as_ref().and_then(value_text),
            ops,
        ),
        "subagent_spawned" => subagent_spawned(ctx, state, line, ops),
        // Completion output is machine plumbing. The ordinary tool lifecycle
        // closes the visible delegate row; no second system message is useful.
        "subagent_finished" => Ok(()),
        "plan" => plan(ctx, state, line, ops),
        "goal_updated" => goal(ctx, line, ops),
        "task_backgrounded" => task_backgrounded(ctx, line, ops),
        "task_completed" => task_completed(ctx, line, ops),
        // Declared session bookkeeping with no honest canonical projection.
        "compaction_checkpoint" | "hook_execution" => Ok(()),
        // Every remaining arm is canonical-declared but intentionally
        // unprojected.
        _ => Ok(()),
    };

    match result {
        Ok(()) => LineStatus::Parsed,
        Err(()) => hard_into(ctx, stream_id, &update.session_update, stamp(line), ops),
    }
}

fn user_chunk(
    ctx: &mut NormalizeCtx,
    state: &mut GrokState,
    line: &UpdateLine,
    ops: &mut Vec<IngestOp>,
) -> Result<(), ()> {
    let update = &line.params.update;
    let meta = update.meta.as_ref().and_then(Value::as_object).ok_or(())?;
    let prompt_index = integer(meta.get("promptIndex").ok_or(())?).ok_or(())?;
    let model = meta
        .get("modelId")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or(())?;
    state.current_model = Some(model.to_owned());

    // Hidden native reminders are not visible conversation rows.
    if meta
        .get("hideFromScrollback")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Ok(());
    }

    let event_id = event_id(line)?;
    let content = update.content.as_ref().ok_or(())?;
    let content_type = content["type"].as_str().ok_or(())?;
    let text = match content_type {
        "text" => Some(content["text"].as_str().ok_or(())?),
        "image" => None,
        _ => return Err(()),
    };
    let discriminator = format!("user:{prompt_index}");
    let attachment = if content_type == "image" {
        use rosetta_adapter_support::media;
        use rosetta_core::timeline::{AttachmentKind, AttachmentUnavailableReason};
        // Source position, not the event counter (which can repeat on resume),
        // identifies this occurrence in the accumulated prompt.
        let index = state
            .messages
            .get(&discriminator)
            .map_or(0, |row| row.attachments.len());
        let id = format!("image-{index}");
        let (attachment, asset) = match (
            content["data"].as_str(),
            content["mimeType"].as_str(),
            content["uri"].as_str(),
        ) {
            (Some(data), Some(mime), _) => {
                media::from_base64(&id, AttachmentKind::Image, mime, data)
            }
            (None, mime, Some(uri)) => media::from_url(&id, AttachmentKind::Image, uri, mime),
            _ => media::unavailable(
                &id,
                AttachmentKind::Image,
                AttachmentUnavailableReason::InvalidData,
            ),
        };
        ops.extend(asset.map(|asset| IngestOp::ProjectionAsset { asset }));
        Some(attachment)
    } else {
        None
    };
    accumulate_message(
        ctx,
        state,
        ChunkSpec {
            discriminator,
            kind: MessageKind::User,
            uuid: event_id,
            ts: stamp(line),
            model: None,
            text,
            separator: "\n",
            attachment,
        },
        ops,
    );
    Ok(())
}

fn response_chunk(
    ctx: &mut NormalizeCtx,
    state: &mut GrokState,
    line: &UpdateLine,
    kind: MessageKind,
    ops: &mut Vec<IngestOp>,
) -> Result<(), ()> {
    let meta = envelope_meta(line)?;
    let event_id = string(meta, "eventId")?;
    let prompt_id = string(meta, "promptId")?;
    let stream_start = integer(meta.get("streamStartMs").ok_or(())?).ok_or(())?;

    let content = line.params.update.content.as_ref().ok_or(())?;
    if content["type"].as_str() != Some("text") {
        return Err(());
    }
    let text = content["text"].as_str().ok_or(())?;
    let label = if kind == MessageKind::Agent {
        "agent"
    } else {
        "thinking"
    };
    let discriminator = format!("{label}:{prompt_id}:{stream_start}");
    let timeline_id = ids::timeline_id(&ctx.session_id, &discriminator);
    accumulate_message(
        ctx,
        state,
        ChunkSpec {
            discriminator,
            kind,
            uuid: event_id,
            ts: stamp(line),
            model: state.current_model.clone(),
            text: Some(text),
            // Model deltas concatenate byte-for-byte.
            separator: "",
            attachment: None,
        },
        ops,
    );
    remember_response(
        state,
        prompt_id,
        timeline_id,
        if kind == MessageKind::Agent { 3 } else { 1 },
    );
    Ok(())
}

struct ChunkSpec<'a> {
    discriminator: String,
    kind: MessageKind,
    uuid: &'a str,
    ts: Option<Ms>,
    model: Option<String>,
    text: Option<&'a str>,
    separator: &'a str,
    attachment: Option<rosetta_core::timeline::AttachmentRef>,
}

fn accumulate_message(
    ctx: &mut NormalizeCtx,
    state: &mut GrokState,
    spec: ChunkSpec<'_>,
    ops: &mut Vec<IngestOp>,
) {
    if !state.messages.contains_key(&spec.discriminator) {
        let mut row = row(ctx, &spec.discriminator, spec.kind, spec.ts);
        row.uuid = Some(spec.uuid.to_owned());
        row.model = spec.model;
        state.messages.insert(spec.discriminator.clone(), row);
    }
    let row = state
        .messages
        .get_mut(&spec.discriminator)
        .expect("message was inserted above");
    append(&mut row.content, spec.text, spec.separator);
    row.attachments.extend(spec.attachment);
    row.src_cursor = ctx.src_cursor;
    row.src_offset = ctx.src_offset;
    IngestOp::push_message(ops, row.clone());
}

fn append(target: &mut Option<String>, text: Option<&str>, separator: &str) {
    let Some(text) = text else { return };
    match target {
        Some(current) => {
            if !current.is_empty() && !text.is_empty() {
                current.push_str(separator);
            }
            current.push_str(text);
        }
        None => *target = Some(text.to_owned()),
    }
}

fn tool_call(
    ctx: &mut NormalizeCtx,
    state: &mut GrokState,
    stream_id: &str,
    line: &UpdateLine,
    ops: &mut Vec<IngestOp>,
) -> Result<(), ()> {
    let update = &line.params.update;
    let meta = envelope_meta(line)?;
    let event_id = string(meta, "eventId")?;
    let prompt_id = string(meta, "promptId")?;
    let tool_id = update.tool_call_id.as_deref().ok_or(())?;
    let input = update.raw_input.clone().ok_or(())?;
    if !input.is_object() {
        return Err(());
    }
    let descriptor = tool_descriptor(update);
    let name = descriptor
        .and_then(|value| value["name"].as_str())
        .or(update.title.as_deref())
        .filter(|value| !value.is_empty())
        .ok_or(())?;

    let discriminator = format!("tool:{tool_id}");
    let mut message = row(ctx, &discriminator, MessageKind::ToolUse, stamp(line));
    message.uuid = Some(event_id.to_owned());
    message.model = state.current_model.clone();
    let projected = crate::tool::project_request(
        ctx.agent_version.as_deref(),
        name,
        &input,
        None,
        tool_id,
        Some(tool_id),
    );
    message.tool = Some(match projected {
        Ok(tool) => tool,
        Err(error) => {
            ops.push(drift(
                ctx,
                stream_id,
                &projection_record_kind("request", None, name),
                error.drift_code(),
                &projection_shape_paths(&input),
                stamp(line),
            ));
            unknown_interaction(
                tool_id,
                CorrelationDerivation::NativeId,
                Some(tool_id.to_owned()),
                None,
                name,
                None,
            )
        }
    });
    if matches!(
        message.tool.as_ref().map(|tool| &tool.action),
        Some(ToolAction::Delegate { .. })
    ) {
        message.branch_tool_id = Some(tool_id.to_owned());
        if let Some(description) = input
            .get("description")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            state
                .pending_delegates
                .entry(description.to_owned())
                .or_default()
                .push(tool_id.to_owned());
        }
    }
    let timeline_id = message.timeline_id.clone();
    IngestOp::push_message(ops, message);

    if let Some(edit) = file_edit(
        &ctx.session_id,
        &timeline_id,
        descriptor.and_then(|value| value["kind"].as_str()),
        &input,
        stamp(line),
    ) {
        ops.push(IngestOp::FileEdit(edit));
    }
    remember_response(state, prompt_id, timeline_id, 2);
    Ok(())
}

fn subagent_spawned(
    ctx: &NormalizeCtx,
    state: &mut GrokState,
    line: &UpdateLine,
    ops: &mut Vec<IngestOp>,
) -> Result<(), ()> {
    let update = &line.params.update;
    if let (Some(expected), Some(actual)) = (
        ctx.native_session_id.as_deref(),
        update.parent_session_id.as_deref(),
    ) && expected != actual
    {
        return Err(());
    }
    let description = update
        .description
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or(())?;
    let child_native_id = update
        .child_session_id
        .as_deref()
        .or(update.subagent_id.as_deref())
        .filter(|value| !value.is_empty())
        .ok_or(())?;

    let correlation_key = match state.pending_delegates.get(description) {
        Some(candidates) if candidates.len() == 1 => candidates[0].clone(),
        // Duplicate descriptions are not ordered evidence. Leave those
        // children honestly unanchored instead of guessing by adjacency.
        _ => return Ok(()),
    };
    state.pending_delegates.remove(description);
    ops.push(IngestOp::ToolUpdate {
        session_id: ctx.session_id.clone(),
        correlation_key,
        evidence: ctx.evidence(0),
        patch: Box::new(ToolUpdatePatch {
            delegate_child_session_id: Some(ids::session_id(&ctx.runtime_id, child_native_id)),
            ..ToolUpdatePatch::default()
        }),
        at: stamp(line),
    });
    Ok(())
}

fn tool_descriptor(update: &SessionUpdate) -> Option<&Value> {
    update
        .meta
        .as_ref()?
        .as_object()?
        .values()
        .find(|value| value["name"].is_string())
}

fn file_edit(
    session_id: &str,
    timeline_id: &str,
    tool_kind: Option<&str>,
    input: &Value,
    at: Option<Ms>,
) -> Option<FileEditRow> {
    let path = ["file_path", "target_file", "path"]
        .into_iter()
        .find_map(|key| input[key].as_str())?;
    let (kind, lines) = match tool_kind? {
        "edit" => (
            EditKind::Edit,
            input["new_string"]
                .as_str()
                .map(|new| fold::line_delta(input["old_string"].as_str(), new))
                .unwrap_or((None, None)),
        ),
        "write" => (
            EditKind::Write,
            input["content"]
                .as_str()
                .map(|content| fold::line_delta(None, content))
                .unwrap_or((None, None)),
        ),
        _ => return None,
    };
    Some(FileEditRow {
        session_id: session_id.to_owned(),
        message_timeline_id: Some(timeline_id.to_owned()),
        path: path.to_owned(),
        kind,
        snapshot_ref: None,
        lines_added: lines.0,
        lines_removed: lines.1,
        at,
    })
}

fn tool_result(ctx: &NormalizeCtx, line: &UpdateLine, ops: &mut Vec<IngestOp>) -> Result<(), ()> {
    let update = &line.params.update;
    let tool_id = update.tool_call_id.as_deref().ok_or(())?;
    let terminal_error = match update.status.as_deref() {
        None | Some("in_progress") => return Ok(()),
        Some("completed") => None,
        Some("failed") => Some(true),
        Some(_) => return Err(()),
    };
    let result = update
        .content
        .as_ref()
        .and_then(tool_content_text)
        .or_else(|| update.raw_output.as_ref().and_then(raw_output_text));
    let is_error = terminal_error.or_else(|| update.raw_output.as_ref().and_then(raw_output_error));
    ops.push(IngestOp::ToolUpdate {
        session_id: ctx.session_id.clone(),
        correlation_key: tool_id.to_owned(),
        evidence: ctx.evidence(0),
        patch: Box::new(terminal_update(
            is_error,
            terminal_error.is_none(),
            StatusBasis::NativeState,
            stamp(line),
            is_error
                .is_some_and(|value| value)
                .then_some(result.as_deref())
                .flatten(),
        )),
        at: stamp(line),
    });
    Ok(())
}

fn tool_content_text(value: &Value) -> Option<String> {
    let text = value
        .as_array()?
        .iter()
        .filter_map(|block| {
            (block["type"].as_str() == Some("content"))
                .then(|| block["content"]["text"].as_str())
                .flatten()
        })
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    (!text.is_empty()).then_some(text)
}

fn raw_output_text(value: &Value) -> Option<String> {
    for key in [
        "output_for_prompt",
        "tool_output_for_prompt",
        "raw_output",
        "content",
        "summary",
        "message",
        "error",
    ] {
        if let Some(text) = value[key].as_str().filter(|text| !text.is_empty()) {
            return Some(text.to_owned());
        }
    }
    let object = value.as_object()?;
    object
        .values()
        .filter(|value| value.is_object())
        .find_map(raw_output_text)
        .or_else(|| {
            object
                .iter()
                .filter(|(key, _)| key.as_str() != "type")
                .find_map(|(_, value)| value.as_str().map(str::to_owned))
        })
}

fn raw_output_error(value: &Value) -> Option<bool> {
    if let Some(exit) = value["exit_code"].as_i64() {
        return Some(exit != 0);
    }
    if value["status"].as_str() == Some("failed") {
        return Some(true);
    }
    value
        .as_object()?
        .values()
        .filter(|value| value.is_object())
        .find_map(raw_output_error)
}

fn remember_response(state: &mut GrokState, prompt_id: &str, timeline_id: String, rank: u8) {
    match state.responses.get_mut(prompt_id) {
        Some(target) if rank > target.rank => {
            *target = ResponseTarget { timeline_id, rank };
        }
        Some(_) => {}
        None => {
            state
                .responses
                .insert(prompt_id.to_owned(), ResponseTarget { timeline_id, rank });
        }
    }
}

fn turn_completed(
    ctx: &mut NormalizeCtx,
    state: &mut GrokState,
    line: &UpdateLine,
    ops: &mut Vec<IngestOp>,
) -> Result<(), ()> {
    let update = &line.params.update;
    let prompt_id = update.prompt_id.as_deref().ok_or(())?;
    let stop_reason = update.stop_reason.as_deref().ok_or(())?;
    if !matches!(stop_reason, "end_turn" | "cancelled" | "error") {
        return Err(());
    }

    if let (Some(usage), Some(ts)) = (update.usage.as_ref(), stamp(line)) {
        turn_usage(ctx, state, prompt_id, usage, ts, ops);
    }

    if stop_reason != "end_turn" {
        simple_message(
            ctx,
            line,
            MessageKind::System,
            &format!("turn_{stop_reason}"),
            update.agent_result.clone(),
            ops,
        )?;
    }
    Ok(())
}

/// One completed prompt's usage (RESEARCH_PLAN §5.13). `turn_completed`
/// totals every main-loop model call of the prompt — per completed turn, not
/// a running counter — with one `modelUsage` entry per served model, so each
/// model prices at its own rate. The prompt's main model rides on its response
/// row; any other model, or a prompt without a row, is a sample of its own.
fn turn_usage(
    ctx: &NormalizeCtx,
    state: &GrokState,
    prompt_id: &str,
    usage: &Value,
    ts: Ms,
    ops: &mut Vec<IngestOp>,
) {
    let mut entries: Vec<(Option<String>, &Value)> = match usage["modelUsage"].as_object() {
        Some(models) if !models.is_empty() => models
            .iter()
            .map(|(model, entry)| (Some(model.clone()), entry))
            .collect(),
        _ => vec![(state.current_model.clone(), usage)],
    };
    // The current model's entry first, so it is the one on the response row.
    entries.sort_by_key(|(model, _)| model.as_deref() != state.current_model.as_deref());
    let mut target = state
        .responses
        .get(prompt_id)
        .map(|target| target.timeline_id.clone());
    for (model, entry) in entries {
        let (tokens, clamped) = grok_snapshot(entry);
        if clamped {
            ops.push(drift(
                ctx,
                spec::UPDATES,
                "turn_completed",
                "usage_subset_exceeds_total",
                &["usage".to_owned()],
                Some(ts),
            ));
        }
        if tokens.is_empty() {
            continue;
        }
        match target.take() {
            Some(response) => ops.push(IngestOp::Usage {
                session_id: ctx.session_id.clone(),
                response,
                ts,
                model,
                tokens,
            }),
            None => ops.push(IngestOp::SessionUsage(SessionUsageRow {
                session_id: ctx.session_id.clone(),
                key: ids::usage_id(
                    &ctx.session_id,
                    &format!("turn:{prompt_id}:{}", model.as_deref().unwrap_or("")),
                ),
                ts,
                model,
                tokens,
            })),
        }
    }
}

/// ACP `inputTokens` is the full prompt (uncached + cache reads + cache
/// writes) and `outputTokens` includes `reasoningTokens`. `costUsdTicks` is
/// xAI's own charge at 10¹⁰ ticks per USD (100 pico-USD per tick); the CLI
/// removes it when the turn's cost is partial. A turn with more than one model
/// call is a total over several requests.
fn grok_snapshot(usage: &Value) -> (TokenSnapshot, bool) {
    let cache_read = number(usage, "cachedReadTokens");
    let cache_write = number(usage, "cacheCreationTokens");
    let (input, input_clamped) =
        fold::exclusive_remainder(number(usage, "inputTokens"), &[cache_read, cache_write]);
    let (output, thinking, output_clamped) = fold::split_subset(
        number(usage, "outputTokens"),
        number(usage, "reasoningTokens"),
    );
    let tokens = TokenSnapshot {
        input,
        output,
        cache_read,
        cache_write,
        thinking,
        billed_pico_usd: number(usage, "costUsdTicks")
            .filter(|ticks| *ticks > 0)
            .and_then(|ticks| ticks.checked_mul(100)),
        scope: if number(usage, "modelCalls") == Some(1) {
            UsageScope::Request
        } else {
            UsageScope::Aggregate
        },
        ..TokenSnapshot::default()
    };
    (tokens, input_clamped || output_clamped)
}

fn simple_message(
    ctx: &mut NormalizeCtx,
    line: &UpdateLine,
    kind: MessageKind,
    subtype: &str,
    content: Option<String>,
    ops: &mut Vec<IngestOp>,
) -> Result<(), ()> {
    let event = event_id(line)?;
    let at = stamp(line).ok_or(())?;
    let discriminator = format!("update:{subtype}:{event}:{at}");
    let mut message = row(ctx, &discriminator, kind, Some(at));
    message.subtype = Some(subtype.to_owned());
    message.uuid = Some(event.to_owned());
    message.content = content;
    IngestOp::push_message(ops, message);
    Ok(())
}

fn plan(
    ctx: &NormalizeCtx,
    state: &mut GrokState,
    line: &UpdateLine,
    ops: &mut Vec<IngestOp>,
) -> Result<(), ()> {
    let entries = line.params.update.entries.as_ref().ok_or(())?;
    if !entries.is_array() {
        return Err(());
    }
    let updated_at = stamp(line).ok_or(())?;
    let content = serde_json::to_string(entries).map_err(|_| ())?;
    ops.push(IngestOp::Plan(PlanRow {
        session_id: ctx.session_id.clone(),
        revision: state.plan_revision,
        content,
        updated_at,
    }));
    state.plan_revision += 1;
    Ok(())
}

fn goal(ctx: &NormalizeCtx, line: &UpdateLine, ops: &mut Vec<IngestOp>) -> Result<(), ()> {
    let update = &line.params.update;
    let native_id = update.goal_id.as_deref().ok_or(())?;
    let content = update.objective.clone().ok_or(())?;
    let status = match update.status.as_deref() {
        Some("active") => TaskStatus::InProgress,
        Some("complete") => TaskStatus::Completed,
        _ => return Err(()),
    };
    let at = stamp(line);
    ops.push(IngestOp::Task(TaskRow {
        id: ids::task_id(&ctx.session_id, native_id),
        session_id: ctx.session_id.clone(),
        status,
        content,
        created_at: None,
        updated_at: at,
        started_at: (status == TaskStatus::InProgress).then_some(at).flatten(),
        terminal_at: matches!(
            status,
            TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Cancelled
        )
        .then_some(at)
        .flatten(),
    }));
    Ok(())
}

fn task_backgrounded(
    ctx: &NormalizeCtx,
    line: &UpdateLine,
    ops: &mut Vec<IngestOp>,
) -> Result<(), ()> {
    let update = &line.params.update;
    let native_id = update.task_id.as_deref().ok_or(())?;
    let content = update.description.clone().ok_or(())?;
    let at = stamp(line);
    ops.push(IngestOp::Task(TaskRow {
        id: ids::task_id(&ctx.session_id, native_id),
        session_id: ctx.session_id.clone(),
        status: TaskStatus::InProgress,
        content,
        created_at: at,
        updated_at: at,
        started_at: at,
        terminal_at: None,
    }));
    Ok(())
}

fn task_completed(
    ctx: &NormalizeCtx,
    line: &UpdateLine,
    ops: &mut Vec<IngestOp>,
) -> Result<(), ()> {
    let snapshot = line
        .params
        .update
        .task_snapshot
        .as_ref()
        .and_then(Value::as_object)
        .ok_or(())?;
    let native_id = snapshot.get("task_id").and_then(Value::as_str).ok_or(())?;
    let content = snapshot
        .get("description")
        .and_then(Value::as_str)
        .ok_or(())?
        .to_owned();
    let completed = snapshot
        .get("completed")
        .and_then(Value::as_bool)
        .ok_or(())?;
    let started_at = snapshot.get("start_time").and_then(system_time_ms);
    let terminal_at = snapshot
        .get("end_time")
        .and_then(system_time_ms)
        .or_else(|| stamp(line));
    ops.push(IngestOp::Task(TaskRow {
        id: ids::task_id(&ctx.session_id, native_id),
        session_id: ctx.session_id.clone(),
        status: if completed {
            TaskStatus::Completed
        } else {
            TaskStatus::InProgress
        },
        content,
        created_at: started_at,
        updated_at: terminal_at,
        started_at,
        terminal_at: completed.then_some(terminal_at).flatten(),
    }));
    Ok(())
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

fn envelope_meta(line: &UpdateLine) -> Result<&serde_json::Map<String, Value>, ()> {
    line.params
        .meta
        .as_ref()
        .and_then(Value::as_object)
        .ok_or(())
}

fn event_id(line: &UpdateLine) -> Result<&str, ()> {
    string(envelope_meta(line)?, "eventId")
}

fn string<'a>(map: &'a serde_json::Map<String, Value>, key: &str) -> Result<&'a str, ()> {
    map.get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or(())
}

fn stamp(line: &UpdateLine) -> Option<Ms> {
    line.params
        .meta
        .as_ref()
        .and_then(|meta| value_ms(&meta["agentTimestampMs"]))
        .or_else(|| seconds_ms(line.timestamp))
}

fn stamp_value(value: &Value) -> Option<Ms> {
    value["params"]["_meta"]["agentTimestampMs"]
        .as_i64()
        .or_else(|| {
            value["params"]["_meta"]["agentTimestampMs"]
                .as_u64()
                .and_then(|value| i64::try_from(value).ok())
        })
        .or_else(|| value["timestamp"].as_i64()?.checked_mul(1_000))
}

fn integer(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
}

fn number(value: &Value, key: &str) -> Option<i64> {
    integer(value.get(key)?)
}

fn value_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Array(_) | Value::Object(_) => serde_json::to_string(value).ok(),
        _ => None,
    }
}

fn update_kind(value: &Value) -> Option<&str> {
    value["params"]["update"]["sessionUpdate"].as_str()
}

fn parse_failure(ctx: &NormalizeCtx, stream_id: &str, text: &str) -> LineOutcome {
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        return hard(ctx, stream_id, INVALID_JSON, None, &[]);
    };
    let kind = update_kind(&value).unwrap_or(UNTYPED);
    if spec::declared_update_types().contains(kind) {
        hard(ctx, stream_id, kind, stamp_value(&value), &keys(&value))
    } else {
        unknown(ctx, stream_id, kind, stamp_value(&value), &keys(&value))
    }
}

fn unknown(
    ctx: &NormalizeCtx,
    stream_id: &str,
    kind: &str,
    at: Option<Ms>,
    paths: &[String],
) -> LineOutcome {
    LineOutcome {
        ops: vec![drift(ctx, stream_id, kind, UNKNOWN_TYPE, paths, at)],
        status: LineStatus::UnknownType,
    }
}

fn hard(
    ctx: &NormalizeCtx,
    stream_id: &str,
    kind: &str,
    at: Option<Ms>,
    paths: &[String],
) -> LineOutcome {
    LineOutcome {
        ops: vec![drift(ctx, stream_id, kind, PARSE_FAILED, paths, at)],
        status: LineStatus::HardError {
            code: PARSE_FAILED.to_owned(),
        },
    }
}

fn hard_into(
    ctx: &NormalizeCtx,
    stream_id: &str,
    kind: &str,
    at: Option<Ms>,
    ops: &mut Vec<IngestOp>,
) -> LineStatus {
    ops.push(drift(ctx, stream_id, kind, PARSE_FAILED, &[], at));
    LineStatus::HardError {
        code: PARSE_FAILED.to_owned(),
    }
}

fn drift(
    ctx: &NormalizeCtx,
    stream_id: &str,
    kind: &str,
    code: &str,
    paths: &[String],
    at: Option<Ms>,
) -> IngestOp {
    IngestOp::Drift(DriftFact {
        runtime_id: ctx.runtime_id.clone(),
        agent_version: ctx.agent_version.clone().unwrap_or_default(),
        stream: stream_id.to_owned(),
        record_kind: kind.to_owned(),
        error_code: code.to_owned(),
        shape_hash: ids::shape_hash(paths),
        at,
        sample_cursor: ctx.src_cursor,
        sample_offset: ctx.src_offset,
    })
}

fn keys(value: &Value) -> Vec<String> {
    value
        .as_object()
        .map(|object| object.keys().cloned().collect())
        .unwrap_or_else(|| vec!["<non-object>".to_owned()])
}

#[cfg(test)]
mod tests {
    use rosetta_adapter_support::adapter::{LineStatus, NormalizeCtx};
    use rosetta_core::{IngestOp, ids};

    use super::{GrokState, ingest_line};
    use crate::spec;

    fn ctx() -> NormalizeCtx {
        let mut ctx = NormalizeCtx::new("runtime", "session");
        ctx.native_session_id = Some("parent".into());
        ctx
    }

    #[test]
    fn declared_unprojected_updates_are_parsed_no_ops() {
        let mut ctx = ctx();
        let mut state = GrokState::default();
        let line = br#"{"timestamp":1,"method":"session/update","params":{"sessionId":"s","_meta":{"eventId":"e","agentTimestampMs":1000},"update":{"sessionUpdate":"hook_execution"}}}"#;
        let outcome = ingest_line(&mut ctx, &mut state, spec::UPDATES, line);
        assert_eq!(outcome.status, LineStatus::Parsed);
        assert!(outcome.ops.is_empty());
    }

    #[test]
    fn undeclared_updates_are_soft_drift() {
        let mut ctx = ctx();
        let mut state = GrokState::default();
        let line = br#"{"timestamp":1,"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"future_update"}}}"#;
        let outcome = ingest_line(&mut ctx, &mut state, spec::UPDATES, line);
        assert_eq!(outcome.status, LineStatus::UnknownType);
        assert!(matches!(outcome.ops.as_slice(), [IngestOp::Drift(_)]));
    }

    #[test]
    fn known_update_shape_failure_is_hard_drift() {
        let mut ctx = ctx();
        let mut state = GrokState::default();
        let line = br#"{"timestamp":1,"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"tool_call"}}}"#;
        let outcome = ingest_line(&mut ctx, &mut state, spec::UPDATES, line);
        assert!(matches!(outcome.status, LineStatus::HardError { .. }));
        assert!(matches!(outcome.ops.as_slice(), [IngestOp::Drift(_)]));
    }

    #[test]
    fn exact_spawn_description_links_the_delegate_to_the_child() {
        let mut ctx = ctx();
        let mut state = GrokState::default();
        let call = br#"{"timestamp":1,"method":"session/update","params":{"sessionId":"parent","_meta":{"eventId":"e1","promptId":"p1","agentTimestampMs":1000},"update":{"sessionUpdate":"tool_call","toolCallId":"call-1","title":"spawn_subagent","rawInput":{"description":"map store","prompt":"inspect the store"}}}}"#;
        let spawned = br#"{"timestamp":2,"method":"session/update","params":{"sessionId":"parent","_meta":{"eventId":"e2","promptId":"p1","agentTimestampMs":2000},"update":{"sessionUpdate":"subagent_spawned","description":"map store","child_session_id":"child","parent_session_id":"parent","parent_prompt_id":"p1","subagent_id":"child"}}}"#;

        let call = ingest_line(&mut ctx, &mut state, spec::UPDATES, call);
        assert_eq!(call.status, LineStatus::Parsed);
        let spawned = ingest_line(&mut ctx, &mut state, spec::UPDATES, spawned);
        assert_eq!(spawned.status, LineStatus::Parsed);
        let expected = ids::session_id("runtime", "child");
        assert!(spawned.ops.iter().any(|op| matches!(
            op,
            IngestOp::ToolUpdate { correlation_key, patch, .. }
                if correlation_key == "call-1"
                    && patch.delegate_child_session_id.as_deref() == Some(expected.as_str())
        )));
    }
}
