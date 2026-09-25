//! rosetta-adapter-copilot — the Layer-2 PARSER for GitHub Copilot CLI,
//! designed against the common interface with the Layer-1 canonical ground
//! truth (`groundtruth/copilot/groundtruth.json`) as its specification.

pub mod adapter;
pub mod normalize;
pub mod raw;
pub mod spec;
pub mod time;
pub mod tool;
pub mod usage;

pub use adapter::CopilotAdapter;

mod images;
