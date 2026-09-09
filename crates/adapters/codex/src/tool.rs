//! Versioned Codex CLI tool request projection.
//!
//! Coverage is declared per release LINE, not per release. `groundtruth/codex`
//! pins fixtures at 0.149.1 and surveyed 0.150.1; Codex ships a new minor every
//! few days, and the persisted rollout tool vocabulary registered below is the
//! same one from 0.144.6 through 0.152.0 (privacy-safe name census of the local
//! rollout corpus, 2026-09-01). A two-release window therefore erased the typed
//! vocabulary for nearly every rollout on disk, so the range covers the 0.x line.
//!
//! Failing closed is preserved where it carries evidence. A 1.x writer is a new
//! vocabulary claim and still projects `unknown` until the pack is re-surveyed;
//! a name added by a later release is unregistered and still projects `unknown`;
//! and a changed input shape inside the line still fails closed, because
//! `project_request` returns `MissingField`/`InvalidField` and the caller records
//! a drift fact.

use rosetta_adapter_support::tool::{
    NativeToolSelector, ProjectionError, ProjectorRegistration, SearchPolicy, StatusPolicy,
    VersionRange, canonical_text, first_bool, first_string, first_u64, lexical_resource,
    lookup_registration, open_interaction, string_values, url,
};
use rosetta_core::timeline::{
    ArtifactKind, ArtifactOp, AttachmentKind, ChildTrace, CollaborationOp, CorrelationDerivation,
    FileChangeOp, FileDelta, PlanOp, ProcessOp, ReadTarget, ToolAction, ToolActionKind,
    ToolInteraction, WebNavigationOp,
};
use serde_json::Value;

use crate::exec_program::{
    EmbeddedCall, argument_string, has_property, literal_parallel_calls, nested_property_count,
    nested_property_string, property_bool, property_string, property_u64, tool_calls,
};

/// Calls that exist only to continue the outer tool transport. They are not
/// agent activity in their own right: an empty `write_stdin` polls an existing
/// command, while `wait` resumes a yielded `functions.exec` cell. The
/// normalizer keeps these addresses transiently so their results can enrich
/// the original command without emitting a second ladder row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TransportCall {
    WaitCell(String),
    PollProcesses(Vec<String>),
}

pub(crate) fn transport_call(name: &str, input: &Value) -> Option<TransportCall> {
    if name == "wait" {
        return direct_process_id(input).map(TransportCall::WaitCell);
    }

    if name == "write_stdin" {
        let has_input =
            first_string(input, &["chars", "input"]).is_some_and(|value| !value.is_empty());
        return (!has_input)
            .then(|| direct_process_id(input))
            .flatten()
            .map(|id| TransportCall::PollProcesses(vec![id]));
    }

    if name != "exec" {
        return None;
    }
    let program = input
        .as_str()
        .or_else(|| first_string(input, &["program", "code", "input"]))?;
    let calls = tool_calls(program);
    if calls.is_empty() || calls.iter().any(|call| call.name != "write_stdin") {
        return None;
    }

    let mut process_ids = Vec::with_capacity(calls.len());
    for call in &calls {
        if property_string(program, call, &["chars", "input"])
            .is_some_and(|value| !value.is_empty())
        {
            return None;
        }
        process_ids.push(embedded_process_id(program, call)?);
    }
    Some(TransportCall::PollProcesses(process_ids))
}

fn direct_process_id(input: &Value) -> Option<String> {
    first_string(input, &["session_id", "process_id", "cell_id"])
        .map(str::to_owned)
        .or_else(|| {
            first_u64(input, &["session_id", "process_id", "cell_id"])
                .map(|value| value.to_string())
        })
}

fn embedded_process_id(program: &str, call: &EmbeddedCall) -> Option<String> {
    property_string(program, call, &["session_id", "process_id", "cell_id"]).or_else(|| {
        property_u64(call, &["session_id", "process_id", "cell_id"]).map(|value| value.to_string())
    })
}

pub const AGENT: &str = "codex";
/// Writer the committed fixtures were farmed from (`watch.toml` `pinned`).
pub const FIXTURE_WRITER_VERSION: &str = "0.149.1";
/// Newest writer surveyed on the release channel (`watch.toml` `surveyed`).
pub const CURRENT_WRITER_VERSION: &str = "0.150.1";
/// Oldest writer observed to persist this rollout tool vocabulary.
pub const EARLIEST_WRITER_VERSION: &str = "0.144.6";
/// Ceiling of the 0.x release line. `1.0.0` and later fail closed to `unknown`.
pub const WRITER_LINE_CEILING: &str = "0.99999.0";

