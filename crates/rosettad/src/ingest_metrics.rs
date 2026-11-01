//! Behaviour-neutral measurement of the fresh cold build (docs/ARCHITECTURE.md,
//! "Cold-build measurement"). The sole canonical writer reports what it
//! already knows as it publishes — `milestone:` and `census:` stdout lines,
//! plus JSON events under `--json --profile` — and a sampler thread traces
//! the process's memory. None of it schedules, orders, gates or fails work: a
//! measurement that cannot be taken is reported as missing, never as an error.
//!
//! Census lines carry structure only — counts, byte sizes and ages — never a
//! path, a session id or content.

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use rosetta_adapter_support::StreamDriver;
use rosetta_pipeline::{ClosedSource, Instrument, Metric, SessionFile};
use rosetta_store::Store;
use serde_json::{Value, json};

use crate::bench;

mod memory;
pub(crate) use memory::MemorySampler;

const HOUR_MS: i64 = 60 * 60 * 1_000;
const DAY_MS: i64 = 24 * HOUR_MS;

/// The recent-ready horizons: a roster session is recent when its newest
/// source was modified at most this long before the census cut.
const HORIZONS: [(&str, i64); 2] = [("24h", DAY_MS), ("7d", 7 * DAY_MS)];

/// Census age buckets, cumulative: each counts every file at most that old,
/// so `le30d` includes `le7d`; `older` is the rest.
const AGES: [(&str, i64); 4] = [
    ("le1h", HOUR_MS),
    ("le24h", DAY_MS),
    ("le7d", 7 * DAY_MS),
    ("le30d", 30 * DAY_MS),
];

/// How many of a runtime's largest file sizes the census reports.
const LARGEST_FILES: usize = 20;

/// A session or tree over this many roster bytes counts as large: the size
/// the recency plan's big-tree deferral is to be set from.
const LARGE_BYTES: u64 = 64 * 1024 * 1024;

/// The streamed writer's milestones, census and horizon bookkeeping. Owned
/// by the writer thread; every call is one map update or one line.
pub(crate) struct ColdMilestones {
    started: Instant,
    /// Wall-clock milliseconds the census ages and horizons are measured
    /// back from: the moment the writer starts, just after the census walk.
    cut_ms: i64,
    events: bool,
    /// Runtimes whose roster has not been published yet. A horizon is only
    /// decided once every roster is known.
    rosters_unknown: usize,
    runtimes: usize,
    admitted: usize,
    first_readable: bool,
    /// Roster sources per session the writer has not applied yet, until the
    /// first session has all of them (then emptied).
    remaining: HashMap<String, u32>,
    first_complete: bool,
    horizons: Vec<Horizon>,
}

struct Horizon {
    label: &'static str,
    span_ms: i64,
    /// Roster sessions inside the horizon, over every known roster.
    sessions: usize,
    /// Those of them still Pending.
    pending: HashSet<String>,
    ready: bool,
}

impl ColdMilestones {
    /// `runtimes` rosters are expected; `events` adds the JSON census event.
    pub(crate) fn new(started: Instant, cut_ms: i64, runtimes: usize, events: bool) -> Self {
        Self {
            started,
            cut_ms,
            events,
            rosters_unknown: runtimes,
            runtimes,
            admitted: 0,
            first_readable: false,
            remaining: HashMap::new(),
            first_complete: false,
            horizons: HORIZONS
                .iter()
                .map(|(label, span_ms)| Horizon {
                    label,
                    span_ms: *span_ms,
                    sessions: 0,
                    pending: HashSet::new(),
                    ready: false,
                })
                .collect(),
        }
    }

    fn elapsed(&self) -> f64 {
        self.started.elapsed().as_secs_f64()
    }

