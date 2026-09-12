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
use rusqlite::{Connection, OptionalExtension, Row};
use serde_json::{Value, json};

use crate::runtime_ledger::open_runtime_ledger;
use crate::{ledger_root, parse_flags, projection_lock, selected};

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

pub(crate) fn emit_command_stage_profile(
    command: &str,
    stage: &str,
    agent: Option<&str>,
    started: Instant,
    before: &InstrumentSnapshot,
    instrument: &Instrument,
) {
    let delta = instrument.snapshot().delta_since(before);
    emit(&json!({
        "event": "stage-profile",
        "command": command,
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

const DEFERRED_INDEXES: [&str; 10] = [
    "session_project",
    "session_runtime",
    "session_parent",
    "task_session",
    "message_bucket_key",
    "session_usage_bucket_key",
    "file_edit_session",
    "file_edit_path",
    "file_edit_message",
    "usage_bucket_hour",
];

fn table_count(connection: &Connection, table: &str) -> Result<i64, String> {
    connection
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .map_err(|error| format!("counting {table} for boundary evidence: {error}"))
}

fn text_preview(value: Option<String>) -> Value {
    let Some(value) = value else {
        return Value::Null;
    };
    const LIMIT: usize = 8 * 1024;
    let bytes = value.len();
    let mut end = bytes.min(LIMIT);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    json!({
        "bytes": bytes,
        "truncated": end < bytes,
        "text": if end < bytes {
            format!("{}\n… bounded SQLite value truncated …", &value[..end])
        } else {
            value
        },
    })
}

fn message_value(row: &Row<'_>) -> rusqlite::Result<Value> {
    let detail: Option<Value> = row
        .get::<_, Option<String>>(16)?
        .and_then(|value| serde_json::from_str(&value).ok());
    let tool_detail: Option<Value> = row
        .get::<_, Option<String>>(26)?
        .and_then(|value| serde_json::from_str(&value).ok());
    Ok(json!({
        "timelineId": row.get::<_, String>(0)?,
        "sessionId": row.get::<_, String>(1)?,
        "ordinal": row.get::<_, i64>(2)?,
        "group": {
            "id": row.get::<_, String>(3)?,
            "kind": row.get::<_, String>(4)?,
            "derivation": row.get::<_, String>(5)?,
        },
        "response": {
            "id": row.get::<_, Option<String>>(6)?,
            "derivation": row.get::<_, Option<String>>(7)?,
        },
        "blockIndex": row.get::<_, Option<i64>>(8)?,
        "type": row.get::<_, String>(9)?,
        "uuid": row.get::<_, Option<String>>(10)?,
        "parentUuid": row.get::<_, Option<String>>(11)?,
        "ts": row.get::<_, Option<i64>>(12)?,
        "model": row.get::<_, Option<String>>(13)?,
        "effort": row.get::<_, Option<String>>(14)?,
        "content": text_preview(row.get::<_, Option<String>>(15)?),
        "detail": detail,
        "tool": {
            "correlationKey": row.get::<_, Option<String>>(17)?,
            "correlationKind": row.get::<_, Option<String>>(18)?,
            "nativeId": row.get::<_, Option<String>>(19)?,
            "namespace": row.get::<_, Option<String>>(20)?,
            "provider": row.get::<_, Option<String>>(21)?,
            "name": row.get::<_, Option<String>>(22)?,
            "kind": row.get::<_, Option<String>>(23)?,
            "status": row.get::<_, Option<String>>(24)?,
            "statusBasis": row.get::<_, Option<String>>(25)?,
            "detail": tool_detail,
            "failure": {
                "kind": row.get::<_, Option<String>>(27)?,
                "summary": row.get::<_, Option<String>>(28)?,
                "diagnostic": text_preview(row.get::<_, Option<String>>(29)?),
            },
            "timing": {
                "startedAt": row.get::<_, Option<i64>>(30)?,
                "completedAt": row.get::<_, Option<i64>>(31)?,
                "durationMs": row.get::<_, Option<i64>>(32)?,
            },
        },
        "parentToolNativeId": row.get::<_, Option<String>>(33)?,
        "usage": {
            "input": row.get::<_, Option<i64>>(34)?,
            "output": row.get::<_, Option<i64>>(35)?,
            "cacheRead": row.get::<_, Option<i64>>(36)?,
            "cacheWrite": row.get::<_, Option<i64>>(37)?,
            "thinking": row.get::<_, Option<i64>>(38)?,
        },
        "agentVersion": row.get::<_, Option<String>>(39)?,
        "srcCursor": row.get::<_, Option<i64>>(40)?,
        "srcOffset": row.get::<_, Option<i64>>(41)?,
    }))
}

fn sample_message(
    connection: &Connection,
    timeline_id: Option<&str>,
) -> Result<Option<Value>, String> {
    const COLUMNS: &str = "timeline_id, session_id, ordinal,
        group_id, group_kind, group_derivation, response_id, response_derivation, block_index,
        type, uuid, parent_uuid, ts, model, effort, content, detail,
        tool_correlation_key, tool_correlation_kind, tool_native_id,
        tool_namespace, tool_provider, tool_name, tool_kind, tool_status, tool_status_basis,
        tool_detail, tool_error_kind, tool_error_summary, tool_diagnostic, tool_started_at,
        tool_completed_at, tool_duration_ms, parent_tool_native_id, in_tokens, out_tokens,
        cache_r_tokens, cache_w_tokens, think_tokens, agent_version, src_cursor, src_offset";
    let exact = match timeline_id {
        Some(timeline_id) => connection
            .query_row(
                &format!("SELECT {COLUMNS} FROM message WHERE timeline_id = ?1"),
                [timeline_id],
                message_value,
            )
            .optional()
            .map_err(|error| format!("reading sampled canonical message: {error}"))?,
        None => None,
    };
    if exact.is_some() {
        return Ok(exact);
    }
    connection
        .query_row(
            &format!("SELECT {COLUMNS} FROM message ORDER BY id LIMIT 1"),
            [],
            message_value,
        )
        .optional()
        .map_err(|error| format!("reading fallback canonical message: {error}"))
}

/// A bounded structural/content snapshot on either side of global
/// finalization. It is diagnostic evidence only and is never a projection API.
pub(crate) fn projection_snapshot(
    connection: &Connection,
    phase: &str,
    timeline_id: Option<&str>,
    validated: bool,
) -> Result<Value, String> {
    let mut index_names = Vec::new();
    let mut statement = connection
        .prepare("SELECT name FROM sqlite_master WHERE type = 'index' ORDER BY name")
        .map_err(|error| format!("preparing boundary index inventory: {error}"))?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|error| format!("reading boundary index inventory: {error}"))?;
    for row in rows {
        index_names.push(row.map_err(|error| format!("decoding boundary index name: {error}"))?);
    }
    let deferred_indexes: Vec<&str> = DEFERRED_INDEXES
        .iter()
        .copied()
        .filter(|name| index_names.iter().any(|candidate| candidate == name))
        .collect();
    let analyzed = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'sqlite_stat1')",
            [],
            |row| row.get::<_, bool>(0),
        )
        .map_err(|error| format!("checking ANALYZE evidence: {error}"))?;
    let analyze_rows = if analyzed {
        connection
            .query_row("SELECT COUNT(*) FROM sqlite_stat1", [], |row| {
                row.get::<_, i64>(0)
            })
            .map_err(|error| format!("counting ANALYZE evidence: {error}"))?
    } else {
        0
    };
    let (epoch, revision, canonical_ready): (String, i64, bool) = connection
        .query_row(
            "SELECT projection_epoch, search_revision, canonical_ready
             FROM projection_state WHERE singleton = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(|error| format!("reading projection state for boundary evidence: {error}"))?;
    let page_count: i64 = connection
        .query_row("PRAGMA page_count", [], |row| row.get(0))
        .map_err(|error| format!("reading page count for boundary evidence: {error}"))?;
    let page_size: i64 = connection
        .query_row("PRAGMA page_size", [], |row| row.get(0))
        .map_err(|error| format!("reading page size for boundary evidence: {error}"))?;

    Ok(json!({
        "phase": phase,
        "databaseBytes": page_count.saturating_mul(page_size),
        "counts": {
            "runtimes": table_count(connection, "agent_runtime")?,
            "projects": table_count(connection, "project")?,
            "sessions": table_count(connection, "session")?,
            "messages": table_count(connection, "message")?,
            "fileEdits": table_count(connection, "file_edit")?,
            "plans": table_count(connection, "plan")?,
            "tasks": table_count(connection, "task")?,
            "gitCommits": table_count(connection, "git_commit")?,
            "driftAggregates": table_count(connection, "drift_agg")?,
            "sessionUsageSamples": table_count(connection, "session_usage")?,
            "usageBuckets": table_count(connection, "usage_bucket")?,
            "pendingMerges": 0,
            "stagedSourceOps": 0,
        },
        "reconciliationStorage": "ram_only",
        "projectionState": {
            "epoch": epoch,
            "searchRevision": revision,
            "canonicalReady": canonical_ready,
        },
        "materialization": {
            "allIndexes": index_names.len(),
            "deferredIndexesPresent": deferred_indexes,
            "deferredIndexesExpected": DEFERRED_INDEXES.len(),
            "analyzeRows": analyze_rows,
        },
        "integrity": if validated {
            json!({"quickCheck": "ok", "foreignKeyViolations": 0})
        } else {
            json!({"quickCheck": "not run yet", "foreignKeyCheck": "not run yet"})
        },
        "sampleMessage": sample_message(connection, timeline_id)?,
    }))
}

fn projection_block(representation: &str, snapshot: Value) -> Value {
    let bytes = snapshot
        .get("databaseBytes")
        .cloned()
        .unwrap_or(Value::Null);
    json!({
        "representation": representation,
        "bytes": bytes,
        "previewBytes": Value::Null,
        "truncated": false,
        "data": snapshot,
    })
}

pub(crate) fn boundary_evidence_event(
    pipeline: &Value,
    before_finalize: Value,
    after_finalize: Value,
    diagnostic_overhead_ms: f64,
) -> Value {
    let parsed = pipeline.get("parsedRecord").cloned().unwrap_or(Value::Null);
    let ops = pipeline
        .get("normalizedOps")
        .cloned()
        .unwrap_or(Value::Null);
    let batch = pipeline.get("writeBatch").cloned().unwrap_or(Value::Null);
    let pre = projection_block("PRIVATE SQLite before global finalization", before_finalize);
    let post = projection_block("validated canonical SQLite", after_finalize);
    json!({
        "event": "boundary-evidence",
        "command": "ingest",
        "diagnosticOverheadMs": diagnostic_overhead_ms,
        "sample": {
            "stream": pipeline.get("stream"),
            "sourceOffset": pipeline.get("sourceOffset"),
            "lineStatus": pipeline.get("lineStatus"),
            "timelineId": pipeline.get("sampleTimelineId"),
        },
        "steps": [
            {
                "step": 5,
                "name": "normalize (including parse)",
                "before": parsed,
                "after": ops,
                "note": "The before pane is a generic parsed view of the exact native bytes. The adapter's typed ParsedRecord is internal to ingest_line, so parse and normalization remain one production call."
            },
            {
                "step": 6,
                "name": "fold / merge",
                "before": ops,
                "after": batch,
                "note": "The real next materialized value is WriteBatch after workspace enrichment. Tool-result, usage, pending-record, and identity merges do not produce a separate folded object; they complete transactionally in step 7."
            },
            {
                "step": 7,
                "name": "canonical SQLite apply",
                "before": batch,
                "after": pre,
                "note": "The sampled batch has committed with its checkpoint. This private projection is transactionally consistent but still has provisional global presentation state."
            },
            {
                "step": 8,
                "name": "global finalization",
                "before": pre,
                "after": post,
                "note": "The same private SQLite projection is shown on both sides: finalization renumbers timelines, rebuilds usage buckets, creates deferred indexes, runs ANALYZE, marks canonical readiness, and validates integrity."
            }
        ]
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
        let mut ledger = open_runtime_ledger(&ledgers, &runtime)?;
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

    #[test]
    fn projection_snapshot_exposes_the_real_global_finalization_delta() {
        let mut store = Store::open_in_memory_for_rebuild().unwrap();
        let before = projection_snapshot(store.conn(), "before", None, false).unwrap();
        assert_eq!(before["projectionState"]["canonicalReady"], false);
        assert_eq!(
            before["materialization"]["deferredIndexesPresent"]
                .as_array()
                .unwrap()
                .len(),
            0
        );
        assert_eq!(before["integrity"]["quickCheck"], "not run yet");

        store.finish_bulk_load().unwrap();
        let after = projection_snapshot(store.conn(), "after", None, true).unwrap();
        assert_eq!(after["projectionState"]["canonicalReady"], true);
        assert_eq!(
            after["materialization"]["deferredIndexesPresent"]
                .as_array()
                .unwrap()
                .len(),
            DEFERRED_INDEXES.len()
        );
        assert_eq!(after["integrity"]["quickCheck"], "ok");
    }
}