const fn registration(
    namespace: Option<&'static str>,
    name: &'static str,
    action_kind: ToolActionKind,
) -> ProjectorRegistration {
    ProjectorRegistration {
        agent: AGENT,
        writer_versions: VersionRange {
            min_inclusive: EARLIEST_WRITER_VERSION,
            max_inclusive: WRITER_LINE_CEILING,
        },
        accepts_unversioned: false,
        native: NativeToolSelector { namespace, name },
        action_kind,
        status_policy: StatusPolicy::ExplicitLifecycle,
        // `exec` is an outer persistence envelope. Its reviewed JavaScript
        // body can project to a canonical child family, while a genuinely
        // opaque body remains an empty, non-searchable orchestration action.
        search_policy: SearchPolicy::Canonical,
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
    let registration = lookup_registration(REGISTRATIONS, AGENT, writer_version, namespace, name)?;
    let action = match registration.action_kind {
        ToolActionKind::Orchestration => project_exec(input, cwd)?,
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
                source_truncated: false,
                cwd: cwd
                    .map(|path| lexical_resource(path, None, true))
                    .transpose()?,
                outcome: None,
            }
        }
        _ => unreachable!("Codex registry and projector are closed together"),
    };
    open_interaction(
        correlation_key,
        if native_id.is_some() {
            CorrelationDerivation::NativeId
        } else {
            CorrelationDerivation::Synthetic
        },
        native_id.map(str::to_owned),
        namespace.map(str::to_owned),
        name,
        namespace.map(str::to_owned),
        action,
    )
}

/// Expand a statically-proven literal `Promise.all([call, call])` into the
/// semantic child actions. The outer `functions.exec` call is transport; when
/// every direct element is understood, retaining an opaque `exec_command`
/// name list would throw away the command text the source actually recorded.
pub(crate) fn project_exec_batch(
    input: &Value,
    cwd: Option<&str>,
    correlation_key: &str,
) -> Option<Vec<ToolInteraction>> {
    let program = input
        .as_str()
        .or_else(|| first_string(input, &["program", "code", "input"]))?;
    literal_parallel_calls(program)?
        .iter()
        .enumerate()
        .map(|(index, call)| {
            let action = project_embedded_call(program, call, cwd)?;
            open_interaction(
                format!("{correlation_key}#child-{index}"),
                CorrelationDerivation::Synthetic,
                None,
                None,
                &call.name,
                None,
                action,
            )
            .ok()
        })
        .collect()
}

fn project_exec(input: &Value, cwd: Option<&str>) -> Result<ToolAction, ProjectionError> {
    let program = input
        .as_str()
        .or_else(|| first_string(input, &["program", "code", "input"]))
        .ok_or(ProjectionError::MissingField("exec program"))?;
    let calls = tool_calls(program);

    // A one-call envelope is transport, not user intent. Project the explicit
    // inner call so Codex has the same semantic vocabulary as every other
    // adapter. If a required scalar is dynamic, retain the honest batch fact.
    if let [call] = calls.as_slice()
        && let Some(action) = project_embedded_call(program, call, cwd)
    {
        return Ok(action);
    }

    Ok(ToolAction::Orchestration {
        runtime: Some("functions.exec".to_owned()),
        trace: if calls.is_empty() {
            ChildTrace::Unavailable
        } else {
            ChildTrace::Partial
        },
        executed_children: calls.into_iter().map(|call| call.name).collect(),
        // These are statically visible call sites, not an execution trace. A
        // call inside `items.map(...)` may run many times, so no numeric total
        // is claimed until the source persists one.
        executed_child_count: None,
    })
}

