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
/// Newest writer whose fixtures replay through these projectors
/// (fixtures/1.4.12). The canonical ground truth stays pinned at
/// [`PINNED_WRITER_VERSION`]; this bound only widens what the registry accepts.
pub const LATEST_FIXTURE_WRITER_VERSION: &str = "1.4.12";

const fn registration(name: &'static str, action_kind: ToolActionKind) -> ProjectorRegistration {
    ProjectorRegistration {
        agent: AGENT,
        writer_versions: VersionRange {
            min_inclusive: PINNED_WRITER_VERSION,
            max_inclusive: LATEST_FIXTURE_WRITER_VERSION,
        },
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
    let registration = lookup_registration(REGISTRATIONS, AGENT, writer_version, None, name)?;
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
            let command = shell_command(input).ok_or(ProjectionError::MissingField("command"))?;
            ToolAction::Shell {
                command: canonical_text(&command)?,
                source_truncated: false,
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
    open_interaction(
        correlation_key,
        CorrelationDerivation::Positional,
        None,
        None,
        name,
        None,
        action,
    )
}

/// 1.1.20 writes `commandLine` as the plain command. 1.4.12 writes
/// `CommandLine`, and its primary transcript JSON-encodes every argument value
/// (`"\"echo x\""`), while `transcript_full.jsonl` keeps it plain. Decode the
/// encoded form; a value that is not a JSON string literal is already plain.
fn shell_command(input: &Value) -> Option<String> {
    if let Some(value) = input.get("CommandLine").and_then(Value::as_str) {
        return Some(serde_json::from_str::<String>(value).unwrap_or_else(|_| value.to_owned()));
    }
    first_string(input, &["commandLine", "command"]).map(str::to_owned)
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

    #[test]
    fn writer_1_4_12_command_line_decodes_from_both_transcript_forms() {
        // transcript.jsonl (JSON-encoded values) and transcript_full.jsonl
        // (typed values) of fixtures/1.4.12/tool-use.
        for args in [
            json!({"CommandLine": "\"echo rosetta-mock\"", "WaitMsBeforeAsync": "5000", "toolSummary": "\"Echo command check\""}),
            json!({"CommandLine": "echo rosetta-mock", "WaitMsBeforeAsync": 5000, "toolSummary": "Echo command check"}),
        ] {
            let tool = project_request(
                Some(LATEST_FIXTURE_WRITER_VERSION),
                "run_command",
                &args,
                None,
                "step#1:tool#0",
            )
            .unwrap();
            match tool.action {
                ToolAction::Shell { command, .. } => {
                    assert_eq!(command.as_preview(), "echo rosetta-mock")
                }
                other => panic!("expected shell, got {other:?}"),
            }
        }
    }
}
