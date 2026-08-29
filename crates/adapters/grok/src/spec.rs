//! The Layer-1 canonical ground truth, compiled into the adapter.
//!
//! `updates` is the vendor-documented authoritative conversation stream. The
//! other session JSONL files remain layout inventory: `chat-history` repeats
//! model messages and `events` repeats turn/tool lifecycle telemetry.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use rosetta_adapter_support::GroundTruthDoc;
use serde::Deserialize;
use serde::de::IgnoredAny;

const CANONICAL: &str = include_str!("../../../../groundtruth/grok/groundtruth.json");

pub const UPDATES: &str = "updates";
pub const CHAT_HISTORY: &str = "chat-history";
pub const EVENTS: &str = "events";
pub const SUMMARY: &str = "summary";

pub fn doc() -> &'static GroundTruthDoc {
    static DOC: OnceLock<GroundTruthDoc> = OnceLock::new();
    DOC.get_or_init(|| {
        GroundTruthDoc::parse(CANONICAL).expect("embedded grok groundtruth.json is valid")
    })
}

/// `params.update.sessionUpdate` values declared by the canonical spec.
pub fn declared_update_types() -> &'static BTreeSet<String> {
    static TYPES: OnceLock<BTreeSet<String>> = OnceLock::new();
    TYPES.get_or_init(|| {
        let view: RecordTypesView =
            serde_json::from_str(CANONICAL).expect("embedded groundtruth.json is valid");
        view.streams
            .into_iter()
            .find(|stream| stream.id == UPDATES)
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
    use super::{CHAT_HISTORY, EVENTS, SUMMARY, UPDATES, declared_update_types, doc};

    #[test]
    fn embedded_canonical_describes_grok() {
        assert_eq!(doc().agent, "grok");
        assert_eq!(doc().data_root, "~/.grok");
        assert_eq!(
            doc().stream(UPDATES).map(|stream| stream.glob.as_str()),
            Some("sessions/*/*/updates.jsonl")
        );
        assert!(doc().stream(CHAT_HISTORY).is_some());
        assert!(doc().stream(EVENTS).is_some());
        assert!(doc().stream(SUMMARY).is_some());
    }

    #[test]
    fn declared_update_vocabulary_comes_from_the_canonical() {
        let types = declared_update_types();
        for projected in [
            "user_message_chunk",
            "agent_message_chunk",
            "agent_thought_chunk",
            "tool_call",
            "tool_call_update",
            "turn_completed",
        ] {
            assert!(types.contains(projected), "missing {projected}");
        }
        for unprojected in ["hook_execution", "subagent_finished"] {
            assert!(types.contains(unprojected), "missing {unprojected}");
        }
        assert!(!types.contains("no-such-update"));
    }
}
