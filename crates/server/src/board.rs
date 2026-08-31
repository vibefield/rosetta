//! The progress board — per-project cold progress as a derived RAM view.
//!
//! The ingest plan settles denominators (sessions and census bytes per
//! project) the moment the skims commit; the serve loop feeds pass outcomes
//! as they happen. The board folds both into the per-project rows the app
//! draws under its veils, and that is all it is: nothing here is durable,
//! nothing is authoritative, and a crash costs a rebuild from the plan.
//! Durable progress remains the store's cursors, generations, and
//! `stream_state` (iron rule 5).
//!
//! Attribution is file-grained on purpose: a multi-session source is ONE
//! pass attributed to its roster session, so project figures are an honest
//! fold of pass outcomes, not a claim of per-row precision. `sessions_done`
//! is monotone cold-progress — a live hint that requeues a finished session
//! flips its readiness badge, not this meter.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

use rosetta_protocol::ProjectBackfill;

#[derive(Default)]
struct ProjectProgress {
    sessions_total: u64,
    sessions_done: u64,
    bytes_total: u64,
    bytes_streamed: u64,
}

#[derive(Default)]
struct BoardState {
    /// Keyed by canonical project id; `None` is the unassigned bucket.
    projects: BTreeMap<Option<String>, ProjectProgress>,
    /// Idempotence guards: canonical session ids are globally unique.
    seeded: BTreeSet<String>,
    done: BTreeSet<String>,
}

#[derive(Default)]
pub struct ProgressBoard {
    state: Mutex<BoardState>,
}

impl ProgressBoard {
    pub fn new() -> Self {
        Self::default()
    }

    /// Enter one planned session under its settled project. Idempotent per
    /// session id, so plan seeding and mid-cold admissions can never double
    /// a denominator.
    pub fn seed_session(&self, project_id: Option<&str>, session_id: &str, bytes: u64) {
        let mut state = self.state.lock().expect("board state poisoned");
        if !state.seeded.insert(session_id.to_owned()) {
            return;
        }
        let entry = state
            .projects
            .entry(project_id.map(str::to_owned))
            .or_default();
        entry.sessions_total += 1;
        entry.bytes_total += bytes;
    }

    /// Grow a known session's census share — a mid-cold admission surfaced
    /// another source file for it.
    pub fn extend_session(&self, project_id: Option<&str>, bytes: u64) {
        let mut state = self.state.lock().expect("board state poisoned");
        let entry = state
            .projects
            .entry(project_id.map(str::to_owned))
            .or_default();
        entry.bytes_total += bytes;
    }

    /// Fold one pass outcome: bytes the executor read for this project's
    /// session. May legitimately run past the census total when a source
    /// grows mid-backfill; meters clamp at render time.
    pub fn advance(&self, project_id: Option<&str>, bytes: u64) {
        if bytes == 0 {
            return;
        }
        let mut state = self.state.lock().expect("board state poisoned");
        let entry = state
            .projects
            .entry(project_id.map(str::to_owned))
            .or_default();
        entry.bytes_streamed += bytes;
    }

    /// One session's last queued cold task finished. Idempotent and
    /// monotone: a later live requeue does not un-finish cold progress.
    pub fn session_done(&self, project_id: Option<&str>, session_id: &str) {
        let mut state = self.state.lock().expect("board state poisoned");
        if !state.done.insert(session_id.to_owned()) {
            return;
        }
        let entry = state
            .projects
            .entry(project_id.map(str::to_owned))
            .or_default();
        entry.sessions_done += 1;
    }

    /// The wire rows, largest plans first — ties broken by id for a stable
    /// order under the app's poll.
    pub fn snapshot(&self) -> Vec<ProjectBackfill> {
        let state = self.state.lock().expect("board state poisoned");
        let mut rows: Vec<ProjectBackfill> = state
            .projects
            .iter()
            .map(|(project_id, progress)| ProjectBackfill {
                project_id: project_id.clone(),
                sessions_total: progress.sessions_total,
                sessions_done: progress.sessions_done,
                bytes_total: progress.bytes_total,
                bytes_streamed: progress.bytes_streamed,
            })
            .collect();
        rows.sort_by(|left, right| {
            right
                .bytes_total
                .cmp(&left.bytes_total)
                .then_with(|| left.project_id.cmp(&right.project_id))
        });
        rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_seeds_outcomes_and_completions_into_stable_rows() {
        let board = ProgressBoard::new();
        board.seed_session(Some("p1"), "s1", 100);
        board.seed_session(Some("p1"), "s2", 50);
        board.seed_session(None, "s3", 10);
        board.seed_session(Some("p1"), "s1", 999); // double-seed: ignored

        board.advance(Some("p1"), 60);
        board.advance(None, 10);
        board.advance(None, 0); // no-op
        board.session_done(None, "s3");
        board.session_done(None, "s3"); // idempotent

        let rows = board.snapshot();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].project_id.as_deref(), Some("p1"));
        assert_eq!(
            (rows[0].sessions_total, rows[0].sessions_done),
            (2, 0),
            "p1 has both sessions planned, none finished"
        );
        assert_eq!((rows[0].bytes_total, rows[0].bytes_streamed), (150, 60));
        assert_eq!(rows[1].project_id, None);
        assert_eq!((rows[1].sessions_total, rows[1].sessions_done), (1, 1));
        assert_eq!((rows[1].bytes_total, rows[1].bytes_streamed), (10, 10));
    }

    #[test]
    fn admissions_extend_totals_and_overrun_is_preserved_for_the_meter_to_clamp() {
        let board = ProgressBoard::new();
        board.seed_session(Some("p1"), "s1", 100);
        board.extend_session(Some("p1"), 40);
        board.advance(Some("p1"), 200); // the source grew mid-backfill
        board.session_done(Some("p1"), "s1");

        let rows = board.snapshot();
        assert_eq!((rows[0].bytes_total, rows[0].bytes_streamed), (140, 200));
        assert_eq!((rows[0].sessions_total, rows[0].sessions_done), (1, 1));
    }
}
