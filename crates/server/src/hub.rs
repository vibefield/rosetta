//! server::hub — the pub/sub fan-out over the store's hot tier
//! (docs/ingest-engine.html, "The hot tier"). Topics are cache keys: one per
//! watched session (ring-backed) plus the metadata surface (mirror-backed).
//!
//! Subscription protocol (spaghetti's observer semantics, ported):
//! subscribing returns a [`Stamp`]; the caller reads its DB snapshot AFTER
//! subscribing (the projection is always at or ahead of the stamp), replays
//! `> stamp.seq` via [`Hub::replay_session`] on reconnect, then consumes
//! live items. Delivery is at-least-once and idempotent for clients keyed by
//! `timeline_id`, so a snapshot/replay overlap is harmless. Ring overflow, a
//! lagging consumer, or a generation swap raises a RESYNC BARRIER: the
//! subscriber sees [`HubItem::Resync`] exactly once, delivery stops, and
//! [`Hub::resync`] re-stamps for a replacement snapshot. Identity is never
//! bound to delivery sequence.
//!
//! The hub is synchronous and lock-based — one local daemon, in-process,
//! never an external cache server. The async axum layer arrives on top of
//! it; nothing here blocks except subscribers waiting for items.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use rosetta_core::rows::{Ms, ProjectRow, RuntimeRow, SessionRow};
use rosetta_store::StoreError;
use rosetta_store::hot::{
    Effects, HotConfig, MetaEffects, MetaMirror, Mutation, Replay, Stamp, TailRings,
};
use rusqlite::Connection;

#[derive(Debug, Clone)]
pub struct HubConfig {
    pub hot: HotConfig,
    /// Undelivered items a subscriber may queue before its resync barrier
    /// trips (the "lagging subscriber" rule).
    pub subscriber_queue: usize,
}

impl Default for HubConfig {
    fn default() -> Self {
        Self {
            hot: HotConfig::default(),
            subscriber_queue: 1024,
        }
    }
}

/// One delivery to a subscriber.
#[derive(Debug, Clone, PartialEq)]
pub enum HubItem {
    Mutation {
        /// Ring position after this mutation — the client's resume point.
        /// `None` on the meta topic, which has no ring (the mirror is the
        /// state; resync re-reads it wholesale).
        stamp: Option<Stamp>,
        mutation: Arc<Mutation>,
    },
    /// Barrier: delivery has stopped for this subscriber. Call
    /// [`Hub::resync`] and take a replacement snapshot.
    Resync,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Topic {
    Session(String),
    Meta,
}

#[derive(Debug)]
struct SubShared {
    queue: Mutex<VecDeque<HubItem>>,
    ready: Condvar,
    /// The barrier: while set, publishes skip this subscriber.
    needs_resync: AtomicBool,
    closed: AtomicBool,
}

/// A subscriber handle. Dropping it unsubscribes (the hub prunes lazily).
#[derive(Debug)]
pub struct Subscription {
    id: u64,
    shared: Arc<SubShared>,
}

impl Subscription {
    /// Block for the next item, up to `timeout`.
    pub fn recv_timeout(&self, timeout: Duration) -> Option<HubItem> {
        let deadline = Instant::now() + timeout;
        let mut queue = self.shared.queue.lock().expect("hub queue poisoned");
        loop {
            if let Some(item) = queue.pop_front() {
                return Some(item);
            }
            let now = Instant::now();
            if now >= deadline {
                return None;
            }
            let (guard, _) = self
                .shared
                .ready
                .wait_timeout(queue, deadline - now)
                .expect("hub queue poisoned");
            queue = guard;
        }
    }

    pub fn try_recv(&self) -> Option<HubItem> {
        self.shared
            .queue
            .lock()
            .expect("hub queue poisoned")
            .pop_front()
    }

