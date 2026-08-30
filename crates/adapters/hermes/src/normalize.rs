//! Stable snapshot records → canonical operations.
//!
//! Rows arrive from the shared SQLite driver as
//! `{"query": name, "row": {...}}`. Full per-session snapshots are replayed
//! from the beginning, so native message ids define presentation order and no
//! adapter-local database cursor is needed.

use rosetta_adapter_support::adapter::{LineOutcome, LineStatus, NormalizeCtx};
use rosetta_adapter_support::tool::{
    projection_record_kind, projection_shape_paths, terminal_update, unknown_interaction,
};
use rosetta_adapter_support::{Drift, Unknown, note_unknown};
use rosetta_core::rows::{
    DriftFact, MessageKind, MessageRow, Ms, SessionUsageRow, TokenSnapshot, WorkspaceObsRow,
};
use rosetta_core::{IngestOp, ids};
use serde::Deserialize;
use serde_json::Value;

use crate::raw::state::{
    KnownRecord, MessageRow as NativeMessage, ParsedRow, SessionRow, SystemPromptRow, ToolCallRow,
    ToolResultRow, parse_row,
};
use crate::spec;

const INVALID_JSON: &str = "<invalid-json>";
const UNTYPED: &str = "<untyped>";
const UNKNOWN_FIELDS: &str = "unknown_fields";
const UNKNOWN_QUERY: &str = "unknown_query";
const UNKNOWN_TABLE: &str = "unknown_table";
const UNKNOWN_SUBTYPE: &str = "unknown_subtype";
const PARSE_FAILED: &str = "parse_failed";

#[derive(Deserialize)]
struct SnapshotRecord {
    query: String,
    row: Value,
    #[serde(flatten)]
    unknown: Unknown,
}

#[derive(Deserialize)]
struct SchemaInventoryRow {
    table_name: String,
    #[serde(flatten)]
    unknown: Unknown,
}

pub fn ingest_line(ctx: &mut NormalizeCtx, stream_id: &str, line: &[u8]) -> LineOutcome {
    let Ok(text) = std::str::from_utf8(line) else {
        return hard(ctx, stream_id, INVALID_JSON, PARSE_FAILED, &[], None);
    };
    if text.trim().is_empty() {
        return LineOutcome::parsed(Vec::new());
    }

    let value: Value = match serde_json::from_str(text) {
        Ok(value) => value,
        Err(_) => return hard(ctx, stream_id, INVALID_JSON, PARSE_FAILED, &[], None),
    };
    let record: SnapshotRecord = match serde_json::from_value(value.clone()) {
        Ok(record) => record,
        Err(_) => {
            let kind = value["query"].as_str().unwrap_or(UNTYPED);
            return hard(
                ctx,
                stream_id,
                kind,
                PARSE_FAILED,
                &keys(&value),
                stamp(&value["row"]),
            );
        }
    };

    let mut ops = Vec::new();
    if !record.unknown.is_empty() {
        let mut paths = Vec::new();
        note_unknown(&record.unknown, "envelope", &mut paths);
        ops.push(drift(
            ctx,
            stream_id,
            &record.query,
            UNKNOWN_FIELDS,
            &paths,
            stamp(&record.row),
        ));
    }

    if record.query == "schema_inventory" {
        return schema_inventory(ctx, stream_id, record.row, ops);
    }

    let known_query = matches!(
        record.query.as_str(),
        "sessions"
            | "messages"
            | "session_model_usage"
            | "schema_version"
            | "state_meta"
            | "system_prompts"
    );
    if !known_query {
        if spec::declared_tables().contains(&record.query) {
            return LineOutcome::parsed(ops);
        }
        ops.push(drift(
            ctx,
            stream_id,
            &record.query,
            UNKNOWN_QUERY,
            &keys(&record.row),
            stamp(&record.row),
        ));
        return LineOutcome {
            ops,
            status: LineStatus::UnknownType,
        };
    }

    let parsed = match parse_row(&record.query, record.row.clone()) {
        Ok(parsed) => parsed,
        Err(_) => {
            return hard_with(
                ctx,
                stream_id,
                &record.query,
                PARSE_FAILED,
                &keys(&record.row),
                stamp(&record.row),
                ops,
            );
        }
    };
    let ParsedRow::Known(native) = parsed else {
        // The query is canonical-declared above, so this is an intentional
        // parser no-op rather than drift.
        return LineOutcome::parsed(ops);
    };

    let paths = native.drift_paths();
    if !paths.is_empty() {
        ops.push(drift(
            ctx,
            stream_id,
            &record.query,
            UNKNOWN_FIELDS,
            &paths,
            stamp(&record.row),
        ));
    }

    let status = match &*native {
        KnownRecord::Session(row) => {
            project_session(ctx, row, &mut ops);
            LineStatus::Parsed
        }
        KnownRecord::Message(row) => project_message(ctx, stream_id, row, &mut ops),
        KnownRecord::SystemPrompt(row) => {
            project_system_prompt(ctx, row, &mut ops);
            LineStatus::Parsed
        }
        KnownRecord::SchemaVersion(row) if row.version != spec::SCHEMA_VERSION => {
            ops.push(drift(
                ctx,
                stream_id,
                "schema_version",
                UNKNOWN_SUBTYPE,
                &[format!("version={}", row.version)],
                None,
            ));
            LineStatus::UnknownType
        }
        KnownRecord::SessionModelUsage(row) => {
            project_session_usage(ctx, row, &mut ops);
            LineStatus::Parsed
        }
        KnownRecord::SchemaVersion(_) | KnownRecord::StateMeta(_) => LineStatus::Parsed,
    };
    LineOutcome { ops, status }
}

