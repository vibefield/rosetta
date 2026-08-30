//! Raw transcript records → canonical ops.
//!
//! The one transform that matters here is the SPLIT: Claude Code can pack text,
//! thinking and tool calls into one assistant record or persist one response
//! across several records. The canonical model wants one row per visible block
//! with response-wide block identity (docs/canonical-model.html, "Message rows").
//! Everything else — tool results folding onto their call and file edits —
//! follows from that flattening. Session cwd is admitted by the bounded skim.
//!
//! Pure by contract (CLAUDE.md): no IO, no clock, no randomness. Every id is
//! derived from the session id plus a stable discriminator, so replaying a file
//! twice produces byte-identical ops.

use std::collections::{BTreeMap, BTreeSet};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use rosetta_adapter_support::tool::{
    projection_record_kind, projection_shape_paths, terminal_update, unknown_interaction,
};
use rosetta_adapter_support::{Drift, LineOutcome, LineStatus, NormalizeCtx};
use rosetta_core::rows::{
    DriftFact, EditKind, FileEditRow, MessageKind, MessageRow, Ms, SessionUsageRow, TokenSnapshot,
};
use rosetta_core::timeline::{
    AssetRef, AttachmentKind, AttachmentRef, CorrelationDerivation, GroupDerivation, GroupKind,
    ResourceRef, ResponseGroup, StatusBasis, TimelineGroup,
};
use rosetta_core::{IngestOp, ProjectionAsset, fold, ids};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::raw::transcript::{
    AiTitleLine, AssistantContentBlock, AssistantLine, CustomTitleLine, KnownAssistantBlock,
    KnownRecord, KnownUserBlock, RecordedUsage, SystemLine, UserContent, UserContentBlock,
    UserLine,
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

/// Adapter-private per-file response-fold state.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ClaudeCodeState {
    /// Native response ids are not necessarily contiguous in a transcript:
    /// branch reconstruction can repeat an older assistant record thousands
    /// of lines later. Keep the closed response block identity so checkpoint
    /// resume and branch replay assign the same canonical block index.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    responses: BTreeMap<String, ClaudeResponseState>,
    /// Result records carry no tool name. Retain only confirmed Agent/Task
    /// opener ids so an incidental `agentId` field cannot link an unrelated
    /// tool to a child session.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    delegate_calls: BTreeSet<String>,
}

impl ClaudeCodeState {
    /// Claude may persist one API response as several assistant JSONL records,
    /// each containing only newly materialized blocks. A branch replay may then
    /// repeat a prior record with the same UUID and a different parent. Stable
    /// block identities make that a last-writer update, not a new response slot.
    fn enter_response(&mut self, id: &str, block_identities: &[String]) -> ResponseProjection {
        let response = self.responses.entry(id.to_owned()).or_default();
        let mut block_indices = Vec::with_capacity(block_identities.len());
        for identity in block_identities {
            let index = response
                .blocks
                .iter()
                .position(|known| known == identity)
                .and_then(|index| u32::try_from(index).ok())
                .unwrap_or_else(|| {
                    let index = u32::try_from(response.blocks.len()).unwrap_or(u32::MAX);
                    response.blocks.push(identity.clone());
                    index
                });
            block_indices.push(index);
        }
        ResponseProjection {
            block_indices,
            usage: response.usage.clone(),
        }
    }

    fn remember_usage(&mut self, response_id: &str, usage: ClaudeResponseUsage) {
        if let Some(response) = self.responses.get_mut(response_id) {
            response.usage = Some(usage);
        }
    }

    fn remember_delegate_call(&mut self, tool_use_id: &str) {
        self.delegate_calls.insert(tool_use_id.to_owned());
    }

