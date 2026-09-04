//! Versioned Claude Code tool request projection.
//!
//! Coverage is declared per release LINE, not per release. `groundtruth/claude-code`
//! pins fixtures at 2.1.241, and the census committed beside them
//! (`census/2026-08-24-local-structure.json`) spans 2.1.37..=2.1.241 — 40 patch
//! releases whose aggregated `tool_use_names` are one stable vocabulary, the one
//! registered below. Claude Code ships patches near-daily, so pinning a single
//! patch release erases the typed vocabulary for every transcript written by any
//! other one; the range covers the whole 2.1 line instead.
//!
//! Failing closed is preserved where it carries evidence. A 2.2 (or later) writer
//! is a new vocabulary claim and still projects `unknown` until the pack is
//! re-farmed; an unregistered name still projects `unknown`; and a changed input
//! shape inside the line still fails closed, because `project_request` returns
//! `MissingField`/`InvalidField` and the caller records a drift fact.

use rosetta_adapter_support::tool::{
    NativeToolSelector, ProjectionError, ProjectorRegistration, SearchPolicy, StatusPolicy,
    VersionRange, canonical_text, first_bool, first_string, first_u64, lexical_resource,
    lookup_registration, open_interaction, string_values, url,
};
use rosetta_core::timeline::{
    ArtifactKind, ArtifactOp, CANONICAL_LIST_LIMIT, CORE_SCALAR_MAX_BYTES, CodeLookupOp,
    CollaborationOp, CollaborationOutcome, CollaboratorRef, CorrelationDerivation, ExecutionMode,
    FileChangeOp, FileDelta, FileSearchMode, ID_MAX_BYTES, ModeOp, ParticipantColor,
    ParticipantState, ProcessOp, QuestionOption, RangeUnit, ReadTarget, ScheduleOp, SearchFilter,
    SearchFilterKind, SourceRange, TaskOp, TaskSnapshotItem, ToolAction, ToolActionKind,
    ToolInteraction, UserQuestion,
};
use rosetta_core::{TaskStatus, ids};
use serde_json::Value;

pub const AGENT: &str = "claude-code";
/// Writer the committed fixtures were farmed from (`watch.toml` `pinned`).
pub const PINNED_WRITER_VERSION: &str = "2.1.241";
/// Oldest writer in the committed census — the floor of the proven vocabulary.
pub const EARLIEST_WRITER_VERSION: &str = "2.1.37";
/// Ceiling of the 2.1 release line. `2.2.0` and later fail closed to `unknown`.
pub const WRITER_LINE_CEILING: &str = "2.1.99999";

const fn registration(name: &'static str, action_kind: ToolActionKind) -> ProjectorRegistration {
    ProjectorRegistration {
        agent: AGENT,
        writer_versions: VersionRange {
            min_inclusive: EARLIEST_WRITER_VERSION,
            max_inclusive: WRITER_LINE_CEILING,
        },
        accepts_unversioned: false,
        native: NativeToolSelector {
            namespace: None,
            name,
        },
        action_kind,
        status_policy: StatusPolicy::TerminalRecord,
        search_policy: SearchPolicy::Canonical,
        bounds_version: 1,
    }
}

pub const REGISTRATIONS: &[ProjectorRegistration] = &[
    registration("Read", ToolActionKind::FileRead),
    registration("Glob", ToolActionKind::FileSearch),
    registration("Grep", ToolActionKind::FileSearch),
    registration("Write", ToolActionKind::FileChange),
    registration("Edit", ToolActionKind::FileChange),
    registration("NotebookEdit", ToolActionKind::FileChange),
    registration("Bash", ToolActionKind::Shell),
    registration("bash", ToolActionKind::Shell),
    registration("PowerShell", ToolActionKind::Shell),
    registration("Monitor", ToolActionKind::Shell),
    registration("KillShell", ToolActionKind::Process),
    registration("Task", ToolActionKind::Delegate),
    registration("Agent", ToolActionKind::Delegate),
    registration("TodoWrite", ToolActionKind::Task),
    registration("TaskCreate", ToolActionKind::Task),
    registration("TaskUpdate", ToolActionKind::Task),
    registration("TaskList", ToolActionKind::Task),
    registration("TaskOutput", ToolActionKind::Task),
    registration("TaskStop", ToolActionKind::Task),
    registration("TaskGet", ToolActionKind::Task),
    registration("WebSearch", ToolActionKind::WebSearch),
    registration("WebFetch", ToolActionKind::WebFetch),
    registration("AskUserQuestion", ToolActionKind::Question),
    registration("Skill", ToolActionKind::Skill),
    registration("Workflow", ToolActionKind::Workflow),
    registration("CronCreate", ToolActionKind::Schedule),
    registration("CronDelete", ToolActionKind::Schedule),
    registration("CronList", ToolActionKind::Schedule),
    registration("ScheduleWakeup", ToolActionKind::Schedule),
    registration("Artifact", ToolActionKind::Artifact),
    registration("SendUserFile", ToolActionKind::Artifact),
    registration("ToolSearch", ToolActionKind::ToolDiscovery),
    registration("EnterPlanMode", ToolActionKind::Mode),
    registration("ExitPlanMode", ToolActionKind::Mode),
    registration("EnterWorktree", ToolActionKind::Mode),
    registration("ExitWorktree", ToolActionKind::Mode),
    registration("SendMessage", ToolActionKind::Collaborate),
    registration("ListAgents", ToolActionKind::Collaborate),
    registration("TeamCreate", ToolActionKind::Collaborate),
    registration("TeamDelete", ToolActionKind::Collaborate),
    registration("ReportFindings", ToolActionKind::Finding),
    registration("PushNotification", ToolActionKind::Notification),
    registration("SendFeedback", ToolActionKind::Notification),
    registration("LSP", ToolActionKind::CodeLookup),
];

