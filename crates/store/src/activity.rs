//! Conversation time comes from canonical rows, independently of source
//! metadata. Bounds live with each message so generation replacement removes
//! its activity too. Indexed reads roll them up through the actual parent tree.

use std::collections::{HashMap, HashSet};

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

/// One session's family rollup, every member counted.
const FAMILY_ACTIVITY: &str = "WITH RECURSIVE family(id) AS (
             SELECT id FROM session WHERE id = ?1
             UNION
             SELECT child.id FROM session child JOIN family ON child.parent_session_id = family.id
         )
         SELECT MIN((SELECT MIN(activity_first_at) FROM message
                     WHERE session_id = family.id)),
                MAX((SELECT MAX(activity_last_at) FROM message
                     WHERE session_id = family.id))
         FROM family";

/// [`FAMILY_ACTIVITY`] behind the cold admission barrier: the walk neither
/// starts at nor descends through a Pending session, whose rows are not
/// public yet. A Pending session therefore has no activity at all.
const ADMITTED_FAMILY_ACTIVITY: &str = "WITH RECURSIVE family(id) AS (
             SELECT id FROM session WHERE id = ?1 AND stream_state != 'pending'
             UNION
             SELECT child.id FROM session child JOIN family ON child.parent_session_id = family.id
             WHERE child.stream_state != 'pending'
         )
         SELECT MIN((SELECT MIN(activity_first_at) FROM message
                     WHERE session_id = family.id)),
                MAX((SELECT MAX(activity_last_at) FROM message
                     WHERE session_id = family.id))
         FROM family";

/// A session's activity rolled up over its family. With `hide_pending` the
/// admission barrier stands: Pending members, and the subtrees below them,
/// are left out (a session in a fresh build's unpublished RAM generation).
pub(crate) fn session_activity(
    conn: &Connection,
    session_id: &str,
    hide_pending: bool,
) -> Result<Option<SessionActivity>, StoreError> {
    // Each bound uses its own covering index. This visits the small session
    // tree and performs two index seeks per member, never scans its transcript.
    let sql = if hide_pending {
        ADMITTED_FAMILY_ACTIVITY
    } else {
        FAMILY_ACTIVITY
    };
    let (first, last): (Option<Ms>, Option<Ms>) = conn
        .prepare_cached(sql)?
        .query_row(params![session_id], |row| Ok((row.get(0)?, row.get(1)?)))?;
    Ok(first
        .zip(last)
        .map(|(first_at, last_at)| SessionActivity { first_at, last_at }))
}

