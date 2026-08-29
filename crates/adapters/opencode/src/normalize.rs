//! Deterministic SQLite snapshot rows → canonical operations.
//!
//! `message.data` and `part.data` remain the conversational truth. The shared
//! snapshot driver only supplies relational envelopes and stable row order; it
//! never moves OpenCode's grammar out of this crate.

use std::collections::BTreeSet;

use rosetta_adapter_support::adapter::{LineOutcome, LineStatus, NormalizeCtx};
use rosetta_adapter_support::{Drift, Unknown, note_unknown};
use rosetta_core::rows::{
    DriftFact, EditKind, FileEditRow, MessageKind, MessageRow, SessionUsageRow, TokenSnapshot,
    ToolCall, WorkspaceObsRow,
};
use rosetta_core::{IngestOp, ids};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::raw::{MessageData, ParsedMessage, ParsedPart, PartData, parse_message, parse_part};
use crate::spec;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct OpenCodeState {
    /// Message usage is repeated on every joined part; attach it once.
    usage_messages: BTreeSet<String>,
    /// Assistant envelopes repeat their cwd on every part.
    workspace: Option<String>,
}

#[derive(Deserialize)]
struct SnapshotEnvelope {
    query: String,
    row: Value,
    #[serde(flatten)]
    unknown: Unknown,
}

#[derive(Deserialize)]
struct TimelineRecord {
    message_id: String,
    message_data: String,
    part_id: String,
    part_time_created: i64,
    part_data: String,
    first_event_seq: Option<i64>,
    #[serde(flatten)]
    unknown: Unknown,
}

pub fn ingest(
    ctx: &mut NormalizeCtx,
    state: &mut OpenCodeState,
    stream_id: &str,
    line: &[u8],
) -> LineOutcome {
    let envelope = match serde_json::from_slice::<SnapshotEnvelope>(line) {
        Ok(envelope) => envelope,
        Err(_) => return hard(ctx, stream_id, "<snapshot>", "invalid_json", None),
    };
    if envelope.query != spec::TIMELINE_QUERY {
        return LineOutcome {
            ops: vec![drift(
                ctx,
                stream_id,
                &format!("snapshot/{}", envelope.query),
                "unknown_query",
                &[],
                None,
            )],
            status: LineStatus::UnknownType,
        };
    }

    let mut outer_paths = Vec::new();
    note_unknown(&envelope.unknown, "snapshot", &mut outer_paths);
    let record = match serde_json::from_value::<TimelineRecord>(envelope.row) {
        Ok(record) => record,
        Err(_) => {
            return hard(ctx, stream_id, "snapshot/timeline", "parse_failed", None);
        }
    };
    note_unknown(&record.unknown, "snapshot.row", &mut outer_paths);

    let message = match parse_message(&record.message_data) {
        Ok(ParsedMessage::Known(message)) => message,
        Ok(ParsedMessage::UnknownType { role, .. }) => {
            let role = role.unwrap_or_else(|| "<untyped>".to_owned());
            if spec::declared_message_roles().contains(&role) {
                return LineOutcome::parsed(Vec::new());
            }
            return LineOutcome {
                ops: vec![drift(
                    ctx,
                    stream_id,
                    &format!("message/{role}"),
                    "unknown_type",
                    &[],
                    Some(record.part_time_created),
                )],
                status: LineStatus::UnknownType,
            };
        }
        Err(_) => {
            return hard(
                ctx,
                stream_id,
                &format!("message/{}", discriminant(&record.message_data, "role")),
                "parse_failed",
                Some(record.part_time_created),
            );
        }
    };
    let part = match parse_part(&record.part_data) {
        Ok(ParsedPart::Known(part)) => part,
        Ok(ParsedPart::UnknownType { rtype, .. }) => {
            let rtype = rtype.unwrap_or_else(|| "<untyped>".to_owned());
            if spec::declared_part_types().contains(&rtype) {
                return LineOutcome::parsed(Vec::new());
            }
            return LineOutcome {
                ops: vec![drift(
                    ctx,
                    stream_id,
                    &format!("part/{rtype}"),
                    "unknown_type",
                    &[],
                    Some(record.part_time_created),
                )],
                status: LineStatus::UnknownType,
            };
        }
        Err(_) => {
            return hard(
                ctx,
                stream_id,
                &format!("part/{}", discriminant(&record.part_data, "type")),
                "parse_failed",
                Some(record.part_time_created),
            );
        }
    };

    let mut ops = Vec::new();
    let mut paths = outer_paths;
    paths.extend(message.drift_paths());
    paths.extend(part.drift_paths());
    if !paths.is_empty() {
        ops.push(drift(
            ctx,
            stream_id,
            record_kind(&message, &part),
            "unknown_fields",
            &paths,
            Some(record.part_time_created),
        ));
    }
    if record.first_event_seq.is_none() {
        ops.push(drift(
            ctx,
            stream_id,
            record_kind(&message, &part),
            "missing_event_sequence",
            &[],
            Some(record.part_time_created),
        ));
    }

    observe_workspace(ctx, state, &message, record.part_time_created, &mut ops);

    let status = project(ctx, stream_id, state, &record, &message, &part, &mut ops);
    LineOutcome { ops, status }
}

