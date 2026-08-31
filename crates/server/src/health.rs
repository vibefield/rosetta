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
}

pub type IngestCounterSource = Arc<dyn Fn() -> IngestCounters + Send + Sync>;

struct BackfillEntry {
    kind: String,
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
    latest: RuntimeTick,
    last_activity_ms: Option<Ms>,
}

pub struct DaemonHealth {
    started_at_ms: Ms,
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
            }),
        }
    }

    /// The census is on the board and nothing has been read — the user is
    /// choosing. The app renders the roll call off this phase alone.
    pub fn ingest_await_consent(&self) {
        let mut ingest = self.ingest.lock().expect("ingest state poisoned");
        ingest.phase = IngestPhase::AwaitingConsent;
    }

    /// Enroll one installed runtime with its census denominators, queued.
    pub fn ingest_register(&self, kind: &str, files_total: u64, bytes_total: u64) {
        let mut ingest = self.ingest.lock().expect("ingest state poisoned");
        ingest.entries.push(BackfillEntry {
            kind: kind.to_string(),
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
    pub fn ingest_skim(&self, kind: &str, source: IngestCounterSource) {
        let mut ingest = self.ingest.lock().expect("ingest state poisoned");
        ingest.phase = IngestPhase::Backfilling;
        if let Some(entry) = ingest.entries.iter_mut().find(|entry| entry.kind == kind) {
            entry.state = BackfillState::Skimming;
            entry.live = Some(source);
        }
    }

    /// The skim landed: freeze its metadata counts and return the runtime to
    /// the transcript queue. No complete source record has been read yet.
    pub fn ingest_skim_done(&self, kind: &str, sessions_total: u64, files_skimmed: u64) {
        let mut ingest = self.ingest.lock().expect("ingest state poisoned");
        if let Some(entry) = ingest.entries.iter_mut().find(|entry| entry.kind == kind) {
            entry.state = BackfillState::Queued;
            entry.sessions_total = sessions_total;
            entry.files_skimmed = files_skimmed;
            entry.live = None;
        }
    }

    /// Every enrolled runtime has completed its bounded skim. This is a real
    /// observable boundary even when the ordinary daemon immediately proceeds
    /// to streaming; metadata-audit mode deliberately remains here.
    pub fn ingest_metadata_ready(&self) {
        let mut ingest = self.ingest.lock().expect("ingest state poisoned");
        ingest.phase = IngestPhase::MetadataReady;
        for entry in &mut ingest.entries {
            entry.live = None;
        }
    }

    /// Complete-record streaming begins for this runtime. The meter is
    /// cumulative across its interleaved per-file calls.
    pub fn ingest_stream(&self, kind: &str, source: IngestCounterSource) {
        let mut ingest = self.ingest.lock().expect("ingest state poisoned");
        ingest.phase = IngestPhase::Backfilling;
        if let Some(entry) = ingest.entries.iter_mut().find(|entry| entry.kind == kind) {
            entry.state = BackfillState::Streaming;
            entry.live = Some(source);
        }
    }

    /// One runtime's backfill is done: freeze its final numerators.
    pub fn ingest_done(&self, kind: &str, bytes_streamed: u64, records: u64) {
        let mut ingest = self.ingest.lock().expect("ingest state poisoned");
        if let Some(entry) = ingest.entries.iter_mut().find(|entry| entry.kind == kind) {
            entry.state = BackfillState::Done;
            entry.bytes_streamed = bytes_streamed;
            entry.records = records;
            entry.live = None;
        }
    }

    /// The watch loop owns ingest now. Any entry still mid-phase (a graceful
    /// interrupt) freezes where it stood.
    pub fn ingest_live(&self) {
        let mut ingest = self.ingest.lock().expect("ingest state poisoned");
        ingest.phase = IngestPhase::Live;
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
            projects: ingest.projects.clone(),
            runtimes: ingest
                .entries
                .iter()
                .map(|entry| {
                    let live = entry.live.as_ref().map(|source| source());
                    let mut row = RuntimeBackfill {
                        kind: entry.kind.clone(),
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
                            BackfillState::Skimming => row.files_skimmed = live.files_skimmed,
                            BackfillState::Streaming => {
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
    pub fn update(&self, kind: &str, tick: RuntimeTick, now_ms: Ms) {
        let mut runtimes = self.runtimes.lock().expect("health state poisoned");
        let entry = runtimes.entry(kind.to_string()).or_insert(RuntimeEntry {
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
                .map(|(kind, entry)| RuntimeIngestStatus {
                    kind: kind.clone(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub::HubConfig;

    #[test]
    fn activity_moves_the_clock_but_idle_ticks_do_not() {
        let hub = Hub::new(HubConfig::default(), 1);
        let health = DaemonHealth::new(1_000);
        health.update("claude-code", RuntimeTick::default(), 1_000);

        // Idle ticks and polls are not activity.
        health.update(
            "claude-code",
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
            "claude-code",
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

        health.ingest_register("claude-code", 100, 1_000_000);
        let counter = Arc::new(AtomicU64::new(0));

        // Skimming: the files meter reads through the live source.
        let skim_counter = counter.clone();
        health.ingest_skim(
            "claude-code",
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
        health.ingest_skim_done("claude-code", 250, 100);
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
            "claude-code",
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
        assert_eq!(ingest.runtimes[0].files_skimmed, 100);
        assert_eq!(ingest.runtimes[0].sessions_total, 250);
        assert_eq!(ingest.runtimes[0].bytes_streamed, 600_000);
        assert_eq!(ingest.runtimes[0].records, 7);

        // Done freezes finals; live flips the phase and drops every source.
        health.ingest_done("claude-code", 1_000_000, 5_000);
        health.ingest_live();
        counter.store(9, Ordering::Relaxed);
        let ingest = health.snapshot(&hub, 4_000).ingest;
        assert_eq!(ingest.phase, IngestPhase::Live);
        assert_eq!(ingest.runtimes[0].state, BackfillState::Done);
        assert_eq!(ingest.runtimes[0].bytes_streamed, 1_000_000);
        assert_eq!(ingest.runtimes[0].records, 5_000);
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
}