/// [`session_activity`] for many sessions at once, with the same answer for
/// each. The per-session query pays a recursive family walk for every
/// session it is asked about; refreshing a whole runtime asks about
/// thousands. Here every member's own bounds are read once — the same two
/// covering-index seeks — and each family is rolled up in RAM along the
/// session parent links, deduplicated like the query's `UNION`, so a cycle
/// still terminates. A session the projection does not hold has no family
/// and no activity; with `hide_pending`, neither has a Pending one, and no
/// walk descends through one — [`session_activity`]'s barrier.
pub(crate) fn sessions_activity<'a>(
    conn: &Connection,
    session_ids: impl IntoIterator<Item = &'a str>,
    hide_pending: bool,
) -> Result<HashMap<String, Option<SessionActivity>>, StoreError> {
    let mut children: HashMap<String, Vec<String>> = HashMap::new();
    let mut known: HashSet<String> = HashSet::new();
    {
        let mut statement = conn.prepare_cached(
            "SELECT id, parent_session_id, stream_state = 'pending' FROM session",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, bool>(2)?,
            ))
        })?;
        for row in rows {
            let (id, parent, pending) = row?;
            // A withheld session is neither a family nor anyone's member.
            if hide_pending && pending {
                continue;
            }
            if let Some(parent) = parent {
                children.entry(parent).or_default().push(id.clone());
            }
            known.insert(id);
        }
    }
    let mut own_bounds = conn.prepare_cached(
        "SELECT (SELECT MIN(activity_first_at) FROM message WHERE session_id = ?1),
                (SELECT MAX(activity_last_at) FROM message WHERE session_id = ?1)",
    )?;
    let mut own: HashMap<String, (Option<Ms>, Option<Ms>)> = HashMap::new();
    let mut result = HashMap::new();
    for session_id in session_ids {
        if !known.contains(session_id) {
            result.insert(session_id.to_owned(), None);
            continue;
        }
        let (mut first, mut last): (Option<Ms>, Option<Ms>) = (None, None);
        let mut visited: HashSet<&str> = HashSet::new();
        let mut queue = vec![session_id];
        while let Some(member) = queue.pop() {
            if !visited.insert(member) {
                continue;
            }
            let bounds = match own.get(member) {
                Some(bounds) => *bounds,
                None => {
                    let bounds = own_bounds.query_row(params![member], |row| {
                        Ok((row.get::<_, Option<Ms>>(0)?, row.get::<_, Option<Ms>>(1)?))
                    })?;
                    own.insert(member.to_owned(), bounds);
                    bounds
                }
            };
            first = match (first, bounds.0) {
                (Some(left), Some(right)) => Some(left.min(right)),
                (left, right) => left.or(right),
            };
            last = match (last, bounds.1) {
                (Some(left), Some(right)) => Some(left.max(right)),
                (left, right) => left.or(right),
            };
            if let Some(members) = children.get(member) {
                queue.extend(members.iter().map(String::as_str));
            }
        }
        result.insert(
            session_id.to_owned(),
            first
                .zip(last)
                .map(|(first_at, last_at)| SessionActivity { first_at, last_at }),
        );
    }
    Ok(result)
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

#[cfg(test)]
mod tests {
    use rosetta_core::IngestOp;
    use rosetta_core::rows::{MessageKind, MessageRow, SessionRole, SessionRow, StreamState};

    use super::*;
    use crate::{Batch, Store};

    fn session(id: &str, parent: Option<&str>) -> IngestOp {
        IngestOp::SessionMeta(SessionRow {
            id: id.into(),
            native_session_id: id.into(),
            runtime_id: "rt".into(),
            project_id: None,
            cwd: None,
            parent_session_id: parent.map(str::to_owned),
            delegation: None,
            root_session_id: id.into(),
            role: SessionRole::Main,
            title: None,
            summary: None,
            started_at: 1,
            updated_at: 1,
            activity: None,
            stream_state: StreamState::Indexed,
        })
    }

    fn message(timeline_id: &str, session_id: &str, ordinal: i64, at: Option<i64>) -> IngestOp {
        IngestOp::project_message(MessageRow {
            timeline_id: timeline_id.into(),
            session_id: session_id.into(),
            ordinal,
            kind: MessageKind::User,
            subtype: None,
            uuid: None,
            parent_uuid: None,
            ts: at,
            model: None,
            effort: None,
            content: Some(timeline_id.into()),
            tool: None,
            attachments: Vec::new(),
            citations: Vec::new(),
            group: None,
            response: None,
            block_index: None,
            branch_tool_id: None,
            usage: None,
            agent_version: None,
            src_cursor: None,
            src_offset: None,
        })
        .pop()
        .expect("a user message projects one row")
    }

