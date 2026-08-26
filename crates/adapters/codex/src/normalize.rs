//! Rollout records → canonical ops.
//!
//! Every projection here cites the ground-truth path it rests on
//! (`groundtruth/codex/groundtruth.json`, stream `rollout`). The rule for the
//! two open payload vocabularies is in [`crate::spec::declared`]: declared and
//! unprojected is silence, undeclared is drift.
//!
//! Pure by construction — no IO, no clock, no randomness. Ids are deterministic
//! blake3 over the session and the envelope `ordinal`, so replaying a file
//! produces byte-identical ops.

use rosetta_adapter_support::Drift;
use rosetta_adapter_support::adapter::{LineOutcome, LineStatus, NormalizeCtx};
use rosetta_core::rows::{
    DriftFact, EditKind, FileEditRow, MessageKind, MessageRow, Ms, SessionRole, SessionRow,
    StreamState, TokenSnapshot, ToolCall, WorkspaceObsRow,
};
use rosetta_core::{IngestOp, fold, ids};
use serde_json::Value;

use crate::raw::rollout::{
    ContentBlock, EventMsg, ResponseItem, RolloutLine, RolloutPayload, SessionMeta, TurnContext,
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
#[derive(Debug, Default)]
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
    // The envelope ordinal is the only within-session key codex guarantees:
    // rollout records carry no uuid, and payload ids repeat across records
    // (an AgentMessage `item_completed` reuses the response item's id).
    let discriminator = format!("rollout#{}", record.ordinal);

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
            response_item(ctx, state, stream_id, item, &discriminator, ts, ops)
        }
        RolloutPayload::EventMsg(event) => event_msg(ctx, state, stream_id, event, ts, ops),
        // Declared and unprojected: the world-state snapshot is a full copy of
        // the prompt context, and its cwd is already carried by turn_context.
        RolloutPayload::WorldState(_) => LineStatus::Parsed,
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

    ops.push(IngestOp::SessionMeta(SessionRow {
        id: ctx.session_id.clone(),
        native_session_id: meta.id.clone(),
        runtime_id: ctx.runtime_id.clone(),
        // Left to the pipeline: `workspace_obs` is the documented input to
        // project assignment, so the adapter reports cwd and claims nothing.
        project_id: None,
        parent_session_id: None,
        root_session_id: ctx.session_id.clone(),
        // The canonical observes one `thread_source` (`user`) at 0.149.1 — no
        // evidence yet for what a spawned thread looks like.
        role: SessionRole::Main,
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
    let settings = &turn.collaboration_mode["settings"];
    // The collaboration mode's setting is the effective one; payload.model is
    // the fallback the canonical marks required.
    state.model = settings["model"]
        .as_str()
        .map(str::to_owned)
        .or_else(|| Some(turn.model.clone()));
    state.effort = settings["reasoning_effort"].as_str().map(str::to_owned);

    if state.cwd.as_deref() != Some(turn.cwd.as_str()) {
        state.cwd = Some(turn.cwd.clone());
        ops.push(IngestOp::WorkspaceObs(WorkspaceObsRow {
            session_id: ctx.session_id.clone(),
            at: ts.unwrap_or_default(),
            cwd: turn.cwd.clone(),
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
            row.subtype = subtype;
            row.uuid = Some(item.id.clone());
            row.content = block_text(item.content.as_deref());
            if kind == MessageKind::Agent {
                stamp_model(&mut row, state);
                begin_response(state, &row.timeline_id);
            }
            ops.push(IngestOp::Message(Box::new(row)));
            LineStatus::Parsed
        }
        // NOT fixture-backed at 0.149.1 — the canonical's `response-items` facet
        // has no `reasoning` yet. Kept because the flattened taxonomy names the
        // codex reasoning item as its thinking row (docs/canonical-model.html):
        // when the corpus grows one, it projects instead of vanishing.
        "reasoning" => {
            let mut row = row(ctx, discriminator, MessageKind::Thinking, ts);
            row.uuid = Some(item.id.clone());
            row.content = block_text(item.content.as_deref()).or_else(|| summary_text(item));
            stamp_model(&mut row, state);
            begin_response(state, &row.timeline_id);
            ops.push(IngestOp::Message(Box::new(row)));
            LineStatus::Parsed
        }
        "function_call" => {
            let Some(name) = item.name.clone() else {
                // The canonical marks `name` required; a call without one cannot
                // become a tool row at all.
                return hard_into(
                    ctx,
                    stream_id,
                    "response_item/function_call",
                    "missing_name",
                    ts,
                    ops,
                );
            };
            let input = tool_input(item.arguments.as_deref());
            let mut row = row(ctx, discriminator, MessageKind::ToolUse, ts);
            row.uuid = Some(item.id.clone());
            stamp_model(&mut row, state);
            row.tool = Some(ToolCall {
                tool_id: item.call_id.clone(),
                name: name.clone(),
                input: input.clone(),
                // Merged in later by the writer, keyed on tool_id.
                result: None,
                payload: None,
                is_error: None,
            });
            // A tool call IS a response: the model produced it, and the
            // token_count that follows belongs to it.
            begin_response(state, &row.timeline_id);
            let timeline_id = row.timeline_id.clone();
            ops.push(IngestOp::Message(Box::new(row)));

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
            if item.call_id.is_none() {
                ops.push(drift(
                    ctx,
                    stream_id,
                    "response_item/function_call",
                    "missing_call_id",
                    &[],
                    ts,
                ));
            }
            LineStatus::Parsed
        }
        "function_call_output" => {
            let Some(tool_id) = item.call_id.clone() else {
                // Without the correlation id the result can never reach its call.
                return hard_into(
                    ctx,
                    stream_id,
                    "response_item/function_call_output",
                    "missing_call_id",
                    ts,
                    ops,
                );
            };
            let output = item.output.clone();
            ops.push(IngestOp::ToolResult {
                session_id: ctx.session_id.clone(),
                tool_id,
                // The canonical types `output` as a bare string at 0.149.1 and
                // declares no status field anywhere on the record — so success
                // stays UNKNOWN rather than being sniffed out of the text.
                payload: output.as_deref().and_then(structured),
                result: output,
                is_error: None,
                at: ts,
            });
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

/// Lifecycle and telemetry events. Only `token_count` projects.
fn event_msg(
    ctx: &mut NormalizeCtx,
    state: &mut CodexState,
    stream_id: &str,
    event: &EventMsg,
    ts: Option<Ms>,
    ops: &mut Vec<IngestOp>,
) -> LineStatus {
    if event.event_type != "token_count" {
        return undeclared(
            ctx,
            stream_id,
            spec::declared_event_subtypes().contains(&event.event_type),
            &format!("event_msg/{}", event.event_type),
            ts,
            ops,
        );
    }
    token_count(ctx, state, event, ts, ops);
    LineStatus::Parsed
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

/// Reasoning items keep their visible text under `summary`, which the raw type
/// does not declare (no fixture evidence) — so it is read out of the permissive
/// flatten map instead, where an undeclared field legitimately lands.
fn summary_text(item: &ResponseItem) -> Option<String> {
    let joined = item
        .unknown
        .get("summary")?
        .as_array()?
        .iter()
        .filter_map(|entry| entry["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    (!joined.is_empty()).then_some(joined)
}

/// `arguments` is a JSON-encoded STRING (canonical: `function_call.arguments`).
/// A body that will not parse is kept verbatim rather than dropped.
fn tool_input(arguments: Option<&str>) -> Value {
    let Some(arguments) = arguments else {
        return Value::Null;
    };
    serde_json::from_str(arguments).unwrap_or_else(|_| Value::String(arguments.to_owned()))
}

/// Keep a structured tool result structured; plain text stays in `result` alone
/// rather than being duplicated into the payload.
fn structured(output: &str) -> Option<Value> {
    match serde_json::from_str(output) {
        Ok(value @ (Value::Object(_) | Value::Array(_))) => Some(value),
        _ => None,
    }
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
    use super::{APPLY_PATCH, patch_body, patch_edits, structured, tool_input};
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
            tool_input(Some(r#"{"cmd":"echo rosetta-mock"}"#)),
            json!({"cmd": "echo rosetta-mock"})
        );
        // A body that will not parse is kept verbatim, never dropped.
        assert_eq!(tool_input(Some("not json")), json!("not json"));
        assert_eq!(tool_input(None), Value::Null);
    }

    #[test]
    fn only_structured_output_becomes_a_payload() {
        assert_eq!(structured(r#"{"ok":true}"#), Some(json!({"ok": true})));
        // Plain text lives in `result`; duplicating it into the payload would
        // just be the same bytes twice.
        assert_eq!(structured("Process exited with code 0"), None);
        assert_eq!(structured("42"), None);
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