fn project_embedded_call(
    program: &str,
    call: &EmbeddedCall,
    cwd: Option<&str>,
) -> Option<ToolAction> {
    match call.name.as_str() {
        "exec_command" => project_embedded_shell(program, call, cwd).ok(),
        "write_stdin" => project_embedded_process(program, call).ok(),
        "apply_patch" => {
            let patch = argument_string(program, call)?;
            project_patch(&Value::String(patch), cwd).ok()
        }
        "mcp__node_repl__js" => {
            let code = property_string(program, call, &["code", "input"])?;
            Some(ToolAction::CodeExecute {
                language: Some("javascript".to_owned()),
                code: canonical_text(code).ok()?,
                source_truncated: false,
                cwd: cwd
                    .map(|path| lexical_resource(path, None, true))
                    .transpose()
                    .ok()?,
                outcome: None,
            })
        }
        "update_plan" => Some(ToolAction::Plan {
            operation: PlanOp::Update,
            plan_id: None,
            title: None,
            summary: nested_property_string(program, call, &["explanation", "summary"])
                .map(canonical_text)
                .transpose()
                .ok()?,
            step_count: match nested_property_count(call, "step") {
                0 => None,
                count => Some(count as u64),
            },
        }),
        "web__run" => project_embedded_web(program, call),
        "view_image" => {
            let path = property_string(program, call, &["path", "image_path", "file_path"])?;
            Some(ToolAction::FileRead {
                targets: vec![ReadTarget {
                    resource: lexical_resource(&path, cwd, false).ok()?,
                    range: None,
                    media_kind: Some(AttachmentKind::Image),
                }],
                outcome: None,
            })
        }
        _ => None,
    }
}

fn project_embedded_shell(
    program: &str,
    call: &EmbeddedCall,
    cwd: Option<&str>,
) -> Result<ToolAction, ProjectionError> {
    let command = property_string(program, call, &["cmd", "command"])
        .ok_or(ProjectionError::MissingField("command"))?;
    Ok(ToolAction::Shell {
        command: canonical_text(command)?,
        source_truncated: false,
        cwd: property_string(program, call, &["workdir", "cwd"])
            .as_deref()
            .or(cwd)
            .map(|path| lexical_resource(path, cwd, true))
            .transpose()?,
        timeout_ms: property_u64(call, &["yield_time_ms", "timeout_ms"]),
        background: property_bool(call, &["background"]).unwrap_or(false),
        outcome: None,
    })
}

fn project_embedded_process(
    program: &str,
    call: &EmbeddedCall,
) -> Result<ToolAction, ProjectionError> {
    let process_id = property_string(program, call, &["session_id", "process_id", "cell_id"])
        .or_else(|| {
            property_u64(call, &["session_id", "process_id", "cell_id"])
                .map(|value| value.to_string())
        })
        .ok_or(ProjectionError::MissingField("process id"))?;
    let input =
        property_string(program, call, &["chars", "input"]).filter(|value| !value.is_empty());
    Ok(ToolAction::Process {
        operation: if input.is_some() {
            ProcessOp::Input
        } else {
            ProcessOp::Inspect
        },
        process_id,
        input: input.map(canonical_text).transpose()?,
        outcome: None,
    })
}

fn project_embedded_web(program: &str, call: &EmbeddedCall) -> Option<ToolAction> {
    if has_property(call, "search_query") || has_property(call, "image_query") {
        return Some(ToolAction::WebSearch {
            query: nested_property_string(program, call, &["q", "query"]),
            allowed_domains: vec![],
            blocked_domains: vec![],
            outcome: None,
        });
    }
    if has_property(call, "find") {
        let target = nested_property_string(program, call, &["url", "ref_id"]);
        let resource = target.as_deref().and_then(|value| url(value).ok());
        let fallback_target = target.filter(|_| resource.is_none());
        return Some(ToolAction::WebNavigate {
            operation: WebNavigationOp::FindInPage,
            url: resource,
            pattern: nested_property_string(program, call, &["pattern"]).or(fallback_target),
            outcome: None,
        });
    }
    if has_property(call, "open") || has_property(call, "click") {
        let target = nested_property_string(program, call, &["url", "ref_id"]);
        let resource = target.as_deref().and_then(|value| url(value).ok());
        let fallback_target = target.filter(|_| resource.is_none());
        return Some(ToolAction::WebNavigate {
            operation: WebNavigationOp::OpenPage,
            url: resource,
            pattern: fallback_target,
            outcome: None,
        });
    }
    None
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
        source_truncated: false,
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
    let process_input = first_string(input, &["chars", "input"])
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    Ok(ToolAction::Process {
        operation: if name == "write_stdin" {
            if process_input.is_some() {
                ProcessOp::Input
            } else {
                ProcessOp::Inspect
            }
        } else {
            ProcessOp::Wait
        },
        process_id,
        input: process_input.map(canonical_text).transpose()?,
        outcome: None,
    })
}

