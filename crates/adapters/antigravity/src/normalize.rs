//! Primary Antigravity transcript records to canonical operations.
//!
//! The native `step_index` is the stable timeline discriminator. Tool calls
//! add their array index because one planner response may carry several calls.
//! Results have no native call id: the stream's ordered result records and
//! result kinds are their only correlation facts, so a small checkpointed queue
//! carries the synthesized, deterministic call id across lines.

use std::collections::BTreeMap;

use rosetta_adapter_support::Drift;
use rosetta_adapter_support::adapter::{LineOutcome, LineStatus, NormalizeCtx};
use rosetta_adapter_support::tool::{
    projection_record_kind, projection_shape_paths, terminal_update, unknown_interaction,
};
use rosetta_core::rows::{DriftFact, MessageKind, MessageRow, Ms};
use rosetta_core::{IngestOp, ids};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::raw::transcript::{
    ErrorMessageLine, KnownRecord, ListDirectoryLine, PlannerResponseLine, RunCommandLine,
    ViewFileLine,
};
use crate::raw::{ParsedLine, parse_line};
use crate::spec;
use crate::time::rfc3339_ms;

const UNKNOWN_TYPE: &str = "unknown_type";
const UNKNOWN_FIELDS: &str = "unknown_fields";
const PARSE_FAILED: &str = "parse_failed";
const INVALID_JSON: &str = "invalid_json";
const INVALID_UTF8: &str = "invalid_utf8";
const ORPHAN_TOOL_RESULT: &str = "orphan_tool_result";
const NO_TYPE: &str = "<untyped>";
const INVALID_RECORD: &str = "<invalid-json>";

/// Cross-line state. The transcript stream queues pending tool calls so
/// result records can merge into earlier tool rows; the conversation-store
/// stream indexes generations so each step's usage can name the model that
/// served it (see [`crate::usage`]). Both hold identifiers only.
#[derive(Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AntigravityState {
    pending_tools: Vec<PendingTool>,
    /// `steps.idx` → the served model string of the generation that wrote
    /// that step, `None` when the generation recorded no model string.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) generations: BTreeMap<u32, Option<String>>,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingTool {
    tool_id: String,
    name: String,
}

impl AntigravityState {
    fn push(&mut self, tool_id: String, name: String) {
        self.pending_tools.push(PendingTool { tool_id, name });
    }

    /// Typed result records select the oldest pending call with the matching
    /// name. `ERROR_MESSAGE` has no tool-name field, so it selects the oldest
    /// pending call regardless of name. Both rules use only native stream order.
    fn take(&mut self, expected_name: Option<&str>) -> Option<String> {
        let index = match expected_name {
            Some(name) => self
                .pending_tools
                .iter()
                .position(|pending| pending.name == name)?,
            None => (!self.pending_tools.is_empty()).then_some(0)?,
        };
        Some(self.pending_tools.remove(index).tool_id)
    }
}

pub fn ingest(
    ctx: &mut NormalizeCtx,
    state: &mut AntigravityState,
    stream_id: &str,
    line: &[u8],
) -> LineOutcome {
    let Ok(text) = std::str::from_utf8(line) else {
        return hard(ctx, stream_id, INVALID_RECORD, INVALID_UTF8, &[], None);
    };
    if text.trim().is_empty() {
        return LineOutcome::parsed(Vec::new());
    }

    let parsed = match parse_line(text) {
        Ok(parsed) => parsed,
        Err(_) => return parse_failure(ctx, stream_id, text),
    };
    match parsed {
        ParsedLine::UnknownType { rtype, value } => {
            let kind = rtype.unwrap_or_else(|| NO_TYPE.to_owned());
            let op = drift(
                ctx,
                stream_id,
                &kind,
                UNKNOWN_TYPE,
                &top_level_keys(&value),
                stamp(&value),
            );
            LineOutcome {
                ops: vec![op],
                status: LineStatus::UnknownType,
            }
        }
        ParsedLine::Known(record) => normalize(ctx, state, stream_id, &record),
    }
}

