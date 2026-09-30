//! rosetta-adapter-claude-code — the Layer-2 PARSER for Claude Code, designed
//! against the common interface with the Layer-1 canonical ground truth
//! (`groundtruth/claude-code/groundtruth.json`) as its specification.
//! Replay tests prove it against the L1 fixture corpus.
//!
//! Two gaps in the L2 contract are worked around locally rather than by
//! widening shared crates:
//!
//! * `AgentAdapter::skim` receives no runtime id, so it cannot derive a rosetta
//!   session id and emits no `SessionMeta` — it reports the NATIVE session id
//!   instead (see [`adapter::ClaudeCodeAdapter::skim`]).
//! * The workspace carries no date crate and `rosetta-core` exposes no
//!   timestamp parser, so RFC 3339 → `Ms` lives in [`time`]. Every agent will
//!   need it; it belongs in `core`.

pub mod adapter;
mod cost_state;
mod envelope;
pub mod normalize;
pub mod raw;
pub mod spec;
pub mod time;
pub mod tool;
mod usage;
mod web;
mod workflow;

pub use adapter::ClaudeCodeAdapter;
mod media;
