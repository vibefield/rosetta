//! rosettad — ingestion, live watching, and ledger-backed projection rebuilds.
//!
//!   rosettad discover [--home DIR]
//!   rosettad meta --db PATH [--home DIR] [--agent KIND ...] [--profile]
//!   rosettad ingest --db PATH [--home DIR] [--agent KIND ...] [--profile]
//!   rosettad watch --db PATH [--home DIR] [--agent KIND ...] [--ticks N]
//!   rosettad rebuild --db PATH [--ledger DIR] [--agent KIND ...]
//!
//! `ingest` implies consent for the selected agents (the CLI user asked);
//! the app flow instead runs `discover`, asks, and flips `enabled` itself.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use rosetta_adapter_antigravity::AntigravityAdapter;
use rosetta_adapter_claude_code::ClaudeCodeAdapter;
use rosetta_adapter_codex::CodexAdapter;
use rosetta_adapter_copilot::CopilotAdapter;
use rosetta_adapter_grok::GrokAdapter;
use rosetta_adapter_hermes::HermesAdapter;
use rosetta_adapter_kimi_code::KimiCodeAdapter;
use rosetta_adapter_opencode::OpenCodeAdapter;
use rosetta_adapter_pi::PiAdapter;
use rosetta_adapter_support::AgentAdapter;
use rosetta_capture::{Ledger, NotifyEvents};
use rosetta_pipeline::{
    IngestOptions, Instrument, InstrumentSnapshot, LiveOptions, LiveRunner, Metric, discover,
    meta_ingest_into, rebuild_runtime, stream_backfill_into,
};
use rosetta_store::Store;

mod bench;
mod serve;

fn registry() -> Vec<&'static dyn AgentAdapter> {
    vec![
        &AntigravityAdapter,
        &ClaudeCodeAdapter,
        &CodexAdapter,
        &CopilotAdapter,
        &GrokAdapter,
        &HermesAdapter,
        &KimiCodeAdapter,
        &OpenCodeAdapter,
        &PiAdapter,
    ]
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("discover") => bench::cmd_discover(&args[1..]),
        Some("meta") => bench::cmd_meta(&args[1..]),
        Some("ingest") => cmd_ingest(&args[1..]),
        Some("watch") => cmd_watch(&args[1..]),
        Some("serve") => serve::cmd_serve(&args[1..]),
        Some("rebuild") => cmd_rebuild(&args[1..]),
        _ => {
            eprintln!("usage: rosettad <discover|meta|ingest|watch|serve|rebuild> [options]");
            eprintln!("  discover [--home DIR] [--agent KIND ...] [--json]");
            eprintln!("  meta --db PATH [--home DIR] [--agent KIND ...] [--profile] [--json]");
            eprintln!("  ingest --db PATH [--home DIR] [--agent KIND ...] [--profile] [--json]");
            eprintln!("  watch --db PATH [--home DIR] [--agent KIND ...] [--ticks N] [--profile]");
            eprintln!(
                "  serve --db PATH [--addr HOST:PORT] [--home DIR] [--agent KIND ...] [--profile]"
            );
            eprintln!("  rebuild --db PATH [--ledger DIR] [--agent KIND ...] [--profile]");
            eprintln!(
                "  (ingest and watch keep a raw ledger at <db>.ledger/ unless --ledger overrides)"
            );
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("rosettad: {message}");
            ExitCode::FAILURE
        }
    }
}

struct Flags {
    home: PathBuf,
    db: Option<PathBuf>,
    agents: Vec<String>,
    profile: bool,
    /// Emit newline-delimited JSON events for repository tools.
    json: bool,
    ticks: Option<u64>,
    ledger: Option<PathBuf>,
    /// serve only: the localhost listen address.
    addr: Option<String>,
}

