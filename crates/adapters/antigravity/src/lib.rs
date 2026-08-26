//! rosetta-adapter-antigravity — the Layer-2 PARSER for Antigravity CLI,
//! designed against the common interface with the Layer-1 canonical ground
//! truth (`groundtruth/antigravity/groundtruth.json`) as its specification.
//!
//! `transcript.jsonl` is the only normalized stream. The canonical ground
//! truth calls it the primary transcript, and every committed fixture proves
//! that `transcript_full.jsonl` and both `chunks/*/00000000.jsonl` files are
//! byte-identical copies. Driving any sibling would ingest every timeline row
//! twice (or four times), so those streams remain layout inventory.

pub mod adapter;
pub mod normalize;
pub mod raw;
pub mod spec;
pub mod time;

pub use adapter::AntigravityAdapter;
