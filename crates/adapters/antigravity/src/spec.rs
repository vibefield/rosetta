//! The Layer-1 canonical ground truth, compiled into the adapter.
//!
//! Stream ownership is deliberately narrower than the layout. In every 1.1.20
//! fixture, these four files have the same SHA-256 and the same bytes:
//!
//! * `logs/transcript.jsonl`
//! * `logs/transcript_full.jsonl`
//! * `logs/chunks/transcript/00000000.jsonl`
//! * `logs/chunks/transcript_full/00000000.jsonl`
//!
//! The canonical calls the first one the primary transcript and the chunk
//! streams copies. It is therefore the sole timeline owner; the other three
//! are inventory. The corpus has no second chunk or rotation boundary, so it
//! does not establish truncation, boundary overlap, or generation behavior.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use rosetta_adapter_support::GroundTruthDoc;
use serde::Deserialize;
use serde::de::IgnoredAny;

const CANONICAL: &str = include_str!("../../../../groundtruth/antigravity/groundtruth.json");

/// The one stream family member that owns canonical timeline rows.
pub const TRANSCRIPT: &str = "transcript";

/// Overlapping transcript representations kept as inventory.
pub const TRANSCRIPT_FULL: &str = "transcript-full";
pub const TRANSCRIPT_CHUNKS: &str = "transcript-chunks";
pub const TRANSCRIPT_FULL_CHUNKS: &str = "transcript-full-chunks";

/// Authoritative multi-session metadata index. It supplements the transcript
/// with titles, previews, workspace identity, and native parentage, and also
/// keeps sessions whose transcript has not been retained discoverable.
pub const CONVERSATION_SUMMARIES: &str = "conversation-summaries";

pub fn doc() -> &'static GroundTruthDoc {
    static DOC: OnceLock<GroundTruthDoc> = OnceLock::new();
    DOC.get_or_init(|| {
        GroundTruthDoc::parse(CANONICAL).expect("embedded antigravity groundtruth.json is valid")
    })
}

/// Top-level `type` values declared for the canonical transcript owner.
pub fn declared_transcript_types() -> &'static BTreeSet<String> {
    static TYPES: OnceLock<BTreeSet<String>> = OnceLock::new();
    TYPES.get_or_init(|| {
        let view: RecordTypesView =
            serde_json::from_str(CANONICAL).expect("embedded groundtruth.json is valid");
        view.streams
            .into_iter()
            .find(|stream| stream.id == TRANSCRIPT)
            .map(|stream| stream.record_types.into_keys().collect())
            .unwrap_or_default()
    })
}

#[derive(Deserialize)]
struct RecordTypesView {
    streams: Vec<StreamRecordTypes>,
}

#[derive(Deserialize)]
struct StreamRecordTypes {
    id: String,
    #[serde(default)]
    record_types: BTreeMap<String, IgnoredAny>,
}

#[cfg(test)]
mod tests {
    use super::{
        CONVERSATION_SUMMARIES, TRANSCRIPT, TRANSCRIPT_CHUNKS, TRANSCRIPT_FULL,
        TRANSCRIPT_FULL_CHUNKS, declared_transcript_types, doc,
    };

    #[test]
    fn embedded_canonical_describes_antigravity() {
        assert_eq!(doc().agent, "antigravity");
        assert_eq!(doc().data_root, "~/.gemini/antigravity-cli");
        assert_eq!(
            doc().stream(TRANSCRIPT).expect("transcript stream").glob,
            "brain/*/.system_generated/logs/transcript.jsonl"
        );
        for stream in [TRANSCRIPT_FULL, TRANSCRIPT_CHUNKS, TRANSCRIPT_FULL_CHUNKS] {
            assert!(doc().stream(stream).is_some(), "missing {stream}");
        }
        assert!(doc().stream(CONVERSATION_SUMMARIES).is_some());
    }

    #[test]
    fn declared_types_come_from_the_primary_transcript() {
        let types = declared_transcript_types();
        for name in [
            "USER_INPUT",
            "PLANNER_RESPONSE",
            "RUN_COMMAND",
            "CHECKPOINT",
            "SYSTEM_MESSAGE",
            "CONVERSATION_HISTORY",
            "LIST_DIRECTORY",
            "VIEW_FILE",
            "ERROR_MESSAGE",
        ] {
            assert!(types.contains(name), "{name} missing from the canonical");
        }
        assert!(!types.contains("NO_SUCH_RECORD"));
    }
}