fn normalize(
    ctx: &mut NormalizeCtx,
    state: &mut AntigravityState,
    stream_id: &str,
    record: &KnownRecord,
) -> LineOutcome {
    let (kind, at) = record_identity(record);
    let mut emit = Emit::new(ctx, stream_id);

    let paths = record.drift_paths();
    if !paths.is_empty() {
        emit.drift(kind, UNKNOWN_FIELDS, &paths, at);
    }

    let status = match record {
        KnownRecord::UserInput(line) => {
            let mut row = emit.row(line.step_index, "user", MessageKind::User, at);
            row.content = Some(line.content.clone());
            emit.message(row);
            LineStatus::Parsed
        }
        KnownRecord::PlannerResponse(line) => planner_response(&mut emit, state, line, at),
        KnownRecord::RunCommand(line) => run_command(&mut emit, state, line, at),
        KnownRecord::ListDirectory(line) => list_directory(&mut emit, state, line, at),
        KnownRecord::ViewFile(line) => view_file(&mut emit, state, line, at),
        KnownRecord::ErrorMessage(line) => error_message(&mut emit, state, line, at),
        KnownRecord::Checkpoint(line) => {
            let mut row = emit.row(line.step_index, "checkpoint", MessageKind::Summary, at);
            row.subtype = Some("CHECKPOINT".to_owned());
            row.content = Some(line.content.clone());
            emit.message(row);
            if let Some(title) = crate::raw::transcript::checkpoint_objective(&line.content) {
                let mut row = emit.row(line.step_index, "title-fallback", MessageKind::Summary, at);
                row.subtype = Some("title_fallback".to_owned());
                row.content = Some(title);
                emit.message(row);
            }
            LineStatus::Parsed
        }
        KnownRecord::SystemMessage(line) => {
            let mut row = emit.row(line.step_index, "system", MessageKind::System, at);
            row.subtype = Some("SYSTEM_MESSAGE".to_owned());
            row.content = Some(line.content.clone());
            emit.message(row);
            LineStatus::Parsed
        }
        KnownRecord::Generic(line) => {
            let message_kind = match line.source.as_str() {
                "MODEL" => MessageKind::Agent,
                "USER" => MessageKind::User,
                _ => MessageKind::System,
            };
            let mut row = emit.row(line.step_index, "generic", message_kind, at);
            row.subtype = Some("GENERIC".to_owned());
            row.content = Some(line.content.clone());
            emit.message(row);
            LineStatus::Parsed
        }
        // Declared and intentionally unprojected: this record carries only the
        // common envelope and no content in the canonical ground truth.
        KnownRecord::ConversationHistory(_) => LineStatus::Parsed,
    };

    LineOutcome {
        ops: emit.ops,
        status,
    }
}

fn planner_response(
    emit: &mut Emit<'_>,
    state: &mut AntigravityState,
    line: &PlannerResponseLine,
    at: Option<Ms>,
) -> LineStatus {
    if let Some(content) = &line.content {
        let mut row = emit.row(line.step_index, "agent", MessageKind::Agent, at);
        row.content = Some(content.clone());
        emit.message(row);
    }

    let calls = line.tool_calls.as_deref().unwrap_or_default();
    for (index, call) in calls.iter().enumerate() {
        let discriminator = tool_discriminator(line.step_index, index);
        let mut row = emit.row_with_discriminator(&discriminator, MessageKind::ToolUse, at);
        let tool_id = discriminator.clone();
        let projected = crate::tool::project_request(
            emit.ctx.agent_version.as_deref(),
            &call.name,
            &call.args,
            None,
            &tool_id,
        );
        row.tool = Some(match projected {
            Ok(tool) => tool,
            Err(error) => {
                emit.drift(
                    &projection_record_kind("request", None, &call.name),
                    error.drift_code(),
                    &projection_shape_paths(&call.args),
                    at,
                );
                unknown_interaction(
                    tool_id.clone(),
                    rosetta_core::CorrelationDerivation::Positional,
                    None,
                    None,
                    call.name.clone(),
                    None,
                )
            }
        });
        emit.message(row);
        state.push(tool_id, call.name.clone());
    }

    // An otherwise empty planner response is still an assistant timeline event.
    if line.content.is_none() && calls.is_empty() {
        let row = emit.row(line.step_index, "agent", MessageKind::Agent, at);
        emit.message(row);
    }
    LineStatus::Parsed
}