fn parse_flags(args: &[String]) -> Result<Flags, String> {
    let mut flags = Flags {
        home: default_home()?,
        db: None,
        agents: Vec::new(),
        profile: false,
        json: false,
        ticks: None,
        ledger: None,
        addr: None,
    };
    let mut at = 0;
    while at < args.len() {
        match args[at].as_str() {
            "--home" => {
                flags.home = PathBuf::from(take_value(args, &mut at)?);
            }
            "--db" => {
                flags.db = Some(PathBuf::from(take_value(args, &mut at)?));
            }
            "--agent" => {
                flags.agents.push(take_value(args, &mut at)?);
            }
            "--profile" => {
                flags.profile = true;
                at += 1;
            }
            "--json" => {
                flags.json = true;
                at += 1;
            }
            "--ledger" => {
                flags.ledger = Some(PathBuf::from(take_value(args, &mut at)?));
            }
            "--addr" => {
                flags.addr = Some(take_value(args, &mut at)?);
            }
            "--ticks" => {
                let value = take_value(args, &mut at)?;
                flags.ticks = Some(
                    value
                        .parse()
                        .map_err(|_| format!("--ticks needs a number, got {value}"))?,
                );
            }
            other => return Err(format!("unknown flag {other}")),
        }
    }
    Ok(flags)
}

fn take_value(args: &[String], at: &mut usize) -> Result<String, String> {
    let flag = &args[*at];
    let value = args
        .get(*at + 1)
        .ok_or_else(|| format!("{flag} needs a value"))?;
    *at += 2;
    Ok(value.clone())
}

/// The raw ledger lives beside the projection unless overridden — the
/// projection is the disposable one of the pair.
fn ledger_root(flags: &Flags, db: &std::path::Path) -> PathBuf {
    flags
        .ledger
        .clone()
        .unwrap_or_else(|| db.with_extension("ledger"))
}

/// Cross-process writer/rebuild lock. SQLite owns the lock lifetime, so a
/// crash releases it automatically instead of leaving a stale pid file.
struct ProjectionLock {
    _connection: rusqlite::Connection,
}

fn projection_lock(db: &std::path::Path) -> Result<ProjectionLock, String> {
    let mut lock_name = db.as_os_str().to_owned();
    lock_name.push(".lock");
    let lock_path = PathBuf::from(lock_name);
    if let Some(parent) = lock_path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("creating {}: {error}", parent.display()))?;
    }
    let connection = rusqlite::Connection::open(&lock_path)
        .map_err(|error| format!("opening projection lock {}: {error}", lock_path.display()))?;
    connection
        .busy_timeout(Duration::ZERO)
        .map_err(|error| format!("configuring projection lock: {error}"))?;
    connection
        .execute_batch("PRAGMA journal_mode = DELETE; BEGIN EXCLUSIVE;")
        .map_err(|error| {
            format!(
                "projection {} is already owned by another ingest/watch/rebuild process: {error}",
                db.display()
            )
        })?;
    Ok(ProjectionLock {
        _connection: connection,
    })
}

fn open_ledger(root: &std::path::Path, kind: &str) -> Result<Ledger, String> {
    Ledger::open(&root.join(kind)).map_err(|error| error.to_string())
}

fn default_home() -> Result<PathBuf, String> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .ok_or_else(|| "no HOME in the environment; pass --home".to_string())
}

fn selected(flags: &Flags) -> Result<Vec<&'static dyn AgentAdapter>, String> {
    let all = registry();
    if flags.agents.is_empty() {
        return Ok(all);
    }
    flags
        .agents
        .iter()
        .map(|wanted| {
            all.iter()
                .copied()
                .find(|adapter| adapter.kind() == wanted)
                .ok_or_else(|| format!("unknown agent kind {wanted}"))
        })
        .collect()
}

