//! Versioned Kimi Code CLI tool request projection.

use rosetta_adapter_support::tool::{
    NativeToolSelector, ProjectionError, ProjectorRegistration, SearchPolicy, StatusPolicy,
    VersionRange, canonical_text, first_bool, first_string, lexical_resource, lookup_registration,
    open_interaction, url,
};
use rosetta_core::TaskStatus;
use rosetta_core::timeline::{
    CorrelationDerivation, FileDelta, FileSearchMode, ReadTarget, TaskOp, ToolAction,
    ToolActionKind, ToolInteraction,
};
use serde_json::Value;

pub const AGENT: &str = "kimi-code";
pub const PINNED_WRITER_VERSION: &str = "0.38.0";

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
        status_policy: StatusPolicy::TerminalRecord,
        search_policy: SearchPolicy::Canonical,
        bounds_version: 1,
    }
}

pub const REGISTRATIONS: &[ProjectorRegistration] = &[
    registration("Read", ToolActionKind::FileRead),
    registration("ReadMediaFile", ToolActionKind::FileRead),
    registration("Grep", ToolActionKind::FileSearch),
    registration("Edit", ToolActionKind::FileChange),
    registration("Write", ToolActionKind::FileChange),
    registration("Bash", ToolActionKind::Shell),
    registration("FetchURL", ToolActionKind::WebFetch),
    registration("Agent", ToolActionKind::Delegate),
    registration("TodoList", ToolActionKind::Task),
];

pub fn project_request(
    writer_version: Option<&str>,
    name: &str,
    input: &Value,
    cwd: Option<&str>,
    correlation_key: &str,
    native_id: Option<&str>,
) -> Result<ToolInteraction, ProjectionError> {
    let registration = lookup_registration(REGISTRATIONS, AGENT, writer_version, None, name)?;
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
        ToolActionKind::FileSearch => {
            let query = first_string(input, &["pattern", "query"])
                .ok_or(ProjectionError::MissingField("query"))?
                .to_owned();
            let roots = first_string(input, &["path", "directory"])
                .map(|path| lexical_resource(path, cwd, true))
                .transpose()?
                .into_iter()
                .collect();
            ToolAction::FileSearch {
                query,
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
                cwd: cwd
                    .map(|path| lexical_resource(path, None, true))
                    .transpose()?,
                timeout_ms: None,
                background: first_bool(input, &["background"]).unwrap_or(false),
                outcome: None,
            }
        }
        ToolActionKind::WebFetch => {
            let target =
                first_string(input, &["url"]).ok_or(ProjectionError::MissingField("url"))?;
            ToolAction::WebFetch {
                url: url(target)?,
                purpose: first_string(input, &["prompt", "purpose"]).map(str::to_owned),
                outcome: None,
            }
        }
        ToolActionKind::Delegate => {
            let objective = first_string(input, &["description", "objective", "prompt"])
                .ok_or(ProjectionError::MissingField("objective"))?;
            ToolAction::Delegate {
                agent_kind: first_string(input, &["agent", "profile"]).map(str::to_owned),
                name: None,
                model: first_string(input, &["model"]).map(str::to_owned),
                mode: first_string(input, &["mode"]).map(str::to_owned),
                team: None,
                color: None,
                objective: canonical_text(objective)?,
                child_session_id: None,
                resume_id: first_string(input, &["resume_id"]).map(str::to_owned),
                background: first_bool(input, &["background"]).unwrap_or(false),
                orphan_result_summary: None,
            }
        }
        ToolActionKind::Task => ToolAction::Task {
            operation: TaskOp::Update,
            task_id: first_string(input, &["id", "task_id"]).map(str::to_owned),
            subject: first_string(input, &["title", "subject"]).map(str::to_owned),
            description: first_string(input, &["description", "content"])
                .map(canonical_text)
                .transpose()?,
            status: first_string(input, &["status"]).and_then(task_status),
            owner: None,
            blocker_ids: vec![],
            snapshot: None,
        },
        _ => unreachable!("Kimi registry and projector are closed together"),
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
    match value.to_ascii_lowercase().as_str() {
        "pending" => Some(TaskStatus::Pending),
        "in_progress" | "in progress" => Some(TaskStatus::InProgress),
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
            "Bash",
            &json!({"command":"echo rosetta-mock"}),
            None,
            "call_mock_0001",
            Some("call_mock_0001"),
        )
        .unwrap();
        assert!(matches!(tool.action, ToolAction::Shell { .. }));
    }
}
