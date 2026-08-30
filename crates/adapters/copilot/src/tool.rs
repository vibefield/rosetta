//! Versioned GitHub Copilot CLI tool request projection.

use rosetta_adapter_support::tool::{
    NativeToolSelector, ProjectionError, ProjectorRegistration, SearchPolicy, StatusPolicy,
    VersionRange, canonical_text, first_string, lexical_resource, lookup_registration,
    open_interaction,
};
use rosetta_core::timeline::{
    CorrelationDerivation, FileSearchMode, ReadTarget, SearchFilter, SearchFilterKind, ToolAction,
    ToolActionKind, ToolInteraction,
};
use serde_json::Value;

pub const AGENT: &str = "copilot";
pub const PINNED_WRITER_VERSION: &str = "1.0.80";

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
        status_policy: StatusPolicy::ExplicitLifecycle,
        search_policy: SearchPolicy::Canonical,
        bounds_version: 1,
    }
}

pub const REGISTRATIONS: &[ProjectorRegistration] = &[
    registration("view", ToolActionKind::FileRead),
    registration("glob", ToolActionKind::FileSearch),
    registration("rg", ToolActionKind::FileSearch),
    registration("bash", ToolActionKind::Shell),
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
            let query = first_string(input, &["query", "pattern", "glob"])
                .ok_or(ProjectionError::MissingField("query"))?
                .to_owned();
            let roots = first_string(input, &["path", "directory", "cwd"])
                .map(|path| lexical_resource(path, cwd, true))
                .transpose()?
                .into_iter()
                .collect();
            let filters = first_string(input, &["glob"])
                .filter(|glob| *glob != query)
                .map(|glob| SearchFilter {
                    kind: SearchFilterKind::Glob,
                    value: glob.to_owned(),
                })
                .into_iter()
                .collect();
            ToolAction::FileSearch {
                query,
                mode: if name == "glob" {
                    FileSearchMode::Glob
                } else {
                    FileSearchMode::Content
                },
                roots,
                filters,
                outcome: None,
            }
        }
        ToolActionKind::Shell => {
            let command = first_string(input, &["command"])
                .ok_or(ProjectionError::MissingField("command"))?;
            ToolAction::Shell {
                command: canonical_text(command)?,
                cwd: cwd
                    .map(|path| lexical_resource(path, None, true))
                    .transpose()?,
                timeout_ms: None,
                background: false,
                outcome: None,
            }
        }
        _ => unreachable!("Copilot registry and projector are closed together"),
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

#[cfg(test)]
mod tests {
    use super::*;
    use rosetta_adapter_support::tool::validate_registry;
    use serde_json::json;

    #[test]
    fn registry_and_fixture_shell_are_typed() {
        validate_registry(REGISTRATIONS).unwrap();
        let tool = project_request(
            Some(PINNED_WRITER_VERSION),
            "bash",
            &json!({"command":"echo rosetta-mock"}),
            None,
            "call_mock_0001",
            Some("call_mock_0001"),
            None,
        )
        .unwrap();
        assert!(matches!(tool.action, ToolAction::Shell { .. }));
    }

    #[test]
    fn unregistered_mcp_tool_does_not_inherit_a_builtin_name() {
        assert!(
            project_request(
                Some(PINNED_WRITER_VERSION),
                "bash",
                &json!({"command":"unsafe"}),
                None,
                "mcp-1",
                Some("mcp-1"),
                Some("third-party")
            )
            .is_err()
        );
    }
}
