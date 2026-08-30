//! Versioned Antigravity CLI tool request projection.

use rosetta_adapter_support::tool::{
    NativeToolSelector, ProjectionError, ProjectorRegistration, SearchPolicy, StatusPolicy,
    VersionRange, canonical_text, first_string, lexical_resource, lookup_registration,
    open_interaction,
};
use rosetta_core::timeline::{
    CorrelationDerivation, FileSearchMode, ReadTarget, ToolAction, ToolActionKind, ToolInteraction,
};
use serde_json::Value;

pub const AGENT: &str = "antigravity";
pub const PINNED_WRITER_VERSION: &str = "1.1.20";

const fn registration(name: &'static str, action_kind: ToolActionKind) -> ProjectorRegistration {
    ProjectorRegistration {
        agent: AGENT,
        writer_versions: VersionRange::exact(PINNED_WRITER_VERSION),
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
    registration("view_file", ToolActionKind::FileRead),
    registration("list_dir", ToolActionKind::FileList),
    registration("find_by_name", ToolActionKind::FileSearch),
    registration("run_command", ToolActionKind::Shell),
];

pub fn project_request(
    writer_version: Option<&str>,
    name: &str,
    input: &Value,
    cwd: Option<&str>,
    correlation_key: &str,
) -> Result<ToolInteraction, ProjectionError> {
    let registration = lookup_registration(
        REGISTRATIONS,
        AGENT,
        writer_version.or(Some(PINNED_WRITER_VERSION)),
        None,
        name,
    )?;
    let action = match registration.action_kind {
        ToolActionKind::FileRead => {
            let path = first_string(input, &["AbsolutePath", "absolute_path", "path"])
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
            let path = first_string(input, &["DirectoryPath", "directory", "path"])
                .ok_or(ProjectionError::MissingField("directory"))?;
            ToolAction::FileList {
                directory: lexical_resource(path, cwd, true)?,
                depth: None,
                filter: None,
                outcome: None,
            }
        }
        ToolActionKind::FileSearch => {
            let query = first_string(input, &["Pattern", "pattern", "name", "query"])
                .ok_or(ProjectionError::MissingField("query"))?
                .to_owned();
            let roots = first_string(input, &["SearchDirectory", "directory", "path"])
                .map(|path| lexical_resource(path, cwd, true))
                .transpose()?
                .into_iter()
                .collect();
            ToolAction::FileSearch {
                query,
                mode: FileSearchMode::Name,
                roots,
                filters: Vec::new(),
                outcome: None,
            }
        }
        ToolActionKind::Shell => {
            let command = first_string(input, &["commandLine", "command"])
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
        _ => unreachable!("Antigravity registry and projector are closed together"),
    };
    Ok(open_interaction(
        correlation_key,
        CorrelationDerivation::Positional,
        None,
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
    fn registry_is_closed_and_non_overlapping() {
        validate_registry(REGISTRATIONS).unwrap();
    }

    #[test]
    fn fixture_shell_request_keeps_command_and_drops_description() {
        let tool = project_request(
            Some(PINNED_WRITER_VERSION),
            "run_command",
            &json!({"commandLine": "echo rosetta-mock", "description": "noise"}),
            Some("/project"),
            "step#1:tool#0",
        )
        .unwrap();
        match tool.action {
            ToolAction::Shell { command, cwd, .. } => {
                assert_eq!(command.as_preview(), "echo rosetta-mock");
                assert_eq!(cwd.unwrap().display_value(), "/project");
            }
            other => panic!("expected shell, got {other:?}"),
        }
    }
}
