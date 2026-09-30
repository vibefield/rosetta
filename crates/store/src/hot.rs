//! store::hot — the in-process hot tier (docs/ingest-engine.html, "The hot
//! tier"). Redis-like in keyspace, pub/sub discipline, and invalidation,
//! never an external cache server. Two structures, two coherence classes:
//!
//! * [`TailRings`] — per-session ring buffers of mutation-log entries tagged
//!   `(epoch, mutation_seq)`. Delivery is a MUTATION LOG, not ordinal
//!   replay: tool-result and usage merges mutate earlier rows, so ordinals
//!   cannot sequence a subscription. Ring overflow or a dead epoch answers
//!   [`Replay::Expired`] — the subscriber takes a replacement snapshot.
//! * [`MetaMirror`] — the whole session/project/runtime metadata surface,
//!   fully resident, refreshed strictly after commit; row diffs become
//!   precise events. List/browse reads never touch the DB.
//!
//! Effects are the seam between the store's write path and this tier, split
//! by granularity because the two halves cost very different amounts:
//!
//! * [`Effects`] — row level. Every committed batch appends the row
//!   mutations it projected, which is only worth paying for sessions a
//!   reader is watching; the cold paths suppress it deliberately.
//! * [`MetaEffects`] — metadata level. Identity keys only, so it is cheap
//!   enough to stay on everywhere, including the cold generations a fresh
//!   `serve` projects before its final canonical gate.
//!
//! Row [`Effects`] are what the daemon drains and feeds to the hub today;
//! [`MetaEffects`] is the capability the cold-start wiring will drain, in
//! place of the whole-surface rewarm `serve` still runs on a timer.
//! Everything here is rebuilt from the projection on restart, the way the
//! projection rebuilds from the ledger — nothing is durable, and no clock is
//! read internally (callers pass `now` so tests stay deterministic).

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::Arc;

use rusqlite::Connection;

use rosetta_core::IngestOp;
use rosetta_core::rows::{Ms, ProjectRow, RuntimeRow, SessionRow, TokenSnapshot};
use rosetta_core::{TimelineBody, TimelineEntry, ToolInteraction};

use crate::{StoreError, queries};

/// One projected mutation, as written. At-least-once redelivery is safe:
/// clients key by `timeline_id` (deterministic ids make replays idempotent).
/// This is a wire type: the hub serializes it into WebSocket frames, tagged
/// by `type`.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub enum Mutation {
    /// Full message-row upsert — insert or replacement — carrying the
    /// ordinal the apply chose.
    Message(Box<TimelineEntry>),
    /// Final typed tool state after a lifecycle patch merged into its opener.
    /// Native result bodies never enter this mutation log.
    ToolUpdated {
        session_id: String,
        timeline_id: String,
        tool: Box<ToolInteraction>,
        at: Option<Ms>,
    },
    /// Response-level usage replacement; `model` and `ts` are the row's
    /// final values after the merge.
    Usage {
        session_id: String,
        timeline_id: String,
        ts: Option<Ms>,
        model: Option<String>,
        tokens: Box<TokenSnapshot>,
        /// Read-time list-price cost of the replaced usage.
        cost: Option<rosetta_core::UsageCost>,
    },
    /// The session's projection was atomically replaced (generation swap),
    /// sealed from a cold pass, or renumbered underneath its delivered rows.
    /// Every delta delivered before this is void: the ring restarts with a
    /// new epoch and subscribers take a replacement snapshot.
    SessionSwept {
        session_id: String,
    },
    /// A committed task, Workflow, plan, or artifact fact changed. The marker carries
    /// no duplicated metadata; the WebSocket boundary resolves one fresh
    /// Rust-built `timeline_meta` snapshot for the watching session.
    TimelineMetaChanged {
        session_id: String,
    },
    /// Metadata-mirror diffs — emitted by [`MetaMirror::refresh`], never
    /// collected during apply.
    Session(Box<SessionRow>),
    Project(Box<ProjectRow>),
    Runtime(Box<RuntimeRow>),
}

