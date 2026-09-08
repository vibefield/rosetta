//! Bridge L2's batch observations to the transient L3 progress board. This
//! never makes a source, session, or half-reconciled timeline public.

use std::{collections::BTreeMap, sync::Arc};

use rosetta_pipeline::{IngestPlan, SessionFile, progress::ColdProgressObserver};
use rosetta_server::BackfillState;
use rosetta_server::{DaemonHealth, ProgressBoard};

pub(crate) struct ColdProgress {
    kind: String,
    plan: IngestPlan,
    board: Arc<ProgressBoard>,
    health: Arc<DaemonHealth>,
}

impl ColdProgress {
    pub fn observe(
        kind: &str,
        files: &[SessionFile],
        ownership: &BTreeMap<String, Option<String>>,
        board: Arc<ProgressBoard>,
        health: Arc<DaemonHealth>,
        fresh: bool,
    ) -> Arc<Self> {
        let plan = IngestPlan::settle(&[files.to_vec()], ownership);
        for (_, session_id, session) in plan.sessions() {
            board.seed_session(session.project_id.as_deref(), session_id, session.bytes);
            if !fresh {
                board.mark_bytes_unknown(session.project_id.as_deref());
            }
        }
        let mut known = fresh;
        for file in files {
            if file.driver != rosetta_adapter_support::StreamDriver::JsonLines {
                known = false;
                board.mark_bytes_unknown(plan.source_project_of(0, file));
            }
        }
        let total = if known {
            plan.sessions().map(|(_, _, session)| session.bytes).sum()
        } else {
            0
        };
        health.ingest_stream_total(kind, total);
        Arc::new(Self {
            kind: kind.to_owned(),
            plan,
            board,
            health,
        })
    }
}

impl std::fmt::Debug for ColdProgress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ColdProgress")
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

impl ColdProgressObserver for ColdProgress {
    fn source_bytes(&self, file: &SessionFile, bytes: u64) {
        self.board
            .advance(self.plan.source_project_of(0, file), bytes);
    }

    fn reading_finished(&self) {
        self.health
            .ingest_runtime_stage(&self.kind, BackfillState::Preparing);
    }
}