pub fn project_request(
    writer_version: Option<&str>,
    name: &str,
    input: &Value,
    cwd: Option<&str>,
    correlation_key: &str,
    native_id: Option<&str>,
    namespace: Option<&str>,
) -> Result<ToolInteraction, ProjectionError> {
    let registration = lookup_registration(REGISTRATIONS, AGENT, writer_version, namespace, name)?;
    let action = match registration.action_kind {
        ToolActionKind::FileRead => project_read(input, cwd)?,
        ToolActionKind::FileSearch => project_search(name, input, cwd)?,
        ToolActionKind::FileChange => project_change(name, input, cwd)?,
        ToolActionKind::Shell => project_shell(input, cwd)?,
        ToolActionKind::Process => {
            let process_id = first_string(input, &["shell_id", "process_id", "task_id"])
                .ok_or(ProjectionError::MissingField("process id"))?;
            ToolAction::Process {
                operation: ProcessOp::Stop,
                process_id: process_id.to_owned(),
                input: None,
                outcome: None,
            }
        }
        ToolActionKind::Delegate => project_delegate(input)?,
        ToolActionKind::Task => project_task(name, input)?,
        ToolActionKind::WebSearch => ToolAction::WebSearch {
            query: first_string(input, &["query"]).map(str::to_owned),
            allowed_domains: string_values(input, &["allowed_domains"], 20),
            blocked_domains: string_values(input, &["blocked_domains"], 20),
            outcome: None,
        },
        ToolActionKind::WebFetch => {
            let target =
                first_string(input, &["url"]).ok_or(ProjectionError::MissingField("url"))?;
            ToolAction::WebFetch {
                url: url(target)?,
                purpose: first_string(input, &["prompt", "purpose"]).map(str::to_owned),
                outcome: None,
            }
        }
        ToolActionKind::Question => ToolAction::Question {
            questions: project_questions(input)?,
            answers: vec![],
        },
        ToolActionKind::Skill => {
            let skill = first_string(input, &["skill", "name"])
                .ok_or(ProjectionError::MissingField("skill name"))?;
            ToolAction::Skill {
                name: skill.to_owned(),
                arguments: first_string(input, &["args", "arguments"])
                    .map(canonical_text)
                    .transpose()?,
            }
        }
        ToolActionKind::Workflow => ToolAction::Workflow {
            name: first_string(input, &["name", "workflow"]).map(str::to_owned),
            path: first_string(input, &["path", "scriptPath", "script_path"])
                .map(|path| lexical_resource(path, cwd, false))
                .transpose()?,
            arguments: first_string(input, &["args", "arguments"])
                .map(canonical_text)
                .transpose()?,
            objective: first_string(input, &["description", "objective", "prompt"])
                .map(canonical_text)
                .transpose()?,
            resume_id: first_string(input, &["resume", "resume_id"]).map(str::to_owned),
            background: first_bool(input, &["background", "run_in_background"]).unwrap_or(false),
            task_id: None,
            run_id: None,
            attempt: None,
            callback_summary: None,
            callback_result: None,
            usage: None,
            run: None,
        },
        ToolActionKind::Schedule => project_schedule(name, input)?,
        ToolActionKind::Artifact => project_artifact(name, input, cwd)?,
        ToolActionKind::ToolDiscovery => {
            let query =
                first_string(input, &["query"]).ok_or(ProjectionError::MissingField("query"))?;
            ToolAction::ToolDiscovery {
                query: query.to_owned(),
                matches: vec![],
                total_count: 0,
            }
        }
        ToolActionKind::Mode => project_mode(name, input, cwd)?,
        ToolActionKind::Collaborate => project_collaboration(name, input)?,
        ToolActionKind::Finding => ToolAction::Finding {
            findings: vec![],
            total_count: input
                .get("findings")
                .and_then(Value::as_array)
                .map_or(0, |items| items.len() as u64),
        },
        ToolActionKind::Notification => {
            let summary = first_string(input, &["message", "feedback", "summary"])
                .ok_or(ProjectionError::MissingField("notification summary"))?;
            ToolAction::Notification {
                channel: first_string(input, &["channel"]).map(str::to_owned),
                destination: first_string(input, &["destination", "recipient"]).map(str::to_owned),
                summary: canonical_text(summary)?,
                delivery_id: None,
            }
        }
        ToolActionKind::CodeLookup => {
            let query = first_string(input, &["symbol", "query", "word"])
                .ok_or(ProjectionError::MissingField("query"))?;
            ToolAction::CodeLookup {
                operation: CodeLookupOp::Definition,
                query: query.to_owned(),
                path: first_string(input, &["file_path", "path"])
                    .map(|path| lexical_resource(path, cwd, false))
                    .transpose()?,
                outcome: None,
            }
        }
        _ => unreachable!("Claude registry and projector are closed together"),
    };
    open_interaction(
        correlation_key,
        CorrelationDerivation::NativeId,
        native_id.map(str::to_owned),
        namespace.map(str::to_owned),
        name,
        namespace.map(str::to_owned),
        action,
    )
}

fn project_read(input: &Value, cwd: Option<&str>) -> Result<ToolAction, ProjectionError> {
    let path =
        first_string(input, &["file_path", "path"]).ok_or(ProjectionError::MissingField("path"))?;
    let start = first_u64(input, &["offset", "line"]);
    let count = first_u64(input, &["limit", "count"]);
    Ok(ToolAction::FileRead {
        targets: vec![ReadTarget {
            resource: lexical_resource(path, cwd, false)?,
            range: start.map(|start| SourceRange {
                unit: RangeUnit::Line,
                start,
                count,
            }),
            media_kind: None,
        }],
        outcome: None,
    })
}

fn project_search(
    name: &str,
    input: &Value,
    cwd: Option<&str>,
) -> Result<ToolAction, ProjectionError> {
    let query = first_string(input, &["pattern", "query"])
        .ok_or(ProjectionError::MissingField("query"))?
        .to_owned();
    let roots = first_string(input, &["path", "directory"])
        .map(|path| lexical_resource(path, cwd, true))
        .transpose()?
        .into_iter()
        .collect();
    let filters = first_string(input, &["glob"])
        .map(|value| SearchFilter {
            kind: SearchFilterKind::Glob,
            value: value.to_owned(),
        })
        .into_iter()
        .collect();
    Ok(ToolAction::FileSearch {
        query,
        mode: if name == "Glob" {
            FileSearchMode::Glob
        } else {
            FileSearchMode::Content
        },
        roots,
        filters,
        outcome: None,
    })
}