    /// One runtime's metadata became public `public_at` after serve start
    /// and its roster is known: the milestone, and its sessions joining the
    /// horizons. Runs just before the runtime's producer starts, so it does
    /// roster work in RAM only, of the order `ColdProgress::observe` already
    /// does there; the census's store read and its line wait for
    /// [`ColdMilestones::census`], once the producer is running.
    pub(crate) fn metadata_public(
        &mut self,
        public_at: Duration,
        runtime_id: &str,
        kind: &'static str,
        roster: &[SessionFile],
    ) -> RosterCensus {
        println!(
            "milestone: runtime-metadata-public runtime={runtime_id} agent={kind} elapsed_s={:.2}",
            public_at.as_secs_f64()
        );
        let (census, per_session) = Census::of(roster.iter().map(CensusFile::from), self.cut_ms);
        let newest = newest_by_session(roster);
        for horizon in &mut self.horizons {
            for (session, modified_ms) in &newest {
                if within(self.cut_ms, *modified_ms, horizon.span_ms)
                    && horizon.pending.insert((*session).to_owned())
                {
                    horizon.sessions += 1;
                }
            }
        }
        if !self.first_complete {
            for file in roster.iter().filter(|file| !file.ledger_only) {
                *self.remaining.entry(file.session_id.clone()).or_default() += 1;
            }
        }
        self.rosters_unknown = self.rosters_unknown.saturating_sub(1);
        self.settle();
        RosterCensus {
            runtime_id: runtime_id.to_owned(),
            kind,
            census,
            per_session,
        }
    }

    /// Print one runtime's census, its sessions grouped into trees by their
    /// settled roots: a store read, so the writer runs it once the runtime's
    /// producer has started.
    pub(crate) fn census(&self, roster: RosterCensus, store: &Store) {
        // Settled metadata names each tree's root. Without it every session
        // is its own tree; a measurement never fails the build.
        let roots = store
            .runtime_session_roots(&roster.runtime_id)
            .unwrap_or_default();
        let census = roster.census.with_roots(&roster.per_session, &roots);
        println!("{}", census.line(&roster.runtime_id, roster.kind));
        if self.events {
            bench::emit(&census.event(&roster.runtime_id, roster.kind));
        }
    }

    /// Before the writer applies `sources`: whether they complete a session
    /// (its last roster source among them) while none has completed yet.
    /// [`ColdMilestones::first_session_complete`] follows a true answer once
    /// the apply has committed.
    pub(crate) fn completes_first_session(&mut self, sources: &[ClosedSource]) -> bool {
        if self.first_complete {
            return false;
        }
        let mut completes = false;
        for session in sources.iter().filter_map(ClosedSource::session_id) {
            if let Some(left) = self.remaining.get_mut(session) {
                *left = left.saturating_sub(1);
                completes |= *left == 0;
            }
        }
        completes
    }

    /// The counterfactual first-readable on today's path: the first session
    /// whose every roster source the writer has applied. An optimistic lower
    /// bound for any finer flip, not a moment that session was publishable:
    /// facts of a session another source crossed, a resolved session's
    /// leftover usage and file edits, and ordinals still to canonicalize
    /// wait for the runtime fold at admission.
    pub(crate) fn first_session_complete(&mut self) {
        if self.first_complete {
            return;
        }
        self.first_complete = true;
        self.remaining = HashMap::new();
        println!(
            "milestone: first-session-complete elapsed_s={:.2}",
            self.elapsed()
        );
    }

    /// One runtime was admitted: `sessions` are no longer Pending, and no
    /// other session of its `roster` can be.
    pub(crate) fn readable(
        &mut self,
        runtime_id: &str,
        kind: &str,
        sessions: &[String],
        roster: &[SessionFile],
    ) {
        let elapsed = self.elapsed();
        println!(
            "milestone: runtime-readable runtime={runtime_id} agent={kind} sessions={} elapsed_s={elapsed:.2}",
            sessions.len()
        );
        if !self.first_readable && !sessions.is_empty() {
            self.first_readable = true;
            println!("milestone: first-readable elapsed_s={elapsed:.2}");
        }
        let done = sessions
            .iter()
            .map(String::as_str)
            .chain(roster.iter().map(|file| file.session_id.as_str()));
        for session in done {
            for horizon in &mut self.horizons {
                horizon.pending.remove(session);
            }
        }
        self.admitted += 1;
        self.settle();
    }

    /// Print each horizon the first moment it is decided and holds no
    /// Pending session. Decided means every roster is known and some
    /// admission made a session readable (or every runtime is admitted), so
    /// an empty horizon reads ready at the first such admission and never
    /// before `first-readable`.
    fn settle(&mut self) {
        let admitted = self.first_readable || self.admitted == self.runtimes;
        if self.rosters_unknown > 0 || !admitted {
            return;
        }
        let elapsed = self.elapsed();
        for horizon in &mut self.horizons {
            if horizon.ready || !horizon.pending.is_empty() {
                continue;
            }
            horizon.ready = true;
            println!(
                "milestone: recent-ready horizon={} horizon_sessions={} elapsed_s={elapsed:.2}",
                horizon.label, horizon.sessions
            );
        }
    }
}

