//! Product admission policy for Codex's internal approval-review threads.
//!
//! Match native provenance, never prompt text, titles, or the generic subagent
//! role: ordinary delegated code reviews remain user-facing work.

use serde_json::Value;

pub(crate) fn ledger_only(source: &Value, thread_source: Option<&str>) -> bool {
    if thread_source == Some("guardian_review") {
        return true;
    }
    if let Some(encoded) = source.as_str() {
        return serde_json::from_str::<Value>(encoded)
            .ok()
            .is_some_and(|source| guardian_source(&source));
    }
    guardian_source(source)
}

fn guardian_source(source: &Value) -> bool {
    source.pointer("/subagent/other").and_then(Value::as_str) == Some("guardian")
}
