//! The ingest plan: meta's settled certainty as a first-class RAM value.
//!
//! After the bounded skims commit, everything the cold phase will do is
//! already known — every project, every session, every source file and its
//! census size. This module holds that knowledge as data (projects →
//! sessions → sources) instead of leaving it implicit in rosters and SQL.
//! The scheduler seeds its lanes from the same rosters; the plan adds the
//! ownership dimension a per-project progress fold needs.
//!
//! Iron rule 5: the plan is RAM only, rebuilt from the store and the rosters
//! on every run, and free to crash. Sessions whose ownership the metadata
//! source could not prove keep `project_id = None` — the unassigned bucket
//! is modeled honestly, never invented. A multi-session source stays ONE
//! planned file attributed to its roster session: the task is the physical
//! pass, and project progress is a fold over attributions, not a claim of
//! per-row precision.

use std::collections::BTreeMap;

use crate::meta::SessionFile;

/// One session's share of the cold work, as the census measured it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionPlan {
    /// Canonical project this session belongs to; `None` is the real
    /// unassigned bucket (ownership unproven by bounded meta).
    pub project_id: Option<String>,
    /// Source files attributed to this session.
    pub files: u64,
    /// Census bytes across those files.
    pub bytes: u64,
}

/// One runtime's planned sessions, keyed by canonical session id.
#[derive(Debug, Default)]
struct RuntimePlan {
    sessions: BTreeMap<String, SessionPlan>,
}

/// The settled cold-work inventory, indexed like the backfill roster list.
#[derive(Debug, Default)]
pub struct IngestPlan {
    runtimes: Vec<RuntimePlan>,
}

impl IngestPlan {
    /// Settle the plan from the enrolled rosters and the ownership the
    /// metadata batches committed (`session_id → project_id`). A roster
    /// session the ownership map does not name stays unassigned — the map is
    /// read from canonical rows, so absence means meta proved nothing.
    pub fn settle(
        rosters: &[Vec<SessionFile>],
        ownership: &BTreeMap<String, Option<String>>,
    ) -> Self {
        let mut plan = Self {
            runtimes: rosters.iter().map(|_| RuntimePlan::default()).collect(),
        };
        for (index, roster) in rosters.iter().enumerate() {
            for file in roster {
                plan.admit(
                    index,
                    file,
                    ownership.get(&file.session_id).cloned().flatten(),
                );
            }
        }
        plan
    }

    /// Admit one source file — plan construction, and the mid-cold admission
    /// path where a fresh meta rerun surfaced a brand-new session. Returns
    /// `true` when this file's session was new to the plan.
    pub fn admit(
        &mut self,
        runtime: usize,
        file: &SessionFile,
        project_id: Option<String>,
    ) -> bool {
        let Some(entry) = self.runtimes.get_mut(runtime) else {
            return false;
        };
        let new_session = !entry.sessions.contains_key(&file.session_id);
        let session = entry.sessions.entry(file.session_id.clone()).or_default();
        if new_session {
            session.project_id = project_id;
        }
        session.files += 1;
        session.bytes += file.size;
        new_session
    }

    /// The settled project of one planned session; the outer `None` means
    /// the session is not in the plan at all.
    pub fn project_of(&self, runtime: usize, session_id: &str) -> Option<&SessionPlan> {
        self.runtimes.get(runtime)?.sessions.get(session_id)
    }

    /// Every planned session with its runtime index — the board's seed.
    pub fn sessions(&self) -> impl Iterator<Item = (usize, &str, &SessionPlan)> {
        self.runtimes.iter().enumerate().flat_map(|(index, entry)| {
            entry
                .sessions
                .iter()
                .map(move |(session_id, plan)| (index, session_id.as_str(), plan))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rosetta_adapter_support::{FileDiscipline, FileFormat, StreamDriver};

    fn file(path: &str, session_id: &str, size: u64) -> SessionFile {
        SessionFile {
            relative_path: path.into(),
            stream_id: "transcript".into(),
            file_format: FileFormat::Jsonl,
            discipline: FileDiscipline::AppendOnly,
            driver: StreamDriver::JsonLines,
            session_id: session_id.into(),
            native_session_id: format!("native-{session_id}"),
            size,
            modified_ms: 0,
            agent_version: None,
            source_missing: false,
        }
    }

    fn owned(pairs: &[(&str, Option<&str>)]) -> BTreeMap<String, Option<String>> {
        pairs
            .iter()
            .map(|(session, project)| ((*session).to_string(), project.map(str::to_string)))
            .collect()
    }

    #[test]
    fn settle_folds_files_into_sessions_and_keeps_unproven_ownership_absent() {
        let rosters = vec![
            vec![
                file("a.jsonl", "s1", 100),
                file("a-sibling.jsonl", "s1", 40),
                file("b.jsonl", "s2", 7),
            ],
            vec![file("c.jsonl", "s3", 9)],
        ];
        // s2 is known unassigned; s3 is absent from the map entirely — both
        // land in the honest unassigned bucket.
        let plan = IngestPlan::settle(&rosters, &owned(&[("s1", Some("p1")), ("s2", None)]));

        let s1 = plan.project_of(0, "s1").unwrap();
        assert_eq!(s1.project_id.as_deref(), Some("p1"));
        assert_eq!((s1.files, s1.bytes), (2, 140));
        assert_eq!(plan.project_of(0, "s2").unwrap().project_id, None);
        assert_eq!(plan.project_of(1, "s3").unwrap().project_id, None);
        assert!(plan.project_of(1, "s1").is_none(), "plans are per-runtime");

        let mut seeds: Vec<_> = plan
            .sessions()
            .map(|(runtime, session, row)| (runtime, session.to_string(), row.bytes))
            .collect();
        seeds.sort();
        assert_eq!(
            seeds,
            [
                (0, "s1".to_string(), 140),
                (0, "s2".to_string(), 7),
                (1, "s3".to_string(), 9),
            ]
        );
    }

    #[test]
    fn admissions_add_new_sessions_and_extend_known_ones() {
        let rosters = vec![vec![file("a.jsonl", "s1", 100)]];
        let mut plan = IngestPlan::settle(&rosters, &owned(&[("s1", Some("p1"))]));

        assert!(
            plan.admit(0, &file("fresh.jsonl", "s-new", 50), Some("p2".into())),
            "a brand-new session is new to the plan"
        );
        assert!(
            !plan.admit(0, &file("a-growth.jsonl", "s1", 25), None),
            "another file for a known session is not"
        );
        let s1 = plan.project_of(0, "s1").unwrap();
        assert_eq!(
            s1.project_id.as_deref(),
            Some("p1"),
            "a later admission never rewrites settled ownership"
        );
        assert_eq!((s1.files, s1.bytes), (2, 125));
        assert!(
            !plan.admit(9, &file("x.jsonl", "sx", 1), None),
            "an unknown runtime index admits nothing"
        );
    }
}
