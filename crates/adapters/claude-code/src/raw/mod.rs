//! Versioned raw schemas — the parseable ground truth of what Claude Code writes.
//! Every field here is backed by the canonical ground truth: the committed
//! fixture corpus (`fixtures/<version>/`) or, for this closed writer, a
//! committed structure census (`Evidence::Observed`); do not add fields
//! without that evidence.

pub mod cost_state;
pub mod subagent_meta;
pub mod transcript;

pub use subagent_meta::{SubagentMeta, parse as parse_subagent_meta};
pub use transcript::{KnownRecord, ParsedLine, parse_line};
