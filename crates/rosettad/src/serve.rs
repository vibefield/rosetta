//! `rosettad serve` — the daemon: watch-style live ingest on the main
//! thread, the L3 HTTP/WS transport on a tokio runtime, and the hot tier
//! between them. The topology keeps every iron rule intact: the single
//! synchronous writer owns the store on this thread; async lives only in
//! the server; the hub absorbs drained effects strictly after commit.
//!
//! Effects observation starts AFTER the cold backfill publishes — bulk load
//! must never pay the mutation-log cost, and the hub warms its mirror from
//! the published projection instead.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use rosetta_capture::NotifyEvents;
use rosetta_pipeline::{IngestOptions, Instrument, LiveOptions, LiveRunner};
use rosetta_server::{AppState, DaemonHealth, Hub, HubConfig, RuntimeTick, router};
use rosetta_store::Store;

use crate::{
    ColdLedgerStage, ledger_root, open_for_backfill, open_ledger, parse_flags, projection_lock,
    publish_private_projection, record_backfill_failures, selected,
};

const DEFAULT_ADDR: &str = "127.0.0.1:4640";

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(0)
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
    let instrument = Instrument::new();
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
    let discoveries = rosetta_pipeline::discover(&adapters, &flags.home, &opts, &instrument)
        .map_err(|error| error.to_string())?;

    let mut pending_runners = Vec::new();
    let mut failures = Vec::new();
    for (adapter, discovery) in adapters.iter().zip(&discoveries) {
        if !discovery.installed {
            println!("{}: not installed, skipping", adapter.kind());
            continue;
        }
        let mut runtime = discovery.runtime.clone();
        runtime.enabled = true; // serving is explicit consent

        let events = NotifyEvents::watch(&discovery.root).map_err(|error| error.to_string())?;
        let ledger = open_ledger(backfill_ledgers, adapter.kind())?;
        let mut ledger = if defer_ledger_sync {
            ledger.defer_sync_until_flush()
        } else {
            ledger
        };
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
        let stream = rosetta_pipeline::stream_backfill_into(
            *adapter,
            &runtime,
            &discovery.root,
            &mut store,
            &files,
            &opts,
            &instrument,
            Some(&mut ledger),
        )
        .map_err(|error| error.to_string())?;
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
    if pending_runners.is_empty() {
        return Err("nothing to serve: no installed runtimes".to_string());
    }
    if !failures.is_empty() {
        return Err(failures.join("; "));
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
        let ledger_stage = ledger_stage.expect("a private projection has a private ledger stage");
        publish_private_projection(store, temp, &db, &instrument, || ledger_stage.publish())?;
        store = Store::open(&db)
            .map_err(|error| format!("opening published {}: {error:?}", db.display()))?;
    }

    let mut runners = Vec::new();
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

    // The live-read tier: observation starts now, the mirror warms from the
    // published projection, and the transport serves from both.
    store.observe_effects();
    let hub = Arc::new(Hub::new(HubConfig::default(), now_ms() as u64));
    let warmed = hub.warm(store.conn()).map_err(|error| error.to_string())?;
    store.drain_effects();

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
        let stop = stop.clone();
        let shutdown_tx = shutdown_tx.clone();
        tokio_runtime.spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                stop.store(true, Ordering::SeqCst);
                let _ = shutdown_tx.send(true);
            }
        });
    }
    let health = Arc::new(DaemonHealth::new(now_ms()));
    let state = AppState::new(hub.clone(), db.clone()).with_daemon_health(health.clone());
    let server = tokio_runtime.spawn(async move {
        let mut shutdown_rx = shutdown_rx;
        axum::serve(listener, router(state))
            .with_graceful_shutdown(async move {
                let _ = shutdown_rx.wait_for(|stopped| *stopped).await;
            })
            .await
    });
    println!("serving http://{local} — {warmed} mirror rows (ctrl-c to stop)");

    // The ingest loop: ticks ARE the existing phases; the drain feeds the
    // hub strictly after each batch's commit.
    let mut remaining = flags.ticks;
    let mut absorbed = 0_u64;
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
