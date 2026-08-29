//! The Layer-1 canonical ground truth, compiled in — this adapter's SPEC
//! (CLAUDE.md iron rule #6). Parser and spec ship as one artifact, so a
//! regenerated `groundtruth.json` cannot drift away from the code claiming to
//! implement it: the crate stops compiling or its tests fail.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use rosetta_adapter_support::GroundTruthDoc;
use serde::Deserialize;
use serde::de::IgnoredAny;

const CANONICAL: &str = include_str!("../../../../groundtruth/codex/groundtruth.json");

/// The one stream carrying timeline records: `sessions/*/*/*/rollout-*.jsonl`.
pub const ROLLOUT: &str = "rollout";
/// The versioned metadata database (`state_*.sqlite`, including sidecar hints).
pub const STATE_DB: &str = "state-db";

/// The layout view every adapter exposes through `AgentAdapter::layout`.
pub fn doc() -> &'static GroundTruthDoc {
    static DOC: OnceLock<GroundTruthDoc> = OnceLock::new();
    DOC.get_or_init(|| {
        GroundTruthDoc::parse(CANONICAL).expect("embedded codex groundtruth.json is valid")
    })
}

/// `event_msg.payload.type` values the canonical declares (facet `event-msg`).
pub fn declared_event_subtypes() -> &'static BTreeSet<String> {
    declared("event-msg")
}

/// `response_item.payload.type` values the canonical declares
/// (facet `response-items`).
pub fn declared_response_items() -> &'static BTreeSet<String> {
    declared("response-items")
}

/// The line between "we chose not to project this" and "the agent changed under
/// us". Both payload vocabularies are open and churn fast upstream, so a
/// declared subtype this adapter does not map is skipped in silence and only an
/// UNDECLARED one reports as drift — without the distinction every
/// `task_started` in the corpus would raise an alarm, which is the fatigue
/// spaghetti drowned in.
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
            .filter(|stream| stream.id == ROLLOUT)
            .flat_map(|stream| stream.facets)
            .map(|facet| (facet.id, facet.record_types.into_keys().collect()))
            .collect()
    })
}

/// `GroundTruthDoc` keeps only the layout fields, so the payload vocabularies
/// are read through this second, private view of the same bytes.
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
    use super::{ROLLOUT, STATE_DB, declared_event_subtypes, declared_response_items, doc};

    #[test]
    fn embedded_canonical_describes_this_agent() {
        assert_eq!(doc().agent, "codex");
        assert_eq!(doc().data_root, "~/.codex");
        let rollout = doc().stream(ROLLOUT).expect("rollout stream");
        assert_eq!(rollout.glob, "sessions/*/*/*/rollout-*.jsonl");
        let state = doc().stream(STATE_DB).expect("state database stream");
        assert_eq!(state.glob, "state_*.sqlite*");
    }

    #[test]
    fn declared_vocabularies_come_from_the_facets() {
        let events = declared_event_subtypes();
        // Projected.
        assert!(events.contains("token_count"));
        // Declared but unprojected — must NOT read as drift.
        for name in ["task_started", "task_complete", "item_completed"] {
            assert!(events.contains(name), "{name} missing from the canonical");
        }
        assert!(!events.contains("no-such-subtype"));

        let items = declared_response_items();
        for name in [
            "message",
            "function_call",
            "function_call_output",
            "custom_tool_call",
            "custom_tool_call_output",
        ] {
            assert!(items.contains(name), "{name} missing from the canonical");
        }
    }
}
