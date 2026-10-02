//! Deterministic 2.x snapshot rows → canonical operations.
//!
//! One `session_message` row is one record and, for an assistant, one model
//! request: each step appends its own assistant message
//! (`session/message-updater.ts:213-255`) and the step's usage is written on
//! it in the exclusive buckets (`:261-285`, `session/usage.ts:11-19`). The
//! row's content parts become the same canonical rows the 1.x reader emits
//! for `part` rows, so the two readers meet on one timeline.

use rosetta_adapter_support::adapter::{LineOutcome, LineStatus, NormalizeCtx};
use rosetta_adapter_support::media::{self, MaterializedAttachment};
use rosetta_adapter_support::tool::{
    first_string, projection_record_kind, projection_shape_paths, terminal_update,
    unknown_interaction,
};
use rosetta_adapter_support::{Drift, Unknown, note_unknown};
use rosetta_core::rows::{MessageKind, MessageRow};
use rosetta_core::timeline::AttachmentKind;
use rosetta_core::{IngestOp, ids};
use serde::Deserialize;
use serde_json::Value;

use crate::normalize::{drift, hard};
use crate::raw::message::{
    Assistant, AssistantContent, Compaction, ParsedContent, ParsedSessionMessage, SessionMessage,
    parse_content, parse_session_message,
};
use crate::spec;

#[derive(Deserialize)]
pub(crate) struct TimelineRecord {
    message_id: String,
    message_type: String,
    #[allow(dead_code)]
    seq: i64,
    message_time_created: i64,
    message_data: String,
    session_time_created: i64,
    fork_session_id: Option<String>,
    session_directory: Option<String>,
    #[serde(flatten)]
    unknown: Unknown,
}

pub(crate) fn ingest(
    ctx: &mut NormalizeCtx,
    stream_id: &str,
    query: &str,
    row: Value,
    outer_paths: Vec<String>,
) -> LineOutcome {
    if query == spec::USAGE_V2_QUERY {
        return crate::usage_v2::usage_event(ctx, stream_id, row, outer_paths);
    }
    let record = match serde_json::from_value::<TimelineRecord>(row) {
        Ok(record) => record,
        Err(_) => return hard(ctx, stream_id, "snapshot/timeline-v2", "parse_failed", None),
    };
    let mut paths = outer_paths;
    note_unknown(&record.unknown, "snapshot.row", &mut paths);
    let at = Some(record.message_time_created);
    let message = match parse_session_message(&record.message_type, &record.message_data) {
        Ok(ParsedSessionMessage::Known(message)) => message,
        Ok(ParsedSessionMessage::UnknownType { rtype, .. }) => {
            if spec::declared_session_message_types().contains(&rtype) {
                return LineOutcome::parsed(Vec::new());
            }
            return LineOutcome {
                ops: vec![drift(
                    ctx,
                    stream_id,
                    &format!("session_message/{rtype}"),
                    "unknown_type",
                    &[],
                    at,
                )],
                status: LineStatus::UnknownType,
            };
        }
        Err(_) => {
            return hard(
                ctx,
                stream_id,
                &format!("session_message/{}", record.message_type),
                "parse_failed",
                at,
            );
        }
    };
    let kind = format!("session_message/{}", message.type_name());
    let mut ops = Vec::new();
    paths.extend(message.drift_paths());
    if !paths.is_empty() {
        ops.push(drift(ctx, stream_id, &kind, "unknown_fields", &paths, at));
    }
    // A 2.x fork copies the parent's settled history under new ids with the
    // original `time_created` (`session/projector.ts:186-230`); the forked
    // session itself is created at fork time. OpenCode's own stats apply this
    // rule (`session/stats.ts:147`).
    let copied = record.fork_session_id.is_some()
        && record.message_time_created < record.session_time_created;
    let status = project(ctx, stream_id, &record, &message, copied, &mut ops);
    LineOutcome { ops, status }
}