fn run_command(
    emit: &mut Emit<'_>,
    state: &mut AntigravityState,
    line: &RunCommandLine,
    at: Option<Ms>,
) -> LineStatus {
    tool_result(
        emit,
        state,
        "RUN_COMMAND",
        Some("run_command"),
        Some(line.content.clone()),
        None,
        Some(line.exit_code != 0),
        at,
    )
}

fn list_directory(
    emit: &mut Emit<'_>,
    state: &mut AntigravityState,
    line: &ListDirectoryLine,
    at: Option<Ms>,
) -> LineStatus {
    tool_result(
        emit,
        state,
        "LIST_DIRECTORY",
        Some("list_dir"),
        Some(line.content.clone()),
        None,
        None,
        at,
    )
}

fn view_file(
    emit: &mut Emit<'_>,
    state: &mut AntigravityState,
    line: &ViewFileLine,
    at: Option<Ms>,
) -> LineStatus {
    tool_result(
        emit,
        state,
        "VIEW_FILE",
        Some("view_file"),
        Some(line.content.clone()),
        None,
        None,
        at,
    )
}

fn error_message(
    emit: &mut Emit<'_>,
    state: &mut AntigravityState,
    line: &ErrorMessageLine,
    at: Option<Ms>,
) -> LineStatus {
    tool_result(
        emit,
        state,
        "ERROR_MESSAGE",
        None,
        Some(line.content.clone()),
        Some(Value::String(line.error.clone())),
        Some(true),
        at,
    )
}

#[allow(clippy::too_many_arguments)]
fn tool_result(
    emit: &mut Emit<'_>,
    state: &mut AntigravityState,
    record_kind: &str,
    expected_name: Option<&str>,
    result: Option<String>,
    _payload: Option<Value>,
    is_error: Option<bool>,
    at: Option<Ms>,
) -> LineStatus {
    let Some(tool_id) = state.take(expected_name) else {
        emit.drift(record_kind, ORPHAN_TOOL_RESULT, &[], at);
        return LineStatus::HardError {
            code: ORPHAN_TOOL_RESULT.to_owned(),
        };
    };
    emit.ops.push(IngestOp::ToolUpdate {
        session_id: emit.session_id.clone(),
        correlation_key: tool_id,
        evidence: emit.ctx.evidence(0),
        patch: Box::new(terminal_update(
            is_error,
            true,
            rosetta_core::StatusBasis::NativeTerminalRecord,
            at,
            result.as_deref(),
        )),
        at,
    });
    LineStatus::Parsed
}

fn tool_discriminator(step_index: u64, call_index: usize) -> String {
    format!("step#{step_index}:tool#{call_index}")
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

    fn row(
        &mut self,
        step_index: u64,
        suffix: &str,
        kind: MessageKind,
        ts: Option<Ms>,
    ) -> MessageRow {
        self.row_with_discriminator(&format!("step#{step_index}:{suffix}"), kind, ts)
    }

    fn row_with_discriminator(
        &mut self,
        discriminator: &str,
        kind: MessageKind,
        ts: Option<Ms>,
    ) -> MessageRow {
        MessageRow {
            timeline_id: ids::timeline_id(&self.session_id, discriminator),
            session_id: self.session_id.clone(),
            ordinal: self.ctx.take_ordinal(),
            kind,
            subtype: None,
            // Antigravity transcript records carry no UUID or parent UUID.
            uuid: None,
            parent_uuid: None,
            ts,
            // No model, effort, or usage fields exist in this stream.
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
            agent_version: self.ctx.agent_version.clone(),
            src_cursor: self.ctx.src_cursor,
            src_offset: self.ctx.src_offset,
        }
    }

    fn message(&mut self, row: MessageRow) {
        IngestOp::push_message(&mut self.ops, row);
    }

    fn drift(&mut self, record_kind: &str, code: &str, paths: &[String], at: Option<Ms>) {
        self.ops.push(drift(
            self.ctx,
            self.stream_id,
            record_kind,
            code,
            paths,
            at,
        ));
    }
}