fn project(
    ctx: &mut NormalizeCtx,
    stream_id: &str,
    state: &mut OpenCodeState,
    record: &TimelineRecord,
    message: &MessageData,
    part: &PartData,
    ops: &mut Vec<IngestOp>,
) -> LineStatus {
    match part {
        PartData::Text(text) => {
            let kind = match message {
                MessageData::User(_) => MessageKind::User,
                MessageData::Assistant(_) => MessageKind::Agent,
            };
            let mut row = timeline_row(ctx, record, message, kind);
            row.content = Some(text.text.clone());
            attach_usage(state, &record.message_id, message, &mut row);
            ops.push(IngestOp::Message(Box::new(row)));
            LineStatus::Parsed
        }
        PartData::Tool(tool) => {
            let MessageData::Assistant(_) = message else {
                ops.push(drift(
                    ctx,
                    stream_id,
                    "part/tool",
                    "tool_on_user_message",
                    &[],
                    Some(record.part_time_created),
                ));
                return LineStatus::HardError {
                    code: "tool_on_user_message".to_owned(),
                };
            };
            let mut row = timeline_row(ctx, record, message, MessageKind::ToolUse);
            row.tool = Some(ToolCall {
                tool_id: Some(tool.call_id.clone()),
                name: tool.tool.clone(),
                input: tool.state.get("input").cloned().unwrap_or(Value::Null),
                result: None,
                payload: None,
                is_error: None,
            });
            attach_usage(state, &record.message_id, message, &mut row);
            ops.push(IngestOp::Message(Box::new(row)));

            match tool.state.get("status").and_then(Value::as_str) {
                Some("completed") => ops.push(IngestOp::ToolResult {
                    session_id: ctx.session_id.clone(),
                    tool_id: tool.call_id.clone(),
                    result: tool
                        .state
                        .get("output")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    payload: tool
                        .state
                        .get("metadata")
                        .filter(|value| !value.is_null())
                        .cloned(),
                    is_error: Some(false),
                    at: tool
                        .state
                        .get("time")
                        .and_then(|time| time.get("end"))
                        .and_then(Value::as_u64)
                        .and_then(|at| i64::try_from(at).ok())
                        .or(Some(record.part_time_created)),
                }),
                Some("error") => ops.push(IngestOp::ToolResult {
                    session_id: ctx.session_id.clone(),
                    tool_id: tool.call_id.clone(),
                    result: tool
                        .state
                        .get("error")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    payload: tool
                        .state
                        .get("metadata")
                        .filter(|value| !value.is_null())
                        .cloned(),
                    is_error: Some(true),
                    at: tool
                        .state
                        .get("time")
                        .and_then(|time| time.get("end"))
                        .and_then(Value::as_u64)
                        .and_then(|at| i64::try_from(at).ok())
                        .or(Some(record.part_time_created)),
                }),
                Some("pending" | "running") => {}
                Some(status) => ops.push(drift(
                    ctx,
                    stream_id,
                    "part/tool",
                    &format!("unknown_tool_status:{status}"),
                    &[],
                    Some(record.part_time_created),
                )),
                None => ops.push(drift(
                    ctx,
                    stream_id,
                    "part/tool",
                    "missing_tool_status",
                    &[],
                    Some(record.part_time_created),
                )),
            }
            LineStatus::Parsed
        }
        PartData::Reasoning(reasoning) => {
            let MessageData::Assistant(_) = message else {
                ops.push(drift(
                    ctx,
                    stream_id,
                    "user/reasoning",
                    "reasoning_on_user_message",
                    &[],
                    Some(record.part_time_created),
                ));
                return LineStatus::HardError {
                    code: "reasoning_on_user_message".to_owned(),
                };
            };
            let mut row = timeline_row(ctx, record, message, MessageKind::Thinking);
            row.ts = i64::try_from(reasoning.time.start)
                .ok()
                .or(Some(record.part_time_created));
            row.content = Some(reasoning.text.clone());
            attach_usage(state, &record.message_id, message, &mut row);
            ops.push(IngestOp::Message(Box::new(row)));
            LineStatus::Parsed
        }
        PartData::Patch(patch) => {
            for path in &patch.files {
                ops.push(IngestOp::FileEdit(FileEditRow {
                    session_id: ctx.session_id.clone(),
                    message_timeline_id: None,
                    path: path.clone(),
                    kind: EditKind::Checkpoint,
                    snapshot_ref: Some(patch.hash.clone()),
                    lines_added: None,
                    lines_removed: None,
                    at: Some(record.part_time_created),
                }));
            }
            LineStatus::Parsed
        }
        PartData::Retry(retry) => {
            let mut row = timeline_row(ctx, record, message, MessageKind::System);
            row.subtype = Some("retry".to_owned());
            row.ts = i64::try_from(retry.time.created)
                .ok()
                .or(Some(record.part_time_created));
            row.content = retry
                .error
                .get("data")
                .and_then(|data| data.get("message"))
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or_else(|| serde_json::to_string(&retry.error).ok());
            attach_usage(state, &record.message_id, message, &mut row);
            ops.push(IngestOp::Message(Box::new(row)));
            LineStatus::Parsed
        }
        PartData::StepFinish(_) => {
            // Error/empty assistant turns can end without a text, reasoning,
            // or tool row. Preserve their exact response usage without
            // manufacturing a visible message.
            if let MessageData::Assistant(message) = message
                && state.usage_messages.insert(record.message_id.clone())
            {
                ops.push(IngestOp::SessionUsage(SessionUsageRow {
                    session_id: ctx.session_id.clone(),
                    key: ids::usage_id(&ctx.session_id, &record.message_id),
                    ts: record.part_time_created,
                    model: Some(message.model_id.clone()),
                    tokens: token_snapshot(&message.tokens),
                }));
            }
            LineStatus::Parsed
        }
        // Execution boundaries carry no canonical timeline content. Their
        // persisted fields have no lossless canonical projection.
        PartData::File(_)
        | PartData::Subtask(_)
        | PartData::StepStart(_)
        | PartData::Snapshot(_)
        | PartData::Agent(_)
        | PartData::Compaction(_) => LineStatus::Parsed,
    }
}

