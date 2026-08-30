//! Versioned Claude Code tool request projection.
//!
//! The pinned writer is intentionally the upper bound.  A newer Claude
//! version does not silently inherit these mappings; release drift must first
//! refresh the canonical ground-truth pack.

use rosetta_adapter_support::tool::{
    NativeToolSelector, ProjectionError, ProjectorRegistration, SearchPolicy, StatusPolicy,
    VersionRange, canonical_text, first_bool, first_string, first_u64, lexical_resource,
    lookup_registration, open_interaction, string_values, url,
};
use rosetta_core::TaskStatus;
use rosetta_core::timeline::{
    ArtifactKind, ArtifactOp, CodeLookupOp, CollaborationOp, CorrelationDerivation, ExecutionMode,
    FileDelta, FileSearchMode, ModeOp, ProcessOp, QuestionOption, RangeUnit, ReadTarget,
    ScheduleOp, SearchFilter, SearchFilterKind, SourceRange, TaskOp, ToolAction, ToolActionKind,
    ToolInteraction, UserQuestion,
};
use serde_json::Value;

pub const AGENT: &str = "claude-code";
pub const PINNED_WRITER_VERSION: &str = "2.1.241";

const fn registration(name: &'static str, action_kind: ToolActionKind) -> ProjectorRegistration {
    ProjectorRegistration {
        agent: AGENT,
        writer_versions: VersionRange::exact(PINNED_WRITER_VERSION),
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
        ToolActionKind::FileChange => project_change(input, cwd)?,
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
            path: first_string(input, &["path"])
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
    Ok(open_interaction(
        correlation_key,
        CorrelationDerivation::NativeId,
        native_id.map(str::to_owned),
        namespace.map(str::to_owned),
        name,
        namespace.map(str::to_owned),
        action,
    ))
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

fn project_change(input: &Value, cwd: Option<&str>) -> Result<ToolAction, ProjectionError> {
    let path = first_string(input, &["file_path", "notebook_path", "path"])
        .ok_or(ProjectionError::MissingField("path"))?;
    Ok(ToolAction::FileChange {
        changes: vec![FileDelta {
            operation: None,
            path: lexical_resource(path, cwd, false)?,
            destination: None,
            diff: None,
            lines_added: None,
            lines_removed: None,
        }],
    })
}

fn project_shell(input: &Value, cwd: Option<&str>) -> Result<ToolAction, ProjectionError> {
    let command =
        first_string(input, &["command"]).ok_or(ProjectionError::MissingField("command"))?;
    Ok(ToolAction::Shell {
        command: canonical_text(command)?,
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
        objective: canonical_text(objective)?,
        child_session_id: None,
        resume_id: first_string(input, &["resume", "resume_id"]).map(str::to_owned),
        background: first_bool(input, &["run_in_background", "background"]).unwrap_or(false),
        orphan_result_summary: None,
    })
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
        task_id: first_string(input, &["task_id", "id"]).map(str::to_owned),
        subject: first_string(input, &["subject", "title"]).map(str::to_owned),
        description: first_string(input, &["description", "content"])
            .map(canonical_text)
            .transpose()?,
        status: first_string(input, &["status"]).and_then(task_status),
        owner: first_string(input, &["owner"]).map(str::to_owned),
        blocker_ids: string_values(input, &["addBlocks", "blocker_ids"], 20),
    })
}

fn task_status(value: &str) -> Option<TaskStatus> {
    match value.to_ascii_lowercase().replace(' ', "_").as_str() {
        "pending" => Some(TaskStatus::Pending),
        "in_progress" => Some(TaskStatus::InProgress),
        "completed" | "done" => Some(TaskStatus::Completed),
        "failed" => Some(TaskStatus::Failed),
        "cancelled" | "canceled" => Some(TaskStatus::Cancelled),
        _ => None,
    }
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
    let resources = first_string(input, &["path", "file_path"])
        .map(|path| lexical_resource(path, cwd, false))
        .transpose()?
        .into_iter()
        .collect();
    Ok(ToolAction::Artifact {
        operation: if name == "SendUserFile" {
            ArtifactOp::Send
        } else {
            ArtifactOp::Create
        },
        artifact_kind: ArtifactKind::File,
        name: first_string(input, &["name"]).map(str::to_owned),
        description: first_string(input, &["description", "prompt"])
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
        body: first_string(input, &["message", "body"])
            .map(canonical_text)
            .transpose()?,
        linked_session_id: None,
        delivery_id: None,
        outcome: None,
    })
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
    fn newer_writer_does_not_silently_inherit_the_mapping() {
        assert!(
            project_request(
                Some("2.1.251"),
                "Bash",
                &json!({"command":"echo unsafe"}),
                None,
                "x",
                Some("x"),
                None
            )
            .is_err()
        );
    }

    #[test]
    fn write_keeps_path_but_not_whole_file_content() {
        let tool = project_request(
            Some(PINNED_WRITER_VERSION),
            "Write",
            &json!({"file_path":"src/main.rs", "content":"very large body"}),
            Some("/repo"),
            "w1",
            Some("w1"),
            None,
        )
        .unwrap();
        match tool.action {
            ToolAction::FileChange { changes } => {
                assert_eq!(changes[0].path.display_value(), "/repo/src/main.rs");
                assert!(changes[0].diff.is_none());
            }
            other => panic!("expected file change, got {other:?}"),
        }
    }
}
