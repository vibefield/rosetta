//! `GET /api/activity`: which top-level sessions were active in a time window,
//! and the turns that made them so. The sessions come from the hub's mirror
//! (their activity bounds), the turns from one bounded read per session tree
//! ([`rosetta_store::turns::turn_spans`]): a monitor draws a day of lanes from
//! one ask, never by paging every timeline.
//!
//! Not a grant route: it spans sessions, so it needs the bearer.

use axum::extract::{Query, State};
use axum::response::Json;
use rosetta_core::rows::{Ms, StreamState};
use rosetta_protocol::{ActivityResponse, ActivitySession};
use rosetta_store::turns::turn_spans;
use serde::Deserialize;

use super::{AppState, Failure, clamp_limit, now_ms, selector_list, store_err, with_read};

/// The widest window one ask may read.
pub const MAX_ACTIVITY_WINDOW_MS: Ms = 7 * 24 * 60 * 60 * 1000;
/// A silence inside a turn is reported once it passes this, unless asked otherwise.
pub const DEFAULT_GAP_MS: Ms = 15 * 60 * 1000;
/// The shortest silence an ask may ask about: finer than this is a timeline read.
pub const MIN_GAP_MS: Ms = 60 * 1000;
/// The most sessions one ask may name.
pub const MAX_ACTIVITY_SESSIONS: usize = 256;
const DEFAULT_ACTIVITY_LIMIT: i64 = 200;

#[derive(Deserialize)]
pub(super) struct ActivityQuery {
    since_ms: Option<Ms>,
    until_ms: Option<Ms>,
    /// Comma-separated top-level session ids to narrow to.
    sessions: Option<String>,
    gap_ms: Option<Ms>,
    limit: Option<i64>,
}

pub(super) async fn activity(
    State(state): State<AppState>,
    Query(query): Query<ActivityQuery>,
) -> Result<Json<ActivityResponse>, Failure> {
    let Some(since_ms) = query.since_ms else {
        return Err(Failure::bad_request("since_ms is required"));
    };
    let until_ms = query.until_ms.unwrap_or_else(now_ms);
    if until_ms <= since_ms || until_ms - since_ms > MAX_ACTIVITY_WINDOW_MS {
        return Err(Failure::bad_request(format!(
            "until_ms must follow since_ms by at most {MAX_ACTIVITY_WINDOW_MS} ms"
        )));
    }
    let gap_ms = query.gap_ms.unwrap_or(DEFAULT_GAP_MS);
    if gap_ms < MIN_GAP_MS {
        return Err(Failure::bad_request(format!(
            "gap_ms must be at least {MIN_GAP_MS}"
        )));
    }
    let named = match query.sessions.as_deref() {
        None => None,
        Some(raw) => {
            let ids = selector_list(raw, "sessions")?;
            if ids.len() > MAX_ACTIVITY_SESSIONS {
                return Err(Failure::bad_request(format!(
                    "sessions exceeds {MAX_ACTIVITY_SESSIONS} ids"
                )));
            }
            Some(ids)
        }
    };
    let limit = clamp_limit(query.limit, DEFAULT_ACTIVITY_LIMIT) as usize;

    // Roots only: a delegated thread's turns are its root's (turn_spans walks
    // the tree). Bounds that miss the window cannot hold a turn inside it.
    let mut roots: Vec<_> = state
        .hub
        .sessions()
        .into_iter()
        .filter(|session| session.parent_session_id.is_none())
        .filter(|session| {
            named
                .as_ref()
                .is_none_or(|ids| ids.iter().any(|id| id == &session.id))
        })
        .filter(|session| {
            session
                .activity
                .is_some_and(|bounds| bounds.last_at >= since_ms && bounds.first_at <= until_ms)
        })
        .collect();
    roots.sort_by(rosetta_core::SessionRow::compare_recent);
    let truncated = roots.len() > limit;
    roots.truncate(limit);

    // A fresh daemon's Pending rows are not public until their runtime's
    // readiness commit, exactly as the timeline route withholds them.
    let hide_pending = !state.projection.is_published();
    let sessions = with_read(state.projection.clone(), move |conn| {
        roots
            .into_iter()
            .map(|session| {
                let withheld = hide_pending && session.stream_state == StreamState::Pending;
                let turns = if withheld {
                    Vec::new()
                } else {
                    turn_spans(conn, &session.id, since_ms, until_ms, gap_ms).map_err(store_err)?
                };
                Ok(ActivitySession {
                    session,
                    turns,
                    turns_withheld: withheld,
                })
            })
            .collect::<Result<Vec<_>, Failure>>()
    })
    .await?;
    Ok(Json(ActivityResponse {
        since_ms,
        until_ms,
        gap_ms,
        sessions,
        truncated,
    }))
}
