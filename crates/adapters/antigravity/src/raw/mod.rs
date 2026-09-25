//! Versioned raw schemas — the parseable ground truth of what Antigravity CLI writes.
//! Every field here is backed by the committed fixture corpus (see
//! `fixtures/<version>/`) or, for the conversation store's protobuf blobs, by
//! the writer's own descriptor set declared in the canonical ground truth;
//! do not add fields without that evidence.

pub mod base64;
pub mod conversation;
pub mod cortex;
pub mod transcript;
pub mod wire;

pub use transcript::{ParsedLine, parse_line};
