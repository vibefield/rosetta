//! Raw types for the `transcript` stream (`projects/<slug>/<session>.jsonl`).
//!
//! Evidence: THE CANONICAL `groundtruth.json` (iron rule #6) — fixtures at
//! 2.1.241 merged with the structure census of 1.2M organic records across
//! ~40 writer versions. Field optionality mirrors the canonical file, not the
//! fixtures alone: a field is required here only if every observed record
//! carries it.
//!
//! Record types typed below: user, assistant, system, ai-title, atis-latch,
//! queue-operation, last-prompt, mode. The remaining canonical types
//! (attachment, permission-mode, bridge-session, file-history-*) parse as
//! `ParsedLine::UnknownType` (soft) until typed — the adapter treats a type the
//! canonical declares as handled-but-unprojected, never as drift.
//!
//! Parsing policy (CLAUDE.md): permissive on fields — every struct carries a
//! `#[serde(flatten)] unknown` map and a non-empty map is a drift observation.
//! Content blocks use an untagged Known/Other split so an unfamiliar block
//! kind degrades to a soft `Other(Value)` (drift) instead of failing the line
//! (spaghetti pitfall §8.4). A KNOWN record type that fails to parse is still
//! a hard error — that is the alarm for removed/retyped fields.

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
            "user"
                | "assistant"
                | "system"
                | "ai-title"
                | "atis-latch"
                | "queue-operation"
                | "last-prompt"
                | "mode"
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
// Variant sizes intentionally differ (user/assistant lines are big); records
// are boxed once at the ParsedLine level, which is enough.
#[allow(clippy::large_enum_variant)]
pub enum KnownRecord {
    #[serde(rename = "user")]
    User(UserLine),
    #[serde(rename = "assistant")]
    Assistant(AssistantLine),
    #[serde(rename = "system")]
    System(SystemLine),
    #[serde(rename = "ai-title")]
    AiTitle(AiTitleLine),
    #[serde(rename = "atis-latch")]
    AtisLatch(AtisLatchLine),
    #[serde(rename = "queue-operation")]
    QueueOperation(QueueOperationLine),
    #[serde(rename = "last-prompt")]
    LastPrompt(LastPromptLine),
    #[serde(rename = "mode")]
    Mode(ModeLine),
}

impl Drift for KnownRecord {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        match self {
            KnownRecord::User(l) => l.drift(prefix, out),
            KnownRecord::Assistant(l) => l.drift(prefix, out),
            KnownRecord::System(l) => note_unknown(&l.unknown, prefix, out),
            KnownRecord::AiTitle(l) => note_unknown(&l.unknown, prefix, out),
            KnownRecord::AtisLatch(l) => note_unknown(&l.unknown, prefix, out),
            KnownRecord::QueueOperation(l) => note_unknown(&l.unknown, prefix, out),
            KnownRecord::LastPrompt(l) => note_unknown(&l.unknown, prefix, out),
            KnownRecord::Mode(l) => note_unknown(&l.unknown, prefix, out),
        }
    }
}

