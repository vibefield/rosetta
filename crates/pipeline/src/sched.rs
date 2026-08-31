//! The cold-start scheduler: one queue, four lanes — live ▸ boost ▸ starter
//! ▸ completion. Starter seeds one task per source (budgeted slices when
//! serve enables them), paused tasks requeue into completion, boost
//! promotions from the app jump the cold lanes, and watcher hints nominate
//! actively-growing sources into the live lane so a running session stays
//! current DURING the backfill. Fairness is per runtime inside the cold
//! lanes: round-robin keeps one giant agent from hiding every smaller one,
//! which a scalar priority alone would not.
//!
//! Invariants the queue owns: a session's sources surface in canonical path
//! order (the ordinal-reveal rule), sessions surface newest-first within a
//! runtime, boost requests drain oldest-request-first, and a source is
//! queued at most once across every lane.

use std::collections::{BTreeMap, VecDeque};

use crate::meta::SessionFile;

/// Which lane a popped task came from — the caller picks pass options by it
/// (starter/completion/live passes may carry a slice budget; boost never
/// does).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColdLane {
    Live,
    Boost,
    Starter,
    Completion,
}

/// Newest session first, canonical source path within a session. The latter
/// makes public first-generation ordinals append in their final order,
/// avoiding a growing session-wide rank after every source while preserving
/// the product's newest-first reveal.
pub fn order_runtime_roster(roster: Vec<SessionFile>) -> Vec<SessionFile> {
    let mut sessions: BTreeMap<String, Vec<SessionFile>> = BTreeMap::new();
    for file in roster {
        sessions
            .entry(file.session_id.clone())
            .or_default()
            .push(file);
    }
    let mut sessions = sessions.into_values().collect::<Vec<_>>();
    for files in &mut sessions {
        files.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    }
    sessions.sort_by(|left, right| {
        let left_newest = left.iter().map(|file| file.modified_ms).max().unwrap_or(0);
        let right_newest = right.iter().map(|file| file.modified_ms).max().unwrap_or(0);
        right_newest
            .cmp(&left_newest)
            .then_with(|| left[0].session_id.cmp(&right[0].session_id))
    });
    sessions.into_iter().flatten().collect()
}

#[derive(Debug, Default)]
pub struct ColdSchedule {
    /// Watcher-nominated files across all runtimes, oldest hint first —
    /// freshness beats everything, including a boost.
    live: VecDeque<(usize, SessionFile)>,
    /// Boosted files across all runtimes, oldest request first; one
    /// promotion keeps a session's files in path order.
    boost: VecDeque<(usize, SessionFile)>,
    starter: Vec<VecDeque<SessionFile>>,
    completion: Vec<VecDeque<SessionFile>>,
    starter_rr: usize,
    completion_rr: usize,
    /// Queued tasks per session across every lane — the readiness guard:
    /// a session with anything still queued must not be badged Indexed by a
    /// sibling's pass completing.
    sessions: BTreeMap<String, usize>,
}

impl ColdSchedule {
    pub fn new(rosters: impl IntoIterator<Item = Vec<SessionFile>>) -> Self {
        let starter: Vec<VecDeque<SessionFile>> = rosters
            .into_iter()
            .map(order_runtime_roster)
            .map(VecDeque::from)
            .collect();
        let completion = starter.iter().map(|_| VecDeque::new()).collect();
        let mut sessions: BTreeMap<String, usize> = BTreeMap::new();
        for file in starter.iter().flatten() {
            *sessions.entry(file.session_id.clone()).or_default() += 1;
        }
        Self {
            live: VecDeque::new(),
            boost: VecDeque::new(),
            starter,
            completion,
            starter_rr: 0,
            completion_rr: 0,
            sessions,
        }
    }

