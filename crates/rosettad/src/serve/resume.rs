//! The cross-launch resume of an interrupted fresh RAM build. Unlike a fresh
//! build, which admits each runtime with its rows final, the resumed build
//! replays its sealed evidence and catches up serially into the canonical RAM
//! generation the read router already targets, reconciles cross-file tool
//! lifecycle once at the end, and leaves provisional timeline ordinals for
//! finalization to renumber. Every readiness decision it makes is therefore
//! withheld here and committed in one step after finalization and the
//! publication fence, before the backup that becomes the installed file: no
//! reader ever sees a non-Pending session whose rows are not final, and a
//! pause that wins before publication leaves its retained RAM view Pending.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use rosetta_adapter_support::AgentAdapter;
use rosetta_capture::{Ledger, SourceDef};
use rosetta_core::rows::{RuntimeRow, StreamState};
use rosetta_pipeline::{
    IngestOptions, Instrument, MetaReport, Metric, SessionFile, activate_stopped_source,
    reserve_retired_generation, reserve_unsealed_generation,
};
use rosetta_store::Store;

/// Readiness a resumed build computed and has not committed. Every session
/// stays Pending, its payload hidden from every read route, until
/// [`WithheldReadiness::commit`].
#[derive(Debug, Default)]
pub(super) struct WithheldReadiness {
    /// The last decision per session, in the order replay and catch-up made
    /// them: a later decision replaces an earlier one, as the commits it
    /// stands for would have.
    states: BTreeMap<String, StreamState>,
    /// Sessions whose interrupted source no longer reads into the roster.
    /// Each is held Streaming whatever its other sources decided, as a source
    /// cut short at capture holds its session on a fresh start.
    missing: BTreeSet<String>,
}

impl WithheldReadiness {
    /// Replay's decisions for one runtime's sealed sessions.
    pub(super) fn replayed(&mut self, states: BTreeMap<String, StreamState>) {
        self.states.extend(states);
    }

    /// The map the serial catch-up records its decisions into.
    pub(super) fn caught_up(&mut self) -> &mut BTreeMap<String, StreamState> {
        &mut self.states
    }

    /// Commit every decision in one transaction. Call only once the
    /// projection is reconciled and finalized and publication can no longer
    /// be paused, and before it is staged for installation, so the installed
    /// file carries the same states.
    pub(super) fn commit(self, store: &mut Store) -> Result<(), String> {
        store
            .set_session_stream_states(&self.into_states())
            .map_err(|error| format!("committing resumed readiness: {error}"))?;
        Ok(())
    }

    fn into_states(self) -> Vec<(String, StreamState)> {
        let mut states = self.states;
        for session_id in self.missing {
            states.insert(session_id, StreamState::Streaming);
        }
        states.into_iter().collect()
    }
}

