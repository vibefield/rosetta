//! Claude Code Workflow sidecars -> the closed canonical run document.
//!
//! A run snapshot is mutable current state, while its journal is append-only
//! lifecycle evidence. Neither is a human-authored message. Both are attached
//! to the parent session named by their path and meet the transcript again only
//! through the exact `runId` carried by an accepted Workflow result.

use std::collections::BTreeSet;

use rosetta_adapter_support::tool::{canonical_text, lexical_resource, url};
use rosetta_adapter_support::{FileStat, LineOutcome, LineStatus, NormalizeCtx, SkimResult};
use rosetta_core::rows::{DriftFact, SessionRole};
use rosetta_core::timeline::{
    CANONICAL_LIST_LIMIT, EXTERNAL_LABEL_MAX_BYTES, EXTERNAL_VALUE_MAX_BYTES, ResourceRef,
    WORKFLOW_LOG_LIMIT, WORKFLOW_PHASE_LIMIT, WORKFLOW_WORKER_LIMIT, WorkflowJournalKind,
    WorkflowJournalObservation, WorkflowPhase, WorkflowResultKind, WorkflowRun, WorkflowRunStatus,
    WorkflowUsage, WorkflowWorker, WorkflowWorkerState,
};
use rosetta_core::{IngestOp, ids};
use serde::Deserialize;
use serde_json::Value;

use crate::time::rfc3339_ms;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawRun {
    run_id: String,
    #[serde(default)]
    task_id: Option<String>,
    #[serde(default)]
    workflow_name: Option<String>,
    status: String,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    start_time: Option<i64>,
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default)]
    duration_ms: Option<u64>,
    #[serde(default)]
    agent_count: Option<u64>,
    #[serde(default)]
    total_tokens: Option<u64>,
    #[serde(default)]
    total_tool_calls: Option<u64>,
    #[serde(default)]
    default_model: Option<String>,
    #[serde(default)]
    args: Value,
    #[serde(default)]
    script_path: Option<String>,
    #[serde(default)]
    script: Option<String>,
    #[serde(default)]
    phases: Vec<RawPhase>,
    #[serde(default)]
    workflow_progress: Vec<RawProgress>,
    #[serde(default)]
    logs: Vec<String>,
    #[serde(default)]
    result: Value,
    #[serde(default)]
    error: Value,
}

