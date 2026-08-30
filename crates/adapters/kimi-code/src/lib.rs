//! rosetta-adapter-kimi-code — the Layer-2 PARSER for Kimi Code CLI, designed
//! against the common interface with the Layer-1 canonical ground truth
//! (`groundtruth/kimi-code/groundtruth.json`) as its specification.

pub mod adapter;
pub mod normalize;
pub mod raw;
pub mod spec;
pub mod time;
pub mod tool;

pub use adapter::KimiCodeAdapter;
