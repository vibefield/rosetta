//! Cold pause boundaries. Fresh workers stop through the existing cancellation
//! path; resume reconstructs their disposable RAM state from sealed evidence.
//! The old RAM reader and mirror remain readable throughout an in-process
//! restore, until the ordinary canonical publication switch.

use super::*;
use rosetta_server::ingestion::{IngestionControl, IngestionPhase, IngestionState};

#[allow(clippy::too_many_arguments)]
pub(super) fn run_fresh(
    flags: &Flags,
    db: &Path,
    adapters: &[&'static dyn rosetta_adapter_support::AgentAdapter],
    enrollment: Option<&Enrollment>,
    targets: &[RuntimeTarget],
    consented: &BTreeSet<String>,
    generation_consented: &BTreeSet<String>,
    opts: &IngestOptions,
    live: &LiveOptions,
    instrument: &Arc<Instrument>,
    store: Store,
    temp: tempfile::TempPath,
    mut recovering: bool,
    reader: &Arc<ProjectionReader>,
    health: &Arc<DaemonHealth>,
    hub: &Arc<Hub>,
    search_worker: &Fts5SearchWorker,
    search: &Fts5SidecarSearch,
    stop: &Arc<AtomicBool>,
    started: Instant,
    control: &ColdControl,
    commands: &commands::LedgerCommandReader,
) -> Result<(Store, u64), String> {
    let public_ledgers = ledger_root(flags, db);
    let mut next_store = Some((store, temp));
    let mut refreshed = None;
    let mut attempt = 0_u64;
    loop {
        if control.pause_requested() {
            control.stopped(None)?;
        }
        control.wait()?;
        let current = refreshed.as_deref().unwrap_or(targets);
        control.configure(consented.iter().cloned().collect())?;
        let stage = ServeColdStage::open(
            &public_ledgers,
            db,
            &ColdEnrollment::from_targets(consented_targets(current, consented)),
        )?;
        let stage_path = stage.path().to_path_buf();
        commands.stage(&stage_path);
        let (store, temp) = next_store.take().expect("one canonical writer per attempt");
        let outcome = if recovering {
            run_resumed_fresh_serve(
                flags,
                db,
                current,
                consented,
                generation_consented,
                opts,
                live,
                instrument,
                store,
                temp,
                stage,
                reader,
                health,
                hub,
                search_worker,
                search,
                stop,
                started,
                control,
                attempt > 0,
            )
        } else {
            run_fresh_fast_serve(
                flags,
                db,
                current,
                consented,
                opts,
                live,
                instrument,
                store,
                temp,
                stage,
                reader,
                health,
                hub,
                search_worker,
                search,
                stop,
                started,
                control,
            )
        };
        match outcome {
            Ok(done) => return Ok(done),
            Err(error) if stop.load(Ordering::SeqCst) => {
                // A stop (ctrl-c, the app's quit) also keeps the staged
                // generation for the next start, so it completes the same
                // physical barrier a pause does, within the host's kill
                // window. A failed barrier is reported; the stop keeps its
                // own error.
                stop_barrier(&stage_path);
                return Err(error);
            }
            Err(error) => {
                let observed = control.snapshot();
                if observed.state == IngestionState::Succeeded
                    || observed.phase == Some(IngestionPhase::Publishing)
                {
                    // Installation may already have promoted the ledger or
                    // SQLite. Restart reconciles that fenced interval before
                    // admitting another command; never retry it as a new
                    // in-process private build.
                    return Err(error);
                }
                // Cancellation returns only after planner, sequencer, pool,
                // and ledger workers are joined. Complete the physical sync
                // barrier before making a paused claim. This also covers a
                // synchronous ledger's buffered drop on an error path.
                let sync = sync_evidence(&stage_path);
                if let Err(sync_error) = sync {
                    control.stopped(Some(&sync_error))?;
                    return Err(sync_error);
                }
                let paused = control.pause_requested();
                control.stopped((!paused).then_some(error.as_str()))?;
                if !paused && flags.ticks.is_some() {
                    return Err(error);
                }
                control.wait()?;
            }
        }
        attempt += 1;
        recovering = true;
        // Rewalk on every resume; the saved watcher queue is not a source
        // inventory. Enrollment itself is still this process's loaded policy.
        refreshed = Some(resolve_targets(
            adapters,
            &flags.home,
            enrollment,
            opts,
            instrument,
        )?);
        health.ingest_reset_progress();
        for target in consented_targets(refreshed.as_ref().unwrap(), consented) {
            health.ingest_register(
                &target.identity(),
                target.census.streamable_count as u64,
                target.census.streamable_bytes,
            );
        }
        let temp = tempfile::Builder::new()
            .prefix(".rosetta-backfill-")
            .suffix(".sqlite")
            .tempfile_in(projection_parent(db))
            .map_err(|e| e.to_string())?
            .into_temp_path();
        let uri = format!(
            "file:rosetta-resume-{}-{}-{attempt}?mode=memory&cache=shared",
            std::process::id(),
            now_ms()
        );
        let store = Store::open_shared_in_memory_for_rebuild_with_assets(&uri, db)
            .map_err(|e| e.to_string())?;
        next_store = Some((store, temp));
    }
}

/// The app host SIGKILLs ten seconds after its SIGINT; the stop barrier
/// takes at most half of that.
const STOP_SYNC_BUDGET: Duration = Duration::from_secs(5);

/// The stop path's barrier, with every writer already joined: report how
/// long syncing the staged evidence took, or why it did not finish.
fn stop_barrier(stage: &Path) {
    if !stage.exists() {
        // Publication already promoted it, after its own barrier.
        return;
    }
    let started = Instant::now();
    match sync_evidence_within(stage, STOP_SYNC_BUDGET) {
        Ok(()) => eprintln!(
            "stop barrier: staged raw ledger synced elapsed_ms={:.1}",
            started.elapsed().as_secs_f64() * 1_000.0
        ),
        Err(error) => eprintln!("stop barrier: staged raw ledger not synced ({error})"),
    }
}

/// [`sync_evidence`] on a helper thread, waited on for at most `budget`.
/// An fsync cannot be interrupted, so one that overruns is left to finish or
/// to end with the process; evidence synced at earlier seals stays valid.
fn sync_evidence_within(root: &Path, budget: Duration) -> Result<(), String> {
    let (sender, receiver) = std::sync::mpsc::channel();
    let root = root.to_path_buf();
    std::thread::Builder::new()
        .name("stop-barrier".into())
        .spawn(move || {
            let _ = sender.send(sync_evidence(&root));
        })
        .map_err(|error| format!("starting the sync: {error}"))?;
    match receiver.recv_timeout(budget) {
        Ok(synced) => synced,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            Err(format!("still syncing after {} s", budget.as_secs()))
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            Err("the sync thread panicked".into())
        }
    }
}