fn cmd_ingest(args: &[String]) -> Result<(), String> {
    let flags = parse_flags(args)?;
    let db = flags.db.clone().ok_or("ingest needs --db PATH")?;
    let opts = IngestOptions::default();
    let instrument = Instrument::new();
    let adapters = selected(&flags)?;
    let _projection_lock = projection_lock(&db)?;

    let (mut store, private_projection) = open_for_backfill(&db)?;
    let ledgers = ledger_root(&flags, &db);
    // Only a brand-new private projection may defer per-batch ledger sync.
    // Existing projections are public and retain the live durability order.
    let defer_ledger_sync = private_projection.is_some();
    let mut ingest_ledgers = Vec::new();
    let discoveries =
        discover(&adapters, &flags.home, &opts, &instrument).map_err(|error| error.to_string())?;
    let mut failures = Vec::new();

    for (adapter, discovery) in adapters.iter().zip(&discoveries) {
        if !discovery.installed {
            if flags.json {
                bench::emit(&serde_json::json!({
                    "event": "skipped",
                    "phase": "ingest",
                    "agent": adapter.kind(),
                    "reason": "not-installed",
                }));
            } else {
                println!("{}: not installed, skipping", adapter.kind());
            }
            continue;
        }
        let mut runtime = discovery.runtime.clone();
        runtime.enabled = true; // CLI ingest is explicit consent
        if discovery.walk_truncated {
            failures.push(format!("{} discovery walk was truncated", adapter.kind()));
        }
        let ledger = open_ledger(&ledgers, adapter.kind())?;
        ingest_ledgers.push(if defer_ledger_sync {
            ledger.defer_sync_until_flush()
        } else {
            ledger
        });
        let ledger = ingest_ledgers
            .last_mut()
            .expect("the runtime ledger was just pushed");

        let (meta, files) = meta_ingest_into(
            *adapter,
            &runtime,
            &discovery.root,
            &mut store,
            &opts,
            &instrument,
            Some(&mut *ledger),
        )
        .map_err(|error| error.to_string())?;
        if flags.json {
            bench::emit(&bench::meta_event(adapter.kind(), &meta));
        } else {
            println!(
                "{}: meta — {} sessions, {} projects, {} files ({} inventory-only, {} unidentified)",
                adapter.kind(),
                meta.sessions,
                meta.projects,
                meta.files_matched,
                meta.files_inventory_only,
                meta.files_unidentified,
            );
        }
        if meta.walk_truncated {
            failures.push(format!("{} meta walk was truncated", adapter.kind()));
        }
        if meta.files_unidentified > 0 {
            failures.push(format!(
                "{} left {} streamable files unidentified",
                adapter.kind(),
                meta.files_unidentified
            ));
        }
        if meta.files_vanished > 0 {
            failures.push(format!(
                "{} lost {} files during meta ingest",
                adapter.kind(),
                meta.files_vanished
            ));
        }

        let stream = stream_backfill_into(
            *adapter,
            &runtime,
            &discovery.root,
            &mut store,
            &files,
            &opts,
            &instrument,
            Some(&mut *ledger),
        )
        .map_err(|error| error.to_string())?;
        if flags.json {
            bench::emit(&serde_json::json!({
                "event": "stream",
                "agent": adapter.kind(),
                "records": stream.records,
                "parsed": stream.parsed,
                "unknown": stream.unknown,
                "hardErrors": stream.hard_errors,
                "oversized": stream.oversized,
                "filesMissing": stream.files_missing,
                "retriesExhausted": stream.retries_exhausted,
                "conflicts": stream.conflicts,
                "filesIncomplete": stream.files_incomplete,
                "messagesInserted": stream.stats.messages_inserted,
                "messagesUpdated": stream.stats.messages_updated,
                "toolResultsMerged": stream.stats.tool_results_merged,
                "sessionsIndexed": stream.sessions_indexed,
            }));
        } else {
            println!(
                "{}: stream — {} records ({} parsed, {} unknown, {} hard errors), \
                 {} rows inserted, {} tool results merged, {} sessions indexed",
                adapter.kind(),
                stream.records,
                stream.parsed,
                stream.unknown,
                stream.hard_errors,
                stream.stats.messages_inserted,
                stream.stats.tool_results_merged,
                stream.sessions_indexed,
            );
        }
        if stream.hard_errors > 0
            || stream.oversized > 0
            || stream.files_missing > 0
            || stream.retries_exhausted > 0
            || stream.conflicts > 0
            || stream.files_incomplete > 0
        {
            failures.push(format!(
                "{} ingest incomplete: {} hard, {} oversized, {} missing, {} retry-exhausted, {} conflicts, {} incomplete files",
                adapter.kind(),
                stream.hard_errors,
                stream.oversized,
                stream.files_missing,
                stream.retries_exhausted,
                stream.conflicts,
                stream.files_incomplete,
            ));
        }
    }

    // A private projection has no externally visible commits yet. Make every
    // runtime ledger durable in one pre-publication barrier. Reported source
    // failures still cross the barrier before publication is refused; an IO
    // error may return earlier, but no private projection can be published.
    if defer_ledger_sync {
        ingest_ledgers.iter_mut().try_for_each(|ledger| {
            instrument
                .ledger_write(ledger, |ledger| ledger.flush())
                .map_err(|error| error.to_string())
        })?;
    }

    if !failures.is_empty() {
        if flags.json {
            bench::emit_profile("ingest", &instrument);
        } else if flags.profile {
            println!("\n== profile ==\n{}", instrument.report());
        }
        return Err(failures.join("; "));
    }
    if let Some(temp) = private_projection {
        publish_private_projection(store, temp, &db, &instrument)?;
    }
    if flags.json {
        bench::emit_profile("ingest", &instrument);
    } else if flags.profile {
        println!("\n== profile ==\n{}", instrument.report());
    }
    Ok(())
}