fn drift(
    ctx: &NormalizeCtx,
    stream_id: &str,
    record_kind: &str,
    code: &str,
    paths: &[String],
    at: Option<Ms>,
) -> IngestOp {
    IngestOp::Drift(DriftFact {
        runtime_id: ctx.runtime_id.clone(),
        agent_version: ctx.agent_version.clone().unwrap_or_default(),
        stream: stream_id.to_owned(),
        record_kind: record_kind.to_owned(),
        error_code: code.to_owned(),
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
    LineOutcome {
        ops: vec![drift(ctx, stream_id, record_kind, code, paths, at)],
        status: LineStatus::HardError {
            code: code.to_owned(),
        },
    }
}

fn parse_failure(ctx: &NormalizeCtx, stream_id: &str, text: &str) -> LineOutcome {
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        return hard(ctx, stream_id, INVALID_RECORD, INVALID_JSON, &[], None);
    };
    let kind = value["type"].as_str().unwrap_or(NO_TYPE);
    let declared = spec::declared_transcript_types().contains(kind);
    // `parse_line` only fails after valid JSON for a declared record whose
    // required shape moved. Keep the branch explicit for future parser changes.
    let code = if declared { PARSE_FAILED } else { UNKNOWN_TYPE };
    hard(
        ctx,
        stream_id,
        kind,
        code,
        &top_level_keys(&value),
        stamp(&value),
    )
}

fn record_identity(record: &KnownRecord) -> (&'static str, Option<Ms>) {
    match record {
        KnownRecord::UserInput(line) => ("USER_INPUT", rfc3339_ms(&line.created_at)),
        KnownRecord::PlannerResponse(line) => ("PLANNER_RESPONSE", rfc3339_ms(&line.created_at)),
        KnownRecord::RunCommand(line) => ("RUN_COMMAND", rfc3339_ms(&line.created_at)),
        KnownRecord::Checkpoint(line) => ("CHECKPOINT", rfc3339_ms(&line.created_at)),
        KnownRecord::SystemMessage(line) => ("SYSTEM_MESSAGE", rfc3339_ms(&line.created_at)),
        KnownRecord::ConversationHistory(line) => {
            ("CONVERSATION_HISTORY", rfc3339_ms(&line.created_at))
        }
        KnownRecord::ListDirectory(line) => ("LIST_DIRECTORY", rfc3339_ms(&line.created_at)),
        KnownRecord::ViewFile(line) => ("VIEW_FILE", rfc3339_ms(&line.created_at)),
        KnownRecord::ErrorMessage(line) => ("ERROR_MESSAGE", rfc3339_ms(&line.created_at)),
        KnownRecord::Generic(line) => ("GENERIC", rfc3339_ms(&line.created_at)),
    }
}

fn stamp(value: &Value) -> Option<Ms> {
    value["created_at"].as_str().and_then(rfc3339_ms)
}

fn top_level_keys(value: &Value) -> Vec<String> {
    match value.as_object() {
        Some(object) => object.keys().cloned().collect(),
        None => vec!["<non-object>".to_owned()],
    }
}

#[cfg(test)]
mod tests {
    use super::AntigravityState;
    use crate::AntigravityAdapter;
    use crate::spec;
    use rosetta_adapter_support::adapter::{AgentAdapter, LineStatus, NormalizeCtx};
    use rosetta_core::timeline::SummaryPurpose;
    use rosetta_core::{IngestOp, TimelineBody, ids};
    use serde_json::{Value, json};

    fn context() -> NormalizeCtx {
        let runtime = ids::runtime_id("antigravity", "~/.gemini/antigravity-cli");
        let session = ids::session_id(&runtime, "11111111-2222-3333-4444-555555555555");
        let mut ctx = NormalizeCtx::new(runtime, session);
        ctx.state = AntigravityAdapter.init_state();
        ctx
    }

    fn ingest(ctx: &mut NormalizeCtx, value: Value) -> rosetta_adapter_support::LineOutcome {
        AntigravityAdapter.ingest_line(ctx, spec::TRANSCRIPT, &serde_json::to_vec(&value).unwrap())
    }

    #[test]
    fn tool_call_and_result_are_one_row_plus_one_merge() {
        let mut ctx = context();
        let call = ingest(
            &mut ctx,
            json!({
                "step_index": 1,
                "source": "MODEL",
                "type": "PLANNER_RESPONSE",
                "status": "DONE",
                "created_at": "2026-08-25T05:10:44Z",
                "tool_calls": [{"name": "run_command", "args": {"commandLine": "echo ok"}}]
            }),
        );
        let tool_id = match call.ops.as_slice() {
            [IngestOp::Message(row)] => {
                let TimelineBody::ToolUse { tool } = &row.body else {
                    panic!("expected tool body, got {:?}", row.body);
                };
                tool.correlation.key.clone()
            }
            other => panic!("expected one tool row, got {other:?}"),
        };

        let result = ingest(
            &mut ctx,
            json!({
                "step_index": 2,
                "source": "MODEL",
                "type": "RUN_COMMAND",
                "status": "DONE",
                "exit_code": 0,
                "created_at": "2026-08-25T05:10:44Z",
                "content": "ok\n"
            }),
        );
        match result.ops.as_slice() {
            [
                IngestOp::ToolUpdate {
                    correlation_key: merged,
                    patch,
                    ..
                },
            ] => {
                assert_eq!(merged, &tool_id);
                assert_eq!(
                    patch.status.map(|status| status.value),
                    Some(rosetta_core::ToolStatus::Succeeded)
                );
            }
            other => panic!("expected one tool result, got {other:?}"),
        }
    }

    #[test]
    fn generic_model_record_projects_as_agent_prose() {
        let mut ctx = context();
        let outcome = ingest(
            &mut ctx,
            json!({
                "step_index": 9,
                "source": "MODEL",
                "type": "GENERIC",
                "status": "DONE",
                "created_at": "2026-08-27T01:00:00Z",
                "content": "finished",
                "truncated_fields": ["content"]
            }),
        );
        assert_eq!(outcome.status, LineStatus::Parsed);
        match outcome.ops.as_slice() {
            [IngestOp::Message(row)] => {
                let TimelineBody::Agent {
                    text: Some(text), ..
                } = &row.body
                else {
                    panic!("expected agent prose, got {:?}", row.body);
                };
                assert_eq!(text.as_preview(), "finished");
            }
            other => panic!("expected one agent message, got {other:?}"),
        }
    }

    #[test]
    fn checkpoint_keeps_its_summary_and_emits_a_bounded_title_fallback() {
        let mut ctx = context();
        let outcome = ingest(
            &mut ctx,
            json!({
                "step_index": 4,
                "source": "SYSTEM",
                "type": "CHECKPOINT",
                "status": "DONE",
                "created_at": "2026-08-27T01:00:00Z",
                "content": "{{ CHECKPOINT }}\n# USER Objective:\n  Map   the system\ncarefully\n# User Requests\nignore"
            }),
        );
        assert_eq!(outcome.status, LineStatus::Parsed);
        match outcome.ops.as_slice() {
            [IngestOp::Message(summary), IngestOp::SessionTitle(title)] => {
                let TimelineBody::Summary { purpose, text } = &summary.body else {
                    panic!("expected checkpoint summary, got {:?}", summary.body);
                };
                assert_eq!(*purpose, SummaryPurpose::Compaction);
                assert!(text.as_preview().contains("User Requests"));
                assert_eq!(title.title, "Map the system carefully");
            }
            other => panic!("expected checkpoint plus title fallback, got {other:?}"),
        }
    }

    #[test]
    fn checkpoint_round_trip_preserves_pending_tools() {
        let adapter = AntigravityAdapter;
        let mut ctx = context();
        let _ = ingest(
            &mut ctx,
            json!({
                "step_index": 1,
                "source": "MODEL",
                "type": "PLANNER_RESPONSE",
                "status": "DONE",
                "created_at": "2026-08-25T05:10:44Z",
                "tool_calls": [{"name": "view_file", "args": {"AbsolutePath": "/fixture/file"}}]
            }),
        );
        let checkpoint = adapter
            .checkpoint_state(ctx.state.as_deref())
            .unwrap()
            .expect("checkpoint");
        let restored = adapter
            .restore_state(Some(&checkpoint))
            .unwrap()
            .expect("state");
        assert_eq!(
            restored.downcast_ref::<AntigravityState>(),
            ctx.state
                .as_deref()
                .and_then(|state| state.downcast_ref::<AntigravityState>())
        );
    }

    #[test]
    fn malformed_checkpoint_fails_closed() {
        let adapter = AntigravityAdapter;
        for malformed in [
            json!({}),
            json!({"pending_tools": "not-an-array"}),
            json!({"pending_tools": [], "extra": true}),
        ] {
            assert!(adapter.restore_state(Some(&malformed)).is_err());
        }
    }

    #[test]
    fn orphan_result_is_a_hard_error_with_drift() {
        let mut ctx = context();
        let outcome = ingest(
            &mut ctx,
            json!({
                "step_index": 2,
                "source": "MODEL",
                "type": "VIEW_FILE",
                "status": "DONE",
                "created_at": "2026-08-25T05:10:44Z",
                "content": "contents"
            }),
        );
        assert_eq!(
            outcome.status,
            LineStatus::HardError {
                code: "orphan_tool_result".to_owned()
            }
        );
        assert!(matches!(outcome.ops.as_slice(), [IngestOp::Drift(_)]));
    }

    #[test]
    fn undeclared_type_is_soft_drift_and_declared_noop_is_parsed() {
        let mut ctx = context();
        let unknown = ingest(
            &mut ctx,
            json!({
                "step_index": 1,
                "source": "SYSTEM",
                "type": "FUTURE_RECORD",
                "status": "DONE",
                "created_at": "2026-08-25T05:10:44Z"
            }),
        );
        assert_eq!(unknown.status, LineStatus::UnknownType);
        assert!(matches!(unknown.ops.as_slice(), [IngestOp::Drift(_)]));

        let declared = ingest(
            &mut ctx,
            json!({
                "step_index": 2,
                "source": "SYSTEM",
                "type": "CONVERSATION_HISTORY",
                "status": "DONE",
                "created_at": "2026-08-25T05:10:44Z"
            }),
        );
        assert_eq!(declared.status, LineStatus::Parsed);
        assert!(declared.ops.is_empty());
    }

    #[test]
    fn known_malformed_record_is_a_hard_error_with_drift() {
        let mut ctx = context();
        let outcome = ingest(
            &mut ctx,
            json!({
                "step_index": 1,
                "source": "USER_EXPLICIT",
                "type": "USER_INPUT",
                "status": "DONE",
                "created_at": "2026-08-25T05:10:44Z"
            }),
        );
        assert_eq!(
            outcome.status,
            LineStatus::HardError {
                code: "parse_failed".to_owned()
            }
        );
        assert!(matches!(outcome.ops.as_slice(), [IngestOp::Drift(_)]));
    }

    #[test]
    fn inventory_sibling_cannot_emit_a_duplicate_message() {
        let mut ctx = context();
        let line = json!({
            "step_index": 0,
            "source": "USER_EXPLICIT",
            "type": "USER_INPUT",
            "status": "DONE",
            "created_at": "2026-08-25T05:10:43Z",
            "content": "hello"
        });
        let primary = ingest(&mut ctx, line.clone());
        assert!(matches!(primary.ops.as_slice(), [IngestOp::Message(_)]));

        for inventory in [
            spec::TRANSCRIPT_FULL,
            spec::TRANSCRIPT_CHUNKS,
            spec::TRANSCRIPT_FULL_CHUNKS,
        ] {
            let duplicate = AntigravityAdapter.ingest_line(
                &mut ctx,
                inventory,
                &serde_json::to_vec(&line).unwrap(),
            );
            assert!(duplicate.ops.is_empty(), "{inventory} duplicated a row");
        }
    }
}
