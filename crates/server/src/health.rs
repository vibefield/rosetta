//! Daemon liveness shared between the synchronous ingest loop and the
//! transport: the loop pushes plain counters after each tick, `/api/status`
//! snapshots them with the hub's occupancy. Deliberately dumb — no clock of
//! its own (callers pass `now`), no dependency on pipeline types, so the
//! server never reaches below L3 for its health surface.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use rosetta_core::rows::Ms;
use rosetta_protocol::{
    BackfillState, DaemonStatus, IngestPhase, IngestProgress, ProjectBackfill, RuntimeBackfill,
    RuntimeIngestStatus,
};

use crate::hub::Hub;

/// Live cold-start numerators, read on demand. The serve loop installs a
/// source per phase (a closure over the pipeline's atomic counters), because
/// the writer thread is blocked inside a backfill call for minutes and can
/// only be READ from, never push.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IngestCounters {
    pub files_skimmed: u64,
    pub bytes_read: u64,
    pub records: u64,
    pub sessions_discovered: u64,
}

pub type IngestCounterSource = Arc<dyn Fn() -> IngestCounters + Send + Sync>;

/// How the serve loop names a runtime to the health surface. Entries are keyed
/// by the canonical runtime id (2026-09-12): a supervisor may enroll several
/// roots of one kind, so `kind` is presentation, not identity. `label` is the
/// enrollment file's name for an enrolled runtime and rides the status rows
/// verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeIdentity {
    pub id: String,
    pub kind: String,
    pub label: Option<String>,
}

impl RuntimeIdentity {
    pub fn new(id: impl Into<String>, kind: impl Into<String>, label: Option<String>) -> Self {
        Self {
            id: id.into(),
            kind: kind.into(),
            label,
        }
    }
}

struct BackfillEntry {
    runtime_id: String,
    kind: String,
    label: Option<String>,
    state: BackfillState,
    files_total: u64,
    bytes_total: u64,
    /// Frozen numerators (finished phases); the live source overlays the
    /// active phase's numbers on top of these at snapshot time.
    files_skimmed: u64,
    bytes_streamed: u64,
    sessions_total: u64,
    records: u64,
    live: Option<IngestCounterSource>,
}

struct IngestState {
    phase: IngestPhase,
    entries: Vec<BackfillEntry>,
    /// The ProgressBoard's latest per-project fold, pushed by the serve
    /// loop between passes. A view of RAM state — never authoritative.
    projects: Vec<ProjectBackfill>,
    project_source: Option<Arc<dyn Fn() -> Vec<ProjectBackfill> + Send + Sync>>,
}

/// One runtime's cumulative live-ingest counters, as the daemon reads them
/// off its runner after a tick.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RuntimeTick {
    pub ticks: u64,
    pub events: u64,
    pub polls: u64,
    pub rescans: u64,
    pub admissions: u64,
    /// Rows written by the live lane (inserts + replacements).
    pub live_rows: u64,
}

impl RuntimeTick {
    /// Data movement, as distinct from idle ticking/polling.
    fn activity(&self) -> (u64, u64, u64, u64) {
        (self.events, self.rescans, self.admissions, self.live_rows)
    }
}

struct RuntimeEntry {
    kind: String,
    label: Option<String>,
    latest: RuntimeTick,
    last_activity_ms: Option<Ms>,
}

pub struct DaemonHealth {
    started_at_ms: Ms,
    /// Keyed by runtime id.
    runtimes: Mutex<BTreeMap<String, RuntimeEntry>>,
    ingest: Mutex<IngestState>,
}

impl DaemonHealth {
    pub fn new(now_ms: Ms) -> Self {
        Self {
            started_at_ms: now_ms,
            runtimes: Mutex::new(BTreeMap::new()),
            ingest: Mutex::new(IngestState {
                phase: IngestPhase::Discovering,
                entries: Vec::new(),
                projects: Vec::new(),
                project_source: None,
            }),
        }
    }

