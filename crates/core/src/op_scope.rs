//! What one [`IngestOp`] addresses and how much it writes: the two facts a
//! streamed fresh-cold build plans by. A root tree closes only when no op of
//! another tree names one of its sessions ([`IngestOp::named_session`]), and
//! the canonical writer cuts a large source into bounded transactions by the
//! bytes each op writes ([`IngestOp::approx_bytes`]).
//!
//! Both are exhaustive matches with no wildcard arm: a new variant does not
//! compile until it is classified here.

use serde::Serialize;

use crate::IngestOp;
use crate::timeline::{CanonicalText, TimelineBody, TimelineEntry};

/// Fixed per-op overhead in [`IngestOp::approx_bytes`]: the row's keys,
/// numbers and SQLite record framing that no string length accounts for.
const OP_OVERHEAD_BYTES: u64 = 64;

impl IngestOp {
    /// The session this op writes, for closure; `None` for an op that
    /// belongs to no session (content-addressed bytes, runtime- and
    /// project-wide rows, diagnostic drift). A link field (a parent or
    /// child session named inside a payload) is not a write: the store
    /// applies no cross-session write from one.
    pub fn named_session(&self) -> Option<&str> {
        match self {
            Self::ProjectionAsset { .. } | Self::Runtime(_) | Self::Project(_) | Self::Drift(_) => {
                None
            }
            Self::HtmlArtifact(publication) => Some(&publication.session_id),
            Self::HtmlArtifactContent(content) => Some(&content.session_id),
            Self::SessionMeta(row) => Some(&row.id),
            Self::SessionTitle(row) => Some(&row.session_id),
            Self::Message(entry) => Some(&entry.session_id),
            Self::ToolUpdate { session_id, .. }
            | Self::Usage { session_id, .. }
            | Self::WorkflowRun { session_id, .. }
            | Self::WorkflowJournal { session_id, .. } => Some(session_id),
            Self::SessionUsage(row) => Some(&row.session_id),
            Self::SessionUsageTotals(totals) => Some(&totals.session_id),
            Self::FileEdit(row) => Some(&row.session_id),
            Self::Plan(row) => Some(&row.session_id),
            Self::Task(row) => Some(&row.session_id),
            Self::GitCommit(row) => Some(&row.session_id),
        }
    }

    /// Content bytes this op writes: its string and byte payload lengths,
    /// plus `materialized_byte_len` for a [`IngestOp::ProjectionAsset`],
    /// plus a small fixed overhead. Prose is measured by length; structured
    /// payloads (tool interactions and patches, workflow documents, usage
    /// snapshots) by their serialized length, counted without allocating.
    /// Deterministic: the writer cuts transactions by it.
    pub fn approx_bytes(&self) -> u64 {
        let payload = match self {
            Self::ProjectionAsset { asset } => (asset.materialized_byte_len() as u64)
                .saturating_add(len(&asset.digest))
                .saturating_add(len(&asset.media_type)),
            Self::HtmlArtifact(publication) => json_len(publication),
            Self::HtmlArtifactContent(content) => json_len(content),
            Self::Runtime(row) => json_len(row),
            Self::Project(row) => json_len(row),
            Self::SessionMeta(row) => json_len(row),
            Self::SessionTitle(row) => len(&row.session_id).saturating_add(len(&row.title)),
            Self::Message(entry) => entry_bytes(entry),
            Self::ToolUpdate {
                session_id,
                correlation_key,
                evidence,
                patch,
                at: _,
            } => len(session_id)
                .saturating_add(len(correlation_key))
                .saturating_add(len(&evidence.source_identity.0))
                .saturating_add(json_len(patch)),
            Self::Usage {
                session_id,
                response,
                ts: _,
                model,
                tokens,
                identity,
            } => len(session_id)
                .saturating_add(len(response))
                .saturating_add(model.as_deref().map_or(0, len))
                .saturating_add(identity.as_deref().map_or(0, len))
                .saturating_add(json_len(tokens)),
            Self::SessionUsage(row) => json_len(row),
            Self::SessionUsageTotals(totals) => json_len(totals),
            Self::FileEdit(row) => json_len(row),
            Self::Plan(row) => json_len(row),
            Self::Task(row) => json_len(row),
            Self::WorkflowRun { session_id, run } => len(session_id).saturating_add(json_len(run)),
            Self::WorkflowJournal {
                session_id,
                observation,
            } => len(session_id).saturating_add(json_len(observation)),
            Self::GitCommit(row) => json_len(row),
            Self::Drift(fact) => json_len(fact),
        };
        payload.saturating_add(OP_OVERHEAD_BYTES)
    }
}

