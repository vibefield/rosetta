//! Raw types for the `transcript` stream
//! (`brain/<id>/.system_generated/logs/transcript.jsonl` and the full/chunk siblings).
//!
//! Evidence: THE CANONICAL `groundtruth.json` — fixtures/1.1.20 merged with the
//! structure census of the organic CLI tree. Fixture types: USER_INPUT,
//! PLANNER_RESPONSE, RUN_COMMAND, CHECKPOINT, SYSTEM_MESSAGE. Census-only
//! types also typed below: CONVERSATION_HISTORY, LIST_DIRECTORY, VIEW_FILE,
//! ERROR_MESSAGE. Field optionality mirrors the canonical file.
//!
//! Parsing policy (CLAUDE.md): permissive on fields — flatten-unknown maps
//! everywhere, non-empty = drift. `tool_calls[].args` stays `Value` (do not
//! narrow by tool). A known record type that fails to parse is a hard error.

use rosetta_adapter_support::{Drift, Unknown, note_unknown};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Outcome of parsing one transcript line.
#[derive(Debug)]
pub enum ParsedLine {
    Known(Box<KnownRecord>),
    /// A record `type` this schema version does not know — soft drift.
    UnknownType {
        rtype: Option<String>,
        value: Value,
    },
}

/// Parse one line of a transcript JSONL file.
pub fn parse_line(line: &str) -> Result<ParsedLine, serde_json::Error> {
    let value: Value = serde_json::from_str(line)?;
    let rtype = value["type"].as_str().map(str::to_owned);
    let known = matches!(
        rtype.as_deref(),
        Some(
            "USER_INPUT"
                | "PLANNER_RESPONSE"
                | "RUN_COMMAND"
                | "CHECKPOINT"
                | "SYSTEM_MESSAGE"
                | "CONVERSATION_HISTORY"
                | "LIST_DIRECTORY"
                | "VIEW_FILE"
                | "ERROR_MESSAGE"
                | "GENERIC"
        )
    );
    if known {
        Ok(ParsedLine::Known(Box::new(serde_json::from_value(value)?)))
    } else {
        Ok(ParsedLine::UnknownType { rtype, value })
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type")]
#[allow(clippy::large_enum_variant)]
pub enum KnownRecord {
    #[serde(rename = "USER_INPUT")]
    UserInput(UserInputLine),
    #[serde(rename = "PLANNER_RESPONSE")]
    PlannerResponse(PlannerResponseLine),
    #[serde(rename = "RUN_COMMAND")]
    RunCommand(RunCommandLine),
    #[serde(rename = "CHECKPOINT")]
    Checkpoint(CheckpointLine),
    #[serde(rename = "SYSTEM_MESSAGE")]
    SystemMessage(SystemMessageLine),
    #[serde(rename = "CONVERSATION_HISTORY")]
    ConversationHistory(ConversationHistoryLine),
    #[serde(rename = "LIST_DIRECTORY")]
    ListDirectory(ListDirectoryLine),
    #[serde(rename = "VIEW_FILE")]
    ViewFile(ViewFileLine),
    #[serde(rename = "ERROR_MESSAGE")]
    ErrorMessage(ErrorMessageLine),
    #[serde(rename = "GENERIC")]
    Generic(GenericLine),
}

impl Drift for KnownRecord {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        match self {
            KnownRecord::UserInput(line) => note_unknown(&line.unknown, prefix, out),
            KnownRecord::PlannerResponse(line) => line.drift(prefix, out),
            KnownRecord::RunCommand(line) => note_unknown(&line.unknown, prefix, out),
            KnownRecord::Checkpoint(line) => note_unknown(&line.unknown, prefix, out),
            KnownRecord::SystemMessage(line) => note_unknown(&line.unknown, prefix, out),
            KnownRecord::ConversationHistory(line) => note_unknown(&line.unknown, prefix, out),
            KnownRecord::ListDirectory(line) => note_unknown(&line.unknown, prefix, out),
            KnownRecord::ViewFile(line) => note_unknown(&line.unknown, prefix, out),
            KnownRecord::ErrorMessage(line) => note_unknown(&line.unknown, prefix, out),
            KnownRecord::Generic(line) => note_unknown(&line.unknown, prefix, out),
        }
    }
}

/// User prompt turn.
#[derive(Debug, Deserialize, Serialize)]
pub struct UserInputLine {
    pub step_index: u64,
    pub source: String,
    pub status: String,
    pub created_at: String,
    pub content: String,
    #[serde(flatten)]
    pub unknown: Unknown,
}

/// Model planner/assistant turn. `content` is absent when the turn is a tool call.
#[derive(Debug, Deserialize, Serialize)]
pub struct PlannerResponseLine {
    pub step_index: u64,
    pub source: String,
    pub status: String,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl Drift for PlannerResponseLine {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        note_unknown(&self.unknown, prefix, out);
        if let Some(calls) = &self.tool_calls {
            for (index, call) in calls.iter().enumerate() {
                note_unknown(&call.unknown, &format!("{prefix}.tool_calls[{index}]"), out);
            }
        }
    }
}

/// One planner-requested tool. `args` is deliberately untyped.
#[derive(Debug, Deserialize, Serialize)]
pub struct ToolCall {
    pub name: String,
    pub args: Value,
    #[serde(flatten)]
    pub unknown: Unknown,
}

/// Shell tool result (`run_command`). `content` is the tool output string.
#[derive(Debug, Deserialize, Serialize)]
pub struct RunCommandLine {
    pub step_index: u64,
    pub source: String,
    pub status: String,
    pub created_at: String,
    pub exit_code: i64,
    pub content: String,
    #[serde(flatten)]
    pub unknown: Unknown,
}

/// System checkpoint blob.
#[derive(Debug, Deserialize, Serialize)]
pub struct CheckpointLine {
    pub step_index: u64,
    pub source: String,
    pub status: String,
    pub created_at: String,
    pub content: String,
    #[serde(flatten)]
    pub unknown: Unknown,
}

/// Native checkpoints may carry a generated session-list fallback under this
/// exact section. Keep the extraction narrow: other headings contain user
/// requests, logs, or instructions and are not titles.
pub(crate) const OBJECTIVE_TITLE_CHARS: usize = 80;

pub(crate) fn checkpoint_objective(content: &str) -> Option<String> {
    let mut in_objective = false;
    let mut title = String::new();
    let mut title_chars = 0_usize;
    let mut truncated = false;

    'section: for raw_line in content.lines() {
        let line = raw_line.trim();
        if !in_objective {
            if line == "# USER Objective:" {
                in_objective = true;
            }
            continue;
        }
        if is_markdown_heading(line) {
            break;
        }
        for word in line.split_whitespace() {
            if !title.is_empty() {
                if title_chars == OBJECTIVE_TITLE_CHARS {
                    truncated = true;
                    break 'section;
                }
                title.push(' ');
                title_chars += 1;
            }
            for character in word.chars() {
                if title_chars == OBJECTIVE_TITLE_CHARS {
                    truncated = true;
                    break 'section;
                }
                title.push(character);
                title_chars += 1;
            }
        }
    }

    if title.is_empty() {
        return None;
    }
    if truncated {
        while title.chars().count() >= OBJECTIVE_TITLE_CHARS {
            title.pop();
        }
        while title.ends_with(char::is_whitespace) {
            title.pop();
        }
        title.push('…');
    }
    Some(title)
}

