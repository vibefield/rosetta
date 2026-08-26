//! Raw transcript records → canonical ops.
//!
//! The one transform that matters here is the SPLIT: Claude Code packs text,
//! thinking and tool calls into one assistant record's `content` array, and the
//! canonical model wants one row per block (docs/canonical-model.html, "Message
//! rows"). Everything else — tool results folding onto their call, cwd
//! observations, file edits — follows from that flattening.
//!
//! Pure by contract (CLAUDE.md): no IO, no clock, no randomness. Every id is
//! derived from the session id plus a stable discriminator, so replaying a file
//! twice produces byte-identical ops.

use rosetta_adapter_support::{Drift, LineOutcome, LineStatus, NormalizeCtx};
use rosetta_core::rows::{
    DriftFact, EditKind, FileEditRow, MessageKind, MessageRow, Ms, TokenSnapshot, ToolCall,
    WorkspaceObsRow,
};
use rosetta_core::{IngestOp, fold, ids};
use serde_json::Value;

use crate::raw::transcript::{
    AiTitleLine, AssistantContentBlock, AssistantLine, KnownAssistantBlock, KnownRecord,
    KnownUserBlock, RecordedUsage, SystemLine, UserContent, UserContentBlock, UserLine,
};
use crate::raw::{ParsedLine, parse_line};
use crate::spec;
use crate::time::rfc3339_ms;

/// A record `type` the canonical does not declare — the agent changed under us.
const UNKNOWN_TYPE: &str = "unknown_type";
/// A record type we DO know, whose shape moved. The alarm.
const PARSE_FAILED: &str = "parse_failed";
/// Fields the raw types did not expect, on an otherwise good record.
const UNKNOWN_FIELDS: &str = "unknown_fields";

/// Drift grouping keys for records that cannot name themselves. Angle brackets
/// match the canonical file's own sentinel style (`<unmatched>`, `<str>`).
const NO_TYPE: &str = "<none>";
const INVALID_JSON: &str = "<invalid-json>";

/// Tools that spawn a subagent thread: their row is the anchor a child session
/// hangs from. `Agent` is the current name and `Task` the older one — both are
/// live in the observed corpus (census `tool_use_names`: 2,237 and 71).
const SPAWN_TOOLS: [&str; 2] = ["Agent", "Task"];

/// Adapter-private per-file state, created through `AgentAdapter::init_state`.
///
/// Only the last workspace observation: every single record repeats `cwd` and
/// `gitBranch`, and the projection wants transitions, not one row per line.
#[derive(Debug, Default)]
pub struct ClaudeCodeState {
    workspace: Option<(String, String)>,
}

impl ClaudeCodeState {
    /// True when this observation differs from the previous one.
    fn observe(&mut self, cwd: &str, git_branch: &str) -> bool {
        let seen = self
            .workspace
            .as_ref()
            .is_some_and(|(last_cwd, last_branch)| last_cwd == cwd && last_branch == git_branch);
        if !seen {
            self.workspace = Some((cwd.to_owned(), git_branch.to_owned()));
        }
        !seen
    }
}

/// One raw line in, canonical ops out. See [`crate::adapter::ClaudeCodeAdapter`].
pub fn ingest(ctx: &mut NormalizeCtx, stream_id: &str, line: &[u8]) -> LineOutcome {
    let Ok(text) = std::str::from_utf8(line) else {
        return hard_error(ctx, stream_id, INVALID_JSON, None);
    };
    if text.trim().is_empty() {
        return LineOutcome::parsed(Vec::new());
    }

    match parse_line(text) {
        Ok(ParsedLine::Known(record)) => normalize(ctx, stream_id, &record),
        Ok(ParsedLine::UnknownType { rtype, value }) => {
            let kind = rtype.as_deref().unwrap_or(NO_TYPE);
            // A type the canonical declares is handled — we simply project
            // nothing from it. Only an UNDECLARED type is drift, otherwise the
            // most common record in the corpus (`attachment`) would raise the
            // alarm on every ingest.
            if spec::declared_transcript_types().contains(kind) {
                return LineOutcome::parsed(Vec::new());
            }
            let mut emit = Emit::new(ctx, stream_id);
            emit.drift(
                UNKNOWN_TYPE,
                kind,
                top_level_keys(&value),
                record_ts(&value),
                record_version(&value),
            );
            LineOutcome {
                ops: emit.ops,
                status: LineStatus::UnknownType,
            }
        }
        // parse_line fails on malformed JSON, or on a KNOWN type whose required
        // shape moved — that second case is the drift the raw types exist to
        // catch, so it is a hard error rather than a shrug.
        Err(_) => match serde_json::from_str::<Value>(text) {
            Ok(value) => {
                let kind = value["type"].as_str().unwrap_or(NO_TYPE).to_owned();
                hard_error(ctx, stream_id, &kind, Some(&value))
            }
            Err(_) => hard_error(ctx, stream_id, INVALID_JSON, None),
        },
    }
}

