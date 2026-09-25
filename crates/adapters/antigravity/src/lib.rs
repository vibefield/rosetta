//! rosetta-adapter-antigravity — the Layer-2 PARSER for Antigravity CLI,
//! designed against the common interface with the Layer-1 canonical ground
//! truth (`groundtruth/antigravity/groundtruth.json`) as its specification.
//!
//! Two streams are normalized. `transcript.jsonl` owns the timeline: the
//! canonical ground truth calls it the primary transcript, and every
//! committed fixture proves that `transcript_full.jsonl` and both
//! `chunks/*/00000000.jsonl` files are byte-identical copies, so those
//! siblings remain layout inventory. `conversations/<id>.db` owns usage: the
//! transcript carries no token counts, while the store's protobuf step
//! metadata records every model request (see [`usage`]).

pub mod adapter;
pub mod normalize;
pub mod raw;
pub mod spec;
pub mod time;
pub mod tool;
pub mod usage;

pub use adapter::AntigravityAdapter;