fn project_session_usage(
    ctx: &NormalizeCtx,
    row: &crate::raw::state::SessionModelUsageRow,
    ops: &mut Vec<IngestOp>,
) {
    // The native composite key is the table's replacement identity. Hashing
    // it keeps provider URLs (which can contain credentials) out of the
    // projection while preserving exact replacement semantics.
    let native_key = serde_json::to_string(&(
        &row.model,
        &row.billing_provider,
        &row.billing_base_url,
        &row.billing_mode,
        &row.task,
    ))
    .expect("usage key serializes");
    let ts = row
        .last_seen
        .or(row.first_seen)
        .and_then(seconds_ms)
        .unwrap_or(0);
    ops.push(IngestOp::SessionUsage(SessionUsageRow {
        session_id: ctx.session_id.clone(),
        key: ids::usage_id(&ctx.session_id, &native_key),
        ts,
        model: Some(row.model.clone()),
        tokens: TokenSnapshot {
            input: Some(row.input_tokens),
            output: Some(row.output_tokens),
            cache_read: Some(row.cache_read_tokens),
            cache_write: Some(row.cache_write_tokens),
            thinking: Some(row.reasoning_tokens),
        },
    }));
}

fn schema_inventory(
    ctx: &NormalizeCtx,
    stream_id: &str,
    value: Value,
    mut ops: Vec<IngestOp>,
) -> LineOutcome {
    let row: SchemaInventoryRow = match serde_json::from_value(value.clone()) {
        Ok(row) => row,
        Err(_) => {
            return hard_with(
                ctx,
                stream_id,
                "schema_inventory",
                PARSE_FAILED,
                &keys(&value),
                None,
                ops,
            );
        }
    };
    if !row.unknown.is_empty() {
        let mut paths = Vec::new();
        note_unknown(&row.unknown, "schema_inventory", &mut paths);
        ops.push(drift(
            ctx,
            stream_id,
            "schema_inventory",
            UNKNOWN_FIELDS,
            &paths,
            None,
        ));
    }
    if spec::declared_tables().contains(&row.table_name) {
        return LineOutcome::parsed(ops);
    }
    ops.push(drift(
        ctx,
        stream_id,
        &row.table_name,
        UNKNOWN_TABLE,
        &[],
        None,
    ));
    LineOutcome {
        ops,
        status: LineStatus::UnknownType,
    }
}

