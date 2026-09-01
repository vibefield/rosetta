//! Versioned raw schemas — the parseable ground truth of what Grok writes.
//! Fields come from `groundtruth/grok/groundtruth.json` (fixtures/1.0.5 merged
//! with the 2026-08-25 organic census). Do not add fields without that evidence.

pub mod chat_history;
pub mod events;
pub mod sidecars;
pub mod subagent;
pub mod summary;
pub mod updates;

pub use chat_history::{ParsedLine as ChatHistoryLine, parse_line as parse_chat_history_line};
pub use events::{ParsedLine as EventLine, parse_line as parse_event_line};
pub use sidecars::{
    PromptHistoryEntry, RewindPoint, Signals, parse_prompt_history, parse_rewind_point,
    parse_signals,
};
pub use subagent::{SubagentMeta, parse as parse_subagent_meta};
pub use summary::{Summary, parse as parse_summary};
pub use updates::{ParsedLine as UpdateLine, parse_line as parse_update_line};