impl Mutation {
    /// The session this mutation is scoped to, for ring/topic routing.
    /// Mirror diffs return `None` — they ride the meta topic.
    pub fn session_scope(&self) -> Option<&str> {
        match self {
            Self::Message(row) => Some(&row.session_id),
            Self::ToolUpdated { session_id, .. }
            | Self::Usage { session_id, .. }
            | Self::SessionSwept { session_id }
            | Self::TimelineMetaChanged { session_id } => Some(session_id),
            Self::Session(_) | Self::Project(_) | Self::Runtime(_) => None,
        }
    }

    /// Rough resident size for the ring byte cap. Deliberately cheap: fixed
    /// overhead plus the large owned strings, skipping JSON payloads.
    pub fn approx_bytes(&self) -> usize {
        const BASE: usize = 160;
        let strings = match self {
            Self::Message(row) => {
                let prose = match &row.body {
                    TimelineBody::User { text, .. } | TimelineBody::Agent { text, .. } => {
                        text.as_ref().map_or(0, |text| text.as_preview().len())
                    }
                    TimelineBody::Thinking { text } | TimelineBody::Summary { text, .. } => {
                        text.as_preview().len()
                    }
                    TimelineBody::Collaboration { event } => event
                        .body
                        .as_ref()
                        .map_or(0, |text| text.as_preview().len()),
                    TimelineBody::ToolUse { .. } | TimelineBody::Notice { .. } => 0,
                };
                prose
                    + match &row.body {
                        TimelineBody::ToolUse { tool } => {
                            tool.native.name.len()
                                + tool.failure.as_ref().map_or(0, |failure| {
                                    failure.summary.as_deref().map_or(0, str::len)
                                        + failure.diagnostic.as_deref().map_or(0, str::len)
                                })
                        }
                        _ => 0,
                    }
            }
            Self::ToolUpdated { tool, .. } => {
                tool.native.name.len()
                    + tool.failure.as_ref().map_or(0, |failure| {
                        failure.summary.as_deref().map_or(0, str::len)
                            + failure.diagnostic.as_deref().map_or(0, str::len)
                    })
            }
            Self::Session(row) => {
                row.title.as_deref().map_or(0, str::len)
                    + row.summary.as_deref().map_or(0, str::len)
            }
            Self::Project(row) => row.root_paths.iter().map(String::len).sum(),
            Self::Usage { .. }
            | Self::SessionSwept { .. }
            | Self::TimelineMetaChanged { .. }
            | Self::Runtime(_) => 0,
        };
        BASE + strings
    }
}

/// Ceiling on undrained mutations. Past it the log truncates and
/// `overflowed` tells the hub to resynchronize wholesale instead of letting
/// an undrained store grow without bound.
pub const EFFECTS_MUTATION_CAP: usize = 65_536;

/// Which sessions the row-level mutation log admits. `All` is the live
/// loop's arming; `Watched` is the cold paths' — only sessions a reader
/// holds open collect mutations, which is what makes row effects affordable
/// during a backfill that projects millions of unwatched rows. The scope
/// bounds session-scoped data only: mutations and session touches. Project
/// and runtime touches stay unscoped — at most a handful per batch, and the
/// mirror diff makes redundant reloads free.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub enum RowScope {
    #[default]
    All,
    Watched(BTreeSet<String>),
}

impl RowScope {
    pub fn admits(&self, session_id: &str) -> bool {
        match self {
            Self::All => true,
            Self::Watched(watched) => watched.contains(session_id),
        }
    }
}

/// Row-level effects of committed batches, collected by the store while
/// observation is enabled and drained by the daemon strictly after commit.
#[derive(Debug, Default, PartialEq)]
pub struct Effects {
    /// Session-scoped mutations in apply order.
    pub mutations: Vec<Mutation>,
    /// Sessions whose `session` row may have changed — the mirror reloads
    /// and diffs exactly these.
    pub touched_sessions: BTreeSet<String>,
    pub touched_projects: BTreeSet<String>,
    pub touched_runtimes: BTreeSet<String>,
    /// Sessions whose light work index must be rebuilt after this commit.
    /// Kept as ids so a batch emits at most one refresh regardless of how
    /// many task/workflow/plan/artifact facts it contained.
    pub timeline_meta_sessions: BTreeSet<String>,
    /// The mutation list hit [`EFFECTS_MUTATION_CAP`] and truncated;
    /// consumers must invalidate rings and rewarm the mirror.
    pub overflowed: bool,
    /// The admission filter this log was collected under. Enforced at the
    /// push sites, never on drain: filtering after collection would let one
    /// unwatched bulk pass trip the overflow cap for everyone.
    pub scope: RowScope,
}