fn project_session(ctx: &NormalizeCtx, row: &SessionRow, ops: &mut Vec<IngestOp>) {
    let (Some(cwd), Some(at)) = (
        row.cwd.as_deref().filter(|cwd| !cwd.is_empty()),
        seconds_ms(row.started_at),
    ) else {
        return;
    };
    ops.push(IngestOp::WorkspaceObs(WorkspaceObsRow {
        session_id: ctx.session_id.clone(),
        at,
        cwd: cwd.to_owned(),
        git_branch: row.git_branch.clone().filter(|branch| !branch.is_empty()),
        git_repo: row.git_repo_root.clone().filter(|repo| !repo.is_empty()),
    }));
}

fn project_system_prompt(ctx: &mut NormalizeCtx, row: &SystemPromptRow, ops: &mut Vec<IngestOp>) {
    let discriminator = format!("system-prompt:{}", row.hash);
    let mut message = message_row(ctx, &discriminator, MessageKind::System, None);
    message.subtype = Some("system_prompt".to_owned());
    message.uuid = Some(row.hash.clone());
    message.content = Some(row.prompt.clone());
    IngestOp::push_message(ops, message);
}

fn project_message(
    ctx: &mut NormalizeCtx,
    stream_id: &str,
    native: &NativeMessage,
    ops: &mut Vec<IngestOp>,
) -> LineStatus {
    let at = seconds_ms(native.timestamp);
    match native.role.as_str() {
        "user" => {
            let discriminator = format!("message:{}:content", native.id);
            let mut row = message_row(ctx, &discriminator, MessageKind::User, at);
            row.uuid = Some(native.id.to_string());
            row.content = native.content.clone();
            IngestOp::push_message(ops, row);
            LineStatus::Parsed
        }
        "assistant" => project_assistant(ctx, stream_id, native, at, ops),
        "tool" => project_tool_result(ctx, stream_id, native, at, ops),
        "system" => {
            let discriminator = format!("message:{}:content", native.id);
            let mut row = message_row(ctx, &discriminator, MessageKind::System, at);
            row.uuid = Some(native.id.to_string());
            row.content = native.content.clone();
            IngestOp::push_message(ops, row);
            LineStatus::Parsed
        }
        other => {
            ops.push(drift(
                ctx,
                stream_id,
                &format!("messages/role/{other}"),
                UNKNOWN_SUBTYPE,
                &[],
                at,
            ));
            LineStatus::UnknownType
        }
    }
}

fn project_assistant(
    ctx: &mut NormalizeCtx,
    stream_id: &str,
    native: &NativeMessage,
    at: Option<Ms>,
    ops: &mut Vec<IngestOp>,
) -> LineStatus {
    let calls = match native.tool_calls.as_deref() {
        None => Vec::new(),
        Some(text) => match serde_json::from_str::<Vec<ToolCallRow>>(text) {
            Ok(calls) => calls,
            Err(_) => {
                return hard_into(
                    ctx,
                    stream_id,
                    "messages/tool_calls",
                    PARSE_FAILED,
                    &[],
                    at,
                    ops,
                );
            }
        },
    };

    let has_content = native
        .content
        .as_deref()
        .is_some_and(|text| !text.is_empty());
    if has_content || calls.is_empty() {
        let discriminator = format!("message:{}:content", native.id);
        let mut row = message_row(ctx, &discriminator, MessageKind::Agent, at);
        row.uuid = Some(native.id.to_string());
        row.content = native.content.clone().filter(|text| !text.is_empty());
        IngestOp::push_message(ops, row);
    }

    let mut status = LineStatus::Parsed;
    for call in calls {
        let paths = call.drift_paths();
        if !paths.is_empty() {
            ops.push(drift(
                ctx,
                stream_id,
                "messages/tool_calls",
                UNKNOWN_FIELDS,
                &paths,
                at,
            ));
        }
        if call.call_type != "function"
            || !spec::declared_tool_names().contains(&call.function.name)
        {
            ops.push(drift(
                ctx,
                stream_id,
                &format!("tool_call/{}", call.function.name),
                UNKNOWN_SUBTYPE,
                &[],
                at,
            ));
            status = LineStatus::UnknownType;
        }

        let tool_id = call.call_id.clone();
        let name = call.function.name.clone();
        let input = decode_arguments(&call.function.arguments);
        let discriminator = format!("message:{}:tool:{tool_id}", native.id);
        let mut row = message_row(ctx, &discriminator, MessageKind::ToolUse, at);
        row.uuid = Some(call.id);
        let projected = crate::tool::project_request(
            ctx.agent_version.as_deref(),
            &name,
            &input,
            None,
            &tool_id,
            Some(&tool_id),
        );
        row.tool = Some(match projected {
            Ok(tool) => tool,
            Err(error) => {
                ops.push(drift(
                    ctx,
                    stream_id,
                    &projection_record_kind("request", None, &name),
                    error.drift_code(),
                    &projection_shape_paths(&input),
                    at,
                ));
                unknown_interaction(
                    tool_id.clone(),
                    rosetta_core::CorrelationDerivation::NativeId,
                    Some(tool_id.clone()),
                    None,
                    name,
                    None,
                )
            }
        });
        IngestOp::push_message(ops, row);
    }
    status
}

