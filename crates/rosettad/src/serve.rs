//! `rosettad serve` — the daemon: watch-style live ingest on the main
//! thread, the L3 HTTP/WS transport on a tokio runtime, and the hot tier
//! between them. The topology keeps every iron rule intact: the single
//! synchronous writer owns the store on this thread; async lives only in
//! the server; the hub absorbs drained effects strictly after commit.
//!
//! The transport binds FIRST — `/api/status` answers from process start and
//! carries the cold-start meters, so the app is never met by a refused
//! connection. Consent gates all reading: discovery fills the roll-call
//! board, and a runtime is ingested only if it was enrolled on an earlier
//! run (the projection's `enabled` flags), named on the command line, or
//! chosen in the app while the daemon waits. On a normal first run,
//! source-free metadata enters a named canonical RAM projection immediately;
//! closed reconciled runtimes follow in completion order, and the read router
//! switches to the validated disk copy at the durability barrier.
//! Existing/resumable projections and metadata-audit mode retain the sliced
//! `ColdEngine`: metadata effects drain between passes, watcher hints and
//! boosts can reorder its lanes, and scoped row effects serve only sessions
//! readers currently hold open.

use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use rosetta_capture::{NotifyEvents, SourceWake};
use rosetta_pipeline::{
    IngestOptions, Instrument, InstrumentSnapshot, LiveOptions, LiveRunner, Metric,
    RawCommandLocator, RebuildOptions, SessionFile, SliceBudget, StreamReport,
    meta_ingest_inventory_into, read_raw_command, rebuild_runtime_with_options,
    reserve_unsealed_generation, stream_backfill_into,
};
use rosetta_search::{Fts5SearchWorker, Fts5SidecarSearch, SearchBackend, SearchReadiness};
use rosetta_server::{
    AppState, BoostLedger, ConsentGate, DaemonHealth, EnrolledRuntimes, Hub, HubConfig,
    IngestCounterSource, IngestCounters, ProjectionReader, RawCommandReader, RuntimeIdentity,
    RuntimeTick, router,
};
use rosetta_store::{Store, queries};

use crate::cold_stage::{ColdEnrollment, ColdGeneration, ServeColdStage};
use crate::engine::{
    BackfillRuntime, ColdCx, ColdEngine, StreamMeter, absorb_effects, acknowledge_search, now_ms,
};
use crate::enroll::{RuntimeTarget, discovered_by_kind, resolve_targets};
use crate::recovery::restart_incomplete_projection;
use crate::runtime_ledger::{is_layout_root, ledger_dir, ledger_dir_for, open_runtime_ledger};
use crate::{
    AppliedColdRuntime, ColdCaptureRequest, Flags, apply_captured_cold_runtime,
    finish_parallel_cold_runtime, identity_name, install_rebuilt_projection, ledger_root,
    load_enrollment, make_single_file, open_progressive_backfill, parse_flags,
    plan_parallel_cold_runtime, projection_lock, projection_parent, record_backfill_failures,
    selected,
};

const DEFAULT_ADDR: &str = "127.0.0.1:4640";
const CONSENT_POLL: Duration = Duration::from_millis(200);

#[cfg(test)]
#[path = "serve_tests.rs"]
mod tests;

struct LedgerCommandReader {
    root: PathBuf,
    adapters: Vec<&'static dyn rosetta_adapter_support::AgentAdapter>,
}

impl RawCommandReader for LedgerCommandReader {
    fn read_command(&self, source: &queries::CommandSource) -> Result<Option<String>, String> {
        let adapter = self
            .adapters
            .iter()
            .copied()
            .find(|adapter| adapter.kind() == source.agent_kind)
            .ok_or_else(|| format!("no adapter for {}", source.agent_kind))?;
        let locator = RawCommandLocator {
            runtime_id: source.runtime_id.clone(),
            session_id: source.session_id.clone(),
            cursor_id: source.cursor_id,
            relative_path: source.relative_path.clone(),
            generation: source.generation,
            offset: source.offset,
            timeline_id: source.timeline_id.clone(),
            correlation_key: source.correlation_key.clone(),
        };
        // The runtime's ledger directory follows its identity root: the
        // per-kind directory for a discovered runtime, `<kind>~<digest>` for
        // a supervisor-enrolled one.
        let dir = ledger_dir_for(&self.root, &source.agent_kind, &source.data_root);
        read_raw_command(adapter, &dir, &locator).map_err(|error| error.to_string())
    }
}

/// The consented targets a serve mode iterates, in target order.
fn consented_targets<'a>(
    targets: &'a [RuntimeTarget],
    consented: &'a BTreeSet<String>,
) -> impl Iterator<Item = &'a RuntimeTarget> + 'a {
    targets
        .iter()
        .filter(move |target| consented.contains(target.id()))
}

/// Map a resumed generation's consent onto this start's targets: its
/// discovered kinds go to those kinds' discovered targets; its enrolled
/// runtimes must still be named by this start's file, or the generation
/// cannot finish — its staged evidence would publish without its rows.
fn generation_consent(
    generation: &ColdGeneration,
    targets: &[RuntimeTarget],
) -> Result<BTreeSet<String>, String> {
    let by_kind = discovered_by_kind(targets);
    let mut consented = BTreeSet::new();
    for kind in &generation.enabled_agents {
        match by_kind.get(kind.as_str()) {
            Some(target) => {
                consented.insert(target.id().to_string());
            }
            None => {
                return Err(format!(
                    "cold generation names unavailable adapters: {kind}"
                ));
            }
        }
    }
    for runtime in generation.enrolled_runtimes() {
        if !targets
            .iter()
            .any(|target| target.is_enrolled() && target.id() == runtime.id)
        {
            return Err(format!(
                "cannot resume cold generation: it holds enrolled runtime {} {} which this start's --enroll file no longer names; add it back, or run `rosettad rebuild`",
                runtime.kind, runtime.data_root
            ));
        }
        consented.insert(runtime.id.clone());
    }
    Ok(consented)
}

/// A live meter over the pipeline's always-on atomic counters: the status
/// endpoint reads deltas since `baseline` while the writer thread is blocked
/// inside a backfill call.
fn counter_source(
    instrument: &Arc<Instrument>,
    baseline: InstrumentSnapshot,
) -> IngestCounterSource {
    let instrument = instrument.clone();
    Arc::new(move || {
        let delta = instrument.snapshot().delta_since(&baseline);
        IngestCounters {
            files_skimmed: delta.get(Metric::FilesSkimmed),
            bytes_read: delta.get(Metric::PayloadBytes),
            sessions_discovered: delta.get(Metric::SessionsDiscovered),
            records: delta.get(Metric::RecordsFramed),
        }
    })
}

