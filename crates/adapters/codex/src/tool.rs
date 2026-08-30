//! Versioned Codex CLI tool request projection.

use rosetta_adapter_support::tool::{
    NativeToolSelector, ProjectionError, ProjectorRegistration, SearchPolicy, StatusPolicy,
    VersionRange, canonical_text, first_bool, first_string, first_u64, lexical_resource,
    lookup_registration, open_interaction, string_values, url,
};
use rosetta_core::timeline::{
    ArtifactKind, ArtifactOp, AttachmentKind, ChildTrace, CollaborationOp, CorrelationDerivation,
    FileDelta, PlanOp, ProcessOp, ReadTarget, ToolAction, ToolActionKind, ToolInteraction,
    WebNavigationOp,
};
use serde_json::Value;

pub const AGENT: &str = "codex";
pub const FIXTURE_WRITER_VERSION: &str = "0.149.1";
pub const CURRENT_WRITER_VERSION: &str = "0.150.1";

const fn registration(
    namespace: Option<&'static str>,
    name: &'static str,
    action_kind: ToolActionKind,
) -> ProjectorRegistration {
    ProjectorRegistration {
        agent: AGENT,
        writer_versions: VersionRange {
            min_inclusive: FIXTURE_WRITER_VERSION,
            max_inclusive: CURRENT_WRITER_VERSION,
        },
        native: NativeToolSelector { namespace, name },
        action_kind,
        status_policy: StatusPolicy::ExplicitLifecycle,
        search_policy: if matches!(action_kind, ToolActionKind::Orchestration) {
            SearchPolicy::None
        } else {
            SearchPolicy::Canonical
        },
        bounds_version: 1,
    }
}

