//! Versioned OpenCode tool request projection.

use rosetta_adapter_support::tool::{
    NativeToolSelector, ProjectionError, ProjectorRegistration, SearchPolicy, StatusPolicy,
    VersionRange, canonical_text, first_string, lexical_resource, lookup_registration,
    open_interaction,
};
use rosetta_core::timeline::{
    CorrelationDerivation, ReadTarget, ToolAction, ToolActionKind, ToolInteraction,
};
use serde_json::Value;

pub const AGENT: &str = "opencode";
pub const FIXTURE_WRITER_VERSION: &str = "1.18.22";
pub const CURRENT_WRITER_VERSION: &str = "1.18.23";

const fn registration(name: &'static str, action_kind: ToolActionKind) -> ProjectorRegistration {
    ProjectorRegistration {
        agent: AGENT,
        writer_versions: VersionRange {
            min_inclusive: FIXTURE_WRITER_VERSION,
            max_inclusive: CURRENT_WRITER_VERSION,
        },
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
    registration("read", ToolActionKind::FileRead),
    registration("bash", ToolActionKind::Shell),
];

pub fn project_request(
    writer_version: Option<&str>,
    name: &str,
    input: &Value,
    cwd: Option<&str>,
    correlation_key: &str,
    native_id: Option<&str>,
) -> Result<ToolInteraction, ProjectionError> {
    let registration = lookup_registration(
        REGISTRATIONS,
        AGENT,
        writer_version.or(Some(FIXTURE_WRITER_VERSION)),
        None,
        name,
    )?;
    let action = match registration.action_kind {
        ToolActionKind::FileRead => {
            let path = first_string(input, &["filePath", "path", "file_path"])
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
                cwd: cwd
                    .map(|path| lexical_resource(path, None, true))
                    .transpose()?,
                timeout_ms: None,
                background: false,
                outcome: None,
            }
        }
        _ => unreachable!("OpenCode registry and projector are closed together"),
    };
    Ok(open_interaction(
        correlation_key,
        CorrelationDerivation::NativeId,
        native_id.map(str::to_owned),
        None,
        name,
        None,
        action,
    ))
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
            "bash",
            &json!({"command":"echo rosetta-mock"}),
            None,
            "call_mock_0003",
            Some("call_mock_0003"),
        )
        .unwrap();
        assert!(matches!(tool.action, ToolAction::Shell { .. }));
    }
}
