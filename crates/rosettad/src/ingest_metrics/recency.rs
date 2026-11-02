//! Recency milestones measured from the newest roster entry rather than the
//! wall clock (R1 evidence, brief v3 §9.1): session horizons `@newest`, and
//! the milestones of the root trees of every runtime admitted per tree. A
//! frozen corpus keeps the same membership however long ago it was frozen.
//! Measurement only: nothing here schedules, orders or gates work.
//!
//! Only a session with a public roster source is ever required. A tree's
//! metadata-only placeholder (a member no roster source reads) joins its
//! tree but never becomes readable, so it never holds the tree back, and a
//! tree with no required session at all is not ranked.

use std::cmp::Reverse;
use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use rosetta_pipeline::{TreeId, TreeMap};
use serde_json::{Value, json};

use super::{DAY_MS, LARGE_BYTES, within};

/// The `@newest` horizons, sessions and small trees alike.
const NEWEST_HORIZONS: [(&str, i64); 2] = [("24h@newest", DAY_MS), ("7d@newest", 7 * DAY_MS)];

/// How many of the newest trees `newest-trees-ready` waits for.
const NEWEST_TREES: usize = 10;

/// One milestone line and its `--json --profile` event.
pub(super) type Said = (String, Value);

/// A `recent-ready` horizon measured back from the newest public roster
/// session over every roster, not from the census cut.
pub(super) struct NewestHorizon {
    label: &'static str,
    span_ms: i64,
    /// The newest session modification over the rosters known so far.
    newest: i64,
    /// Sessions within the span of `newest`, by their newest modification.
    /// A later roster that raises `newest` drops those it leaves behind.
    members: HashMap<String, i64>,
    /// Members not readable yet.
    pending: HashSet<String>,
    ready: bool,
}

impl NewestHorizon {
    pub(super) fn all() -> Vec<Self> {
        NEWEST_HORIZONS
            .iter()
            .map(|(label, span_ms)| Self {
                label,
                span_ms: *span_ms,
                newest: i64::MIN,
                members: HashMap::new(),
                pending: HashSet::new(),
                ready: false,
            })
            .collect()
    }

    /// One roster's public sessions, each at its newest source modification.
    pub(super) fn roster(&mut self, sessions: &HashMap<&str, i64>) {
        let newest = sessions.values().copied().fold(self.newest, i64::max);
        let span_ms = self.span_ms;
        if newest > self.newest {
            self.newest = newest;
            let pending = &mut self.pending;
            self.members.retain(|session, modified_ms| {
                let keep = within(newest, *modified_ms, span_ms);
                if !keep {
                    pending.remove(session);
                }
                keep
            });
        }
        for (session, modified_ms) in sessions {
            if !within(self.newest, *modified_ms, span_ms) {
                continue;
            }
            match self.members.entry((*session).to_owned()) {
                Entry::Vacant(slot) => {
                    self.pending.insert(slot.key().clone());
                    slot.insert(*modified_ms);
                }
                Entry::Occupied(mut slot) => {
                    let seen = slot.get_mut();
                    *seen = (*seen).max(*modified_ms);
                }
            }
        }
    }

    pub(super) fn readable(&mut self, session: &str) {
        self.pending.remove(session);
    }

    /// The line, the first time the decided horizon holds no pending session.
    pub(super) fn settle(&mut self, elapsed: f64) -> Option<Said> {
        if self.ready || !self.pending.is_empty() {
            return None;
        }
        self.ready = true;
        let sessions = self.members.len();
        Some((
            format!(
                "milestone: recent-ready horizon={} horizon_sessions={sessions} elapsed_s={elapsed:.2}",
                self.label
            ),
            json!({
                "event": "milestone",
                "milestone": "recent-ready",
                "horizon": self.label,
                "horizonSessions": sessions,
                "elapsedS": elapsed,
            }),
        ))
    }

    #[cfg(test)]
    pub(super) fn members(&self) -> std::collections::BTreeSet<&str> {
        self.members.keys().map(String::as_str).collect()
    }
}

/// The root-tree milestones over every runtime admitted per tree.
/// Runtime-admission runtimes have no trees and contribute nothing here.
pub(super) struct TreeMilestones {
    /// Runtimes whose setup the writer has not opened yet: until none is
    /// left, another runtime's trees could still be the newest.
    setups_unknown: usize,
    /// Each runtime's public roster sessions, from its metadata until its
    /// setup.
    public: HashMap<String, HashSet<String>>,
    runtimes: Vec<RuntimeTrees>,
    /// Decided once every roster and every setup is known.
    targets: Option<Vec<Target>>,
}

/// One root-tree runtime's trees and how far each is from readable.
struct RuntimeTrees {
    runtime_id: String,
    trees: Arc<TreeMap>,
    /// Per tree: its required sessions, the members with a public roster
    /// source.
    required: Vec<u32>,
    /// Per tree: those not readable yet.
    unready: Vec<u32>,
    /// Required sessions not readable yet.
    pending: HashSet<String>,
}

/// One tree milestone: the trees it waits for, by runtime position and tree.
struct Target {
    kind: TargetKind,
    waiting: Vec<(usize, TreeId)>,
    ready: bool,
}

