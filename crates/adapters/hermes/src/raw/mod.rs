//! Versioned raw schemas — the parseable ground truth of what Hermes writes.
//! Every field here is backed by the committed fixture corpus (see
//! `fixtures/<version>/`); do not add fields without fixture evidence.

pub mod state;

pub use state::{KnownRecord, ParsedRow, parse_row};