    pub fn pending(&self) -> usize {
        self.shared.queue.lock().expect("hub queue poisoned").len()
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        self.shared.closed.store(true, Ordering::SeqCst);
    }
}

struct SubEntry {
    topic: Topic,
    shared: Arc<SubShared>,
}

struct HubState {
    rings: TailRings,
    mirror: MetaMirror,
    subs: HashMap<u64, SubEntry>,
    next_sub_id: u64,
    /// Cumulative absorb counters since construction, for /api/status.
    totals: rosetta_protocol::HubStatus,
}

/// What one [`Hub::absorb`] call did — the live path's instrumentation.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct AbsorbReport {
    /// Session-scoped mutations fanned out (and ringed where rings exist).
    pub session_mutations: u64,
    /// Precise metadata diffs delivered to meta subscribers.
    pub meta_events: u64,
    /// Resync barriers raised this pass (lag, sweeps, or overflow).
    pub resyncs: u64,
    pub rings_evicted: u64,
    /// The drained effects overflowed: rings were cleared wholesale and the
    /// mirror rewarmed from the projection.
    pub overflow_rewarm: bool,
}

/// The fan-out hub. `Sync` by design: the daemon absorbs from its writer
/// loop while server threads subscribe and read.
pub struct Hub {
    config: HubConfig,
    state: Mutex<HubState>,
}

impl Hub {
    /// `epoch_base` must be fresh per process (pass wall-clock millis) so
    /// stamps from before a restart can never validate — restart mints new
    /// epochs by construction.
    pub fn new(config: HubConfig, epoch_base: u64) -> Self {
        let rings = TailRings::new(config.hot.clone(), epoch_base);
        Self {
            config,
            state: Mutex::new(HubState {
                rings,
                mirror: MetaMirror::default(),
                subs: HashMap::new(),
                next_sub_id: 1,
                totals: rosetta_protocol::HubStatus::default(),
            }),
        }
    }

    /// Load the metadata mirror from the projection — startup, before the
    /// first absorb. Returns the number of resident rows loaded.
    pub fn warm(&self, conn: &Connection) -> Result<usize, StoreError> {
        let mut state = self.state.lock().expect("hub state poisoned");
        let events = state.mirror.warm(conn)?;
        Ok(events.len())
    }

    /// Mid-backfill mirror refresh: re-read the whole surface and fan the
    /// changes to meta subscribers. The cold path's substitute for effects,
    /// which only flow once the live loop owns the store — and precise,
    /// because the mirror's warm diffs against resident state and emits
    /// only rows that actually changed. Returns the diffs delivered.
    pub fn rewarm(&self, conn: &Connection) -> Result<usize, StoreError> {
        let mut state = self.state.lock().expect("hub state poisoned");
        let state = &mut *state;
        state
            .subs
            .retain(|_, entry| !entry.shared.closed.load(Ordering::SeqCst));
        let events = state.mirror.warm(conn)?;
        let (delivered, resyncs) = publish_meta(state, self.config.subscriber_queue, events);
        state.totals.meta_events += delivered as u64;
        state.totals.resyncs += resyncs;
        Ok(delivered)
    }

    /// Absorb one drained [`MetaEffects`] batch, strictly after its
    /// transactions committed: re-read only the identity rows it names,
    /// upsert-or-drop them in the mirror, and fan the diffs to meta
    /// subscribers. The events are exactly the ones [`Hub::rewarm`] would
    /// have emitted for those rows — precise, because the mirror diffs
    /// against resident state — at the cost of the named rows instead of the
    /// whole surface. Returns the diffs delivered.
    ///
    /// Same calling discipline as [`Hub::absorb`]: the writer that committed
    /// the batch calls this, so the mirror advances in commit order.
    pub fn absorb_meta(
        &self,
        conn: &Connection,
        effects: MetaEffects,
    ) -> Result<usize, StoreError> {
        let mut state = self.state.lock().expect("hub state poisoned");
        let state = &mut *state;
        state
            .subs
            .retain(|_, entry| !entry.shared.closed.load(Ordering::SeqCst));
        let events = state.mirror.refresh_meta(conn, &effects)?;
        let (delivered, resyncs) = publish_meta(state, self.config.subscriber_queue, events);
        state.totals.meta_events += delivered as u64;
        state.totals.resyncs += resyncs;
        Ok(delivered)
    }

