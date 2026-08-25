//! Versioned raw schemas — the parseable ground truth of what OpenCode writes.
//! Every field here is backed by the committed fixture corpus (see
//! `fixtures/<version>/`); do not add fields without fixture evidence.

pub mod event;
pub mod payload;
pub mod rows;

pub use event::{EventData, ParsedEvent, parse_event};
pub use payload::{MessageData, ParsedMessage, ParsedPart, PartData, parse_message, parse_part};
pub use rows::{
    ParsedProject, ParsedSession, ProjectRow, SessionRow, parse_project, parse_session,
};