/// A user turn: a typed prompt or a tool_result report-back.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UserLine {
    // Required across all 88k observed records (canonical).
    /// Explicit null on session-opening records — nullable, never absent.
    pub parent_uuid: Option<String>,
    pub is_sidechain: bool,
    pub message: UserMessage,
    pub uuid: String,
    pub timestamp: String,
    pub session_id: String,
    /// Writing CLI version — per-record version stamp.
    pub version: String,
    pub cwd: String,
    pub git_branch: String,
    pub user_type: String,
    // Optional-by-absence (canonical: present on a subset of records).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entrypoint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_source: Option<String>,
    #[serde(
        default,
        rename = "sourceToolAssistantUUID",
        skip_serializing_if = "Option::is_none"
    )]
    pub source_tool_assistant_uuid: Option<String>,
    #[serde(
        default,
        rename = "sourceToolUseID",
        skip_serializing_if = "Option::is_none"
    )]
    pub source_tool_use_id: Option<String>,
    /// Tool-dependent result payload (object | string | array per canonical).
    /// Deliberately untyped — narrowing this per tool is a known production
    /// bug (`docs/groundtruth/claude-code.md` §3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_use_result: Option<Value>,
    /// snake_case duplicate of sessionId that some versions emit.
    #[serde(
        default,
        rename = "session_id",
        skip_serializing_if = "Option::is_none"
    )]
    pub session_id_snake: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_meta: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_compact_summary: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_visible_in_transcript_only: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_origin: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_priority: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub todos: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub classifier_meta_lines: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interrupted_message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_paste_ids: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_denial_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_companion: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_feedback: Option<String>,
    /// Subagent transcripts stamp the agent id on every line (probe: 351k).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    /// Rare marker on tool_result user lines (probe: 575).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_ends_turn: Option<bool>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl Drift for UserLine {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        note_unknown(&self.unknown, prefix, out);
        self.message.drift(&format!("{prefix}.message"), out);
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct UserMessage {
    pub role: String,
    pub content: UserContent,
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl Drift for UserMessage {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        note_unknown(&self.unknown, prefix, out);
        if let UserContent::Blocks(blocks) = &self.content {
            for (i, b) in blocks.iter().enumerate() {
                b.drift(&format!("{prefix}.content[{i}]"), out);
            }
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(untagged)]
pub enum UserContent {
    Text(String),
    Blocks(Vec<UserContentBlock>),
}

/// Untagged Known/Other split: an unfamiliar block kind lands in `Other`
/// (soft drift) instead of failing the whole line.
#[derive(Debug, Deserialize, Serialize)]
#[serde(untagged)]
pub enum UserContentBlock {
    Known(KnownUserBlock),
    Other(Value),
}

impl Drift for UserContentBlock {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        match self {
            UserContentBlock::Known(KnownUserBlock::ToolResult { unknown, .. })
            | UserContentBlock::Known(KnownUserBlock::Text { unknown, .. })
            | UserContentBlock::Known(KnownUserBlock::Image { unknown, .. })
            | UserContentBlock::Known(KnownUserBlock::Document { unknown, .. }) => {
                note_unknown(unknown, prefix, out)
            }
            UserContentBlock::Other(v) => {
                out.push(format!(
                    "{prefix}<unknown block type={}>",
                    v["type"].as_str().unwrap_or("?")
                ));
            }
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type")]
pub enum KnownUserBlock {
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        /// String or block array — kept raw.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content: Option<Value>,
        /// Absent on over half of observed tool_results (canonical).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        is_error: Option<bool>,
        #[serde(flatten)]
        unknown: Unknown,
    },
    #[serde(rename = "text")]
    Text {
        text: String,
        #[serde(flatten)]
        unknown: Unknown,
    },
    #[serde(rename = "image")]
    Image {
        source: Value,
        #[serde(flatten)]
        unknown: Unknown,
    },
    #[serde(rename = "document")]
    Document {
        source: Value,
        #[serde(flatten)]
        unknown: Unknown,
    },
}

/// An assistant turn wrapping the (augmented) API message.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantLine {
    // Required across all 155k observed records (canonical).
    /// Explicit null on subagent session-opening records (probe-proven).
    pub parent_uuid: Option<String>,
    pub is_sidechain: bool,
    pub message: AssistantMessage,
    pub uuid: String,
    pub timestamp: String,
    pub session_id: String,
    pub version: String,
    pub cwd: String,
    pub git_branch: String,
    pub user_type: String,
    // Optional-by-absence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entrypoint: Option<String>,
    /// Absent on 94k of 155k records — older writers don't stamp it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(
        default,
        rename = "session_id",
        skip_serializing_if = "Option::is_none"
    )]
    pub session_id_snake: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_details: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_error_status: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_api_error_message: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attribution_mcp_server: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attribution_mcp_tool: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attribution_skill: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attribution_plugin: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quota_limits: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersedes_uuids: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_name: Option<String>,
    /// Subagent transcripts stamp the agent id on every line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attribution_agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_error: Option<String>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl Drift for AssistantLine {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        note_unknown(&self.unknown, prefix, out);
        self.message.drift(&format!("{prefix}.message"), out);
    }
}

/// API-shaped message body (snake_case fields, per the Anthropic wire format).
#[derive(Debug, Deserialize, Serialize)]
pub struct AssistantMessage {
    pub id: String,
    #[serde(rename = "type")]
    pub message_type: String,
    pub role: String,
    pub model: String,
    pub content: Vec<AssistantContentBlock>,
    /// Present on every record, null while streaming was aborted etc.
    pub stop_reason: Option<String>,
    pub stop_sequence: Option<Value>,
    /// Claude Code addition; explicit null in fixtures, absent pre-2.1.x.
    #[serde(default)]
    pub stop_details: Option<Value>,
    /// Absent on API-error synthetic messages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<RecordedUsage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_management: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<Value>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl Drift for AssistantMessage {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        note_unknown(&self.unknown, prefix, out);
        if let Some(usage) = &self.usage {
            note_unknown(&usage.unknown, &format!("{prefix}.usage"), out);
        }
        for (i, b) in self.content.iter().enumerate() {
            b.drift(&format!("{prefix}.content[{i}]"), out);
        }
    }
}