fn is_markdown_heading(line: &str) -> bool {
    let rest = line.trim_start_matches('#');
    rest.len() != line.len() && (rest.is_empty() || rest.starts_with(char::is_whitespace))
}

/// Injected system notice (resume fixture: subagents stopped).
#[derive(Debug, Deserialize, Serialize)]
pub struct SystemMessageLine {
    pub step_index: u64,
    pub source: String,
    pub status: String,
    pub created_at: String,
    pub content: String,
    #[serde(flatten)]
    pub unknown: Unknown,
}

/// Injected prior-conversation summary. Census-only so far: no `content`.
#[derive(Debug, Deserialize, Serialize)]
pub struct ConversationHistoryLine {
    pub step_index: u64,
    pub source: String,
    pub status: String,
    pub created_at: String,
    #[serde(flatten)]
    pub unknown: Unknown,
}

/// `list_dir` tool result. Census-only so far.
#[derive(Debug, Deserialize, Serialize)]
pub struct ListDirectoryLine {
    pub step_index: u64,
    pub source: String,
    pub status: String,
    pub created_at: String,
    pub content: String,
    #[serde(flatten)]
    pub unknown: Unknown,
}

/// `view_file` tool result. Census-only so far.
#[derive(Debug, Deserialize, Serialize)]
pub struct ViewFileLine {
    pub step_index: u64,
    pub source: String,
    pub status: String,
    pub created_at: String,
    pub content: String,
    #[serde(flatten)]
    pub unknown: Unknown,
}

