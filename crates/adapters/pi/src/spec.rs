//! The Layer-1 canonical ground truth compiled into the adapter.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use rosetta_adapter_support::GroundTruthDoc;
use serde::Deserialize;
use serde::de::IgnoredAny;

const CANONICAL: &str = include_str!("../../../../groundtruth/pi/groundtruth.json");

/// Pi's one driven transcript stream.
pub const SESSION: &str = "session";

pub fn doc() -> &'static GroundTruthDoc {
    static DOC: OnceLock<GroundTruthDoc> = OnceLock::new();
    DOC.get_or_init(|| GroundTruthDoc::parse(CANONICAL).expect("embedded Pi ground truth is valid"))
}

/// Top-level `type` values declared by the canonical session stream.
pub fn declared_session_types() -> &'static BTreeSet<String> {
    static TYPES: OnceLock<BTreeSet<String>> = OnceLock::new();
    TYPES.get_or_init(|| {
        let view: RecordTypesView =
            serde_json::from_str(CANONICAL).expect("embedded Pi ground truth is valid");
        view.streams
            .into_iter()
            .find(|stream| stream.id == SESSION)
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
    use super::{SESSION, declared_session_types, doc};

    #[test]
    fn embedded_canonical_describes_pi() {
        assert_eq!(doc().agent, "pi");
        assert_eq!(doc().data_root, "~/.pi");
        let session = doc().stream(SESSION).expect("session stream");
        assert_eq!(session.glob, "agent/sessions/*/*.jsonl");
    }

    #[test]
    fn declared_types_come_from_the_canonical() {
        let types = declared_session_types();
        for name in [
            "session",
            "message",
            "compaction",
            "branch_summary",
            "custom",
            "custom_message",
            "label",
        ] {
            assert!(types.contains(name), "{name} missing from the canonical");
        }
        assert!(!types.contains("no-such-record"));
    }
}