impl Effects {
    /// A fresh, empty log collecting under the same scope — the per-batch
    /// transient [`Store::apply`] merges back on commit.
    pub(crate) fn scoped_like(&self) -> Effects {
        Effects {
            scope: self.scope.clone(),
            ..Effects::default()
        }
    }

    pub fn is_empty(&self) -> bool {
        self.mutations.is_empty()
            && self.touched_sessions.is_empty()
            && self.touched_projects.is_empty()
            && self.touched_runtimes.is_empty()
            && self.timeline_meta_sessions.is_empty()
            && !self.overflowed
    }

    pub(crate) fn push(&mut self, mutation: Mutation) {
        if let Some(session_id) = mutation.session_scope()
            && !self.scope.admits(session_id)
        {
            return;
        }
        if self.mutations.len() >= EFFECTS_MUTATION_CAP {
            self.overflowed = true;
        } else {
            self.mutations.push(mutation);
        }
    }

    /// Record that a session's identity row may have moved, if the scope
    /// admits it — unwatched sessions ride [`MetaEffects`] instead.
    pub(crate) fn touch_session(&mut self, session_id: &str) {
        if self.scope.admits(session_id) {
            note_id(&mut self.touched_sessions, session_id);
        }
    }

    pub(crate) fn touch_timeline_meta(&mut self, session_id: &str) {
        if self.scope.admits(session_id) {
            note_id(&mut self.timeline_meta_sessions, session_id);
        }
    }

    /// Fold a committed transient into the armed accumulator. The transient
    /// was collected under `self`'s scope ([`Effects::scoped_like`]), so its
    /// content needs no re-filtering and its scope copy is dropped.
    pub(crate) fn merge(&mut self, other: Effects) {
        self.touched_sessions.extend(other.touched_sessions);
        self.touched_projects.extend(other.touched_projects);
        self.touched_runtimes.extend(other.touched_runtimes);
        self.timeline_meta_sessions
            .extend(other.timeline_meta_sessions);
        self.overflowed |= other.overflowed;
        let room = EFFECTS_MUTATION_CAP.saturating_sub(self.mutations.len());
        if other.mutations.len() > room {
            self.overflowed = true;
        }
        self.mutations
            .extend(other.mutations.into_iter().take(room));
    }
}

/// Which mirror-backed identity row an op may change. One classification,
/// shared by both granularities so they can never disagree about what a
/// batch touched.
pub(crate) enum MetaTouch<'a> {
    Runtime(&'a str),
    Project(&'a str),
    Session(&'a str),
    /// The op projects rows the mirror does not hold.
    None,
}

pub(crate) fn meta_touch(op: &IngestOp) -> MetaTouch<'_> {
    match op {
        IngestOp::Runtime(row) => MetaTouch::Runtime(&row.id),
        IngestOp::Project(row) => MetaTouch::Project(&row.id),
        IngestOp::SessionMeta(row) => MetaTouch::Session(&row.id),
        IngestOp::SessionTitle(row) => MetaTouch::Session(&row.session_id),
        // Messages and late merge facts touch the session's activity time.
        IngestOp::Message(row) => MetaTouch::Session(&row.session_id),
        IngestOp::ToolUpdate { session_id, .. }
        | IngestOp::Usage { session_id, .. }
        | IngestOp::WorkflowRun { session_id, .. }
        | IngestOp::WorkflowJournal { session_id, .. } => MetaTouch::Session(session_id),
        IngestOp::SessionUsage(row) => MetaTouch::Session(&row.session_id),
        IngestOp::SessionUsageTotals(row) => MetaTouch::Session(&row.session_id),
        IngestOp::ProjectionAsset { .. }
        | IngestOp::FileEdit(_)
        | IngestOp::Plan(_)
        | IngestOp::Task(_)
        | IngestOp::GitCommit(_)
        | IngestOp::Drift(_) => MetaTouch::None,
    }
}

/// Metadata-level effects of committed batches: the identity keys of the
/// mirror-backed rows they may have changed. Ids only — no mutation log, no
/// row payloads, nothing to cap — which is what makes this half affordable
/// on the bulk and progressive cold paths where row [`Effects`] are
/// deliberately suppressed.
///
/// Removals need no tombstone set. A named row the projection no longer
/// holds reads back absent, and [`MetaMirror::refresh_meta`] drops it on
/// that evidence; a second set would only restate what the re-read proves.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct MetaEffects {
    pub runtimes: BTreeSet<String>,
    pub projects: BTreeSet<String>,
    pub sessions: BTreeSet<String>,
}

