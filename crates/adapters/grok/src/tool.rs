//! Versioned Grok Build tool request projection.

use rosetta_adapter_support::tool::{
    NativeToolSelector, ProjectionError, ProjectorRegistration, SearchPolicy, StatusPolicy,
    VersionRange, canonical_text, first_bool, first_string, first_u64, lexical_resource,
    lookup_registration, open_interaction, url,
};
use rosetta_core::TaskStatus;
use rosetta_core::timeline::{
    ArtifactKind, ArtifactOp, CorrelationDerivation, ExecutionMode, FileDelta, FileSearchMode,
    ModeOp, ProcessOp, ReadTarget, ScheduleOp, TaskOp, ToolAction, ToolActionKind, ToolInteraction,
};
use serde_json::Value;

pub const AGENT: &str = "grok";
pub const PINNED_WRITER_VERSION: &str = "1.0.5";

const fn registration(name: &'static str, action_kind: ToolActionKind) -> ProjectorRegistration {
    ProjectorRegistration {
        agent: AGENT,
        writer_versions: VersionRange::exact(PINNED_WRITER_VERSION),
        accepts_unversioned: true,
        native: NativeToolSelector {
            namespace: None,
            name,
        },
        action_kind,
        status_policy: StatusPolicy::ExplicitLifecycle,
        search_policy: SearchPolicy::Canonical,
        bounds_version: 1,
    }
}

pub const REGISTRATIONS: &[ProjectorRegistration] = &[
    registration("read_file", ToolActionKind::FileRead),
    registration("list_dir", ToolActionKind::FileList),
    registration("grep", ToolActionKind::FileSearch),
    registration("search_replace", ToolActionKind::FileChange),
    registration("write", ToolActionKind::FileChange),
    registration("run_terminal_command", ToolActionKind::Shell),
    registration("monitor", ToolActionKind::Process),
    registration("get_command_or_subagent_output", ToolActionKind::Process),
    registration("spawn_subagent", ToolActionKind::Delegate),
    registration("kill_command_or_subagent", ToolActionKind::Process),
    registration("todo_write", ToolActionKind::Task),
    registration("update_goal", ToolActionKind::Task),
    registration("scheduler_create", ToolActionKind::Schedule),
    registration("scheduler_cancel", ToolActionKind::Schedule),
    registration("scheduler_list", ToolActionKind::Schedule),
    registration("scheduler_wake", ToolActionKind::Schedule),
    registration("web_search", ToolActionKind::WebSearch),
    registration("x_search", ToolActionKind::WebSearch),
    registration("web_fetch", ToolActionKind::WebFetch),
    registration("ask_user_question", ToolActionKind::Question),
    registration("search_tool", ToolActionKind::ToolDiscovery),
    registration("enter_plan_mode", ToolActionKind::Mode),
    registration("exit_plan_mode", ToolActionKind::Mode),
    registration("image_gen", ToolActionKind::Artifact),
    registration("image_edit", ToolActionKind::Artifact),
    registration("image_to_video", ToolActionKind::Artifact),
];