    /// A watcher hint nominated this source: move its queued task (from any
    /// cold lane, boost included) into the live lane, or enqueue the given
    /// file fresh when nothing is queued. A source is queued at most once,
    /// so a chatty file cannot flood the lane.
    pub fn nominate_live(&mut self, runtime: usize, file: SessionFile) {
        if self.live.iter().any(|(queued_runtime, queued)| {
            *queued_runtime == runtime && queued.relative_path == file.relative_path
        }) {
            return;
        }
        let take = |queue: &mut VecDeque<SessionFile>| -> Option<SessionFile> {
            let at = queue
                .iter()
                .position(|queued| queued.relative_path == file.relative_path)?;
            queue.remove(at)
        };
        let moved = take(&mut self.starter[runtime])
            .or_else(|| take(&mut self.completion[runtime]))
            .or_else(|| {
                let at = self.boost.iter().position(|(queued_runtime, queued)| {
                    *queued_runtime == runtime && queued.relative_path == file.relative_path
                })?;
                self.boost.remove(at).map(|(_, queued)| queued)
            });
        match moved {
            Some(queued) => self.live.push_back((runtime, queued)),
            None => {
                *self.sessions.entry(file.session_id.clone()).or_default() += 1;
                self.live.push_back((runtime, file));
            }
        }
    }

    /// Whether any task for this session — in any lane — is still queued.
    pub fn session_pending(&self, session_id: &str) -> bool {
        self.sessions.contains_key(session_id)
    }

    fn session_popped(&mut self, session_id: &str) {
        if let Some(count) = self.sessions.get_mut(session_id) {
            *count -= 1;
            if *count == 0 {
                self.sessions.remove(session_id);
            }
        }
    }

    pub fn pending(&self) -> bool {
        !self.live.is_empty()
            || !self.boost.is_empty()
            || self.starter_pending()
            || self.completion_pending()
    }

    /// Whether any un-started source remains — the starter-drained milestone
    /// flips the first time this goes false.
    pub fn starter_pending(&self) -> bool {
        self.starter.iter().any(|queue| !queue.is_empty())
    }

    pub fn completion_pending(&self) -> bool {
        self.completion.iter().any(|queue| !queue.is_empty())
    }

    /// A paused pass hands its file back for the completion lane; repeated
    /// pauses of a large file rotate fairly with its runtime's other work.
    pub fn requeue(&mut self, runtime: usize, file: SessionFile) {
        *self.sessions.entry(file.session_id.clone()).or_default() += 1;
        self.completion[runtime].push_back(file);
    }

    /// Move every queued source of the named sessions into the boost lane,
    /// oldest request first; within one (session, runtime) the files keep
    /// canonical path order whether they waited in starter or completion. A
    /// task already in the LIVE lane stays there — moving it would demote
    /// it. Returns how many files moved (session counts are unchanged — the
    /// tasks are still queued, just ahead of everything else).
    pub fn promote(&mut self, session_ids: impl IntoIterator<Item = String>) -> usize {
        let mut moved = 0;
        for session_id in session_ids {
            for runtime in 0..self.starter.len() {
                let mut matched: Vec<SessionFile> = Vec::new();
                for queue in [&mut self.starter[runtime], &mut self.completion[runtime]] {
                    let mut at = 0;
                    while at < queue.len() {
                        if queue[at].session_id == session_id {
                            matched.push(queue.remove(at).expect("index was in bounds"));
                        } else {
                            at += 1;
                        }
                    }
                }
                matched.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
                moved += matched.len();
                for file in matched {
                    self.boost.push_back((runtime, file));
                }
            }
        }
        moved
    }

    /// Live first, then boost, then every starter task, then completion.
    /// Starter fully drains before completion starts, so breadth ("every
    /// session opens") always beats depth.
    pub fn pop(&mut self) -> Option<(ColdLane, usize, SessionFile)> {
        if let Some((runtime, file)) = self.live.pop_front() {
            self.session_popped(&file.session_id);
            return Some((ColdLane::Live, runtime, file));
        }
        if let Some((runtime, file)) = self.boost.pop_front() {
            self.session_popped(&file.session_id);
            return Some((ColdLane::Boost, runtime, file));
        }
        if let Some((runtime, file)) = pop_round_robin(&mut self.starter, &mut self.starter_rr) {
            self.session_popped(&file.session_id);
            return Some((ColdLane::Starter, runtime, file));
        }
        let popped = pop_round_robin(&mut self.completion, &mut self.completion_rr)
            .map(|(runtime, file)| (ColdLane::Completion, runtime, file));
        if let Some((_, _, file)) = &popped {
            self.session_popped(&file.session_id);
        }
        popped
    }
}