    /// Subscribe to one session's timeline. Take the DB snapshot AFTER this
    /// returns and stamp it with the returned [`Stamp`]; mutations delivered
    /// afterwards continue from there (overlap is idempotent).
    pub fn subscribe_session(&self, session_id: &str, now: Ms) -> (Subscription, Stamp) {
        let mut state = self.state.lock().expect("hub state poisoned");
        let stamp = state.rings.stamp(session_id, now);
        let sub = register(&mut state, Topic::Session(session_id.to_string()));
        (sub, stamp)
    }

    /// Subscribe to the metadata surface. Read the current lists via
    /// [`Hub::sessions`]/[`Hub::projects`]/[`Hub::runtimes`] after
    /// subscribing; diffs stream from there.
    pub fn subscribe_meta(&self) -> Subscription {
        let mut state = self.state.lock().expect("hub state poisoned");
        register(&mut state, Topic::Meta)
    }

    /// Clear a subscriber's resync barrier. Session topics get a fresh stamp
    /// for the replacement snapshot; the meta topic re-reads the mirror.
    pub fn resync(&self, sub: &Subscription, now: Ms) -> Option<Stamp> {
        let mut state = self.state.lock().expect("hub state poisoned");
        let entry_topic = state.subs.get(&sub.id).map(|entry| entry.topic.clone())?;
        sub.shared.queue.lock().expect("hub queue poisoned").clear();
        sub.shared.needs_resync.store(false, Ordering::SeqCst);
        match entry_topic {
            Topic::Session(session_id) => Some(state.rings.stamp(&session_id, now)),
            Topic::Meta => None,
        }
    }

    /// Ring replay for a reconnecting client holding a previous stamp.
    pub fn replay_session(&self, session_id: &str, stamp: Stamp, now: Ms) -> Replay {
        let mut state = self.state.lock().expect("hub state poisoned");
        state.rings.replay_after(session_id, stamp, now)
    }

    /// Mirror reads — list/browse never touches the DB.
    pub fn sessions(&self) -> Vec<SessionRow> {
        let state = self.state.lock().expect("hub state poisoned");
        state.mirror.sessions().cloned().collect()
    }

    pub fn session(&self, session_id: &str) -> Option<SessionRow> {
        let state = self.state.lock().expect("hub state poisoned");
        state.mirror.session(session_id).cloned()
    }

    pub fn projects(&self) -> Vec<ProjectRow> {
        let state = self.state.lock().expect("hub state poisoned");
        state.mirror.projects().cloned().collect()
    }

    pub fn runtimes(&self) -> Vec<RuntimeRow> {
        let state = self.state.lock().expect("hub state poisoned");
        state.mirror.runtimes().cloned().collect()
    }

    /// Absorb one drained [`Effects`] batch, strictly after its transactions
    /// committed: refresh the mirror (row diffs → precise meta events), push
    /// session-scoped mutations into rings, fan out, evict idle rings.
    pub fn absorb(
        &self,
        conn: &Connection,
        effects: Effects,
        now: Ms,
    ) -> Result<AbsorbReport, StoreError> {
        let mut report = AbsorbReport::default();
        let mut state = self.state.lock().expect("hub state poisoned");
        let state = &mut *state;
        state
            .subs
            .retain(|_, entry| !entry.shared.closed.load(Ordering::SeqCst));

        if effects.overflowed {
            // Wholesale resynchronization: every ring identity dies and
            // every subscriber replaces its snapshot. The mirror rewarm
            // below still yields precise diffs against resident state.
            report.overflow_rewarm = true;
            state.rings.clear();
            for entry in state.subs.values() {
                report.resyncs += u64::from(raise_barrier(&entry.shared));
            }
        }

        let meta_events = state.mirror.refresh(conn, &effects)?;

        for mutation in effects.mutations {
            if let Mutation::SessionSwept { session_id } = &mutation {
                state.rings.invalidate(session_id, now);
                for entry in state.subs.values() {
                    if let Topic::Session(topic_session) = &entry.topic
                        && topic_session == session_id
                    {
                        report.resyncs += u64::from(raise_barrier(&entry.shared));
                    }
                }
                continue;
            }
            let Some(session_id) = mutation.session_scope().map(str::to_owned) else {
                continue;
            };
            let item = Arc::new(mutation);
            let stamp = state.rings.push(&session_id, item.clone());
            report.session_mutations += 1;
            for entry in state.subs.values() {
                if let Topic::Session(topic_session) = &entry.topic
                    && topic_session == &session_id
                {
                    report.resyncs += u64::from(deliver(
                        &entry.shared,
                        self.config.subscriber_queue,
                        HubItem::Mutation {
                            stamp,
                            mutation: item.clone(),
                        },
                    ));
                }
            }
        }

        let (delivered, resyncs) = publish_meta(state, self.config.subscriber_queue, meta_events);
        report.meta_events += delivered as u64;
        report.resyncs += resyncs;

        // Idle eviction, sparing sessions with live subscribers — their
        // readers wait passively and must not lose the ring under them.
        let keep: HashSet<String> = state
            .subs
            .values()
            .filter_map(|entry| match &entry.topic {
                Topic::Session(session_id) => Some(session_id.clone()),
                Topic::Meta => None,
            })
            .collect();
        report.rings_evicted = state.rings.evict_idle(now, &keep) as u64;

        state.totals.session_mutations += report.session_mutations;
        state.totals.meta_events += report.meta_events;
        state.totals.resyncs += report.resyncs;
        state.totals.rings_evicted += report.rings_evicted;
        state.totals.overflow_rewarms += u64::from(report.overflow_rewarm);
        Ok(report)
    }