/// Backfill, then hold the projection live: one watcher + runner per
/// installed runtime, round-robin ticks over a single writer. A hard kill is
/// always safe — every write is one transaction behind a checkpoint CAS.
fn cmd_watch(args: &[String]) -> Result<(), String> {
    let flags = parse_flags(args)?;
    let db = flags.db.clone().ok_or("watch needs --db PATH")?;
    let opts = IngestOptions::default();
    let live = LiveOptions::default();
    let instrument = Instrument::new();
    let adapters = selected(&flags)?;
    let _projection_lock = projection_lock(&db)?;

    let (mut store, private_projection) = open_for_backfill(&db)?;
    let ledgers = ledger_root(&flags, &db);
    let discoveries =
        discover(&adapters, &flags.home, &opts, &instrument).map_err(|error| error.to_string())?;

    let mut runners = Vec::new();
    for (adapter, discovery) in adapters.iter().zip(&discoveries) {
        if !discovery.installed {
            println!("{}: not installed, skipping", adapter.kind());
            continue;
        }
        let mut runtime = discovery.runtime.clone();
        runtime.enabled = true; // watching is explicit consent

        // Watch BEFORE the backfill scan: a write landing mid-backfill still
        // produces a hint for the first live tick.
        let events = NotifyEvents::watch(&discovery.root).map_err(|error| error.to_string())?;
        let mut ledger = open_ledger(&ledgers, adapter.kind())?;

        let (meta, files) = meta_ingest_into(
            *adapter,
            &runtime,
            &discovery.root,
            &mut store,
            &opts,
            &instrument,
            Some(&mut ledger),
        )
        .map_err(|error| error.to_string())?;
        let stream = stream_backfill_into(
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

        let runner = LiveRunner::start_with_ledger(
            *adapter,
            runtime,
            discovery.root.clone(),
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
    if runners.is_empty() {
        return Err("nothing to watch: no installed runtimes".to_string());
    }
    if let Some(temp) = private_projection {
        publish_private_projection(store, temp, &db, &instrument)?;
        store = Store::open(&db)
            .map_err(|error| format!("opening published {}: {error:?}", db.display()))?;
    }

    let mut remaining = flags.ticks;
    loop {
        match &mut remaining {
            Some(0) => break,
            Some(count) => *count -= 1,
            None => {}
        }
        for (kind, runner, seen_batches) in &mut runners {
            runner
                .tick(&mut store, &instrument)
                .map_err(|error| error.to_string())?;
            let report = runner.report();
            if report.stream.batches > *seen_batches {
                *seen_batches = report.stream.batches;
                println!(
                    "{kind}: live — {} rows, {} tool results, {} sessions touched",
                    report.stream.stats.messages_inserted + report.stream.stats.messages_updated,
                    report.stream.stats.tool_results_merged,
                    report.stream.sessions_indexed,
                );
            }
        }
    }

    for (kind, runner, _) in &runners {
        let report = runner.report();
        println!(
            "{kind}: watched {} ticks — {} events ({} ignored), {} polls, {} rescans, {} admissions, {} rows",
            report.ticks,
            report.events,
            report.hints_ignored,
            report.polls,
            report.rescans,
            report.admissions,
            report.stream.stats.messages_inserted,
        );
    }
    if flags.profile {
        println!("\n== profile ==\n{}", instrument.report());
    }
    Ok(())
}

fn rebuild_adapters(
    flags: &Flags,
    ledgers: &std::path::Path,
) -> Result<Vec<&'static dyn AgentAdapter>, String> {
    let requested = selected(flags)?;
    let mut chosen = Vec::new();
    for adapter in requested {
        let dir = ledgers.join(adapter.kind());
        if dir.is_dir() {
            if !chosen
                .iter()
                .any(|existing: &&dyn AgentAdapter| existing.kind() == adapter.kind())
            {
                chosen.push(adapter);
            }
        } else if !flags.agents.is_empty() {
            return Err(format!("no {} ledger at {}", adapter.kind(), dir.display()));
        }
    }
    if chosen.is_empty() {
        return Err(format!("no runtime ledgers at {}", ledgers.display()));
    }

    // A partial rebuild would silently erase every unselected runtime from a
    // shared projection. Require the selected set to cover every ledger dir.
    for entry in std::fs::read_dir(ledgers)
        .map_err(|error| format!("listing {}: {error}", ledgers.display()))?
    {
        let entry = entry.map_err(|error| format!("listing {}: {error}", ledgers.display()))?;
        if !entry
            .file_type()
            .map_err(|error| format!("stating {}: {error}", entry.path().display()))?
            .is_dir()
        {
            continue;
        }
        let kind = entry.file_name().to_string_lossy().into_owned();
        if !chosen.iter().any(|adapter| adapter.kind() == kind) {
            return Err(format!(
                "ledger {kind} would be omitted; rebuild is whole-projection, so select every ledger runtime"
            ));
        }
    }

    for adapter in &chosen {
        let dir = ledgers.join(adapter.kind());
        let scan =
            rosetta_capture::visit_ledger::<rosetta_capture::CaptureError, _>(&dir, |_| Ok(()))
                .map_err(|error| error.to_string())?;
        if scan.entries == 0 {
            return Err(format!("{} ledger is empty", dir.display()));
        }
        if scan.torn_segments > 0 {
            return Err(format!(
                "{} has {} torn/corrupt segment tails; refusing to replace the projection",
                dir.display(),
                scan.torn_segments
            ));
        }
    }
    Ok(chosen)
}

fn projection_parent(db: &std::path::Path) -> &std::path::Path {
    db.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."))
}

/// A first ingest is a cold build, not a live mutation: construct it in a
/// private sibling file so derived indexes can be deferred without exposing
/// an inconsistent projection. Existing projections always stay in live
/// mode and retain per-transaction FTS/rollup maintenance.
fn open_for_backfill(db: &std::path::Path) -> Result<(Store, Option<tempfile::TempPath>), String> {
    if db.exists() {
        let store =
            Store::open(db).map_err(|error| format!("opening {}: {error:?}", db.display()))?;
        return Ok((store, None));
    }

    let parent = projection_parent(db);
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("creating {}: {error}", parent.display()))?;
    let named = tempfile::Builder::new()
        .prefix(".rosetta-backfill-")
        .suffix(".sqlite")
        .tempfile_in(parent)
        .map_err(|error| {
            format!(
                "creating backfill database in {}: {error}",
                parent.display()
            )
        })?;
    let temp = named.into_temp_path();
    let store = Store::open_for_rebuild(temp.as_ref())
        .map_err(|error| format!("opening backfill database {}: {error:?}", temp.display()))?;
    Ok((store, Some(temp)))
}

fn publish_private_projection(
    mut store: Store,
    temp: tempfile::TempPath,
    db: &std::path::Path,
    instrument: &Instrument,
) -> Result<(), String> {
    instrument
        .finish_bulk_load(&mut store)
        .map_err(|error| format!("finalizing private projection: {error}"))?;
    instrument.time(Metric::PublishNanos, || {
        drop(store);
        let finished = rusqlite::Connection::open(&*temp)
            .map_err(|error| format!("reopening private projection: {error}"))?;
        make_single_file(&finished, "private projection")?;
        drop(finished);
        if let Ok(permissions) = std::fs::metadata(db).map(|metadata| metadata.permissions()) {
            std::fs::set_permissions(&*temp, permissions)
                .map_err(|error| format!("preserving projection permissions: {error}"))?;
        }
        std::fs::File::open(&*temp)
            .and_then(|file| file.sync_all())
            .map_err(|error| format!("syncing private projection: {error}"))?;
        install_rebuilt_projection(db, temp.as_ref())?;
        let _installed = temp.keep();
        Ok(())
    })
}

fn sidecar(db: &std::path::Path, suffix: &str) -> PathBuf {
    let mut path = db.as_os_str().to_owned();
    path.push(suffix);
    PathBuf::from(path)
}

fn make_single_file(connection: &rusqlite::Connection, label: &str) -> Result<(), String> {
    connection
        .busy_timeout(Duration::from_secs(5))
        .map_err(|error| format!("configuring {label}: {error}"))?;
    connection
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        .map_err(|error| format!("checkpointing {label}: {error}"))?;
    let mode: String = connection
        .query_row("PRAGMA journal_mode = DELETE", [], |row| row.get(0))
        .map_err(|error| format!("switching {label} to a single-file journal: {error}"))?;
    if mode != "delete" {
        return Err(format!("{label} stayed in unexpected journal mode {mode}"));
    }
    Ok(())
}

fn install_rebuilt_projection(db: &std::path::Path, temp: &std::path::Path) -> Result<(), String> {
    if db.exists() {
        let old = rusqlite::Connection::open(db)
            .map_err(|error| format!("opening old projection {}: {error}", db.display()))?;
        make_single_file(&old, "old projection")?;
        drop(old);
        for suffix in ["-wal", "-shm"] {
            let path = sidecar(db, suffix);
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(format!("removing {}: {error}", path.display())),
            }
        }
    }

    std::fs::rename(temp, db).map_err(|error| {
        format!(
            "atomically replacing {} with {}: {error}",
            db.display(),
            temp.display()
        )
    })?;
    std::fs::File::open(projection_parent(db))
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("syncing {}: {error}", projection_parent(db).display()))?;
    Ok(())
}