    /// The census is on the board and nothing has been read — the user is
    /// choosing. The app renders the roll call off this phase alone.
    pub fn ingest_await_consent(&self) {
        let mut ingest = self.ingest.lock().expect("ingest state poisoned");
        ingest.phase = IngestPhase::AwaitingConsent;
    }

    /// Enroll one runtime with its census denominators, queued. Every later
    /// ingest call names the entry by `runtime.id`.
    pub fn ingest_register(&self, runtime: &RuntimeIdentity, files_total: u64, bytes_total: u64) {
        let mut ingest = self.ingest.lock().expect("ingest state poisoned");
        ingest.entries.push(BackfillEntry {
            runtime_id: runtime.id.clone(),
            kind: runtime.kind.clone(),
            label: runtime.label.clone(),
            state: BackfillState::Queued,
            files_total,
            bytes_total,
            files_skimmed: 0,
            bytes_streamed: 0,
            sessions_total: 0,
            records: 0,
            live: None,
        });
    }

    /// The skim begins: install the live counter source for its meter.
    pub fn ingest_skim(&self, runtime_id: &str, source: IngestCounterSource) {
        let mut ingest = self.ingest.lock().expect("ingest state poisoned");
        ingest.phase = IngestPhase::Backfilling;
        if let Some(entry) = entry_mut(&mut ingest, runtime_id) {
            entry.state = BackfillState::Skimming;
            entry.live = Some(source);
        }
    }

    /// The skim landed: freeze its metadata counts and return the runtime to
    /// the transcript queue. No complete source record has been read yet.
    pub fn ingest_skim_done(&self, runtime_id: &str, sessions_total: u64, files_skimmed: u64) {
        let mut ingest = self.ingest.lock().expect("ingest state poisoned");
        if let Some(entry) = entry_mut(&mut ingest, runtime_id) {
            entry.state = BackfillState::Queued;
            entry.sessions_total = sessions_total;
            entry.files_skimmed = files_skimmed;
            entry.live = None;
        }
    }

    /// Every enrolled runtime has completed its bounded skim. This is a real
    /// observable boundary even when the ordinary daemon immediately proceeds
    /// to streaming; metadata-audit mode deliberately remains here. A fresh
    /// cold start begins each runtime's capture as soon as its own metadata
    /// is public, so by the last skim others may be streaming already: their
    /// meters keep running and the phase stays `Backfilling`.
    pub fn ingest_metadata_ready(&self) {
        let mut ingest = self.ingest.lock().expect("ingest state poisoned");
        let skim_side = |entry: &BackfillEntry| {
            matches!(entry.state, BackfillState::Queued | BackfillState::Skimming)
        };
        if ingest.entries.iter().all(skim_side) {
            ingest.phase = IngestPhase::MetadataReady;
        }
        for entry in ingest.entries.iter_mut().filter(|entry| skim_side(entry)) {
            entry.live = None;
        }
    }

    /// Complete-record streaming begins for this runtime. The meter is
    /// cumulative across its interleaved per-file calls.
    pub fn ingest_stream(&self, runtime_id: &str, source: IngestCounterSource) {
        let mut ingest = self.ingest.lock().expect("ingest state poisoned");
        ingest.phase = IngestPhase::Backfilling;
        if let Some(entry) = entry_mut(&mut ingest, runtime_id) {
            entry.state = BackfillState::Streaming;
            entry.live = Some(source);
        }
    }

    /// Replace discovery's whole inventory with the streamable plan's bytes.
    /// Zero denotes an unknown payload total (document/database snapshots).
    pub fn ingest_stream_total(&self, runtime_id: &str, bytes_total: u64) {
        let mut ingest = self.ingest.lock().expect("ingest state poisoned");
        if let Some(entry) = entry_mut(&mut ingest, runtime_id) {
            entry.bytes_total = bytes_total;
        }
    }

