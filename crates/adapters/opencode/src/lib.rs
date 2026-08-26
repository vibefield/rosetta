//! rosetta-adapter-opencode — the Layer-2 PARSER for OpenCode, designed
//! against the common interface with the Layer-1 canonical ground truth
//! (`groundtruth/opencode/groundtruth.json`) as its specification.
//! Replay tests prove it against the L1 fixture corpus.

pub mod adapter;
pub mod normalize;
pub mod raw;
pub mod spec;

pub use adapter::OpenCodeAdapter;
