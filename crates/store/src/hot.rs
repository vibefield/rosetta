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
//! [`Effects`] is the seam between the store's write path and this tier:
//! while observation is enabled, every committed batch appends the row
//! mutations it projected. The daemon drains and feeds them to the hub.
//! Everything here is rebuilt from the projection on restart, the way the
//! projection rebuilds from the ledger — nothing is durable, and no clock is
//! read internally (callers pass `now` so tests stay deterministic).

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::Arc;

use rusqlite::Connection;

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
        tokens: TokenSnapshot,
    },
    /// The session's projection was atomically replaced (generation swap).
    /// Every delta delivered before this is void: the ring restarts with a
    /// new epoch and subscribers take a replacement snapshot.
    SessionSwept {
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
            | Self::SessionSwept { session_id } => Some(session_id),
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
            Self::Usage { .. } | Self::SessionSwept { .. } | Self::Runtime(_) => 0,
        };
        BASE + strings
    }
}

/// Ceiling on undrained mutations. Past it the log truncates and
/// `overflowed` tells the hub to resynchronize wholesale instead of letting
/// an undrained store grow without bound.
pub const EFFECTS_MUTATION_CAP: usize = 65_536;

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
    /// The mutation list hit [`EFFECTS_MUTATION_CAP`] and truncated;
    /// consumers must invalidate rings and rewarm the mirror.
    pub overflowed: bool,
}

impl Effects {
    pub fn is_empty(&self) -> bool {
        self.mutations.is_empty()
            && self.touched_sessions.is_empty()
            && self.touched_projects.is_empty()
            && self.touched_runtimes.is_empty()
            && !self.overflowed
    }

    pub(crate) fn push(&mut self, mutation: Mutation) {
        if self.mutations.len() >= EFFECTS_MUTATION_CAP {
            self.overflowed = true;
        } else {
            self.mutations.push(mutation);
        }
    }

    pub(crate) fn merge(&mut self, other: Effects) {
        self.touched_sessions.extend(other.touched_sessions);
        self.touched_projects.extend(other.touched_projects);
        self.touched_runtimes.extend(other.touched_runtimes);
        self.overflowed |= other.overflowed;
        let room = EFFECTS_MUTATION_CAP.saturating_sub(self.mutations.len());
        if other.mutations.len() > room {
            self.overflowed = true;
        }
        self.mutations
            .extend(other.mutations.into_iter().take(room));
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
        let mut events = Vec::new();
        for row in queries::all_runtimes(conn)? {
            self.absorb_runtime(row, &mut events);
        }
        for row in queries::all_projects(conn)? {
            self.absorb_project(row, &mut events);
        }
        for row in queries::all_sessions(conn)? {
            self.absorb_session(row, &mut events);
        }
        Ok(events)
    }

    /// Reload exactly the rows a drained [`Effects`] touched; unchanged rows
    /// emit nothing. An overflowed drain falls back to [`Self::warm`].
    pub fn refresh(
        &mut self,
        conn: &Connection,
        effects: &Effects,
    ) -> Result<Vec<Mutation>, StoreError> {
        if effects.overflowed {
            return self.warm(conn);
        }
        let mut events = Vec::new();
        for id in &effects.touched_runtimes {
            if let Some(row) = queries::runtime(conn, id)? {
                self.absorb_runtime(row, &mut events);
            }
        }
        for id in &effects.touched_projects {
            if let Some(row) = queries::project(conn, id)? {
                self.absorb_project(row, &mut events);
            }
        }
        for id in &effects.touched_sessions {
            if let Some(row) = queries::session(conn, id)? {
                self.absorb_session(row, &mut events);
            }
        }
        Ok(events)
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