fn project(
    ctx: &mut NormalizeCtx,
    stream_id: &str,
    record: &TimelineRecord,
    message: &SessionMessage,
    copied: bool,
    ops: &mut Vec<IngestOp>,
) -> LineStatus {
    let created = created_ms(message.time().created, record.message_time_created);
    match message {
        SessionMessage::User(user) => {
            let mut row = message_row(ctx, record, MessageKind::User, "message", created, None);
            row.content = (!user.text.is_empty()).then(|| user.text.clone());
            for (index, file) in user.files.iter().flatten().enumerate() {
                if !file.mime.to_ascii_lowercase().starts_with("image/") {
                    continue;
                }
                let id = format!("{}#file{index}", record.message_id);
                let url = format!("data:{};base64,{}", file.mime, file.data);
                let (mut attachment, asset): MaterializedAttachment =
                    media::from_url(&id, AttachmentKind::Image, &url, Some(&file.mime));
                attachment.name = file.name.as_deref().and_then(media::bounded_name);
                row.attachments.push(attachment);
                ops.extend(asset.map(|asset| IngestOp::ProjectionAsset { asset }));
            }
            IngestOp::push_message(ops, row);
            LineStatus::Parsed
        }
        // A user-run `!command` is a shell execution with a status of its
        // own; it projects like the model's shell tool calls.
        SessionMessage::Shell(shell) => {
            let mut row = message_row(ctx, record, MessageKind::ToolUse, "message", created, None);
            let input = serde_json::json!({ "command": shell.command });
            row.tool = Some(
                crate::tool::project_request(
                    Some(writer_version(ctx)),
                    "shell",
                    &input,
                    record.session_directory.as_deref(),
                    &shell.shell_id,
                    Some(&shell.shell_id),
                )
                .unwrap_or_else(|_| {
                    unknown_interaction(
                        shell.shell_id.clone(),
                        rosetta_core::CorrelationDerivation::NativeId,
                        Some(shell.shell_id.clone()),
                        None,
                        "shell",
                        None,
                    )
                }),
            );
            IngestOp::push_message(ops, row);
            let ended = shell.time.completed.map_or(Some(created), |completed| {
                Some(created_ms(completed, created))
            });
            let is_error = match shell.status.as_str() {
                "completed" => Some(false),
                "failed" | "error" => Some(true),
                "running" | "pending" | "queued" => None,
                other => {
                    ops.push(drift(
                        ctx,
                        stream_id,
                        "session_message/shell",
                        &format!("unknown_shell_status:{other}"),
                        &[],
                        Some(created),
                    ));
                    None
                }
            };
            if let Some(is_error) = is_error {
                ops.push(IngestOp::ToolUpdate {
                    session_id: ctx.session_id.clone(),
                    correlation_key: shell.shell_id.clone(),
                    evidence: ctx.evidence(1),
                    patch: Box::new(terminal_update(
                        Some(is_error),
                        true,
                        rosetta_core::StatusBasis::NativeState,
                        ended,
                        None,
                    )),
                    at: ended,
                });
            }
            LineStatus::Parsed
        }
        SessionMessage::Assistant(assistant) => {
            assistant_rows(ctx, stream_id, record, assistant, copied, created, ops)
        }
        SessionMessage::Compaction(compaction) => {
            compaction_rows(ctx, record, compaction, copied, created, ops);
            LineStatus::Parsed
        }
        // Model-facing injections (synthetic and system text, skill bodies)
        // are neither the user's words nor the model's reply, and session
        // state transitions carry no content: none has a canonical row in
        // the closed timeline vocabulary. They stay in the raw ledger.
        SessionMessage::Synthetic(_)
        | SessionMessage::System(_)
        | SessionMessage::Skill(_)
        | SessionMessage::Idle(_)
        | SessionMessage::AgentSwitched(_)
        | SessionMessage::ModelSwitched(_)
        | SessionMessage::LocationSwitched(_) => LineStatus::Parsed,
    }
}

