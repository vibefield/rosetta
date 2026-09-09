//! Conversation time comes from canonical rows, independently of source
//! metadata. Bounds live with each message so generation replacement removes
//! its activity too. Indexed reads roll them up through the actual parent tree.

use rosetta_core::rows::{Ms, SessionActivity};
use rosetta_core::{TimelineBody, TimelineEntry};
use rusqlite::{Connection, params};

use crate::StoreError;

pub(crate) fn message_bounds(entry: &TimelineEntry) -> (Option<Ms>, Option<Ms>) {
    if matches!(entry.body, TimelineBody::Notice { .. }) {
        return (None, None);
    }
    let mut times = [entry.at, None, None];
    if let TimelineBody::ToolUse { tool } = &entry.body
        && let Some(timing) = &tool.timing
    {
        times[1] = timing.started_at;
        times[2] = timing.completed_at;
    }
    (
        times.iter().flatten().copied().min(),
        times.into_iter().flatten().max(),
    )
}

/// Result/update records may carry a timestamp without a native timing block.
/// Retain that evidence on the resolved tool row, not on session metadata.
pub(crate) fn touch_tool(
    conn: &Connection,
    session_id: &str,
    correlation_key: &str,
    at: Ms,
) -> Result<(), StoreError> {
    conn.prepare_cached(
        "UPDATE message
         SET activity_first_at = MIN(COALESCE(activity_first_at, ?3), ?3),
             activity_last_at = MAX(COALESCE(activity_last_at, ?3), ?3)
         WHERE session_id = ?1 AND tool_correlation_key = ?2 AND type = 'tool_use'",
    )?
    .execute(params![session_id, correlation_key, at])?;
    Ok(())
}

pub(crate) fn session_activity(
    conn: &Connection,
    session_id: &str,
) -> Result<Option<SessionActivity>, StoreError> {
    // Each bound uses its own covering index. This visits the small session
    // tree and performs two index seeks per member, never scans its transcript.
    let (first, last): (Option<Ms>, Option<Ms>) = conn
        .prepare_cached(
            "WITH RECURSIVE family(id) AS (
             SELECT id FROM session WHERE id = ?1
             UNION
             SELECT child.id FROM session child JOIN family ON child.parent_session_id = family.id
         )
         SELECT MIN((SELECT MIN(activity_first_at) FROM message
                     WHERE session_id = family.id)),
                MAX((SELECT MAX(activity_last_at) FROM message
                     WHERE session_id = family.id))
         FROM family",
        )?
        .query_row(params![session_id], |row| Ok((row.get(0)?, row.get(1)?)))?;
    Ok(first
        .zip(last)
        .map(|(first_at, last_at)| SessionActivity { first_at, last_at }))
}

/// Current ancestors, including self. UNION also terminates malformed cycles.
pub(crate) fn ancestors(conn: &Connection, session_id: &str) -> Result<Vec<String>, StoreError> {
    let mut stmt = conn.prepare_cached(
        "WITH RECURSIVE ancestors(id) AS (
             SELECT ?1
             UNION
             SELECT s.parent_session_id FROM session s JOIN ancestors ON s.id = ancestors.id
             WHERE s.parent_session_id IS NOT NULL
         ) SELECT id FROM ancestors",
    )?;
    Ok(stmt
        .query_map(params![session_id], |row| row.get(0))?
        .collect::<Result<_, _>>()?)
}