fn project_change(
    name: &str,
    input: &Value,
    cwd: Option<&str>,
) -> Result<ToolAction, ProjectionError> {
    let source_path = first_string(input, &["file_path", "notebook_path", "path"])
        .ok_or(ProjectionError::MissingField("path"))?;
    let (operation, diff, lines_added, lines_removed) = match name {
        "Edit" => {
            let old = first_string(input, &["old_string"]);
            let new = first_string(input, &["new_string"]);
            let diff = old.zip(new).and_then(|(old, new)| {
                (old != new)
                    .then(|| replacement_diff(source_path, source_path, Some(old), new, None))
            });
            let (added, removed) = replacement_counts(old, new);
            (Some(FileChangeOp::Update), diff, added, removed)
        }
        "Write" => {
            let content = first_string(input, &["content"]);
            (
                None,
                content
                    .map(|content| replacement_diff(source_path, source_path, None, content, None)),
                content.map(line_count),
                None,
            )
        }
        "NotebookEdit" => {
            let source = first_string(input, &["new_source"]);
            (
                Some(FileChangeOp::Update),
                source.map(|source| replacement_diff(source_path, source_path, None, source, None)),
                source.map(line_count),
                None,
            )
        }
        _ => (None, None, None, None),
    };
    Ok(ToolAction::FileChange {
        changes: vec![FileDelta {
            operation,
            path: lexical_resource(source_path, cwd, false)?,
            destination: None,
            diff: diff.map(canonical_text).transpose()?,
            lines_added,
            lines_removed,
        }],
    })
}

/// Claude's successful Edit/Write result carries the applied patch, including
/// context and real hunk coordinates. It is better evidence than the request's
/// old/new fragment, so the result record can enrich the already-open tool.
/// The raw field intentionally remains `Value`; narrowing is confined to this
/// optional projection so a new result shape can never make a line unparseable.
pub fn project_change_result(
    result: &Value,
    cwd: Option<&str>,
) -> Result<Option<ToolAction>, ProjectionError> {
    let Some(source_path) = first_string(result, &["filePath", "file_path"]) else {
        return Ok(None);
    };
    let operation = match first_string(result, &["type"]) {
        Some("create") => Some(FileChangeOp::Create),
        Some("update") => Some(FileChangeOp::Update),
        Some("delete") => Some(FileChangeOp::Delete),
        _ if result
            .get("structuredPatch")
            .and_then(Value::as_array)
            .is_some_and(|hunks| !hunks.is_empty()) =>
        {
            Some(FileChangeOp::Update)
        }
        _ => None,
    };

    let projected = structured_patch_diff(source_path, result, operation).or_else(|| {
        let old = first_string(result, &["originalFile", "oldString"]);
        let new = first_string(result, &["content", "newString"])?;
        let old_path = if operation == Some(FileChangeOp::Create) {
            "/dev/null"
        } else {
            source_path
        };
        let (added, removed) = replacement_counts(old, Some(new));
        Some((
            replacement_diff(
                old_path,
                source_path,
                old,
                new,
                positioned_whole_file(old, new),
            ),
            added.unwrap_or(0),
            removed,
        ))
    });
    let Some((diff, lines_added, lines_removed)) = projected else {
        return Ok(None);
    };

    Ok(Some(ToolAction::FileChange {
        changes: vec![FileDelta {
            operation,
            path: lexical_resource(source_path, cwd, false)?,
            destination: None,
            diff: Some(canonical_text(diff)?),
            lines_added: Some(lines_added),
            lines_removed,
        }],
    }))
}

fn structured_patch_diff(
    path: &str,
    result: &Value,
    operation: Option<FileChangeOp>,
) -> Option<(String, u64, Option<u64>)> {
    let hunks = result.get("structuredPatch")?.as_array()?;
    if hunks.is_empty() {
        return None;
    }

    let old_path = if operation == Some(FileChangeOp::Create) {
        "/dev/null"
    } else {
        path
    };
    let mut diff = vec![format!("--- {old_path}"), format!("+++ {path}")];
    let mut added = 0_u64;
    let mut removed = 0_u64;
    let mut projected_hunks = 0_u64;
    for hunk in hunks {
        let lines = hunk.get("lines").and_then(Value::as_array)?;
        let old_start = hunk.get("oldStart").and_then(Value::as_u64);
        let old_lines = hunk.get("oldLines").and_then(Value::as_u64);
        let new_start = hunk.get("newStart").and_then(Value::as_u64);
        let new_lines = hunk.get("newLines").and_then(Value::as_u64);
        match (old_start, old_lines, new_start, new_lines) {
            (Some(old_start), Some(old_lines), Some(new_start), Some(new_lines)) => diff.push(
                format!("@@ -{old_start},{old_lines} +{new_start},{new_lines} @@"),
            ),
            _ => diff.push("@@".to_owned()),
        }
        for line in lines {
            let line = line.as_str()?;
            if line.starts_with('+') {
                added += 1;
            } else if line.starts_with('-') {
                removed += 1;
            }
            diff.push(line.to_owned());
        }
        projected_hunks += 1;
    }
    (projected_hunks > 0).then_some((diff.join("\n"), added, Some(removed)))
}

fn replacement_counts(old: Option<&str>, new: Option<&str>) -> (Option<u64>, Option<u64>) {
    (new.map(line_count), old.map(line_count))
}

fn positioned_whole_file(old: Option<&str>, new: &str) -> Option<(u64, u64, u64, u64)> {
    Some((1, old.map_or(0, line_count), 1, line_count(new)))
}