fn normalize(ctx: &mut NormalizeCtx, stream_id: &str, record: &KnownRecord) -> LineOutcome {
    let mut emit = Emit::new(ctx, stream_id);
    match record {
        KnownRecord::User(line) => user_line(&mut emit, line),
        KnownRecord::Assistant(line) => assistant_line(&mut emit, line),
        KnownRecord::System(line) => system_line(&mut emit, line),
        KnownRecord::AiTitle(line) => ai_title_line(&mut emit, line),
        // Session bookkeeping the canonical model has no row for: the resume
        // latch, the prompt queue, the last-prompt pointer, the mode marker.
        // None of them carries a uuid or a timestamp, so none could be placed
        // on a timeline even if we wanted it there.
        KnownRecord::AtisLatch(_)
        | KnownRecord::QueueOperation(_)
        | KnownRecord::LastPrompt(_)
        | KnownRecord::Mode(_) => {}
    }

    let paths = record.drift_paths();
    if !paths.is_empty() {
        let (kind, at, version) = record_identity(record);
        emit.drift(UNKNOWN_FIELDS, kind, paths, at, version);
    }
    LineOutcome::parsed(emit.ops)
}

// ---------------------------------------------------------------- record kinds

fn user_line(emit: &mut Emit, line: &UserLine) {
    let ts = rfc3339_ms(&line.timestamp);
    emit.workspace(&line.cwd, &line.git_branch, ts);
    let seed = Seed {
        uuid: Some(&line.uuid),
        parent_uuid: line.parent_uuid.as_deref(),
        ts,
        version: Some(&line.version),
        model: None,
        effort: None,
    };

    let mut text = String::new();
    let mut results: Vec<ToolResultBlock<'_>> = Vec::new();
    let mut has_other_block = false;
    match &line.message.content {
        UserContent::Text(prompt) => text.push_str(prompt),
        UserContent::Blocks(blocks) => {
            for block in blocks {
                match block {
                    UserContentBlock::Known(KnownUserBlock::Text { text: block, .. }) => {
                        push_text(&mut text, block);
                    }
                    UserContentBlock::Known(KnownUserBlock::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                        ..
                    }) => results.push(ToolResultBlock {
                        tool_use_id,
                        content: content.as_ref(),
                        is_error: *is_error,
                    }),
                    // Images, documents and block kinds we do not know: no text
                    // to carry, but the turn itself still happened.
                    _ => has_other_block = true,
                }
            }
        }
    }

    // A turn made only of tool_results is the report-back for a tool call — it
    // folds into that call's row instead of becoming a row of its own.
    if !text.is_empty() || has_other_block || results.is_empty() {
        let mut row = emit.row(&seed, MessageKind::User, &line.uuid);
        row.content = (!text.is_empty()).then_some(text);
        emit.push_row(row);
    }

    // `toolUseResult` is one structured payload per LINE, so it can only be
    // attributed when the line carries a single result — or when the writer
    // named the call it belongs to.
    let payload_target = if results.len() == 1 {
        Some(results[0].tool_use_id)
    } else {
        line.source_tool_use_id.as_deref()
    };
    for result in results {
        let payload = (payload_target == Some(result.tool_use_id))
            .then(|| line.tool_use_result.clone())
            .flatten();
        emit.tool_result(&result, payload, ts);
    }
}