fn timeline_row(
    ctx: &mut NormalizeCtx,
    record: &TimelineRecord,
    message: &MessageData,
    kind: MessageKind,
) -> MessageRow {
    let (parent_uuid, model) = match message {
        MessageData::User(_) => (None, None),
        MessageData::Assistant(message) => (
            Some(message.parent_id.clone()),
            Some(message.model_id.clone()),
        ),
    };
    MessageRow {
        timeline_id: ids::timeline_id(&ctx.session_id, &record.part_id),
        session_id: ctx.session_id.clone(),
        ordinal: ctx.take_ordinal(),
        kind,
        subtype: None,
        // One native message may split into several canonical part rows.
        uuid: Some(record.message_id.clone()),
        parent_uuid,
        ts: Some(record.part_time_created),
        model,
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

fn attach_usage(
    state: &mut OpenCodeState,
    message_id: &str,
    message: &MessageData,
    row: &mut MessageRow,
) {
    let MessageData::Assistant(message) = message else {
        return;
    };
    if state.usage_messages.insert(message_id.to_owned()) {
        row.usage = Some(token_snapshot(&message.tokens));
    }
}

fn token_snapshot(tokens: &crate::raw::payload::Tokens) -> TokenSnapshot {
    TokenSnapshot {
        input: exact_i64(&tokens.input),
        output: exact_i64(&tokens.output),
        cache_read: exact_i64(&tokens.cache.read),
        cache_write: exact_i64(&tokens.cache.write),
        thinking: exact_i64(&tokens.reasoning),
    }
}

fn exact_i64(number: &serde_json::Number) -> Option<i64> {
    number
        .as_i64()
        .or_else(|| number.as_u64().and_then(|value| i64::try_from(value).ok()))
}

fn observe_workspace(
    ctx: &NormalizeCtx,
    state: &mut OpenCodeState,
    message: &MessageData,
    fallback_at: i64,
    ops: &mut Vec<IngestOp>,
) {
    let MessageData::Assistant(message) = message else {
        return;
    };
    if state.workspace.as_deref() == Some(message.path.cwd.as_str()) {
        return;
    }
    state.workspace = Some(message.path.cwd.clone());
    ops.push(IngestOp::WorkspaceObs(WorkspaceObsRow {
        session_id: ctx.session_id.clone(),
        at: i64::try_from(message.time.created).unwrap_or(fallback_at),
        cwd: message.path.cwd.clone(),
        git_branch: None,
        git_repo: None,
    }));
}

fn record_kind(message: &MessageData, part: &PartData) -> &'static str {
    match (message, part) {
        (MessageData::User(_), PartData::Text(_)) => "user/text",
        (MessageData::Assistant(_), PartData::Text(_)) => "assistant/text",
        (_, PartData::Tool(_)) => "assistant/tool",
        (_, PartData::Reasoning(_)) => "assistant/reasoning",
        (_, PartData::File(_)) => "part/file",
        (_, PartData::Subtask(_)) => "part/subtask",
        (_, PartData::StepStart(_)) => "assistant/step-start",
        (_, PartData::StepFinish(_)) => "assistant/step-finish",
        (_, PartData::Snapshot(_)) => "part/snapshot",
        (_, PartData::Patch(_)) => "part/patch",
        (_, PartData::Agent(_)) => "part/agent",
        (_, PartData::Retry(_)) => "assistant/retry",
        (_, PartData::Compaction(_)) => "part/compaction",
    }
}

fn discriminant(json: &str, key: &str) -> String {
    serde_json::from_str::<Value>(json)
        .ok()
        .and_then(|value| value[key].as_str().map(str::to_owned))
        .unwrap_or_else(|| "<untyped>".to_owned())
}

fn drift(
    ctx: &NormalizeCtx,
    stream_id: &str,
    record_kind: &str,
    error_code: &str,
    paths: &[String],
    at: Option<i64>,
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

fn hard(
    ctx: &NormalizeCtx,
    stream_id: &str,
    record_kind: &str,
    code: &str,
    at: Option<i64>,
) -> LineOutcome {
    LineOutcome {
        ops: vec![drift(ctx, stream_id, record_kind, code, &[], at)],
        status: LineStatus::HardError {
            code: code.to_owned(),
        },
    }
}

#[cfg(test)]
mod tests {
    use rosetta_adapter_support::adapter::{LineStatus, NormalizeCtx};
    use rosetta_core::IngestOp;
    use rosetta_core::rows::MessageKind;
    use serde_json::json;

    use super::{OpenCodeState, ingest};
    use crate::spec;

    fn line(message: serde_json::Value, part: serde_json::Value) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "query": spec::TIMELINE_QUERY,
            "row": {
                "message_id": "msg-1",
                "message_data": message.to_string(),
                "part_id": "part-1",
                "part_time_created": 123,
                "part_data": part.to_string(),
                "first_event_seq": 7
            }
        }))
        .unwrap()
    }

    #[test]
    fn unknown_query_is_soft_drift() {
        let mut ctx = NormalizeCtx::new("runtime", "session");
        let mut state = OpenCodeState::default();
        let outcome = ingest(
            &mut ctx,
            &mut state,
            spec::DATABASE,
            br#"{"query":"future","row":{}}"#,
        );
        assert_eq!(outcome.status, LineStatus::UnknownType);
        assert!(matches!(outcome.ops.as_slice(), [IngestOp::Drift(_)]));
    }

    #[test]
    fn user_text_uses_the_part_id_as_timeline_identity() {
        let message = json!({
            "role": "user",
            "agent": "build",
            "model": {"providerID": "mock", "modelID": "mock-model"},
            "summary": {"diffs": []},
            "time": {"created": 100}
        });
        let mut ctx = NormalizeCtx::new("runtime", "session");
        let mut state = OpenCodeState::default();
        let outcome = ingest(
            &mut ctx,
            &mut state,
            spec::DATABASE,
            &line(message, json!({"type": "text", "text": "hello"})),
        );
        assert_eq!(outcome.status, LineStatus::Parsed);
        let IngestOp::Message(row) = &outcome.ops[0] else {
            panic!("expected message");
        };
        assert_eq!(row.kind, MessageKind::User);
        assert_eq!(row.content.as_deref(), Some("hello"));
        assert_eq!(row.uuid.as_deref(), Some("msg-1"));
        assert_eq!(
            row.timeline_id,
            rosetta_core::ids::timeline_id("session", "part-1")
        );
    }

    #[test]
    fn writer_backed_reasoning_projects_as_thinking() {
        let message = json!({
            "role": "assistant",
            "agent": "build",
            "cost": 0.125,
            "mode": "build",
            "modelID": "mock-model",
            "parentID": "msg-user",
            "path": {"cwd": "/workspace", "root": "/workspace"},
            "providerID": "mock",
            "time": {"created": 100},
            "tokens": {
                "input": 10,
                "output": 4,
                "reasoning": 3,
                "cache": {"read": 2, "write": 1}
            }
        });
        let mut ctx = NormalizeCtx::new("runtime", "session");
        let mut state = OpenCodeState::default();
        let outcome = ingest(
            &mut ctx,
            &mut state,
            spec::DATABASE,
            &line(
                message,
                json!({
                    "type": "reasoning",
                    "text": "considering the options",
                    "metadata": {"provider": {"signature": "opaque"}},
                    "time": {"start": 110}
                }),
            ),
        );
        assert_eq!(outcome.status, LineStatus::Parsed);
        let message = outcome
            .ops
            .iter()
            .find_map(|op| match op {
                IngestOp::Message(row) => Some(row),
                _ => None,
            })
            .expect("thinking row");
        assert_eq!(message.kind, MessageKind::Thinking);
        assert_eq!(message.content.as_deref(), Some("considering the options"));
        assert_eq!(message.ts, Some(110));
        assert_eq!(message.usage.expect("usage").thinking, Some(3));
    }

    #[test]
    fn writer_backed_tool_errors_merge_into_the_tool_row() {
        let message = json!({
            "role": "assistant",
            "agent": "build",
            "cost": 0,
            "mode": "build",
            "modelID": "mock-model",
            "parentID": "msg-user",
            "path": {"cwd": "/workspace", "root": "/workspace"},
            "providerID": "mock",
            "time": {"created": 100, "completed": 130},
            "tokens": {
                "total": 14,
                "input": 10,
                "output": 4,
                "reasoning": 0,
                "cache": {"read": 0, "write": 0}
            },
            "finish": "tool-calls"
        });
        let mut ctx = NormalizeCtx::new("runtime", "session");
        let mut state = OpenCodeState::default();
        let outcome = ingest(
            &mut ctx,
            &mut state,
            spec::DATABASE,
            &line(
                message,
                json!({
                    "type": "tool",
                    "callID": "call-1",
                    "tool": "bash",
                    "state": {
                        "status": "error",
                        "input": {"command": "false"},
                        "error": "exit status 1",
                        "time": {"start": 120, "end": 125}
                    }
                }),
            ),
        );
        assert_eq!(outcome.status, LineStatus::Parsed);
        assert!(matches!(
            outcome.ops.as_slice(),
            [IngestOp::WorkspaceObs(_), IngestOp::Message(_), IngestOp::ToolResult {
                result: Some(result),
                is_error: Some(true),
                at: Some(125),
                ..
            }] if result == "exit status 1"
        ));
    }
}
