//! Versioned raw schemas — the parseable ground truth of what Kimi Code writes.
//! Every field here is backed by the committed fixture corpus (see
//! `fixtures/<version>/`); do not add fields without fixture evidence.
//! Optionality also reflects organic census merge into the canonical JSON.

pub mod session_index;
pub mod session_state;
pub mod wire;
pub mod workspaces;

pub use session_index::{SessionIndexLine, parse_line as parse_session_index_line};
pub use session_state::{SessionState, parse_document as parse_session_state};
pub use wire::{ParsedLine, parse_line as parse_wire_line};
pub use workspaces::{WorkspacesFile, parse_document as parse_workspaces};
