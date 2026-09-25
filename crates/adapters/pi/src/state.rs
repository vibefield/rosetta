//! Cross-line state for the Pi session stream.
//!
//! Entry contexts are keyed by native ids so settings follow graph ancestry
//! even when alternate branches are interleaved in the file. Everything here
//! is transient RAM plus the adapter checkpoint: identifiers, roles and
//! counters only, never native payloads (iron rule 5).

use std::collections::BTreeMap;

use rosetta_core::rows::Ms;
use serde::{Deserialize, Serialize};

use crate::raw::session::CompactionEntry;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct PiState {
    header: Option<HeaderState>,
    entries: BTreeMap<String, EntryContext>,
    /// The model the running session most recently used, in FILE order: the
    /// last `model_change` or assistant `responseModel ?? model`. A resume
    /// takes `--model`, the session's model, or a fallback (`sdk.ts:198-228` at
    /// pi-mono a8ed497) and appends a `model_change` only for a new session
    /// (`:406-414`), so ancestry alone under-reports; summaries run on this
    /// model (`agent-session.ts:2414`, `:3675`).
    #[serde(default)]
    latest_model: Option<String>,
    /// Compaction id → its `firstKeptEntryId`: where the next compaction's
    /// history begins.
    #[serde(default)]
    compactions: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct HeaderState {
    pub cwd: String,
    pub started_at: Ms,
    /// The file was forked from another session (`parentSession`): entries
    /// older than the header are the parent's, copied usage included.
    #[serde(default)]
    pub forked: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct EntryContext {
    pub model: Option<String>,
    pub effort: Option<String>,
    /// Native parent id, so a compaction can walk its kept range back to the
    /// turn start the way Pi's cut-point search does.
    #[serde(default)]
    pub parent: Option<String>,
    #[serde(default)]
    pub kind: EntryKind,
}

/// What an entry contributes to the model context, as Pi's compaction sees it
/// (`compaction.ts:373-401` at pi-mono a8ed497; identical at 0.84.3 `:308-343`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EntryKind {
    /// No context message: model/thinking changes, session info, labels,
    /// extension state, usage entries. Also the default for checkpoints that
    /// predate this field.
    #[default]
    Silent,
    /// A context message that starts a turn: user, bashExecution, custom,
    /// branchSummary or compactionSummary roles, `custom_message` and
    /// `branch_summary` entries.
    TurnStart,
    /// A context message inside a turn: assistant, toolResult, undeclared roles.
    TurnBody,
    /// A compaction entry. It never starts a turn; whether it retained a tail
    /// decides where the next compaction's history begins.
    Compaction { retains_tail: bool },
}

impl PiState {
    pub(crate) fn header(&self) -> Option<&HeaderState> {
        self.header.as_ref()
    }

    pub(crate) fn set_header(&mut self, header: HeaderState) {
        self.header = Some(header);
    }

    pub(crate) fn inherited(&self, parent_id: Option<&str>) -> EntryContext {
        let inherited = parent_id
            .and_then(|parent| self.entries.get(parent))
            .cloned()
            .unwrap_or_default();
        EntryContext {
            parent: parent_id.map(str::to_owned),
            kind: EntryKind::Silent,
            ..inherited
        }
    }

    pub(crate) fn remember(&mut self, id: &str, context: EntryContext) {
        self.entries.insert(id.to_owned(), context);
    }

    pub(crate) fn remember_compaction(&mut self, entry: &CompactionEntry, context: EntryContext) {
        let retains_tail = entry.first_kept_entry_id != entry.id;
        self.compactions
            .insert(entry.id.clone(), entry.first_kept_entry_id.clone());
        self.remember(
            &entry.id,
            EntryContext {
                kind: EntryKind::Compaction { retains_tail },
                ..context
            },
        );
    }

    pub(crate) fn latest_model(&self) -> Option<&str> {
        self.latest_model.as_deref()
    }

    pub(crate) fn observe_model(&mut self, model: &str) {
        if !model.is_empty() {
            self.latest_model = Some(model.to_owned());
        }
    }

    /// A fork copies its parent's entries with their original timestamps, so
    /// anything older than the fork's own header is inherited history: its
    /// row stays, its usage was already counted in the parent.
    pub(crate) fn inherited_copy(&self, at: Option<Ms>) -> bool {
        match (&self.header, at) {
            (Some(header), Some(at)) => header.forked && at < header.started_at,
            _ => false,
        }
    }

    /// How many model requests produced a compaction's usage.
    ///
    /// Pi summarizes in one request unless the cut splits a turn; a split turn
    /// adds a turn-prefix request and only summarizes history when there is
    /// any (`compaction.ts:1016-1076` at a8ed497, `:873-932` at 0.84.3). The
    /// turn is split when `firstKeptEntryId` does not start a turn and a turn
    /// start exists at or after the boundary; the boundary is the previous
    /// compaction's first kept entry, or the session root (`prepareCompaction`,
    /// `:753-758` at 0.84.3). History exists when a context message precedes
    /// that turn start inside the boundary.
    pub(crate) fn compaction_requests(&self, entry: &CompactionEntry) -> i64 {
        let kept_id = entry.first_kept_entry_id.as_str();
        // A retain-none compaction keeps nothing, so nothing is split.
        if kept_id == entry.id {
            return 1;
        }
        let Some(kept) = self.entries.get(kept_id) else {
            return 1;
        };
        if kept.kind != EntryKind::TurnBody {
            return 1;
        }

        // Walk back to the turn start. Passing an older compaction continues
        // into its kept tail (it is transparent to the search) and pins the
        // boundary there; a retain-none one ends the search. Parents always
        // precede their children in an append-only file, so the walk is
        // bounded by the entry count even on a corrupt link cycle.
        let mut boundary: Option<&str> = None;
        let mut cursor = kept;
        for _ in 0..self.entries.len() {
            let Some(parent_id) = cursor.parent.as_deref() else {
                return 1;
            };
            let Some(parent) = self.entries.get(parent_id) else {
                return 1;
            };
            match parent.kind {
                EntryKind::TurnStart => {
                    // History before the turn start: anything kept from the
                    // previous compaction is a context message, so a turn
                    // start after the boundary always has history.
                    return match boundary {
                        Some(boundary) => {
                            if boundary == parent_id {
                                1
                            } else {
                                2
                            }
                        }
                        None => {
                            if self.has_history_before(parent) {
                                2
                            } else {
                                1
                            }
                        }
                    };
                }
                EntryKind::Compaction { retains_tail } if boundary.is_none() => {
                    if !retains_tail {
                        return 1;
                    }
                    let Some(first_kept) = self.compactions.get(parent_id) else {
                        return 1;
                    };
                    boundary = Some(first_kept.as_str());
                }
                EntryKind::Compaction { .. } | EntryKind::TurnBody | EntryKind::Silent => {}
            }
            if boundary == Some(parent_id) {
                // Reached the boundary without a turn start: not split.
                return 1;
            }
            cursor = parent;
        }
        1
    }

    /// Whether a context message precedes `turn_start` inside the current
    /// compaction boundary.
    fn has_history_before(&self, turn_start: &EntryContext) -> bool {
        let mut cursor = turn_start;
        for _ in 0..self.entries.len() {
            let Some(parent_id) = cursor.parent.as_deref() else {
                return false;
            };
            let Some(parent) = self.entries.get(parent_id) else {
                return false;
            };
            match parent.kind {
                EntryKind::TurnStart | EntryKind::TurnBody => return true,
                EntryKind::Compaction { retains_tail } => return retains_tail,
                EntryKind::Silent => {}
            }
            cursor = parent;
        }
        false
    }
}
