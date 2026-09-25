//! rosetta-adapter-kimi-code — the Layer-2 PARSER for Kimi Code CLI, designed
//! against the common interface with the Layer-1 canonical ground truth
//! (`groundtruth/kimi-code/groundtruth.json`) as its specification.

pub mod adapter;
mod content;
pub mod normalize;
pub mod raw;
pub mod spec;
pub mod time;
pub mod tool;
mod usage;

pub use adapter::KimiCodeAdapter;
