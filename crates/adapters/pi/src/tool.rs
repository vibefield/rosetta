//! Versioned Pi tool request projection.

use rosetta_adapter_support::tool::{
    NativeToolSelector, ProjectionError, ProjectorRegistration, SearchPolicy, StatusPolicy,
    VersionRange, canonical_text, first_string, lexical_resource, lookup_registration,
    open_interaction,
};
use rosetta_core::timeline::{
    CorrelationDerivation, ReadTarget, ToolAction, ToolActionKind, ToolInteraction,
};
use serde_json::Value;

pub const AGENT: &str = "pi";
pub const PINNED_WRITER_VERSION: &str = "0.84.3";

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
    registration("read", ToolActionKind::FileRead),
    registration("bash", ToolActionKind::Shell),
    registration("BashExecution", ToolActionKind::Shell),
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
        _ => unreachable!("Pi registry and projector are closed together"),
    };
    open_interaction(
        correlation_key,
        if native_id.is_some() {
            CorrelationDerivation::NativeId
        } else {
            CorrelationDerivation::Synthetic
        },
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
    fn registry_and_fixture_shell_are_typed() {
        validate_registry(REGISTRATIONS).unwrap();
        let tool = project_request(
            None,
            "bash",
            &json!({"command":"echo rosetta-mock"}),
            None,
            "call_mock_0001",
            Some("call_mock_0001"),
        )
        .unwrap();
        assert!(matches!(tool.action, ToolAction::Shell { .. }));
    }
}