    /// One runtime's backfill is done: freeze its final numerators.
    pub fn ingest_done(&self, runtime_id: &str, bytes_streamed: u64, records: u64) {
        let mut ingest = self.ingest.lock().expect("ingest state poisoned");
        if let Some(entry) = entry_mut(&mut ingest, runtime_id) {
            entry.state = BackfillState::Done;
            entry.bytes_streamed = bytes_streamed;
            entry.records = records;
            entry.live = None;
        }
    }

    /// Keep live numerators visible during the CPU/write stages after reading.
    pub fn ingest_runtime_stage(&self, runtime_id: &str, state: BackfillState) {
        let mut ingest = self.ingest.lock().expect("ingest state poisoned");
        if let Some(entry) = entry_mut(&mut ingest, runtime_id) {
            entry.state = state;
        }
    }

    pub fn ingest_phase(&self, phase: IngestPhase) {
        self.ingest.lock().expect("ingest state poisoned").phase = phase;
    }

    /// Fresh parallel workers update the board while the sole writer is busy.
    /// Status snapshots it on demand; no SQL or timer on an ingest worker.
    pub fn ingest_project_source(
        &self,
        source: Arc<dyn Fn() -> Vec<ProjectBackfill> + Send + Sync>,
    ) {
        self.ingest
            .lock()
            .expect("ingest state poisoned")
            .project_source = Some(source);
    }

    /// The watch loop owns ingest now. Any entry still mid-phase (a graceful
    /// interrupt) freezes where it stood.
    pub fn ingest_live(&self) {
        let mut ingest = self.ingest.lock().expect("ingest state poisoned");
        ingest.phase = IngestPhase::Live;
        if let Some(source) = ingest.project_source.take() {
            ingest.projects = source();
        }
        for entry in &mut ingest.entries {
            entry.live = None;
        }
    }

    /// Publish the board's latest per-project rows onto the status surface.
    pub fn ingest_projects(&self, rows: Vec<ProjectBackfill>) {
        let mut ingest = self.ingest.lock().expect("ingest state poisoned");
        ingest.projects = rows;
    }

    fn ingest_progress(&self) -> IngestProgress {
        let ingest = self.ingest.lock().expect("ingest state poisoned");
        IngestProgress {
            phase: ingest.phase,
            projects: ingest
                .project_source
                .as_ref()
                .map_or_else(|| ingest.projects.clone(), |source| source()),
            runtimes: ingest
                .entries
                .iter()
                .map(|entry| {
                    let live = entry.live.as_ref().map(|source| source());
                    let mut row = RuntimeBackfill {
                        kind: entry.kind.clone(),
                        runtime_id: entry.runtime_id.clone(),
                        label: entry.label.clone(),
                        state: entry.state,
                        files_total: entry.files_total,
                        bytes_total: entry.bytes_total,
                        files_skimmed: entry.files_skimmed,
                        bytes_streamed: entry.bytes_streamed,
                        sessions_total: entry.sessions_total,
                        records: entry.records,
                    };
                    if let Some(live) = live {
                        match entry.state {
                            BackfillState::Skimming => {
                                row.files_skimmed = live.files_skimmed;
                                row.sessions_total = live.sessions_discovered;
                            }
                            BackfillState::Streaming
                            | BackfillState::Preparing
                            | BackfillState::Applying => {
                                row.bytes_streamed = live.bytes_read;
                                row.records = live.records;
                            }
                            BackfillState::Queued | BackfillState::Done => {}
                        }
                    }
                    row
                })
                .collect(),
        }
    }

    /// Record one runtime's counters after a tick. Activity (not mere
    /// ticking or idle polling) advances `last_activity_ms`.
    pub fn update(&self, runtime: &RuntimeIdentity, tick: RuntimeTick, now_ms: Ms) {
        let mut runtimes = self.runtimes.lock().expect("health state poisoned");
        let entry = runtimes
            .entry(runtime.id.clone())
            .or_insert_with(|| RuntimeEntry {
                kind: runtime.kind.clone(),
                label: runtime.label.clone(),
                latest: RuntimeTick::default(),
                last_activity_ms: None,
            });
        if entry.latest.activity() != tick.activity() {
            entry.last_activity_ms = Some(now_ms);
        }
        entry.latest = tick;
    }