fn len(text: &str) -> u64 {
    text.len() as u64
}

fn text_bytes(text: &CanonicalText) -> u64 {
    len(text.as_preview())
}

fn entry_bytes(entry: &TimelineEntry) -> u64 {
    let ids = len(&entry.timeline_id)
        .saturating_add(len(&entry.session_id))
        .saturating_add(len(&entry.group.id))
        .saturating_add(entry.native_id.as_deref().map_or(0, len))
        .saturating_add(entry.parent_native_id.as_deref().map_or(0, len))
        .saturating_add(entry.model.as_deref().map_or(0, len))
        .saturating_add(entry.effort.as_deref().map_or(0, len))
        .saturating_add(entry.parent_tool_native_id.as_deref().map_or(0, len));
    let body = match &entry.body {
        TimelineBody::User { text, attachments } => text
            .as_ref()
            .map_or(0, text_bytes)
            .saturating_add(collection_len(attachments)),
        TimelineBody::Agent {
            text,
            attachments,
            citations,
        } => text
            .as_ref()
            .map_or(0, text_bytes)
            .saturating_add(collection_len(attachments))
            .saturating_add(collection_len(citations)),
        TimelineBody::Thinking { text } => text_bytes(text),
        TimelineBody::Summary { purpose: _, text } => text_bytes(text),
        TimelineBody::Collaboration { event } => json_len(event),
        TimelineBody::ToolUse { tool } => json_len(tool),
        TimelineBody::Notice { notice } => json_len(notice),
    };
    ids.saturating_add(body)
}

/// The serialized length of a list, zero when it is empty.
fn collection_len<T: Serialize>(items: &[T]) -> u64 {
    if items.is_empty() { 0 } else { json_len(items) }
}

/// Bytes `value` serializes to, counted without keeping them.
fn json_len<T: Serialize + ?Sized>(value: &T) -> u64 {
    struct Counter(u64);
    impl std::io::Write for Counter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0 = self.0.saturating_add(buf.len() as u64);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter(0);
    // Every canonical payload serializes; a failure would only undercount.
    let _ = serde_json::to_writer(&mut counter, value);
    counter.0
}

#[cfg(test)]
mod tests {
    use crate::rows::{MessageKind, MessageRow, TokenSnapshot};

    use super::*;

    fn text_message(session_id: &str, text: &str) -> IngestOp {
        IngestOp::project_message(MessageRow {
            timeline_id: "t1".into(),
            session_id: session_id.into(),
            ordinal: 0,
            kind: MessageKind::User,
            subtype: None,
            uuid: None,
            parent_uuid: None,
            ts: Some(1),
            model: None,
            effort: None,
            content: Some(text.into()),
            tool: None,
            attachments: Vec::new(),
            citations: Vec::new(),
            group: None,
            response: None,
            block_index: None,
            branch_tool_id: None,
            usage: None,
            agent_version: None,
            src_cursor: None,
            src_offset: None,
        })
        .pop()
        .unwrap()
    }

    #[test]
    fn named_session_is_the_written_session() {
        assert_eq!(text_message("s-a", "hi").named_session(), Some("s-a"));
        let usage = IngestOp::Usage {
            session_id: "s-b".into(),
            response: "r".into(),
            ts: 1,
            model: None,
            tokens: TokenSnapshot::default(),
            identity: None,
        };
        assert_eq!(usage.named_session(), Some("s-b"));
    }

    #[test]
    fn approx_bytes_grows_with_the_text_it_writes() {
        let small = text_message("s", "x").approx_bytes();
        let large = text_message("s", &"x".repeat(10_000)).approx_bytes();
        assert!(large >= small + 9_999, "{small} → {large}");
        // Deterministic: the writer's cut points depend on it.
        assert_eq!(large, text_message("s", &"x".repeat(10_000)).approx_bytes());
    }
}
