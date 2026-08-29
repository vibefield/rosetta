//! Diagnostic CLI seams used by `tools/ingest-bench`.
//!
//! This module only orchestrates the production pipeline. It deliberately
//! owns no agent paths, parsers, or projection SQL.

use std::path::Path;
use std::time::Instant;

use rosetta_adapter_support::{AgentAdapter, FileDiscipline, FileFormat, StreamDriver};
use rosetta_pipeline::{
    IngestOptions, Instrument, InstrumentSnapshot, MetaReport, expand_data_root, meta_ingest_into,
};
use rosetta_store::Store;
use serde_json::{Value, json};

use crate::{ledger_root, open_ledger, parse_flags, projection_lock, selected};

pub(crate) fn emit(event: &Value) {
    println!("{event}");
}

pub(crate) fn emit_profile(command: &str, instrument: &Instrument) {
    emit(&json!({
        "event": "profile",
        "command": command,
        "report": instrument.report(),
    }));
}

/// Emit one wall-clock boundary together with the profiler counters accrued
/// inside it. The wall clock is the stage headline; the counter delta explains
/// where that time went and may contain documented parent/child overlaps.
pub(crate) fn emit_stage_profile(
    stage: &str,
    agent: Option<&str>,
    started: Instant,
    before: &InstrumentSnapshot,
    instrument: &Instrument,
) {
    let delta = instrument.snapshot().delta_since(before);
    emit(&json!({
        "event": "stage-profile",
        "command": "ingest",
        "stage": stage,
        "agent": agent,
        "wallMs": started.elapsed().as_secs_f64() * 1_000.0,
        "report": delta.report(),
    }));
}

fn driver_name(driver: StreamDriver) -> &'static str {
    match driver {
        StreamDriver::JsonLines => "json-lines",
        StreamDriver::DocumentSnapshot => "document-snapshot",
        StreamDriver::SqliteSnapshot => "sqlite-snapshot",
    }
}

fn format_name(format: FileFormat) -> &'static str {
    match format {
        FileFormat::Jsonl => "jsonl",
        FileFormat::Json => "json",
        FileFormat::Toml => "toml",
        FileFormat::Markdown => "markdown",
        FileFormat::Sqlite => "sqlite",
        FileFormat::Text => "text",
    }
}

fn discipline_name(discipline: FileDiscipline) -> &'static str {
    match discipline {
        FileDiscipline::AppendOnly => "append-only",
        FileDiscipline::RewriteInPlace => "rewrite-in-place",
        FileDiscipline::Database => "database",
    }
}

/// Describe the adapter streams which have production drivers. Audit tools
/// use this to ask the adapter how an explicitly selected file may be staged.
pub(crate) fn cmd_streams(args: &[String]) -> Result<(), String> {
    let flags = parse_flags(args)?;
    let adapters = selected(&flags)?;
    if !flags.json {
        println!("{:<14} {:<24} {:<20}  glob", "agent", "stream", "driver");
    }
    for adapter in adapters {
        let layout = adapter.layout();
        for stream in &layout.streams {
            let Some(driver) = adapter.stream_driver(&stream.id) else {
                continue;
            };
            if flags.json {
                emit(&json!({
                    "event": "stream-spec",
                    "agent": adapter.kind(),
                    "dataRoot": layout.data_root,
                    "stream": stream.id,
                    "glob": stream.glob,
                    "driver": driver_name(driver),
                    "fileFormat": format_name(stream.file_format),
                    "discipline": discipline_name(stream.discipline),
                }));
            } else {
                println!(
                    "{:<14} {:<24} {:<20}  {}",
                    adapter.kind(),
                    stream.id,
                    driver_name(driver),
                    stream.glob,
                );
            }
        }
    }
    Ok(())
}

fn admitted_sources(
    adapter: &dyn AgentAdapter,
    root: &Path,
    opts: &IngestOptions,
) -> Result<rosetta_capture::WalkReport, String> {
    let layout = adapter.layout();
    rosetta_capture::walk_filtered(root, opts.max_files, |relative_path| {
        layout
            .matching_stream(relative_path)
            .is_some_and(|stream| adapter.stream_driver(&stream.id).is_some())
    })
    .map_err(|error| error.to_string())
}

/// List only source files admitted by an adapter driver. This keeps local
/// benchmark tools out of agent-specific path and stream-id knowledge.
pub(crate) fn cmd_sources(args: &[String]) -> Result<(), String> {
    let flags = parse_flags(args)?;
    let opts = IngestOptions::default();
    let adapters = selected(&flags)?;
    let mut truncated = Vec::new();

    if !flags.json {
        println!("{:<14} {:<20} {:>14}  path", "agent", "driver", "bytes");
    }
    for adapter in adapters {
        let layout = adapter.layout();
        let root = expand_data_root(&layout.data_root, &flags.home);
        let report = admitted_sources(adapter, &root, &opts)?;
        if report.truncated {
            truncated.push(adapter.kind());
        }
        for file in report.files {
            let stream = layout
                .matching_stream(&file.relative_path)
                .expect("the admitted-source filter matched this path");
            let driver = adapter
                .stream_driver(&stream.id)
                .expect("the admitted-source filter required a driver");
            if flags.json {
                emit(&json!({
                    "event": "source",
                    "agent": adapter.kind(),
                    "dataRoot": layout.data_root,
                    "resolvedRoot": root,
                    "relativePath": file.relative_path,
                    "stream": stream.id,
                    "driver": driver_name(driver),
                    "bytes": file.size,
                    "modifiedMs": file.modified_ms,
                }));
            } else {
                println!(
                    "{:<14} {:<20} {:>14}  {}",
                    adapter.kind(),
                    driver_name(driver),
                    file.size,
                    file.relative_path,
                );
            }
        }
    }

    if truncated.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "admitted-source coverage was truncated for {}",
            truncated.join(", ")
        ))
    }
}

