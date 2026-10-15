//! rosetta-adapter-pi — the Layer-2 PARSER for Pi, designed against the
//! common interface with the Layer-1 canonical ground truth
//! (`groundtruth/pi/groundtruth.json`) as its specification.
//!
//! @atlas layer agents · domain normalize

pub mod adapter;
pub mod normalize;
pub mod raw;
pub mod spec;
pub mod state;
pub mod time;
pub mod tool;
pub mod usage;

pub use adapter::PiAdapter;
