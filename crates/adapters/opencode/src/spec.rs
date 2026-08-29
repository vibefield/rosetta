//! The Layer-1 canonical ground truth, compiled into the adapter.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use rosetta_adapter_support::GroundTruthDoc;
use serde::Deserialize;
use serde::de::IgnoredAny;

const CANONICAL: &str = include_str!("../../../../groundtruth/opencode/groundtruth.json");

/// The current v1.2+ session store and sole runtime transcript owner.
pub const DATABASE: &str = "opencode-db";
pub const TIMELINE_QUERY: &str = "timeline";

pub fn doc() -> &'static GroundTruthDoc {
    static DOC: OnceLock<GroundTruthDoc> = OnceLock::new();
    DOC.get_or_init(|| {
        GroundTruthDoc::parse(CANONICAL).expect("embedded opencode groundtruth.json is valid")
    })
}

pub fn declared_message_roles() -> &'static BTreeSet<String> {
    declared("message-data")
}

pub fn declared_part_types() -> &'static BTreeSet<String> {
    declared("part-data")
}

fn declared(facet: &str) -> &'static BTreeSet<String> {
    static EMPTY: BTreeSet<String> = BTreeSet::new();
    facets().get(facet).unwrap_or(&EMPTY)
}

fn facets() -> &'static BTreeMap<String, BTreeSet<String>> {
    static FACETS: OnceLock<BTreeMap<String, BTreeSet<String>>> = OnceLock::new();
    FACETS.get_or_init(|| {
        let view: FacetsView =
            serde_json::from_str(CANONICAL).expect("embedded groundtruth.json is valid");
        view.streams
            .into_iter()
            .filter(|stream| stream.id == DATABASE)
            .flat_map(|stream| stream.facets)
            .map(|facet| (facet.id, facet.record_types.into_keys().collect()))
            .collect()
    })
}

#[derive(Deserialize)]
struct FacetsView {
    streams: Vec<StreamFacets>,
}

#[derive(Deserialize)]
struct StreamFacets {
    id: String,
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
    use super::{DATABASE, declared_message_roles, declared_part_types, doc};

    #[test]
    fn embedded_canonical_describes_opencode() {
        assert_eq!(doc().agent, "opencode");
        assert_eq!(doc().data_root, "~/.local/share/opencode");
        let database = doc().stream(DATABASE).expect("database stream");
        assert_eq!(database.glob, "opencode.db*");
    }

    #[test]
    fn declared_payload_vocabularies_come_from_the_canonical() {
        assert_eq!(
            declared_message_roles(),
            &["assistant".to_owned(), "user".to_owned()]
                .into_iter()
                .collect()
        );
        for part in [
            "text",
            "subtask",
            "reasoning",
            "file",
            "tool",
            "step-start",
            "step-finish",
            "snapshot",
            "patch",
            "agent",
            "retry",
            "compaction",
        ] {
            assert!(declared_part_types().contains(part), "missing {part}");
        }
        assert!(!declared_part_types().contains("thinking"));
    }
}
