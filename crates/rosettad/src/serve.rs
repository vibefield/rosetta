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
//! chosen in the app while the daemon waits. Metadata effects are observed
//! from process start — ids only, cheap on the cold paths — and the writer
//! thread drains them into the mirror between passes, fanning precise
//! `/ws/meta` diffs with no polling cadence. The cold scheduler pops one
//! budgeted pass at a time so the boost ledger and the live lane's watcher
//! hints can jump ahead of the rotation. Row-effects observation runs
//! scoped during the backfill — armed per pass with the sessions readers
//! hold open, so an open transcript streams in while everything unwatched
//! still pays no per-row mutation log — and widens to every session at the
//! live handoff.

use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use rosetta_capture::NotifyEvents;
use rosetta_pipeline::{
    IngestOptions, Instrument, InstrumentSnapshot, LiveOptions, LiveRunner, Metric, SessionFile,
    SliceBudget, StreamReport,
};
use rosetta_search::{Fts5SearchWorker, Fts5SidecarSearch, SearchBackend, SearchReadiness};
use rosetta_server::{
    AppState, BoostLedger, ConsentGate, DaemonHealth, DiscoveryRow, Hub, HubConfig,
    IngestCounterSource, IngestCounters, RuntimeTick, router,
};
use rosetta_store::{Store, queries};

use crate::engine::{
    BackfillRuntime, ColdCx, ColdEngine, StreamMeter, absorb_effects, acknowledge_search, now_ms,
};
use crate::{
    ledger_root, open_ledger, open_progressive_backfill, parse_flags, projection_lock, selected,
};