    fn take_delegate_call(&mut self, tool_use_id: &str) -> bool {
        self.delegate_calls.remove(tool_use_id)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ClaudeResponseState {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    blocks: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    usage: Option<ClaudeResponseUsage>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct ClaudeResponseUsage {
    snapshot: TokenSnapshot,
    target: ClaudeUsageTarget,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ClaudeUsageTarget {
    Timeline { timeline_id: String },
    Session { sample_key: String },
}

struct ResponseProjection {
    block_indices: Vec<u32>,
    usage: Option<ClaudeResponseUsage>,
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
                let paths = spec::unexpected_declared_fields(kind, &value);
                if paths.is_empty() {
                    return LineOutcome::parsed(Vec::new());
                }
                let mut emit = Emit::new(ctx, stream_id);
                emit.drift(
                    UNKNOWN_FIELDS,
                    kind,
                    paths,
                    record_ts(&value),
                    record_version(&value),
                );
                return LineOutcome::parsed(emit.ops);
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
        KnownRecord::CustomTitle(line) => custom_title_line(&mut emit, line),
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
    let mut attachments = Vec::new();
    let mut assets = Vec::new();
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
                        delegate_child_native_id: line
                            .tool_use_result
                            .as_ref()
                            .and_then(|result| result.get("agentId"))
                            .and_then(Value::as_str)
                            .filter(|id| !id.is_empty())
                            .map(str::to_owned),
                    }),
                    UserContentBlock::Known(KnownUserBlock::Image { source, .. }) => {
                        match authored_attachment(AttachmentKind::Image, source) {
                            Some((attachment, asset)) => {
                                attachments.push(attachment);
                                assets.extend(asset);
                            }
                            None => has_other_block = true,
                        }
                    }
                    UserContentBlock::Known(KnownUserBlock::Document { source, .. }) => {
                        match authored_attachment(AttachmentKind::Document, source) {
                            Some((attachment, asset)) => {
                                attachments.push(attachment);
                                assets.extend(asset);
                            }
                            None => has_other_block = true,
                        }
                    }
                    UserContentBlock::Other(_) => has_other_block = true,
                }
            }
        }
    }

    // A turn made only of tool_results is the report-back for a tool call — it
    // folds into that call's row instead of becoming a row of its own.
    if !text.is_empty() || !attachments.is_empty() || has_other_block || results.is_empty() {
        let mut row = emit.row(&seed, MessageKind::User, &line.uuid);
        row.content = (!text.is_empty()).then_some(text);
        row.attachments = attachments;
        row.group = Some(TimelineGroup {
            id: line.uuid.clone(),
            kind: GroupKind::HumanTurn,
            derivation: GroupDerivation::Native,
        });
        emit.ops.extend(
            assets
                .into_iter()
                .map(|asset| IngestOp::ProjectionAsset { asset }),
        );
        emit.push_row(row);
    }

    for (index, result) in results.into_iter().enumerate() {
        emit.tool_result(&result, u32::try_from(index).unwrap_or(u32::MAX), ts);
    }
}

