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
//! chosen in the app while the daemon waits. During the backfill the mirror
//! re-warms from the projection on a cadence and fans precise diffs to
//! `/ws/meta`, and the stream queue is driven one file at a time so the
//! boost ledger can move what the reader opened to the front. Effects
//! observation still starts only AFTER the cold backfill publishes — bulk
//! load never pays the mutation-log cost.

use std::collections::{BTreeSet, VecDeque};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use rosetta_capture::NotifyEvents;
use rosetta_pipeline::{
    IngestOptions, Instrument, InstrumentSnapshot, LiveOptions, LiveRunner, Metric, StreamReport,
};
use rosetta_server::{
    AppState, BoostLedger, ConsentGate, DaemonHealth, DiscoveryRow, Hub, HubConfig,
    IngestCounterSource, IngestCounters, RuntimeTick, router,
};
use rosetta_store::Store;

use crate::{
    ColdLedgerStage, ledger_root, open_for_backfill, open_ledger, parse_flags, projection_lock,
    publish_private_projection, record_backfill_failures, selected,
};

const DEFAULT_ADDR: &str = "127.0.0.1:4640";
const REWARM_INTERVAL: Duration = Duration::from_secs(2);
const CONSENT_POLL: Duration = Duration::from_millis(200);

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(0)
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
            bytes_read: delta.get(Metric::BytesRead),
            records: delta.get(Metric::RecordsFramed),
        }
    })
}

/// Fold one per-file stream report into the runtime's aggregate — the
/// fields the failure verdict and the operator's summary read.
fn fold_stream(total: &mut StreamReport, part: StreamReport) {
    total.files += part.files;
    total.files_missing += part.files_missing;
    total.batches += part.batches;
    total.records += part.records;
    total.oversized += part.oversized;
    total.parsed += part.parsed;
    total.unknown += part.unknown;
    total.hard_errors += part.hard_errors;
    total.generations_restarted += part.generations_restarted;
    total.files_audited += part.files_audited;
    total.conflicts += part.conflicts;
    total.retries_exhausted += part.retries_exhausted;
    total.files_incomplete += part.files_incomplete;
    total.sessions_indexed += part.sessions_indexed;
    total.siblings_healed += part.siblings_healed;
    total.stats.messages_inserted += part.stats.messages_inserted;
    total.stats.messages_updated += part.stats.messages_updated;
}