fn project_patch(input: &Value, cwd: Option<&str>) -> Result<ToolAction, ProjectionError> {
    let patch = first_string(input, &["patch", "input"])
        .or_else(|| input.as_str())
        .ok_or(ProjectionError::MissingField("patch"))?;
    let changes = patch_sections(patch)
        .into_iter()
        .map(|section| section.project(cwd))
        .collect::<Result<Vec<_>, _>>()?;
    if changes.is_empty() {
        return Err(ProjectionError::InvalidField("patch headers"));
    }
    Ok(ToolAction::FileChange { changes })
}

/// One file section in Codex's `apply_patch` dialect. The dialect is almost a
/// unified diff, but often omits hunk coordinates (`@@` rather than
/// `@@ -12,3 +12,4 @@`). Keep that honest source text and let the renderer show
/// blank gutters when coordinates were not persisted.
struct PatchSection<'a> {
    operation: FileChangeOp,
    path: &'a str,
    destination: Option<&'a str>,
    body: Vec<&'a str>,
}

impl PatchSection<'_> {
    fn project(self, cwd: Option<&str>) -> Result<FileDelta, ProjectionError> {
        let added = self
            .body
            .iter()
            .filter(|line| line.starts_with('+'))
            .count() as u64;
        let removed = self
            .body
            .iter()
            .filter(|line| line.starts_with('-'))
            .count() as u64;
        let carries_content = added > 0 || removed > 0;
        let (lines_added, lines_removed) = match self.operation {
            FileChangeOp::Create => (Some(added), None),
            FileChangeOp::Update | FileChangeOp::Move if carries_content => {
                (Some(added), Some(removed))
            }
            _ => (None, None),
        };

        let diff = self
            .diff_text(carries_content)
            .map(canonical_text)
            .transpose()?;
        Ok(FileDelta {
            operation: Some(self.operation),
            path: lexical_resource(self.path, cwd, false)?,
            destination: self
                .destination
                .map(|path| lexical_resource(path, cwd, false))
                .transpose()?,
            diff,
            lines_added,
            lines_removed,
        })
    }

    fn diff_text(&self, carries_content: bool) -> Option<String> {
        // A delete call does not include the old file's bytes, so its path and
        // operation are real evidence but a content diff is not. A pure move
        // does carry a useful structural diff even when no lines changed.
        if !carries_content
            && self.operation != FileChangeOp::Create
            && self.operation != FileChangeOp::Move
        {
            return None;
        }

        let destination = self.destination.unwrap_or(self.path);
        let old_path = if self.operation == FileChangeOp::Create {
            "/dev/null"
        } else {
            self.path
        };
        let mut lines = vec![format!("--- {old_path}"), format!("+++ {destination}")];
        if self.operation == FileChangeOp::Move {
            lines.insert(0, format!("rename to {destination}"));
            lines.insert(0, format!("rename from {}", self.path));
        }
        if !self.body.is_empty() {
            if !self.body.iter().any(|line| line.starts_with("@@")) {
                lines.push("@@".to_owned());
            }
            lines.extend(self.body.iter().map(|line| (*line).to_owned()));
        }
        Some(lines.join("\n"))
    }
}

fn patch_sections(patch: &str) -> Vec<PatchSection<'_>> {
    let mut sections = Vec::new();
    let mut open: Option<PatchSection<'_>> = None;
    for line in patch.lines() {
        if let Some((operation, path)) = patch_file_header(line) {
            if let Some(section) = open.take() {
                sections.push(section);
            }
            open = Some(PatchSection {
                operation,
                path: path.trim(),
                destination: None,
                body: Vec::new(),
            });
        } else if let Some(path) = line.strip_prefix("*** Move to: ") {
            if let Some(section) = open.as_mut() {
                section.operation = FileChangeOp::Move;
                section.destination = Some(path.trim());
            }
        } else if line == "*** End Patch" {
            if let Some(section) = open.take() {
                sections.push(section);
            }
        } else if line != "*** Begin Patch"
            && let Some(section) = open.as_mut()
        {
            section.body.push(line);
        }
    }
    if let Some(section) = open {
        sections.push(section);
    }
    sections
}