    /// Cumulative fan-out counters plus current subscriber/ring occupancy.
    pub fn status(&self) -> rosetta_protocol::HubStatus {
        let mut state = self.state.lock().expect("hub state poisoned");
        state
            .subs
            .retain(|_, entry| !entry.shared.closed.load(Ordering::SeqCst));
        let mut status = state.totals.clone();
        status.subscribers = state.subs.len() as u64;
        status.rings = state.rings.len() as u64;
        status
    }
}

fn register(state: &mut HubState, topic: Topic) -> Subscription {
    let id = state.next_sub_id;
    state.next_sub_id += 1;
    let shared = Arc::new(SubShared {
        queue: Mutex::new(VecDeque::new()),
        ready: Condvar::new(),
        needs_resync: AtomicBool::new(false),
        closed: AtomicBool::new(false),
    });
    state.subs.insert(
        id,
        SubEntry {
            topic,
            shared: shared.clone(),
        },
    );
    Subscription { id, shared }
}

/// Fan mirror diffs to every meta subscriber — the one delivery path for the
/// metadata topic, whether the diffs came from a whole-surface warm or from
/// the named rows an effects drain touched. Returns (delivered, resyncs).
fn publish_meta(state: &HubState, capacity: usize, events: Vec<Mutation>) -> (usize, u64) {
    let delivered = events.len();
    let mut resyncs = 0_u64;
    for event in events {
        let item = Arc::new(event);
        for entry in state.subs.values() {
            if entry.topic == Topic::Meta {
                resyncs += u64::from(deliver(
                    &entry.shared,
                    capacity,
                    HubItem::Mutation {
                        stamp: None,
                        mutation: item.clone(),
                    },
                ));
            }
        }
    }
    (delivered, resyncs)
}

/// Queue one item unless the subscriber is barred; a full queue trips the
/// barrier instead. Returns whether a barrier was (newly) raised.
fn deliver(shared: &SubShared, capacity: usize, item: HubItem) -> bool {
    if shared.closed.load(Ordering::SeqCst) || shared.needs_resync.load(Ordering::SeqCst) {
        return false;
    }
    let mut queue = shared.queue.lock().expect("hub queue poisoned");
    if queue.len() >= capacity {
        drop(queue);
        return raise_barrier(shared);
    }
    queue.push_back(item);
    shared.ready.notify_all();
    false
}

/// Stop delivery and leave exactly one [`HubItem::Resync`] in the queue.
fn raise_barrier(shared: &SubShared) -> bool {
    if shared.closed.load(Ordering::SeqCst) || shared.needs_resync.swap(true, Ordering::SeqCst) {
        return false;
    }
    let mut queue = shared.queue.lock().expect("hub queue poisoned");
    queue.clear();
    queue.push_back(HubItem::Resync);
    shared.ready.notify_all();
    true
}