fn replacement_diff(
    old_path: &str,
    new_path: &str,
    old: Option<&str>,
    new: &str,
    position: Option<(u64, u64, u64, u64)>,
) -> String {
    let mut diff = vec![format!("--- {old_path}"), format!("+++ {new_path}")];
    diff.push(match position {
        Some((old_start, old_lines, new_start, new_lines)) => {
            format!("@@ -{old_start},{old_lines} +{new_start},{new_lines} @@")
        }
        None => "@@".to_owned(),
    });
    if let Some(old) = old {
        diff.extend(old.lines().map(|line| format!("-{line}")));
    }
    diff.extend(new.lines().map(|line| format!("+{line}")));
    diff.join("\n")
}

fn line_count(text: &str) -> u64 {
    if text.is_empty() {
        0
    } else {
        text.lines().count() as u64
    }
}

fn project_shell(input: &Value, cwd: Option<&str>) -> Result<ToolAction, ProjectionError> {
    let command =
        first_string(input, &["command"]).ok_or(ProjectionError::MissingField("command"))?;
    Ok(ToolAction::Shell {
        command: canonical_text(command)?,
        source_truncated: false,
        cwd: first_string(input, &["cwd"])
            .or(cwd)
            .map(|path| lexical_resource(path, cwd, true))
            .transpose()?,
        timeout_ms: first_u64(input, &["timeout", "timeout_ms"]),
        background: first_bool(input, &["run_in_background", "background"]).unwrap_or(false),
        outcome: None,
    })
}

fn project_delegate(input: &Value) -> Result<ToolAction, ProjectionError> {
    let objective = first_string(input, &["description", "objective", "prompt"])
        .ok_or(ProjectionError::MissingField("objective"))?;
    Ok(ToolAction::Delegate {
        agent_kind: first_string(input, &["subagent_type", "agent", "agent_kind"])
            .map(str::to_owned),
        name: first_string(input, &["name"]).map(str::to_owned),
        model: first_string(input, &["model"]).map(str::to_owned),
        mode: first_string(input, &["mode"]).map(str::to_owned),
        team: None,
        color: None,
        objective: canonical_text(objective)?,
        child_session_id: None,
        resume_id: first_string(input, &["resume", "resume_id"]).map(str::to_owned),
        background: first_bool(input, &["run_in_background", "background"]).unwrap_or(false),
        orphan_result_summary: None,
    })
}

/// Outcome-only identity for an Agent invocation.  `teammate_spawned` is not
/// itself the relation proof; the request's launch/resume intent remains on
/// the opener, while a team name on this structured result proves membership.
pub fn project_delegate_result(
    result: &Value,
    child_session_id: Option<String>,
) -> Result<ToolAction, ProjectionError> {
    let objective = first_string(result, &["prompt", "description"])
        .ok_or(ProjectionError::MissingField("delegate result objective"))?;
    Ok(ToolAction::Delegate {
        agent_kind: first_string(result, &["agentType", "agent_type"]).map(str::to_owned),
        name: first_string(result, &["name"]).map(str::to_owned),
        model: first_string(result, &["model"]).map(str::to_owned),
        mode: None,
        team: first_string(result, &["team_name", "teamName"]).map(str::to_owned),
        color: first_string(result, &["color"]).and_then(participant_color),
        objective: canonical_text(objective)?,
        child_session_id,
        resume_id: None,
        background: first_bool(result, &["isAsync", "is_async"]).unwrap_or(false),
        orphan_result_summary: None,
    })
}

fn participant_color(value: &str) -> Option<ParticipantColor> {
    crate::envelope::participant_color(value)
}

fn project_task(name: &str, input: &Value) -> Result<ToolAction, ProjectionError> {
    let operation = match name {
        "TaskCreate" => TaskOp::Create,
        "TaskList" => TaskOp::List,
        "TaskGet" => TaskOp::Get,
        "TaskStop" => TaskOp::Stop,
        "TaskOutput" => TaskOp::Output,
        _ => TaskOp::Update,
    };
    Ok(ToolAction::Task {
        operation,
        task_id: first_string(input, &["taskId", "task_id", "id"]).map(str::to_owned),
        subject: first_string(input, &["subject", "title"]).map(str::to_owned),
        description: first_string(input, &["description", "content"])
            .map(canonical_text)
            .transpose()?,
        status: if operation == TaskOp::Create {
            first_string(input, &["status"])
                .and_then(task_status)
                .or(Some(TaskStatus::Pending))
        } else {
            first_string(input, &["status"]).and_then(task_status)
        },
        owner: first_string(input, &["owner"]).map(str::to_owned),
        blocker_ids: string_values(
            input,
            &["addBlockedBy", "blockedBy", "addBlocks", "blocker_ids"],
            20,
        ),
        snapshot: None,
    })
}

pub(crate) fn task_status(value: &str) -> Option<TaskStatus> {
    match value.to_ascii_lowercase().replace(' ', "_").as_str() {
        "pending" => Some(TaskStatus::Pending),
        "in_progress" => Some(TaskStatus::InProgress),
        "completed" | "done" => Some(TaskStatus::Completed),
        "failed" => Some(TaskStatus::Failed),
        "cancelled" | "canceled" => Some(TaskStatus::Cancelled),
        _ => None,
    }
}

/// Task result structures are captured in the Claude ground-truth document:
/// TaskCreate returns `task`, while TaskList returns `tasks`. Keep this parser
/// beside the request projector so native field knowledge does not leak into
/// the client.
pub(crate) fn project_task_item(value: &Value) -> Option<TaskSnapshotItem> {
    let task_id = first_string(value, &["id", "taskId", "task_id"])?;
    Some(TaskSnapshotItem {
        task_id: task_id.to_owned(),
        subject: first_string(value, &["subject", "title"]).map(str::to_owned),
        status: first_string(value, &["status"]).and_then(task_status),
        owner: first_string(value, &["owner"]).map(str::to_owned),
        blocker_ids: string_values(value, &["blockedBy", "blocker_ids"], 20),
    })
}

pub(crate) fn project_task_list_result(value: &Value) -> Option<(Vec<TaskSnapshotItem>, u64)> {
    let values = value.get("tasks")?.as_array()?;
    let total_count = u64::try_from(values.len()).unwrap_or(u64::MAX);
    Some((
        values.iter().filter_map(project_task_item).collect(),
        total_count,
    ))
}