/// Content items in order, then the step's usage on its first surviving row
/// (or a request sample when none survived). A copied message keeps its rows
/// and drops its usage (ARCHITECTURE §4.1.3, counting rule 2).
fn assistant_rows(
    ctx: &mut NormalizeCtx,
    stream_id: &str,
    record: &TimelineRecord,
    assistant: &Assistant,
    copied: bool,
    created: i64,
    ops: &mut Vec<IngestOp>,
) -> LineStatus {
    let model = Some(assistant.model.id.clone());
    let mut first_row: Option<String> = None;
    let mut push = |ops: &mut Vec<IngestOp>, row: MessageRow| {
        let timeline_id = row.timeline_id.clone();
        if IngestOp::push_message(ops, row) && first_row.is_none() {
            first_row = Some(timeline_id);
        }
    };
    if let Some(retry) = &assistant.retry {
        let mut row = message_row(
            ctx,
            record,
            MessageKind::System,
            "retry",
            created_ms(retry.at, created),
            model.clone(),
        );
        row.subtype = Some("retry".to_owned());
        row.content = retry
            .error
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| serde_json::to_string(&retry.error).ok());
        push(ops, row);
    }
    for (index, item) in assistant.content.iter().enumerate() {
        let discriminator = index.to_string();
        let content = match parse_content(item) {
            Ok(ParsedContent::Known(content)) => content,
            Ok(ParsedContent::UnknownType { rtype, .. }) => {
                let rtype = rtype.unwrap_or_else(|| "<untyped>".to_owned());
                if !spec::declared_assistant_content_types().contains(&rtype) {
                    ops.push(drift(
                        ctx,
                        stream_id,
                        &format!("assistant/content/{rtype}"),
                        "unknown_type",
                        &[],
                        Some(created),
                    ));
                }
                continue;
            }
            Err(_) => {
                let rtype = item["type"].as_str().unwrap_or("<untyped>");
                ops.push(drift(
                    ctx,
                    stream_id,
                    &format!("assistant/content/{rtype}"),
                    "parse_failed",
                    &[],
                    Some(created),
                ));
                return LineStatus::HardError {
                    code: "parse_failed".to_owned(),
                };
            }
        };
        let paths = content.drift_paths();
        if !paths.is_empty() {
            ops.push(drift(
                ctx,
                stream_id,
                &format!("assistant/content/{}", content_kind(&content)),
                "unknown_fields",
                &paths,
                Some(created),
            ));
        }
        match &*content {
            AssistantContent::Text(text) => {
                let mut row = message_row(
                    ctx,
                    record,
                    MessageKind::Agent,
                    &discriminator,
                    created,
                    model.clone(),
                );
                row.content = Some(text.text.clone());
                push(ops, row);
            }
            AssistantContent::Reasoning(reasoning) => {
                let at = reasoning
                    .time
                    .as_ref()
                    .map_or(created, |time| created_ms(time.created, created));
                let mut row = message_row(
                    ctx,
                    record,
                    MessageKind::Thinking,
                    &discriminator,
                    at,
                    model.clone(),
                );
                row.content = Some(reasoning.text.clone());
                push(ops, row);
            }
            AssistantContent::Tool(tool) => {
                let at = created_ms(tool.time.created, created);
                let mut row = message_row(
                    ctx,
                    record,
                    MessageKind::ToolUse,
                    &discriminator,
                    at,
                    model.clone(),
                );
                let input = tool.state.get("input").cloned().unwrap_or(Value::Null);
                // A 2.x shell call names its own working directory; other
                // calls resolve relative arguments against the session's.
                let cwd = first_string(&input, &["workdir"])
                    .map(str::to_owned)
                    .or_else(|| record.session_directory.clone());
                let projected = crate::tool::project_request(
                    Some(writer_version(ctx)),
                    &tool.name,
                    &input,
                    cwd.as_deref(),
                    &tool.id,
                    Some(&tool.id),
                );
                row.tool = Some(match projected {
                    Ok(interaction) => interaction,
                    Err(error) => {
                        ops.push(drift(
                            ctx,
                            stream_id,
                            &projection_record_kind("request", None, &tool.name),
                            error.drift_code(),
                            &projection_shape_paths(&input),
                            Some(at),
                        ));
                        unknown_interaction(
                            tool.id.clone(),
                            rosetta_core::CorrelationDerivation::NativeId,
                            Some(tool.id.clone()),
                            None,
                            tool.name.clone(),
                            None,
                        )
                    }
                });
                push(ops, row);
                let ended = tool
                    .time
                    .completed
                    .and_then(|completed| i64::try_from(completed).ok())
                    .or(Some(at));
                crate::images::tool_output_v2(ctx, &tool.id, &tool.state, ended, ops);
                match tool.state.get("status").and_then(Value::as_str) {
                    Some("completed") => ops.push(IngestOp::ToolUpdate {
                        session_id: ctx.session_id.clone(),
                        correlation_key: tool.id.clone(),
                        evidence: ctx.evidence(1),
                        patch: Box::new(terminal_update(
                            Some(false),
                            true,
                            rosetta_core::StatusBasis::NativeState,
                            ended,
                            None,
                        )),
                        at: ended,
                    }),
                    Some("error") => ops.push(IngestOp::ToolUpdate {
                        session_id: ctx.session_id.clone(),
                        correlation_key: tool.id.clone(),
                        evidence: ctx.evidence(1),
                        patch: Box::new(terminal_update(
                            Some(true),
                            true,
                            rosetta_core::StatusBasis::NativeState,
                            ended,
                            tool.state
                                .get("error")
                                .and_then(|error| error.get("message"))
                                .and_then(Value::as_str),
                        )),
                        at: ended,
                    }),
                    Some("streaming" | "running") => {}
                    Some(status) => ops.push(drift(
                        ctx,
                        stream_id,
                        "assistant/content/tool",
                        &format!("unknown_tool_status:{status}"),
                        &[],
                        Some(at),
                    )),
                    None => ops.push(drift(
                        ctx,
                        stream_id,
                        "assistant/content/tool",
                        "missing_tool_status",
                        &[],
                        Some(at),
                    )),
                }
            }
        }
    }
    if !copied {
        crate::usage_v2::step_usage(ctx, &record.message_id, assistant, first_row, created, ops);
    }
    LineStatus::Parsed
}

