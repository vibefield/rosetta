//! rosetta-adapter-opencode — the Layer-2 PARSER for OpenCode, designed
//! against the common interface with the Layer-1 canonical ground truth
//! (`groundtruth/opencode/groundtruth.json`) as its specification.
//! Replay tests prove it against the L1 fixture corpus.

pub mod adapter;
pub mod normalize;
pub mod plan;
pub mod raw;
pub mod spec;
pub mod tool;

pub use adapter::OpenCodeAdapter;

mod images;
mod normalize_v2;
mod usage;
mod usage_v2;