pub(crate) fn cmd_serve(args: &[String]) -> Result<(), String> {
    let flags = parse_flags(args)?;
    let db = flags.db.clone().ok_or("serve needs --db PATH")?;
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

    // Cold start: watch-before-scan backfill, exactly like `watch`.
    let (mut store, private_projection) = open_for_backfill(&db)?;
    let public_ledgers = ledger_root(&flags, &db);
    let defer_ledger_sync = private_projection.is_some();
    let ledger_stage = defer_ledger_sync
        .then(|| ColdLedgerStage::new(&public_ledgers))
        .transpose()?;
    let backfill_ledgers = ledger_stage
        .as_ref()
        .map_or(public_ledgers.as_path(), ColdLedgerStage::path);

    // The transport comes up before any ingest work. A returning user's
    // mirror warms from the standing projection so the room is instantly
    // whole; a fresh install serves empty lists and honest meters.
    let health = Arc::new(DaemonHealth::new(now_ms()));
    let hub = Arc::new(Hub::new(HubConfig::default(), now_ms() as u64));
    let consent = Arc::new(ConsentGate::new());
    let boost = Arc::new(BoostLedger::new());
    let mut warmed = 0_usize;
    if private_projection.is_none() {
        warmed = hub.warm(store.conn()).map_err(|error| error.to_string())?;
    }

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
    let state = AppState::new(hub.clone(), db.clone())
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

    // Mid-backfill readers: re-read the mirror from the projection on a
    // cadence and fan precise diffs to /ws/meta. Skims and per-file stream
    // commits become visible as they land. WAL keeps the read harmless; a
    // missing file (fresh install, private projection) skips the pass.
    let (rewarm_tx, rewarm_rx) = tokio::sync::watch::channel(false);
    let rewarm_task = tokio_runtime.spawn({
        let hub = hub.clone();
        let db = db.clone();
        let mut rewarm_rx = rewarm_rx;
        async move {
            let mut tick = tokio::time::interval(REWARM_INTERVAL);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    _ = tick.tick() => {
                        let hub = hub.clone();
                        let db = db.clone();
                        let _ = tokio::task::spawn_blocking(move || {
                            let Ok(conn) = rusqlite::Connection::open_with_flags(
                                &db,
                                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
                            ) else {
                                return;
                            };
                            let _ = hub.rewarm(&conn);
                        })
                        .await;
                    }
                    changed = rewarm_rx.changed() => {
                        if changed.is_err() || *rewarm_rx.borrow() {
                            break;
                        }
                    }
                }
            }
        }
    });

    // The census fills the roll-call board; consent decides what is read.
    let discoveries = rosetta_pipeline::discover(&adapters, &flags.home, &opts, &instrument)
        .map_err(|error| error.to_string())?;
    // Consent persists as the projection's `enabled` flags — enrollment
    // happens once, and restarts proceed without asking again.
    let persisted: BTreeSet<String> = if private_projection.is_none() {
        rosetta_store::queries::all_runtimes(store.conn())
            .map_err(|error| format!("reading enrolled runtimes: {error:?}"))?
            .into_iter()
            .filter(|runtime| runtime.enabled)
            .map(|runtime| runtime.kind)
            .collect()
    } else {
        BTreeSet::new()
    };
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
            health.ingest_register(
                adapter.kind(),
                discovery.file_count as u64,
                discovery.total_bytes,
            );
        }
    }

    // The backfill queue: skim (lists) before stream (transcripts), per
    // runtime, with live meters on the health surface.
    let mut pending_runners = Vec::new();
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
        let ledger = open_ledger(backfill_ledgers, adapter.kind())?;
        let mut ledger = if defer_ledger_sync {
            ledger.defer_sync_until_flush()
        } else {
            ledger
        };
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
        let stream_baseline = instrument.snapshot();
        health.ingest_stream(
            adapter.kind(),
            meta.sessions as u64,
            meta.files_skimmed as u64,
            counter_source(&instrument, stream_baseline.clone()),
        );

        // The stream queue, one file per call: the boost ledger reorders
        // between files (what the reader opened jumps the line), and a
        // graceful stop lands between files instead of minutes away. The
        // audit lane is disabled per file — a single-file slice would make
        // every call force-replay its file — and runs once at the end.
        let audit_free = IngestOptions {
            audit_files_per_run: 0,
            ..opts.clone()
        };
        let roster = files.clone();
        let mut queue: VecDeque<_> = files.into();
        let mut stream = StreamReport::default();
        while !queue.is_empty() {
            if stop.load(Ordering::SeqCst) {
                break;
            }
            for session_id in boost.drain().iter().rev() {
                if let Some(at) = queue.iter().position(|file| &file.session_id == session_id)
                    && let Some(file) = queue.remove(at)
                {
                    queue.push_front(file);
                }
            }
            let file = queue.pop_front().expect("checked non-empty");
            let report = rosetta_pipeline::stream_backfill_into(
                *adapter,
                &runtime,
                &discovery.root,
                &mut store,
                std::slice::from_ref(&file),
                &audit_free,
                &instrument,
                Some(&mut ledger),
            )
            .map_err(|error| error.to_string())?;
            fold_stream(&mut stream, report);
        }
        if stop.load(Ordering::SeqCst) {
            // A partial runtime earns no verdict and starts no runner.
            break;
        }
        // One audit-enabled pass over the whole roster: everything is
        // checkpointed by now, so this only performs the run's single
        // rotating forced replay — the audit lane's contract, unchanged.
        let report = rosetta_pipeline::stream_backfill_into(
            *adapter,
            &runtime,
            &discovery.root,
            &mut store,
            &roster,
            &opts,
            &instrument,
            Some(&mut ledger),
        )
        .map_err(|error| error.to_string())?;
        fold_stream(&mut stream, report);

        let stream_delta = instrument.snapshot().delta_since(&stream_baseline);
        health.ingest_done(
            adapter.kind(),
            stream_delta.get(Metric::BytesRead),
            stream_delta.get(Metric::RecordsFramed),
        );
        println!(
            "{}: backfilled {} sessions ({} rows) — watching {}",
            adapter.kind(),
            meta.sessions,
            stream.stats.messages_inserted,
            discovery.root.display(),
        );
        record_backfill_failures(
            adapter.kind(),
            discovery.walk_truncated,
            &meta,
            &stream,
            &mut failures,
        );

        pending_runners.push((
            *adapter,
            runtime,
            discovery.root.clone(),
            events,
            Some(ledger),
        ));
    }
    let interrupted = stop.load(Ordering::SeqCst);
    if !failures.is_empty() {
        return Err(failures.join("; "));
    }

    let mut runners = Vec::new();
    let mut absorbed = 0_u64;
    if !interrupted {
        // A graceful interrupt mid-backfill never publishes: a fresh
        // install's private projection and staged ledgers are dropped
        // whole, and the next start redoes the cold load from scratch.
        if pending_runners.is_empty() {
            return Err("nothing to serve: no enrolled runtimes".to_string());
        }
        if defer_ledger_sync {
            for (_, _, _, _, ledger) in &mut pending_runners {
                let mut ledger = ledger.take().expect("a cold runner owns its staged ledger");
                instrument
                    .ledger_write(&mut ledger, |ledger| ledger.flush())
                    .map_err(|error| error.to_string())?;
            }
        }
        if let Some(temp) = private_projection {
            let ledger_stage =
                ledger_stage.expect("a private projection has a private ledger stage");
            publish_private_projection(store, temp, &db, &instrument, || ledger_stage.publish())?;
            store = Store::open(&db)
                .map_err(|error| format!("opening published {}: {error:?}", db.display()))?;
        }

        // Backfill over: retire the rewarm cadence, take one authoritative
        // warm that also fans the closing diffs, and hand the mirror to the
        // effects drain. On a fresh install this is the moment the whole
        // surface arrives; meta subscribers absorb it via their designed
        // overflow-resync path.
        let _ = rewarm_tx.send(true);
        let _ = tokio_runtime.block_on(rewarm_task);
        for (adapter, runtime, root, events, ledger) in pending_runners {
            let ledger = match ledger {
                Some(ledger) => ledger,
                None => open_ledger(&public_ledgers, adapter.kind())?,
            };
            let runner = LiveRunner::start_with_ledger(
                adapter,
                runtime,
                root,
                Box::new(events),
                &mut store,
                opts.clone(),
                live.clone(),
                Some(ledger),
                &instrument,
            )
            .map_err(|error| error.to_string())?;
            runners.push((adapter.kind(), runner, 0_u64));
        }
        store.observe_effects();
        let published = hub
            .rewarm(store.conn())
            .map_err(|error| error.to_string())?;
        store.drain_effects();
        health.ingest_live();
        println!("live — {published} mirror diffs published at publication");

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
            let effects = store.drain_effects();
            if !effects.is_empty() {
                let report = hub
                    .absorb(store.conn(), effects, now_ms())
                    .map_err(|error| error.to_string())?;
                absorbed += report.session_mutations + report.meta_events;
            }
        }
    } else {
        let _ = rewarm_tx.send(true);
        let _ = tokio_runtime.block_on(rewarm_task);
        println!("interrupted during backfill — nothing published, shutting down");
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
    println!("hub: {absorbed} live items fanned out");
    if flags.profile {
        println!("\n== profile ==\n{}", instrument.report());
    }
    Ok(())
}
