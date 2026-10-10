//! Turn spans: when a session tree's message groups were active inside a time
//! window, and where each group fell silent. A read-time projection over the
//! canonical `message` rows (`group_id`, `group_kind`, and each row's own
//! activity bounds) — nothing new is stored, and no payload is read.
//!
//! A turn is one message group of any member of the tree: its first and last
//! recorded activity among the rows that touch the window. Its gaps are the
//! stretches between consecutive activity instants (each row's first and last
//! bound) longer than the caller's threshold. Instants, not merged intervals:
//! a tool row whose bounds are its request and its completion records nothing
//! in between, so a long wait inside it is a gap, never presumed work.

use std::collections::BTreeMap;

use rosetta_core::GroupKind;
use rosetta_core::rows::Ms;
use rusqlite::{Connection, params};

use crate::StoreError;
use crate::message_codec::parse_group_kind;

/// A stretch inside a turn with no recorded activity, longer than the
/// threshold the caller asked about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct TurnGap {
    pub from_ms: Ms,
    pub to_ms: Ms,
}

/// One message group's activity inside the window.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct TurnSpan {
    pub group_id: String,
    pub kind: GroupKind,
    pub first_at: Ms,
    pub last_at: Ms,
    /// Oldest first; empty when the turn never paused longer than the threshold.
    pub gaps: Vec<TurnGap>,
}

struct Turn {
    group_id: String,
    kind: GroupKind,
    instants: Vec<Ms>,
}

/// Every turn of `root_id`'s session tree whose rows touch `[since_ms,
/// until_ms]`, oldest first, with gaps longer than `gap_ms`. The tree is walked
/// through parent links (as [`crate::queries::session`]'s activity is), so a
/// delegated thread's turns are the root's too. A row touches the window when
/// its own bounds overlap it; its bounds are kept whole, so a turn that began
/// before the window may start before `since_ms`.
pub fn turn_spans(
    conn: &Connection,
    root_id: &str,
    since_ms: Ms,
    until_ms: Ms,
    gap_ms: Ms,
) -> Result<Vec<TurnSpan>, StoreError> {
    // One range seek per member on `message_activity_last`.
    let mut stmt = conn.prepare_cached(
        "WITH RECURSIVE family(id) AS (
             SELECT id FROM session WHERE id = ?1
             UNION
             SELECT child.id FROM session child JOIN family ON child.parent_session_id = family.id
         )
         SELECT m.session_id, m.group_id, m.group_kind, m.activity_first_at, m.activity_last_at
         FROM family JOIN message m ON m.session_id = family.id
         WHERE m.activity_last_at >= ?2 AND m.activity_first_at <= ?3",
    )?;
    let mut turns: BTreeMap<(String, String), Turn> = BTreeMap::new();
    let mut rows = stmt.query(params![root_id, since_ms, until_ms])?;
    while let Some(row) = rows.next()? {
        let session_id: String = row.get(0)?;
        let group_id: String = row.get(1)?;
        let kind = parse_group_kind(&row.get::<_, String>(2)?)?;
        let first: Ms = row.get(3)?;
        let last: Ms = row.get(4)?;
        let turn = turns
            .entry((session_id, group_id.clone()))
            .or_insert_with(|| Turn {
                group_id,
                kind,
                instants: Vec::new(),
            });
        turn.instants.push(first);
        turn.instants.push(last);
    }
    let mut spans: Vec<TurnSpan> = turns
        .into_values()
        .map(|mut turn| {
            turn.instants.sort_unstable();
            turn.instants.dedup();
            let gaps = turn
                .instants
                .windows(2)
                .filter(|pair| pair[1] - pair[0] > gap_ms)
                .map(|pair| TurnGap {
                    from_ms: pair[0],
                    to_ms: pair[1],
                })
                .collect();
            TurnSpan {
                group_id: turn.group_id,
                kind: turn.kind,
                first_at: turn.instants[0],
                last_at: turn.instants[turn.instants.len() - 1],
                gaps,
            }
        })
        .collect();
    spans.sort_by(|a, b| {
        a.first_at
            .cmp(&b.first_at)
            .then_with(|| a.group_id.cmp(&b.group_id))
    });
    Ok(spans)
}