const DEFAULT_ADDR: &str = "127.0.0.1:4640";
const CONSENT_POLL: Duration = Duration::from_millis(200);

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
            bytes_read: delta.get(Metric::BytesRead),
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
    let _projection_lock = projection_lock(&db)?;

    // Serve is the progressive first-run owner: the installed projection is
    // public from its first committed skim. `rebuild` and finite `ingest`
    // retain their private atomic path.
    let mut store = open_progressive_backfill(&db)?;
    let progressive = store.is_progressive();
    let public_ledgers = ledger_root(&flags, &db);

    // The transport comes up before any ingest work. A returning user's
    // mirror warms from the standing projection so the room is instantly
    // whole; a fresh install serves empty lists and honest meters.
    let health = Arc::new(DaemonHealth::new(now_ms()));
    let hub = Arc::new(Hub::new(HubConfig::default(), now_ms() as u64));
    let consent = Arc::new(ConsentGate::new());
    let boost = Arc::new(BoostLedger::new());
    let warmed = hub.warm(store.conn()).map_err(|error| error.to_string())?;
    let search = Fts5SidecarSearch::new(db.clone());
    let search_worker = Fts5SearchWorker::start(search.clone());

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
        .with_daemon_health(health.clone())
        .with_consent(consent.clone())
        .with_boost(boost.clone());
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
    println!("serving http://{local} — {warmed} mirror rows warm (ctrl-c to stop)");

    // Mid-backfill readers ride the metadata effects seam: every committed
    // batch names exactly the identity rows it may have moved, and the
    // writer thread drains them into the mirror between passes — precise
    // diffs to /ws/meta with no polling cadence, no second read connection
    // pinning the cold WAL, and no hub mutation off the writer thread.
    // Budgeted slices keep passes short, so the drain cadence is bounded by
    // one slice.
    store.observe_meta_effects();

    // The census fills the roll-call board; consent decides what is read.
    let discoveries = rosetta_pipeline::discover(&adapters, &flags.home, &opts, &instrument)
        .map_err(|error| error.to_string())?;
    // Consent persists as the projection's `enabled` flags — enrollment
    // happens once, and restarts proceed without asking again.
    let persisted: BTreeSet<String> = rosetta_store::queries::all_runtimes(store.conn())
        .map_err(|error| format!("reading enrolled runtimes: {error:?}"))?
        .into_iter()
        .filter(|runtime| runtime.enabled)
        .map(|runtime| runtime.kind)
        .collect();
    let installed: BTreeSet<String> = adapters
        .iter()
        .zip(&discoveries)
        .filter(|(_, discovery)| discovery.installed)
        .map(|(adapter, _)| adapter.kind().to_string())
        .collect();
    let mut consented: BTreeSet<String> = persisted.intersection(&installed).cloned().collect();
    // `--agent` names kinds outright and `--all-agents` says everything
    // installed — both are consent from the command line, the headless
    // stand-in for the app's roll call.
    if !flags.agents.is_empty() || flags.all_agents {
        consented.extend(installed.iter().cloned());
    }
    consent.publish(
        adapters
            .iter()
            .zip(&discoveries)
            .map(|(adapter, discovery)| DiscoveryRow {
                kind: adapter.kind().to_string(),
                data_root: discovery.runtime.data_root.clone(),
                installed: discovery.installed,
                files: discovery.file_count as u64,
                bytes: discovery.total_bytes,
                newest_modified_ms: discovery.newest_modified_ms,
                enabled: consented.contains(adapter.kind()),
            })
            .collect(),
    );
    if consented.is_empty() {
        // Nothing enrolled and nobody said yes: the board is published and
        // NOTHING has been read. Wait for the roll call.
        let granted = consent.arm();
        health.ingest_await_consent();
        println!("no enrolled agents — choose in the app (or pass --all-agents)");
        loop {
            if stop.load(Ordering::SeqCst) {
                break;
            }
            match granted.recv_timeout(CONSENT_POLL) {
                Ok(kinds) => {
                    consented.extend(kinds);
                    break;
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
    }
    for (adapter, discovery) in adapters.iter().zip(&discoveries) {
        if discovery.installed && consented.contains(adapter.kind()) {
            // Meter denominators are the STREAMABLE census — the files the
            // skim and stream phases will actually read. The full footprint
            // (inventory streams included) belongs to the roll call, not to
            // a progress meter that could then never reach 100%.
            health.ingest_register(
                adapter.kind(),
                discovery.streamable_count as u64,
                discovery.streamable_bytes,
            );
        }
    }

    // Phase one: skim every enrolled runtime before reading any transcript.
    // This publishes the whole project/session roster first, so one large
    // runtime cannot hide every smaller runtime's lists.
    let mut backfills = Vec::new();
    let mut failures = Vec::new();
    for (adapter, discovery) in adapters.iter().zip(&discoveries) {
        if !discovery.installed {
            println!("{}: not installed, skipping", adapter.kind());
            continue;
        }
        if !consented.contains(adapter.kind()) {
            println!("{}: installed but not enrolled, skipping", adapter.kind());
            continue;
        }
        if stop.load(Ordering::SeqCst) {
            break;
        }
        let mut runtime = discovery.runtime.clone();
        runtime.enabled = true; // consent, persisted for the next start

        let events = NotifyEvents::watch(&discovery.root).map_err(|error| error.to_string())?;
        let mut ledger = open_ledger(&public_ledgers, adapter.kind())?;
        health.ingest_skim(
            adapter.kind(),
            counter_source(&instrument, instrument.snapshot()),
        );
        let (meta, files) = rosetta_pipeline::meta_ingest_into(
            *adapter,
            &runtime,
            &discovery.root,
            &mut store,
            &opts,
            &instrument,
            Some(&mut ledger),
        )
        .map_err(|error| error.to_string())?;
        let stream_meter = StreamMeter::new(&instrument);
        health.ingest_skim_done(
            adapter.kind(),
            meta.sessions as u64,
            meta.files_skimmed as u64,
        );
        // Every skim's lists fan out the moment its batch commits.
        absorb_effects(&mut store, &hub)?;
        let roster = files.clone();
        backfills.push(BackfillRuntime {
            adapter: *adapter,
            runtime,
            root: discovery.root.clone(),
            discovery_truncated: discovery.walk_truncated,
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
                entry.adapter.kind(),
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
    // session finishes outright; watcher hints feed the live lane so a
    // running session stays current during the backfill. Per-file durability
    // and the graceful-stop boundary are unchanged.
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
            runners.push((entry.adapter.kind(), runner, 0_u64));
        }
        let published = absorb_effects(&mut store, &hub)?;
        store.observe_effects();
        health.ingest_live();
        println!("live — {published} closing mirror diffs published");

        // The ingest loop: ticks ARE the existing phases; the drain feeds
        // the hub strictly after each batch's commit.
        let mut remaining = flags.ticks;
        while !stop.load(Ordering::SeqCst) {
            match &mut remaining {
                Some(0) => break,
                Some(count) => *count -= 1,
                None => {}
            }
            for (kind, runner, _) in &mut runners {
                runner
                    .tick(&mut store, &instrument)
                    .map_err(|error| error.to_string())?;
                let report = runner.report();
                health.update(
                    kind,
                    RuntimeTick {
                        ticks: report.ticks,
                        events: report.events,
                        polls: report.polls,
                        rescans: report.rescans,
                        admissions: report.admissions,
                        live_rows: report.stream.stats.messages_inserted
                            + report.stream.stats.messages_updated,
                    },
                    now_ms(),
                );
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
    for (kind, runner, _) in &runners {
        let report = runner.report();
        println!(
            "{kind}: {} ticks — {} events, {} polls, {} rows live",
            report.ticks, report.events, report.polls, report.stream.stats.messages_inserted,
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