/// Tool/runtime error line. Census-only so far.
#[derive(Debug, Deserialize, Serialize)]
pub struct ErrorMessageLine {
    pub step_index: u64,
    pub source: String,
    pub status: String,
    pub created_at: String,
    pub error: String,
    pub content: String,
    #[serde(flatten)]
    pub unknown: Unknown,
}

/// Generic model prose emitted by newer organic Antigravity writers. The
/// structure-only census observes `source=MODEL`; keep the envelope open so a
/// future source/status addition is drift rather than a parse outage.
#[derive(Debug, Deserialize, Serialize)]
pub struct GenericLine {
    pub step_index: u64,
    pub source: String,
    pub status: String,
    pub created_at: String,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncated_fields: Option<Vec<String>>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[cfg(test)]
mod tests {
    use super::{KnownRecord, OBJECTIVE_TITLE_CHARS, ParsedLine, checkpoint_objective, parse_line};

    #[test]
    fn census_only_types_parse() {
        let lines = [
            r#"{"step_index":1,"source":"SYSTEM","type":"CONVERSATION_HISTORY","status":"DONE","created_at":"2026-08-12T21:44:01Z"}"#,
            r#"{"step_index":3,"source":"MODEL","type":"LIST_DIRECTORY","status":"DONE","created_at":"2026-08-12T21:44:01Z","content":"."}"#,
            r#"{"step_index":14,"source":"MODEL","type":"VIEW_FILE","status":"DONE","created_at":"2026-08-12T21:44:01Z","content":"ok"}"#,
            r#"{"step_index":8,"source":"MODEL","type":"ERROR_MESSAGE","status":"DONE","error":"denied","created_at":"2026-08-12T21:44:01Z","content":"denied"}"#,
            r#"{"step_index":9,"source":"MODEL","type":"GENERIC","status":"DONE","created_at":"2026-08-12T21:44:01Z","content":"finished","truncated_fields":["content"]}"#,
        ];
        for line in lines {
            match parse_line(line).unwrap() {
                ParsedLine::Known(_) => {}
                ParsedLine::UnknownType { rtype, .. } => {
                    panic!("expected known census type, got {rtype:?}")
                }
            }
        }
    }

    #[test]
    fn objective_extraction_is_section_aware_normalized_and_unicode_bounded() {
        assert_eq!(
            checkpoint_objective(
                "prefix\n# USER Objective:\n  Review   the codebase\ncarefully \n# User Requests\nignore me"
            )
            .as_deref(),
            Some("Review the codebase carefully")
        );
        assert_eq!(
            checkpoint_objective("# User Requests\nnot an objective"),
            None
        );
        assert_eq!(checkpoint_objective("# USER Objective:\n\n# Next"), None);
        assert_eq!(
            checkpoint_objective("## USER Objective:\nwrong heading"),
            None
        );

        let long = format!(
            "# USER Objective:\n{}",
            "界".repeat(OBJECTIVE_TITLE_CHARS + 5)
        );
        let title = checkpoint_objective(&long).unwrap();
        assert_eq!(title.chars().count(), OBJECTIVE_TITLE_CHARS);
        assert!(title.ends_with('…'));
    }

    #[test]
    fn conversation_history_round_trips_without_content() {
        let line = r#"{"step_index":1,"source":"SYSTEM","type":"CONVERSATION_HISTORY","status":"DONE","created_at":"2026-08-12T21:44:01Z"}"#;
        let ParsedLine::Known(record) = parse_line(line).unwrap() else {
            panic!("expected known record");
        };
        assert!(matches!(
            record.as_ref(),
            KnownRecord::ConversationHistory(_)
        ));
        let back = serde_json::to_value(&*record).unwrap();
        let original: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(back, original);
    }
}