fn project_questions(input: &Value) -> Result<Vec<UserQuestion>, ProjectionError> {
    input
        .get("questions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
        .map(|(index, question)| {
            let prompt = first_string(question, &["question", "prompt"])
                .ok_or(ProjectionError::MissingField("question"))?;
            let options = question
                .get("options")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .enumerate()
                .filter_map(|(option_index, option)| {
                    let label = first_string(option, &["label"])?;
                    Some(QuestionOption {
                        id: first_string(option, &["id"])
                            .map(str::to_owned)
                            .unwrap_or_else(|| format!("q{index}:o{option_index}")),
                        label: label.to_owned(),
                        description: first_string(option, &["description"]).map(str::to_owned),
                    })
                })
                .take(20)
                .collect();
            Ok(UserQuestion {
                id: first_string(question, &["id"])
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("q{index}")),
                prompt: canonical_text(prompt)?,
                header: first_string(question, &["header"]).map(str::to_owned),
                options,
                allow_multiple: first_bool(question, &["multiSelect", "allow_multiple"])
                    .unwrap_or(false),
            })
        })
        .collect()
}

fn project_schedule(name: &str, input: &Value) -> Result<ToolAction, ProjectionError> {
    let operation = match name {
        "CronDelete" => ScheduleOp::Delete,
        "CronList" => ScheduleOp::List,
        "ScheduleWakeup" => ScheduleOp::Wake,
        _ => ScheduleOp::Create,
    };
    Ok(ToolAction::Schedule {
        operation,
        schedule_id: first_string(input, &["id", "schedule_id"]).map(str::to_owned),
        due_at: input.get("due_at").and_then(Value::as_i64),
        delay_ms: first_u64(input, &["delay_ms", "delay"]),
        reason: first_string(input, &["reason"]).map(str::to_owned),
        objective: first_string(input, &["objective", "prompt"])
            .map(canonical_text)
            .transpose()?,
    })
}

fn project_artifact(
    name: &str,
    input: &Value,
    cwd: Option<&str>,
) -> Result<ToolAction, ProjectionError> {
    // `SendUserFile` is plural in the native record even when it sends one
    // file. Preserve every entry; falling back to the singular fields keeps
    // the separate `Artifact` tool's observed shape intact.
    let mut paths = string_values(input, &["files"], CANONICAL_LIST_LIMIT);
    if paths.is_empty()
        && let Some(path) = first_string(input, &["path", "file_path"])
    {
        paths.push(path.to_owned());
    }
    let resources = paths
        .into_iter()
        .map(|path| lexical_resource(path, cwd, false))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ToolAction::Artifact {
        operation: if name == "SendUserFile" {
            ArtifactOp::Send
        } else {
            ArtifactOp::Create
        },
        artifact_kind: ArtifactKind::File,
        name: first_string(input, &["name", "title"]).map(str::to_owned),
        description: first_string(input, &["description", "prompt", "caption"])
            .map(canonical_text)
            .transpose()?,
        resources,
        media: None,
    })
}

fn project_mode(
    name: &str,
    input: &Value,
    cwd: Option<&str>,
) -> Result<ToolAction, ProjectionError> {
    let entering = name.starts_with("Enter");
    let mode = if name.contains("Worktree") {
        ExecutionMode::Worktree
    } else {
        ExecutionMode::Plan
    };
    Ok(ToolAction::Mode {
        operation: if entering {
            ModeOp::Enter
        } else {
            ModeOp::Exit
        },
        mode,
        workspace: first_string(input, &["path", "name"])
            .map(|path| lexical_resource(path, cwd, true))
            .transpose()?,
    })
}

fn project_collaboration(name: &str, input: &Value) -> Result<ToolAction, ProjectionError> {
    let operation = match name {
        "SendMessage"
            if first_string(input, &["type"]).is_some_and(|kind| kind == "shutdown_request") =>
        {
            CollaborationOp::RequestStop
        }
        "SendMessage"
            if first_string(input, &["type"]).is_some_and(|kind| kind == "shutdown_response") =>
        {
            CollaborationOp::RespondStop
        }
        "TeamCreate" => CollaborationOp::Create,
        "TeamDelete" => CollaborationOp::Delete,
        "ListAgents" => CollaborationOp::List,
        _ => CollaborationOp::Send,
    };
    Ok(ToolAction::Collaborate {
        operation,
        recipient: first_string(input, &["recipient"]).map(str::to_owned),
        team: first_string(input, &["team_name", "team"]).map(str::to_owned),
        summary: first_string(input, &["summary"]).map(str::to_owned),
        body: first_string(input, &["message", "body", "content"])
            .map(canonical_text)
            .transpose()?,
        linked_session_id: None,
        delivery_id: first_string(input, &["msg_id", "request_id", "requestId"]).map(str::to_owned),
        outcome: None,
    })
}