fn assistant_line(emit: &mut Emit, line: &AssistantLine) {
    let ts = rfc3339_ms(&line.timestamp);
    emit.workspace(&line.cwd, &line.git_branch, ts);
    let seed = Seed {
        uuid: Some(&line.uuid),
        parent_uuid: line.parent_uuid.as_deref(),
        ts,
        version: Some(&line.version),
        model: Some(&line.message.model),
        effort: line.effort.as_deref(),
    };

    let mut rows: Vec<(MessageRow, Option<FileEditRow>)> = Vec::new();
    for (index, block) in line.message.content.iter().enumerate() {
        // Split rows share the record's uuid, so the block index is what makes
        // the timeline id unique within the session.
        let discriminator = format!("{}#{index}", line.uuid);
        match block {
            AssistantContentBlock::Known(KnownAssistantBlock::Text { text, .. }) => {
                let mut row = emit.row(&seed, MessageKind::Agent, &discriminator);
                row.content = Some(text.clone());
                rows.push((row, None));
            }
            AssistantContentBlock::Known(KnownAssistantBlock::Thinking { thinking, .. }) => {
                let mut row = emit.row(&seed, MessageKind::Thinking, &discriminator);
                row.content = Some(thinking.clone());
                rows.push((row, None));
            }
            AssistantContentBlock::Known(KnownAssistantBlock::ToolUse {
                id, name, input, ..
            }) => {
                let mut row = emit.row(&seed, MessageKind::ToolUse, &discriminator);
                row.tool = Some(ToolCall {
                    tool_id: Some(id.clone()),
                    name: name.clone(),
                    input: input.clone(),
                    // Both arrive later, on the user line that reports back.
                    result: None,
                    payload: None,
                    is_error: None,
                });
                if SPAWN_TOOLS.contains(&name.as_str()) {
                    row.branch_tool_id = Some(id.clone());
                }
                let edit = file_edit(&row, name, input, ts);
                rows.push((row, edit));
            }
            // `fallback` (an in-response model switch) is not a declared block
            // kind in the canonical and carries no timeline content; unfamiliar
            // kinds land in `Other` and are reported by the drift pass.
            _ => {}
        }
    }

    // A response with nothing projectable still happened — API-error records
    // carry an empty content array — and its usage must not vanish with it.
    if rows.is_empty() {
        let row = emit.row(&seed, MessageKind::Agent, &format!("{}#0", line.uuid));
        rows.push((row, None));
    }
    if let Some(usage) = line.message.usage.as_ref() {
        attach_usage(&mut rows, usage);
    }
    for (row, edit) in rows {
        emit.push_row(row);
        if let Some(edit) = edit {
            emit.ops.push(IngestOp::FileEdit(edit));
        }
    }
}

fn system_line(emit: &mut Emit, line: &SystemLine) {
    let ts = rfc3339_ms(&line.timestamp);
    emit.workspace(&line.cwd, &line.git_branch, ts);
    let seed = Seed {
        uuid: Some(&line.uuid),
        parent_uuid: line.parent_uuid.as_deref(),
        ts,
        version: Some(&line.version),
        model: None,
        effort: None,
    };
    let mut row = emit.row(&seed, MessageKind::System, &line.uuid);
    row.subtype = Some(line.subtype.clone());
    row.content = line.content.clone();
    emit.push_row(row);
}

/// The title is a session singleton the CLI rewrites, and the record carries no
/// uuid to key it by. Every ai-title therefore folds onto ONE row — a later
/// title upserts over the earlier one instead of stacking up.
fn ai_title_line(emit: &mut Emit, line: &AiTitleLine) {
    let seed = Seed {
        uuid: None,
        parent_uuid: None,
        ts: None,
        version: None,
        model: None,
        effort: None,
    };
    let mut row = emit.row(&seed, MessageKind::Summary, "ai-title");
    row.subtype = Some("ai_title".to_owned());
    row.content = Some(line.ai_title.clone());
    emit.push_row(row);
}

// -------------------------------------------------------------------- derived

/// Usage is reported once per response, not per block, so it lands on the row a
/// reader would attribute the response to: the first text row, else the first
/// tool call, else whatever the response did produce.
fn attach_usage(rows: &mut [(MessageRow, Option<FileEditRow>)], usage: &RecordedUsage) {
    let target = rows
        .iter()
        .position(|(row, _)| row.kind == MessageKind::Agent)
        .or_else(|| {
            rows.iter()
                .position(|(row, _)| row.kind == MessageKind::ToolUse)
        })
        .unwrap_or(0);
    if let Some((row, _)) = rows.get_mut(target) {
        row.usage = Some(snapshot(usage));
    }
}

