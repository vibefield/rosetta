//! Daemon liveness shared between the synchronous ingest loop and the
//! transport: the loop pushes plain counters after each tick, `/api/status`
//! snapshots them with the hub's occupancy. Deliberately dumb — no clock of
//! its own (callers pass `now`), no dependency on pipeline types, so the
//! server never reaches below L3 for its health surface.

use std::collections::BTreeMap;
use std::sync::Mutex;

use rosetta_core::rows::Ms;
use rosetta_protocol::{DaemonStatus, RuntimeIngestStatus};

use crate::hub::Hub;

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
}

impl DaemonHealth {
    pub fn new(now_ms: Ms) -> Self {
        Self {
            started_at_ms: now_ms,
            runtimes: Mutex::new(BTreeMap::new()),
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
}