impl MetaEffects {
    pub fn is_empty(&self) -> bool {
        self.runtimes.is_empty() && self.projects.is_empty() && self.sessions.is_empty()
    }

    /// Record which mirror-backed row this op may change. Idempotent set
    /// inserts of an id; the payload never leaves the batch.
    pub(crate) fn note(&mut self, op: &IngestOp) {
        match meta_touch(op) {
            MetaTouch::Runtime(id) => self.note_runtime(id),
            MetaTouch::Project(id) => self.note_project(id),
            MetaTouch::Session(id) => self.note_session(id),
            MetaTouch::None => {}
        }
    }

    pub(crate) fn note_runtime(&mut self, id: &str) {
        note_id(&mut self.runtimes, id);
    }

    pub(crate) fn note_project(&mut self, id: &str) {
        note_id(&mut self.projects, id);
    }

    pub(crate) fn note_session(&mut self, id: &str) {
        note_id(&mut self.sessions, id);
    }

    pub(crate) fn merge(&mut self, other: MetaEffects) {
        self.runtimes.extend(other.runtimes);
        self.projects.extend(other.projects);
        self.sessions.extend(other.sessions);
    }
}

/// A repeat touch — one session across a whole batch of messages is the
/// common case — must not allocate a second copy of the id.
fn note_id(set: &mut BTreeSet<String>, id: &str) {
    if !set.contains(id) {
        set.insert(id.to_owned());
    }
}

/// A subscription position: ring identity plus the last mutation sequence
/// covered. Identity is never bound to delivery order — a dead epoch means
/// "replacement snapshot", never a protocol error. Both fields stay far
/// below 2^53, so the wire carries them as plain numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct Stamp {
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub epoch: u64,
    /// Sequence of the last mutation this position covers; 0 = none yet.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub seq: u64,
}

#[derive(Debug, PartialEq)]
pub enum Replay {
    /// Deliver these, then continue live.
    Mutations(Vec<Arc<Mutation>>),
    /// The position is unreachable — the epoch died or the ring dropped the
    /// gap. Take a replacement snapshot stamped with the current position.
    Expired(Stamp),
}

#[derive(Debug, Clone)]
pub struct HotConfig {
    /// Entries per session ring (~"last N mutations").
    pub ring_entries: usize,
    /// Byte cap per session ring; the oldest entries drop first.
    pub ring_bytes: usize,
    /// Reader-idle horizon after which an unwatched ring is dropped — the
    /// one legitimate TTL (resource-bound, never correctness).
    pub idle_evict_ms: Ms,
}

impl Default for HotConfig {
    fn default() -> Self {
        Self {
            ring_entries: 256,
            ring_bytes: 1024 * 1024,
            idle_evict_ms: 10 * 60 * 1000,
        }
    }
}

#[derive(Debug)]
struct RingEntry {
    seq: u64,
    bytes: usize,
    mutation: Arc<Mutation>,
}

#[derive(Debug)]
struct SessionRing {
    epoch: u64,
    /// Sequence of the last pushed mutation in this epoch; 0 = none yet.
    last_seq: u64,
    entries: VecDeque<RingEntry>,
    bytes: usize,
    /// Last reader interest (stamp/replay). Pushes do NOT refresh this — a
    /// busy session nobody watches must still evict.
    last_interest: Ms,
}

impl SessionRing {
    fn new(epoch: u64, now: Ms) -> Self {
        Self {
            epoch,
            last_seq: 0,
            entries: VecDeque::new(),
            bytes: 0,
            last_interest: now,
        }
    }

    fn stamp(&self) -> Stamp {
        Stamp {
            epoch: self.epoch,
            seq: self.last_seq,
        }
    }
}