/// Anthropic's `input_tokens` already EXCLUDES the cached read, so the
/// components map across one-to-one — no inclusive-counter split here (that is
/// codex's rule, `fold::split_inclusive_input`).
fn snapshot(usage: &RecordedUsage) -> TokenSnapshot {
    TokenSnapshot {
        input: i64::try_from(usage.input_tokens).ok(),
        output: i64::try_from(usage.output_tokens).ok(),
        cache_read: i64::try_from(usage.cache_read_input_tokens).ok(),
        cache_write: i64::try_from(usage.cache_creation_input_tokens).ok(),
        thinking: usage
            .output_tokens_details
            .as_ref()
            .and_then(|details| details["thinking_tokens"].as_i64()),
    }
}

/// Edit and Write are the file-changing tools. Edit's input shape is canonical
/// (`tool-input` facet: file_path/old_string/new_string/replace_all); Write's is
/// only in the census keysets, so it is read defensively — a change we cannot
/// measure is still recorded, with the line counts left unknown.
fn file_edit(row: &MessageRow, name: &str, input: &Value, at: Option<Ms>) -> Option<FileEditRow> {
    let (kind, (lines_added, lines_removed)) = match name {
        "Edit" => (
            EditKind::Edit,
            input["new_string"].as_str().map_or((None, None), |new| {
                fold::line_delta(input["old_string"].as_str(), new)
            }),
        ),
        "Write" => (
            EditKind::Write,
            input["content"]
                .as_str()
                .map_or((None, None), |content| fold::line_delta(None, content)),
        ),
        _ => return None,
    };
    Some(FileEditRow {
        session_id: row.session_id.clone(),
        message_timeline_id: Some(row.timeline_id.clone()),
        path: input["file_path"].as_str()?.to_owned(),
        kind,
        // Claude Code keeps its own backups under file-history/; wiring that up
        // is that stream's job, not this one's.
        snapshot_ref: None,
        lines_added,
        lines_removed,
        at,
    })
}

/// A tool_result block's display text. The canonical observes only strings, but
/// the API also allows a block array (text + images), so both are read.
fn result_text(content: Option<&Value>) -> Option<String> {
    match content? {
        Value::String(text) => Some(text.clone()),
        Value::Array(blocks) => {
            let mut text = String::new();
            for block in blocks {
                if let Some(part) = block["text"].as_str() {
                    push_text(&mut text, part);
                }
            }
            (!text.is_empty()).then_some(text)
        }
        _ => None,
    }
}

// --------------------------------------------------------------------- plumbing

struct ToolResultBlock<'a> {
    tool_use_id: &'a str,
    content: Option<&'a Value>,
    is_error: Option<bool>,
}

/// The per-line fields every row from that line inherits.
struct Seed<'a> {
    uuid: Option<&'a str>,
    parent_uuid: Option<&'a str>,
    ts: Option<Ms>,
    version: Option<&'a str>,
    model: Option<&'a str>,
    effort: Option<&'a str>,
}

struct Emit<'a> {
    ctx: &'a mut NormalizeCtx,
    stream_id: &'a str,
    session_id: String,
    ops: Vec<IngestOp>,
}

impl<'a> Emit<'a> {
    fn new(ctx: &'a mut NormalizeCtx, stream_id: &'a str) -> Self {
        let session_id = ctx.session_id.clone();
        Self {
            ctx,
            stream_id,
            session_id,
            ops: Vec::new(),
        }
    }

    fn row(&mut self, seed: &Seed, kind: MessageKind, discriminator: &str) -> MessageRow {
        MessageRow {
            timeline_id: ids::timeline_id(&self.session_id, discriminator),
            session_id: self.session_id.clone(),
            ordinal: self.ctx.take_ordinal(),
            kind,
            subtype: None,
            uuid: seed.uuid.map(str::to_owned),
            parent_uuid: seed.parent_uuid.map(str::to_owned),
            ts: seed.ts,
            model: seed.model.map(str::to_owned),
            effort: seed.effort.map(str::to_owned),
            content: None,
            tool: None,
            branch_tool_id: None,
            usage: None,
            // Claude Code stamps the writing CLI version on every record, which
            // beats the runtime-wide version the pipeline guessed.
            agent_version: seed
                .version
                .map(str::to_owned)
                .or_else(|| self.ctx.agent_version.clone()),
            src_cursor: self.ctx.src_cursor,
            src_offset: self.ctx.src_offset,
        }
    }