fn pop_round_robin(
    queues: &mut [VecDeque<SessionFile>],
    cursor: &mut usize,
) -> Option<(usize, SessionFile)> {
    let len = queues.len();
    if len == 0 {
        return None;
    }
    let index = (0..len)
        .map(|offset| (*cursor + offset) % len)
        .find(|index| !queues[*index].is_empty())?;
    *cursor = (index + 1) % len;
    queues[index].pop_front().map(|file| (index, file))
}

#[cfg(test)]
mod tests {
    use rosetta_adapter_support::{FileDiscipline, FileFormat, StreamDriver};

    use super::*;

    fn file(session_id: &str) -> SessionFile {
        SessionFile {
            relative_path: format!("{session_id}.jsonl"),
            stream_id: "transcript".into(),
            file_format: FileFormat::Jsonl,
            discipline: FileDiscipline::AppendOnly,
            driver: StreamDriver::JsonLines,
            session_id: session_id.into(),
            native_session_id: session_id.into(),
            size: 1,
            modified_ms: 1,
            agent_version: None,
            source_missing: false,
        }
    }

    fn drain(schedule: &mut ColdSchedule) -> Vec<(ColdLane, usize, String)> {
        std::iter::from_fn(|| schedule.pop())
            .map(|(lane, runtime, file)| (lane, runtime, file.session_id))
            .collect()
    }

    #[test]
    fn starter_is_round_robin_and_preserves_each_runtime_order() {
        let mut schedule = ColdSchedule::new(vec![
            vec![file("a-new"), file("a-old")],
            vec![file("b-new"), file("b-old")],
            vec![file("c-only")],
        ]);
        let order = drain(&mut schedule);
        assert_eq!(
            order,
            [
                (ColdLane::Starter, 0, "a-new".into()),
                (ColdLane::Starter, 1, "b-new".into()),
                (ColdLane::Starter, 2, "c-only".into()),
                (ColdLane::Starter, 0, "a-old".into()),
                (ColdLane::Starter, 1, "b-old".into()),
            ]
        );
    }

    #[test]
    fn one_session_reveals_newest_first_but_activates_sources_in_path_order() {
        let mut older_b = file("older");
        older_b.relative_path = "z.jsonl".into();
        older_b.modified_ms = 10;
        let mut newest_b = file("newest");
        newest_b.relative_path = "z.jsonl".into();
        newest_b.modified_ms = 20;
        let mut newest_a = file("newest");
        newest_a.relative_path = "a.jsonl".into();
        newest_a.modified_ms = 19;
        let mut older_a = file("older");
        older_a.relative_path = "a.jsonl".into();
        older_a.modified_ms = 9;

        let mut schedule = ColdSchedule::new(vec![vec![older_b, newest_b, older_a, newest_a]]);
        let order = std::iter::from_fn(|| schedule.pop())
            .map(|(_, _, file)| (file.session_id, file.relative_path))
            .collect::<Vec<_>>();
        assert_eq!(
            order,
            [
                ("newest".into(), "a.jsonl".into()),
                ("newest".into(), "z.jsonl".into()),
                ("older".into(), "a.jsonl".into()),
                ("older".into(), "z.jsonl".into()),
            ]
        );
    }

    #[test]
    fn boosts_jump_the_rotation_oldest_request_first() {
        let mut schedule = ColdSchedule::new(vec![
            vec![file("a1"), file("a2"), file("a3")],
            vec![file("b1"), file("b2")],
        ]);
        assert_eq!(schedule.pop().unwrap().2.session_id, "a1");
        schedule.promote(["a3".to_string(), "a2".to_string()]);
        assert_eq!(schedule.pop().unwrap().2.session_id, "a3");
        assert_eq!(schedule.pop().unwrap().2.session_id, "a2");
        assert_eq!(schedule.pop().unwrap().2.session_id, "b1");
    }

    #[test]
    fn boost_promotes_every_source_for_the_session_in_path_order() {
        let mut target_z = file("target");
        target_z.relative_path = "z.jsonl".into();
        let mut target_a = file("target");
        target_a.relative_path = "a.jsonl".into();
        let mut schedule = ColdSchedule::new(vec![vec![
            file("first"),
            target_z,
            file("middle"),
            target_a,
        ]]);
        assert_eq!(schedule.pop().unwrap().2.session_id, "first");
        schedule.promote(["target".to_string()]);
        let first = schedule.pop().unwrap().2;
        let second = schedule.pop().unwrap().2;
        assert_eq!(first.relative_path, "a.jsonl");
        assert_eq!(second.relative_path, "z.jsonl");
    }