#[derive(Debug, Deserialize)]
struct RawPhase {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    detail: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
enum RawProgress {
    WorkflowPhase {
        #[serde(default)]
        index: Option<u64>,
        #[serde(default)]
        title: Option<String>,
    },
    WorkflowAgent(Box<RawWorkflowAgent>),
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawWorkflowAgent {
    #[serde(default)]
    index: Option<u64>,
    #[serde(default)]
    key: Option<String>,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    phase_index: Option<u64>,
    #[serde(default)]
    phase_title: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    cached: Option<bool>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    effort: Option<String>,
    #[serde(default)]
    agent_id: Option<String>,
    #[serde(default)]
    prompt_preview: Option<String>,
    #[serde(default)]
    result_preview: Option<String>,
    #[serde(default)]
    last_tool_name: Option<String>,
    #[serde(default)]
    last_tool_summary: Option<String>,
    #[serde(default)]
    queued_at: Option<i64>,
    #[serde(default)]
    started_at: Option<i64>,
    #[serde(default)]
    last_progress_at: Option<i64>,
    #[serde(default)]
    duration_ms: Option<u64>,
    #[serde(default)]
    tokens: Option<u64>,
    #[serde(default)]
    tool_calls: Option<u64>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    attempt: Option<u64>,
    #[serde(default)]
    agent_type: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawJournal {
    #[serde(rename = "type")]
    kind: String,
    key: String,
    #[serde(rename = "agentId", default)]
    agent_id: Option<String>,
    #[serde(default)]
    result: Value,
}

pub(crate) fn parent_session_from_path(relative_path: &str) -> Option<&str> {
    let mut parts = relative_path.split('/');
    (parts.next()? == "projects").then_some(())?;
    let _project = parts.next()?;
    let session = parts.next()?;
    (!session.is_empty()).then_some(session)
}

pub(crate) fn run_id_from_path(relative_path: &str) -> Option<&str> {
    let path = relative_path
        .split_once('#')
        .map_or(relative_path, |(path, _)| path);
    let parts: Vec<&str> = path.split('/').collect();
    let workflows = parts.iter().position(|part| *part == "workflows")?;
    let candidate = *parts.get(workflows + 1)?;
    if candidate == "scripts" {
        return None;
    }
    let run_id = candidate.strip_suffix(".json").unwrap_or(candidate);
    run_id.starts_with("wf_").then_some(run_id)
}

pub(crate) fn skim_run(stat: &FileStat, document: &[u8]) -> SkimResult {
    let parent = parent_session_from_path(&stat.relative_path).map(str::to_owned);
    let raw = serde_json::from_slice::<RawRun>(document).ok();
    SkimResult {
        session_id: parent,
        role: Some(SessionRole::Main),
        started_at: raw.as_ref().and_then(|run| run.start_time),
        updated_at: raw
            .as_ref()
            .and_then(|run| run.timestamp.as_deref())
            .and_then(rfc3339_ms)
            .or(Some(stat.modified_ms)),
        ..SkimResult::default()
    }
}

pub(crate) fn skim_journal(stat: &FileStat) -> SkimResult {
    SkimResult {
        session_id: parent_session_from_path(&stat.relative_path).map(str::to_owned),
        role: Some(SessionRole::Main),
        updated_at: Some(stat.modified_ms),
        ..SkimResult::default()
    }
}

pub(crate) fn ingest_run(ctx: &mut NormalizeCtx, line: &[u8]) -> LineOutcome {
    let raw = match serde_json::from_slice::<RawRun>(line) {
        Ok(raw) => raw,
        Err(error) => return malformed(ctx, "workflow-run", error.to_string()),
    };
    if let Some(expected) = ctx.source_identity.as_deref().and_then(run_id_from_path)
        && raw.run_id != expected
    {
        return malformed(
            ctx,
            "workflow-run",
            format!(
                "payload runId {:?} does not match source {expected:?}",
                raw.run_id
            ),
        );
    }
    let run = match normalize_run(ctx, raw) {
        Ok(run) => run,
        Err(error) => return malformed(ctx, "workflow-run", error),
    };
    let mut ops = Vec::new();
    IngestOp::push_workflow_run(&mut ops, ctx.session_id.clone(), run);
    LineOutcome::parsed(ops)
}

pub(crate) fn ingest_journal(ctx: &mut NormalizeCtx, line: &[u8]) -> LineOutcome {
    let raw = match serde_json::from_slice::<RawJournal>(line) {
        Ok(raw) => raw,
        Err(error) => return malformed(ctx, "workflow-journal", error.to_string()),
    };
    let source = ctx.source_identity.as_deref().unwrap_or_default();
    let Some(run_id) = run_id_from_path(source).map(str::to_owned) else {
        return malformed(
            ctx,
            "workflow-journal",
            "source path has no workflow run id".into(),
        );
    };
    let kind = match raw.kind.as_str() {
        "started" => WorkflowJournalKind::Started,
        "result" => WorkflowJournalKind::Result,
        _ => {
            return malformed(
                ctx,
                "workflow-journal",
                "unknown journal record type".into(),
            );
        }
    };
    let result_kind = (raw.kind == "result").then(|| result_kind(&raw.result));
    let result_preview = (raw.kind == "result" && !raw.result.is_null())
        .then(|| json_preview(&raw.result, EXTERNAL_VALUE_MAX_BYTES));
    let discriminator = format!(
        "workflow-journal:{run_id}:{}",
        ctx.src_offset.unwrap_or_default()
    );
    let observation = WorkflowJournalObservation {
        observation_id: ids::timeline_id(&ctx.session_id, &discriminator),
        run_id,
        source_offset: ctx.src_offset.and_then(|offset| u64::try_from(offset).ok()),
        kind,
        worker_key: truncate(raw.key, EXTERNAL_LABEL_MAX_BYTES),
        agent_id: raw
            .agent_id
            .filter(|value| !value.is_empty())
            .map(|value| truncate(value, EXTERNAL_LABEL_MAX_BYTES)),
        result_kind,
        result_preview,
    };
    LineOutcome::parsed(vec![IngestOp::WorkflowJournal {
        session_id: ctx.session_id.clone(),
        observation,
    }])
}

fn normalize_run(ctx: &NormalizeCtx, raw: RawRun) -> Result<WorkflowRun, String> {
    if raw.run_id.is_empty() {
        return Err("runId is empty".into());
    }
    let mut phases = raw
        .phases
        .into_iter()
        .take(WORKFLOW_PHASE_LIMIT)
        .enumerate()
        .map(|(at, phase)| WorkflowPhase {
            index: u64::try_from(at + 1).unwrap_or(u64::MAX),
            title: truncate(
                phase.title.unwrap_or_else(|| format!("Phase {}", at + 1)),
                EXTERNAL_LABEL_MAX_BYTES,
            ),
            detail: phase
                .detail
                .map(|value| truncate(value, EXTERNAL_VALUE_MAX_BYTES)),
        })
        .collect::<Vec<_>>();

    if phases.is_empty() {
        phases.extend(
            raw.workflow_progress
                .iter()
                .filter_map(|progress| match progress {
                    RawProgress::WorkflowPhase { index, title } => Some(WorkflowPhase {
                        index: index.unwrap_or(0),
                        title: truncate(
                            title.clone().unwrap_or_else(|| "Phase".into()),
                            EXTERNAL_LABEL_MAX_BYTES,
                        ),
                        detail: None,
                    }),
                    RawProgress::WorkflowAgent(_) => None,
                })
                .take(WORKFLOW_PHASE_LIMIT),
        );
    }

    let workers = raw
        .workflow_progress
        .into_iter()
        .filter_map(|progress| match progress {
            RawProgress::WorkflowPhase { .. } => None,
            RawProgress::WorkflowAgent(agent) => {
                let RawWorkflowAgent {
                    index,
                    key,
                    label,
                    phase_index,
                    phase_title,
                    state,
                    cached,
                    model,
                    effort,
                    agent_id,
                    prompt_preview,
                    result_preview,
                    last_tool_name,
                    last_tool_summary,
                    queued_at,
                    started_at,
                    last_progress_at,
                    duration_ms,
                    tokens,
                    tool_calls,
                    error,
                    attempt,
                    agent_type,
                } = *agent;
                let index = index.unwrap_or(0);
                let fallback = key
                    .as_deref()
                    .or(agent_id.as_deref())
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("worker {index}"));
                let _native_only = (attempt, agent_type);
                Some(WorkflowWorker {
                    index,
                    key: key.map(|value| truncate(value, EXTERNAL_LABEL_MAX_BYTES)),
                    label: truncate(label.unwrap_or(fallback), EXTERNAL_LABEL_MAX_BYTES),
                    phase_index,
                    phase_title: phase_title.map(|value| truncate(value, EXTERNAL_LABEL_MAX_BYTES)),
                    state: worker_state(state.as_deref()),
                    cached: cached.unwrap_or(false),
                    configured_model: model.map(|value| truncate(value, EXTERNAL_LABEL_MAX_BYTES)),
                    configured_effort: effort
                        .map(|value| truncate(value, EXTERNAL_LABEL_MAX_BYTES)),
                    observed_models: Vec::new(),
                    observed_efforts: Vec::new(),
                    child_session_id: agent_id
                        .as_deref()
                        .filter(|value| !value.is_empty())
                        .map(|value| ids::session_id(&ctx.runtime_id, value)),
                    prompt_preview: prompt_preview
                        .map(|value| truncate(value, EXTERNAL_VALUE_MAX_BYTES)),
                    result_preview: result_preview
                        .map(|value| truncate(value, EXTERNAL_VALUE_MAX_BYTES)),
                    last_tool_name: last_tool_name
                        .map(|value| truncate(value, EXTERNAL_VALUE_MAX_BYTES)),
                    last_tool_summary: last_tool_summary
                        .map(|value| truncate(value, EXTERNAL_VALUE_MAX_BYTES)),
                    queued_at,
                    started_at,
                    last_progress_at,
                    duration_ms,
                    tokens,
                    tool_calls,
                    error: error.map(|value| truncate(value, EXTERNAL_VALUE_MAX_BYTES)),
                })
            }
        })
        .take(WORKFLOW_WORKER_LIMIT)
        .collect::<Vec<_>>();

    let result_kind = Some(result_kind(&raw.result));
    let result = (!raw.result.is_null())
        .then(|| canonical_text(json_text(&raw.result)))
        .transpose()
        .map_err(|error| error.to_string())?;
    let error = (!raw.error.is_null())
        .then(|| canonical_text(json_text(&raw.error)))
        .transpose()
        .map_err(|error| error.to_string())?;
    let arguments = (!raw.args.is_null())
        .then(|| canonical_text(json_text(&raw.args)))
        .transpose()
        .map_err(|error| error.to_string())?;
    let summary = raw
        .summary
        .map(canonical_text)
        .transpose()
        .map_err(|error| error.to_string())?;
    let script = raw
        .script
        .map(canonical_text)
        .transpose()
        .map_err(|error| error.to_string())?;
    let script_path = raw
        .script_path
        .as_deref()
        .map(|path| lexical_resource(path, None, false))
        .transpose()
        .map_err(|error| error.to_string())?;
    let resources = resources(&raw.result);
    let logs = raw
        .logs
        .into_iter()
        .take(WORKFLOW_LOG_LIMIT)
        .map(|log| truncate(log, EXTERNAL_VALUE_MAX_BYTES))
        .collect();
    let updated_at = raw.timestamp.as_deref().and_then(rfc3339_ms);
    Ok(WorkflowRun {
        run_id: raw.run_id,
        task_id: raw
            .task_id
            .map(|value| truncate(value, EXTERNAL_LABEL_MAX_BYTES)),
        name: raw
            .workflow_name
            .map(|value| truncate(value, EXTERNAL_LABEL_MAX_BYTES)),
        summary,
        status: run_status(&raw.status),
        started_at: raw.start_time,
        updated_at,
        usage: WorkflowUsage {
            duration_ms: raw.duration_ms,
            tokens: raw.total_tokens,
            tool_calls: raw.total_tool_calls,
            agent_count: raw
                .agent_count
                .or_else(|| u64::try_from(workers.len()).ok()),
        },
        default_model: raw
            .default_model
            .map(|value| truncate(value, EXTERNAL_LABEL_MAX_BYTES)),
        arguments,
        script_path,
        script,
        phases,
        workers,
        logs,
        result_kind,
        result,
        error,
        resources,
        attempts: Vec::new(),
        attempt_total_count: 0,
        journal: Vec::new(),
        journal_total_count: 0,
        journal_started_count: 0,
        journal_result_count: 0,
    })
}

fn run_status(value: &str) -> WorkflowRunStatus {
    match value {
        "running" | "pending" => WorkflowRunStatus::Running,
        "completed" => WorkflowRunStatus::Completed,
        "failed" => WorkflowRunStatus::Failed,
        "killed" => WorkflowRunStatus::Killed,
        "cancelled" | "canceled" => WorkflowRunStatus::Cancelled,
        "stopped" => WorkflowRunStatus::Stopped,
        _ => WorkflowRunStatus::Unknown,
    }
}

fn worker_state(value: Option<&str>) -> WorkflowWorkerState {
    match value {
        Some("done") => WorkflowWorkerState::Done,
        Some("error") => WorkflowWorkerState::Error,
        Some("progress") => WorkflowWorkerState::Progress,
        Some("start") => WorkflowWorkerState::Start,
        _ => WorkflowWorkerState::Unknown,
    }
}

fn result_kind(value: &Value) -> WorkflowResultKind {
    match value {
        Value::Object(_) => WorkflowResultKind::Object,
        Value::Array(_) => WorkflowResultKind::Array,
        Value::String(_) => WorkflowResultKind::String,
        Value::Number(_) => WorkflowResultKind::Number,
        Value::Bool(_) => WorkflowResultKind::Boolean,
        Value::Null => WorkflowResultKind::Null,
    }
}

fn json_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        _ => serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string()),
    }
}