/// A completed compaction's summary is a summary row; the compaction
/// request's own usage (`compaction.ts:435-457,557-573,609-733`) is a sample,
/// because the message that carries it is the summary, not a response.
fn compaction_rows(
    ctx: &mut NormalizeCtx,
    record: &TimelineRecord,
    compaction: &Compaction,
    copied: bool,
    created: i64,
    ops: &mut Vec<IngestOp>,
) {
    let model = compaction.model.as_ref().map(|model| model.id.clone());
    if compaction.status == "completed"
        && let Some(summary) = compaction
            .summary
            .as_deref()
            .filter(|text| !text.is_empty())
    {
        let mut row = message_row(
            ctx,
            record,
            MessageKind::Summary,
            "message",
            created,
            model.clone(),
        );
        row.subtype = Some(format!("compaction:{}", compaction.reason));
        row.content = Some(summary.to_owned());
        IngestOp::push_message(ops, row);
    }
    if !copied {
        crate::usage_v2::compaction_usage(ctx, &record.message_id, compaction, created, ops);
    }
}

fn message_row(
    ctx: &mut NormalizeCtx,
    record: &TimelineRecord,
    kind: MessageKind,
    discriminator: &str,
    at: i64,
    model: Option<String>,
) -> MessageRow {
    // Content items have no ids of their own: `<message>#<index>` is stable
    // because a message's content only ever appends.
    let native = match discriminator {
        "message" => format!("message:{}", record.message_id),
        other => format!("{}#{other}", record.message_id),
    };
    MessageRow {
        timeline_id: ids::timeline_id(&ctx.session_id, &native),
        session_id: ctx.session_id.clone(),
        ordinal: ctx.take_ordinal(),
        kind,
        subtype: None,
        uuid: Some(record.message_id.clone()),
        parent_uuid: None,
        ts: Some(at),
        model,
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

/// The writer of a 2.x-native row. `session_v2.version` is the creation
/// version, which an imported session keeps from 1.x while 2.x appends to
/// it; every row this reader sees was written by a 2.x writer, so a session
/// version below 2.0.0 names the first 2.x writer for tool projection.
fn writer_version(ctx: &NormalizeCtx) -> &str {
    ctx.agent_version
        .as_deref()
        .filter(|version| {
            version
                .trim_start_matches('v')
                .split('.')
                .next()
                .and_then(|major| major.parse::<u64>().ok())
                .is_some_and(|major| major >= 2)
        })
        .unwrap_or(crate::tool::V2_FIRST_WRITER_VERSION)
}

fn created_ms(value: u64, fallback: i64) -> i64 {
    i64::try_from(value).unwrap_or(fallback)
}

fn content_kind(content: &AssistantContent) -> &'static str {
    match content {
        AssistantContent::Text(_) => "text",
        AssistantContent::Reasoning(_) => "reasoning",
        AssistantContent::Tool(_) => "tool",
    }
}

#[cfg(test)]
mod tests {
    use rosetta_adapter_support::adapter::{LineStatus, NormalizeCtx};
    use rosetta_core::rows::{DriftFact, TokenSnapshot};
    use rosetta_core::timeline::{SummaryPurpose, TimelineEntry};
    use rosetta_core::{IngestOp, TimelineBody};
    use serde_json::json;

    use crate::normalize::{OpenCodeState, ingest};
    use crate::spec;

    fn line(query: &str, row: serde_json::Value) -> Vec<u8> {
        serde_json::to_vec(&json!({"query": query, "row": row})).unwrap()
    }

    fn message_line(
        id: &str,
        message_type: &str,
        created: i64,
        session_created: i64,
        fork: Option<&str>,
        data: serde_json::Value,
    ) -> Vec<u8> {
        line(
            spec::TIMELINE_V2_QUERY,
            json!({
                "message_id": id,
                "message_type": message_type,
                "seq": 0,
                "message_time_created": created,
                "message_data": data.to_string(),
                "session_time_created": session_created,
                "fork_session_id": fork,
                "session_directory": "/workspace"
            }),
        )
    }

    fn run(lines: &[Vec<u8>]) -> (Vec<IngestOp>, Vec<LineStatus>) {
        run_as("2.0.14", lines)
    }

    fn run_as(version: &str, lines: &[Vec<u8>]) -> (Vec<IngestOp>, Vec<LineStatus>) {
        let mut ctx = NormalizeCtx::new("runtime", "session");
        ctx.agent_version = Some(version.to_owned());
        let mut state = OpenCodeState::default();
        let mut ops = Vec::new();
        let mut statuses = Vec::new();
        for line in lines {
            let outcome = ingest(&mut ctx, &mut state, spec::DATABASE, line);
            statuses.push(outcome.status);
            ops.extend(outcome.ops);
        }
        (ops, statuses)
    }

    fn messages(ops: &[IngestOp]) -> Vec<&TimelineEntry> {
        ops.iter()
            .filter_map(|op| match op {
                IngestOp::Message(row) => Some(row.as_ref()),
                _ => None,
            })
            .collect()
    }

    type UsageOp = (&'static str, String, Option<String>, TokenSnapshot, i64);

    fn usage_ops(ops: &[IngestOp]) -> Vec<UsageOp> {
        ops.iter()
            .filter_map(|op| match op {
                IngestOp::Usage {
                    response,
                    model,
                    tokens,
                    ts,
                    ..
                } => Some(("row", response.clone(), model.clone(), tokens.clone(), *ts)),
                IngestOp::SessionUsage(sample) => Some((
                    "sample",
                    sample.key.clone(),
                    sample.model.clone(),
                    sample.tokens.clone(),
                    sample.ts,
                )),
                _ => None,
            })
            .collect()
    }

    fn drift_codes(ops: &[IngestOp]) -> Vec<String> {
        ops.iter()
            .filter_map(|op| match op {
                IngestOp::Drift(DriftFact { error_code, .. }) => Some(error_code.clone()),
                _ => None,
            })
            .collect()
    }

    fn assistant(tokens: bool) -> serde_json::Value {
        let mut value = json!({
            "agent": "build",
            "model": {"id": "claude-sonnet-4-5", "providerID": "anthropic"},
            "content": [
                {"type": "reasoning", "text": "thinking", "time": {"created": 105, "completed": 106}},
                {"type": "tool", "id": "call_1", "name": "read",
                 "state": {"status": "completed", "input": {"path": "src/lib.rs"}, "content": [{"type": "text", "text": "x"}]},
                 "time": {"created": 107, "ran": 108, "completed": 109}},
                {"type": "text", "text": "done"}
            ],
            "finish": "stop",
            "cost": 0.01,
            "time": {"created": 100, "completed": 120}
        });
        if tokens {
            value["tokens"] = json!({"input": 10, "output": 20, "reasoning": 5, "cache": {"read": 1000, "write": 200}});
        }
        value
    }

    #[test]
    fn an_assistant_step_is_one_request_on_its_first_row() {
        let (ops, statuses) = run(&[
            message_line(
                "msg_u",
                "user",
                90,
                50,
                None,
                json!({"text": "hello", "time": {"created": 90}}),
            ),
            message_line("msg_a", "assistant", 100, 50, None, assistant(true)),
        ]);
        assert!(
            statuses.iter().all(|status| *status == LineStatus::Parsed),
            "{statuses:?}"
        );
        assert!(drift_codes(&ops).is_empty(), "{ops:?}");
        let rows = messages(&ops);
        assert_eq!(rows.len(), 4, "{rows:?}");
        assert!(matches!(rows[0].body, TimelineBody::User { .. }));
        assert_eq!(rows[0].native_id.as_deref(), Some("msg_u"));
        assert!(matches!(rows[1].body, TimelineBody::Thinking { .. }));
        assert_eq!(rows[1].at, Some(105));
        assert!(matches!(rows[2].body, TimelineBody::ToolUse { .. }));
        assert_eq!(rows[2].at, Some(107));
        assert_eq!(rows[3].model.as_deref(), Some("claude-sonnet-4-5"));
        assert!(
            ops.iter()
                .any(|op| matches!(op, IngestOp::ToolUpdate { at: Some(109), .. }))
        );
        let usage = usage_ops(&ops);
        assert_eq!(usage.len(), 1, "{usage:?}");
        let (kind, target, model, tokens, ts) = &usage[0];
        assert_eq!(*kind, "row");
        assert_eq!(
            target,
            &rosetta_core::ids::timeline_id("session", "msg_a#0")
        );
        assert_eq!(model.as_deref(), Some("claude-sonnet-4-5"));
        assert_eq!(*ts, 120);
        assert_eq!(
            (
                tokens.input,
                tokens.output,
                tokens.thinking,
                tokens.cache_read,
                tokens.cache_write
            ),
            (Some(10), Some(20), Some(5), Some(1000), Some(200))
        );
        assert_eq!(tokens.route.as_deref(), Some("anthropic"));
    }

    #[test]
    fn a_forked_copy_keeps_its_rows_and_drops_its_usage() {
        // The fork was created at 500; the copied message keeps its original
        // creation time of 100.
        let (ops, _) = run(&[message_line(
            "msg_c",
            "assistant",
            100,
            500,
            Some("ses_parent"),
            assistant(true),
        )]);
        assert_eq!(messages(&ops).len(), 3);
        assert!(usage_ops(&ops).is_empty(), "{ops:?}");
        // Post-fork activity in the same session counts.
        let (ops, _) = run(&[message_line(
            "msg_n",
            "assistant",
            600,
            500,
            Some("ses_parent"),
            assistant(true),
        )]);
        assert_eq!(usage_ops(&ops).len(), 1);
    }

    #[test]
    fn a_step_without_a_surviving_row_becomes_a_request_sample() {
        let mut value = assistant(true);
        value["content"] = json!([]);
        let (ops, _) = run(&[message_line("msg_e", "assistant", 100, 50, None, value)]);
        assert!(messages(&ops).is_empty());
        let usage = usage_ops(&ops);
        assert_eq!(usage.len(), 1);
        assert_eq!(usage[0].0, "sample");
        assert_eq!(usage[0].1, rosetta_core::ids::usage_id("session", "msg_e"));
        // A message without tokens (still streaming) records nothing yet.
        let (ops, _) = run(&[message_line(
            "msg_s",
            "assistant",
            100,
            50,
            None,
            assistant(false),
        )]);
        assert!(usage_ops(&ops).is_empty());
    }

    #[test]
    fn a_retry_and_an_unknown_content_item_are_soft() {
        let mut value = assistant(true);
        value["retry"] = json!({"attempt": 2, "at": 101, "error": {"type": "provider", "message": "overloaded"}});
        value["content"] = json!([{"type": "text", "text": "ok"}, {"type": "hologram", "x": 1}]);
        let (ops, statuses) = run(&[message_line("msg_r", "assistant", 100, 50, None, value)]);
        assert_eq!(statuses, [LineStatus::Parsed]);
        let rows = messages(&ops);
        assert_eq!(rows.len(), 2);
        assert!(matches!(&rows[0].body, TimelineBody::Notice { .. }));
        assert_eq!(rows[0].at, Some(101));
        assert_eq!(drift_codes(&ops), vec!["unknown_type"]);
        // The retry row carried the step's usage: it was the first survivor.
        assert_eq!(
            usage_ops(&ops)[0].1,
            rosetta_core::ids::timeline_id("session", "msg_r#retry")
        );
    }

    #[test]
    fn a_completed_compaction_is_a_summary_with_a_usage_sample() {
        let (ops, statuses) = run(&[message_line(
            "msg_k",
            "compaction",
            300,
            50,
            None,
            json!({"status": "completed", "reason": "auto", "summary": "so far", "recent": "",
                   "model": {"id": "gpt-5.5", "providerID": "openai"}, "cost": 0.02,
                   "tokens": {"input": 4000, "output": 300, "reasoning": 100, "cache": {"read": 0, "write": 0}},
                   "time": {"created": 300}}),
        )]);
        assert_eq!(statuses, [LineStatus::Parsed]);
        let rows = messages(&ops);
        assert_eq!(rows.len(), 1);
        assert!(matches!(
            &rows[0].body,
            TimelineBody::Summary {
                purpose: SummaryPurpose::Compaction,
                ..
            }
        ));
        let usage = usage_ops(&ops);
        assert_eq!(usage.len(), 1);
        assert_eq!(usage[0].0, "sample");
        assert_eq!(usage[0].2.as_deref(), Some("gpt-5.5"));
        assert_eq!(usage[0].3.route.as_deref(), Some("openai"));
        assert_eq!(
            (usage[0].3.input, usage[0].3.thinking),
            (Some(4000), Some(100))
        );
        // A failed compaction still cost its tokens.
        let (ops, _) = run(&[message_line(
            "msg_f",
            "compaction",
            300,
            50,
            None,
            json!({"status": "failed", "reason": "auto", "error": {"type": "compaction.failed", "message": "x"},
                   "tokens": {"input": 4000, "output": 0, "reasoning": 0, "cache": {"read": 0, "write": 0}},
                   "time": {"created": 300}}),
        )]);
        assert!(messages(&ops).is_empty());
        assert_eq!(usage_ops(&ops).len(), 1);
        assert_eq!(usage_ops(&ops)[0].2, None);
    }

    #[test]
    fn title_usage_events_count_and_compaction_events_do_not() {
        let event = |id: &str, source: &str| {
            line(
                spec::USAGE_V2_QUERY,
                json!({
                    "event_id": id,
                    "seq": 3,
                    "created": 400,
                    "event_type": "session.usage.recorded.1",
                    "data": json!({"sessionID": "ses_1", "source": source, "cost": 0.001,
                                   "tokens": {"input": 50, "output": 8, "reasoning": 0, "cache": {"read": 0, "write": 0}}}).to_string()
                }),
            )
        };
        let (ops, statuses) = run(&[event("evt_1", "title"), event("evt_2", "compaction")]);
        assert_eq!(statuses, [LineStatus::Parsed, LineStatus::Parsed]);
        let usage = usage_ops(&ops);
        assert_eq!(usage.len(), 1);
        assert_eq!(
            usage[0].1,
            rosetta_core::ids::usage_id("session", "event:evt_1")
        );
        assert_eq!(usage[0].2, None);
        assert_eq!(usage[0].4, 400);
        assert_eq!((usage[0].3.input, usage[0].3.output), (Some(50), Some(8)));
        assert!(drift_codes(&ops).is_empty());
    }

    #[test]
    fn user_files_become_image_attachments() {
        let (ops, _) = run(&[message_line(
            "msg_i",
            "user",
            90,
            50,
            None,
            json!({"text": "see", "files": [
                {"data": "iVBORw0KGgo=", "mime": "image/png", "source": {"type": "inline"}, "name": "shot.png"},
                {"data": "JVBERi0=", "mime": "application/pdf", "source": {"type": "inline"}}
            ], "time": {"created": 90}}),
        )]);
        let rows = messages(&ops);
        assert_eq!(rows.len(), 1);
        let TimelineBody::User { attachments, .. } = &rows[0].body else {
            panic!("user row");
        };
        assert_eq!(attachments.len(), 1);
    }

    #[test]
    fn state_transitions_and_injections_project_nothing_and_shells_are_tool_rows() {
        let (ops, statuses) = run(&[
            message_line(
                "msg_1",
                "idle",
                1,
                0,
                None,
                json!({"outcome": "succeeded", "time": {"created": 1}}),
            ),
            message_line(
                "msg_2",
                "agent-switched",
                2,
                0,
                None,
                json!({"agent": "plan", "time": {"created": 2}}),
            ),
            message_line(
                "msg_3",
                "future-type",
                3,
                0,
                None,
                json!({"time": {"created": 3}}),
            ),
            message_line(
                "msg_4",
                "synthetic",
                3,
                0,
                None,
                json!({"text": "t", "time": {"created": 3}}),
            ),
            message_line(
                "msg_5",
                "shell",
                4,
                0,
                None,
                json!({"shellID": "sh_1", "command": "ls", "status": "completed", "time": {"created": 4, "completed": 6}}),
            ),
        ]);
        assert_eq!(
            statuses,
            [
                LineStatus::Parsed,
                LineStatus::Parsed,
                LineStatus::UnknownType,
                LineStatus::Parsed,
                LineStatus::Parsed
            ]
        );
        assert_eq!(drift_codes(&ops), vec!["unknown_type"]);
        let rows = messages(&ops);
        assert_eq!(rows.len(), 1);
        assert!(matches!(&rows[0].body, TimelineBody::ToolUse { .. }));
        assert!(ops.iter().any(|op| matches!(
            op,
            IngestOp::ToolUpdate { correlation_key, at: Some(6), .. } if correlation_key == "sh_1"
        )));
    }

    #[test]
    fn an_imported_session_keeps_its_1x_version_but_its_2x_rows_use_the_2x_tools() {
        let mut value = assistant(true);
        value["content"] = json!([{"type": "tool", "id": "call_9", "name": "shell",
            "state": {"status": "completed", "input": {"command": "true"}, "content": [{"type": "text", "text": ""}]},
            "time": {"created": 107, "completed": 109}}]);
        let (ops, statuses) = run_as(
            "1.18.23",
            &[message_line("msg_t", "assistant", 700, 50, None, value)],
        );
        assert_eq!(statuses, [LineStatus::Parsed]);
        assert!(drift_codes(&ops).is_empty(), "{ops:?}");
        let rows = messages(&ops);
        assert_eq!(rows.len(), 1);
        let TimelineBody::ToolUse { tool } = &rows[0].body else {
            panic!("tool row");
        };
        assert!(matches!(
            tool.action,
            rosetta_core::timeline::ToolAction::Shell { .. }
        ));
    }
}
