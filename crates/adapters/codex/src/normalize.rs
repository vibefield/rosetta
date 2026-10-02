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

use std::collections::BTreeMap;

use rosetta_adapter_support::Drift;
use rosetta_adapter_support::adapter::{LineOutcome, LineStatus, NormalizeCtx};
use rosetta_adapter_support::tool::{
    projection_record_kind, projection_shape_paths, terminal_update, unknown_interaction,
};
use rosetta_core::rows::{
    DriftFact, EditKind, FileEditRow, MessageKind, MessageRow, Ms, ServiceTier,
};
use rosetta_core::timeline::{
    CorrelationDerivation, FileChangeOp, ShellOutcome, StatusBasis, ToolAction, ToolOutcomePatch,
    ToolStatus, ToolStatusFact, ToolTiming, ToolUpdatePatch,
};
use rosetta_core::{IngestOp, ids};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::raw::rollout::{
    Compacted, ContentBlock, EventMsg, InterAgentCommunication, RealtimeItem, ResponseItem,
    RolloutLine, RolloutPayload, SessionMeta, TurnContext,
};
use crate::raw::{ParsedLine, parse_line};
use crate::spec;
use crate::time::rfc3339_ms;
use crate::tool::TransportCall;
use crate::usage::{
    RawUsage, ReplayGate, UntieredRequest, begin_response, billable_search, push_response,
    thread_settings_applied, token_count, token_usage_record, usage_only,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CanonicalCall {
    correlation_key: String,
    action: ToolAction,
    started_at: Option<Ms>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum PendingCall {
    Canonical { call: Box<CanonicalCall> },
    CanonicalBatch { calls: Vec<CanonicalCall> },
    PollProcesses { process_ids: Vec<String> },
    WaitCell { cell_id: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum CellRoute {
    Canonical { call: Box<CanonicalCall> },
    CanonicalBatch { calls: Vec<CanonicalCall> },
    PollProcesses { process_ids: Vec<String> },
}

/// Per-file adapter state ([`NormalizeCtx::state`]).
///
/// `model`/`effort` are carried forward because codex stamps them on
/// `turn_context`, not on the rows they describe; `response` and
/// `response_total` are the machinery for the cumulative token counters.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct CodexState {
    /// Internal approval review is retained only in the raw ledger: no rows,
    /// assets or drift. Its usage still reaches `parent_session` when the
    /// header proves one. This bit survives live-tail checkpoints so resumed
    /// suffixes stay unprojected.
    #[serde(default)]
    pub(crate) ledger_only: bool,
    /// The session that ran a guardian review, as a canonical session id, when
    /// its header names the parent thread (§4.1.3 rule 5).
    #[serde(default)]
    pub(crate) parent_session: Option<String>,
    /// The file's native thread id from its first header: the owner that a
    /// per-response usage record must name to be this file's own.
    #[serde(default)]
    pub(crate) thread_id: Option<String>,
    /// A copied prefix can repeat the parent's header; only the first header
    /// configures the file.
    #[serde(default)]
    header_seen: bool,
    /// Current execution cwd used only to resolve relative tool arguments.
    /// The canonical session cwd is selected independently during meta ingest.
    tool_cwd: Option<String>,
    pub(crate) model: Option<String>,
    effort: Option<String>,
    /// The latest surviving assistant row created since the previous token
    /// count: the request that count reports produced it.
    #[serde(default)]
    pub(crate) request_row: Option<String>,
    /// Server-side web searches the current request ran.
    #[serde(default)]
    pub(crate) request_searches: i64,
    /// The last `total_token_usage` reading of THIS file.
    #[serde(default)]
    pub(crate) prev_total: Option<RawUsage>,
    #[serde(default)]
    pub(crate) replay_gate: ReplayGate,
    /// Token counts seen in this file; the stable key of a request sample.
    #[serde(default)]
    pub(crate) token_counts: u64,
    /// The configured tier from the latest `thread_settings_applied`.
    #[serde(default)]
    pub(crate) service_tier: Option<ServiceTier>,
    /// Whether a settings snapshot has been seen; until then requests are
    /// held in `untiered` for the tier the first snapshot will name.
    #[serde(default)]
    pub(crate) tier_settled: bool,
    #[serde(default)]
    pub(crate) untiered: Vec<UntieredRequest>,
    /// The usage of the latest own `token_usage_record`, which the next
    /// `token_count` repeats.
    #[serde(default)]
    pub(crate) last_record_usage: Option<RawUsage>,
    /// `session_meta.model_provider` when it is not Codex's default `openai`.
    #[serde(default)]
    pub(crate) route: Option<String>,
    /// One-record-lived tool requests, plus the rare outer executor cell that
    /// yielded before its real result was ready. These maps are adapter state,
    /// never canonical data; they disappear as soon as the matching output is
    /// reduced.
    #[serde(default)]
    pending_calls: BTreeMap<String, PendingCall>,
    #[serde(default)]
    yielded_cells: BTreeMap<String, CellRoute>,
    /// Unified exec sessions point back to the command that created them. Poll
    /// calls can therefore update that command's lifecycle without becoming
    /// hundreds of meaningless transcript rows themselves.
    #[serde(default)]
    running_processes: BTreeMap<String, CanonicalCall>,
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
        if state.ledger_only {
            return LineOutcome::parsed(Vec::new());
        }
        return hard(ctx, stream_id, "<binary>", "invalid_utf8", &[], None);
    };
    // Rollout files are append-only; a torn write shows up as a blank line.
    if text.trim().is_empty() {
        return LineOutcome::parsed(Vec::new());
    }
    if state.ledger_only {
        return usage_only(ctx, state, stream_id, text);
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
    if let RolloutPayload::SessionMeta(meta) = &record.payload
        && !state.header_seen
        && crate::session_policy::ledger_only(&meta.source, meta.thread_source.as_deref())
    {
        state.ledger_only = true;
        // The header still configures the file: its usage facts need the
        // model provider, the replay gate and, above all, the parent.
        session_meta(ctx, state, meta, ts);
        return LineOutcome::parsed(Vec::new());
    }

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
            session_meta(ctx, state, meta, ts);
            LineStatus::Parsed
        }
        RolloutPayload::TurnContext(turn) => {
            turn_context(state, turn);
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
        RolloutPayload::EventMsg(event) => {
            event_msg(ctx, state, stream_id, event, record.ordinal, ts, ops)
        }
        RolloutPayload::TokenUsageRecord(item) => {
            token_usage_record(ctx, state, item, record.ordinal, ts, ops);
            LineStatus::Parsed
        }
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
        // prompt context. Working-directory metadata was settled by the skim.
        RolloutPayload::WorldState(_)
        | RolloutPayload::InterAgentCommunicationMetadata(_)
        | RolloutPayload::SecurityRiskScore(_) => LineStatus::Parsed,
    }
}

/// Session identity, topology, and canonical cwd are admitted from this same
/// opening record by `CodexAdapter::skim`. Streaming retains only provenance
/// and execution context needed to normalize later tools. Only the file's
/// first header counts: a paginated child's inherited prefix repeats the
/// parent's header at ordinal 1, and that copy must not reconfigure the
/// child (CODEX-8).
fn session_meta(
    ctx: &mut NormalizeCtx,
    state: &mut CodexState,
    meta: &SessionMeta,
    ts: Option<Ms>,
) {
    if state.header_seen {
        return;
    }
    state.header_seen = true;
    ctx.agent_version = Some(meta.cli_version.clone());
    state.thread_id = Some(meta.id.clone());
    state.tool_cwd = Some(meta.cwd.clone());
    state.route = (meta.model_provider != "openai")
        .then(|| rosetta_core::rows::canonical_route(&meta.model_provider))
        .flatten();
    state.replay_gate = ReplayGate::for_session(meta, ts);
    // A guardian review's usage belongs to the thread that ran it. The
    // header's `parent_thread_id` is the only proof; without it the review
    // stays entirely in the raw ledger.
    state.parent_session = state
        .ledger_only
        .then_some(meta.parent_thread_id.as_deref())
        .flatten()
        .map(|parent| ids::session_id(&ctx.runtime_id, parent));
}

/// Per-turn execution context used to stamp rows and resolve relative tool
/// paths. It does not revise the session metadata selected during admission.
pub(crate) fn turn_context(state: &mut CodexState, turn: &TurnContext) {
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

    if let Some(cwd) = turn.cwd.as_deref() {
        state.tool_cwd = Some(cwd.to_owned());
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
            let content = block_text(item.content.as_deref());
            let (kind, subtype) = match item.role.as_deref() {
                // Codex records its generated workspace/date/permission snapshot
                // as a user-role message. It is prompt context, not human prose.
                Some("user")
                    if content
                        .as_deref()
                        .is_some_and(is_injected_environment_context) =>
                {
                    (MessageKind::System, Some("environment_context".to_owned()))
                }
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
            row.content = content;
            if matches!(kind, MessageKind::User | MessageKind::Agent) {
                let (text, attachments, assets) =
                    crate::images::authored_content(item.content.as_deref().unwrap_or_default());
                row.content = text;
                row.attachments = attachments;
                ops.extend(
                    assets
                        .into_iter()
                        .map(|asset| IngestOp::ProjectionAsset { asset }),
                );
            }
            if kind == MessageKind::Agent {
                stamp_model(&mut row, state);
                push_response(state, row, ops);
            } else {
                IngestOp::push_message(ops, row);
            }
            LineStatus::Parsed
        }
        "reasoning" => {
            let mut row = row(ctx, discriminator, MessageKind::Thinking, ts);
            row.uuid = item.id.clone();
            row.content =
                block_text(item.summary.as_deref()).or_else(|| block_text(item.content.as_deref()));
            stamp_model(&mut row, state);
            push_response(state, row, ops);
            LineStatus::Parsed
        }
        "agent_message" => {
            let mut row = row(ctx, discriminator, MessageKind::Agent, ts);
            row.uuid = item.id.clone();
            row.subtype = Some("agent_message".to_owned());
            row.content = block_text(item.content.as_deref());
            stamp_model(&mut row, state);
            push_response(state, row, ops);
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

            // `functions.exec` can yield its own cell, and an empty
            // `write_stdin` is merely a poll of a command already represented
            // by its original shell row. Keep those routing addresses only in
            // adapter state. Their outputs will update the original command;
            // neither layer deserves another transcript row.
            if let Some(transport) = crate::tool::transport_call(&name, &input) {
                let pending = match transport {
                    TransportCall::WaitCell(cell_id) => PendingCall::WaitCell { cell_id },
                    TransportCall::PollProcesses(process_ids) => {
                        PendingCall::PollProcesses { process_ids }
                    }
                };
                state.pending_calls.insert(tool_id, pending);
                return LineStatus::Parsed;
            }

            if name == "exec"
                && let Some(children) =
                    crate::tool::project_exec_batch(&input, state.tool_cwd.as_deref(), &tool_id)
            {
                let mut calls = Vec::with_capacity(children.len());
                let mut last_timeline_id = None;
                for (index, tool) in children.into_iter().enumerate() {
                    let child_discriminator = format!("{discriminator}/child#{index}");
                    let mut child = row(ctx, &child_discriminator, MessageKind::ToolUse, ts);
                    child.uuid = item.id.clone();
                    stamp_model(&mut child, state);
                    let correlation_key = tool.correlation.key.clone();
                    calls.push(CanonicalCall {
                        correlation_key,
                        action: tool.action.clone(),
                        started_at: ts,
                    });
                    let file_edits = projected_file_edits(&tool.action);
                    child.tool = Some(tool);
                    let timeline_id = child.timeline_id.clone();
                    last_timeline_id = Some(timeline_id.clone());
                    IngestOp::push_message(ops, child);
                    for edit in file_edits {
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
                }
                state
                    .pending_calls
                    .insert(tool_id, PendingCall::CanonicalBatch { calls });
                if let Some(timeline_id) = last_timeline_id {
                    begin_response(state, &timeline_id);
                }
                return LineStatus::Parsed;
            }

            let mut row = row(ctx, discriminator, MessageKind::ToolUse, ts);
            row.uuid = item.id.clone();
            stamp_model(&mut row, state);
            let projected = crate::tool::project_request(
                ctx.agent_version.as_deref(),
                &name,
                &input,
                state.tool_cwd.as_deref(),
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
            let file_edits = row
                .tool
                .as_ref()
                .map(|tool| projected_file_edits(&tool.action))
                .unwrap_or_default();
            if let Some(tool) = row.tool.as_ref() {
                state.pending_calls.insert(
                    tool_id.clone(),
                    PendingCall::Canonical {
                        call: Box::new(CanonicalCall {
                            correlation_key: tool_id,
                            action: tool.action.clone(),
                            started_at: ts,
                        }),
                    },
                );
            }
            // A tool call IS a response: the model produced it, and the
            // token_count that follows belongs to it.
            let timeline_id = row.timeline_id.clone();
            push_response(state, row, ops);

            for edit in file_edits {
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
            reduce_tool_output(ctx, state, item, &tool_id, ts, ops);
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
                state.tool_cwd.as_deref(),
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
            push_response(state, row, ops);
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
            if billable_search(item) {
                state.request_searches += 1;
            }
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
                state.tool_cwd.as_deref(),
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
            if name == "image_generation_call"
                && let Some(data) = item.result.as_deref()
            {
                let (attachment, asset) =
                    rosetta_adapter_support::media::from_image_base64("image-0", data);
                tool.attachments.push(attachment);
                ops.extend(asset.map(|asset| IngestOp::ProjectionAsset { asset }));
            }
            row.tool = Some(tool);
            push_response(state, row, ops);
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

/// Reduce Codex's two execution layers into the semantic call the transcript
/// already owns. A yielded outer cell and an empty process poll are routing,
/// not actions; structured unified-exec results are the native evidence that
/// can enrich the original shell row with its process id and final exit code.
fn reduce_tool_output(
    ctx: &NormalizeCtx,
    state: &mut CodexState,
    item: &ResponseItem,
    tool_id: &str,
    ts: Option<Ms>,
    ops: &mut Vec<IngestOp>,
) {
    let route = match state.pending_calls.remove(tool_id) {
        Some(PendingCall::Canonical { call }) => Some(CellRoute::Canonical { call }),
        Some(PendingCall::CanonicalBatch { calls }) => Some(CellRoute::CanonicalBatch { calls }),
        Some(PendingCall::PollProcesses { process_ids }) => {
            Some(CellRoute::PollProcesses { process_ids })
        }
        Some(PendingCall::WaitCell { cell_id }) => state.yielded_cells.remove(&cell_id),
        None => None,
    };

    let Some(route) = route else {
        // A partial source can begin at an output record. Preserve the old
        // correlation behavior in that case; a transport output with no
        // opener simply remains an orphan and never becomes a message.
        push_update(ctx, tool_id, terminal_patch(item, ts), ts, ops);
        crate::images::tool_output(ctx, tool_id, item.output.as_ref(), ts, ops);
        return;
    };

    if let CellRoute::Canonical { call } = &route {
        crate::images::tool_output(ctx, &call.correlation_key, item.output.as_ref(), ts, ops);
    }

    if let Some(cell_id) = yielded_cell_id(item.output.as_ref()) {
        state.yielded_cells.insert(cell_id, route);
        return;
    }

    let execution = execution_results(item.output.as_ref());
    match route {
        CellRoute::Canonical { call } => {
            reduce_canonical_output(ctx, state, item, *call, &execution, ts, ops)
        }
        CellRoute::CanonicalBatch { calls } => {
            reduce_canonical_batch(ctx, state, item, calls, &execution, ts, ops)
        }
        CellRoute::PollProcesses { process_ids } => {
            reduce_process_poll(ctx, state, item, &process_ids, &execution, ts, ops)
        }
    }
}

fn reduce_canonical_batch(
    ctx: &NormalizeCtx,
    state: &mut CodexState,
    item: &ResponseItem,
    calls: Vec<CanonicalCall>,
    execution: &[ExecutionResult],
    ts: Option<Ms>,
    ops: &mut Vec<IngestOp>,
) {
    let aligned = execution.len() == calls.len();
    for (index, call) in calls.into_iter().enumerate() {
        let result = aligned.then(|| execution.get(index)).flatten();
        reduce_canonical_output(
            ctx,
            state,
            item,
            call,
            result.map(std::slice::from_ref).unwrap_or_default(),
            ts,
            ops,
        );
    }
}

fn reduce_canonical_output(
    ctx: &NormalizeCtx,
    state: &mut CodexState,
    item: &ResponseItem,
    call: CanonicalCall,
    execution: &[ExecutionResult],
    ts: Option<Ms>,
    ops: &mut Vec<IngestOp>,
) {
    if matches!(
        call.action,
        ToolAction::Shell { .. } | ToolAction::CodeExecute { .. }
    ) && let Some(result) = execution.first()
    {
        if result.exit_code.is_some() || result.signal.is_some() {
            let patch = completed_command_patch(&call, result, None, ts);
            push_update(ctx, &call.correlation_key, patch, ts, ops);
            return;
        }
        if let Some(process_id) = result.process_id.as_ref() {
            state
                .running_processes
                .insert(process_id.clone(), call.clone());
            let patch = running_command_patch(&call, process_id);
            push_update(ctx, &call.correlation_key, patch, ts, ops);
            return;
        }
    }

    push_update(
        ctx,
        &call.correlation_key,
        terminal_patch(item, ts),
        ts,
        ops,
    );
}

fn reduce_process_poll(
    ctx: &NormalizeCtx,
    state: &mut CodexState,
    item: &ResponseItem,
    process_ids: &[String],
    execution: &[ExecutionResult],
    ts: Option<Ms>,
    ops: &mut Vec<IngestOp>,
) {
    let failed_without_result = status_error(item.status.as_deref()) == Some(true);
    for (index, process_id) in process_ids.iter().enumerate() {
        let result = execution
            .iter()
            .find(|result| result.process_id.as_deref() == Some(process_id))
            .or_else(|| (execution.len() == process_ids.len()).then(|| &execution[index]))
            .or_else(|| {
                (process_ids.len() == 1)
                    .then(|| execution.first())
                    .flatten()
            });
        let finished = result
            .is_some_and(|result| result.exit_code.is_some() || result.signal.is_some())
            || failed_without_result;
        if !finished {
            continue;
        }
        let Some(call) = state.running_processes.remove(process_id) else {
            continue;
        };
        let fallback;
        let result = match result {
            Some(result) => result,
            None => {
                fallback = ExecutionResult {
                    process_id: Some(process_id.clone()),
                    exit_code: Some(1),
                    signal: None,
                    diagnostic: tool_output_text(item.output.as_ref()),
                };
                &fallback
            }
        };
        let patch = completed_command_patch(&call, result, Some(process_id), ts);
        push_update(ctx, &call.correlation_key, patch, ts, ops);
    }
}

fn push_update(
    ctx: &NormalizeCtx,
    correlation_key: &str,
    patch: ToolUpdatePatch,
    at: Option<Ms>,
    ops: &mut Vec<IngestOp>,
) {
    ops.push(IngestOp::ToolUpdate {
        session_id: ctx.session_id.clone(),
        correlation_key: correlation_key.to_owned(),
        evidence: ctx.evidence(0),
        patch: Box::new(patch),
        at,
    });
}

fn terminal_patch(item: &ResponseItem, at: Option<Ms>) -> ToolUpdatePatch {
    let is_error = status_error(item.status.as_deref());
    let output = is_error
        .is_some_and(|value| value)
        .then(|| tool_output_text(item.output.as_ref()))
        .flatten();
    terminal_update(
        is_error,
        false,
        StatusBasis::NativeTerminalRecord,
        at,
        output.as_deref(),
    )
}

fn running_command_patch(call: &CanonicalCall, process_id: &str) -> ToolUpdatePatch {
    let outcome = ShellOutcome {
        exit_code: None,
        signal: None,
        process_id: Some(process_id.to_owned()),
    };
    ToolUpdatePatch {
        outcome: outcome_action(&call.action, outcome).map(|action| ToolOutcomePatch { action }),
        status: Some(ToolStatusFact {
            value: ToolStatus::Running,
            basis: StatusBasis::NativeState,
        }),
        timing: call.started_at.map(|started_at| ToolTiming {
            started_at: Some(started_at),
            completed_at: None,
            duration_ms: None,
        }),
        ..ToolUpdatePatch::default()
    }
}

fn completed_command_patch(
    call: &CanonicalCall,
    result: &ExecutionResult,
    routed_process_id: Option<&str>,
    at: Option<Ms>,
) -> ToolUpdatePatch {
    let failed = result.exit_code.is_some_and(|code| code != 0) || result.signal.is_some();
    let process_id = routed_process_id
        .map(str::to_owned)
        .or_else(|| result.process_id.clone());
    let outcome = ShellOutcome {
        exit_code: result.exit_code,
        signal: result.signal.clone(),
        process_id,
    };
    let mut patch = terminal_update(
        Some(failed),
        false,
        StatusBasis::NativeState,
        at,
        failed.then_some(result.diagnostic.as_deref()).flatten(),
    );
    patch.outcome = outcome_action(&call.action, outcome).map(|action| ToolOutcomePatch { action });
    complete_timing(&mut patch, call.started_at, at);
    patch
}

fn outcome_action(action: &ToolAction, outcome_value: ShellOutcome) -> Option<ToolAction> {
    let mut action = action.clone();
    match &mut action {
        ToolAction::Shell { outcome, .. } | ToolAction::CodeExecute { outcome, .. } => {
            *outcome = Some(outcome_value);
            Some(action)
        }
        _ => None,
    }
}

fn complete_timing(patch: &mut ToolUpdatePatch, started_at: Option<Ms>, completed_at: Option<Ms>) {
    let duration_ms = started_at.zip(completed_at).and_then(|(start, end)| {
        end.checked_sub(start)
            .and_then(|duration| u64::try_from(duration).ok())
    });
    patch.timing = Some(ToolTiming {
        started_at,
        completed_at,
        duration_ms,
    });
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ExecutionResult {
    process_id: Option<String>,
    exit_code: Option<i32>,
    signal: Option<String>,
    diagnostic: Option<String>,
}

fn yielded_cell_id(output: Option<&Value>) -> Option<String> {
    let mut found = None;
    visit_output_text(output?, &mut |text| {
        if found.is_some() {
            return;
        }
        let Some(rest) = text.split("Script running with cell ID ").nth(1) else {
            return;
        };
        found = rest
            .split_whitespace()
            .next()
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
    });
    found
}

fn execution_results(output: Option<&Value>) -> Vec<ExecutionResult> {
    let mut results = Vec::new();
    let Some(output) = output else {
        return results;
    };
    visit_execution_values(output, &mut results);
    results
}

fn visit_execution_values(value: &Value, results: &mut Vec<ExecutionResult>) {
    match value {
        Value::Array(values) => {
            for value in values {
                visit_execution_values(value, results);
            }
        }
        Value::Object(map) => {
            let process_id = map.get("session_id").and_then(value_id);
            let exit_code = map
                .get("exit_code")
                .and_then(Value::as_i64)
                .and_then(|value| i32::try_from(value).ok());
            let signal = map
                .get("signal")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_owned);
            let is_unified_result = (map.contains_key("chunk_id")
                || map.contains_key("wall_time_seconds")
                || map.contains_key("original_token_count"))
                && (process_id.is_some() || exit_code.is_some() || signal.is_some());
            if is_unified_result {
                results.push(ExecutionResult {
                    process_id,
                    exit_code,
                    signal,
                    diagnostic: map
                        .get("output")
                        .and_then(Value::as_str)
                        .filter(|value| !value.is_empty())
                        .map(str::to_owned),
                });
                return;
            }
            if let Some(text) = map.get("text").and_then(Value::as_str) {
                visit_embedded_json(text, results);
            }
        }
        Value::String(text) => visit_embedded_json(text, results),
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn visit_embedded_json(text: &str, results: &mut Vec<ExecutionResult>) {
    let trimmed = text.trim();
    let start = trimmed
        .char_indices()
        .find_map(|(index, ch)| matches!(ch, '{' | '[').then_some(index));
    let Some(start) = start else {
        return;
    };
    let mut values = serde_json::Deserializer::from_str(&trimmed[start..]).into_iter::<Value>();
    if let Some(Ok(value)) = values.next() {
        visit_execution_values(&value, results);
    }
}

fn value_id(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::to_owned)
        .or_else(|| value.as_i64().map(|value| value.to_string()))
        .or_else(|| value.as_u64().map(|value| value.to_string()))
}

fn visit_output_text<'a>(value: &'a Value, visit: &mut impl FnMut(&'a str)) {
    match value {
        Value::String(text) => visit(text),
        Value::Array(values) => {
            for value in values {
                visit_output_text(value, visit);
            }
        }
        Value::Object(map) => {
            if let Some(text) = map.get("text").and_then(Value::as_str) {
                visit(text);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
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
    ordinal: Option<u64>,
    ts: Option<Ms>,
    ops: &mut Vec<IngestOp>,
) -> LineStatus {
    match event.event_type.as_str() {
        "token_count" => {
            token_count(ctx, state, event, ordinal, ts, ops);
            LineStatus::Parsed
        }
        "thread_settings_applied" => {
            thread_settings_applied(ctx, state, stream_id, event, ts, ops);
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

fn is_injected_environment_context(text: &str) -> bool {
    let text = text.trim();
    text.strip_prefix("<environment_context>")
        .and_then(|body| body.strip_suffix("</environment_context>"))
        .is_some()
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

pub(crate) fn number(value: &Value, key: &str) -> Option<i64> {
    value.get(key).and_then(Value::as_i64)
}

// ————— canonical file changes → file-edit activity ——————————————————

struct PatchEdit {
    path: String,
    kind: EditKind,
    added: Option<i64>,
    removed: Option<i64>,
}

fn projected_file_edits(action: &ToolAction) -> Vec<PatchEdit> {
    let ToolAction::FileChange { changes } = action else {
        return Vec::new();
    };
    changes
        .iter()
        .map(|change| PatchEdit {
            path: change.path.display_value().to_owned(),
            kind: if change.operation == Some(FileChangeOp::Create) {
                EditKind::Write
            } else {
                EditKind::Edit
            },
            added: change.lines_added.and_then(|value| value.try_into().ok()),
            removed: change.lines_removed.and_then(|value| value.try_into().ok()),
        })
        .collect()
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
        RolloutPayload::TokenUsageRecord(_) => "token_usage_record".to_owned(),
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
    use super::{tool_input, tool_output_text};
    use serde_json::{Value, json};

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
}
