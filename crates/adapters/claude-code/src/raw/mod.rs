//! Versioned raw schemas — the parseable ground truth of what Claude Code writes.
//! Every field here is backed by the committed fixture corpus (see
//! `fixtures/<version>/`); do not add fields without fixture evidence.

pub mod transcript;

pub use transcript::{ParsedLine, parse_line};