pub fn project_request(
    writer_version: Option<&str>,
    name: &str,
    input: &Value,
    cwd: Option<&str>,
    correlation_key: &str,
    native_id: Option<&str>,
) -> Result<ToolInteraction, ProjectionError> {
    let lookup_name = if name.starts_with("Web search:") {
        "web_search"
    } else if name.starts_with("X search:") {
        "x_search"
    } else {
        name
    };
    let registration =
        lookup_registration(REGISTRATIONS, AGENT, writer_version, None, lookup_name)?;
    let action = match registration.action_kind {
        ToolActionKind::FileRead => {
            let path = first_string(input, &["path", "file_path"])
                .ok_or(ProjectionError::MissingField("path"))?;
            ToolAction::FileRead {
                targets: vec![ReadTarget {
                    resource: lexical_resource(path, cwd, false)?,
                    range: None,
                    media_kind: None,
                }],
                outcome: None,
            }
        }
        ToolActionKind::FileList => {
            let path = first_string(input, &["path", "directory"])
                .ok_or(ProjectionError::MissingField("directory"))?;
            ToolAction::FileList {
                directory: lexical_resource(path, cwd, true)?,
                depth: first_u64(input, &["depth"]).and_then(|value| u32::try_from(value).ok()),
                filter: first_string(input, &["filter"]).map(str::to_owned),
                outcome: None,
            }
        }
        ToolActionKind::FileSearch => {
            let query = first_string(input, &["pattern", "query"])
                .ok_or(ProjectionError::MissingField("query"))?;
            let roots = first_string(input, &["path", "directory"])
                .map(|path| lexical_resource(path, cwd, true))
                .transpose()?
                .into_iter()
                .collect();
            ToolAction::FileSearch {
                query: query.to_owned(),
                mode: FileSearchMode::Content,
                roots,
                filters: vec![],
                outcome: None,
            }
        }
        ToolActionKind::FileChange => {
            let path = first_string(input, &["path", "file_path"])
                .ok_or(ProjectionError::MissingField("path"))?;
            ToolAction::FileChange {
                changes: vec![FileDelta {
                    operation: None,
                    path: lexical_resource(path, cwd, false)?,
                    destination: None,
                    diff: None,
                    lines_added: None,
                    lines_removed: None,
                }],
            }
        }
        ToolActionKind::Shell => {
            let command = first_string(input, &["command"])
                .ok_or(ProjectionError::MissingField("command"))?;
            ToolAction::Shell {
                command: canonical_text(command)?,
                source_truncated: false,
                cwd: first_string(input, &["current_dir", "cwd"])
                    .or(cwd)
                    .map(|path| lexical_resource(path, cwd, true))
                    .transpose()?,
                timeout_ms: first_u64(input, &["timeout_ms", "timeout"]),
                background: first_bool(input, &["background"]).unwrap_or(false),
                outcome: None,
            }
        }
        ToolActionKind::Process => {
            let process_id = first_string(
                input,
                &["command_id", "process_id", "task_id", "subagent_id"],
            )
            .map(str::to_owned)
            .or_else(|| {
                input
                    .get("command_id")
                    .and_then(Value::as_i64)
                    .map(|value| value.to_string())
            })
            .ok_or(ProjectionError::MissingField("process id"))?;
            ToolAction::Process {
                operation: if lookup_name == "kill_command_or_subagent" {
                    ProcessOp::Stop
                } else {
                    ProcessOp::Wait
                },
                process_id,
                input: None,
                outcome: None,
            }
        }
        ToolActionKind::Delegate => {
            let objective = first_string(input, &["task", "objective", "prompt", "description"])
                .ok_or(ProjectionError::MissingField("objective"))?;
            ToolAction::Delegate {
                agent_kind: first_string(input, &["agent", "agent_kind"]).map(str::to_owned),
                name: first_string(input, &["name"]).map(str::to_owned),
                model: first_string(input, &["model"]).map(str::to_owned),
                mode: first_string(input, &["mode"]).map(str::to_owned),
                objective: canonical_text(objective)?,
                child_session_id: None,
                resume_id: first_string(input, &["resume_id"]).map(str::to_owned),
                background: first_bool(input, &["background"]).unwrap_or(false),
                orphan_result_summary: None,
            }
        }
        ToolActionKind::Task => ToolAction::Task {
            operation: TaskOp::Update,
            task_id: first_string(input, &["id", "goal_id", "task_id"]).map(str::to_owned),
            subject: first_string(input, &["title", "subject"]).map(str::to_owned),
            description: first_string(input, &["description", "content", "goal"])
                .map(canonical_text)
                .transpose()?,
            status: first_string(input, &["status"]).and_then(task_status),
            owner: None,
            blocker_ids: vec![],
        },
        ToolActionKind::Schedule => ToolAction::Schedule {
            operation: match lookup_name {
                "scheduler_cancel" => ScheduleOp::Delete,
                "scheduler_list" => ScheduleOp::List,
                "scheduler_wake" => ScheduleOp::Wake,
                _ => ScheduleOp::Create,
            },
            schedule_id: first_string(input, &["id", "schedule_id"]).map(str::to_owned),
            due_at: input.get("due_at").and_then(Value::as_i64),
            delay_ms: first_u64(input, &["delay_ms"]),
            reason: first_string(input, &["reason"]).map(str::to_owned),
            objective: first_string(input, &["objective", "prompt"])
                .map(canonical_text)
                .transpose()?,
        },
        ToolActionKind::WebSearch => ToolAction::WebSearch {
            query: first_string(input, &["query"])
                .map(str::to_owned)
                .or_else(|| {
                    name.split_once(':')
                        .map(|(_, query)| query.trim().to_owned())
                        .filter(|query| !query.is_empty())
                }),
            allowed_domains: vec![],
            blocked_domains: vec![],
            outcome: None,
        },
        ToolActionKind::WebFetch => {
            let target =
                first_string(input, &["url"]).ok_or(ProjectionError::MissingField("url"))?;
            ToolAction::WebFetch {
                url: url(target)?,
                purpose: first_string(input, &["purpose", "prompt"]).map(str::to_owned),
                outcome: None,
            }
        }
        ToolActionKind::Question => ToolAction::Question {
            questions: vec![],
            answers: vec![],
        },
        ToolActionKind::ToolDiscovery => {
            let query =
                first_string(input, &["query"]).ok_or(ProjectionError::MissingField("query"))?;
            ToolAction::ToolDiscovery {
                query: query.to_owned(),
                matches: vec![],
                total_count: 0,
            }
        }
        ToolActionKind::Mode => ToolAction::Mode {
            operation: if lookup_name == "enter_plan_mode" {
                ModeOp::Enter
            } else {
                ModeOp::Exit
            },
            mode: ExecutionMode::Plan,
            workspace: None,
        },
        ToolActionKind::Artifact => ToolAction::Artifact {
            operation: if lookup_name == "image_edit" {
                ArtifactOp::Edit
            } else {
                ArtifactOp::Create
            },
            artifact_kind: if lookup_name.ends_with("video") {
                ArtifactKind::Video
            } else {
                ArtifactKind::Image
            },
            name: first_string(input, &["name"]).map(str::to_owned),
            description: first_string(input, &["prompt", "description"])
                .map(canonical_text)
                .transpose()?,
            resources: first_string(input, &["path", "image_path", "output_path"])
                .map(|path| lexical_resource(path, cwd, false))
                .transpose()?
                .into_iter()
                .collect(),
            media: None,
        },
        _ => unreachable!("Grok registry and projector are closed together"),
    };
    open_interaction(
        correlation_key,
        CorrelationDerivation::NativeId,
        native_id.map(str::to_owned),
        None,
        name,
        None,
        action,
    )
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

#[cfg(test)]
mod tests {
    use super::*;
    use rosetta_adapter_support::tool::validate_registry;
    use serde_json::json;
    #[test]
    fn registry_and_fixture_shell_are_typed() {
        validate_registry(REGISTRATIONS).unwrap();
        let tool = project_request(
            None,
            "run_terminal_command",
            &json!({"command":"echo rosetta-mock", "description":"echo mock output"}),
            None,
            "call_mock_0004",
            Some("call_mock_0004"),
        )
        .unwrap();
        assert!(matches!(tool.action, ToolAction::Shell { .. }));
    }
    #[test]
    fn dynamic_web_label_keeps_optional_query() {
        let tool = project_request(
            None,
            "Web search: rust sqlite",
            &json!({}),
            None,
            "web-1",
            Some("web-1"),
        )
        .unwrap();
        assert!(matches!(
            tool.action,
            ToolAction::WebSearch { query: Some(_), .. }
        ));
    }
}
