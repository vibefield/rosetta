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

#[cfg(test)]
mod tests {
    use super::{KnownRecord, ParsedLine, parse_line};

    #[test]
    fn census_only_types_parse() {
        let lines = [
            r#"{"step_index":1,"source":"SYSTEM","type":"CONVERSATION_HISTORY","status":"DONE","created_at":"2026-08-12T21:44:01Z"}"#,
            r#"{"step_index":3,"source":"MODEL","type":"LIST_DIRECTORY","status":"DONE","created_at":"2026-08-12T21:44:01Z","content":"."}"#,
            r#"{"step_index":14,"source":"MODEL","type":"VIEW_FILE","status":"DONE","created_at":"2026-08-12T21:44:01Z","content":"ok"}"#,
            r#"{"step_index":8,"source":"MODEL","type":"ERROR_MESSAGE","status":"DONE","error":"denied","created_at":"2026-08-12T21:44:01Z","content":"denied"}"#,
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
