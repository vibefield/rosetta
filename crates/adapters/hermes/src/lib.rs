//! rosetta-adapter-hermes — the Layer-2 adapter for Hermes Agent, designed
//! against the common interface with the Layer-1 canonical ground truth
//! (`groundtruth/hermes/groundtruth.json`) as its specification.
//! Replay tests prove it against the L1 fixture corpus.

pub mod adapter;
pub mod normalize;
pub mod raw;
pub mod spec;
pub mod state;
pub mod tool;
pub mod usage;

pub use adapter::HermesAdapter;
