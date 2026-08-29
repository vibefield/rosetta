//! The Layer-1 canonical ground truth, compiled in — this adapter's SPEC
//! (CLAUDE.md iron rule #6). Parser and spec ship as one artifact, so a
//! regenerated `groundtruth.json` cannot drift away from the code that claims
//! to implement it: the crate simply stops compiling or its tests fail.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use rosetta_adapter_support::GroundTruthDoc;
use serde::Deserialize;
use serde::de::IgnoredAny;
use serde_json::Value;

const CANONICAL: &str = include_str!("../../../../groundtruth/claude-code/groundtruth.json");

/// The two streams that carry timeline records in the same JSONL dialect. Their
/// censuses are independent samples of one writer, so the adapter unions their
/// vocabularies — a type missing from one census is not evidence of a different
/// format, only of a smaller sample.
const TRANSCRIPT_STREAMS: [&str; 2] = ["transcript", "subagent-transcript"];

/// The layout view every adapter exposes through `AgentAdapter::layout`.
pub fn doc() -> &'static GroundTruthDoc {
    static DOC: OnceLock<GroundTruthDoc> = OnceLock::new();
    DOC.get_or_init(|| {
        GroundTruthDoc::parse(CANONICAL).expect("embedded claude-code groundtruth.json is valid")
    })
}

/// Record `type` values the canonical declares for the transcript dialect.
///
/// This is the line between "we chose not to project this" and "the agent
/// changed under us". A declared type the adapter does not map is skipped
/// silently; anything else is drift. Without the distinction the most common
/// record in the corpus (`attachment`, 8.7k observed) would report as unknown
/// on every ingest — exactly the alarm fatigue spaghetti drowned in.
pub fn declared_transcript_types() -> &'static BTreeSet<String> {
    static TYPES: OnceLock<BTreeSet<String>> = OnceLock::new();
    TYPES.get_or_init(|| {
        let view: RecordTypesView =
            serde_json::from_str(CANONICAL).expect("embedded groundtruth.json is valid");
        view.streams
            .into_iter()
            .filter(|stream| TRANSCRIPT_STREAMS.contains(&stream.id.as_str()))
            .flat_map(|stream| stream.record_types.into_keys())
            .collect()
    })
}

/// Validate top-level keys on a declared-but-unprojected record against the
/// union of both transcript censuses. These records remain permissive raw
/// values, but canonical schema additions must still raise drift.
pub fn unexpected_declared_fields(kind: &str, value: &Value) -> Vec<String> {
    let Some(expected) = declared_field_sets().get(kind) else {
        return Vec::new();
    };
    let Some(record) = value.as_object() else {
        return vec![".<non-object>".to_owned()];
    };
    record
        .keys()
        .filter(|key| !expected.contains(*key))
        .map(|key| format!(".{key}"))
        .collect()
}

fn declared_field_sets() -> &'static BTreeMap<String, BTreeSet<String>> {
    static FIELDS: OnceLock<BTreeMap<String, BTreeSet<String>>> = OnceLock::new();
    FIELDS.get_or_init(|| {
        let canonical: Value =
            serde_json::from_str(CANONICAL).expect("embedded groundtruth.json is valid");
        let mut by_kind = BTreeMap::<String, BTreeSet<String>>::new();
        for stream in canonical["streams"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|stream| {
                stream["id"]
                    .as_str()
                    .is_some_and(|id| TRANSCRIPT_STREAMS.contains(&id))
            })
        {
            for (kind, record) in stream["record_types"].as_object().into_iter().flatten() {
                let fields = record["shape"]["fields"]
                    .as_object()
                    .into_iter()
                    .flatten()
                    .map(|(field, _)| field.clone());
                by_kind.entry(kind.clone()).or_default().extend(fields);
            }
        }
        by_kind
    })
}

/// `GroundTruthDoc` keeps only the layout fields, so the record-type vocabulary
/// is read through this second, private view of the same bytes.
#[derive(Deserialize)]
struct RecordTypesView {
    streams: Vec<StreamRecordTypes>,
}

#[derive(Deserialize)]
struct StreamRecordTypes {
    id: String,
    /// Names only — the shapes are the raw types' business, not the router's.
    #[serde(default)]
    record_types: BTreeMap<String, IgnoredAny>,
}

#[cfg(test)]
mod tests {
    use super::{declared_transcript_types, doc, unexpected_declared_fields};
    use serde_json::json;

    #[test]
    fn embedded_canonical_describes_this_agent() {
        assert_eq!(doc().agent, "claude-code");
        assert_eq!(doc().data_root, "~/.claude");
        assert!(doc().stream("transcript").is_some());
    }

    #[test]
    fn declared_types_union_both_transcript_streams() {
        let types = declared_transcript_types();
        // Typed and projected.
        for name in ["user", "assistant", "system", "ai-title"] {
            assert!(types.contains(name), "{name} missing from the canonical");
        }
        // Declared but unprojected — must NOT read as drift.
        for name in ["attachment", "permission-mode", "bridge-session"] {
            assert!(types.contains(name), "{name} missing from the canonical");
        }
        // Only the subagent stream's census saw this one.
        assert!(types.contains("fork-context-ref"));
        assert!(!types.contains("no-such-record-type"));
    }

    #[test]
    fn declared_unprojected_records_still_detect_schema_additions() {
        assert!(
            unexpected_declared_fields(
                "attachment",
                &json!({"type":"attachment","attachment":{},"sessionId":"s"})
            )
            .is_empty()
        );
        assert_eq!(
            unexpected_declared_fields(
                "attachment",
                &json!({"type":"attachment","attachment":{},"sessionId":"s","new":1})
            ),
            [".new"]
        );
    }
}