/// Per-session tail rings, ahead-of-DB in the final writer topology. Rings
/// are lazy: they exist only after reader interest (stamp/replay/subscribe)
/// — an unwatched session's mutations skip the ring entirely, because a
/// later subscriber starts from a DB snapshot anyway ("rewarms lazily on
/// subscribe").
#[derive(Debug)]
pub struct TailRings {
    config: HotConfig,
    next_epoch: u64,
    rings: HashMap<String, SessionRing>,
}

impl TailRings {
    /// `epoch_base` must be fresh per process (the daemon passes wall-clock
    /// millis) so stamps issued before a restart can never validate against
    /// a post-restart ring.
    pub fn new(config: HotConfig, epoch_base: u64) -> Self {
        Self {
            config,
            next_epoch: epoch_base.max(1),
            rings: HashMap::new(),
        }
    }

    fn mint_epoch(&mut self) -> u64 {
        let epoch = self.next_epoch;
        self.next_epoch += 1;
        epoch
    }

    /// Current position for snapshot stamping; creates (rewarms) an empty
    /// ring on first interest.
    pub fn stamp(&mut self, session_id: &str, now: Ms) -> Stamp {
        if !self.rings.contains_key(session_id) {
            let epoch = self.mint_epoch();
            self.rings
                .insert(session_id.to_string(), SessionRing::new(epoch, now));
        }
        let ring = self.rings.get_mut(session_id).expect("ring just ensured");
        ring.last_interest = now;
        ring.stamp()
    }

    /// Push one mutation into the session's ring, if reader interest created
    /// one. Returns the assigned stamp when ringed.
    pub fn push(&mut self, session_id: &str, mutation: Arc<Mutation>) -> Option<Stamp> {
        let ring = self.rings.get_mut(session_id)?;
        ring.last_seq += 1;
        let bytes = mutation.approx_bytes();
        ring.entries.push_back(RingEntry {
            seq: ring.last_seq,
            bytes,
            mutation,
        });
        ring.bytes += bytes;
        while ring.entries.len() > self.config.ring_entries
            || (ring.bytes > self.config.ring_bytes && ring.entries.len() > 1)
        {
            let dropped = ring.entries.pop_front().expect("ring is non-empty");
            ring.bytes -= dropped.bytes;
        }
        Some(ring.stamp())
    }

    /// Void a session's ring after a generation swap: fresh epoch, cleared
    /// entries. Old stamps expire; subscribers take replacement snapshots.
    pub fn invalidate(&mut self, session_id: &str, now: Ms) -> Option<Stamp> {
        if !self.rings.contains_key(session_id) {
            return None;
        }
        let epoch = self.mint_epoch();
        let ring = self.rings.get_mut(session_id).expect("ring exists");
        *ring = SessionRing::new(epoch, now);
        Some(ring.stamp())
    }

    /// Drop every ring (wholesale resync — effects overflow).
    pub fn clear(&mut self) {
        self.rings.clear();
    }

    /// Mutations after `stamp`, or [`Replay::Expired`] with the current
    /// position when the stamp is unreachable.
    pub fn replay_after(&mut self, session_id: &str, stamp: Stamp, now: Ms) -> Replay {
        if !self.rings.contains_key(session_id) {
            return Replay::Expired(self.stamp(session_id, now));
        }
        let ring = self.rings.get_mut(session_id).expect("ring exists");
        ring.last_interest = now;
        if stamp.epoch != ring.epoch || stamp.seq > ring.last_seq {
            return Replay::Expired(ring.stamp());
        }
        match ring.entries.front() {
            // The ring dropped entries inside (stamp.seq, oldest): a gap.
            Some(front) if stamp.seq + 1 < front.seq => return Replay::Expired(ring.stamp()),
            None if stamp.seq < ring.last_seq => return Replay::Expired(ring.stamp()),
            _ => {}
        }
        Replay::Mutations(
            ring.entries
                .iter()
                .filter(|entry| entry.seq > stamp.seq)
                .map(|entry| entry.mutation.clone())
                .collect(),
        )
    }

    /// Drop rings without reader interest for `idle_evict_ms`, except
    /// sessions in `keep` (live subscribers wait passively — the hub passes
    /// their sessions). Returns the evicted count.
    pub fn evict_idle(&mut self, now: Ms, keep: &HashSet<String>) -> usize {
        let cutoff = now.saturating_sub(self.config.idle_evict_ms);
        let before = self.rings.len();
        self.rings
            .retain(|session_id, ring| keep.contains(session_id) || ring.last_interest >= cutoff);
        before - self.rings.len()
    }

