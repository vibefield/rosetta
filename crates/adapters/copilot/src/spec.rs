//! The embedded Layer-1 canonical document: this adapter's parser spec and
//! stream inventory.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use rosetta_adapter_support::GroundTruthDoc;
use serde::Deserialize;
use serde::de::IgnoredAny;

const CANONICAL: &str = include_str!("../../../../groundtruth/copilot/groundtruth.json");

/// The sole append-only transcript stream.
pub const EVENTS: &str = "events";

pub fn doc() -> &'static GroundTruthDoc {
    static DOC: OnceLock<GroundTruthDoc> = OnceLock::new();
    DOC.get_or_init(|| {
        GroundTruthDoc::parse(CANONICAL).expect("embedded copilot groundtruth.json is valid")
    })
}

/// Top-level event `type` values declared by the canonical writer schema.
///
/// This separates a declared record the adapter intentionally ignores from a
/// new event introduced by a later Copilot release.
pub fn declared_event_types() -> &'static BTreeSet<String> {
    static TYPES: OnceLock<BTreeSet<String>> = OnceLock::new();
    TYPES.get_or_init(|| {
        let view: RecordTypesView =
            serde_json::from_str(CANONICAL).expect("embedded groundtruth.json is valid");
        view.streams
            .into_iter()
            .find(|stream| stream.id == EVENTS)
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
    use super::{EVENTS, declared_event_types, doc};

    #[test]
    fn embedded_canonical_describes_copilot() {
        assert_eq!(doc().agent, "copilot");
        assert_eq!(doc().data_root, "~/.copilot");
        let events = doc().stream(EVENTS).expect("events stream");
        assert_eq!(events.glob, "session-state/*/events.jsonl");
    }

    #[test]
    fn declared_vocabulary_comes_from_the_canonical() {
        let types = declared_event_types();
        for name in [
            "session.start",
            "assistant.message",
            "tool.execution_complete",
            "assistant.reasoning",
        ] {
            assert!(types.contains(name), "{name} missing from the canonical");
        }
        assert!(!types.contains("no-such-event"));
    }
}
