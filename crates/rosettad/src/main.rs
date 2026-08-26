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
use rosetta_pipeline::{IngestOptions, Instrument, discover, meta_ingest, stream_backfill};
use rosetta_store::Store;

fn registry() -> Vec<&'static dyn AgentAdapter> {
    vec![&ClaudeCodeAdapter, &CodexAdapter]
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("discover") => cmd_discover(&args[1..]),
        Some("ingest") => cmd_ingest(&args[1..]),
        _ => {
            eprintln!("usage: rosettad <discover|ingest> [options]");
            eprintln!("  discover [--home DIR]");
            eprintln!("  ingest --db PATH [--home DIR] [--agent KIND ...] [--profile]");
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
}

fn parse_flags(args: &[String]) -> Result<Flags, String> {
    let mut flags = Flags {
        home: default_home()?,
        db: None,
        agents: Vec::new(),
        profile: false,
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

    for (adapter, discovery) in adapters.iter().zip(&discoveries) {
        if !discovery.installed {
            println!("{}: not installed, skipping", adapter.kind());
            continue;
        }
        let mut runtime = discovery.runtime.clone();
        runtime.enabled = true; // CLI ingest is explicit consent

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
            "{}: meta — {} sessions, {} projects, {} files ({} unidentified)",
            adapter.kind(),
            meta.sessions,
            meta.projects,
            meta.files_matched,
            meta.files_unidentified,
        );

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
    }

    if flags.profile {
        println!("\n== profile ==\n{}", instrument.report());
    }
    Ok(())
}
