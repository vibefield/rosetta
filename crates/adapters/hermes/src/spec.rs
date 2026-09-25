//! The embedded Layer-1 canonical ground truth — this adapter's specification.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use rosetta_adapter_support::GroundTruthDoc;
use serde::Deserialize;
use serde::de::IgnoredAny;

const CANONICAL: &str = include_str!("../../../../groundtruth/hermes/groundtruth.json");

pub const STATE_DB: &str = "state-db";
/// A named profile's own `state.db` (`profiles/<name>/state.db`): the same
/// schema and writer as the default store, so the same snapshot plan.
pub const PROFILE_STATE_DB: &str = "profile-state-db";
pub const SCHEMA_VERSION: i64 = 26;

/// Streams that are Hermes session stores.
pub fn is_state_stream(stream_id: &str) -> bool {
    stream_id == STATE_DB || stream_id == PROFILE_STATE_DB
}

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
    use super::{
        PROFILE_STATE_DB, STATE_DB, declared_tables, declared_tool_names, doc, is_state_stream,
    };
    use rosetta_adapter_support::{Evidence, FileFormat};

    #[test]
    fn embedded_canonical_describes_hermes_state() {
        assert_eq!(doc().agent, "hermes");
        assert_eq!(doc().data_root, "~/.hermes");
        let stream = doc().stream(STATE_DB).expect("state-db stream");
        assert_eq!(stream.glob, "state.db*");
    }

    #[test]
    fn named_profile_stores_have_their_own_stream() {
        let stream = doc().stream(PROFILE_STATE_DB).expect("profile stream");
        assert_eq!(stream.glob, "profiles/*/state.db*");
        assert_eq!(stream.file_format, FileFormat::Sqlite);
        assert_eq!(stream.evidence, Evidence::Research);
        assert!(is_state_stream(STATE_DB) && is_state_stream(PROFILE_STATE_DB));
        assert!(!is_state_stream("profile-home"));

        let stream_of = |path: &str| doc().matching_stream(path).map(|stream| stream.id.as_str());
        assert_eq!(stream_of("state.db"), Some(STATE_DB));
        assert_eq!(stream_of("state.db-wal"), Some(STATE_DB));
        assert_eq!(stream_of("profiles/coder/state.db"), Some(PROFILE_STATE_DB));
        assert_eq!(
            stream_of("profiles/coder/state.db-wal"),
            Some(PROFILE_STATE_DB)
        );
        // The rest of a profile home is inventory without a driver, like the root.
        assert_eq!(
            stream_of("profiles/coder/config.yaml"),
            Some("profile-home")
        );
        assert_eq!(stream_of("profiles/coder/.env"), Some("profile-home"));
        // Tombstoned profiles sit one level deeper and are never a session store.
        assert_eq!(
            stream_of("profiles/.deleted/coder/state.db"),
            Some("profile-home")
        );
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
