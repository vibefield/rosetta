//! The embedded Layer-1 canonical document: this adapter's parser spec and
//! stream inventory.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use rosetta_adapter_support::GroundTruthDoc;
use serde::Deserialize;
use serde::de::IgnoredAny;
use serde_json::Value;

const CANONICAL: &str = include_str!("../../../../groundtruth/copilot/groundtruth.json");

/// The sole append-only transcript stream.
pub const EVENTS: &str = "events";
/// Per-session title/workspace metadata document.
pub const SESSION_WORKSPACE: &str = "session-workspace";
/// The global SQLite store whose `assistant_usage_events` table is the
/// per-call usage record (RESEARCH_PLAN §5.13).
pub const SESSION_STORE: &str = "session-store";

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

/// Validate the envelope and `data` keys of a declared-but-unprojected event
/// against the embedded writer schema. This keeps the permissive raw fallback
/// without turning declared event types into a blind spot for added fields.
pub fn unexpected_declared_fields(kind: &str, value: &Value) -> Vec<String> {
    let Some(shape) = event_shapes().get(kind) else {
        return Vec::new();
    };
    let Some(record) = value.as_object() else {
        return vec![".<non-object>".to_owned()];
    };
    let top = object_fields(shape);
    let mut unexpected = record
        .keys()
        .filter(|key| !top.contains(*key))
        .map(|key| format!(".{key}"))
        .collect::<Vec<_>>();

    if let Some(data) = record.get("data").and_then(Value::as_object)
        && let Some(data_shape) = field_shape(shape, "data")
    {
        let expected = object_fields(data_shape);
        unexpected.extend(
            data.keys()
                .filter(|key| !expected.contains(*key))
                .map(|key| format!(".data.{key}")),
        );
    }
    unexpected.sort();
    unexpected
}

fn event_shapes() -> &'static BTreeMap<String, Value> {
    static SHAPES: OnceLock<BTreeMap<String, Value>> = OnceLock::new();
    SHAPES.get_or_init(|| {
        let document: Value =
            serde_json::from_str(CANONICAL).expect("embedded groundtruth.json is valid");
        document["streams"]
            .as_array()
            .and_then(|streams| streams.iter().find(|stream| stream["id"] == EVENTS))
            .and_then(|stream| stream["record_types"].as_object())
            .into_iter()
            .flatten()
            .filter_map(|(kind, record)| {
                record
                    .get("shape")
                    .cloned()
                    .map(|shape| (kind.clone(), shape))
            })
            .collect()
    })
}

fn object_fields(shape: &Value) -> BTreeSet<String> {
    if shape["kind"] == "object" {
        return shape["fields"]
            .as_object()
            .into_iter()
            .flatten()
            .map(|(field, _)| field.clone())
            .collect();
    }
    shape["variants"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(object_fields)
        .collect()
}

fn field_shape<'a>(shape: &'a Value, field: &str) -> Option<&'a Value> {
    if shape["kind"] == "object" {
        return shape["fields"][field].get("shape");
    }
    shape["variants"]
        .as_array()?
        .iter()
        .find_map(|variant| field_shape(variant, field))
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
        EVENTS, SESSION_STORE, SESSION_WORKSPACE, declared_event_types, doc,
        unexpected_declared_fields,
    };
    use serde_json::json;

    #[test]
    fn embedded_canonical_describes_copilot() {
        assert_eq!(doc().agent, "copilot");
        assert_eq!(doc().data_root, "~/.copilot");
        let events = doc().stream(EVENTS).expect("events stream");
        assert_eq!(events.glob, "session-state/*/events.jsonl");
        let workspace = doc()
            .stream(SESSION_WORKSPACE)
            .expect("session workspace stream");
        assert_eq!(workspace.glob, "session-state/*/workspace.yaml");
        let store = doc().stream(SESSION_STORE).expect("session store stream");
        assert_eq!(store.glob, "session-store.db*");
        assert_eq!(store.evidence, rosetta_adapter_support::Evidence::Observed);
    }

    #[test]
    fn the_usage_table_is_declared_by_the_canonical() {
        let document: serde_json::Value =
            serde_json::from_str(super::CANONICAL).expect("embedded groundtruth.json is valid");
        let store = document["streams"]
            .as_array()
            .and_then(|streams| streams.iter().find(|stream| stream["id"] == SESSION_STORE))
            .expect("session store stream");
        let table =
            &store["record_types"]["session-store.db:assistant_usage_events"]["shape"]["fields"];
        for column in [
            "id",
            "session_id",
            "model",
            "input_tokens",
            "output_tokens",
            "cache_read_tokens",
            "cache_write_tokens",
            "reasoning_tokens",
            "total_nano_aiu",
            "token_details_json",
            "created_at",
        ] {
            assert!(
                table.get(column).is_some(),
                "{column} missing from the canonical"
            );
        }
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

    #[test]
    fn declared_fallbacks_still_detect_new_fields() {
        let value = json!({
            "id": "event",
            "timestamp": "2026-08-28T00:00:00Z",
            "parentId": null,
            "type": "permission.completed",
            "data": {"requestId": "request", "result": {}, "toolCallId": "call"},
        });
        assert!(unexpected_declared_fields("permission.completed", &value).is_empty());

        let mut changed = value;
        changed["data"]["newPolicyField"] = json!(true);
        assert_eq!(
            unexpected_declared_fields("permission.completed", &changed),
            [".data.newPolicyField"]
        );
    }
}
