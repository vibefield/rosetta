//! A streamed runtime's admission at its producer's close (R1 brief §4.7,
//! "Runtime close"): its ready trees have already flipped (`flips.rs`);
//! everything left — bound and tainted trees, trees that closed but never
//! flipped, and the whole of a runtime admitted whole — is folded,
//! renumbered and flipped in one writer transaction, and published in R1
//! D10's order for the sessions that commit moved.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use rosetta_pipeline::StreamedRuntime;
use rosetta_server::BackfillState;
use serde_json::json;

use super::{ClosedColdRuntime, StreamedCold, cpu_millis, millis};
use crate::engine::{absorb_around, now_ms};
use crate::ingest_metrics::ColdMilestones;
use crate::maintenance_pace::thread_cpu_time;
use crate::{AppliedColdRuntime, bench, identity_name, record_fresh_failures};

/// Admit one closed runtime — everything of it no tree flip made readable
/// already: fold, canonical ordinals, the Pending flip, then the same
/// publication steps the retained path runs after an apply. `applied_cpu`
/// is the writer CPU its streamed sources took. A runtime whose capture
/// recorded a refusal is not admitted; trees it flipped before stay readable
/// (R1 D11).
pub(super) fn admit(
    runtime: ClosedColdRuntime,
    mut streamed: StreamedRuntime,
    applied_cpu: Duration,
    ownership: &BTreeMap<String, Option<String>>,
    milestones: &mut ColdMilestones,
    cx: &mut StreamedCold<'_>,
    failures: &mut Vec<String>,
) -> Result<Option<AppliedColdRuntime>, String> {
    let name = identity_name(&runtime.identity);
    let mut runtime_failures = Vec::new();
    let tolerated = record_fresh_failures(
        &name,
        runtime.discovery_truncated,
        &runtime.meta,
        &runtime.stream,
        &mut runtime_failures,
    );
    if !runtime_failures.is_empty() {
        // Streamed rows stay behind the Pending barrier; the build fails
        // before anything is published.
        failures.extend(runtime_failures);
        return Ok(None);
    }
    if let Some(line) = tolerated {
        println!("{line}");
    }
    let runtime_id = runtime.runtime.id.clone();
    let kind = runtime.adapter.kind();
    cx.health
        .ingest_runtime_stage(&runtime_id, BackfillState::Applying);
    cx.instrument.merge_snapshot(&runtime.profile);
    let stage_profiles = cx.flags.json && cx.flags.profile;
    if cx.flags.json {
        bench::emit(&bench::meta_event(kind, &runtime.meta));
        bench::emit(&json!({
            "event": "stream-prepared",
            "agent": kind,
            "runtime": runtime_id,
            "records": runtime.stream.records,
            "parsed": runtime.stream.parsed,
            "unknown": runtime.stream.unknown,
            "hardErrors": runtime.stream.hard_errors,
            "oversized": runtime.stream.oversized,
            "quarantined": runtime.stream.quarantined,
            "filesMissing": runtime.stream.files_missing,
            "retriesExhausted": runtime.stream.retries_exhausted,
            "conflicts": runtime.stream.conflicts,
            "filesIncomplete": runtime.stream.files_incomplete,
            "captureWallMs": runtime.wall_ms,
        }));
    } else {
        println!(
            "{name}: streamed {} records and {} sessions in {:.2}s",
            runtime.stream.records,
            runtime.meta.sessions,
            runtime.wall_ms / 1_000.0,
        );
    }
    // Readable at this admission: the trees flipped before it, then the
    // sessions its own commit moves.
    let mut readable = streamed.flipped_session_ids();
    let flipped_sessions = readable.len();
    if stage_profiles {
        let (resolved_sessions, resolved_tool_rows) = streamed.resolved_before_admission();
        bench::emit(&json!({
            "event": "stage-profile",
            "command": "serve",
            "stage": "streamed-apply",
            "agent": kind,
            "runtime": runtime_id,
            "wallMs": millis(streamed.apply_nanos()),
            "sources": streamed.sources_applied(),
            "resolvedSessions": resolved_sessions,
            "resolvedToolRows": resolved_tool_rows,
            "resolveMs": millis(streamed.resolve_nanos()),
            "treesFlipped": streamed.flipped_trees(),
            "sessionsFlipped": flipped_sessions,
            "cpuMs": applied_cpu.as_secs_f64() * 1_000.0,
            "lagMs": runtime.captured_at.elapsed().as_secs_f64() * 1_000.0,
        }));
    }

    let admission_started = Instant::now();
    let admission_cpu_started = thread_cpu_time();
    let admission_before = cx.instrument.snapshot();
    // Admission writes no cursor, so these stay the committed checkpoints
    // through finalization and publication.
    let poll_evidence = streamed.take_poll_evidence();
    let rosetta_pipeline::RemainderAdmission {
        report,
        changed,
        displaced,
    } = streamed
        .admit_remainder(cx.store, cx.instrument, &runtime.prepared)
        .map_err(|error| format!("admitting {name}: {error}"))?;
    if stage_profiles {
        bench::emit_command_stage_profile(
            "ingest",
            "apply-prepared",
            Some(kind),
            admission_started,
            &admission_before,
            cx.instrument,
        );
    }
    if cx.flags.json {
        bench::emit(&json!({
            "event": "canonical-applied",
            "agent": kind,
            "runtime": runtime_id,
            "files": report.files,
            "messagesInserted": report.stats.messages_inserted,
            "messagesUpdated": report.stats.messages_updated,
            "toolResultsMerged": report.stats.tool_results_merged,
        }));
    }

    let admission_cpu_ms = cpu_millis(admission_cpu_started);
    let absorb_started = Instant::now();
    let absorb_cpu_started = thread_cpu_time();
    // Rows, then each changed session's barrier, then metadata (R1 D10):
    // only the sessions this commit moved, and flipped trees whose usage its
    // fold displaced (D15); flipped trees were announced at their flips.
    let mut invalidate = changed.clone();
    invalidate.extend(displaced.into_iter().filter(|id| !changed.contains(id)));
    absorb_around(cx.store, cx.hub, &invalidate, now_ms())?;
    if stage_profiles {
        bench::emit(&json!({
            "event": "stage-profile",
            "command": "serve",
            "stage": "admission-absorb",
            "agent": kind,
            "runtime": runtime_id,
            "wallMs": absorb_started.elapsed().as_secs_f64() * 1_000.0,
            "cpuMs": cpu_millis(absorb_cpu_started),
            "admissionCpuMs": admission_cpu_ms,
        }));
    }
    for session_id in &changed {
        cx.board.session_done(
            ownership.get(session_id).and_then(Option::as_deref),
            session_id,
        );
    }
    readable.extend(changed.iter().cloned());
    cx.health.ingest_done(
        &runtime_id,
        runtime.stream.payload_bytes,
        runtime.stream.records,
    );
    println!(
        "{name}: canonical RAM public — {} records, {} sessions",
        runtime.stream.records,
        readable.len()
    );
    milestones.readable(&runtime_id, kind, &readable, &runtime.roster);
    Ok(Some(AppliedColdRuntime {
        adapter: runtime.adapter,
        runtime: runtime.runtime,
        identity: runtime.identity,
        root: runtime.root,
        roster: runtime.roster,
        admission_sources: runtime.meta.admission_sources,
        ledger: runtime.ledger,
        poll_evidence,
    }))
}