/// Give the first detached generation a clean I/O window after canonical
/// publication. HTTP remains live and source watchers are already armed, so
/// agent writes queue as hints while the disposable index builds. A failed
/// search projection never blocks canonical live ingest.
fn await_initial_search(
    store: &mut Store,
    search: &Fts5SidecarSearch,
    worker: &Fts5SearchWorker,
    stop: &AtomicBool,
) -> Result<(), String> {
    worker.wake();
    while !stop.load(Ordering::SeqCst) {
        acknowledge_search(store, worker)?;
        if matches!(
            search.state().readiness,
            SearchReadiness::Ready | SearchReadiness::Failed
        ) {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

pub(crate) fn cmd_serve(args: &[String]) -> Result<(), String> {
    // Milestone base for the sweep harness: every `milestone:` line below
    // reports seconds since this instant.
    let started = Instant::now();
    let flags = parse_flags(args)?;
    let db = flags.db.clone().ok_or("serve needs --db PATH")?;
    // The starter-slice budget is the tunable under sweep: unset = the
    // blessed default, `--slice-budget 0` = whole files, anything else = a
    // byte target with the default record floor.
    let slice_budget = match flags.slice_budget {
        None => Some(SliceBudget::STARTER),
        Some(0) => None,
        Some(max_bytes) => Some(SliceBudget {
            max_bytes,
            min_records: SliceBudget::STARTER.min_records,
        }),
    };
    let addr: SocketAddr = flags
        .addr
        .clone()
        .unwrap_or_else(|| DEFAULT_ADDR.to_string())
        .parse()
        .map_err(|error| format!("bad --addr: {error}"))?;
    let opts = IngestOptions::default();
    let live = LiveOptions::default();
    let instrument = Arc::new(Instrument::new());
    let adapters = selected(&flags)?;
    // The supervisor's file is validated before anything binds or reads: a
    // bad entry is a startup refusal the supervisor sees at once.
    let enrollment = load_enrollment(&flags)?;
    let enrolled_presentation = Arc::new(EnrolledRuntimes::new(
        enrollment
            .iter()
            .flat_map(|enrollment| enrollment.presentation()),
    ));
    let _projection_lock = projection_lock(&db)?;
    let public_ledgers = ledger_root(&flags, &db);
    // A named fresh-cold generation is the normal resume source. Inspect it
    // before the legacy archive/restart policy so an app restart never throws
    // away raw evidence that the previous process already captured.
    let pending_cold = if db.exists() {
        None
    } else {
        ServeColdStage::inspect(&public_ledgers, &db)?
    };
    let recovered = if flags.restart_incomplete && pending_cold.is_none() {
        restart_incomplete_projection(&db, &public_ledgers)?
    } else {
        None
    };
    // Kind-granular consent carried over from an archived projection: the
    // discovered kinds it had enabled. Its enrolled runtimes are recorded
    // only — their consent is this start's file.
    let recovered_enrollment = recovered
        .as_ref()
        .map(|recovery| recovery.enabled_agents.clone())
        .unwrap_or_default();
    if let Some(generation) = &pending_cold {
        println!(
            "recovery: resuming cold generation at {} — {} enrolled runtimes",
            generation.path.display(),
            generation.enabled_agents.len() + generation.enrolled_runtimes().count()
        );
    }
    if let Some(recovery) = &recovered {
        println!(
            "recovery: archived incomplete projection at {} — retained {} enrolled runtimes",
            recovery.path.display(),
            recovery.enabled_agents.len()
        );
        for runtime in &recovery.enrolled_runtimes {
            println!(
                "recovery: archived projection also held enrolled {} {} — consented only if this start's --enroll names it",
                runtime.kind, runtime.data_root
            );
        }
    }

    // A normal fresh app launch keeps canonical construction in RAM and lets
    // HTTP/WS read that same canonical shape progressively. Metadata is the
    // first public layer; closed reconciled runtimes follow. The validated
    // durable projection replaces the read target atomically at the end.
    // Metadata-audit mode deliberately retains the old disk-progressive seam
    // because its purpose is to leave that durable intermediate inspectable.
    let fresh_fast = !db.exists() && !flags.stop_after_meta;
    let (mut store, cold_temp, projection_reader) = if fresh_fast {
        let parent = projection_parent(&db);
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("creating {}: {error}", parent.display()))?;
        let named = tempfile::Builder::new()
            .prefix(".rosetta-backfill-")
            .suffix(".sqlite")
            .tempfile_in(parent)
            .map_err(|error| format!("creating private projection: {error}"))?;
        let temp = named.into_temp_path();
        let uri = format!(
            "file:rosetta-cold-{}-{}?mode=memory&cache=shared",
            std::process::id(),
            now_ms()
        );
        let store = Store::open_shared_in_memory_for_rebuild_with_assets(&uri, &db)
            .map_err(|error| format!("opening canonical RAM projection: {error}"))?;
        let reader = Arc::new(ProjectionReader::shared_memory(&uri)?);
        (store, Some(temp), reader)
    } else {
        (
            open_progressive_backfill(&db)?,
            None,
            Arc::new(ProjectionReader::file(db.clone())),
        )
    };
    let progressive = store.is_progressive();
    let warm_live = !fresh_fast && !progressive && !flags.stop_after_meta;

    // The transport comes up before any ingest work. A returning user's
    // mirror warms from the standing projection so the room is instantly
    // whole; a fresh install serves empty lists and honest meters.
    let health = Arc::new(DaemonHealth::new(now_ms()));
    if warm_live {
        // The standing canonical projection is already valid and readable.
        // Offline catch-up is live maintenance, not another cold backfill, so
        // returning users must never see their room veiled as stale.
        health.ingest_live();
    }
    let hub = Arc::new(Hub::new(HubConfig::default(), now_ms() as u64));
    let consent = Arc::new(ConsentGate::new());
    let boost = Arc::new(BoostLedger::new());
    let warmed = hub.warm(store.conn()).map_err(|error| error.to_string())?;
    let search = Fts5SidecarSearch::new(db.clone());
    let search_worker = if fresh_fast {
        Fts5SearchWorker::start_paused(search.clone())
    } else {
        Fts5SearchWorker::start(search.clone())
    };

    let tokio_runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("starting async runtime: {error}"))?;
    let listener = tokio_runtime
        .block_on(tokio::net::TcpListener::bind(addr))
        .map_err(|error| format!("binding {addr}: {error}"))?;
    let local = listener
        .local_addr()
        .map_err(|error| format!("reading bound address: {error}"))?;

    let stop = Arc::new(AtomicBool::new(false));
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    {
        // First ctrl-c stops gracefully (the backfill finishes its current
        // file; the live loop exits on its next tick); a second forces.
        let stop = stop.clone();
        tokio_runtime.spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                stop.store(true, Ordering::SeqCst);
                eprintln!("stopping — finishing the current file; ctrl-c again to force quit");
            }
            if tokio::signal::ctrl_c().await.is_ok() {
                std::process::exit(130);
            }
        });
    }
    let state = AppState::with_search(hub.clone(), db.clone(), Arc::new(search.clone()))
        .with_projection(projection_reader.clone())
        .with_daemon_health(health.clone())
        .with_consent(consent.clone())
        .with_boost(boost.clone())
        .with_enrolled(enrolled_presentation)
        .with_raw_commands(Arc::new(LedgerCommandReader {
            root: public_ledgers.clone(),
            adapters: crate::registry(),
        }));
    let server = tokio_runtime.spawn({
        let mut shutdown_rx = shutdown_rx.clone();
        let router = router(state);
        async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(async move {
                    let _ = shutdown_rx.wait_for(|stopped| *stopped).await;
                })
                .await
        }
    });
    // Announce before the ready line, so a parent that relays the origin can
    // also point siblings at the file. The guard lives to the end of serve:
    // every return and unwind below removes the file.
    let _announcement = if flags.announce {
        let dir = flags
            .announce_dir
            .clone()
            .or_else(crate::announce::default_dir)
            .ok_or("--announce needs --announce-dir DIR: no home directory to derive one")?;
        let db = std::path::absolute(&db).map_err(|error| format!("resolving --db: {error}"))?;
        Some(crate::announce::announce(
            &dir,
            &crate::announce::Announcement {
                schema: crate::announce::ANNOUNCEMENT_SCHEMA.into(),
                origin: format!("http://{local}"),
                pid: std::process::id(),
                protocol_version: rosetta_server::PROTOCOL_VERSION,
                db,
                started_at: now_ms(),
            },
        )?)
    } else {
        None
    };
    if flags.json {
        let mut ready = serde_json::json!({
            "event": "ready", "origin": format!("http://{local}"),
            "protocol_version": rosetta_server::PROTOCOL_VERSION,
        });
        if let Some(guard) = &_announcement {
            ready["announce"] = serde_json::Value::from(guard.path().display().to_string());
        }
        println!("{ready}");
    } else {
        println!("serving http://{local} — {warmed} mirror rows warm (ctrl-c to stop)");
    }

    // Mid-backfill readers ride the metadata effects seam: every committed
    // batch names exactly the identity rows it may have moved, and the
    // writer thread drains them into the mirror between passes — precise
    // diffs to /ws/meta with no polling cadence, no second read connection
    // pinning the cold WAL, and no hub mutation off the writer thread.
    // Budgeted slices keep passes short, so the drain cadence is bounded by
    // one slice.
    store.observe_meta_effects();

    // The census fills the roll-call board; consent decides what is read.
    // Targets: one Discovered per selected adapter, then this start's
    // Enrolled roots (`enroll.rs`). Consent is a set of runtime IDS.
    let targets = resolve_targets(
        &adapters,
        &flags.home,
        enrollment.as_ref(),
        &opts,
        &instrument,
    )?;
    let by_kind = discovered_by_kind(&targets);
    let installed_ids = |kind: &str| {
        by_kind
            .get(kind)
            .filter(|target| target.installed())
            .map(|target| target.id().to_string())
    };
    // Consent for DISCOVERED kinds persists as the projection's `enabled`
    // flags on their `~`-rooted rows — enrollment happens once, and restarts
    // proceed without asking again. An enabled row with an absolute root is a
    // supervisor-enrolled runtime from an earlier start: the file is its
    // truth on every start, so one the file no longer names is not watched
    // (its rows stay public — "retired" for now).
    let persisted_rows = rosetta_store::queries::all_runtimes(store.conn())
        .map_err(|error| format!("reading enrolled runtimes: {error:?}"))?;
    let persisted: BTreeSet<String> = persisted_rows
        .iter()
        .filter(|runtime| runtime.enabled && is_layout_root(&runtime.data_root))
        .map(|runtime| runtime.kind.clone())
        .collect();
    for retired in persisted_rows.iter().filter(|runtime| {
        runtime.enabled
            && !is_layout_root(&runtime.data_root)
            && !targets.iter().any(|target| target.id() == runtime.id)
    }) {
        println!(
            "{} {}: enrolled on an earlier start but absent from --enroll; not watched (its rows stay public)",
            retired.kind, retired.data_root
        );
    }
    let mut consented: BTreeSet<String> = if warm_live {
        // Enrollment is user state, not a discovery result. A temporarily
        // missing root disables its runner below but must not silently revoke
        // consent from the standing canonical projection.
        persisted
            .iter()
            .filter_map(|kind| {
                by_kind
                    .get(kind.as_str())
                    .map(|target| target.id().to_string())
            })
            .collect()
    } else {
        persisted
            .iter()
            .filter_map(|kind| installed_ids(kind))
            .collect()
    };
    let generation_consented = match &pending_cold {
        // The manifest is durable consent and its ledger may remain rebuildable
        // even if discovery currently cannot see the source root. The resume
        // branch below diagnoses an unavailable root instead of silently
        // dropping an enrolled runtime from the generation.
        Some(generation) => generation_consent(generation, &targets)?,
        None => BTreeSet::new(),
    };
    consented.extend(generation_consented.iter().cloned());
    consented.extend(
        recovered_enrollment
            .iter()
            .filter_map(|kind| installed_ids(kind)),
    );
    // `--agent` names kinds outright and `--all-agents` says everything
    // installed — both are consent from the command line, the headless
    // stand-in for the app's roll call. So is the file's `enabled_kinds`.
    if !flags.agents.is_empty() || flags.all_agents {
        consented.extend(
            targets
                .iter()
                .filter(|target| !target.is_enrolled() && target.installed())
                .map(|target| target.id().to_string()),
        );
    }
    if let Some(enrollment) = &enrollment {
        let (ids, warnings) = enrollment.enabled_discovered(&targets);
        for warning in warnings {
            eprintln!("{warning}");
        }
        consented.extend(ids);
        // Enrolled runtimes are consented by construction. They are never on
        // the roll call, never part of the one-shot gate, always ingested.
        consented.extend(enrollment.enrolled_ids());
    }
    // The roll call lists native layout roots only.
    consent.publish(
        targets
            .iter()
            .filter(|target| !target.is_enrolled())
            .map(|target| target.discovery_row(consented.contains(target.id())))
            .collect(),
    );
    if consented.is_empty() && !warm_live {
        // Nothing enrolled and nobody said yes: the board is published and
        // NOTHING has been read. Wait for the roll call. (With at least one
        // enrolled runtime there is work, so this never arms.)
        let granted = consent.arm();
        health.ingest_await_consent();
        println!("no enrolled agents — choose in the app (or pass --all-agents)");
        loop {
            if stop.load(Ordering::SeqCst) {
                break;
            }
            match granted.recv_timeout(CONSENT_POLL) {
                Ok(kinds) => {
                    // The grant names kinds; each maps to that kind's
                    // discovered target, exactly as the app has always meant.
                    consented.extend(kinds.iter().filter_map(|kind| installed_ids(kind)));
                    break;
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
    }
    for target in consented_targets(&targets, &consented) {
        if target.active() {
            // Meter denominators are the STREAMABLE census — the files the
            // skim and stream phases will actually read. The full footprint
            // (inventory streams included) belongs to the roll call, not to
            // a progress meter that could then never reach 100%.
            health.ingest_register(
                &target.identity(),
                target.census.streamable_count as u64,
                target.census.streamable_bytes,
            );
        }
    }

    if fresh_fast {
        let temp = cold_temp.expect("fresh fast serve owns a private projection file");
        let ledger_stage = ServeColdStage::open(
            &public_ledgers,
            &db,
            &ColdEnrollment::from_targets(consented_targets(&targets, &consented)),
        )?;
        let outcome = if pending_cold.is_some() {
            run_resumed_fresh_serve(
                &flags,
                &db,
                &targets,
                &consented,
                &generation_consented,
                &opts,
                &live,
                &instrument,
                store,
                temp,
                ledger_stage,
                &projection_reader,
                &health,
                &hub,
                &search_worker,
                &stop,
                started,
            )
        } else {
            run_fresh_fast_serve(
                &flags,
                &db,
                &targets,
                &consented,
                &opts,
                &live,
                &instrument,
                store,
                temp,
                ledger_stage,
                &projection_reader,
                &health,
                &hub,
                &search_worker,
                &stop,
                started,
            )
        };
        let (mut store, absorbed) = match outcome {
            Ok(done) => done,
            Err(error) if stop.load(Ordering::SeqCst) => {
                eprintln!(
                    "cold ingest paused — staged raw ledger retained for the next app start ({error})"
                );
                let _ = shutdown_tx.send(true);
                let _ = tokio_runtime.block_on(server);
                search_worker.stop();
                return Ok(());
            }
            Err(error) => {
                let _ = shutdown_tx.send(true);
                let _ = tokio_runtime.block_on(server);
                search_worker.stop();
                return Err(error);
            }
        };

        let _ = shutdown_tx.send(true);
        match tokio_runtime.block_on(server) {
            Ok(Ok(())) => {}
            Ok(Err(error)) => return Err(format!("server: {error}")),
            Err(error) => return Err(format!("server task: {error}")),
        }
        acknowledge_search(&mut store, &search_worker)?;
        search_worker.stop();
        println!("hub: {absorbed} live items fanned out");
        if flags.profile {
            println!("\n== profile ==\n{}", instrument.report());
        }
        return Ok(());
    }

    if warm_live {
        let (mut store, absorbed) = run_warm_live_serve(
            &flags,
            &targets,
            &consented,
            &opts,
            &live,
            &instrument,
            store,
            &public_ledgers,
            &health,
            &hub,
            &search_worker,
            &stop,
        )?;
        let _ = shutdown_tx.send(true);
        match tokio_runtime.block_on(server) {
            Ok(Ok(())) => {}
            Ok(Err(error)) => return Err(format!("server: {error}")),
            Err(error) => return Err(format!("server task: {error}")),
        }
        acknowledge_search(&mut store, &search_worker)?;
        search_worker.stop();
        println!("hub: {absorbed} live items fanned out");
        if flags.profile {
            println!("\n== profile ==\n{}", instrument.report());
        }
        return Ok(());
    }

    // Phase one: skim every enrolled runtime before reading any transcript.
    // This publishes the whole project/session roster first, so one large
    // runtime cannot hide every smaller runtime's lists.
    let wake = SourceWake::default();
    let mut backfills = Vec::new();
    let mut failures = Vec::new();
    for target in &targets {
        if !target.installed() && !target.is_enrolled() {
            println!("{}: not installed, skipping", target.name());
            continue;
        }
        if !consented.contains(target.id()) {
            println!("{}: installed but not enrolled, skipping", target.name());
            continue;
        }
        if stop.load(Ordering::SeqCst) {
            break;
        }
        let adapter = target.adapter;
        let runtime = target.enabled_runtime(); // consent, persisted for the next start

        let events = NotifyEvents::watch_with_wake(&target.root, &wake)
            .map_err(|error| error.to_string())?;
        let mut ledger = open_runtime_ledger(&public_ledgers, &runtime)?;
        health.ingest_skim(
            &runtime.id,
            counter_source(&instrument, instrument.snapshot()),
        );
        let (meta, files) = rosetta_pipeline::meta_ingest_into(
            adapter,
            &runtime,
            &target.root,
            &mut store,
            &opts,
            &instrument,
            Some(&mut ledger),
        )
        .map_err(|error| error.to_string())?;
        let stream_meter = StreamMeter::new(&instrument);
        health.ingest_skim_done(&runtime.id, meta.sessions as u64, meta.files_skimmed as u64);
        // Every skim's lists fan out the moment its batch commits.
        absorb_effects(&mut store, &hub)?;
        let roster = files.clone();
        backfills.push(BackfillRuntime {
            adapter,
            runtime,
            name: target.name(),
            root: target.root.clone(),
            discovery_truncated: target.census.walk_truncated,
            events,
            ledger: Some(ledger),
            meta,
            roster,
            stream: StreamReport::default(),
            stream_meter,
        });
    }

    // This is the exact cold-start seam the audit mode exposes: all bounded
    // metadata batches are committed, while no complete source record has
    // entered the raw ledger or canonical message tables. Freeze the health
    // surface here even though the ordinary path immediately advances.
    health.ingest_metadata_ready();
    if flags.stop_after_meta {
        if backfills.is_empty() {
            return Err("nothing to serve: no enrolled runtimes".to_string());
        }
        for entry in &mut backfills {
            let ledger = entry.ledger.as_mut().expect("a backfill owns its ledger");
            instrument
                .ledger_write(ledger, |ledger| ledger.flush())
                .map_err(|error| error.to_string())?;
        }

        // Nothing else will write in this mode: one precise drain of the
        // committed skims is the authoritative mirror state the app audits.
        let published = absorb_effects(&mut store, &hub)?;
        println!(
            "metadata ready — {published} mirror diffs published; complete-record streaming is paused (ctrl-c to stop)"
        );
        for entry in &backfills {
            println!(
                "{}: {} sessions, {} projects, {} matched files ({} skimmed, {} inventory-only, {} unidentified)",
                entry.name,
                entry.meta.sessions,
                entry.meta.projects,
                entry.meta.files_matched,
                entry.meta.files_skimmed,
                entry.meta.files_inventory_only,
                entry.meta.files_unidentified,
            );
        }
        while !stop.load(Ordering::SeqCst) {
            std::thread::sleep(CONSENT_POLL);
        }

        let _ = shutdown_tx.send(true);
        match tokio_runtime.block_on(server) {
            Ok(Ok(())) => {}
            Ok(Err(error)) => return Err(format!("server: {error}")),
            Err(error) => return Err(format!("server task: {error}")),
        }
        search_worker.stop();
        if flags.profile {
            println!("\n== profile ==\n{}", instrument.report());
        }
        return Ok(());
    }

    // Phase two: the cold scheduler — live ▸ boost ▸ starter ▸ completion,
    // round-robin across runtimes inside the cold lanes. Starter, completion,
    // and live passes carry the slice budget so every session opens fast and
    // everything stays preemptible; a paused file requeues into completion;
    // boost requests jump the cold lanes and run unbudgeted so the opened
    // session finishes outright; watcher hints move not-yet-consumed work to
    // the live lane but never recreate a completed cold obligation. Later
    // growth belongs to the live runner, so continuous writers cannot block
    // canonical publication. Virtual snapshot members coalesce into one
    // physical pass. Per-file durability and the graceful-stop boundary are
    // unchanged.
    let audit_free = IngestOptions {
        audit_files_per_run: 0,
        ..opts.clone()
    };
    let sliced = IngestOptions {
        starter_budget: slice_budget,
        ..audit_free.clone()
    };
    // The plan: meta's settled certainty as data. Ownership comes from the
    // canonical rows the skims just committed; sessions the store cannot
    // prove an owner for stay in the honest unassigned bucket. The board
    // folds pass outcomes over it, and the app polls the fold off
    // /api/status from the first pass on.
    let rosters: Vec<Vec<SessionFile>> =
        backfills.iter().map(|entry| entry.roster.clone()).collect();
    let ownership: std::collections::BTreeMap<String, Option<String>> =
        queries::session_list(store.conn(), None, i64::MAX)
            .map_err(|error| format!("reading settled session ownership: {error}"))?
            .into_iter()
            .map(|session| (session.id, session.project_id))
            .collect();
    let mut engine = ColdEngine::new(rosters, &ownership, slice_budget, started);
    engine.publish_projects(&health);
    engine.bootstrap_whole_file(&store)?;
    let mut cx = ColdCx {
        store: &mut store,
        hub: &hub,
        health: &health,
        search_worker: &search_worker,
        instrument: &instrument,
        boost: &boost,
        stop: &stop,
        opts: &opts,
        sliced: &sliced,
        audit_free: &audit_free,
        backfills: &mut backfills,
    };
    engine.run(&mut cx)?;
    engine.audit(&mut cx, &mut failures)?;
    let interrupted = stop.load(Ordering::SeqCst);
    if !failures.is_empty() {
        return Err(failures.join("; "));
    }

    let mut runners = Vec::new();
    let mut absorbed = 0_u64;
    if !interrupted {
        if backfills.is_empty() {
            return Err("nothing to serve: no enrolled runtimes".to_string());
        }
        for entry in &mut backfills {
            let ledger = entry.ledger.as_mut().expect("a backfill owns its ledger");
            instrument
                .ledger_write(ledger, |ledger| ledger.flush())
                .map_err(|error| error.to_string())?;
        }

        if progressive {
            instrument
                .finish_progressive_load(&mut store)
                .map_err(|error| format!("finalizing progressive projection: {error}"))?;
        }
        println!(
            "milestone: canonical-ready elapsed_s={:.2}",
            started.elapsed().as_secs_f64(),
        );

        // Canonical reads are public now. Let the initial detached generation
        // finish without competing source scans or live SQLite writes; the
        // watchers created before backfill retain every intervening hint.
        await_initial_search(&mut store, &search, &search_worker, &stop)?;
        // The milestone is a claim the sweep harness trusts: a failed or
        // interrupted build must not masquerade as a fast search-ready.
        match search.state().readiness {
            SearchReadiness::Ready => println!(
                "milestone: search-ready elapsed_s={:.2}",
                started.elapsed().as_secs_f64(),
            ),
            SearchReadiness::Failed => println!(
                "milestone: search-failed elapsed_s={:.2}",
                started.elapsed().as_secs_f64(),
            ),
            _ => {}
        }

        // Backfill over: absorb the finalize gate's closing effects while
        // the scope is still the cold one, then widen row observation to
        // every session for the live loop.
        for mut entry in backfills {
            if stop.load(Ordering::SeqCst) {
                break;
            }
            let ledger = entry.ledger.take().expect("a backfill owns its ledger");
            let identity = targets
                .iter()
                .find(|target| target.id() == entry.runtime.id)
                .map(RuntimeTarget::identity)
                .unwrap_or_else(|| {
                    RuntimeIdentity::new(&entry.runtime.id, entry.adapter.kind(), None)
                });
            let runner = LiveRunner::resume_from_roster(
                entry.adapter,
                entry.runtime,
                entry.root,
                Box::new(entry.events),
                entry.roster,
                opts.clone(),
                live.clone(),
                Some(ledger),
            );
            runners.push((identity, runner, 0_u64));
        }
        let published = absorb_effects(&mut store, &hub)?;
        store.observe_effects();
        health.ingest_live();
        println!("live — {published} closing mirror diffs published");

        // The ingest loop: ticks ARE the existing phases; the drain feeds
        // the hub strictly after each batch's commit.
        let mut remaining = flags.ticks;
        let mut more_hints = false;
        while !stop.load(Ordering::SeqCst) {
            match &mut remaining {
                Some(0) => break,
                Some(count) => *count -= 1,
                None => {}
            }
            if !more_hints {
                wake.wait(live.tick);
            }
            more_hints = false;
            for (identity, runner, _) in &mut runners {
                if stop.load(Ordering::SeqCst) {
                    break;
                }
                more_hints |= runner
                    .tick_ready(&mut store, &instrument)
                    .map_err(|error| error.to_string())?;
                update_live_health(&health, identity, runner);
                absorbed += absorb_effects(&mut store, &hub)? as u64;
            }
            search_worker.wake();
            acknowledge_search(&mut store, &search_worker)?;
            absorbed += absorb_effects(&mut store, &hub)? as u64;
        }
    } else {
        println!(
            "interrupted during backfill — committed progress retained for resume, shutting down"
        );
    }

    let _ = shutdown_tx.send(true);
    match tokio_runtime.block_on(server) {
        Ok(Ok(())) => {}
        Ok(Err(error)) => return Err(format!("server: {error}")),
        Err(error) => return Err(format!("server task: {error}")),
    }
    for (identity, runner, _) in &runners {
        let report = runner.report();
        println!(
            "{}: {} ticks — {} events, {} polls, {} rows live",
            identity_name(identity),
            report.ticks,
            report.events,
            report.polls,
            report.stream.stats.messages_inserted,
        );
    }
    acknowledge_search(&mut store, &search_worker)?;
    search_worker.stop();
    println!("hub: {absorbed} live items fanned out");
    if flags.profile {
        println!("\n== profile ==\n{}", instrument.report());
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn run_resumed_fresh_serve(
    flags: &Flags,
    db: &Path,
    targets: &[RuntimeTarget],
    consented: &BTreeSet<String>,
    generation_consented: &BTreeSet<String>,
    opts: &IngestOptions,
    live: &LiveOptions,
    instrument: &Arc<Instrument>,
    mut store: Store,
    temp: tempfile::TempPath,
    ledger_stage: ServeColdStage,
    projection_reader: &Arc<ProjectionReader>,
    health: &Arc<DaemonHealth>,
    hub: &Arc<Hub>,
    search_worker: &Fts5SearchWorker,
    stop: &Arc<AtomicBool>,
    started: Instant,
) -> Result<(Store, u64), String> {
    store.observe_meta_effects();
    let wake = SourceWake::default();
    let stage_root = ledger_stage.path().to_path_buf();
    let cold_opts = IngestOptions {
        starter_budget: None,
        audit_files_per_run: 0,
        cancel: Some(stop.clone()),
        ..opts.clone()
    };
    let mut events = BTreeMap::new();
    let mut applied = Vec::new();
    let mut failures = Vec::new();
    let mut handled = BTreeSet::new();
    let board = Arc::new(rosetta_server::ProgressBoard::new());
    let status_board = board.clone();
    health.ingest_project_source(Arc::new(move || status_board.snapshot()));

    for target in consented_targets(targets, consented) {
        let adapter = target.adapter;
        let name = target.name();
        handled.insert(target.id().to_string());
        if !target.active() {
            return Err(format!(
                "cannot resume {name} cold generation: data root {} is unavailable",
                target.root.display()
            ));
        }
        if stop.load(Ordering::SeqCst) {
            return Err("cold resume cancelled before runtime replay".to_string());
        }

        // Watch before touching either recovery evidence or today's inventory.
        // Hints queue while sealed generations rebuild into the fresh RAM view.
        let watcher = NotifyEvents::watch_with_wake(&target.root, &wake)
            .map_err(|error| error.to_string())?;
        let runtime = target.enabled_runtime();
        let ledger_dir = ledger_dir(&stage_root, &runtime);
        let baseline = instrument.snapshot();
        health.ingest_stream(&runtime.id, counter_source(instrument, baseline));
        health.ingest_phase(rosetta_server::IngestPhase::Restoring);

        let unsealed = if has_ledger_segments(&ledger_dir)? {
            let report = rebuild_runtime_with_options(
                adapter,
                &runtime,
                &ledger_dir,
                &mut store,
                instrument,
                RebuildOptions {
                    flush_reconciliation: false,
                    allow_unsealed_sources: true,
                    cancel: Some(stop.clone()),
                    ..RebuildOptions::default()
                },
            )
            .map_err(|error| format!("replaying {name} staged ledger: {error}"))?;
            if report.hard_errors != 0 {
                return Err(format!(
                    "{name} staged ledger replay had {} hard normalization errors",
                    report.hard_errors
                ));
            }
            if report.torn_segments != 0 {
                println!(
                    "{name}: recovered {} sealed ledger segments ({} torn tail ignored)",
                    report.segments, report.torn_segments
                );
            } else {
                println!(
                    "{name}: recovered {} sealed ledger segments",
                    report.segments
                );
            }
            report.unsealed_sources
        } else {
            BTreeMap::new()
        };

        // Current bounded metadata settles the roster after replay. A raw-only
        // source must still be present: otherwise there is no complete evidence
        // from which this generation can ever be published.
        let mut ledger = open_runtime_ledger(&stage_root, &runtime)?
            .defer_sync_until_flush()
            .into_async(4);
        health.ingest_skim(
            &runtime.id,
            counter_source(instrument, instrument.snapshot()),
        );
        let (meta, roster) = meta_ingest_inventory_into(
            adapter,
            &runtime,
            &target.root,
            &mut store,
            &target.census.inventory,
            target.census.walk_truncated,
            &cold_opts,
            instrument,
            Some(&mut ledger),
        )
        .map_err(|error| format!("{name} resume metadata: {error}"))?;
        for (path, (generation, source)) in &unsealed {
            let file = roster
                .iter()
                .find(|file| file.relative_path == *path)
                .ok_or_else(|| {
                    format!("cannot finish {name}: interrupted source {path} is no longer present")
                })?;
            if file.stream_id != source.stream_id
                || file.session_id != source.session_id
                || file.native_session_id != source.native_session_id
            {
                return Err(format!(
                    "cannot finish {name}: interrupted source {path} changed identity"
                ));
            }
            reserve_unsealed_generation(
                adapter,
                &runtime,
                &mut store,
                file,
                *generation,
                instrument,
            )
            .map_err(|error| format!("reserving {path} generation: {error}"))?;
        }

        health.ingest_skim_done(&runtime.id, meta.sessions as u64, meta.files_skimmed as u64);
        let ownership = queries::session_list(store.conn(), None, i64::MAX)
            .map_err(|error| format!("reading resumed progress ownership: {error}"))?
            .into_iter()
            .map(|session| (session.id, session.project_id))
            .collect();
        let mut observed_opts = cold_opts.clone();
        observed_opts.progress = Some(crate::cold_progress::ColdProgress::observe(
            &runtime.id,
            &roster,
            &ownership,
            board.clone(),
            health.clone(),
            false,
        ));
        health.ingest_stream(
            &runtime.id,
            counter_source(instrument, instrument.snapshot()),
        );
        let stream = stream_backfill_into(
            adapter,
            &runtime,
            &target.root,
            &mut store,
            &roster,
            &observed_opts,
            instrument,
            Some(&mut ledger),
        )
        .map_err(|error| format!("{name} resume source catch-up: {error}"))?;
        record_backfill_failures(
            &name,
            target.census.walk_truncated,
            &meta,
            &stream,
            &mut failures,
        );
        instrument
            .ledger_write(&mut ledger, |ledger| ledger.flush())
            .map_err(|error| error.to_string())?;
        absorb_effects(&mut store, hub)?;
        let session_ids = queries::session_list(store.conn(), None, i64::MAX)
            .map_err(|error| format!("reading {name} resumed sessions: {error}"))?
            .into_iter()
            .filter(|session| session.runtime_id == runtime.id)
            .map(|session| session.id)
            .collect::<Vec<_>>();
        hub.invalidate_sessions(session_ids.iter().map(String::as_str), now_ms());
        for session_id in &session_ids {
            board.session_done(
                ownership.get(session_id).and_then(Option::as_deref),
                session_id,
            );
        }
        health.ingest_done(&runtime.id, stream.payload_bytes, stream.records);
        events.insert(runtime.id.clone(), watcher);
        applied.push(AppliedColdRuntime {
            adapter,
            runtime,
            identity: target.identity(),
            root: target.root.clone(),
            roster,
            ledger: Some(ledger),
        });
    }

    // Every runtime the generation captured must have been handled above;
    // `generation_consent` already refused an enrolled runtime the file no
    // longer names, so a miss here is a kind the selection dropped.
    let unavailable = generation_consented
        .difference(&handled)
        .filter_map(|id| targets.iter().find(|target| target.id() == id))
        .map(RuntimeTarget::name)
        .collect::<Vec<_>>();
    if !unavailable.is_empty() {
        return Err(format!(
            "cold generation names unavailable adapters: {}",
            unavailable.join(", ")
        ));
    }
    if !failures.is_empty() {
        return Err(failures.join("; "));
    }
    if applied.is_empty() {
        return Err("nothing to resume: cold generation has no enrolled runtimes".to_string());
    }

    instrument
        .flush_bulk_reconciliation(&mut store)
        .map_err(|error| format!("reconciling resumed projection: {error}"))?;
    for output in &mut applied {
        drop(output.ledger.take());
    }
    let mut store = publish_fresh_ram_projection(
        store,
        temp,
        db,
        ledger_stage,
        projection_reader,
        instrument,
        health,
    )?;
    println!(
        "milestone: canonical-ready elapsed_s={:.2}",
        started.elapsed().as_secs_f64()
    );

    let public_ledgers = ledger_root(flags, db);
    let mut runners = Vec::with_capacity(applied.len());
    for output in applied {
        let watcher = events
            .remove(&output.runtime.id)
            .ok_or_else(|| format!("lost {} resume watcher", identity_name(&output.identity)))?;
        let ledger = open_runtime_ledger(&public_ledgers, &output.runtime)?;
        let runner = LiveRunner::resume_from_roster(
            output.adapter,
            output.runtime,
            output.root,
            Box::new(watcher),
            output.roster,
            opts.clone(),
            live.clone(),
            Some(ledger),
        );
        runners.push((output.identity, runner));
    }
    let absorbed = drive_live_runners(
        flags,
        &mut store,
        &mut runners,
        &wake,
        live.tick,
        instrument,
        health,
        hub,
        search_worker,
        stop,
    )?;
    Ok((store, absorbed))
}

fn has_ledger_segments(path: &Path) -> Result<bool, String> {
    let entries = match std::fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(format!("listing ledger {}: {error}", path.display())),
    };
    for entry in entries {
        let entry = entry.map_err(|error| format!("listing ledger {}: {error}", path.display()))?;
        if entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.ends_with(".rledger"))
        {
            return Ok(true);
        }
    }
    Ok(false)
}

#[allow(clippy::too_many_arguments)]
fn run_fresh_fast_serve(
    flags: &Flags,
    db: &std::path::Path,
    targets: &[RuntimeTarget],
    consented: &BTreeSet<String>,
    opts: &IngestOptions,
    live: &LiveOptions,
    instrument: &Arc<Instrument>,
    mut store: Store,
    temp: tempfile::TempPath,
    ledger_stage: ServeColdStage,
    projection_reader: &Arc<ProjectionReader>,
    health: &Arc<DaemonHealth>,
    hub: &Arc<Hub>,
    search_worker: &Fts5SearchWorker,
    stop: &Arc<AtomicBool>,
    started: Instant,
) -> Result<(Store, u64), String> {
    store
        .use_precomputed_bulk_ordinals()
        .map_err(|error| format!("enabling precomputed cold ordinals: {error}"))?;
    store.observe_meta_effects();

    // Watch before the first skim. Source changes during the RAM build queue
    // as hints and are replayed by the live runner after durable publication.
    let wake = SourceWake::default();
    let mut events = std::collections::BTreeMap::new();
    let mut planned = Vec::new();
    let cold_opts = IngestOptions {
        cancel: Some(stop.clone()),
        ..opts.clone()
    };
    for target in consented_targets(targets, consented) {
        if !target.active() {
            continue;
        }
        if stop.load(Ordering::SeqCst) {
            return Err("stopped before cold metadata completed".into());
        }
        let adapter = target.adapter;
        let name = target.name();
        let watcher = NotifyEvents::watch_with_wake(&target.root, &wake)
            .map_err(|error| error.to_string())?;
        let worker_instrument = Arc::new(Instrument::new());
        let skim_baseline = worker_instrument.snapshot();
        health.ingest_skim(
            target.id(),
            counter_source(&worker_instrument, skim_baseline),
        );
        let plan = plan_parallel_cold_runtime(ColdCaptureRequest {
            adapter,
            runtime: target.runtime.clone(),
            identity: target.identity(),
            root: target.root.clone(),
            inventory: target.census.inventory.clone(),
            discovery_truncated: target.census.walk_truncated,
            ledger_root: ledger_stage.path().to_path_buf(),
            opts: cold_opts.clone(),
            write_raw_ledger: true,
            async_raw_ledger: true,
            instrument: worker_instrument,
        })?;

        // This is the immediate-public seam: only source-free canonical
        // identity rows cross it. The planner keeps all payload/source state
        // in RAM until the runtime closes.
        for batch in plan
            .planner
            .planned_metadata_batches()
            .map_err(|error| format!("reading {name} metadata plan: {error}"))?
        {
            instrument
                .apply(&mut store, &batch)
                .map_err(|error| format!("publishing {name} metadata: {error}"))?;
        }
        absorb_effects(&mut store, hub)?;
        health.ingest_skim_done(
            target.id(),
            plan.meta.sessions as u64,
            plan.meta.files_skimmed as u64,
        );
        println!(
            "{name}: metadata public — {} sessions, {} projects, {} streamable files",
            plan.meta.sessions,
            plan.meta.projects,
            plan.files.len(),
        );
        events.insert(target.id().to_string(), watcher);
        planned.push(plan);
    }
    if planned.is_empty() {
        return Err("nothing to serve: no enrolled runtimes".to_string());
    }
    health.ingest_metadata_ready();
    println!(
        "milestone: metadata-ready elapsed_s={:.2}",
        started.elapsed().as_secs_f64()
    );

    let ownership = queries::session_list(store.conn(), None, i64::MAX)
        .map_err(|error| format!("reading cold progress ownership: {error}"))?
        .into_iter()
        .map(|session| (session.id, session.project_id))
        .collect();
    let board = Arc::new(rosetta_server::ProgressBoard::new());
    let status_board = board.clone();
    health.ingest_project_source(Arc::new(move || status_board.snapshot()));
    for plan in &mut planned {
        plan.opts.progress = Some(crate::cold_progress::ColdProgress::observe(
            &plan.runtime.id,
            &plan.files,
            &ownership,
            board.clone(),
            health.clone(),
            true,
        ));
    }

    // Every producer owns one planner and ledger. The consumer is the only
    // canonical writer and admits completion-order results, so small agents
    // become browseable while a large agent is still parsing.
    for plan in &planned {
        let baseline = plan.instrument.snapshot();
        health.ingest_stream(&plan.runtime.id, counter_source(&plan.instrument, baseline));
    }
    let mut failures = Vec::new();
    let mut applied = std::thread::scope(|scope| {
        let worker_count = planned.len();
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut handles = Vec::with_capacity(worker_count);
        for plan in planned {
            let sender = sender.clone();
            handles.push(scope.spawn(move || {
                let result = finish_parallel_cold_runtime(plan);
                sender
                    .send(result)
                    .map_err(|_| "fresh cold consumer stopped".to_string())
            }));
        }
        drop(sender);

        let mut outputs = Vec::with_capacity(worker_count);
        for _ in 0..worker_count {
            let mut captured = receiver
                .recv()
                .map_err(|_| "fresh cold capture channel closed".to_string())??;
            let mut runtime_failures = Vec::new();
            let name = identity_name(&captured.identity);
            record_backfill_failures(
                &name,
                captured.discovery_truncated,
                &captured.meta,
                &captured.stream,
                &mut runtime_failures,
            );
            if !runtime_failures.is_empty() {
                failures.extend(runtime_failures);
                continue;
            }
            let runtime_id = captured.runtime.id.clone();
            let payload_bytes = captured.stream.payload_bytes;
            let records = captured.stream.records;
            health.ingest_runtime_stage(&runtime_id, rosetta_server::BackfillState::Applying);
            captured.prepared.mark_metadata_preapplied();
            let output = apply_captured_cold_runtime(
                flags,
                captured,
                &mut store,
                instrument,
                &mut failures,
            )?;
            absorb_effects(&mut store, hub)?;
            let session_ids = queries::session_list(store.conn(), None, i64::MAX)
                .map_err(|error| format!("reading {name} sessions after cold apply: {error}"))?
                .into_iter()
                .filter(|session| session.runtime_id == output.runtime.id)
                .map(|session| session.id)
                .collect::<Vec<_>>();
            hub.invalidate_sessions(session_ids.iter().map(String::as_str), now_ms());
            for session_id in &session_ids {
                board.session_done(
                    ownership.get(session_id).and_then(Option::as_deref),
                    session_id,
                );
            }
            health.ingest_done(&runtime_id, payload_bytes, records);
            println!(
                "{name}: canonical RAM public — {records} records, {} sessions",
                session_ids.len()
            );
            outputs.push(output);
        }
        for handle in handles {
            handle
                .join()
                .map_err(|_| "fresh cold capture worker panicked".to_string())??;
        }
        Ok::<_, String>(outputs)
    })?;
    if !failures.is_empty() {
        return Err(failures.join("; "));
    }

    health.ingest_phase(rosetta_server::IngestPhase::Finalizing);
    drop(ownership);
    drop(board);
    // Async ledger workers have overlapped almost all payload IO. One
    // pre-publication barrier now drains and fsyncs each runtime, after which
    // no staged writer survives the directory rename.
    for output in &mut applied {
        if let Some(ledger) = output.ledger.as_mut() {
            instrument
                .ledger_write(ledger, |ledger| ledger.flush())
                .map_err(|error| error.to_string())?;
        }
    }
    instrument
        .flush_bulk_reconciliation(&mut store)
        .map_err(|error| format!("reconciling whole projection: {error}"))?;
    for output in &mut applied {
        drop(output.ledger.take());
    }

    let mut store = publish_fresh_ram_projection(
        store,
        temp,
        db,
        ledger_stage,
        projection_reader,
        instrument,
        health,
    )?;
    println!(
        "milestone: canonical-ready elapsed_s={:.2}",
        started.elapsed().as_secs_f64()
    );

    // Reattach live ledger handles at their public paths. Watchers and
    // rosters were armed before capture, so no source write can disappear in
    // the RAM-to-disk handoff.
    let public_ledgers = ledger_root(flags, db);
    let mut runners = Vec::with_capacity(applied.len());
    for output in applied {
        let watcher = events
            .remove(&output.runtime.id)
            .ok_or_else(|| format!("lost {} cold watcher", identity_name(&output.identity)))?;
        let ledger = open_runtime_ledger(&public_ledgers, &output.runtime)?;
        let runner = LiveRunner::resume_from_roster(
            output.adapter,
            output.runtime,
            output.root,
            Box::new(watcher),
            output.roster,
            opts.clone(),
            live.clone(),
            Some(ledger),
        );
        runners.push((output.identity, runner));
    }
    let absorbed = drive_live_runners(
        flags,
        &mut store,
        &mut runners,
        &wake,
        live.tick,
        instrument,
        health,
        hub,
        search_worker,
        stop,
    )?;
    Ok((store, absorbed))
}

#[allow(clippy::too_many_arguments)]
fn run_warm_live_serve(
    flags: &Flags,
    targets: &[RuntimeTarget],
    consented: &BTreeSet<String>,
    opts: &IngestOptions,
    live: &LiveOptions,
    instrument: &Arc<Instrument>,
    mut store: Store,
    public_ledgers: &Path,
    health: &Arc<DaemonHealth>,
    hub: &Arc<Hub>,
    search_worker: &Fts5SearchWorker,
    stop: &Arc<AtomicBool>,
) -> Result<(Store, u64), String> {
    let wake = SourceWake::default();
    let mut runners = Vec::new();
    for target in consented_targets(targets, consented) {
        let adapter = target.adapter;
        let name = target.name();
        if !target.active() {
            eprintln!(
                "{name}: enrolled data root is unavailable; standing canonical data remains public"
            );
            continue;
        }
        if stop.load(Ordering::SeqCst) {
            break;
        }

        // Watch before the bounded current inventory is skimmed. The forced
        // catch-up below then combines queued hints with cursor-relative stat
        // evidence, closing both the offline and scan/watch races.
        let watcher = NotifyEvents::watch_with_wake(&target.root, &wake)
            .map_err(|error| error.to_string())?;
        let runtime = target.enabled_runtime();
        let mut ledger = open_runtime_ledger(public_ledgers, &runtime)?;
        let (meta, roster) = meta_ingest_inventory_into(
            adapter,
            &runtime,
            &target.root,
            &mut store,
            &target.census.inventory,
            target.census.walk_truncated,
            opts,
            instrument,
            Some(&mut ledger),
        )
        .map_err(|error| format!("{name} warm metadata catch-up: {error}"))?;
        if meta.walk_truncated || meta.files_unidentified > 0 || meta.files_vanished > 0 {
            eprintln!(
                "{name}: warm inventory changed during scan (truncated={}, unidentified={}, vanished={}); live admission will retry",
                meta.walk_truncated, meta.files_unidentified, meta.files_vanished
            );
        }
        let runner = LiveRunner::resume_from_roster(
            adapter,
            runtime,
            target.root.clone(),
            Box::new(watcher),
            roster,
            opts.clone(),
            live.clone(),
            Some(ledger),
        );
        runners.push((target.identity(), runner));
    }

    let absorbed = drive_live_runners(
        flags,
        &mut store,
        &mut runners,
        &wake,
        live.tick,
        instrument,
        health,
        hub,
        search_worker,
        stop,
    )?;
    Ok((store, absorbed))
}

#[allow(clippy::too_many_arguments)]
fn drive_live_runners(
    flags: &Flags,
    store: &mut Store,
    runners: &mut [(RuntimeIdentity, LiveRunner<'static>)],
    wake: &SourceWake,
    idle_wait: Duration,
    instrument: &Instrument,
    health: &DaemonHealth,
    hub: &Hub,
    search_worker: &Fts5SearchWorker,
    stop: &AtomicBool,
) -> Result<u64, String> {
    store.observe_meta_effects();
    store.observe_effects();
    health.ingest_live();
    let mut absorbed = absorb_effects(store, hub)? as u64;

    // One non-blocking recovery pass is part of entering live mode. It catches
    // offline growth and the private-public handoff even if the OS watcher
    // dropped or coalesced the corresponding event.
    for (identity, runner) in runners.iter_mut() {
        if stop.load(Ordering::SeqCst) {
            break;
        }
        runner
            .catch_up(store, instrument)
            .map_err(|error| format!("{} startup catch-up: {error}", identity_name(identity)))?;
        update_live_health(health, identity, runner);
        search_worker.wake();
        acknowledge_search(store, search_worker)?;
        absorbed += absorb_effects(store, hub)? as u64;
    }

    let mut remaining = flags.ticks;
    let mut more_hints = false;
    while !stop.load(Ordering::SeqCst) {
        match &mut remaining {
            Some(0) => break,
            Some(count) => *count -= 1,
            None => {}
        }
        if !more_hints {
            wake.wait(idle_wait);
        }
        more_hints = false;
        for (identity, runner) in runners.iter_mut() {
            if stop.load(Ordering::SeqCst) {
                break;
            }
            more_hints |= runner
                .tick_ready(store, instrument)
                .map_err(|error| error.to_string())?;
            update_live_health(health, identity, runner);
            // Publish this runtime's commits before doing work for the next.
            absorbed += absorb_effects(store, hub)? as u64;
        }
        search_worker.wake();
        acknowledge_search(store, search_worker)?;
        absorbed += absorb_effects(store, hub)? as u64;
    }
    for (identity, runner) in runners.iter() {
        let report = runner.report();
        println!(
            "{}: {} ticks — {} events, {} polls, {} rows live",
            identity_name(identity),
            report.ticks,
            report.events,
            report.polls,
            report.stream.stats.messages_inserted + report.stream.stats.messages_updated,
        );
    }
    Ok(absorbed)
}

fn update_live_health(health: &DaemonHealth, identity: &RuntimeIdentity, runner: &LiveRunner<'_>) {
    let report = runner.report();
    health.update(
        identity,
        RuntimeTick {
            ticks: report.ticks,
            events: report.events,
            polls: report.polls,
            rescans: report.rescans,
            admissions: report.admissions,
            live_rows: report.stream.stats.messages_inserted + report.stream.stats.messages_updated,
        },
        now_ms(),
    );
}

#[allow(clippy::too_many_arguments)]
fn publish_fresh_ram_projection(
    mut store: Store,
    temp: tempfile::TempPath,
    db: &std::path::Path,
    mut ledger_stage: ServeColdStage,
    projection_reader: &ProjectionReader,
    instrument: &Instrument,
    health: &DaemonHealth,
) -> Result<Store, String> {
    health.ingest_phase(rosetta_server::IngestPhase::Finalizing);
    instrument
        .finish_bulk_load(&mut store)
        .map_err(|error| format!("finalizing canonical RAM projection: {error}"))?;
    health.ingest_phase(rosetta_server::IngestPhase::Publishing);
    instrument.time(Metric::PublishNanos, || {
        store
            .conn()
            .backup(rusqlite::DatabaseName::Main, &*temp, None)
            .map_err(|error| format!("backing up canonical RAM projection: {error}"))?;
        let finished = rusqlite::Connection::open(&*temp)
            .map_err(|error| format!("opening staged durable projection: {error}"))?;
        make_single_file(&finished, "staged durable projection")?;
        drop(finished);
        std::fs::File::open(&*temp)
            .and_then(|file| file.sync_all())
            .map_err(|error| format!("syncing staged durable projection: {error}"))?;

        ledger_stage.publish()?;
        install_rebuilt_projection(db, temp.as_ref())?;
        let _installed = temp.keep();

        // The switch precedes dropping the writer. Any request that captured
        // the old target owns its RAM keeper; every later request opens disk.
        projection_reader.publish_file(db.to_path_buf());
        ledger_stage.complete()?;
        Ok::<(), String>(())
    })?;
    drop(store);
    Store::open(db).map_err(|error| format!("opening published projection: {error}"))
}