fn project_tool_result(
    ctx: &NormalizeCtx,
    stream_id: &str,
    native: &NativeMessage,
    at: Option<Ms>,
    ops: &mut Vec<IngestOp>,
) -> LineStatus {
    let Some(tool_id) = native.tool_call_id.clone() else {
        return hard_into(
            ctx,
            stream_id,
            "messages/tool_result",
            "missing_tool_call_id",
            &[],
            at,
            ops,
        );
    };
    let Some(content) = native.content.as_deref() else {
        return hard_into(
            ctx,
            stream_id,
            "messages/tool_result",
            "missing_content",
            &[],
            at,
            ops,
        );
    };
    let mut status = LineStatus::Parsed;
    // `content` is TEXT, not a single terminal-result schema. Hermes stores
    // both plain output and tool-specific JSON objects here. The fixture's
    // `{output, exit_code, error}` shape gets its precise typed treatment;
    // every other representation remains lossless and becomes soft drift
    // rather than making the whole snapshot generation unpublishable.
    let value = serde_json::from_str(content).unwrap_or_else(|_| Value::String(content.to_owned()));
    let (result_text, is_error) = match serde_json::from_value::<ToolResultRow>(value.clone()) {
        Ok(result) => {
            let paths = result.drift_paths();
            if !paths.is_empty() {
                ops.push(drift(
                    ctx,
                    stream_id,
                    "messages/tool_result",
                    UNKNOWN_FIELDS,
                    &paths,
                    at,
                ));
            }
            (
                Some(result.output),
                Some(result.exit_code != 0 || result.error.is_some()),
            )
        }
        Err(_) => {
            ops.push(drift(
                ctx,
                stream_id,
                "messages/tool_result",
                UNKNOWN_SUBTYPE,
                &keys(&value),
                at,
            ));
            status = LineStatus::UnknownType;
            (tool_result_text(&value), tool_result_error(&value))
        }
    };

    if native
        .tool_name
        .as_deref()
        .is_some_and(|name| !spec::declared_tool_names().contains(name))
    {
        let name = native.tool_name.as_deref().unwrap_or(UNTYPED);
        ops.push(drift(
            ctx,
            stream_id,
            &format!("tool_result/{name}"),
            UNKNOWN_SUBTYPE,
            &[],
            at,
        ));
        status = LineStatus::UnknownType;
    }
    ops.push(IngestOp::ToolUpdate {
        session_id: ctx.session_id.clone(),
        correlation_key: tool_id,
        evidence: ctx.evidence(0),
        patch: Box::new(terminal_update(
            is_error,
            true,
            rosetta_core::StatusBasis::NativeState,
            at,
            result_text.as_deref(),
        )),
        at,
    });
    status
}

