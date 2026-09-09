//! Reapply current adapter admission before any archived session metadata.
//!
//! Older ledgers may name sessions which today's normalizer deliberately
//! omits. Their opening native record is still authoritative; the archived
//! source bit alone cannot decide whether to recreate a canonical header.

use std::collections::{BTreeMap, HashSet};

use rosetta_adapter_support::{AgentAdapter, FileStat, StreamDriver};
use rosetta_capture::SourceDef;
use rosetta_core::IngestOp;
use serde::{Deserialize, Serialize};

#[derive(Default)]
pub(super) struct AdmissionScan {
    excluded_generations: HashSet<(String, u64)>,
}

impl AdmissionScan {
    pub(super) fn observe(
        &mut self,
        adapter: &dyn AgentAdapter,
        source: &SourceDef,
        generation: u64,
        start_offset: u64,
        payload: &[u8],
    ) {
        if start_offset != 0
            || adapter.stream_driver(&source.stream_id) != Some(StreamDriver::JsonLines)
        {
            return;
        }
        // A ledger frame holds one complete native record, without requiring
        // the original file or a second parser for its native provenance.
        let skim = adapter.skim(
            &source.stream_id,
            &FileStat {
                relative_path: source.relative_path.clone(),
                size: payload.len() as u64,
                modified_ms: 0,
            },
            payload,
            &[],
        );
        if skim.ledger_only && skim.session_id.as_deref() == Some(source.native_session_id.as_str())
        {
            self.excluded_generations
                .insert((source.relative_path.clone(), generation));
        }
    }

    pub(super) fn finish(self, surviving: &BTreeMap<String, (u64, SourceDef)>) -> ReplayAdmission {
        ReplayAdmission {
            excluded_sessions: surviving
                .iter()
                .filter(|(path, (generation, source))| {
                    source.ledger_only
                        || self
                            .excluded_generations
                            .contains(&((*path).clone(), *generation))
                })
                .map(|(_, (_, source))| source.session_id.clone())
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(super) struct ReplayAdmission {
    excluded_sessions: HashSet<String>,
}

impl ReplayAdmission {
    pub(super) fn filter_meta(&self, ops: &mut Vec<IngestOp>) {
        ops.retain(|op| match op {
            IngestOp::SessionMeta(row) => !self.excluded_sessions.contains(&row.id),
            IngestOp::SessionTitle(row) => !self.excluded_sessions.contains(&row.session_id),
            _ => true,
        });
    }

    pub(super) fn project_source(&self, source: &mut SourceDef) {
        // Apply at session scope: a metadata-only sibling may have no opening
        // raw record of its own. This changes only the disposable projection,
        // never the source definitions already stored in the raw ledger.
        source.ledger_only |= self.excluded_sessions.contains(&source.session_id);
    }
}