pub const REGISTRATIONS: &[ProjectorRegistration] = &[
    registration(None, "exec", ToolActionKind::Orchestration),
    registration(None, "exec_command", ToolActionKind::Shell),
    registration(None, "local_shell_call", ToolActionKind::Shell),
    registration(None, "shell_command", ToolActionKind::Shell),
    registration(None, "write_stdin", ToolActionKind::Process),
    registration(None, "wait", ToolActionKind::Process),
    registration(None, "apply_patch", ToolActionKind::FileChange),
    registration(None, "view_image", ToolActionKind::FileRead),
    registration(None, "web_search_call", ToolActionKind::WebSearch),
    registration(None, "image_generation_call", ToolActionKind::Artifact),
    registration(None, "tool_search_call", ToolActionKind::ToolDiscovery),
    registration(None, "update_plan", ToolActionKind::Plan),
    registration(None, "spawn_agent", ToolActionKind::Delegate),
    registration(None, "followup_task", ToolActionKind::Delegate),
    registration(None, "wait_agent", ToolActionKind::Delegate),
    registration(None, "send_message", ToolActionKind::Collaborate),
    registration(None, "list_agents", ToolActionKind::Collaborate),
    registration(Some("functions"), "node_repl", ToolActionKind::CodeExecute),
    registration(Some("functions"), "js", ToolActionKind::CodeExecute),
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
    let registration = lookup_registration(
        REGISTRATIONS,
        AGENT,
        writer_version.or(Some(FIXTURE_WRITER_VERSION)),
        namespace,
        name,
    )?;
    let action = match registration.action_kind {
        ToolActionKind::Orchestration => ToolAction::Orchestration {
            runtime: first_string(input, &["runtime"]).map(str::to_owned),
            trace: ChildTrace::Unavailable,
            executed_children: vec![],
            executed_child_count: None,
        },
        ToolActionKind::Shell => project_shell(input, cwd)?,
        ToolActionKind::Process => project_process(name, input)?,
        ToolActionKind::FileChange => project_patch(input, cwd)?,
        ToolActionKind::FileRead => {
            let path = first_string(input, &["path", "image_path", "file_path"])
                .ok_or(ProjectionError::MissingField("path"))?;
            ToolAction::FileRead {
                targets: vec![ReadTarget {
                    resource: lexical_resource(path, cwd, false)?,
                    range: None,
                    media_kind: Some(AttachmentKind::Image),
                }],
                outcome: None,
            }
        }
        ToolActionKind::WebSearch => project_web(input)?,
        ToolActionKind::Artifact => {
            let resources = first_string(input, &["path", "output_path"])
                .map(|path| lexical_resource(path, cwd, false))
                .transpose()?
                .into_iter()
                .collect();
            ToolAction::Artifact {
                operation: ArtifactOp::Create,
                artifact_kind: ArtifactKind::Image,
                name: first_string(input, &["name"]).map(str::to_owned),
                description: first_string(input, &["prompt", "description"])
                    .map(canonical_text)
                    .transpose()?,
                resources,
                media: None,
            }
        }
        ToolActionKind::ToolDiscovery => {
            let query =
                first_string(input, &["query"]).ok_or(ProjectionError::MissingField("query"))?;
            ToolAction::ToolDiscovery {
                query: query.to_owned(),
                matches: vec![],
                total_count: 0,
            }
        }
        ToolActionKind::Plan => ToolAction::Plan {
            operation: PlanOp::Update,
            plan_id: None,
            title: None,
            summary: first_string(input, &["explanation", "summary"])
                .map(canonical_text)
                .transpose()?,
            step_count: input
                .get("plan")
                .and_then(Value::as_array)
                .map(|steps| steps.len() as u64),
        },
        ToolActionKind::Delegate => project_delegate(name, input)?,
        ToolActionKind::Collaborate => project_collaboration(name, input)?,
        ToolActionKind::CodeExecute => {
            let code = first_string(input, &["code", "input"])
                .or_else(|| input.as_str())
                .ok_or(ProjectionError::MissingField("code"))?;
            ToolAction::CodeExecute {
                language: Some(
                    if name == "node_repl" {
                        "javascript"
                    } else {
                        name
                    }
                    .to_owned(),
                ),
                code: canonical_text(code)?,
                cwd: cwd
                    .map(|path| lexical_resource(path, None, true))
                    .transpose()?,
                outcome: None,
            }
        }
        _ => unreachable!("Codex registry and projector are closed together"),
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

fn project_shell(input: &Value, cwd: Option<&str>) -> Result<ToolAction, ProjectionError> {
    let command = first_string(input, &["cmd", "command"])
        .map(str::to_owned)
        .or_else(|| {
            input.get("cmd").and_then(Value::as_array).map(|parts| {
                parts
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" ")
            })
        })
        .ok_or(ProjectionError::MissingField("command"))?;
    Ok(ToolAction::Shell {
        command: canonical_text(command)?,
        cwd: first_string(input, &["workdir", "cwd"])
            .or(cwd)
            .map(|path| lexical_resource(path, cwd, true))
            .transpose()?,
        timeout_ms: first_u64(input, &["yield_time_ms", "yield_time_ms", "timeout_ms"]),
        background: first_bool(input, &["background"]).unwrap_or(false),
        outcome: None,
    })
}

fn project_process(name: &str, input: &Value) -> Result<ToolAction, ProjectionError> {
    let process_id = first_string(input, &["session_id", "process_id", "cell_id"])
        .map(str::to_owned)
        .or_else(|| {
            input
                .get("session_id")
                .and_then(Value::as_i64)
                .map(|id| id.to_string())
        })
        .ok_or(ProjectionError::MissingField("process id"))?;
    Ok(ToolAction::Process {
        operation: if name == "write_stdin" {
            ProcessOp::Input
        } else {
            ProcessOp::Wait
        },
        process_id,
        input: first_string(input, &["chars", "input"])
            .filter(|value| !value.is_empty())
            .map(canonical_text)
            .transpose()?,
        outcome: None,
    })
}

fn project_patch(input: &Value, cwd: Option<&str>) -> Result<ToolAction, ProjectionError> {
    let patch = first_string(input, &["patch", "input"])
        .or_else(|| input.as_str())
        .ok_or(ProjectionError::MissingField("patch"))?;
    let mut changes = Vec::new();
    for line in patch.lines() {
        let Some(header) = line.strip_prefix("*** ") else {
            continue;
        };
        let Some((verb, path)) = header.split_once(": ") else {
            continue;
        };
        if !matches!(verb, "Add File" | "Update File" | "Delete File") {
            continue;
        }
        changes.push(FileDelta {
            // An Add/Delete patch shape is strong native evidence; a whole-file
            // write outside this syntax still leaves operation absent.
            operation: match verb {
                "Add File" => Some(rosetta_core::timeline::FileChangeOp::Create),
                "Delete File" => Some(rosetta_core::timeline::FileChangeOp::Delete),
                "Update File" => Some(rosetta_core::timeline::FileChangeOp::Update),
                _ => None,
            },
            path: lexical_resource(path.trim(), cwd, false)?,
            destination: None,
            diff: None,
            lines_added: None,
            lines_removed: None,
        });
    }
    if changes.is_empty() {
        return Err(ProjectionError::InvalidField("patch headers"));
    }
    Ok(ToolAction::FileChange { changes })
}

fn project_web(input: &Value) -> Result<ToolAction, ProjectionError> {
    let action = input.get("action").unwrap_or(input);
    let operation = first_string(action, &["type", "operation"]).unwrap_or("search");
    match operation {
        "open_page" | "open" => Ok(ToolAction::WebNavigate {
            operation: WebNavigationOp::OpenPage,
            url: first_string(action, &["url"]).map(url).transpose()?,
            pattern: None,
            outcome: None,
        }),
        "find_in_page" | "find" => Ok(ToolAction::WebNavigate {
            operation: WebNavigationOp::FindInPage,
            url: first_string(action, &["url"]).map(url).transpose()?,
            pattern: first_string(action, &["pattern", "query"]).map(str::to_owned),
            outcome: None,
        }),
        "search" | "search_query" => Ok(ToolAction::WebSearch {
            query: first_string(action, &["query", "q"]).map(str::to_owned),
            allowed_domains: string_values(action, &["domains", "allowed_domains"], 20),
            blocked_domains: string_values(action, &["blocked_domains"], 20),
            outcome: None,
        }),
        _ => Err(ProjectionError::InvalidField("web action type")),
    }
}

fn project_delegate(name: &str, input: &Value) -> Result<ToolAction, ProjectionError> {
    let objective = first_string(input, &["message", "objective", "task"]).unwrap_or(name);
    Ok(ToolAction::Delegate {
        agent_kind: first_string(input, &["agent", "agent_kind"]).map(str::to_owned),
        name: first_string(input, &["name"]).map(str::to_owned),
        model: first_string(input, &["model"]).map(str::to_owned),
        mode: first_string(input, &["mode"]).map(str::to_owned),
        objective: canonical_text(objective)?,
        child_session_id: None,
        resume_id: first_string(input, &["target", "resume_id"]).map(str::to_owned),
        background: first_bool(input, &["background"]).unwrap_or(false),
        orphan_result_summary: None,
    })
}

fn project_collaboration(name: &str, input: &Value) -> Result<ToolAction, ProjectionError> {
    Ok(ToolAction::Collaborate {
        operation: if name == "list_agents" {
            CollaborationOp::List
        } else {
            CollaborationOp::Send
        },
        recipient: first_string(input, &["target", "recipient"]).map(str::to_owned),
        team: None,
        summary: None,
        body: first_string(input, &["message"])
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
    fn registry_and_fixture_shell_are_typed() {
        validate_registry(REGISTRATIONS).unwrap();
        let tool = project_request(
            Some(FIXTURE_WRITER_VERSION),
            "exec_command",
            &json!({"cmd":"echo rosetta-mock"}),
            None,
            "call_mock_0001",
            Some("call_mock_0001"),
            None,
        )
        .unwrap();
        assert!(matches!(tool.action, ToolAction::Shell { .. }));
    }

    #[test]
    fn opaque_exec_never_infers_static_child_calls() {
        let tool = project_request(
            Some(CURRENT_WRITER_VERSION),
            "exec",
            &json!({"program":"await tools.exec_command({cmd: 'git status'})"}),
            None,
            "exec-1",
            Some("exec-1"),
            None,
        )
        .unwrap();
        assert_eq!(
            tool.action,
            ToolAction::Orchestration {
                runtime: None,
                trace: ChildTrace::Unavailable,
                executed_children: vec![],
                executed_child_count: None
            }
        );
    }

    #[test]
    fn web_action_dispatches_by_native_operation() {
        let tool = project_request(Some(CURRENT_WRITER_VERSION), "web_search_call", &json!({"action":{"type":"find_in_page", "url":"https://example.test", "pattern":"needle"}}), None, "web-1", Some("web-1"), None).unwrap();
        assert!(matches!(
            tool.action,
            ToolAction::WebNavigate {
                operation: WebNavigationOp::FindInPage,
                ..
            }
        ));
    }

    #[test]
    fn code_execution_requires_the_registered_namespace() {
        assert!(
            project_request(
                Some(CURRENT_WRITER_VERSION),
                "js",
                &json!({"code":"1+1"}),
                None,
                "js-1",
                Some("js-1"),
                None
            )
            .is_err()
        );
        assert!(
            project_request(
                Some(CURRENT_WRITER_VERSION),
                "js",
                &json!({"code":"1+1"}),
                None,
                "js-1",
                Some("js-1"),
                Some("functions")
            )
            .is_ok()
        );
    }
}