/// Whether a file modified at `modified_ms` is at most `span_ms` older than
/// the cut; a file modified after the cut is inside every span.
fn within(cut_ms: i64, modified_ms: i64, span_ms: i64) -> bool {
    cut_ms.saturating_sub(modified_ms) <= span_ms
}

/// Each public roster session's newest source modification.
fn newest_by_session(roster: &[SessionFile]) -> HashMap<&str, i64> {
    let mut newest: HashMap<&str, i64> = HashMap::new();
    for file in roster.iter().filter(|file| !file.ledger_only) {
        let entry = newest
            .entry(file.session_id.as_str())
            .or_insert(file.modified_ms);
        *entry = (*entry).max(file.modified_ms);
    }
    newest
}

/// What the census reads of one roster source.
pub(crate) struct CensusFile<'a> {
    /// The physical file it reads, a grouping key only: never printed.
    pub(crate) physical: &'a str,
    pub(crate) size: u64,
    pub(crate) modified_ms: i64,
    /// Its public session; `None` for a ledger-only source.
    pub(crate) session: Option<&'a str>,
}

impl<'a> From<&'a SessionFile> for CensusFile<'a> {
    fn from(file: &'a SessionFile) -> Self {
        Self {
            physical: physical_path(file),
            size: file.size,
            modified_ms: file.modified_ms,
            session: (!file.ledger_only).then_some(file.session_id.as_str()),
        }
    }
}

/// The physical file a roster source reads. Each native session of a
/// snapshot database or document is a virtual source
/// (`<path>#<native-id>`) carrying the whole file's size; a transcript is
/// its own file.
fn physical_path(file: &SessionFile) -> &str {
    if file.driver == StreamDriver::JsonLines {
        return &file.relative_path;
    }
    file.relative_path
        .strip_suffix(file.native_session_id.as_str())
        .and_then(|path| path.strip_suffix('#'))
        .unwrap_or(&file.relative_path)
}

/// One physical file of a roster: its size, its newest source's
/// modification, and whose bytes it holds.
struct Physical<'a> {
    size: u64,
    modified_ms: i64,
    owner: Owner<'a>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Owner<'a> {
    /// Every source of it belongs to this public session.
    Session(&'a str),
    /// Every source of it is ledger-only.
    Ledger,
    /// Sources of several sessions share it: a snapshot database or
    /// document. Its bytes are no one session's.
    Shared,
}

impl<'a> Owner<'a> {
    fn of(session: Option<&'a str>) -> Self {
        session.map_or(Self::Ledger, Self::Session)
    }
}

/// Files and bytes.
pub(crate) type Tally = (u64, u64);

/// A runtime's census as its roster alone tells it, held while the
/// runtime's producer starts; [`ColdMilestones::census`] groups its trees
/// and prints it.
#[must_use]
pub(crate) struct RosterCensus {
    runtime_id: String,
    kind: &'static str,
    census: Census,
    /// Each public session's own bytes: what its tree sums.
    per_session: Vec<(String, u64)>,
}

/// One runtime's roster, structure only. Every physical file counts once:
/// the virtual sources of a snapshot database or document are one file,
/// whose age is its newest source's.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Census {
    pub(crate) files: u64,
    pub(crate) bytes: u64,
    /// Cumulative per [`AGES`] span.
    pub(crate) ages: [Tally; AGES.len()],
    /// Older than the last span.
    pub(crate) older: Tally,
    /// Files several sessions' sources share, charged to none of them.
    pub(crate) shared: Tally,
    /// The largest file sizes, descending.
    pub(crate) largest: Vec<u64>,
    /// Own bytes per public session: the files it alone reads. A session
    /// that only reads shared files has none and is not counted.
    pub(crate) sessions: Distribution,
    /// Own bytes per tree: sessions grouped by their settled root. Empty
    /// until [`Census::with_roots`].
    pub(crate) roots: Distribution,
}