fn assistant_line(emit: &mut Emit, line: &AssistantLine) {
    let ts = rfc3339_ms(&line.timestamp);
    let seed = Seed {
        uuid: Some(&line.uuid),
        parent_uuid: line.parent_uuid.as_deref(),
        ts,
        version: Some(&line.version),
        model: Some(&line.message.model),
        effort: line.effort.as_deref(),
    };

    let mut rows: Vec<(MessageRow, Option<FileEditRow>)> = Vec::new();
    let mut block_identities = Vec::new();
    for (index, block) in line.message.content.iter().enumerate() {
        // Split rows share the record's uuid, so the block index is what makes
        // the timeline id unique within the session.
        let discriminator = format!("{}#{index}", line.uuid);
        match block {
            AssistantContentBlock::Known(KnownAssistantBlock::Text { text, .. })
                if !text.trim().is_empty() =>
            {
                let mut row = emit.row(&seed, MessageKind::Agent, &discriminator);
                row.content = Some(text.clone());
                rows.push((row, None));
                block_identities.push(discriminator);
            }
            AssistantContentBlock::Known(KnownAssistantBlock::Thinking { thinking, .. })
                if !thinking.trim().is_empty() =>
            {
                let mut row = emit.row(&seed, MessageKind::Thinking, &discriminator);
                row.content = Some(thinking.clone());
                rows.push((row, None));
                block_identities.push(discriminator);
            }
            AssistantContentBlock::Known(KnownAssistantBlock::ToolUse {
                id, name, input, ..
            }) => {
                let mut row = emit.row(&seed, MessageKind::ToolUse, &discriminator);
                let projected = crate::tool::project_request(
                    Some(&line.version),
                    name,
                    input,
                    Some(&line.cwd),
                    id,
                    Some(id),
                    None,
                );
                row.tool = Some(match projected {
                    Ok(tool) => tool,
                    Err(error) => {
                        emit.drift(
                            error.drift_code(),
                            &projection_record_kind("request", None, name),
                            projection_shape_paths(input),
                            ts,
                            Some(&line.version),
                        );
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
                if SPAWN_TOOLS.contains(&name.as_str()) {
                    row.branch_tool_id = Some(id.clone());
                    emit.remember_delegate_call(id);
                }
                let edit = file_edit(&row, name, input, ts);
                rows.push((row, edit));
                block_identities.push(discriminator);
            }
            // Empty prose/reasoning and `fallback` (an in-response model switch)
            // carry no user-visible timeline content. Unfamiliar kinds land in
            // `Other` and are reported by the drift pass.
            _ => {}
        }
    }

    let fallback_first_block = rows
        .first()
        .and_then(|(row, _)| u32::try_from(row.ordinal).ok())
        .unwrap_or_default();
    let response =
        emit.response_projection(&line.message.id, &block_identities, fallback_first_block);
    for ((row, _), block_index) in rows.iter_mut().zip(&response.block_indices) {
        row.group = Some(TimelineGroup {
            id: line.message.id.clone(),
            kind: GroupKind::AgentCycle,
            derivation: GroupDerivation::Native,
        });
        row.response = Some(ResponseGroup {
            id: line.message.id.clone(),
            derivation: GroupDerivation::Native,
        });
        row.block_index = Some(*block_index);
    }

    // Claude repeats the same final response usage on every persisted block.
    // The first observation owns one canonical fact; an actual correction is
    // emitted as a replacement update against that same target.
    let mut deferred_usage = None;
    if let Some(tokens) = line
        .message
        .usage
        .as_ref()
        .map(snapshot)
        .filter(|tokens| !tokens.is_empty())
        && response.usage.as_ref().map(|usage| usage.snapshot) != Some(tokens)
    {
        let target = response
            .usage
            .as_ref()
            .map(|usage| usage.target.clone())
            .unwrap_or_else(|| response_usage_target(&rows, &line.message.id));
        match &target {
            ClaudeUsageTarget::Timeline { timeline_id } if response.usage.is_none() => {
                if let Some((row, _)) = rows
                    .iter_mut()
                    .find(|(row, _)| row.timeline_id == *timeline_id)
                {
                    row.usage = Some(tokens);
                }
            }
            ClaudeUsageTarget::Timeline { timeline_id } => {
                deferred_usage = Some(IngestOp::Usage {
                    session_id: emit.session_id.clone(),
                    response: timeline_id.clone(),
                    ts: ts.unwrap_or(0),
                    model: Some(line.message.model.clone()),
                    tokens,
                });
            }
            ClaudeUsageTarget::Session { sample_key } => {
                deferred_usage = Some(IngestOp::SessionUsage(SessionUsageRow {
                    session_id: emit.session_id.clone(),
                    key: sample_key.clone(),
                    ts: ts.unwrap_or(0),
                    model: Some(line.message.model.clone()),
                    tokens,
                }));
            }
        }
        emit.remember_response_usage(
            &line.message.id,
            ClaudeResponseUsage {
                snapshot: tokens,
                target,
            },
        );
    }
    for (row, edit) in rows {
        emit.push_row(row);
        if let Some(edit) = edit {
            emit.ops.push(IngestOp::FileEdit(edit));
        }
    }
    if let Some(usage) = deferred_usage {
        emit.ops.push(usage);
    }
}

fn system_line(emit: &mut Emit, line: &SystemLine) {
    let ts = rfc3339_ms(&line.timestamp);
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

fn custom_title_line(emit: &mut Emit, line: &CustomTitleLine) {
    let seed = Seed {
        uuid: None,
        parent_uuid: None,
        ts: None,
        version: None,
        model: None,
        effort: None,
    };
    let mut row = emit.row(&seed, MessageKind::Summary, "custom-title");
    row.subtype = Some("custom_title".to_owned());
    row.content = Some(line.custom_title.clone());
    emit.push_row(row);
}

// -------------------------------------------------------------------- derived

/// Pick the best row already available when usage is first observed. A response
/// split across JSONL records cannot be buffered waiting for a later text block,
/// so the first projectable record becomes the stable replacement target.
fn response_usage_target(
    rows: &[(MessageRow, Option<FileEditRow>)],
    response_id: &str,
) -> ClaudeUsageTarget {
    let target = rows
        .iter()
        .position(|(row, _)| row.kind == MessageKind::Agent)
        .or_else(|| {
            rows.iter()
                .position(|(row, _)| row.kind == MessageKind::ToolUse)
        })
        .unwrap_or(0);
    match rows.get(target) {
        Some((row, _)) => ClaudeUsageTarget::Timeline {
            timeline_id: row.timeline_id.clone(),
        },
        None => ClaudeUsageTarget::Session {
            sample_key: format!("response:{response_id}"),
        },
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

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AuthoredSource {
    Base64 {
        media_type: String,
        data: String,
    },
    Url {
        url: String,
    },
    Text {
        data: String,
        #[serde(default)]
        media_type: Option<String>,
    },
}

/// Strictly decode only user-authored attachment sources. Tool-result media
/// never calls this path. Invalid labels or bytes stay recoverable in the raw
/// ledger and do not create a projection member that would fail the batch.
fn authored_attachment(
    kind: AttachmentKind,
    source: &Value,
) -> Option<(AttachmentRef, Option<ProjectionAsset>)> {
    match serde_json::from_value::<AuthoredSource>(source.clone()).ok()? {
        AuthoredSource::Url { url } if !url.is_empty() => Some((
            AttachmentRef {
                kind,
                name: None,
                media_type: None,
                byte_len: None,
                source: ResourceRef::Url { url },
                dimensions: None,
            },
            None,
        )),
        AuthoredSource::Base64 { media_type, data } => {
            let media_type = canonical_media_type(&media_type)?;
            if kind == AttachmentKind::Image && !media_type.starts_with("image/") {
                return None;
            }
            let bytes = STANDARD.decode(data).ok()?;
            if bytes.is_empty() || !member_magic_matches(media_type, &bytes) {
                return None;
            }
            member_attachment(kind, media_type, bytes)
        }
        AuthoredSource::Text { data, media_type } if kind == AttachmentKind::Document => {
            if data.is_empty()
                || media_type.as_deref().is_some_and(|value| {
                    canonical_media_type(value) != Some("text/plain; charset=utf-8")
                })
            {
                return None;
            }
            member_attachment(kind, "text/plain; charset=utf-8", data.into_bytes())
        }
        _ => None,
    }
}

fn member_attachment(
    kind: AttachmentKind,
    media_type: &str,
    bytes: Vec<u8>,
) -> Option<(AttachmentRef, Option<ProjectionAsset>)> {
    let asset = ProjectionAsset::new(media_type, bytes)?;
    let reference = AssetRef {
        url: asset.url()?,
        digest: asset.digest.clone(),
        media_type: asset.media_type.clone(),
        byte_len: u64::try_from(asset.bytes.len()).ok()?,
    };
    Some((
        AttachmentRef {
            kind,
            name: None,
            media_type: Some(asset.media_type.clone()),
            byte_len: Some(reference.byte_len),
            source: ResourceRef::Asset { asset: reference },
            dimensions: None,
        },
        Some(asset),
    ))
}

fn canonical_media_type(media_type: &str) -> Option<&'static str> {
    match media_type.to_ascii_lowercase().as_str() {
        "image/png" => Some("image/png"),
        "image/jpeg" | "image/jpg" => Some("image/jpeg"),
        "image/gif" => Some("image/gif"),
        "image/webp" => Some("image/webp"),
        "application/pdf" => Some("application/pdf"),
        "text/plain" | "text/plain; charset=utf-8" => Some("text/plain; charset=utf-8"),
        _ => None,
    }
}

fn member_magic_matches(media_type: &str, bytes: &[u8]) -> bool {
    match media_type {
        "image/png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "image/jpeg" => bytes.starts_with(b"\xff\xd8\xff"),
        "image/gif" => bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
        "image/webp" => bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP",
        "application/pdf" => bytes.starts_with(b"%PDF-"),
        "text/plain; charset=utf-8" => std::str::from_utf8(bytes).is_ok(),
        _ => false,
    }
}

// --------------------------------------------------------------------- plumbing

struct ToolResultBlock<'a> {
    tool_use_id: &'a str,
    content: Option<&'a Value>,
    is_error: Option<bool>,
    delegate_child_native_id: Option<String>,
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
            attachments: Vec::new(),
            citations: Vec::new(),
            group: None,
            response: None,
            block_index: None,
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
        IngestOp::push_message(&mut self.ops, row);
    }

    fn response_projection(
        &mut self,
        response_id: &str,
        block_identities: &[String],
        fallback_first_block: u32,
    ) -> ResponseProjection {
        self.ctx
            .state
            .as_mut()
            .and_then(|state| state.downcast_mut::<ClaudeCodeState>())
            .map(|state| state.enter_response(response_id, block_identities))
            // `AgentAdapter::init_state` makes this unreachable in production.
            // Keeping a deterministic ordinal fallback makes direct trait use
            // degrade to collision-free output instead of panicking.
            .unwrap_or(ResponseProjection {
                block_indices: (0..block_identities.len())
                    .map(|index| {
                        fallback_first_block
                            .saturating_add(u32::try_from(index).unwrap_or(u32::MAX))
                    })
                    .collect(),
                usage: None,
            })
    }

    fn remember_response_usage(&mut self, response_id: &str, usage: ClaudeResponseUsage) {
        if let Some(state) = self
            .ctx
            .state
            .as_mut()
            .and_then(|state| state.downcast_mut::<ClaudeCodeState>())
        {
            state.remember_usage(response_id, usage);
        }
    }

    fn remember_delegate_call(&mut self, tool_use_id: &str) {
        if let Some(state) = self
            .ctx
            .state
            .as_mut()
            .and_then(|state| state.downcast_mut::<ClaudeCodeState>())
        {
            state.remember_delegate_call(tool_use_id);
        }
    }

    fn take_delegate_call(&mut self, tool_use_id: &str) -> bool {
        self.ctx
            .state
            .as_mut()
            .and_then(|state| state.downcast_mut::<ClaudeCodeState>())
            .is_some_and(|state| state.take_delegate_call(tool_use_id))
    }

    fn tool_result(&mut self, result: &ToolResultBlock<'_>, mutation_index: u32, at: Option<Ms>) {
        let diagnostic = result
            .is_error
            .is_some_and(|is_error| is_error)
            .then(|| result_text(result.content))
            .flatten();
        let mut patch = terminal_update(
            result.is_error,
            true,
            StatusBasis::NativeTerminalRecord,
            at,
            diagnostic.as_deref(),
        );
        if self.take_delegate_call(result.tool_use_id) {
            patch.delegate_child_session_id = result
                .delegate_child_native_id
                .as_deref()
                .map(|native| ids::session_id(&self.ctx.runtime_id, native));
        }
        self.ops.push(IngestOp::ToolUpdate {
            session_id: self.session_id.clone(),
            correlation_key: result.tool_use_id.to_owned(),
            evidence: self.ctx.evidence(mutation_index),
            patch: Box::new(patch),
            at,
        });
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
        KnownRecord::CustomTitle(_) => ("custom-title", None, None),
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
