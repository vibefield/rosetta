//! rosettad — daemon + CLI binary. For now: the ingest phases as commands.
//!
//!   rosettad discover [--home DIR]
//!   rosettad ingest --db PATH [--home DIR] [--agent KIND ...] [--profile]
//!
//! `ingest` implies consent for the selected agents (the CLI user asked);
//! the app flow instead runs `discover`, asks, and flips `enabled` itself.

use std::path::PathBuf;
use std::process::ExitCode;

use rosetta_adapter_claude_code::ClaudeCodeAdapter;
use rosetta_adapter_codex::CodexAdapter;
use rosetta_adapter_support::AgentAdapter;
use rosetta_capture::NotifyEvents;
use rosetta_pipeline::{
    IngestOptions, Instrument, LiveOptions, LiveRunner, discover, meta_ingest, stream_backfill,
};
use rosetta_store::Store;

fn registry() -> Vec<&'static dyn AgentAdapter> {
    vec![&ClaudeCodeAdapter, &CodexAdapter]
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("discover") => cmd_discover(&args[1..]),
        Some("ingest") => cmd_ingest(&args[1..]),
        Some("watch") => cmd_watch(&args[1..]),
        _ => {
            eprintln!("usage: rosettad <discover|ingest|watch> [options]");
            eprintln!("  discover [--home DIR]");
            eprintln!("  ingest --db PATH [--home DIR] [--agent KIND ...] [--profile]");
            eprintln!("  watch --db PATH [--home DIR] [--agent KIND ...] [--ticks N] [--profile]");
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
    ticks: Option<u64>,
}

fn parse_flags(args: &[String]) -> Result<Flags, String> {
    let mut flags = Flags {
        home: default_home()?,
        db: None,
        agents: Vec::new(),
        profile: false,
        ticks: None,
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

fn cmd_discover(args: &[String]) -> Result<(), String> {
    let flags = parse_flags(args)?;
    let opts = IngestOptions::default();
    let instrument = Instrument::new();
    let discoveries = discover(&selected(&flags)?, &flags.home, &opts, &instrument)
        .map_err(|error| error.to_string())?;

    println!(
        "{:<14} {:>10} {:>8} {:>14} {:>22}",
        "agent", "installed", "files", "bytes", "newest activity (ms)"
    );
    for discovery in &discoveries {
        println!(
            "{:<14} {:>10} {:>8} {:>14} {:>22}{}",
            discovery.runtime.kind,
            if discovery.installed { "yes" } else { "no" },
            discovery.file_count,
            discovery.total_bytes,
            discovery.newest_modified_ms,
            if discovery.walk_truncated {
                "  (walk truncated)"
            } else {
                ""
            },
        );
    }
    let truncated: Vec<&str> = discoveries
        .iter()
        .filter(|discovery| discovery.walk_truncated)
        .map(|discovery| discovery.runtime.kind.as_str())
        .collect();
    if !truncated.is_empty() {
        return Err(format!(
            "discovery coverage was truncated for {}",
            truncated.join(", ")
        ));
    }
    Ok(())
}

fn cmd_ingest(args: &[String]) -> Result<(), String> {
    let flags = parse_flags(args)?;
    let db = flags.db.clone().ok_or("ingest needs --db PATH")?;
    let opts = IngestOptions::default();
    let instrument = Instrument::new();
    let adapters = selected(&flags)?;

    let mut store =
        Store::open(&db).map_err(|error| format!("opening {}: {error:?}", db.display()))?;
    let discoveries =
        discover(&adapters, &flags.home, &opts, &instrument).map_err(|error| error.to_string())?;
    let mut failures = Vec::new();

    for (adapter, discovery) in adapters.iter().zip(&discoveries) {
        if !discovery.installed {
            println!("{}: not installed, skipping", adapter.kind());
            continue;
        }
        let mut runtime = discovery.runtime.clone();
        runtime.enabled = true; // CLI ingest is explicit consent
        if discovery.walk_truncated {
            failures.push(format!("{} discovery walk was truncated", adapter.kind()));
        }

        let (meta, files) = meta_ingest(
            *adapter,
            &runtime,
            &discovery.root,
            &mut store,
            &opts,
            &instrument,
        )
        .map_err(|error| error.to_string())?;
        println!(
            "{}: meta — {} sessions, {} projects, {} files ({} inventory-only, {} unidentified)",
            adapter.kind(),
            meta.sessions,
            meta.projects,
            meta.files_matched,
            meta.files_inventory_only,
            meta.files_unidentified,
        );
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

        let stream = stream_backfill(
            *adapter,
            &runtime,
            &discovery.root,
            &mut store,
            &files,
            &opts,
            &instrument,
        )
        .map_err(|error| error.to_string())?;
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

    if flags.profile {
        println!("\n== profile ==\n{}", instrument.report());
    }
    if !failures.is_empty() {
        return Err(failures.join("; "));
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

    let mut store =
        Store::open(&db).map_err(|error| format!("opening {}: {error:?}", db.display()))?;
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

        let (meta, files) = meta_ingest(
            *adapter,
            &runtime,
            &discovery.root,
            &mut store,
            &opts,
            &instrument,
        )
        .map_err(|error| error.to_string())?;
        let stream = stream_backfill(
            *adapter,
            &runtime,
            &discovery.root,
            &mut store,
            &files,
            &opts,
            &instrument,
        )
        .map_err(|error| error.to_string())?;
        println!(
            "{}: backfilled {} sessions ({} rows) — watching {}",
            adapter.kind(),
            meta.sessions,
            stream.stats.messages_inserted,
            discovery.root.display(),
        );

        let runner = LiveRunner::start(
            *adapter,
            runtime,
            discovery.root.clone(),
            Box::new(events),
            &mut store,
            opts.clone(),
            live.clone(),
            &instrument,
        )
        .map_err(|error| error.to_string())?;
        runners.push((adapter.kind(), runner, 0_u64));
    }
    if runners.is_empty() {
        return Err("nothing to watch: no installed runtimes".to_string());
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
