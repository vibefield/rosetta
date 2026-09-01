//! Versioned raw schemas — the parseable ground truth of what Claude Code writes.
//! Every field here is backed by the committed fixture corpus (see
//! `fixtures/<version>/`); do not add fields without fixture evidence.

pub mod subagent_meta;
pub mod transcript;

pub use subagent_meta::{SubagentMeta, parse as parse_subagent_meta};
pub use transcript::{KnownRecord, ParsedLine, parse_line};