    pub fn len(&self) -> usize {
        self.rings.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rings.is_empty()
    }
}

/// The fully-resident metadata surface (10k sessions ≈ a few MB — no LRU,
/// no eviction). Refreshed strictly after commit; row diffs become precise
/// events so list subscribers never see a no-op update.
///
/// Every resident row has been published. Each entry point therefore reads
/// its whole pass before absorbing any of it: absorbing while reading would
/// let a read that fails partway leave rows resident whose diffs the error
/// discarded, and because the mirror publishes only what CHANGED, no later
/// pass would ever mention them again — a permanent, silent disagreement
/// between the mirror and its subscribers. Failing before the first mutation
/// keeps the failed pass repeatable instead.
#[derive(Debug, Default)]
pub struct MetaMirror {
    sessions: HashMap<String, SessionRow>,
    projects: HashMap<String, ProjectRow>,
    runtimes: HashMap<String, RuntimeRow>,
}

impl MetaMirror {
    /// Load the whole surface, diffing against resident state — startup, and
    /// the effects-overflow path ("the hot tier rebuilds from the projection
    /// the way the projection rebuilds from the ledger").
    pub fn warm(&mut self, conn: &Connection) -> Result<Vec<Mutation>, StoreError> {
        let runtimes = queries::all_runtimes(conn)?;
        let projects = queries::all_projects(conn)?;
        let sessions = queries::all_sessions(conn)?;

        let mut events = Vec::new();
        for row in runtimes {
            self.absorb_runtime(row, &mut events);
        }
        for row in projects {
            self.absorb_project(row, &mut events);
        }
        for row in sessions {
            self.absorb_session(row, &mut events);
        }
        Ok(events)
    }

    /// Reload the rows a drained [`Effects`] touched and their activity
    /// ancestors; unchanged rows
    /// emit nothing. An overflowed drain falls back to [`Self::warm`]. A named
    /// row the projection does not hold leaves the resident copy alone, unlike
    /// [`Self::refresh_meta`], which reads that absence as a removal.
    pub fn refresh(
        &mut self,
        conn: &Connection,
        effects: &Effects,
    ) -> Result<Vec<Mutation>, StoreError> {
        if effects.overflowed {
            return self.warm(conn);
        }
        let runtimes = fetch_named(conn, &effects.touched_runtimes, queries::runtime)?;
        let projects = fetch_named(conn, &effects.touched_projects, queries::project)?;
        let session_ids = self.activity_dependents(conn, &effects.touched_sessions)?;
        let sessions = fetch_sessions(conn, &session_ids)?;

        let mut events = Vec::new();
        for (_, row) in runtimes {
            if let Some(row) = row {
                self.absorb_runtime(row, &mut events);
            }
        }
        for (_, row) in projects {
            if let Some(row) = row {
                self.absorb_project(row, &mut events);
            }
        }
        for (_, row) in sessions {
            if let Some(row) = row {
                self.absorb_session(row, &mut events);
            }
        }
        Ok(events)
    }

    /// Reload the identity rows a drained [`MetaEffects`] named and their
    /// old/current activity ancestors:
    /// upsert the ones the projection still holds, drop the ones it no
    /// longer does. Unchanged rows emit nothing, so the diffs are precisely
    /// the ones [`Self::warm`] would have produced for those rows, in the
    /// same order. A dropped row leaves silently — the metadata surface has
    /// no removal event, and the mirror ends up holding what a mirror warmed
    /// from scratch would hold.
    ///
    /// Read-then-absorb, per the type's invariant, which matters most here:
    /// a drain is spent once, so an error must leave the mirror exactly where
    /// a retry with the same [`MetaEffects`] lands the same events. The rows
    /// the read phase holds are a subset of a surface the mirror already
    /// keeps whole, so they cost nothing it was not already paying.
    pub fn refresh_meta(
        &mut self,
        conn: &Connection,
        effects: &MetaEffects,
    ) -> Result<Vec<Mutation>, StoreError> {
        let runtimes = fetch_named(conn, &effects.runtimes, queries::runtime)?;
        let projects = fetch_named(conn, &effects.projects, queries::project)?;
        let session_ids = self.activity_dependents(conn, &effects.sessions)?;
        let sessions = fetch_sessions(conn, &session_ids)?;

        let mut events = Vec::new();
        for (id, row) in runtimes {
            match row {
                Some(row) => self.absorb_runtime(row, &mut events),
                None => {
                    self.runtimes.remove(id);
                }
            }
        }
        for (id, row) in projects {
            match row {
                Some(row) => self.absorb_project(row, &mut events),
                None => {
                    self.projects.remove(id);
                }
            }
        }
        for (id, row) in sessions {
            match row {
                Some(row) => self.absorb_session(row, &mut events),
                None => {
                    self.sessions.remove(id);
                }
            }
        }
        Ok(events)
    }

