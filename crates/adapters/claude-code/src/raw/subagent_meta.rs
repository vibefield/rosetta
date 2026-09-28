//! Raw schema for `agent-<id>.meta.json` subagent sidecars.

use rosetta_adapter_support::{Drift, Unknown, note_unknown};
use serde::{Deserialize, Serialize};

/// Every field is backed by the committed organic census. Only
/// `worktree_path` affects project identity; the remaining fields stay typed
/// so additions cannot silently masquerade as known metadata.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentMeta {
    pub agent_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_fork: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_mode_required: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spawn_depth: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopped_by_user: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_path: Option<String>,
    // Census 2026-09-28.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_non_interactive: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_shape: Option<String>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl Drift for SubagentMeta {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        note_unknown(&self.unknown, prefix, out);
    }
}

pub fn parse(text: &str) -> Result<SubagentMeta, serde_json::Error> {
    serde_json::from_str(text)
}

#[cfg(test)]
mod tests {
    use super::parse;
    use rosetta_adapter_support::Drift;
    use serde_json::json;

    #[test]
    fn observed_worktree_sidecar_round_trips_without_drift() {
        let text = r#"{"agentType":"Explore","description":"map files","worktreePath":"/tmp/repo/.claude/worktrees/agent-a1"}"#;
        let parsed = parse(text).unwrap();
        assert_eq!(
            parsed.worktree_path.as_deref(),
            Some("/tmp/repo/.claude/worktrees/agent-a1")
        );
        assert!(parsed.drift_paths().is_empty());
        assert_eq!(
            serde_json::to_value(parsed).unwrap(),
            serde_json::from_str::<serde_json::Value>(text).unwrap()
        );
    }

    #[test]
    fn additions_remain_drift_visible() {
        let parsed = parse(r#"{"agentType":"Explore","newField":true}"#).unwrap();
        assert_eq!(parsed.drift_paths(), [".newField"]);
        assert_eq!(
            serde_json::to_value(parsed).unwrap()["newField"],
            json!(true)
        );
    }
}
