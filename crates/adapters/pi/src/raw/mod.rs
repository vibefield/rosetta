//! Versioned raw schemas — the parseable ground truth of what Pi writes.
//! Every field here is backed by the committed fixture corpus (see
//! `fixtures/<version>/`); do not add fields without fixture evidence.

pub mod session;

pub use session::{ParsedLine, parse_line};