/// Fold Claude's structured collaboration receipt onto its exact opener.
/// Request prose remains authoritative; receipts contribute only routing,
/// acknowledgement ids, resume links, and participant snapshots.
pub(crate) fn project_collaboration_result(
    request: &ToolAction,
    result: &Value,
    runtime_id: &str,
) -> Option<ToolAction> {
    let ToolAction::Collaborate {
        operation,
        recipient,
        team,
        summary,
        body,
        linked_session_id,
        delivery_id,
        outcome,
    } = request
    else {
        return None;
    };
    let routing = result.get("routing").filter(|value| value.is_object());
    let receipt_string = |keys: &[&str]| {
        keys.iter()
            .find_map(|key| result.get(key).and_then(Value::as_str))
            .or_else(|| {
                routing.and_then(|route| {
                    keys.iter()
                        .find_map(|key| route.get(key).and_then(Value::as_str))
                })
            })
            .map(str::trim)
            .filter(|value| !value.is_empty())
    };
    let receipt_recipient = receipt_string(&["target", "recipient"])
        .map(|value| bounded_field(value.trim_start_matches('@'), ID_MAX_BYTES));
    let receipt_team = receipt_string(&["team_name", "teamName", "team"])
        .map(|value| bounded_field(value, ID_MAX_BYTES));
    let receipt_summary =
        receipt_string(&["summary"]).map(|value| bounded_field(value, CORE_SCALAR_MAX_BYTES));
    let receipt_delivery = receipt_string(&["msg_id", "request_id", "requestId"])
        .map(|value| bounded_field(value, ID_MAX_BYTES));
    let resumed = receipt_string(&["resumedAgentId", "resumed_agent_id"])
        .map(|native| ids::session_id(runtime_id, native));

    let participant_values = result
        .get("agents")
        .or_else(|| result.get("participants"))
        .and_then(Value::as_array);
    let receipt_outcome = participant_values
        .map(|values| {
            let participants = values
                .iter()
                .filter_map(|value| {
                    let native_id =
                        first_string(value, &["agentId", "agent_id", "sessionId", "session_id"])
                            .map(str::trim)
                            .filter(|value| !value.is_empty());
                    let name = first_string(value, &["name", "agentName", "agent_name"])
                        .or_else(|| native_id.and_then(|id| id.split('@').next()))?;
                    let name = bounded_field(name, ID_MAX_BYTES);
                    if name.is_empty() {
                        return None;
                    }
                    let state = participant_state(
                        first_string(value, &["status", "state"]).unwrap_or_default(),
                    );
                    Some(CollaboratorRef {
                        name,
                        role: first_string(value, &["role", "agentType", "agent_type"])
                            .map(|value| bounded_field(value, ID_MAX_BYTES))
                            .filter(|value| !value.is_empty()),
                        session_id: native_id.map(|id| ids::session_id(runtime_id, id)),
                        state,
                    })
                })
                .take(CANONICAL_LIST_LIMIT)
                .collect::<Vec<_>>();
            CollaborationOutcome {
                participants,
                total_count: u64::try_from(values.len()).unwrap_or(u64::MAX),
            }
        })
        .or_else(|| {
            result
                .get("listing")
                .and_then(Value::as_str)
                .map(project_participant_listing)
        });

    Some(ToolAction::Collaborate {
        operation: *operation,
        recipient: receipt_recipient.or_else(|| recipient.clone()),
        team: receipt_team.or_else(|| team.clone()),
        summary: summary.clone().or(receipt_summary),
        body: body.clone(),
        linked_session_id: resumed.or_else(|| linked_session_id.clone()),
        delivery_id: receipt_delivery.or_else(|| delivery_id.clone()),
        outcome: receipt_outcome.or_else(|| outcome.clone()),
    })
}

fn participant_state(value: &str) -> ParticipantState {
    match value.trim().to_ascii_lowercase().as_str() {
        "active" | "running" | "working" | "busy" => ParticipantState::Active,
        "idle" | "available" => ParticipantState::Idle,
        "completed" | "done" => ParticipantState::Completed,
        "failed" | "error" => ParticipantState::Failed,
        _ => ParticipantState::Unknown,
    }
}

fn bounded_field(value: &str, max_bytes: usize) -> String {
    let value = value.trim();
    let mut end = value.len().min(max_bytes);
    while !value.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    value[..end].to_owned()
}