fn tool_result_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Object(object) => object
            .get("output")
            .or_else(|| object.get("content"))
            .or_else(|| object.get("message"))
            .or_else(|| object.get("matches_text"))
            .or_else(|| object.get("error"))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| serde_json::to_string(value).ok()),
        Value::Array(_) => serde_json::to_string(value).ok(),
        Value::Number(_) | Value::Bool(_) => Some(value.to_string()),
        Value::Null => None,
    }
}

fn tool_result_error(value: &Value) -> Option<bool> {
    let object = value.as_object()?;
    let mut observed = false;
    let mut is_error = false;

    if let Some(exit_code) = object.get("exit_code").and_then(Value::as_i64) {
        observed = true;
        is_error |= exit_code != 0;
    }
    if let Some(error) = object.get("error") {
        observed = true;
        is_error |= match error {
            Value::Null => false,
            Value::Bool(value) => *value,
            Value::String(text) => !text.is_empty(),
            Value::Array(values) => !values.is_empty(),
            Value::Object(values) => !values.is_empty(),
            Value::Number(_) => true,
        };
    }
    if let Some(success) = object.get("success").and_then(Value::as_bool) {
        observed = true;
        is_error |= !success;
    }
    if let Some(status) = object.get("status").and_then(Value::as_str) {
        match status.to_ascii_lowercase().as_str() {
            "error" | "failed" | "failure" | "cancelled" | "canceled" => {
                observed = true;
                is_error = true;
            }
            "ok" | "success" | "complete" | "completed" => observed = true,
            _ => {}
        }
    }

    observed.then_some(is_error)
}

fn message_row(
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

fn decode_arguments(arguments: &str) -> Value {
    serde_json::from_str(arguments).unwrap_or_else(|_| Value::String(arguments.to_owned()))
}

fn seconds_ms(seconds: f64) -> Option<Ms> {
    let millis = seconds * 1_000.0;
    (millis.is_finite() && millis >= i64::MIN as f64 && millis <= i64::MAX as f64)
        .then(|| millis.trunc() as i64)
}

fn stamp(value: &Value) -> Option<Ms> {
    value
        .get("timestamp")
        .and_then(Value::as_f64)
        .and_then(seconds_ms)
}

fn keys(value: &Value) -> Vec<String> {
    value
        .as_object()
        .map(|object| object.keys().cloned().collect())
        .unwrap_or_else(|| vec!["<non-object>".to_owned()])
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

fn hard(
    ctx: &NormalizeCtx,
    stream_id: &str,
    record_kind: &str,
    code: &str,
    paths: &[String],
    at: Option<Ms>,
) -> LineOutcome {
    hard_with(ctx, stream_id, record_kind, code, paths, at, Vec::new())
}

fn hard_with(
    ctx: &NormalizeCtx,
    stream_id: &str,
    record_kind: &str,
    code: &str,
    paths: &[String],
    at: Option<Ms>,
    mut ops: Vec<IngestOp>,
) -> LineOutcome {
    ops.push(drift(ctx, stream_id, record_kind, code, paths, at));
    LineOutcome {
        ops,
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
    paths: &[String],
    at: Option<Ms>,
    ops: &mut Vec<IngestOp>,
) -> LineStatus {
    ops.push(drift(ctx, stream_id, record_kind, code, paths, at));
    LineStatus::HardError {
        code: code.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::{decode_arguments, seconds_ms};
    use serde_json::json;

    #[test]
    fn epoch_seconds_truncate_to_milliseconds() {
        assert_eq!(seconds_ms(1_787_635_893.360_734), Some(1_787_635_893_360));
        assert_eq!(seconds_ms(f64::NAN), None);
    }

    #[test]
    fn tool_arguments_keep_unparseable_text() {
        assert_eq!(
            decode_arguments(r#"{"command":"echo rosetta-mock"}"#),
            json!({"command": "echo rosetta-mock"})
        );
        assert_eq!(decode_arguments("not-json"), json!("not-json"));
    }
}