    fn push_row(&mut self, row: MessageRow) {
        self.ops.push(IngestOp::Message(Box::new(row)));
    }

    fn tool_result(
        &mut self,
        result: &ToolResultBlock<'_>,
        payload: Option<Value>,
        at: Option<Ms>,
    ) {
        self.ops.push(IngestOp::ToolResult {
            session_id: self.session_id.clone(),
            tool_id: result.tool_use_id.to_owned(),
            result: result_text(result.content),
            payload,
            is_error: result.is_error,
            at,
        });
    }

    fn workspace(&mut self, cwd: &str, git_branch: &str, at: Option<Ms>) {
        // Without a timestamp there is nowhere to put the observation, and
        // recording it in state would suppress the next one that does have one.
        let Some(at) = at else { return };
        let changed = match self
            .ctx
            .state
            .as_mut()
            .and_then(|state| state.downcast_mut::<ClaudeCodeState>())
        {
            Some(state) => state.observe(cwd, git_branch),
            // A pipeline that skipped init_state gets every observation rather
            // than none: deduplication is an optimization, the data is not.
            None => true,
        };
        if changed {
            self.ops.push(IngestOp::WorkspaceObs(WorkspaceObsRow {
                session_id: self.session_id.clone(),
                at,
                cwd: cwd.to_owned(),
                git_branch: (!git_branch.is_empty()).then(|| git_branch.to_owned()),
                // Repo identity comes from discovery, not from a transcript.
                git_repo: None,
            }));
        }
    }

    fn drift(
        &mut self,
        code: &str,
        record_kind: &str,
        paths: Vec<String>,
        at: Option<Ms>,
        version: Option<&str>,
    ) {
        self.ops.push(IngestOp::Drift(DriftFact {
            runtime_id: self.ctx.runtime_id.clone(),
            agent_version: version
                .map(str::to_owned)
                .or_else(|| self.ctx.agent_version.clone())
                .unwrap_or_default(),
            stream: self.stream_id.to_owned(),
            record_kind: record_kind.to_owned(),
            error_code: code.to_owned(),
            shape_hash: ids::shape_hash(&paths),
            at,
            sample_cursor: self.ctx.src_cursor,
            sample_offset: self.ctx.src_offset,
        }));
    }
}

fn hard_error(
    ctx: &mut NormalizeCtx,
    stream_id: &str,
    record_kind: &str,
    value: Option<&Value>,
) -> LineOutcome {
    let mut emit = Emit::new(ctx, stream_id);
    emit.drift(
        PARSE_FAILED,
        record_kind,
        value.map(top_level_keys).unwrap_or_default(),
        value.and_then(record_ts),
        value.and_then(record_version),
    );
    LineOutcome {
        ops: emit.ops,
        status: LineStatus::HardError {
            code: PARSE_FAILED.to_owned(),
        },
    }
}

fn record_identity(record: &KnownRecord) -> (&'static str, Option<Ms>, Option<&str>) {
    match record {
        KnownRecord::User(line) => (
            "user",
            rfc3339_ms(&line.timestamp),
            Some(line.version.as_str()),
        ),
        KnownRecord::Assistant(line) => (
            "assistant",
            rfc3339_ms(&line.timestamp),
            Some(line.version.as_str()),
        ),
        KnownRecord::System(line) => (
            "system",
            rfc3339_ms(&line.timestamp),
            Some(line.version.as_str()),
        ),
        KnownRecord::AiTitle(_) => ("ai-title", None, None),
        KnownRecord::AtisLatch(_) => ("atis-latch", None, None),
        KnownRecord::QueueOperation(line) => ("queue-operation", rfc3339_ms(&line.timestamp), None),
        KnownRecord::LastPrompt(_) => ("last-prompt", None, None),
        KnownRecord::Mode(_) => ("mode", None, None),
    }
}

fn top_level_keys(value: &Value) -> Vec<String> {
    value
        .as_object()
        .map(|object| object.keys().cloned().collect())
        .unwrap_or_default()
}

fn record_ts(value: &Value) -> Option<Ms> {
    rfc3339_ms(value["timestamp"].as_str()?)
}

fn record_version(value: &Value) -> Option<&str> {
    value["version"].as_str()
}

fn push_text(buffer: &mut String, text: &str) {
    if !buffer.is_empty() {
        buffer.push('\n');
    }
    buffer.push_str(text);
}
