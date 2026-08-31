//! rosettad — ingestion, live watching, and ledger-backed projection rebuilds.
//!
//!   rosettad discover [--home DIR]
//!   rosettad sources [--home DIR] [--agent KIND ...]
//!   rosettad streams [--agent KIND ...]
//!   rosettad meta --db PATH [--home DIR] [--agent KIND ...] [--profile]
//!   rosettad ingest --db PATH [--home DIR] [--agent KIND ...] [--profile] [--audit-boundaries]
//!   rosettad watch --db PATH [--home DIR] [--agent KIND ...] [--ticks N]
//!   rosettad rebuild --db PATH [--ledger DIR] [--agent KIND ...]
//!
//! `ingest` implies consent for the selected agents (the CLI user asked);
//! the app flow instead runs `discover`, asks, and flips `enabled` itself.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

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
use rosetta_search::{
    Fts5SearchWorker, Fts5SidecarSearch, SearchBackend, SearchReadiness, synchronize_fts5_once,
};
use rosetta_store::Store;

mod bench;
mod engine;
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
        Some("sources") => bench::cmd_sources(&args[1..]),
        Some("streams") => bench::cmd_streams(&args[1..]),
        Some("meta") => bench::cmd_meta(&args[1..]),
        Some("ingest") => cmd_ingest(&args[1..]),
        Some("watch") => cmd_watch(&args[1..]),
        Some("serve") => serve::cmd_serve(&args[1..]),
        Some("rebuild") => cmd_rebuild(&args[1..]),
        _ => {
            eprintln!(
                "usage: rosettad <discover|sources|streams|meta|ingest|watch|serve|rebuild> [options]"
            );
            eprintln!("  discover [--home DIR] [--agent KIND ...] [--json]");
            eprintln!("  sources [--home DIR] [--agent KIND ...] [--json]");
            eprintln!("  streams [--agent KIND ...] [--json]");
            eprintln!("  meta --db PATH [--home DIR] [--agent KIND ...] [--profile] [--json]");
            eprintln!(
                "  ingest --db PATH [--home DIR] [--agent KIND ...] [--profile] [--json] [--audit-boundaries]"
            );
            eprintln!("  watch --db PATH [--home DIR] [--agent KIND ...] [--ticks N] [--profile]");
            eprintln!(
                "  serve --db PATH [--addr HOST:PORT] [--home DIR] [--agent KIND ...] [--profile] [--stop-after-meta] [--slice-budget BYTES|0]"
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
    /// ingest only: emit one bounded, private before/after sample for steps 5–8.
    audit_boundaries: bool,
    /// Emit newline-delimited JSON events for repository tools.
    json: bool,
    ticks: Option<u64>,
    ledger: Option<PathBuf>,
    /// serve only: the localhost listen address.
    addr: Option<String>,
    /// serve only: consent for every installed runtime from the command
    /// line — the headless stand-in for the app's roll call.
    all_agents: bool,
    /// serve only: expose the completed metadata projection and keep serving,
    /// but do not read any complete source records.
    stop_after_meta: bool,
    /// serve only: starter-slice byte budget for the cold scheduler.
    /// `None` = the blessed default; `Some(0)` disables slicing.
    slice_budget: Option<u64>,
}

fn parse_flags(args: &[String]) -> Result<Flags, String> {
    let mut flags = Flags {
        home: default_home()?,
        db: None,
        agents: Vec::new(),
        profile: false,
        audit_boundaries: false,
        json: false,
        ticks: None,
        ledger: None,
        addr: None,
        all_agents: false,
        stop_after_meta: false,
        slice_budget: None,
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
            "--audit-boundaries" => {
                flags.audit_boundaries = true;
                at += 1;
            }
            "--all-agents" => {
                flags.all_agents = true;
                at += 1;
            }
            "--stop-after-meta" => {
                flags.stop_after_meta = true;
                at += 1;
            }
            "--slice-budget" => {
                let value = take_value(args, &mut at)?;
                flags.slice_budget = Some(parse_byte_size(&value)?);
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

/// `256k`-style byte sizes for the slice budget: plain digits, or a `k`/`m`
/// suffix (KiB/MiB). `0` means slicing off.
fn parse_byte_size(value: &str) -> Result<u64, String> {
    let lower = value.trim().to_ascii_lowercase();
    let (digits, unit) = match lower.strip_suffix(['k', 'm']) {
        Some(digits) => (
            digits,
            if lower.ends_with('k') {
                1024
            } else {
                1024 * 1024
            },
        ),
        None => (lower.as_str(), 1),
    };
    let number: u64 = digits
        .parse()
        .map_err(|_| format!("--slice-budget wants BYTES, `NNNk`, `NNNm`, or 0 — got {value}"))?;
    number
        .checked_mul(unit)
        .ok_or_else(|| format!("--slice-budget {value} overflows"))
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

/// A fresh cold ingest writes its ledger into a same-filesystem private
/// directory. The tree is promoted only after every segment and directory
/// entry is durable and immediately before the validated SQLite projection
/// is installed. A crash before promotion leaves an unscanned staging tree;
/// a crash after promotion leaves a complete ledger that `rebuild` can use.
#[derive(Debug)]
pub(crate) struct ColdLedgerStage {
    temp: tempfile::TempDir,
    destination: PathBuf,
    publication_barriers: Vec<PathBuf>,
}

impl ColdLedgerStage {
    pub(crate) fn new(destination: &std::path::Path) -> Result<Self, String> {
        match std::fs::symlink_metadata(destination) {
            Ok(_) => {
                return Err(format!(
                    "projection is missing but ledger {} already exists; use `rosettad rebuild` so generations cannot collide",
                    destination.display()
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "stating ledger destination {}: {error}",
                    destination.display()
                ));
            }
        }

        let parent = projection_parent(destination);
        let publication_barriers = directory_publication_barriers(parent);
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("creating {}: {error}", parent.display()))?;
        let temp = tempfile::Builder::new()
            .prefix(".rosetta-ledger-")
            .tempdir_in(parent)
            .map_err(|error| {
                format!(
                    "creating private ledger beside {}: {error}",
                    destination.display()
                )
            })?;
        Ok(Self {
            temp,
            destination: destination.to_path_buf(),
            publication_barriers,
        })
    }

    pub(crate) fn path(&self) -> &std::path::Path {
        self.temp.path()
    }

    pub(crate) fn publish(self) -> Result<(), String> {
        // The projection lock excludes Rosetta writers. Recheck anyway so an
        // unrelated process cannot silently replace a ledger destination.
        match std::fs::symlink_metadata(&self.destination) {
            Ok(_) => {
                return Err(format!(
                    "ledger destination {} appeared during cold ingest",
                    self.destination.display()
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "restating ledger destination {}: {error}",
                    self.destination.display()
                ));
            }
        }
        std::fs::rename(self.temp.path(), &self.destination).map_err(|error| {
            format!(
                "atomically publishing ledger {}: {error}",
                self.destination.display()
            )
        })?;
        for path in &self.publication_barriers {
            std::fs::File::open(path)
                .and_then(|directory| directory.sync_all())
                .map_err(|error| format!("syncing ledger directory {}: {error}", path.display()))?;
        }
        Ok(())
    }
}

/// Child-first barriers for publishing a new entry in `parent`. If the
/// parent itself must be created, include every new directory plus the first
/// existing ancestor whose child entry changes.
fn directory_publication_barriers(parent: &std::path::Path) -> Vec<PathBuf> {
    let mut barriers = Vec::new();
    let mut cursor = parent;
    while !cursor.exists() {
        barriers.push(cursor.to_path_buf());
        let Some(next) = cursor.parent() else {
            return barriers;
        };
        cursor = next;
    }
    if barriers.is_empty() {
        barriers.push(parent.to_path_buf());
    } else {
        barriers.push(cursor.to_path_buf());
    }
    barriers
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

fn acknowledge_search(store: &mut Store, worker: &Fts5SearchWorker) -> Result<(), String> {
    for ack in worker.drain_acknowledgements() {
        store
            .acknowledge_search(&ack.projection_epoch, ack.revision)
            .map_err(|error| format!("acknowledging search revision: {error}"))?;
    }
    Ok(())
}

fn build_search_wait(db: &std::path::Path) -> Result<rosetta_search::SearchAck, String> {
    let search = Fts5SidecarSearch::new(db);
    let stop = std::sync::atomic::AtomicBool::new(false);
    let ack = synchronize_fts5_once(&search, &stop)
        .map_err(|error| format!("building search projection: {error}"))?
        .ok_or_else(|| "canonical projection is not ready for search".to_string())?;
    if search.state().readiness != SearchReadiness::Ready {
        return Err("search projection did not become ready after synchronization".into());
    }
    Ok(ack)
}

fn synchronize_search_wait(db: &std::path::Path, store: &mut Store) -> Result<(), String> {
    let ack = build_search_wait(db)?;
    store
        .acknowledge_search(&ack.projection_epoch, ack.revision)
        .map_err(|error| format!("acknowledging search projection: {error}"))?;
    Ok(())
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

pub(crate) fn record_backfill_failures(
    kind: &str,
    discovery_truncated: bool,
    meta: &rosetta_pipeline::MetaReport,
    stream: &rosetta_pipeline::StreamReport,
    failures: &mut Vec<String>,
) {
    if discovery_truncated {
        failures.push(format!("{kind} discovery walk was truncated"));
    }
    if meta.walk_truncated {
        failures.push(format!("{kind} meta walk was truncated"));
    }
    if meta.files_unidentified > 0 {
        failures.push(format!(
            "{kind} left {} streamable files unidentified",
            meta.files_unidentified
        ));
    }
    if meta.files_vanished > 0 {
        failures.push(format!(
            "{kind} lost {} files during meta ingest",
            meta.files_vanished
        ));
    }
    if stream.hard_errors > 0
        || stream.oversized > 0
        || stream.files_missing > 0
        || stream.retries_exhausted > 0
        || stream.conflicts > 0
        || stream.files_incomplete > 0
    {
        failures.push(format!(
            "{kind} ingest incomplete: {} hard, {} oversized, {} missing, {} retry-exhausted, {} conflicts, {} incomplete files",
            stream.hard_errors,
            stream.oversized,
            stream.files_missing,
            stream.retries_exhausted,
            stream.conflicts,
            stream.files_incomplete,
        ));
    }
}

fn cmd_ingest(args: &[String]) -> Result<(), String> {
    let flags = parse_flags(args)?;
    let db = flags.db.clone().ok_or("ingest needs --db PATH")?;
    if flags.audit_boundaries && (!flags.json || !flags.profile) {
        return Err("--audit-boundaries requires --json and --profile".to_string());
    }
    if flags.audit_boundaries && db.exists() {
        return Err("--audit-boundaries requires a new private cold projection".to_string());
    }
    let opts = IngestOptions::default();
    let instrument = Instrument::new();
    if flags.audit_boundaries {
        instrument.enable_boundary_evidence();
    }
    let adapters = selected(&flags)?;
    let _projection_lock = projection_lock(&db)?;

    let (mut store, private_projection) = open_for_backfill(&db)?;
    let public_ledgers = ledger_root(&flags, &db);
    // Only a brand-new private projection may defer per-batch ledger sync.
    // Existing projections are public and retain the live durability order.
    let defer_ledger_sync = private_projection.is_some();
    let ledger_stage = defer_ledger_sync
        .then(|| ColdLedgerStage::new(&public_ledgers))
        .transpose()?;
    let ledgers = ledger_stage
        .as_ref()
        .map_or(public_ledgers.as_path(), ColdLedgerStage::path);
    let mut ingest_ledgers = Vec::new();
    let stage_started = Instant::now();
    let stage_before = instrument.snapshot();
    let discoveries =
        discover(&adapters, &flags.home, &opts, &instrument).map_err(|error| error.to_string())?;
    if flags.json && flags.profile {
        bench::emit_stage_profile("discovery", None, stage_started, &stage_before, &instrument);
    }
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
        let ledger = open_ledger(ledgers, adapter.kind())?;
        ingest_ledgers.push(if defer_ledger_sync {
            ledger.defer_sync_until_flush()
        } else {
            ledger
        });
        let ledger = ingest_ledgers
            .last_mut()
            .expect("the runtime ledger was just pushed");

        let stage_started = Instant::now();
        let stage_before = instrument.snapshot();
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
        if flags.json && flags.profile {
            bench::emit_stage_profile(
                "metadata",
                Some(adapter.kind()),
                stage_started,
                &stage_before,
                &instrument,
            );
        }
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

        let stage_started = Instant::now();
        let stage_before = instrument.snapshot();
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
        if flags.json && flags.profile {
            bench::emit_stage_profile(
                "stream",
                Some(adapter.kind()),
                stage_started,
                &stage_before,
                &instrument,
            );
        }
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
        let stage_started = Instant::now();
        let stage_before = instrument.snapshot();
        ingest_ledgers.iter_mut().try_for_each(|ledger| {
            instrument
                .ledger_write(ledger, |ledger| ledger.flush())
                .map_err(|error| error.to_string())
        })?;
        if flags.json && flags.profile {
            bench::emit_stage_profile(
                "ledger-durability",
                None,
                stage_started,
                &stage_before,
                &instrument,
            );
        }
    }

    if !failures.is_empty() {
        if flags.json {
            bench::emit_profile("ingest", &instrument);
        } else if flags.profile {
            println!("\n== profile ==\n{}", instrument.report());
        }
        return Err(failures.join("; "));
    }
    let boundary_before = if flags.audit_boundaries {
        let pipeline = instrument.boundary_evidence().unwrap_or_else(|| {
            serde_json::json!({
                "stream": null,
                "sourceOffset": null,
                "lineStatus": "no non-empty normalized record was available",
                "sampleTimelineId": null,
                "parsedRecord": null,
                "normalizedOps": null,
                "writeBatch": null,
            })
        });
        let timeline_id = pipeline
            .get("sampleTimelineId")
            .and_then(serde_json::Value::as_str);
        let started = Instant::now();
        let snapshot = bench::projection_snapshot(
            store.conn(),
            "after canonical apply / before global finalization",
            timeline_id,
            false,
        )?;
        Some((
            pipeline,
            snapshot,
            started.elapsed().as_secs_f64() * 1_000.0,
        ))
    } else {
        None
    };
    drop(ingest_ledgers);
    if let Some(temp) = private_projection {
        let ledger_stage = ledger_stage.expect("a private projection has a private ledger stage");
        let boundary_timeline_id = boundary_before
            .as_ref()
            .and_then(|(pipeline, _, _)| pipeline.get("sampleTimelineId"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
        let mut boundary_after = None;
        let stage_started = Instant::now();
        let stage_before = instrument.snapshot();
        publish_private_projection(
            store,
            temp,
            &db,
            &instrument,
            |finalized| {
                if flags.audit_boundaries {
                    let started = Instant::now();
                    let snapshot = bench::projection_snapshot(
                        finalized.conn(),
                        "after global finalization / before atomic publication",
                        boundary_timeline_id.as_deref(),
                        true,
                    )?;
                    boundary_after = Some((snapshot, started.elapsed().as_secs_f64() * 1_000.0));
                }
                Ok(())
            },
            || ledger_stage.publish(),
        )?;
        if flags.json && flags.profile {
            bench::emit_stage_profile(
                "finalize-publish",
                None,
                stage_started,
                &stage_before,
                &instrument,
            );
        }
        if let (Some((pipeline, before, before_ms)), Some((after, after_ms))) =
            (boundary_before, boundary_after)
        {
            bench::emit(&bench::boundary_evidence_event(
                &pipeline,
                before,
                after,
                before_ms + after_ms,
            ));
        }
        let stage_started = Instant::now();
        let stage_before = instrument.snapshot();
        instrument.time(Metric::SearchSyncNanos, || build_search_wait(&db))?;
        if flags.json && flags.profile {
            bench::emit_stage_profile("search", None, stage_started, &stage_before, &instrument);
        }
    } else {
        let stage_started = Instant::now();
        let stage_before = instrument.snapshot();
        instrument.time(Metric::SearchSyncNanos, || {
            synchronize_search_wait(&db, &mut store)
        })?;
        if flags.json && flags.profile {
            bench::emit_stage_profile("search", None, stage_started, &stage_before, &instrument);
        }
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

    let mut store = open_progressive_backfill(&db)?;
    let progressive = store.is_progressive();
    let public_ledgers = ledger_root(&flags, &db);
    let search = Fts5SidecarSearch::new(db.clone());
    let search_worker = Fts5SearchWorker::start(search);
    let discoveries =
        discover(&adapters, &flags.home, &opts, &instrument).map_err(|error| error.to_string())?;

    let mut pending_runners = Vec::new();
    let mut failures = Vec::new();
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
        let mut ledger = open_ledger(&public_ledgers, adapter.kind())?;

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
        search_worker.wake();
        acknowledge_search(&mut store, &search_worker)?;
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
        return Err("nothing to watch: no installed runtimes".to_string());
    }
    if !failures.is_empty() {
        return Err(failures.join("; "));
    }
    for (_, _, _, _, ledger) in &mut pending_runners {
        let ledger = ledger.as_mut().expect("a watch runner owns its ledger");
        instrument
            .ledger_write(ledger, |ledger| ledger.flush())
            .map_err(|error| error.to_string())?;
    }
    if progressive {
        instrument
            .finish_progressive_load(&mut store)
            .map_err(|error| format!("finalizing progressive projection: {error}"))?;
    }
    search_worker.wake();

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
        search_worker.wake();
        acknowledge_search(&mut store, &search_worker)?;
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
    acknowledge_search(&mut store, &search_worker)?;
    search_worker.stop();
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

/// Open the installed projection itself for a progressive first run. Unlike
/// private `ingest`/`rebuild`, every committed source is immediately readable;
/// durable projection_state records whether the final canonical gate has run.
fn open_progressive_backfill(db: &std::path::Path) -> Result<Store, String> {
    let parent = projection_parent(db);
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("creating {}: {error}", parent.display()))?;
    Store::open_progressive(db)
        .map_err(|error| format!("opening progressive {}: {error:?}", db.display()))
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
    let store = Store::open_for_rebuild_with_assets(temp.as_ref(), db)
        .map_err(|error| format!("opening backfill database {}: {error:?}", temp.display()))?;
    Ok((store, Some(temp)))
}

fn publish_private_projection(
    mut store: Store,
    temp: tempfile::TempPath,
    db: &std::path::Path,
    instrument: &Instrument,
    after_finalize: impl FnOnce(&Store) -> Result<(), String>,
    before_install: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    instrument
        .finish_bulk_load(&mut store)
        .map_err(|error| format!("finalizing private projection: {error}"))?;
    after_finalize(&store)?;
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
        before_install()?;
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
    let mut store = Store::open_for_rebuild_with_assets(temp.as_ref(), &db)
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

    publish_private_projection(store, temp, &db, &instrument, |_| Ok(()), || Ok(()))?;
    instrument.time(Metric::SearchSyncNanos, || build_search_wait(&db))?;

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

    #[test]
    fn cold_ledger_stage_is_private_until_atomic_promotion() {
        let root = tempfile::tempdir().unwrap();
        let destination = root.path().join("projection.ledger");
        let stage = ColdLedgerStage::new(&destination).unwrap();
        let staged_path = stage.path().to_path_buf();
        let mut ledger = Ledger::open(&staged_path.join("codex"))
            .unwrap()
            .defer_sync_until_flush();
        ledger
            .append_meta(rosetta_pipeline::LEDGER_META_SCHEMA_VERSION, "[]")
            .unwrap();
        ledger.checkpoint().unwrap();
        assert!(!destination.exists());
        ledger.flush().unwrap();
        drop(ledger);

        stage.publish().unwrap();
        assert!(!staged_path.exists());
        assert!(destination.join("codex/00000000.rledger").is_file());
        assert!(ColdLedgerStage::new(&destination).is_err());
    }

    #[test]
    fn cold_ledger_stage_refuses_a_preexisting_empty_destination() {
        let root = tempfile::tempdir().unwrap();
        let destination = root.path().join("projection.ledger");
        std::fs::create_dir(&destination).unwrap();
        let error = ColdLedgerStage::new(&destination).unwrap_err();
        assert!(error.contains("use `rosettad rebuild`"));
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

        publish_private_projection(
            store,
            private,
            &db,
            &Instrument::new(),
            |_| Ok(()),
            || Ok(()),
        )
        .unwrap();
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

    #[test]
    fn slice_budget_sizes_parse_with_suffixes() {
        assert_eq!(parse_byte_size("0").unwrap(), 0);
        assert_eq!(parse_byte_size("4096").unwrap(), 4096);
        assert_eq!(parse_byte_size("256k").unwrap(), 256 * 1024);
        assert_eq!(parse_byte_size("1M").unwrap(), 1024 * 1024);
        assert!(parse_byte_size("many").is_err());
        assert!(parse_byte_size("1g").is_err());
    }
}