/// Untagged Known/Other split — see UserContentBlock. `Other` absorbs block
/// kinds without fixture/census evidence (e.g. redacted_thinking) as soft
/// drift.
#[derive(Debug, Deserialize, Serialize)]
#[serde(untagged)]
pub enum AssistantContentBlock {
    Known(KnownAssistantBlock),
    Other(Value),
}

impl Drift for AssistantContentBlock {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        match self {
            AssistantContentBlock::Known(
                KnownAssistantBlock::Text { unknown, .. }
                | KnownAssistantBlock::ToolUse { unknown, .. }
                | KnownAssistantBlock::Thinking { unknown, .. }
                | KnownAssistantBlock::Fallback { unknown, .. },
            ) => note_unknown(unknown, prefix, out),
            AssistantContentBlock::Other(v) => {
                out.push(format!(
                    "{prefix}<unknown block type={}>",
                    v["type"].as_str().unwrap_or("?")
                ));
            }
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type")]
pub enum KnownAssistantBlock {
    #[serde(rename = "text")]
    Text {
        text: String,
        #[serde(flatten)]
        unknown: Unknown,
    },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: Value,
        /// Provenance of the call (probe: 195k in subagent streams).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        caller: Option<Value>,
        #[serde(flatten)]
        unknown: Unknown,
    },
    #[serde(rename = "thinking")]
    Thinking {
        thinking: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
        #[serde(flatten)]
        unknown: Unknown,
    },
    /// Automatic model transition inside a single response.
    #[serde(rename = "fallback")]
    Fallback {
        from: Value,
        to: Value,
        #[serde(flatten)]
        unknown: Unknown,
    },
}

/// Usage as Claude Code RECORDS it — a superset of the API response (the CLI
/// synthesizes inference_geo, iterations, service_tier, speed, … before
/// writing; proven by fixtures where the mock sent only four counters).
#[derive(Debug, Deserialize, Serialize)]
pub struct RecordedUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub cache_creation: Value,
    /// Nullable, never absent (canonical).
    pub inference_geo: Option<String>,
    pub service_tier: Option<String>,
    // Absent on older writer versions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iterations: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens_details: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_tool_use: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed: Option<String>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

/// A lifecycle marker the CLI writes for itself: compaction boundaries, hook
/// summaries, turn timings, slash-command echoes. `subtype` is the real
/// discriminator — the payload fields below are per-subtype, so all of them are
/// optional even where one subtype always carries them (canonical facet
/// `system-subtypes`: away_summary, bridge_status, compact_boundary,
/// local_command, stop_hook_summary, turn_duration).
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemLine {
    // Required across all 210 observed records (canonical).
    pub parent_uuid: Option<String>,
    pub is_sidechain: bool,
    pub subtype: String,
    pub uuid: String,
    pub timestamp: String,
    pub session_id: String,
    pub version: String,
    pub cwd: String,
    pub git_branch: String,
    pub user_type: String,
    // Optional-by-absence, including per-subtype payload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entrypoint: Option<String>,
    /// Display text; absent on the purely numeric subtypes (turn_duration).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_meta: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    #[serde(
        default,
        rename = "session_id",
        skip_serializing_if = "Option::is_none"
    )]
    pub session_id_snake: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compact_metadata: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_count: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub has_output: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hook_count: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hook_errors: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hook_infos: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hook_additional_context: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logical_parent_uuid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_background_agent_count: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prevented_continuation: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
    /// Note the trailing `ID` — not the camelCase this record otherwise uses.
    #[serde(default, rename = "toolUseID", skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

/// The model-generated session title. Carries no uuid or timestamp — it is a
/// session-level singleton the CLI rewrites, not a point on the timeline.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiTitleLine {
    pub ai_title: String,
    pub session_id: String,
    #[serde(flatten)]
    pub unknown: Unknown,
}

/// Session-scoped marker record (opaque anti-tamper token).
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AtisLatchLine {
    pub atis: String,
    pub session_id: String,
    #[serde(flatten)]
    pub unknown: Unknown,
}

/// Prompt-queue bookkeeping (enqueue/dequeue/popAll/remove).
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueOperationLine {
    pub operation: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    pub timestamp: String,
    pub session_id: String,
    #[serde(flatten)]
    pub unknown: Unknown,
}

/// Records the latest prompt + leaf uuid for resume. Both payload fields are
/// absent on a subset of organic records (canonical).
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LastPromptLine {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leaf_uuid: Option<String>,
    pub session_id: String,
    #[serde(flatten)]
    pub unknown: Unknown,
}

/// Permission-mode change marker.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModeLine {
    pub mode: String,
    pub session_id: String,
    #[serde(flatten)]
    pub unknown: Unknown,
}