pub(crate) fn meta_event(agent: &str, meta: &MetaReport) -> Value {
    json!({
        "event": "meta",
        "agent": agent,
        "sessions": meta.sessions,
        "projects": meta.projects,
        "filesMatched": meta.files_matched,
        "filesInventoryOnly": meta.files_inventory_only,
        "filesSkimmed": meta.files_skimmed,
        "filesUnidentified": meta.files_unidentified,
        "filesVanished": meta.files_vanished,
        "walkTruncated": meta.walk_truncated,
    })
}

pub(crate) fn cmd_discover(args: &[String]) -> Result<(), String> {
    let flags = parse_flags(args)?;
    let opts = rosetta_pipeline::IngestOptions::default();
    let instrument = Instrument::new();
    let discoveries =
        rosetta_pipeline::discover(&selected(&flags)?, &flags.home, &opts, &instrument)
            .map_err(|error| error.to_string())?;

    if !flags.json {
        println!(
            "{:<14} {:>10} {:>8} {:>14} {:>22}",
            "agent", "installed", "files", "bytes", "newest activity (ms)"
        );
    }
    for discovery in &discoveries {
        if flags.json {
            emit(&json!({
                "event": "discovery",
                "agent": discovery.runtime.kind,
                "dataRoot": discovery.runtime.data_root,
                "resolvedRoot": discovery.root,
                "installed": discovery.installed,
                "files": discovery.file_count,
                "bytes": discovery.total_bytes,
                "newestActivityMs": discovery.newest_modified_ms,
                "walkTruncated": discovery.walk_truncated,
            }));
        } else {
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
    if flags.json {
        emit_profile("discover", &instrument);
    }
    Ok(())
}

fn open_meta_store(db: &Path) -> Result<Store, String> {
    if let Some(parent) = db.parent().filter(|path| !path.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("creating {}: {error}", parent.display()))?;
    }
    Store::open(db).map_err(|error| format!("opening {}: {error:?}", db.display()))
}

/// Run only discovery and bounded metadata ingestion. The caller should use
/// a scratch database: this is an isolated phase measurement, not a shortcut
/// for a later stream ingest. The normal `ingest` command remains the one
/// atomic cold-publication path.
pub(crate) fn cmd_meta(args: &[String]) -> Result<(), String> {
    let flags = parse_flags(args)?;
    let db = flags.db.clone().ok_or("meta needs --db PATH")?;
    let opts = rosetta_pipeline::IngestOptions::default();
    let instrument = Instrument::new();
    let adapters = selected(&flags)?;
    let ledgers = ledger_root(&flags, &db);
    if db.exists() || ledgers.exists() {
        return Err(format!(
            "meta is an isolated benchmark and needs new paths; {} or {} already exists",
            db.display(),
            ledgers.display()
        ));
    }
    let _projection_lock = projection_lock(&db)?;
    let mut store = open_meta_store(&db)?;
    let discoveries = rosetta_pipeline::discover(&adapters, &flags.home, &opts, &instrument)
        .map_err(|error| error.to_string())?;
    let mut failures = Vec::new();

    for (adapter, discovery) in adapters.iter().zip(&discoveries) {
        if !discovery.installed {
            if flags.json {
                emit(&json!({
                    "event": "skipped",
                    "phase": "meta",
                    "agent": adapter.kind(),
                    "reason": "not-installed",
                }));
            } else {
                println!("{}: not installed, skipping", adapter.kind());
            }
            continue;
        }

        let mut runtime = discovery.runtime.clone();
        runtime.enabled = true;
        let mut ledger = open_ledger(&ledgers, adapter.kind())?;
        let (meta, _) = meta_ingest_into(
            *adapter,
            &runtime,
            &discovery.root,
            &mut store,
            &opts,
            &instrument,
            Some(&mut ledger),
        )
        .map_err(|error| error.to_string())?;

        if flags.json {
            emit(&meta_event(adapter.kind(), &meta));
        } else {
            println!(
                "{}: meta — {} sessions, {} projects, {} files ({} skimmed, {} inventory-only, {} unidentified)",
                adapter.kind(),
                meta.sessions,
                meta.projects,
                meta.files_matched,
                meta.files_skimmed,
                meta.files_inventory_only,
                meta.files_unidentified,
            );
        }

        if discovery.walk_truncated || meta.walk_truncated {
            failures.push(format!("{} metadata walk was truncated", adapter.kind()));
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
    }

    if flags.json {
        emit_profile("meta", &instrument);
    } else if flags.profile {
        println!("\n== profile ==\n{}", instrument.report());
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

#[cfg(test)]
mod tests {
    use rosetta_adapter_claude_code::ClaudeCodeAdapter;

    use super::*;

    #[test]
    fn admitted_source_inventory_uses_the_adapter_driver_boundary() {
        let temp = tempfile::tempdir().unwrap();
        let transcript = temp.path().join("projects/work/session.jsonl");
        let prompt_history = temp.path().join("history.jsonl");
        let state = temp.path().join(".claude.json");
        std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
        std::fs::write(&transcript, b"{}\n").unwrap();
        std::fs::write(&prompt_history, b"{}\n").unwrap();
        std::fs::write(&state, b"{}").unwrap();

        let report =
            admitted_sources(&ClaudeCodeAdapter, temp.path(), &IngestOptions::default()).unwrap();

        assert!(!report.truncated);
        assert_eq!(report.files.len(), 1);
        assert_eq!(report.files[0].relative_path, "projects/work/session.jsonl");
    }
}