/// Rebuild into a private sibling database and atomically install it only
/// after every runtime replays cleanly. The live projection is never the work
/// surface, so validation or replay failure leaves it byte-for-byte usable.
fn cmd_rebuild(args: &[String]) -> Result<(), String> {
    let flags = parse_flags(args)?;
    let db = flags.db.clone().ok_or("rebuild needs --db PATH")?;
    let ledgers = ledger_root(&flags, &db);
    if !ledgers.is_dir() {
        return Err(format!("no ledger at {}", ledgers.display()));
    }
    let adapters = rebuild_adapters(&flags, &ledgers)?;
    let _projection_lock = projection_lock(&db)?;

    let parent = projection_parent(&db);
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("creating {}: {error}", parent.display()))?;
    let named = tempfile::Builder::new()
        .prefix(".rosetta-rebuild-")
        .suffix(".sqlite")
        .tempfile_in(parent)
        .map_err(|error| format!("creating rebuild database in {}: {error}", parent.display()))?;
    let temp = named.into_temp_path();

    let instrument = Instrument::new();
    let mut store = Store::open_for_rebuild(temp.as_ref())
        .map_err(|error| format!("opening rebuild database {}: {error:?}", temp.display()))?;
    let mut reports = Vec::new();
    for adapter in adapters {
        let profile_before = instrument.snapshot();
        let dir = ledgers.join(adapter.kind());
        let layout = adapter.layout();
        let runtime = rosetta_core::rows::RuntimeRow {
            id: rosetta_core::ids::runtime_id(adapter.kind(), &layout.data_root),
            kind: adapter.kind().to_string(),
            data_root: layout.data_root.clone(),
            enabled: true,
            account: None,
            config: None,
            skills: None,
            versions_seen: None,
            discovered_at: 0,
        };
        let report = rebuild_runtime(adapter, &runtime, &dir, &mut store, &instrument)
            .map_err(|error| error.to_string())?;
        if report.hard_errors > 0 {
            return Err(format!(
                "{}: {} hard errors during replay",
                adapter.kind(),
                report.hard_errors
            ));
        }
        if report.torn_segments > 0 {
            return Err(format!(
                "{}: ledger changed or tore during replay",
                adapter.kind()
            ));
        }
        let profile = instrument.snapshot().delta_since(&profile_before);
        reports.push((adapter.kind(), report, profile));
    }

    publish_private_projection(store, temp, &db, &instrument)?;

    for (kind, report, profile) in reports {
        println!(
            "{kind}: rebuilt {} files from {} segments — {} records replayed \
             ({} stale, {} duplicate, {} already projected), {} meta batches, \
             {} rows inserted, {} sessions stubbed",
            report.files,
            report.segments,
            report.raw_records,
            report.stale_records_skipped,
            report.duplicate_records_skipped,
            report.records_already_projected,
            report.meta_batches,
            report.stats.messages_inserted,
            report.sessions_stubbed,
        );
        if flags.profile {
            print_agent_profile(kind, &profile);
        }
    }
    if flags.profile {
        println!("\n== profile ==\n{}", instrument.report());
    }
    Ok(())
}

