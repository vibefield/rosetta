//! The Layer-1 canonical ground truth, compiled into the adapter.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use rosetta_adapter_support::GroundTruthDoc;
use serde::Deserialize;
use serde::de::IgnoredAny;

const CANONICAL: &str = include_str!("../../../../groundtruth/opencode/groundtruth.json");

/// The current v1.2+ session store and sole runtime transcript owner.
pub const DATABASE: &str = "opencode-db";
/// 1.x rows: one `message.data` × `part.data` pair per record.
pub const TIMELINE_QUERY: &str = "timeline";
/// 2.x rows: one `session_message` row per record, its `type` column as the
/// discriminant and its content parts inside `data`.
pub const TIMELINE_V2_QUERY: &str = "timeline-v2";
/// 2.x `session.usage.recorded.*` events: requests recorded outside any
/// message (title generation, compaction).
pub const USAGE_V2_QUERY: &str = "usage-v2";

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

/// The 2.x `session_message.type` vocabulary.
pub fn declared_session_message_types() -> &'static BTreeSet<String> {
    declared("session-message-data")
}

/// The 2.x assistant `content[].type` vocabulary.
pub fn declared_assistant_content_types() -> &'static BTreeSet<String> {
    declared("session-message-content")
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
    use super::{
        DATABASE, declared_assistant_content_types, declared_message_roles, declared_part_types,
        declared_session_message_types, doc,
    };

    #[test]
    fn embedded_canonical_describes_opencode() {
        assert_eq!(doc().agent, "opencode");
        assert_eq!(doc().data_root, "~/.local/share/opencode");
        let database = doc().stream(DATABASE).expect("database stream");
        // Preview channels write `opencode-<channel>.db` beside `opencode.db`
        // (2.x `database/path.ts`), and `-wal`/`-shm` siblings ride along.
        assert_eq!(database.glob, "opencode*.db*");
        for path in [
            "opencode.db",
            "opencode.db-wal",
            "opencode-feature-x.db",
            "opencode-feature-x.db-shm",
        ] {
            assert_eq!(
                doc().matching_stream(path).map(|stream| stream.id.as_str()),
                Some(DATABASE),
                "{path}"
            );
        }
        // The 2.x V1 migration imports `opencode-next.db` into `opencode.db`;
        // reading both would count those sessions twice.
        for path in ["opencode-next.db", "opencode-next.db-wal"] {
            let stream = doc().matching_stream(path).expect(path);
            assert_ne!(stream.id, DATABASE, "{path}");
        }
    }

    #[test]
    fn declared_2x_vocabularies_come_from_the_canonical() {
        for message_type in [
            "user",
            "assistant",
            "compaction",
            "synthetic",
            "system",
            "skill",
            "shell",
            "idle",
            "agent-switched",
            "model-switched",
            "location-switched",
        ] {
            assert!(
                declared_session_message_types().contains(message_type),
                "missing {message_type}"
            );
        }
        for content in ["text", "reasoning", "tool"] {
            assert!(
                declared_assistant_content_types().contains(content),
                "missing {content}"
            );
        }
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