fn json_preview(value: &Value, limit: usize) -> String {
    truncate(json_text(value), limit)
}

fn truncate(value: String, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value;
    }
    let reserve = max_bytes.saturating_sub(3);
    let mut end = reserve.min(value.len());
    while !value.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    format!("{}...", &value[..end])
}

fn resources(value: &Value) -> Vec<ResourceRef> {
    fn visit(value: &Value, seen: &mut BTreeSet<String>, out: &mut Vec<ResourceRef>) {
        if out.len() >= CANONICAL_LIST_LIMIT {
            return;
        }
        match value {
            Value::String(text) => {
                let candidate = if text.starts_with("https://") || text.starts_with("http://") {
                    url(text).ok()
                } else if text.starts_with('/') {
                    lexical_resource(text, None, false).ok()
                } else {
                    None
                };
                if let Some(resource) = candidate {
                    let key = serde_json::to_string(&resource).unwrap_or_default();
                    if seen.insert(key) {
                        out.push(resource);
                    }
                }
            }
            Value::Array(items) => {
                for item in items {
                    visit(item, seen, out);
                }
            }
            Value::Object(fields) => {
                for item in fields.values() {
                    visit(item, seen, out);
                }
            }
            Value::Null | Value::Bool(_) | Value::Number(_) => {}
        }
    }

    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    visit(value, &mut seen, &mut out);
    out
}