    #[test]
    fn bulk_session_activity_equals_the_per_session_family_walk() {
        let mut store = Store::open_in_memory().unwrap();
        let runtime = IngestOp::Runtime(
            serde_json::from_value(serde_json::json!({
                "id": "rt", "kind": "claude-code", "data_root": "/agent",
                "enabled": true, "discovered_at": 1
            }))
            .unwrap(),
        );
        let mut ops = vec![
            runtime,
            session("root", None),
            session("child", Some("root")),
            session("sibling", Some("root")),
            session("grandchild", Some("child")),
            session("quiet", None),
            session("undated", Some("quiet")),
            session("loop-a", None),
            session("loop-b", Some("loop-a")),
        ];
        ops.extend([
            message("r1", "root", 0, Some(50)),
            message("r2", "root", 1, Some(60)),
            message("c1", "child", 0, Some(40)),
            message("s1", "sibling", 0, Some(90)),
            message("g1", "grandchild", 0, Some(10)),
            message("g2", "grandchild", 1, None),
            message("u1", "undated", 0, None),
            message("a1", "loop-a", 0, Some(7)),
            message("b1", "loop-b", 0, Some(8)),
        ]);
        store
            .apply(&Batch {
                ops,
                checkpoints: vec![],
                source: None,
            })
            .unwrap();
        // A malformed parent cycle still terminates, in both walks.
        store
            .conn()
            .execute(
                "UPDATE session SET parent_session_id = 'loop-b' WHERE id = 'loop-a'",
                [],
            )
            .unwrap();

        let ids = [
            "root",
            "child",
            "sibling",
            "grandchild",
            "quiet",
            "undated",
            "loop-a",
            "loop-b",
            "missing",
        ];
        let bulk = sessions_activity(store.conn(), ids, false).unwrap();
        for id in ids {
            assert_eq!(
                bulk[id],
                session_activity(store.conn(), id, false).unwrap(),
                "session {id}"
            );
        }
        assert_eq!(
            bulk["root"],
            Some(SessionActivity {
                first_at: 10,
                last_at: 90
            })
        );
        assert_eq!(bulk["quiet"], None);
    }

    #[test]
    fn the_admission_barrier_leaves_pending_members_out_of_every_rollup() {
        let mut store = Store::open_in_memory().unwrap();
        let runtime = IngestOp::Runtime(
            serde_json::from_value(serde_json::json!({
                "id": "rt", "kind": "codex", "data_root": "/agent",
                "enabled": true, "discovered_at": 1
            }))
            .unwrap(),
        );
        // A depth-2 family split across readiness: the root and one child are
        // admitted, the other child (and so its grandchild) is not.
        let mut ops = vec![
            runtime,
            session("root", None),
            session("open-child", Some("root")),
            session("held-child", Some("root")),
            session("grandchild", Some("held-child")),
            session("held-root", None),
            session("its-child", Some("held-root")),
        ];
        ops.extend([
            message("r1", "root", 0, Some(50)),
            message("o1", "open-child", 0, Some(60)),
            message("h1", "held-child", 0, Some(5)),
            message("g1", "grandchild", 0, Some(500)),
            message("p1", "held-root", 0, Some(70)),
            message("c1", "its-child", 0, Some(80)),
        ]);
        store
            .apply(&Batch {
                ops,
                checkpoints: vec![],
                source: None,
            })
            .unwrap();
        store
            .conn()
            .execute(
                "UPDATE session SET stream_state = 'pending'
                 WHERE id IN ('held-child', 'held-root')",
                [],
            )
            .unwrap();

        let ids = [
            "root",
            "open-child",
            "held-child",
            "grandchild",
            "held-root",
            "its-child",
        ];
        let hidden = sessions_activity(store.conn(), ids, true).unwrap();
        let shown = sessions_activity(store.conn(), ids, false).unwrap();
        for id in ids {
            assert_eq!(
                hidden[id],
                session_activity(store.conn(), id, true).unwrap(),
                "hidden {id}"
            );
            assert_eq!(
                shown[id],
                session_activity(store.conn(), id, false).unwrap(),
                "shown {id}"
            );
        }
        let bounds = |first_at, last_at| Some(SessionActivity { first_at, last_at });
        // Neither the held child nor anything below it reaches the root.
        assert_eq!(hidden["root"], bounds(50, 60));
        assert_eq!(shown["root"], bounds(5, 500));
        // A Pending session has no activity, even with admitted children.
        assert_eq!(hidden["held-child"], None);
        assert_eq!(hidden["held-root"], None);
        assert_eq!(hidden["its-child"], bounds(80, 80));
        // An admitted member below a held one still answers for itself.
        assert_eq!(hidden["grandchild"], bounds(500, 500));
    }
}