/// All writers have gone before entry. Sync owned evidence only; never walk
/// native source roots or follow links out of a stage.
pub(crate) fn sync_evidence(root: &Path) -> Result<(), String> {
    if !root.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(root).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let kind = entry.file_type().map_err(|e| e.to_string())?;
        if kind.is_dir() {
            sync_evidence(&entry.path())?;
        } else if kind.is_file() {
            std::fs::File::open(entry.path())
                .and_then(|f| f.sync_all())
                .map_err(|e| e.to_string())?;
        } else {
            return Err("unexpected non-file in cold evidence".into());
        }
    }
    std::fs::File::open(root)
        .and_then(|f| f.sync_all())
        .map_err(|e| e.to_string())
}

/// Disk-progressive work already commits at source/slice boundaries. Keep
/// its cursor and scheduler in place while stopped, after flushing ledgers.
pub(crate) fn disk_boundary(
    control: Option<&ColdControl>,
    backfills: &mut [BackfillRuntime],
) -> Result<bool, String> {
    let Some(control) = control else {
        return Ok(false);
    };
    if !control.pause_requested() {
        return Ok(false);
    }
    for entry in backfills {
        if let Some(ledger) = &mut entry.ledger {
            ledger.flush().map_err(|e| e.to_string())?;
        }
    }
    control.stopped(None)?;
    control.wait()?;
    Ok(true)
}

pub(crate) fn publish_disk(
    control: Option<&ColdControl>,
    backfills: &mut [BackfillRuntime],
) -> Result<(), String> {
    let Some(control) = control else {
        return Ok(());
    };
    loop {
        disk_boundary(Some(control), backfills)?;
        match control.publishing() {
            Ok(()) => return Ok(()),
            Err(_) if control.pause_requested() => continue,
            Err(error) => return Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stop_barrier_syncs_a_staged_tree_within_its_budget() {
        let temp = tempfile::tempdir().unwrap();
        let stage = temp.path().join("rosetta.ledger.staged");
        std::fs::create_dir_all(stage.join("claude-code")).unwrap();
        std::fs::write(stage.join(".cold-generation.json"), b"{}").unwrap();
        std::fs::write(stage.join("claude-code/00000000.rledger"), b"RLZ1").unwrap();
        assert_eq!(sync_evidence_within(&stage, STOP_SYNC_BUDGET), Ok(()));
    }

    #[cfg(unix)]
    #[test]
    fn a_stop_barrier_that_cannot_sync_reports_instead_of_following_links() {
        let temp = tempfile::tempdir().unwrap();
        let stage = temp.path().join("rosetta.ledger.staged");
        std::fs::create_dir_all(&stage).unwrap();
        std::os::unix::fs::symlink(temp.path(), stage.join("escape")).unwrap();
        assert_eq!(
            sync_evidence_within(&stage, STOP_SYNC_BUDGET),
            Err("unexpected non-file in cold evidence".to_string())
        );
    }
}