fn malformed(ctx: &NormalizeCtx, stream: &str, diagnostic: String) -> LineOutcome {
    let fact = DriftFact {
        runtime_id: ctx.runtime_id.clone(),
        agent_version: ctx.agent_version.clone().unwrap_or_default(),
        stream: stream.to_owned(),
        record_kind: stream.to_owned(),
        error_code: "parse_failed".into(),
        shape_hash: ids::shape_hash(&[diagnostic]),
        at: None,
        sample_cursor: ctx.src_cursor,
        sample_offset: ctx.src_offset,
    };
    LineOutcome {
        ops: vec![IngestOp::Drift(fact)],
        status: LineStatus::HardError {
            code: "parse_failed".into(),
        },
    }
}

#[cfg(test)]
mod tests {
    use rosetta_adapter_support::{LineStatus, NormalizeCtx};
    use rosetta_core::{IngestOp, WorkflowJournalKind, WorkflowRunStatus, WorkflowWorkerState};

    use super::{ingest_journal, ingest_run, parent_session_from_path, run_id_from_path};

    #[test]
    fn workflow_paths_keep_parent_and_run_identity_separate() {
        let run = "projects/project/session/workflows/wf_abc.json";
        let journal = "projects/project/session/subagents/workflows/wf_abc/journal.jsonl";
        assert_eq!(parent_session_from_path(run), Some("session"));
        assert_eq!(parent_session_from_path(journal), Some("session"));
        assert_eq!(run_id_from_path(run), Some("wf_abc"));
        assert_eq!(run_id_from_path(journal), Some("wf_abc"));
    }