    /// Child activity changes its ancestors too. Include the old ancestry when
    /// a session is reparented or removed, so the old root can move backwards.
    fn activity_dependents(
        &self,
        conn: &Connection,
        ids: &BTreeSet<String>,
    ) -> Result<BTreeSet<String>, StoreError> {
        let mut result = ids.clone();
        for id in ids {
            result.extend(crate::activity::ancestors(conn, id)?);
            let mut visited = BTreeSet::new();
            let mut current = Some(id.as_str());
            while let Some(id) = current {
                if !visited.insert(id) {
                    break;
                }
                result.insert(id.to_owned());
                current = self
                    .sessions
                    .get(id)
                    .and_then(|row| row.parent_session_id.as_deref());
            }
        }
        Ok(result)
    }

    fn absorb_session(&mut self, row: SessionRow, events: &mut Vec<Mutation>) {
        if self.sessions.get(&row.id) != Some(&row) {
            events.push(Mutation::Session(Box::new(row.clone())));
            self.sessions.insert(row.id.clone(), row);
        }
    }

    fn absorb_project(&mut self, row: ProjectRow, events: &mut Vec<Mutation>) {
        if self.projects.get(&row.id) != Some(&row) {
            events.push(Mutation::Project(Box::new(row.clone())));
            self.projects.insert(row.id.clone(), row);
        }
    }

    fn absorb_runtime(&mut self, row: RuntimeRow, events: &mut Vec<Mutation>) {
        if self.runtimes.get(&row.id) != Some(&row) {
            events.push(Mutation::Runtime(Box::new(row.clone())));
            self.runtimes.insert(row.id.clone(), row);
        }
    }

    pub fn session(&self, id: &str) -> Option<&SessionRow> {
        self.sessions.get(id)
    }

    pub fn sessions(&self) -> impl Iterator<Item = &SessionRow> {
        self.sessions.values()
    }

    pub fn projects(&self) -> impl Iterator<Item = &ProjectRow> {
        self.projects.values()
    }

    pub fn runtimes(&self) -> impl Iterator<Item = &RuntimeRow> {
        self.runtimes.values()
    }
}

/// Above this many named sessions a refresh reads them in one pass
/// ([`queries::named_sessions`]): a cold runtime's admission names every one
/// of its sessions, and a family walk per session dominated that refresh.
const BULK_SESSION_READ: usize = 64;

/// [`fetch_named`] for sessions: the same rows and answers, read in one pass
/// once there are many of them.
fn fetch_sessions<'a>(
    conn: &Connection,
    ids: &'a BTreeSet<String>,
) -> Result<Vec<(&'a str, Option<SessionRow>)>, StoreError> {
    if ids.len() > BULK_SESSION_READ {
        queries::named_sessions(conn, ids)
    } else {
        fetch_named(conn, ids, queries::session)
    }
}

