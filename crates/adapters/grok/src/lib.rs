//! rosetta-adapter-grok — the Layer-2 PARSER for Grok Build, designed against
//! the common interface with the Layer-1 canonical ground truth
//! (`groundtruth/grok/groundtruth.json`) as its specification.

pub mod adapter;
mod images;
pub mod normalize;
pub mod raw;
pub mod spec;
pub mod time;
pub mod tool;

pub use adapter::GrokAdapter;
