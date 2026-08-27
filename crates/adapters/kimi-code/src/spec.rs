//! The canonical Layer-1 ground truth compiled into the adapter.

use std::collections::BTreeSet;
use std::sync::OnceLock;

use rosetta_adapter_support::GroundTruthDoc;
use serde_json::Value;

const CANONICAL: &str = include_str!("../../../../groundtruth/kimi-code/groundtruth.json");

pub const WIRE: &str = "wire";

pub fn doc() -> &'static GroundTruthDoc {
    static DOC: OnceLock<GroundTruthDoc> = OnceLock::new();
    DOC.get_or_init(|| {
        GroundTruthDoc::parse(CANONICAL).expect("embedded kimi-code groundtruth.json is valid")
    })
}

/// Top-level `wire.type` values declared by the canonical document.
pub fn declared_wire_types() -> &'static BTreeSet<String> {
    static TYPES: OnceLock<BTreeSet<String>> = OnceLock::new();
    TYPES.get_or_init(|| {
        wire_record_types()
            .and_then(Value::as_object)
            .map(|records| records.keys().cloned().collect())
            .unwrap_or_default()
    })
}

/// `context.append_loop_event.event.type` values declared by the canonical.
pub fn declared_loop_event_types() -> &'static BTreeSet<String> {
    static TYPES: OnceLock<BTreeSet<String>> = OnceLock::new();
    TYPES.get_or_init(|| {
        wire_record_types()
            .and_then(|records| records.get("context.append_loop_event"))
            .and_then(|record| record.pointer("/shape/fields/event/shape/fields/type/shape/values"))
            .and_then(Value::as_object)
            .map(|values| values.keys().cloned().collect())
            .unwrap_or_default()
    })
}

/// Writer-declared `ContentPart.type` values shared by messages, streamed
/// assistant parts, and array-valued tool results.
pub fn declared_content_part_types() -> &'static BTreeSet<String> {
    static TYPES: OnceLock<BTreeSet<String>> = OnceLock::new();
    TYPES.get_or_init(|| {
        wire_facet_record_types("content-parts")
            .and_then(Value::as_object)
            .map(|records| records.keys().cloned().collect())
            .unwrap_or_default()
    })
}

fn wire_record_types() -> Option<&'static Value> {
    wire_stream()?.get("record_types")
}

fn wire_facet_record_types(facet_id: &str) -> Option<&'static Value> {
    wire_stream()?
        .get("facets")?
        .as_array()?
        .iter()
        .find(|facet| facet["id"] == facet_id)?
        .get("record_types")
}

fn wire_stream() -> Option<&'static Value> {
    static VALUE: OnceLock<Value> = OnceLock::new();
    let canonical = VALUE.get_or_init(|| {
        serde_json::from_str(CANONICAL).expect("embedded kimi-code groundtruth.json is valid")
    });
    canonical["streams"]
        .as_array()?
        .iter()
        .find(|stream| stream["id"] == WIRE)
}

#[cfg(test)]
mod tests {
    use super::{
        WIRE, declared_content_part_types, declared_loop_event_types, declared_wire_types, doc,
    };

    #[test]
    fn embedded_canonical_describes_kimi_code() {
        assert_eq!(doc().agent, "kimi-code");
        assert_eq!(doc().data_root, "~/.kimi-code");
        assert_eq!(
            doc().stream(WIRE).expect("wire stream").glob,
            "sessions/*/*/agents/*/wire.jsonl"
        );
    }

    #[test]
    fn declared_vocabularies_come_from_the_canonical() {
        let records = declared_wire_types();
        for name in [
            "metadata",
            "context.append_loop_event",
            "context.apply_compaction",
        ] {
            assert!(records.contains(name), "{name} missing from canonical");
        }
        assert!(!records.contains("wormhole.opened"));

        let events = declared_loop_event_types();
        for name in [
            "step.begin",
            "step.end",
            "content.part",
            "tool.call",
            "tool.result",
        ] {
            assert!(events.contains(name), "{name} missing from canonical");
        }
        assert!(!events.contains("wormhole.opened"));

        let content_parts = declared_content_part_types();
        assert_eq!(
            content_parts.iter().map(String::as_str).collect::<Vec<_>>(),
            ["audio_url", "image_url", "text", "think", "video_url"]
        );
    }
}
