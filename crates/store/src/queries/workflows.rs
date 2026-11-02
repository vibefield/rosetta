//! Workflow hydration: the generation-owned Workflow document joined onto
//! each invocation row at read time, with its attempts, its bounded journal
//! window and the runtime profiles its child transcripts observed. Moved out of
//! `queries` so the admission barrier's family filter lives with the one join
//! that reads child sessions.

use std::collections::{BTreeMap, BTreeSet};

use rosetta_core::timeline::{
    WORKFLOW_ATTEMPT_LIMIT, WORKFLOW_JOURNAL_LIMIT, WORKFLOW_RUNTIME_PROFILE_LIMIT,
};
use rosetta_core::{
    TimelineBody, TimelineEntry, ToolAction, WorkflowAttempt, WorkflowJournalObservation,
    WorkflowRun,
};
use rusqlite::{Connection, OptionalExtension, params};

use crate::{StoreError, message_codec};

/// Child runtime profiles, every child shown. This is the published join,
/// unchanged; [`PROFILES_ADMITTED`] is the same join behind the barrier.
const PROFILES: &str = "SELECT m.session_id, m.model, m.effort
         FROM message m
         JOIN session child ON child.id = m.session_id
         WHERE child.parent_session_id = ?1
           AND (m.model IS NOT NULL OR m.effort IS NOT NULL)
         ORDER BY m.id";

/// [`PROFILES`] while a fresh build is unpublished: a Pending child's rows
/// are not public yet, so its observed model and effort are not either.
const PROFILES_ADMITTED: &str = "SELECT m.session_id, m.model, m.effort
         FROM message m
         JOIN session child ON child.id = m.session_id
         WHERE child.parent_session_id = ?1
           AND child.stream_state != 'pending'
           AND (m.model IS NOT NULL OR m.effort IS NOT NULL)
         ORDER BY m.id";