    #[test]
    fn paused_requeues_drain_only_after_starter_and_rotate_runtimes() {
        let mut schedule = ColdSchedule::new(vec![vec![file("a1"), file("a2")], vec![file("b1")]]);
        // Every starter pop pauses and requeues; completion must not start
        // until the starter lane is empty.
        let mut order = Vec::new();
        while let Some((lane, runtime, popped)) = schedule.pop() {
            order.push((lane, runtime, popped.session_id.clone()));
            if lane == ColdLane::Starter {
                schedule.requeue(runtime, popped);
                assert!(!schedule.starter_pending() || order.len() < 3);
            }
        }
        assert_eq!(
            order,
            [
                (ColdLane::Starter, 0, "a1".into()),
                (ColdLane::Starter, 1, "b1".into()),
                (ColdLane::Starter, 0, "a2".into()),
                (ColdLane::Completion, 0, "a1".into()),
                (ColdLane::Completion, 1, "b1".into()),
                (ColdLane::Completion, 0, "a2".into()),
            ]
        );
    }

    #[test]
    fn live_nominations_outrank_boosts_and_never_double_queue() {
        let mut schedule = ColdSchedule::new(vec![vec![file("cold"), file("hot")]]);
        schedule.promote(["cold".to_string()]);

        // Nominating a queued file MOVES it (session count stable, no dup);
        // nominating it again is a no-op while it waits.
        let hot = file("hot");
        schedule.nominate_live(0, hot.clone());
        schedule.nominate_live(0, hot.clone());
        assert!(schedule.session_pending("hot"));

        let (lane, _, popped) = schedule.pop().unwrap();
        assert_eq!(lane, ColdLane::Live, "freshness beats the boost");
        assert_eq!(popped.session_id, "hot");
        assert!(!schedule.session_pending("hot"), "no duplicate was queued");

        // Once popped, a new hint enqueues it fresh into the live lane.
        schedule.nominate_live(0, hot);
        let (lane, _, popped) = schedule.pop().unwrap();
        assert_eq!((lane, popped.session_id.as_str()), (ColdLane::Live, "hot"));

        let (lane, _, popped) = schedule.pop().unwrap();
        assert_eq!(
            (lane, popped.session_id.as_str()),
            (ColdLane::Boost, "cold")
        );
        assert!(!schedule.pending());
    }

    #[test]
    fn session_pending_tracks_every_lane_until_the_last_pop() {
        let mut first = file("s");
        first.relative_path = "a.jsonl".into();
        let mut second = file("s");
        second.relative_path = "b.jsonl".into();
        let mut schedule = ColdSchedule::new(vec![vec![first, second]]);
        assert!(schedule.session_pending("s"));

        let (_, runtime, popped) = schedule.pop().unwrap();
        assert!(schedule.session_pending("s"), "a sibling is still queued");
        schedule.requeue(runtime, popped);
        assert_eq!(schedule.promote(["s".to_string()]), 2);
        assert!(schedule.session_pending("s"), "boosted tasks still count");

        schedule.pop().unwrap();
        assert!(schedule.session_pending("s"));
        schedule.pop().unwrap();
        assert!(
            !schedule.session_pending("s"),
            "last pop clears the session"
        );
        assert!(!schedule.pending());
    }

    #[test]
    fn promote_pulls_a_paused_file_out_of_the_completion_lane() {
        // Equal timestamps order sessions by id, so "target" pops first.
        let mut schedule = ColdSchedule::new(vec![vec![file("target"), file("zother")]]);
        let (lane, runtime, popped) = schedule.pop().unwrap();
        assert_eq!(lane, ColdLane::Starter);
        assert_eq!(popped.session_id, "target");
        schedule.requeue(runtime, popped);
        schedule.promote(["target".to_string()]);
        let (lane, _, popped) = schedule.pop().unwrap();
        assert_eq!(lane, ColdLane::Boost);
        assert_eq!(popped.session_id, "target");
        let (lane, _, popped) = schedule.pop().unwrap();
        assert_eq!(lane, ColdLane::Starter);
        assert_eq!(popped.session_id, "zother");
        assert!(!schedule.pending());
    }
}
