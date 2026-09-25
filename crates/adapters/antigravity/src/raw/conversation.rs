//! Raw rows of the `conversation-db` snapshot stream.
//!
//! The capture layer runs the adapter's declared queries against a stable
//! copy of `conversations/<id>.db` and frames each result row as
//! `{"query": <name>, "row": {<column>: <value>}}`, with blob columns as
//! `{"$base64": "..."}`. Evidence: fixtures/1.1.20 (three conversation
//! stores) and the 2026-09-23 organic structure census; the blob grammar is
//! typed in [`super::cortex`].

use rosetta_adapter_support::{Drift, Unknown, note_unknown};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::base64;

/// `SELECT idx, data FROM gen_metadata`.
pub const GEN_METADATA_QUERY: &str = "gen_metadata";
/// `SELECT idx, step_type, status, metadata FROM steps`.
pub const STEPS_QUERY: &str = "steps";

/// The capture layer's record envelope.
#[derive(Debug, Deserialize, Serialize)]
pub struct SnapshotRecord {
    pub query: String,
    pub row: Value,
    #[serde(flatten)]
    pub unknown: Unknown,
}

/// A blob column as capture serializes it.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct Blob {
    #[serde(rename = "$base64")]
    pub base64: String,
}

impl Blob {
    /// `None` when the text is not canonical base64 — a framing failure the
    /// normalizer reports as a hard parse error.
    pub fn bytes(&self) -> Option<Vec<u8>> {
        base64::decode(&self.base64)
    }
}

/// One `gen_metadata` row: `data` is `CortexStepGeneratorMetadata`.
#[derive(Debug, Deserialize, Serialize)]
pub struct GenMetadataRow {
    pub idx: i64,
    pub data: Option<Blob>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

/// One `steps` row: `metadata` is `CortexStepMetadata`. The payload
/// columns are deliberately never selected; the transcript owns the
/// timeline and this stream owns usage only.
#[derive(Debug, Deserialize, Serialize)]
pub struct StepRow {
    pub idx: i64,
    pub step_type: i64,
    pub status: i64,
    pub metadata: Option<Blob>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl Drift for GenMetadataRow {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        note_unknown(&self.unknown, prefix, out);
    }
}

impl Drift for StepRow {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        note_unknown(&self.unknown, prefix, out);
    }
}

#[derive(Debug)]
pub enum ParsedRow {
    GenMetadata(GenMetadataRow),
    Step(StepRow),
    /// A query name this parser does not know — soft drift, never a failure.
    UnknownQuery {
        query: String,
        row: Value,
    },
}

/// Parse one framed row. A declared query whose columns moved is a hard
/// parse failure; an undeclared query is an unknown record kind.
pub fn parse_row(query: &str, row: Value) -> Result<ParsedRow, serde_json::Error> {
    Ok(match query {
        GEN_METADATA_QUERY => ParsedRow::GenMetadata(serde_json::from_value(row)?),
        STEPS_QUERY => ParsedRow::Step(serde_json::from_value(row)?),
        _ => ParsedRow::UnknownQuery {
            query: query.to_owned(),
            row,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::{ParsedRow, parse_row};
    use rosetta_adapter_support::Drift;
    use serde_json::json;

    #[test]
    fn declared_queries_parse_and_keep_unknown_columns_as_drift() {
        let parsed = parse_row(
            "steps",
            json!({"idx": 2, "step_type": 23, "status": 3, "metadata": {"$base64": "AQI="}, "later": 1}),
        )
        .unwrap();
        let ParsedRow::Step(step) = parsed else {
            panic!("expected a step row, got {parsed:?}");
        };
        assert_eq!(step.idx, 2);
        assert_eq!(step.metadata.as_ref().unwrap().bytes().unwrap(), [1, 2]);
        assert_eq!(step.drift_paths(), [".later"]);

        let parsed = parse_row("gen_metadata", json!({"idx": 0, "data": null})).unwrap();
        let ParsedRow::GenMetadata(generation) = parsed else {
            panic!("expected a generation row, got {parsed:?}");
        };
        assert!(generation.data.is_none());
        assert!(generation.drift_paths().is_empty());
    }

    #[test]
    fn a_moved_column_is_a_parse_failure_and_a_new_query_is_unknown() {
        assert!(parse_row("steps", json!({"idx": "two"})).is_err());
        assert!(parse_row("gen_metadata", json!({"data": {"$base64": ""}})).is_err());
        assert!(matches!(
            parse_row("executor_metadata", json!({"idx": 0})).unwrap(),
            ParsedRow::UnknownQuery { .. }
        ));
    }
}