enum TargetKind {
    NewestTrees { n: usize },
    NewestGiant { tree_bytes: u64 },
    SmallRecent { horizon: &'static str, trees: usize },
}

impl TargetKind {
    fn said(&self, elapsed: f64) -> Said {
        match self {
            Self::NewestTrees { n } => (
                format!("milestone: newest-trees-ready n={n} elapsed_s={elapsed:.2}"),
                json!({
                    "event": "milestone",
                    "milestone": "newest-trees-ready",
                    "n": n,
                    "elapsedS": elapsed,
                }),
            ),
            Self::NewestGiant { tree_bytes } => (
                format!(
                    "milestone: newest-giant-ready tree_bytes={tree_bytes} elapsed_s={elapsed:.2}"
                ),
                json!({
                    "event": "milestone",
                    "milestone": "newest-giant-ready",
                    "treeBytes": tree_bytes,
                    "elapsedS": elapsed,
                }),
            ),
            Self::SmallRecent { horizon, trees } => (
                format!(
                    "milestone: small-recent-ready horizon={horizon} trees={trees} elapsed_s={elapsed:.2}"
                ),
                json!({
                    "event": "milestone",
                    "milestone": "small-recent-ready",
                    "horizon": horizon,
                    "trees": trees,
                    "elapsedS": elapsed,
                }),
            ),
        }
    }
}

impl TreeMilestones {
    /// `runtimes` setups are expected, one per roster.
    pub(super) fn new(runtimes: usize) -> Self {
        Self {
            setups_unknown: runtimes,
            public: HashMap::new(),
            runtimes: Vec::new(),
            targets: None,
        }
    }

    /// One runtime's public roster sessions, held until its setup.
    pub(super) fn roster(&mut self, runtime_id: &str, public: HashSet<String>) {
        self.public.insert(runtime_id.to_owned(), public);
    }

    /// One runtime's setup: its trees when it is admitted per tree, `None`
    /// for runtime admission (declared, fallen back or forced). Arrives
    /// before any of its sessions can be readable: a producer opens its
    /// runtime before it sends a source.
    pub(super) fn known(&mut self, runtime_id: &str, trees: Option<Arc<TreeMap>>) {
        self.setups_unknown = self.setups_unknown.saturating_sub(1);
        let public = self.public.remove(runtime_id).unwrap_or_default();
        let Some(trees) = trees else {
            return;
        };
        let mut required = vec![0_u32; trees.trees().len()];
        let mut pending = HashSet::with_capacity(public.len());
        for session in public {
            if let Some(tree) = trees.tree_of(&session) {
                required[tree as usize] += 1;
                pending.insert(session);
            }
        }
        self.runtimes.push(RuntimeTrees {
            runtime_id: runtime_id.to_owned(),
            trees,
            unready: required.clone(),
            required,
            pending,
        });
    }

    pub(super) fn readable(&mut self, runtime_id: &str, session: &str) {
        let Some(runtime) = self
            .runtimes
            .iter_mut()
            .find(|runtime| runtime.runtime_id == runtime_id)
        else {
            return;
        };
        if runtime.pending.remove(session)
            && let Some(tree) = runtime.trees.tree_of(session)
        {
            let unready = &mut runtime.unready[tree as usize];
            *unready = unready.saturating_sub(1);
        }
    }

    /// Every milestone that became ready, once the caller has decided its
    /// horizons (every roster known and some session readable) and every
    /// setup is known.
    pub(super) fn settle(&mut self, elapsed: f64) -> Vec<Said> {
        if self.setups_unknown > 0 {
            return Vec::new();
        }
        let runtimes = &self.runtimes;
        let targets = self.targets.get_or_insert_with(|| decide(runtimes));
        let mut said = Vec::new();
        for target in targets.iter_mut().filter(|target| !target.ready) {
            target
                .waiting
                .retain(|(runtime, tree)| runtimes[*runtime].unready[*tree as usize] > 0);
            if target.waiting.is_empty() {
                target.ready = true;
                said.push(target.kind.said(elapsed));
            }
        }
        said
    }
}

/// The tree milestones over every root-tree runtime: none without one. Trees
/// rank newest first by k, then smallest id (then runtime id, across
/// runtimes); "newest" is the first ranked tree's k.
fn decide(runtimes: &[RuntimeTrees]) -> Vec<Target> {
    if runtimes.is_empty() {
        return Vec::new();
    }
    let mut ranked = runtimes
        .iter()
        .enumerate()
        .flat_map(|(position, runtime)| {
            runtime
                .trees
                .trees()
                .iter()
                .zip(&runtime.required)
                .enumerate()
                .filter(|(_, (_, required))| **required > 0)
                .map(move |(tree, (facts, _))| {
                    let tree = TreeId::try_from(tree).expect("tree ids are u32");
                    (position, tree, facts)
                })
        })
        .collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        (Reverse(left.2.k), &left.2.id, &runtimes[left.0].runtime_id).cmp(&(
            Reverse(right.2.k),
            &right.2.id,
            &runtimes[right.0].runtime_id,
        ))
    });
    let key = |(position, tree, _): &(usize, TreeId, _)| (*position, *tree);
    let target = |kind, waiting| Target {
        kind,
        waiting,
        ready: false,
    };
    let n = ranked.len().min(NEWEST_TREES);
    let mut targets = vec![target(
        TargetKind::NewestTrees { n },
        ranked[..n].iter().map(key).collect(),
    )];
    if let Some(giant) = ranked
        .iter()
        .find(|(_, _, facts)| facts.max_source_bytes > LARGE_BYTES)
    {
        targets.push(target(
            TargetKind::NewestGiant {
                tree_bytes: giant.2.bytes,
            },
            vec![key(giant)],
        ));
    }
    let newest = ranked.first().map(|(_, _, facts)| facts.k);
    for (horizon, span_ms) in NEWEST_HORIZONS {
        let waiting = ranked
            .iter()
            .filter(|(_, _, facts)| {
                facts.max_source_bytes <= LARGE_BYTES
                    && newest.is_some_and(|newest| within(newest, facts.k, span_ms))
            })
            .map(key)
            .collect::<Vec<_>>();
        targets.push(target(
            TargetKind::SmallRecent {
                horizon,
                trees: waiting.len(),
            },
            waiting,
        ));
    }
    targets
}