    #[test]
    fn closes_run_snapshot_and_append_journal_into_typed_ops() {
        let mut ctx = NormalizeCtx::new("runtime", "session");
        ctx.source_identity = Some("projects/project/session/workflows/wf_main.json".into());
        let outcome = ingest_run(
            &mut ctx,
            br#"{
              "runId":"wf_main","taskId":"task-main","workflowName":"Inspect",
              "status":"completed","summary":"inspection complete",
              "startTime":1786406400000,"timestamp":"2026-08-11T00:00:01.005Z",
              "durationMs":1005,"agentCount":1,"totalTokens":123,"totalToolCalls":4,
              "defaultModel":"claude-opus","scriptPath":"/repo/workflow.js",
              "phases":[{"title":"Inspect","detail":"read the surface"}],
              "workflowProgress":[{"type":"workflow_agent","index":1,"label":"reader",
                "phaseIndex":1,"phaseTitle":"Inspect","agentId":"agent-child",
                "model":"claude-opus","state":"done","tokens":123,"toolCalls":4,
                "resultPreview":"found it"}],
              "logs":["1/1"],"result":{"doc":"/repo/report.md"},"error":null
            }"#,
        );
        assert_eq!(outcome.status, LineStatus::Parsed);
        let run = outcome
            .ops
            .iter()
            .find_map(|op| match op {
                IngestOp::WorkflowRun { run, .. } => Some(run),
                _ => None,
            })
            .unwrap();
        assert_eq!(run.status, WorkflowRunStatus::Completed);
        assert_eq!(run.workers[0].state, WorkflowWorkerState::Done);
        assert_eq!(
            run.workers[0].configured_model.as_deref(),
            Some("claude-opus")
        );
        assert!(run.workers[0].child_session_id.is_some());
        assert_eq!(run.resources[0].display_value(), "/repo/report.md");

        ctx.source_identity =
            Some("projects/project/session/subagents/workflows/wf_main/journal.jsonl".into());
        ctx.src_offset = Some(41);
        let outcome = ingest_journal(
            &mut ctx,
            br#"{"type":"result","key":"worker-key","agentId":"agent-child","result":{"ok":true}}"#,
        );
        let observation = outcome
            .ops
            .iter()
            .find_map(|op| match op {
                IngestOp::WorkflowJournal { observation, .. } => Some(observation),
                _ => None,
            })
            .unwrap();
        assert_eq!(observation.kind, WorkflowJournalKind::Result);
        assert_eq!(observation.run_id, "wf_main");
        assert_eq!(observation.source_offset, Some(41));
    }

    #[test]
    fn rejects_a_run_document_whose_payload_id_disagrees_with_its_path() {
        let mut ctx = NormalizeCtx::new("runtime", "session");
        ctx.source_identity = Some("projects/project/session/workflows/wf_right.json".into());
        let outcome = ingest_run(&mut ctx, br#"{"runId":"wf_wrong","status":"running"}"#);
        assert!(matches!(outcome.status, LineStatus::HardError { .. }));
        assert!(matches!(outcome.ops.as_slice(), [IngestOp::Drift(_)]));
    }
}
