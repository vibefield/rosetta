//! Raw types for the `session-state` stream (`sessions/*/*/state.json`).
//!
//! Evidence: THE CANONICAL `groundtruth.json` — fixtures at 0.38.0
//! (basic-chat, tool-use, resume, subagent, error) merged with an organic
//! census. `title` / `lastPrompt` / `titleKind` are organic-only.
//! Sub-agent entries add `parentAgentId` and `labels`.
//!
//! Kimi Code 2.0.2 (installed bundle, `sessionMetaSchema` kimi@103928171)
//! declares `version`, `cwd`, `agents`, `custom` and `lastTurnReason`
//! optional and adds `forkedFrom`. A fork's `state.json` (`fork()`
//! kimi@97834752) carries `forkedFrom: <source session id>`, a fresh
//! `createdAt` taken after the copied wires were written, the source's
//! `updatedAt`, and omits `custom` and `lastTurnReason` when the source had
//! none; the copied wire keeps the source's `metadata.created_at`.

use std::collections::BTreeMap;

use rosetta_adapter_support::{Drift, Unknown, note_unknown};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Parse a `state.json` document.
pub fn parse_document(text: &str) -> Result<SessionState, serde_json::Error> {
    serde_json::from_str(text)
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionState {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    pub created_at: u64,
    pub updated_at: u64,
    pub archived: bool,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub agents: BTreeMap<String, SessionAgent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_turn_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_custom_title: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_kind: Option<String>,
    /// The native id of the session this one was forked from (2.0.2
    /// `fork()` kimi@97834752). Present only on forks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forked_from: Option<String>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl SessionState {
    /// The fork instant, when this session is a fork: its own `createdAt`,
    /// which the writer stamps after every copied wire has been rewritten and
    /// marked. Every copied `usage.record` is older than it; every request the
    /// fork makes itself is newer. Absent on an ordinary session, so a
    /// migrated or resumed session never gates on its creation time.
    pub fn fork_instant(&self) -> Option<u64> {
        self.forked_from.as_ref().map(|_| self.created_at)
    }
}

impl Drift for SessionState {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        note_unknown(&self.unknown, prefix, out);
        for (id, agent) in &self.agents {
            agent.drift(&format!("{prefix}.agents.{id}"), out);
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionAgent {
    pub homedir: String,
    #[serde(rename = "type")]
    pub agent_type: String,
    /// Present on sub-agents (`type = "sub"`); absent on `main`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub labels: Option<Value>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl Drift for SessionAgent {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        note_unknown(&self.unknown, prefix, out);
    }
}

#[cfg(test)]
mod tests {
    use super::parse_document;

    #[test]
    fn a_fork_state_carries_its_fork_instant() {
        // Shaped from `fork()` kimi@97834752: forkedFrom, a fresh createdAt,
        // the source's updatedAt, titleKind, and no custom/lastTurnReason.
        let state = parse_document(
            r#"{"id":"session_fork","version":2,"cwd":"/work","createdAt":1790000005000,
                "updatedAt":1789999990000,"archived":false,"title":"Fork: t",
                "titleKind":"replaceable","forkedFrom":"session_source",
                "agents":{"main":{"homedir":"/h/main","type":"main"}},
                "isCustomTitle":false}"#,
        )
        .expect("2.0.2 fork state parses");
        assert_eq!(state.forked_from.as_deref(), Some("session_source"));
        assert_eq!(state.fork_instant(), Some(1_790_000_005_000));
        assert!(state.custom.is_none());
        assert!(state.last_turn_reason.is_none());
    }

    #[test]
    fn an_ordinary_state_has_no_fork_instant() {
        let state = parse_document(
            r#"{"id":"session_plain","version":2,"cwd":"/work","createdAt":1790000000000,
                "updatedAt":1790000001000,"archived":false,
                "agents":{"main":{"homedir":"/h/main","type":"main"}},
                "custom":{},"lastTurnReason":"completed","isCustomTitle":false}"#,
        )
        .expect("0.38.0 state parses");
        assert_eq!(state.fork_instant(), None);
    }
}