fn patch_file_header(line: &str) -> Option<(FileChangeOp, &str)> {
    [
        ("*** Add File: ", FileChangeOp::Create),
        ("*** Update File: ", FileChangeOp::Update),
        ("*** Delete File: ", FileChangeOp::Delete),
    ]
    .into_iter()
    .find_map(|(prefix, operation)| line.strip_prefix(prefix).map(|path| (operation, path)))
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
            // The native writer uses `queries` for a batched search. Retain
            // every recorded query even though no result sources are persisted.
            query: first_string(action, &["query", "q"])
                .filter(|query| !query.is_empty())
                .map(str::to_owned)
                .or_else(|| {
                    let queries = action.get("queries")?.as_array()?;
                    let queries = queries
                        .iter()
                        .map(Value::as_str)
                        .collect::<Option<Vec<_>>>()?;
                    (!queries.is_empty()).then(|| queries.join("\n"))
                }),
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
        team: None,
        color: None,
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
    fn empty_process_input_is_a_poll_not_a_write() {
        let poll = project_request(
            Some(FIXTURE_WRITER_VERSION),
            "write_stdin",
            &json!({"session_id": 42, "chars": ""}),
            None,
            "poll-1",
            Some("poll-1"),
            None,
        )
        .unwrap();
        assert!(matches!(
            poll.action,
            ToolAction::Process {
                operation: ProcessOp::Inspect,
                input: None,
                ..
            }
        ));

        let input = project_request(
            Some(FIXTURE_WRITER_VERSION),
            "write_stdin",
            &json!({"session_id": 42, "chars": "q"}),
            None,
            "input-1",
            Some("input-1"),
            None,
        )
        .unwrap();
        assert!(matches!(
            input.action,
            ToolAction::Process {
                operation: ProcessOp::Input,
                input: Some(_),
                ..
            }
        ));
    }

    #[test]
    fn executor_waits_and_empty_process_polls_are_transport() {
        assert_eq!(
            transport_call("wait", &json!({"cell_id":"1144"})),
            Some(TransportCall::WaitCell("1144".to_owned()))
        );
        assert_eq!(
            transport_call(
                "exec",
                &json!(
                    r#"const r = await tools.write_stdin({session_id: 79752, chars: ""}); text(r);"#
                )
            ),
            Some(TransportCall::PollProcesses(vec!["79752".to_owned()]))
        );
        assert_eq!(
            transport_call(
                "exec",
                &json!(
                    r#"await Promise.all([tools.write_stdin({session_id:1, chars:""}), tools.write_stdin({session_id:2, chars:""})]);"#
                )
            ),
            Some(TransportCall::PollProcesses(vec![
                "1".to_owned(),
                "2".to_owned()
            ]))
        );
        assert_eq!(
            transport_call(
                "exec",
                &json!(r#"await tools.write_stdin({session_id: 7, chars: "q"});"#)
            ),
            None,
            "real input remains a visible process action"
        );
    }

    /// The pack's two-release window (0.149.1..=0.150.1) left every rollout
    /// written by any other Codex build with `unknown` actions, even though
    /// 0.144.6 through 0.152.0 persist the same tool names. The 0.x line is the
    /// evidence boundary; a 1.x writer still fails closed.
    #[test]
    fn the_release_line_projects_and_the_next_line_fails_closed() {
        for version in [
            EARLIEST_WRITER_VERSION,
            FIXTURE_WRITER_VERSION,
            CURRENT_WRITER_VERSION,
            "0.152.0",
            WRITER_LINE_CEILING,
        ] {
            let tool = project_request(
                Some(version),
                "exec_command",
                &json!({"cmd":"echo in-line"}),
                None,
                "call-1",
                Some("call-1"),
                None,
            )
            .unwrap_or_else(|error| panic!("{version} must project: {error:?}"));
            assert!(matches!(tool.action, ToolAction::Shell { .. }), "{version}");
        }
        for version in ["0.100.0", "1.0.0"] {
            assert!(
                matches!(
                    project_request(
                        Some(version),
                        "exec_command",
                        &json!({"cmd":"echo out of line"}),
                        None,
                        "call-1",
                        Some("call-1"),
                        None,
                    ),
                    Err(ProjectionError::Unregistered { .. })
                ),
                "{version} must fail closed",
            );
        }
    }

    #[test]
    fn exec_projects_one_explicit_child_and_keeps_real_batches() {
        let tool = project_request(
            Some(CURRENT_WRITER_VERSION),
            "exec",
            &json!("await tools.exec_command({cmd: 'git status'})"),
            None,
            "exec-1",
            Some("exec-1"),
            None,
        )
        .unwrap();
        let ToolAction::Shell { command, .. } = tool.action else {
            panic!("a one-command envelope must project as shell")
        };
        assert_eq!(command.as_preview(), "git status");

        let batch = project_request(
            Some(CURRENT_WRITER_VERSION),
            "exec",
            &json!(
                r#"const r = await Promise.all([
                tools.exec_command({cmd:"cargo test"}),
                tools.exec_command({cmd:"npm test"})
            ]); text(r);"#
            ),
            None,
            "exec-2",
            Some("exec-2"),
            None,
        )
        .unwrap();
        assert_eq!(
            batch.action,
            ToolAction::Orchestration {
                runtime: Some("functions.exec".to_owned()),
                trace: ChildTrace::Partial,
                executed_children: vec!["exec_command".to_owned(), "exec_command".to_owned()],
                executed_child_count: None
            }
        );

        let children = project_exec_batch(
            &json!(
                r#"const r = await Promise.all([
                tools.exec_command({cmd:"cargo test"}),
                tools.exec_command({cmd:"npm test"})
            ]); text(r);"#
            ),
            Some("/work"),
            "exec-2",
        )
        .expect("literal batch children");
        assert_eq!(children.len(), 2);
        assert!(matches!(children[0].action, ToolAction::Shell { .. }));
        assert!(matches!(children[1].action, ToolAction::Shell { .. }));

        let opaque = project_request(
            Some(CURRENT_WRITER_VERSION),
            "exec",
            &json!("text('no child trace')"),
            None,
            "exec-3",
            Some("exec-3"),
            None,
        )
        .unwrap();
        assert!(matches!(
            opaque.action,
            ToolAction::Orchestration {
                trace: ChildTrace::Unavailable,
                executed_child_count: None,
                ..
            }
        ));
    }

    #[test]
    fn exec_patch_preserves_per_file_diffs_counts_and_moves() {
        let tool = project_request(
            Some(CURRENT_WRITER_VERSION),
            "exec",
            &json!(
                r#"const patch = "*** Begin Patch\n*** Update File: src/main.rs\n@@\n-old\n+new\n+more\n*** Add File: src/new.rs\n+hello\n*** Update File: src/from.rs\n*** Move to: src/to.rs\n@@ -1 +1 @@\n-before\n+after\n*** Delete File: src/gone.rs\n*** End Patch"; text(await tools.apply_patch(patch));"#
            ),
            Some("/work"),
            "exec-patch",
            Some("exec-patch"),
            None,
        )
        .unwrap();
        let ToolAction::FileChange { changes } = tool.action else {
            panic!("an apply_patch envelope must project as file_change")
        };
        assert_eq!(changes.len(), 4);
        assert_eq!(changes[0].operation, Some(FileChangeOp::Update));
        assert_eq!(
            (changes[0].lines_added, changes[0].lines_removed),
            (Some(2), Some(1))
        );
        assert_eq!(
            changes[0].diff.as_ref().map(|diff| diff.as_preview()),
            Some("--- src/main.rs\n+++ src/main.rs\n@@\n-old\n+new\n+more")
        );
        assert_eq!(changes[1].operation, Some(FileChangeOp::Create));
        assert_eq!(
            (changes[1].lines_added, changes[1].lines_removed),
            (Some(1), None)
        );
        assert_eq!(changes[2].operation, Some(FileChangeOp::Move));
        assert!(matches!(
            changes[2].destination.as_ref(),
            Some(rosetta_core::timeline::ResourceRef::File { path, .. }) if path == "/work/src/to.rs"
        ));
        assert_eq!(
            (changes[2].lines_added, changes[2].lines_removed),
            (Some(1), Some(1))
        );
        assert!(changes[2].diff.as_ref().is_some_and(|diff| {
            diff.as_preview()
                .contains("rename from src/from.rs\nrename to src/to.rs")
        }));
        assert_eq!(changes[3].operation, Some(FileChangeOp::Delete));
        assert!(changes[3].diff.is_none());
        assert_eq!(
            (changes[3].lines_added, changes[3].lines_removed),
            (None, None)
        );
    }

    #[test]
    fn web_search_retains_native_batch_queries_without_inventing_sources() {
        let action = project_web(&json!({
            "type": "search", "query": null, "queries": ["webgpu vertex pulling", "wgsl instance index"]
        }))
        .unwrap();
        assert!(
            matches!(action, ToolAction::WebSearch { query: Some(query), outcome: None, .. }
                if query == "webgpu vertex pulling\nwgsl instance index")
        );
        let action = project_web(&json!({"type":"search","queries":[]})).unwrap();
        assert!(matches!(
            action,
            ToolAction::WebSearch {
                query: None,
                outcome: None,
                ..
            }
        ));
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