impl Census {
    /// Everything `files` alone tells, plus each public session's own bytes
    /// for [`Census::with_roots`].
    pub(crate) fn of<'a>(
        files: impl IntoIterator<Item = CensusFile<'a>>,
        cut_ms: i64,
    ) -> (Self, Vec<(String, u64)>) {
        let mut physical: HashMap<&str, Physical<'a>> = HashMap::new();
        for file in files {
            let owner = Owner::of(file.session);
            match physical.entry(file.physical) {
                Entry::Vacant(slot) => {
                    slot.insert(Physical {
                        size: file.size,
                        modified_ms: file.modified_ms,
                        owner,
                    });
                }
                Entry::Occupied(mut slot) => {
                    let seen = slot.get_mut();
                    seen.size = seen.size.max(file.size);
                    seen.modified_ms = seen.modified_ms.max(file.modified_ms);
                    if seen.owner != owner {
                        seen.owner = Owner::Shared;
                    }
                }
            }
        }
        let mut census = Self {
            files: 0,
            bytes: 0,
            ages: [(0, 0); AGES.len()],
            older: (0, 0),
            shared: (0, 0),
            largest: Vec::new(),
            sessions: Distribution::default(),
            roots: Distribution::default(),
        };
        let mut sizes = Vec::with_capacity(physical.len());
        let mut per_session: HashMap<&str, u64> = HashMap::new();
        for file in physical.into_values() {
            census.files += 1;
            census.bytes = census.bytes.saturating_add(file.size);
            let mut placed = false;
            for (tally, (_, span_ms)) in census.ages.iter_mut().zip(AGES) {
                if within(cut_ms, file.modified_ms, span_ms) {
                    add(tally, file.size);
                    placed = true;
                }
            }
            if !placed {
                add(&mut census.older, file.size);
            }
            sizes.push(file.size);
            match file.owner {
                Owner::Session(session) => {
                    let total = per_session.entry(session).or_default();
                    *total = total.saturating_add(file.size);
                }
                Owner::Shared => add(&mut census.shared, file.size),
                Owner::Ledger => {}
            }
        }
        census.largest = largest(sizes);
        census.sessions = Distribution::of(per_session.values().copied().collect());
        let per_session = per_session
            .into_iter()
            .map(|(session, bytes)| (session.to_owned(), bytes))
            .collect();
        (census, per_session)
    }

    /// Group `per_session` into trees. `roots` maps a session to its tree's
    /// root session; an unmapped session is a root.
    pub(crate) fn with_roots(
        mut self,
        per_session: &[(String, u64)],
        roots: &[(String, String)],
    ) -> Self {
        let root_of: HashMap<&str, &str> = roots
            .iter()
            .map(|(session, root)| (session.as_str(), root.as_str()))
            .collect();
        let mut per_root: HashMap<&str, u64> = HashMap::new();
        for (session, total) in per_session {
            let root = root_of.get(session.as_str()).copied().unwrap_or(session);
            let tree = per_root.entry(root).or_default();
            *tree = tree.saturating_add(*total);
        }
        self.roots = Distribution::of(per_root.into_values().collect());
        self
    }

    /// The one stdout line: age buckets are cumulative `files/bytes`.
    pub(crate) fn line(&self, runtime_id: &str, kind: &str) -> String {
        let mut line = format!(
            "census: runtime={runtime_id} agent={kind} files={} bytes={}",
            self.files, self.bytes
        );
        for ((label, _), (files, bytes)) in AGES.iter().zip(self.ages) {
            line.push_str(&format!(" {label}={files}/{bytes}"));
        }
        line.push_str(&format!(" older={}/{}", self.older.0, self.older.1));
        line.push_str(&format!(" shared={}/{}", self.shared.0, self.shared.1));
        line.push_str(&self.sessions.fields("session"));
        line.push_str(&self.roots.fields("root"));
        let largest = self
            .largest
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join(",");
        line.push_str(&format!(" largest={largest}"));
        line
    }

    /// The `--json --profile` event.
    pub(crate) fn event(&self, runtime_id: &str, kind: &str) -> Value {
        let mut ages = serde_json::Map::new();
        for ((label, _), (files, bytes)) in AGES.iter().zip(self.ages) {
            ages.insert((*label).to_owned(), json!({"files": files, "bytes": bytes}));
        }
        ages.insert(
            "older".to_owned(),
            json!({"files": self.older.0, "bytes": self.older.1}),
        );
        json!({
            "event": "census",
            "runtime": runtime_id,
            "agent": kind,
            "files": self.files,
            "bytes": self.bytes,
            "agesCumulative": true,
            "ages": ages,
            "shared": {"files": self.shared.0, "bytes": self.shared.1},
            "largestFileBytes": self.largest,
            "sessionBytes": self.sessions.event(),
            "rootBytes": self.roots.event(),
        })
    }
}

