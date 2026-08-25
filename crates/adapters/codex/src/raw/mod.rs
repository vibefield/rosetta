//! Versioned raw schemas — the parseable ground truth of what Codex writes.
//! Every field here is backed by the committed fixture corpus (see
//! `fixtures/<version>/`); do not add fields without fixture evidence.

pub mod rollout;

pub use rollout::{ParsedLine, parse_line};