    pub fn snapshot(&self, hub: &Hub, now_ms: Ms) -> DaemonStatus {
        let runtimes = self.runtimes.lock().expect("health state poisoned");
        DaemonStatus {
            started_at: self.started_at_ms,
            uptime_ms: u64::try_from(now_ms.saturating_sub(self.started_at_ms)).unwrap_or(0),
            runtimes: runtimes
                .iter()
                .map(|(runtime_id, entry)| RuntimeIngestStatus {
                    kind: entry.kind.clone(),
                    runtime_id: runtime_id.clone(),
                    label: entry.label.clone(),
                    ticks: entry.latest.ticks,
                    events: entry.latest.events,
                    polls: entry.latest.polls,
                    rescans: entry.latest.rescans,
                    admissions: entry.latest.admissions,
                    live_rows: entry.latest.live_rows,
                    last_activity_ms: entry.last_activity_ms,
                })
                .collect(),
            hub: hub.status(),
            ingest: self.ingest_progress(),
        }
    }
}

fn entry_mut<'a>(ingest: &'a mut IngestState, runtime_id: &str) -> Option<&'a mut BackfillEntry> {
    ingest
        .entries
        .iter_mut()
        .find(|entry| entry.runtime_id == runtime_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub::HubConfig;

    fn identity(id: &str) -> RuntimeIdentity {
        RuntimeIdentity::new(id, "claude-code", None)
    }

    #[test]
    fn activity_moves_the_clock_but_idle_ticks_do_not() {
        let hub = Hub::new(HubConfig::default(), 1);
        let health = DaemonHealth::new(1_000);
        health.update(&identity("rt1"), RuntimeTick::default(), 1_000);

        // Idle ticks and polls are not activity.
        health.update(
            &identity("rt1"),
            RuntimeTick {
                ticks: 5,
                polls: 2,
                ..RuntimeTick::default()
            },
            2_000,
        );
        let status = health.snapshot(&hub, 3_000);
        assert_eq!(status.runtimes.len(), 1);
        assert_eq!(status.runtimes[0].last_activity_ms, None);
        assert_eq!(status.runtimes[0].ticks, 5);
        assert_eq!(status.uptime_ms, 2_000);

        // A live row is.
        health.update(
            &identity("rt1"),
            RuntimeTick {
                ticks: 6,
                polls: 2,
                events: 1,
                live_rows: 1,
                ..RuntimeTick::default()
            },
            4_000,
        );
        let status = health.snapshot(&hub, 5_000);
        assert_eq!(status.runtimes[0].last_activity_ms, Some(4_000));
        assert_eq!(status.runtimes[0].live_rows, 1);
    }

    #[test]
    fn ingest_meters_read_live_counters_then_freeze() {
        use std::sync::atomic::{AtomicU64, Ordering};

        let hub = Hub::new(HubConfig::default(), 1);
        let health = DaemonHealth::new(1_000);
        assert_eq!(
            health.snapshot(&hub, 1_000).ingest.phase,
            IngestPhase::Discovering
        );

        health.ingest_register(&identity("rt1"), 100, 1_000_000);
        let counter = Arc::new(AtomicU64::new(0));

        // Skimming: the files meter reads through the live source.
        let skim_counter = counter.clone();
        health.ingest_skim(
            "rt1",
            Arc::new(move || IngestCounters {
                files_skimmed: skim_counter.load(Ordering::Relaxed),
                ..IngestCounters::default()
            }),
        );
        counter.store(40, Ordering::Relaxed);
        let ingest = health.snapshot(&hub, 2_000).ingest;
        assert_eq!(ingest.phase, IngestPhase::Backfilling);
        assert_eq!(ingest.runtimes[0].state, BackfillState::Skimming);
        assert_eq!(ingest.runtimes[0].files_skimmed, 40);
        assert_eq!(ingest.runtimes[0].files_total, 100);

        // Streaming: skim numbers freeze; the byte meter goes live.
        let stream_counter = counter.clone();
        health.ingest_skim_done("rt1", 250, 100);
        let ingest = health.snapshot(&hub, 2_500).ingest;
        assert_eq!(ingest.runtimes[0].state, BackfillState::Queued);
        assert_eq!(ingest.runtimes[0].files_skimmed, 100);
        assert_eq!(ingest.runtimes[0].sessions_total, 250);

        health.ingest_metadata_ready();
        let ingest = health.snapshot(&hub, 2_600).ingest;
        assert_eq!(ingest.phase, IngestPhase::MetadataReady);
        assert_eq!(ingest.runtimes[0].bytes_streamed, 0);
        assert_eq!(ingest.runtimes[0].records, 0);

        health.ingest_stream(
            "rt1",
            Arc::new(move || IngestCounters {
                bytes_read: stream_counter.load(Ordering::Relaxed),
                records: 7,
                ..IngestCounters::default()
            }),
        );
        counter.store(600_000, Ordering::Relaxed);
        let ingest = health.snapshot(&hub, 3_000).ingest;
        assert_eq!(ingest.phase, IngestPhase::Backfilling);
        assert_eq!(ingest.runtimes[0].state, BackfillState::Streaming);
        assert_eq!(ingest.runtimes[0].runtime_id, "rt1");
        assert_eq!(ingest.runtimes[0].kind, "claude-code");
        assert_eq!(ingest.runtimes[0].files_skimmed, 100);
        assert_eq!(ingest.runtimes[0].sessions_total, 250);
        assert_eq!(ingest.runtimes[0].bytes_streamed, 600_000);
        assert_eq!(ingest.runtimes[0].records, 7);

        // Done freezes finals; live flips the phase and drops every source.
        health.ingest_done("rt1", 1_000_000, 5_000);
        health.ingest_live();
        counter.store(9, Ordering::Relaxed);
        let ingest = health.snapshot(&hub, 4_000).ingest;
        assert_eq!(ingest.phase, IngestPhase::Live);
        assert_eq!(ingest.runtimes[0].state, BackfillState::Done);
        assert_eq!(ingest.runtimes[0].bytes_streamed, 1_000_000);
        assert_eq!(ingest.runtimes[0].records, 5_000);
    }

    #[test]
    fn metadata_ready_leaves_runtimes_already_streaming_alone() {
        use std::sync::atomic::{AtomicU64, Ordering};

        let hub = Hub::new(HubConfig::default(), 1);
        let health = DaemonHealth::new(1_000);
        health.ingest_register(&identity("early"), 10, 1_000);
        health.ingest_register(&identity("late"), 10, 1_000);
        let streamed = Arc::new(AtomicU64::new(0));

        // The first runtime's metadata is public and its capture has begun
        // while the second is still skimming.
        health.ingest_skim("early", Arc::new(IngestCounters::default));
        health.ingest_skim_done("early", 3, 10);
        let meter = streamed.clone();
        health.ingest_stream(
            "early",
            Arc::new(move || IngestCounters {
                bytes_read: meter.load(Ordering::Relaxed),
                ..IngestCounters::default()
            }),
        );
        health.ingest_skim("late", Arc::new(IngestCounters::default));
        health.ingest_skim_done("late", 4, 10);
        health.ingest_metadata_ready();

        streamed.store(512, Ordering::Relaxed);
        let ingest = health.snapshot(&hub, 2_000).ingest;
        assert_eq!(ingest.phase, IngestPhase::Backfilling);
        assert_eq!(ingest.runtimes[0].state, BackfillState::Streaming);
        assert_eq!(
            ingest.runtimes[0].bytes_streamed, 512,
            "the meter still reads"
        );
        assert_eq!(ingest.runtimes[1].state, BackfillState::Queued);
        assert_eq!(ingest.runtimes[1].sessions_total, 4);
    }

    #[test]
    fn the_boards_project_rows_ride_the_status_surface() {
        let hub = Hub::new(HubConfig::default(), 1);
        let health = DaemonHealth::new(1_000);
        assert!(
            health.snapshot(&hub, 1_500).ingest.projects.is_empty(),
            "no rows before the plan settles"
        );

        health.ingest_projects(vec![ProjectBackfill {
            project_id: Some("p1".into()),
            sessions_total: 4,
            sessions_done: 1,
            bytes_total: 400,
            bytes_streamed: 120,
        }]);
        let ingest = health.snapshot(&hub, 2_000).ingest;
        assert_eq!(ingest.projects.len(), 1);
        assert_eq!(ingest.projects[0].project_id.as_deref(), Some("p1"));
        assert_eq!(ingest.projects[0].sessions_done, 1);

        // Each push replaces the fold wholesale — the board owns the math.
        health.ingest_projects(Vec::new());
        assert!(health.snapshot(&hub, 3_000).ingest.projects.is_empty());
    }

    #[test]
    fn fresh_worker_observations_survive_preparation_and_freeze_only_when_public() {
        use std::sync::atomic::{AtomicU64, Ordering};
        let health = DaemonHealth::new(1_000);
        let hub = Hub::new(HubConfig::default(), 1);
        let counter = Arc::new(AtomicU64::new(4));
        let observed = counter.clone();
        health.ingest_register(
            &RuntimeIdentity::new("agent", "claude-code", Some("Kim".into())),
            100,
            1_000,
        );
        health.ingest_skim(
            "agent",
            Arc::new(move || IngestCounters {
                files_skimmed: observed.load(Ordering::Relaxed),
                sessions_discovered: observed.load(Ordering::Relaxed),
                ..Default::default()
            }),
        );
        assert_eq!(
            health.snapshot(&hub, 1_200).ingest.runtimes[0].sessions_total,
            4
        );
        counter.store(40, Ordering::Relaxed);
        assert_eq!(
            health.snapshot(&hub, 1_300).ingest.runtimes[0].sessions_total,
            40
        );

        let board = Arc::new(crate::ProgressBoard::new());
        board.seed_session(Some("project"), "session", 1_000);
        let observed = board.clone();
        health.ingest_project_source(Arc::new(move || observed.snapshot()));
        health.ingest_skim_done("agent", 40, 100);
        let observed = counter.clone();
        health.ingest_stream(
            "agent",
            Arc::new(move || IngestCounters {
                bytes_read: observed.load(Ordering::Relaxed),
                records: 8,
                ..Default::default()
            }),
        );
        board.advance(Some("project"), 400);
        counter.store(400, Ordering::Relaxed);
        health.ingest_runtime_stage("agent", BackfillState::Preparing);
        let status = health.snapshot(&hub, 2_000).ingest;
        assert_eq!(status.runtimes[0].label.as_deref(), Some("Kim"));
        assert_eq!(status.runtimes[0].bytes_streamed, 400);
        assert_eq!(status.projects[0].bytes_streamed, 400);
        assert_eq!(
            status.projects[0].sessions_done, 0,
            "processed is not public"
        );
        health.ingest_runtime_stage("agent", BackfillState::Applying);
        assert_eq!(health.snapshot(&hub, 2_100).ingest.runtimes[0].records, 8);
        board.session_done(Some("project"), "session");
        health.ingest_done("agent", 400, 8);
        health.ingest_phase(IngestPhase::Publishing);
        assert_eq!(
            health.snapshot(&hub, 2_200).ingest.phase,
            IngestPhase::Publishing
        );
        health.ingest_live();
        board.advance(Some("project"), 800);
        assert_eq!(
            health.snapshot(&hub, 2_500).ingest.projects[0].bytes_streamed,
            400
        );
    }
}
