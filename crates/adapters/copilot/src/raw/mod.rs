//! Versioned raw schemas — the parseable ground truth of what Copilot CLI writes.
//! Every field here is backed by the committed fixture corpus (see
//! `fixtures/<version>/`); do not add fields without fixture evidence.

pub mod events;
pub mod store;

pub use events::{ParsedLine, parse_line};