/// Claude's current `ListAgents` receipt is a human-readable listing rather
/// than JSON rows. Admit only its stable section/count and middle-dot columns;
/// bracketed ids are deliberately not promoted to session ids because some are
/// shortened display ids.
fn project_participant_listing(listing: &str) -> CollaborationOutcome {
    let mut declared_total = 0_u64;
    let mut participants = Vec::new();
    let mut in_participant_section = false;
    for line in listing.lines() {
        let trimmed = line.trim();
        let section = ["Subagents (", "Teammates (", "Peer sessions ("]
            .into_iter()
            .find_map(|prefix| {
                trimmed
                    .strip_prefix(prefix)
                    .and_then(|rest| rest.strip_suffix("):"))
                    .and_then(|count| count.parse::<u64>().ok())
            });
        if let Some(count) = section {
            declared_total = declared_total.saturating_add(count);
            in_participant_section = true;
            continue;
        }
        if trimmed.is_empty() {
            in_participant_section = false;
            continue;
        }
        if !in_participant_section || participants.len() >= CANONICAL_LIST_LIMIT {
            continue;
        }
        let columns = trimmed.split("  ·  ").collect::<Vec<_>>();
        if columns.len() < 3 {
            continue;
        }
        let identity = columns[0].trim();
        let name = identity
            .rsplit_once(" [")
            .and_then(|(name, id)| id.ends_with(']').then_some(name))
            .unwrap_or(identity)
            .trim();
        if name.is_empty() {
            continue;
        }
        participants.push(CollaboratorRef {
            name: bounded_field(name, ID_MAX_BYTES),
            role: columns
                .get(1)
                .map(|value| value.trim())
                .filter(|value| !value.is_empty())
                .map(|value| bounded_field(value, ID_MAX_BYTES)),
            session_id: None,
            state: participant_state(columns[2]),
        });
    }
    CollaborationOutcome {
        total_count: declared_total.max(u64::try_from(participants.len()).unwrap_or(u64::MAX)),
        participants,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rosetta_adapter_support::tool::validate_registry;
    use serde_json::json;

    #[test]
    fn registry_is_closed_and_fixture_bash_is_reduced() {
        validate_registry(REGISTRATIONS).unwrap();
        let tool = project_request(
            Some(PINNED_WRITER_VERSION),
            "Bash",
            &json!({"command":"echo rosetta-mock"}),
            Some("/project"),
            "msg_mock_0001_tool",
            Some("msg_mock_0001_tool"),
            None,
        )
        .unwrap();
        assert!(matches!(tool.action, ToolAction::Shell { .. }));
    }

    #[test]
    fn send_message_prefers_full_message_and_distinguishes_shutdown_operations() {
        let message = project_request(
            Some(PINNED_WRITER_VERSION),
            "SendMessage",
            &json!({
                "type":"message",
                "recipient":"researcher",
                "summary":"found the implementation",
                "content":"The implementation is in src/message.rs. Previ",
                "message":"The implementation is in src/message.rs. Full native body wins."
            }),
            None,
            "send-1",
            Some("send-1"),
            None,
        )
        .unwrap();
        let ToolAction::Collaborate {
            operation,
            recipient,
            summary,
            body,
            ..
        } = message.action
        else {
            panic!("expected collaboration")
        };
        assert_eq!(operation, CollaborationOp::Send);
        assert_eq!(recipient.as_deref(), Some("researcher"));
        assert_eq!(summary.as_deref(), Some("found the implementation"));
        assert_eq!(
            body.as_ref().map(|text| text.as_preview()),
            Some("The implementation is in src/message.rs. Full native body wins.")
        );

        let shutdown = project_request(
            Some(PINNED_WRITER_VERSION),
            "SendMessage",
            &json!({
                "type":"shutdown_request",
                "recipient":"researcher",
                "content":"The assigned work is complete."
            }),
            None,
            "send-2",
            Some("send-2"),
            None,
        )
        .unwrap();
        assert!(matches!(
            shutdown.action,
            ToolAction::Collaborate {
                operation: CollaborationOp::RequestStop,
                ..
            }
        ));

        let response = project_request(
            Some(PINNED_WRITER_VERSION),
            "SendMessage",
            &json!({
                "type":"shutdown_response",
                "recipient":"researcher",
                "request_id":"stop-7",
                "approve":true
            }),
            None,
            "send-3",
            Some("send-3"),
            None,
        )
        .unwrap();
        assert!(matches!(
            response.action,
            ToolAction::Collaborate {
                operation: CollaborationOp::RespondStop,
                delivery_id: Some(ref delivery_id),
                ..
            } if delivery_id == "stop-7"
        ));
    }

    #[test]
    fn collaboration_receipts_add_routing_delivery_resume_and_roster_evidence() {
        let send = project_request(
            Some(PINNED_WRITER_VERSION),
            "SendMessage",
            &json!({
                "recipient":"worker",
                "summary":"author summary",
                "message":"full body"
            }),
            None,
            "send-1",
            Some("send-1"),
            None,
        )
        .unwrap();
        let routed = project_collaboration_result(
            &send.action,
            &json!({
                "msg_id":"message-7",
                "resumedAgentId":"native-worker-7",
                "routing": {
                    "target":"@worker",
                    "targetColor":"green",
                    "summary":"delivered"
                }
            }),
            "runtime-1",
        )
        .unwrap();
        let ToolAction::Collaborate {
            recipient,
            summary,
            body,
            linked_session_id,
            delivery_id,
            ..
        } = routed
        else {
            panic!("expected collaboration receipt")
        };
        assert_eq!(recipient.as_deref(), Some("worker"));
        assert_eq!(summary.as_deref(), Some("author summary"));
        assert_eq!(
            body.as_ref().map(|body| body.as_preview()),
            Some("full body")
        );
        assert_eq!(
            linked_session_id,
            Some(ids::session_id("runtime-1", "native-worker-7"))
        );
        assert_eq!(delivery_id.as_deref(), Some("message-7"));

        let list = project_request(
            Some(PINNED_WRITER_VERSION),
            "ListAgents",
            &json!({}),
            None,
            "list-1",
            Some("list-1"),
            None,
        )
        .unwrap();
        let listed = project_collaboration_result(
            &list.action,
            &json!({"agents":[
                {"agentId":"w1@team", "name":"w1", "status":"working", "agentType":"Explore"},
                {"agentId":"w2@team", "name":"w2", "status":"idle"}
            ]}),
            "runtime-1",
        )
        .unwrap();
        let ToolAction::Collaborate { outcome, .. } = listed else {
            unreachable!()
        };
        let outcome = outcome.unwrap();
        assert_eq!(outcome.total_count, 2);
        assert_eq!(outcome.participants[0].state, ParticipantState::Active);
        assert_eq!(outcome.participants[1].state, ParticipantState::Idle);

        let native_listing = project_collaboration_result(
            &list.action,
            &json!({"listing": concat!(
                "This session is lead [123abc] — it is not listed below.\n\n",
                "Subagents (1):\n",
                "  afa78f5b2b6e9ed84  ·  general-purpose  ·  running  ·  started 9m ago\n\n",
                "Teammates (2):\n",
                "  rust-tools [8c73b8]  ·  general-purpose  ·  working  ·  started 1h ago\n",
                "  docs [224603]  ·  Explore  ·  idle  ·  started 1h ago\n"
            )}),
            "runtime-1",
        )
        .unwrap();
        let ToolAction::Collaborate { outcome, .. } = native_listing else {
            unreachable!()
        };
        let outcome = outcome.unwrap();
        assert_eq!(outcome.total_count, 3);
        assert_eq!(outcome.participants.len(), 3);
        assert_eq!(outcome.participants[0].name, "afa78f5b2b6e9ed84");
        assert_eq!(outcome.participants[0].state, ParticipantState::Active);
        assert_eq!(outcome.participants[1].name, "rust-tools");
        assert_eq!(outcome.participants[1].session_id, None);
        assert_eq!(outcome.participants[2].state, ParticipantState::Idle);
    }

    /// Replaces `newer_writer_does_not_silently_inherit_the_mapping`, which
    /// pinned the vocabulary to the single patch release the fixtures were
    /// farmed from. Claude Code ships patches near-daily, so that pin made a
    /// live projection lose every typed tool action the day 2.1.246 shipped.
    /// The census committed with the pack spans 2.1.37..=2.1.241 with one
    /// vocabulary, so the release line — not the patch — is the evidence
    /// boundary, and the next line still fails closed.
    #[test]
    fn the_release_line_projects_and_the_next_line_fails_closed() {
        for version in [
            EARLIEST_WRITER_VERSION,
            PINNED_WRITER_VERSION,
            "2.1.246",
            "2.1.258",
            WRITER_LINE_CEILING,
        ] {
            let tool = project_request(
                Some(version),
                "Bash",
                &json!({"command":"echo in-line"}),
                None,
                "x",
                Some("x"),
                None,
            )
            .unwrap_or_else(|error| panic!("{version} must project: {error:?}"));
            assert!(matches!(tool.action, ToolAction::Shell { .. }), "{version}");
        }
        for version in ["2.0.99", "2.2.0", "3.0.0"] {
            assert!(
                matches!(
                    project_request(
                        Some(version),
                        "Bash",
                        &json!({"command":"echo out of line"}),
                        None,
                        "x",
                        Some("x"),
                        None,
                    ),
                    Err(ProjectionError::Unregistered { .. })
                ),
                "{version} must fail closed",
            );
        }
    }

    /// Widening the version range must not widen the name vocabulary:
    /// `StructuredOutput` is in the committed census and has no projector.
    #[test]
    fn an_unregistered_name_inside_the_line_still_fails_closed() {
        assert!(matches!(
            project_request(
                Some("2.1.258"),
                "StructuredOutput",
                &json!({"document":"{}"}),
                None,
                "x",
                Some("x"),
                None,
            ),
            Err(ProjectionError::Unregistered { .. })
        ));
    }

    #[test]
    fn edit_and_write_requests_keep_renderable_diffs() {
        let edit = project_request(
            Some(PINNED_WRITER_VERSION),
            "Edit",
            &json!({
                "file_path":"src/main.rs",
                "old_string":"let before = 1;",
                "new_string":"let after = 2;"
            }),
            Some("/repo"),
            "e1",
            Some("e1"),
            None,
        )
        .unwrap();
        let ToolAction::FileChange { changes } = edit.action else {
            panic!("expected file change")
        };
        assert_eq!(changes[0].operation, Some(FileChangeOp::Update));
        assert_eq!(changes[0].lines_added, Some(1));
        assert_eq!(changes[0].lines_removed, Some(1));
        assert_eq!(
            changes[0].diff.as_ref().map(|diff| diff.as_preview()),
            Some("--- src/main.rs\n+++ src/main.rs\n@@\n-let before = 1;\n+let after = 2;")
        );

        let tool = project_request(
            Some(PINNED_WRITER_VERSION),
            "Write",
            &json!({"file_path":"src/main.rs", "content":"first\nsecond"}),
            Some("/repo"),
            "w1",
            Some("w1"),
            None,
        )
        .unwrap();
        match tool.action {
            ToolAction::FileChange { changes } => {
                assert_eq!(changes[0].path.display_value(), "/repo/src/main.rs");
                assert_eq!(changes[0].operation, None);
                assert_eq!(changes[0].lines_added, Some(2));
                assert_eq!(changes[0].lines_removed, None);
                assert_eq!(
                    changes[0].diff.as_ref().map(|diff| diff.as_preview()),
                    Some("--- src/main.rs\n+++ src/main.rs\n@@\n+first\n+second")
                );
            }
            other => panic!("expected file change, got {other:?}"),
        }
    }

    #[test]
    fn successful_result_upgrades_the_fragment_to_claudes_contextual_patch() {
        let action = project_change_result(
            &json!({
                "filePath":"/repo/src/main.rs",
                "oldString":"before();",
                "newString":"after();",
                "originalFile":null,
                "structuredPatch":[{
                    "oldStart":40,
                    "oldLines":3,
                    "newStart":40,
                    "newLines":3,
                    "lines":[" context();", "-before();", "+after();"]
                }]
            }),
            Some("/repo"),
        )
        .unwrap()
        .expect("file result");
        let ToolAction::FileChange { changes } = action else {
            panic!("expected file change")
        };
        assert_eq!(changes[0].operation, Some(FileChangeOp::Update));
        assert_eq!(changes[0].lines_added, Some(1));
        assert_eq!(changes[0].lines_removed, Some(1));
        assert_eq!(
            changes[0].diff.as_ref().map(|diff| diff.as_preview()),
            Some(
                "--- /repo/src/main.rs\n+++ /repo/src/main.rs\n@@ -40,3 +40,3 @@\n context();\n-before();\n+after();"
            )
        );
    }

    #[test]
    fn create_result_marks_dev_null_and_preserves_the_written_lines() {
        let action = project_change_result(
            &json!({
                "type":"create",
                "filePath":"/repo/new.txt",
                "content":"one\ntwo",
                "originalFile":null,
                "structuredPatch":[]
            }),
            Some("/repo"),
        )
        .unwrap()
        .expect("file result");
        let ToolAction::FileChange { changes } = action else {
            panic!("expected file change")
        };
        assert_eq!(changes[0].operation, Some(FileChangeOp::Create));
        assert_eq!(changes[0].lines_added, Some(2));
        assert_eq!(changes[0].lines_removed, None);
        assert_eq!(
            changes[0].diff.as_ref().map(|diff| diff.as_preview()),
            Some("--- /dev/null\n+++ /repo/new.txt\n@@ -1,0 +1,2 @@\n+one\n+two")
        );
    }

    #[test]
    fn send_user_file_preserves_every_file_and_its_caption() {
        let tool = project_request(
            Some(PINNED_WRITER_VERSION),
            "SendUserFile",
            &json!({
                "files": ["shots/first.png", "/tmp/second.png"],
                "caption": "Two views of the finished work"
            }),
            Some("/repo"),
            "send-1",
            Some("send-1"),
            None,
        )
        .unwrap();

        let ToolAction::Artifact {
            operation,
            description,
            resources,
            ..
        } = tool.action
        else {
            panic!("expected artifact")
        };
        assert_eq!(operation, ArtifactOp::Send);
        assert_eq!(
            description.as_ref().map(|text| text.as_preview()),
            Some("Two views of the finished work")
        );
        assert_eq!(resources.len(), 2);
        assert_eq!(resources[0].display_value(), "/repo/shots/first.png");
        assert_eq!(resources[1].display_value(), "/tmp/second.png");
    }
}