/// Join the generation-owned Workflow document onto each chronological
/// invocation that proved the same native run id. Runtime profiles come from
/// child transcripts and therefore override neither configured values nor one
/// another; the UI can honestly say "varies" when multiple were observed.
/// With `hide_pending` (the cold admission barrier stands) a Pending child
/// contributes no profile.
pub(super) fn hydrate_workflows(
    conn: &Connection,
    session_id: &str,
    entries: &mut [TimelineEntry],
    hide_pending: bool,
) -> Result<(), StoreError> {
    let wanted = entries
        .iter()
        .filter_map(|entry| match &entry.body {
            TimelineBody::ToolUse { tool } => match &tool.action {
                ToolAction::Workflow {
                    run_id: Some(run_id),
                    ..
                } => Some(run_id.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    if wanted.is_empty() {
        return Ok(());
    }

    let mut runs = BTreeMap::<String, WorkflowRun>::new();
    for run_id in &wanted {
        let detail = conn
            .prepare_cached(
                "SELECT detail FROM workflow_run WHERE session_id = ?1 AND run_id = ?2",
            )?
            .query_row(params![session_id, run_id], |row| row.get::<_, String>(0))
            .optional()?;
        let Some(detail) = detail else {
            continue;
        };
        let mut run = serde_json::from_str::<WorkflowRun>(&detail)?;
        hydrate_workflow_attempts(conn, session_id, &mut run)?;
        hydrate_workflow_journal(conn, session_id, &mut run)?;
        runs.insert(run_id.clone(), run);
    }
    if runs.is_empty() {
        return Ok(());
    }

    let mut profiles = BTreeMap::<String, (Vec<String>, Vec<String>)>::new();
    let mut statement = conn.prepare_cached(if hide_pending {
        PROFILES_ADMITTED
    } else {
        PROFILES
    })?;
    let rows = statement.query_map(params![session_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, Option<String>>(2)?,
        ))
    })?;
    for row in rows {
        let (child_session_id, model, effort) = row?;
        let profile = profiles.entry(child_session_id).or_default();
        if let Some(model) = model {
            push_unique(&mut profile.0, model);
        }
        if let Some(effort) = effort {
            push_unique(&mut profile.1, effort);
        }
    }
    drop(statement);

    for run in runs.values_mut() {
        for worker in &mut run.workers {
            let Some(child_session_id) = worker.child_session_id.as_ref() else {
                continue;
            };
            if let Some((models, efforts)) = profiles.get(child_session_id) {
                worker.observed_models.clone_from(models);
                worker.observed_efforts.clone_from(efforts);
            }
        }
        run.validate()
            .map_err(|error| StoreError::ProjectionInvalid {
                message: error.to_string(),
            })?;
    }

    for entry in entries {
        let TimelineBody::ToolUse { tool } = &mut entry.body else {
            continue;
        };
        let ToolAction::Workflow { run_id, run, .. } = &mut tool.action else {
            continue;
        };
        *run = run_id
            .as_ref()
            .and_then(|run_id| runs.get(run_id))
            .cloned()
            .map(Box::new);
    }
    Ok(())
}

/// Hydrate every ordinary attempt independent of the caller's timeline page.
/// A pathological native history is capped for wire safety while the exact
/// total remains visible; real Workflow runs are far below this boundary.
fn hydrate_workflow_attempts(
    conn: &Connection,
    session_id: &str,
    run: &mut WorkflowRun,
) -> Result<(), StoreError> {
    let total = conn
        .prepare_cached(
            "SELECT COUNT(*) FROM message
         WHERE session_id = ?1 AND type = 'tool_use' AND tool_kind = 'workflow'
           AND json_extract(tool_detail, '$.run_id') = ?2",
        )?
        .query_row(params![session_id, run.run_id], |row| row.get::<_, i64>(0))?;
    run.attempt_total_count = u64::try_from(total).map_err(|_| StoreError::ProjectionInvalid {
        message: "workflow attempt count is negative".into(),
    })?;

    let mut statement = conn.prepare_cached(
        "SELECT * FROM (
           SELECT * FROM message
           WHERE session_id = ?1 AND type = 'tool_use' AND tool_kind = 'workflow'
             AND json_extract(tool_detail, '$.run_id') = ?2
           ORDER BY ordinal DESC LIMIT ?3
         ) ORDER BY ordinal ASC",
    )?;
    let rows = statement.query_map(
        params![
            session_id,
            run.run_id,
            i64::try_from(WORKFLOW_ATTEMPT_LIMIT).unwrap_or(i64::MAX),
        ],
        message_codec::decode,
    )?;
    run.attempts.clear();
    for row in rows {
        let entry = row?;
        let TimelineBody::ToolUse { tool } = &entry.body else {
            continue;
        };
        let ToolAction::Workflow {
            task_id,
            resume_id,
            attempt,
            callback_summary,
            ..
        } = &tool.action
        else {
            continue;
        };
        let started_at = tool
            .timing
            .as_ref()
            .and_then(|timing| timing.started_at)
            .or(entry.at);
        let completed_at = tool.timing.as_ref().and_then(|timing| timing.completed_at);
        let duration_ms = tool
            .timing
            .as_ref()
            .and_then(|timing| timing.duration_ms)
            .or_else(|| {
                completed_at
                    .zip(started_at)
                    .and_then(|(completed, started)| completed.checked_sub(started))
                    .and_then(|value| u64::try_from(value).ok())
            });
        let attempt = attempt.unwrap_or(1);
        run.attempts.push(WorkflowAttempt {
            timeline_id: entry.timeline_id,
            attempt,
            task_id: task_id.clone(),
            resumed: attempt > 1 || resume_id.is_some(),
            status: tool.status.value,
            at: started_at,
            completed_at,
            duration_ms,
            summary: callback_summary.clone(),
        });
    }
    Ok(())
}

/// Keep only the latest bounded observation window on the wire. Aggregate
/// counts come from all durable observations, so a large journal never makes
/// the whole timeline unreadable and never under-reports progress.
fn hydrate_workflow_journal(
    conn: &Connection,
    session_id: &str,
    run: &mut WorkflowRun,
) -> Result<(), StoreError> {
    let (total, started, result) = conn.prepare_cached(
        "SELECT COUNT(*),
                COALESCE(SUM(CASE WHEN json_extract(detail, '$.kind') = 'started' THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN json_extract(detail, '$.kind') = 'result' THEN 1 ELSE 0 END), 0)
         FROM workflow_journal WHERE session_id = ?1 AND run_id = ?2",
    )?
    .query_row(params![session_id, run.run_id], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
        ))
    })?;
    let count = |value: i64, label: &str| {
        u64::try_from(value).map_err(|_| StoreError::ProjectionInvalid {
            message: format!("workflow journal {label} count is negative"),
        })
    };
    run.journal_total_count = count(total, "total")?;
    run.journal_started_count = count(started, "started")?;
    run.journal_result_count = count(result, "result")?;

    let mut statement = conn.prepare_cached(
        "SELECT detail FROM workflow_journal
         WHERE session_id = ?1 AND run_id = ?2
         ORDER BY COALESCE(CAST(json_extract(detail, '$.source_offset') AS INTEGER), -1) DESC,
                  observation_id DESC
         LIMIT ?3",
    )?;
    let rows = statement.query_map(
        params![
            session_id,
            run.run_id,
            i64::try_from(WORKFLOW_JOURNAL_LIMIT).unwrap_or(i64::MAX),
        ],
        |row| row.get::<_, String>(0),
    )?;
    run.journal.clear();
    for row in rows {
        run.journal
            .push(serde_json::from_str::<WorkflowJournalObservation>(&row?)?);
    }
    run.journal.sort_by(|left, right| {
        left.source_offset
            .cmp(&right.source_offset)
            .then_with(|| left.observation_id.cmp(&right.observation_id))
    });
    Ok(())
}

fn push_unique(values: &mut Vec<String>, value: String) {
    if values.len() < WORKFLOW_RUNTIME_PROFILE_LIMIT && !values.contains(&value) {
        values.push(value);
    }
}
