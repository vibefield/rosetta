//! Versioned Hermes Agent tool request projection.

use rosetta_adapter_support::tool::{
    NativeToolSelector, ProjectionError, ProjectorRegistration, SearchPolicy, StatusPolicy,
    VersionRange, canonical_text, first_string, lexical_resource, lookup_registration,
    open_interaction,
};
use rosetta_core::timeline::{
    CorrelationDerivation, FileSearchMode, ReadTarget, ToolAction, ToolActionKind, ToolInteraction,
};
use serde_json::Value;

pub const AGENT: &str = "hermes";
pub const PINNED_WRITER_VERSION: &str = "0.20.5";

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
        status_policy: StatusPolicy::NativeState,
        search_policy: SearchPolicy::Canonical,
        bounds_version: 1,
    }
}

pub const REGISTRATIONS: &[ProjectorRegistration] = &[
    registration("read_file", ToolActionKind::FileRead),
    registration("search_files", ToolActionKind::FileSearch),
    registration("execute_code", ToolActionKind::CodeExecute),
    registration("terminal", ToolActionKind::Shell),
    registration("skill_view", ToolActionKind::Skill),
];

/// Whether the closed registry maps `name` onto the fixed semantic
/// vocabulary. The L1 `tool-calls` facet is the fixture-proven subset
/// (0.20.5: `terminal`); the registry also names the 0.21.x tools observed
/// in James's store (`read_file`, `search_files`, `execute_code`,
/// `skill_view`) until the L1 re-pin captures them natively.
pub fn is_registered(name: &str) -> bool {
    REGISTRATIONS
        .iter()
        .any(|registration| registration.native.name == name)
}

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
            let path = first_string(input, &["path", "file_path", "filename"])
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
            let query = first_string(input, &["query", "pattern"])
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
        ToolActionKind::CodeExecute => {
            let code =
                first_string(input, &["code"]).ok_or(ProjectionError::MissingField("code"))?;
            ToolAction::CodeExecute {
                language: first_string(input, &["language", "runtime"]).map(str::to_owned),
                code: canonical_text(code)?,
                source_truncated: false,
                cwd: cwd
                    .map(|path| lexical_resource(path, None, true))
                    .transpose()?,
                outcome: None,
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
                background: false,
                outcome: None,
            }
        }
        ToolActionKind::Skill => {
            let skill = first_string(input, &["name", "skill_name", "skill"])
                .ok_or(ProjectionError::MissingField("skill name"))?;
            ToolAction::Skill {
                name: skill.to_owned(),
                arguments: None,
            }
        }
        _ => unreachable!("Hermes registry and projector are closed together"),
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

#[cfg(test)]
mod tests {
    use super::*;
    use rosetta_adapter_support::tool::validate_registry;
    use serde_json::json;

    #[test]
    fn registry_is_closed_and_fixture_shell_is_reduced() {
        validate_registry(REGISTRATIONS).unwrap();
        let tool = project_request(
            None,
            "terminal",
            &json!({"command":"echo rosetta-mock"}),
            None,
            "call_mock_0002",
            Some("call_mock_0002"),
        )
        .unwrap();
        assert!(matches!(tool.action, ToolAction::Shell { .. }));
    }

    #[test]
    fn every_fixture_declared_tool_is_registered() {
        for name in crate::spec::declared_tool_names() {
            assert!(
                is_registered(name),
                "{name} is declared by L1 but not registered"
            );
        }
        for name in ["read_file", "search_files", "execute_code", "skill_view"] {
            assert!(is_registered(name));
        }
        assert!(!is_registered("web_search"));
    }
}
