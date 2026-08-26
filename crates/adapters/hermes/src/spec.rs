//! The embedded Layer-1 canonical ground truth — this adapter's specification.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use rosetta_adapter_support::GroundTruthDoc;
use serde::Deserialize;
use serde::de::IgnoredAny;

const CANONICAL: &str = include_str!("../../../../groundtruth/hermes/groundtruth.json");

pub const STATE_DB: &str = "state-db";
pub const SCHEMA_VERSION: i64 = 26;

pub fn doc() -> &'static GroundTruthDoc {
    static DOC: OnceLock<GroundTruthDoc> = OnceLock::new();
    DOC.get_or_init(|| {
        GroundTruthDoc::parse(CANONICAL).expect("embedded hermes groundtruth.json is valid")
    })
}

/// Tables declared by the canonical `state-db` stream. Empty operational and
/// generated FTS tables are included: declared-but-unprojected is not drift.
pub fn declared_tables() -> &'static BTreeSet<String> {
    static TABLES: OnceLock<BTreeSet<String>> = OnceLock::new();
    TABLES.get_or_init(|| {
        state_stream()
            .record_types
            .keys()
            .filter_map(|name| name.strip_prefix("state.db:"))
            .map(str::to_owned)
            .collect()
    })
}

/// Tool names exercised and declared by the `tool-calls` facet.
pub fn declared_tool_names() -> &'static BTreeSet<String> {
    static TOOLS: OnceLock<BTreeSet<String>> = OnceLock::new();
    TOOLS.get_or_init(|| {
        state_stream()
            .facets
            .iter()
            .find(|facet| facet.id == "tool-calls")
            .map(|facet| facet.record_types.keys().cloned().collect())
            .unwrap_or_default()
    })
}

fn state_stream() -> StreamView {
    let view: CanonicalView =
        serde_json::from_str(CANONICAL).expect("embedded hermes groundtruth.json is valid");
    view.streams
        .into_iter()
        .find(|stream| stream.id == STATE_DB)
        .expect("canonical hermes state-db stream exists")
}

#[derive(Deserialize)]
struct CanonicalView {
    streams: Vec<StreamView>,
}

#[derive(Deserialize)]
struct StreamView {
    id: String,
    #[serde(default)]
    record_types: BTreeMap<String, IgnoredAny>,
    #[serde(default)]
    facets: Vec<FacetView>,
}

#[derive(Deserialize)]
struct FacetView {
    id: String,
    record_types: BTreeMap<String, IgnoredAny>,
}

#[cfg(test)]
mod tests {
    use super::{STATE_DB, declared_tables, declared_tool_names, doc};

    #[test]
    fn embedded_canonical_describes_hermes_state() {
        assert_eq!(doc().agent, "hermes");
        assert_eq!(doc().data_root, "~/.hermes");
        let stream = doc().stream(STATE_DB).expect("state-db stream");
        assert_eq!(stream.glob, "state.db*");
    }

    #[test]
    fn declared_vocabularies_come_from_the_canonical() {
        let tables = declared_tables();
        for table in [
            "sessions",
            "messages",
            "session_model_usage",
            "async_delegations",
            "messages_fts",
        ] {
            assert!(tables.contains(table), "{table} missing from canonical");
        }
        assert!(!tables.contains("future_table"));
        assert!(declared_tool_names().contains("terminal"));
    }
}