/// Fence each source replay left unsealed against the current roster, before
/// the catch-up rereads it. An excluded session's capture is retired: its raw
/// prefix stays unsealed, which `rebuild` accepts only with the same
/// exclusion. A source no longer in the roster (its file vanished, or its
/// first record no longer identifies a session) is tolerated as a source
/// missing at capture, and closed the way a fresh build closes a file it
/// loses mid-read: its staged prefix activates at its generation and is
/// sealed in the staged ledger, so the published archive stays rebuildable,
/// and its session publishes Streaming. A source whose identity changed
/// still refuses. Returns how many sources were missing.
#[allow(clippy::too_many_arguments)]
pub(super) fn settle_interrupted_sources(
    name: &str,
    adapter: &'static dyn AgentAdapter,
    runtime: &RuntimeRow,
    ledger_dir: &Path,
    store: &mut Store,
    ledger: &mut Ledger,
    unsealed: &BTreeMap<String, (u64, SourceDef)>,
    roster: &[SessionFile],
    meta: &MetaReport,
    opts: &IngestOptions,
    instrument: &Instrument,
    withheld: &mut WithheldReadiness,
) -> Result<usize, String> {
    let mut missing = 0;
    for (path, (generation, source)) in unsealed {
        if meta.excluded_sessions.contains(&source.session_id)
            || opts
                .exclude_cwds
                .excludes_session(store, &source.session_id)
                .map_err(|error| error.to_string())?
        {
            // Retire unfinished capture as well as ordinary roster work.
            // Its raw prefix stays unsealed: never certify a complete
            // generation or reread a source the user has excluded.
            reserve_retired_generation(adapter, runtime, store, source, *generation, instrument)
                .map_err(|error| format!("retiring {path} generation: {error}"))?;
            continue;
        }
        let Some(file) = roster.iter().find(|file| file.relative_path == *path) else {
            activate_stopped_source(
                adapter,
                runtime,
                ledger_dir,
                store,
                source,
                *generation,
                ledger,
                instrument,
                opts.cancel.as_deref(),
            )
            .map_err(|error| format!("closing vanished {path} generation: {error}"))?;
            withheld.missing.extend(source.projected_session_id());
            instrument.add(Metric::FilesMissing, 1);
            missing += 1;
            continue;
        };
        if file.stream_id != source.stream_id
            || file.session_id != source.session_id
            || file.native_session_id != source.native_session_id
        {
            return Err(format!(
                "cannot finish {name}: interrupted source {path} changed identity"
            ));
        }
        reserve_unsealed_generation(adapter, runtime, store, file, *generation, instrument)
            .map_err(|error| format!("reserving {path} generation: {error}"))?;
    }
    Ok(missing)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    use rosetta_adapter_claude_code::ClaudeCodeAdapter;
    use rosetta_core::IngestOp;
    use rosetta_core::rows::{SessionRole, SessionRow};
    use rosetta_server::ingestion::{IngestionAction, IngestionControl, IngestionControlRequest};
    use rosetta_store::Batch;
    use rusqlite::OpenFlags;

    use super::*;
    use crate::cold_stage::{ColdEnrollment, ServeColdStage};
    use crate::ingestion_control::ColdControl;

    /// The commit point, deterministically. A pause that wins before the
    /// publication fence leaves the reader's RAM generation Pending although
    /// the build reconciled and finalized; a publication past the fence
    /// installs the committed states.
    #[test]
    fn readiness_commits_past_the_publication_fence_and_into_the_installed_file() {
        for paused in [true, false] {
            let temp = tempfile::tempdir().unwrap();
            let db = temp.path().join("rosetta.db");
            let ledger = db.with_extension("ledger");
            let uri = format!(
                "file:rosetta-resume-fence-{}-{paused}?mode=memory&cache=shared",
                std::process::id()
            );
            let mut store =
                Store::open_shared_in_memory_for_rebuild_with_assets(&uri, &db).unwrap();
            let reader = rosetta_server::ProjectionReader::shared_memory(&uri).unwrap();
            let probe = rusqlite::Connection::open_with_flags(
                &uri,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
            )
            .unwrap();
            let runtime = crate::runtime_ledger::layout_runtime(&ClaudeCodeAdapter);
            let session = "resumed-session".to_string();
            store
                .apply(&Batch {
                    ops: vec![
                        IngestOp::Runtime(runtime.clone()),
                        IngestOp::SessionMeta(SessionRow {
                            id: session.clone(),
                            native_session_id: session.clone(),
                            runtime_id: runtime.id.clone(),
                            project_id: None,
                            cwd: None,
                            parent_session_id: None,
                            delegation: None,
                            root_session_id: session.clone(),
                            role: SessionRole::Main,
                            title: None,
                            summary: None,
                            started_at: 0,
                            updated_at: 0,
                            activity: None,
                            stream_state: StreamState::Pending,
                        }),
                    ],
                    checkpoints: vec![],
                    source: None,
                })
                .unwrap();
            let mut withheld = WithheldReadiness::default();
            withheld
                .caught_up()
                .insert(session.clone(), StreamState::Indexed);
            let control = ColdControl::open(&db, &ledger, false, Arc::new(AtomicBool::new(false)))
                .unwrap()
                .unwrap();
            if paused {
                let run = control.snapshot();
                let reply = control
                    .command(
                        &run.id,
                        &run.index_id,
                        "pause",
                        IngestionAction::Pause,
                        IngestionControlRequest {
                            expected_revision: run.revision,
                        },
                    )
                    .unwrap();
                assert!(reply.accepted);
            }
            let stage = ServeColdStage::open(&ledger, &db, &ColdEnrollment::default()).unwrap();
            let backup = tempfile::Builder::new()
                .suffix(".sqlite")
                .tempfile_in(temp.path())
                .unwrap()
                .into_temp_path();
            let published = super::super::publish_fresh_ram_projection(
                store,
                backup,
                &db,
                stage,
                &reader,
                &Instrument::new(),
                &rosetta_server::DaemonHealth::new(crate::engine::now_ms()),
                &control,
                Some(withheld),
            );
            let state = |conn: &rusqlite::Connection| -> String {
                conn.query_row("SELECT stream_state FROM session", [], |row| row.get(0))
                    .unwrap()
            };
            if paused {
                let error = published.err().expect("a pause wins before the fence");
                assert!(error.contains("fence"), "{error}");
                assert!(!db.exists());
                assert_eq!(state(&probe), "pending");
            } else {
                drop(published.unwrap());
                assert_eq!(state(&rusqlite::Connection::open(&db).unwrap()), "indexed");
            }
        }
    }

    #[test]
    fn later_decisions_replace_earlier_ones_and_missing_sources_hold_streaming() {
        let mut withheld = WithheldReadiness::default();
        withheld.replayed(BTreeMap::from([
            ("a".to_string(), StreamState::Indexed),
            ("b".to_string(), StreamState::Indexed),
            ("c".to_string(), StreamState::Streaming),
        ]));
        withheld
            .caught_up()
            .insert("a".to_string(), StreamState::Streaming);
        withheld
            .caught_up()
            .insert("c".to_string(), StreamState::Indexed);
        withheld.missing.insert("b".to_string());
        withheld.missing.insert("d".to_string());
        assert_eq!(
            withheld.into_states(),
            [
                ("a".to_string(), StreamState::Streaming),
                ("b".to_string(), StreamState::Streaming),
                ("c".to_string(), StreamState::Indexed),
                ("d".to_string(), StreamState::Streaming),
            ]
        );
    }
}
