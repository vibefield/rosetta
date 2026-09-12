//! Bridge L2's batch observations to the transient L3 progress board. This
//! never makes a source, session, or half-reconciled timeline public.

use std::{collections::BTreeMap, sync::Arc};

use rosetta_pipeline::{IngestPlan, SessionFile, progress::ColdProgressObserver};
use rosetta_server::BackfillState;
use rosetta_server::{DaemonHealth, ProgressBoard};

pub(crate) struct ColdProgress {
    /// The health entry this observer feeds — keyed by runtime id, since two
    /// runtimes of one kind each carry their own meter.
    runtime_id: String,
    plan: IngestPlan,
    board: Arc<ProgressBoard>,
    health: Arc<DaemonHealth>,
}

impl ColdProgress {
    pub fn observe(
        runtime_id: &str,
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
        for file in files.iter().filter(|file| !file.ledger_only) {
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
        health.ingest_stream_total(runtime_id, total);
        Arc::new(Self {
            runtime_id: runtime_id.to_owned(),
            plan,
            board,
            health,
        })
    }
}

impl std::fmt::Debug for ColdProgress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ColdProgress")
            .field("runtime_id", &self.runtime_id)
            .finish_non_exhaustive()
    }
}

impl ColdProgressObserver for ColdProgress {
    fn source_bytes(&self, file: &SessionFile, bytes: u64) {
        if !file.ledger_only {
            self.board
                .advance(self.plan.source_project_of(0, file), bytes);
        }
    }

    fn reading_finished(&self) {
        self.health
            .ingest_runtime_stage(&self.runtime_id, BackfillState::Preparing);
    }
}