fn print_agent_profile(kind: &str, profile: &InstrumentSnapshot) {
    println!("\n== profile: {kind} ==\n{}", profile.report());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_contains_every_implemented_adapter() {
        let kinds = registry()
            .into_iter()
            .map(AgentAdapter::kind)
            .collect::<Vec<_>>();
        assert_eq!(
            kinds,
            [
                "antigravity",
                "claude-code",
                "codex",
                "copilot",
                "grok",
                "hermes",
                "kimi-code",
                "opencode",
                "pi",
            ]
        );
    }

    #[test]
    fn standalone_meta_refuses_an_existing_projection() {
        let root = tempfile::tempdir().unwrap();
        let db = root.path().join("existing.sqlite");
        std::fs::write(&db, b"sentinel").unwrap();
        let error = bench::cmd_meta(&[
            "--db".into(),
            db.display().to_string(),
            "--home".into(),
            root.path().display().to_string(),
            "--agent".into(),
            "codex".into(),
        ])
        .unwrap_err();
        assert!(error.contains("needs new paths"));
        assert_eq!(std::fs::read(db).unwrap(), b"sentinel");
    }

    fn args(db: &std::path::Path, ledger: &std::path::Path, agent: &str) -> Vec<String> {
        vec![
            "--db".to_string(),
            db.display().to_string(),
            "--ledger".to_string(),
            ledger.display().to_string(),
            "--agent".to_string(),
            agent.to_string(),
        ]
    }

    fn sentinel(db: &std::path::Path) {
        let connection = rusqlite::Connection::open(db).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE sentinel(value TEXT NOT NULL);
                 INSERT INTO sentinel VALUES ('keep-me');",
            )
            .unwrap();
    }

    fn assert_sentinel(db: &std::path::Path) {
        let connection = rusqlite::Connection::open(db).unwrap();
        let value: String = connection
            .query_row("SELECT value FROM sentinel", [], |row| row.get(0))
            .unwrap();
        assert_eq!(value, "keep-me");
    }

    #[test]
    fn unknown_agent_is_validated_before_the_live_projection_is_touched() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("projection.db");
        let ledgers = temp.path().join("ledger");
        std::fs::create_dir(&ledgers).unwrap();
        sentinel(&db);

        assert!(cmd_rebuild(&args(&db, &ledgers, "not-an-agent")).is_err());
        assert_sentinel(&db);
    }

    #[test]
    fn failed_replay_keeps_the_old_projection() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("projection.db");
        let ledgers = temp.path().join("ledger");
        sentinel(&db);
        let mut ledger = rosetta_capture::Ledger::open(&ledgers.join("claude-code")).unwrap();
        ledger.append_meta(999, "[]").unwrap();
        ledger.flush().unwrap();
        drop(ledger);

        let error = cmd_rebuild(&args(&db, &ledgers, "claude-code")).unwrap_err();
        assert!(error.contains("unsupported ledger meta schema"));
        assert_sentinel(&db);
    }

    #[test]
    fn successful_replay_atomically_replaces_the_old_projection() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("projection.db");
        let ledgers = temp.path().join("ledger");
        sentinel(&db);
        let mut ledger = rosetta_capture::Ledger::open(&ledgers.join("claude-code")).unwrap();
        ledger
            .append_meta(rosetta_pipeline::LEDGER_META_SCHEMA_VERSION, "[]")
            .unwrap();
        ledger.flush().unwrap();
        drop(ledger);

        cmd_rebuild(&args(&db, &ledgers, "claude-code")).unwrap();
        let connection = rusqlite::Connection::open(&db).unwrap();
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM agent_runtime", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            1
        );
        assert!(connection.prepare("SELECT * FROM sentinel").is_err());
        assert!(!sidecar(&db, "-wal").exists());
    }

    #[test]
    fn first_backfill_stays_private_until_finalized_and_published() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("projection.db");
        let (store, private) = open_for_backfill(&db).unwrap();
        assert!(!db.exists());
        let private = private.expect("a missing projection takes the cold path");
        assert!(private.exists());

        publish_private_projection(store, private, &db, &Instrument::new()).unwrap();
        assert!(db.exists());
        assert!(!sidecar(&db, "-wal").exists());
        Store::open(&db).unwrap();
    }

    #[test]
    fn existing_backfill_target_stays_live() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("projection.db");
        drop(Store::open(&db).unwrap());
        let (_store, private) = open_for_backfill(&db).unwrap();
        assert!(private.is_none());
    }

    #[test]
    fn projection_lock_excludes_a_second_writer() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("projection.db");
        let first = projection_lock(&db).unwrap();
        assert!(projection_lock(&db).is_err());
        drop(first);
        assert!(projection_lock(&db).is_ok());
    }
}