fn add(tally: &mut Tally, bytes: u64) {
    tally.0 += 1;
    tally.1 = tally.1.saturating_add(bytes);
}

/// The [`LARGEST_FILES`] largest of `sizes`, descending, without sorting
/// the rest.
fn largest(mut sizes: Vec<u64>) -> Vec<u64> {
    if sizes.len() > LARGEST_FILES {
        sizes.select_nth_unstable_by(LARGEST_FILES - 1, |left, right| right.cmp(left));
        sizes.truncate(LARGEST_FILES);
    }
    sizes.sort_unstable_by(|left, right| right.cmp(left));
    sizes
}

/// A byte-size distribution: nearest-rank percentiles over every value.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Distribution {
    pub(crate) count: u64,
    pub(crate) p50: u64,
    pub(crate) p90: u64,
    pub(crate) p99: u64,
    pub(crate) max: u64,
    /// Values over [`LARGE_BYTES`].
    pub(crate) large: u64,
}

impl Distribution {
    pub(crate) fn of(mut values: Vec<u64>) -> Self {
        values.sort_unstable();
        Self {
            count: values.len() as u64,
            p50: percentile(&values, 50),
            p90: percentile(&values, 90),
            p99: percentile(&values, 99),
            max: values.last().copied().unwrap_or(0),
            large: values.iter().filter(|value| **value > LARGE_BYTES).count() as u64,
        }
    }

    fn fields(&self, unit: &str) -> String {
        format!(
            " {unit}s={} {unit}_p50={} {unit}_p90={} {unit}_p99={} {unit}_max={} {unit}s_over_64mib={}",
            self.count, self.p50, self.p90, self.p99, self.max, self.large
        )
    }

    fn event(&self) -> Value {
        json!({
            "count": self.count,
            "p50": self.p50,
            "p90": self.p90,
            "p99": self.p99,
            "max": self.max,
            "over64MiB": self.large,
        })
    }
}

/// The nearest-rank `pct` percentile of ascending `sorted`: the smallest
/// value at least `pct`% of the values do not exceed. Zero for none.
pub(crate) fn percentile(sorted: &[u64], pct: u64) -> u64 {
    let Some(last) = sorted.len().checked_sub(1) else {
        return 0;
    };
    let rank = (pct.min(100) * sorted.len() as u64).div_ceil(100).max(1);
    sorted[usize::try_from(rank - 1).unwrap_or(last).min(last)]
}

/// Cross-file usage ownership outcomes, from the canonical profiler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct UsageCopies {
    /// Identity-bearing records admitted as their owner's: the denominator.
    pub(crate) owned: u64,
    /// Copies dropped on arrival: an earlier-started session owned them.
    pub(crate) dropped: u64,
    /// Records that displaced the owner, whose usage was retracted.
    pub(crate) displaced: u64,
    /// Cold-folded tool-row usage records whose identity was never claimed.
    pub(crate) unclaimed: u64,
}

impl UsageCopies {
    pub(crate) fn of(instrument: &Instrument) -> Self {
        let snapshot = instrument.snapshot();
        Self {
            owned: snapshot.get(Metric::UsageIdentitiesOwned),
            dropped: snapshot.get(Metric::UsageCopiesDropped),
            displaced: snapshot.get(Metric::UsageCopiesDisplaced),
            unclaimed: snapshot.get(Metric::UsageIdentitiesUnclaimed),
        }
    }
}

/// The closing measurement, printed right after `milestone: canonical-ready`:
/// the memory peak (which also ends the compact memory lines) and the
/// usage-copy counters.
pub(crate) fn after_canonical_ready(started: Instant, instrument: &Instrument) {
    memory::canonical_ready(started);
    let usage = UsageCopies::of(instrument);
    println!(
        "usage-copies: dropped={} displaced={} owned={} unclaimed_usage={}",
        usage.dropped, usage.displaced, usage.owned, usage.unclaimed
    );
}

#[cfg(test)]
mod tests;