/// The read phase both targeted refreshes share: fetch every named row,
/// keeping each id paired with what the projection answered, and stop at the
/// first read that fails — having mutated nothing. What an absence means is
/// the caller's to decide.
fn fetch_named<'a, T>(
    conn: &Connection,
    ids: &'a BTreeSet<String>,
    read: impl Fn(&Connection, &str) -> Result<Option<T>, StoreError>,
) -> Result<Vec<(&'a str, Option<T>)>, StoreError> {
    ids.iter()
        .map(|id| Ok((id.as_str(), read(conn, id)?)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn swept(session_id: &str) -> Arc<Mutation> {
        Arc::new(Mutation::SessionSwept {
            session_id: session_id.to_string(),
        })
    }

    fn small_config(entries: usize) -> HotConfig {
        HotConfig {
            ring_entries: entries,
            ring_bytes: usize::MAX,
            idle_evict_ms: 1_000,
        }
    }

    #[test]
    fn pushes_skip_sessions_without_reader_interest() {
        let mut rings = TailRings::new(HotConfig::default(), 1);
        assert_eq!(rings.push("s1", swept("s1")), None);
        assert!(rings.is_empty());

        let stamp = rings.stamp("s1", 0);
        assert_eq!(stamp.seq, 0);
        let pushed = rings.push("s1", swept("s1")).expect("ring exists now");
        assert_eq!(
            pushed,
            Stamp {
                epoch: stamp.epoch,
                seq: 1
            }
        );
    }

    #[test]
    fn replay_covers_the_gap_or_expires() {
        let mut rings = TailRings::new(small_config(2), 1);
        let start = rings.stamp("s1", 0);
        rings.push("s1", swept("s1"));
        rings.push("s1", swept("s1"));
        match rings.replay_after("s1", start, 1) {
            Replay::Mutations(items) => assert_eq!(items.len(), 2),
            other => panic!("expected mutations, got {other:?}"),
        }

        // A third push drops seq 1 from the two-entry ring: the gap expires.
        rings.push("s1", swept("s1"));
        let current = match rings.replay_after("s1", start, 2) {
            Replay::Expired(stamp) => stamp,
            other => panic!("expected expiry, got {other:?}"),
        };
        assert_eq!(current.seq, 3);
        // From the fresh position the replay is empty, not expired.
        assert_eq!(
            rings.replay_after("s1", current, 3),
            Replay::Mutations(vec![])
        );
    }

    #[test]
    fn byte_cap_drops_oldest_entries() {
        let config = HotConfig {
            ring_entries: usize::MAX,
            ring_bytes: 400,
            idle_evict_ms: 1_000,
        };
        let mut rings = TailRings::new(config, 1);
        let start = rings.stamp("s1", 0);
        for _ in 0..3 {
            // Each SessionSwept estimates ~160 bytes; three exceed 400.
            rings.push("s1", swept("s1"));
        }
        assert!(matches!(
            rings.replay_after("s1", start, 1),
            Replay::Expired(_)
        ));
    }

    #[test]
    fn invalidation_mints_a_new_epoch_and_voids_old_stamps() {
        let mut rings = TailRings::new(HotConfig::default(), 1);
        let old = rings.stamp("s1", 0);
        rings.push("s1", swept("s1"));
        let fresh = rings.invalidate("s1", 1).expect("ring existed");
        assert_ne!(fresh.epoch, old.epoch);
        assert_eq!(fresh.seq, 0);
        assert!(matches!(
            rings.replay_after("s1", old, 2),
            Replay::Expired(_)
        ));
        // A session that never had a ring has nothing to invalidate.
        assert_eq!(rings.invalidate("ghost", 1), None);
    }

    #[test]
    fn idle_eviction_spares_kept_sessions_and_rewarms_with_fresh_epochs() {
        let mut rings = TailRings::new(small_config(8), 1);
        let watched = rings.stamp("watched", 0);
        let idle = rings.stamp("idle", 0);
        let keep = HashSet::from(["watched".to_string()]);
        assert_eq!(rings.evict_idle(5_000, &keep), 1);

        // The kept ring still answers its stamp; the evicted one expired and
        // rewarmed with an epoch that can never repeat.
        assert_eq!(
            rings.replay_after("watched", watched, 5_001),
            Replay::Mutations(vec![])
        );
        let rewarmed = match rings.replay_after("idle", idle, 5_001) {
            Replay::Expired(stamp) => stamp,
            other => panic!("expected expiry, got {other:?}"),
        };
        assert_ne!(rewarmed.epoch, idle.epoch);
    }

    #[test]
    fn effects_cap_flags_overflow_instead_of_growing() {
        let mut effects = Effects::default();
        for _ in 0..EFFECTS_MUTATION_CAP {
            effects.push(Mutation::SessionSwept {
                session_id: "s1".to_string(),
            });
        }
        assert!(!effects.overflowed);
        effects.push(Mutation::SessionSwept {
            session_id: "s1".to_string(),
        });
        assert!(effects.overflowed);
        assert_eq!(effects.mutations.len(), EFFECTS_MUTATION_CAP);
    }
}
