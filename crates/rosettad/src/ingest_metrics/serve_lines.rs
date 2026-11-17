//! Serve-level measurement lines (docs/ARCHITECTURE.md, "Cold-build
//! measurement"): the clock anchor every `elapsed_s` maps onto, the moment a
//! roll-call grant ends the consent wait, and the detached search worker's
//! first outcome after a fresh RAM build's canonical readiness. Each prints
//! once; none waits on, orders or fails anything. They carry times only.

use std::time::{Instant, SystemTime, UNIX_EPOCH};

use rosetta_search::{FirstSync, FirstSyncWatch};
use serde_json::{Value, json};

use crate::bench;

/// Serve's milestone base and its wall-clock anchor, taken together: a
/// wall-clock stamp `w` (Unix ms) is `elapsed_s = (w - wall_ms) / 1000`.
pub(crate) struct ServeClock {
    started: Instant,
    wall_ms: u64,
}

impl ServeClock {
    pub(crate) fn now() -> Self {
        let started = Instant::now();
        let wall_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| {
                u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
            });
        Self { started, wall_ms }
    }

    /// The instant every `milestone:` line's `elapsed_s` counts from.
    pub(crate) fn started(&self) -> Instant {
        self.started
    }

    fn said(&self) -> (String, Value) {
        (
            format!("clock: wall_ms={}", self.wall_ms),
            json!({"event": "clock", "wallMs": self.wall_ms}),
        )
    }

    /// Print `clock: wall_ms=`, and its `clock` event under `--json`.
    pub(crate) fn announce(&self, json: bool) {
        say(self.said(), json);
    }
}

fn say((line, event): (String, Value), json: bool) {
    println!("{line}");
    if json {
        bench::emit(&event);
    }
}

fn consent_said(elapsed_s: f64) -> (String, Value) {
    (
        format!("milestone: consent-granted elapsed_s={elapsed_s:.2}"),
        json!({
            "event": "milestone",
            "milestone": "consent-granted",
            "elapsedS": elapsed_s,
        }),
    )
}

/// A roll-call grant ended the consent wait and this start's enrolment is
/// decided: `milestone: consent-granted`, and its event under `--json`.
pub(crate) fn consent_granted(started: Instant, json: bool) {
    say(consent_said(started.elapsed().as_secs_f64()), json);
}

/// The search milestone for the worker's first outcome; none when it
/// stopped first.
fn search_line(outcome: FirstSync, started: Instant) -> Option<String> {
    let (name, at) = match outcome {
        FirstSync::Published(at) => ("search-ready", at),
        FirstSync::Failed(at) => ("search-failed", at),
        FirstSync::Stopped => return None,
    };
    Some(format!(
        "milestone: {name} elapsed_s={:.2}",
        at.saturating_duration_since(started).as_secs_f64()
    ))
}

/// Print `milestone: search-ready` (or `search-failed`) at the detached
/// worker's first publication (or failure) after its wake. The serve path
/// never waits for it: a detached thread waits on the worker's latch, and
/// prints nothing if the worker stops first or the process exits first.
pub(crate) fn observe_first_search(started: Instant, watch: FirstSyncWatch) {
    let spawned = std::thread::Builder::new()
        .name("rosettad-search-milestone".into())
        .spawn(move || {
            if let Some(line) = search_line(watch.wait(), started) {
                println!("{line}");
            }
        });
    if let Err(error) = spawned {
        eprintln!("search milestone unavailable: {error}");
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn the_clock_anchor_is_one_line_and_one_event_taken_with_the_base() {
        let before = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
        let clock = ServeClock::now();
        let after = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
        let (line, event) = clock.said();
        let wall_ms: u64 = line
            .strip_prefix("clock: wall_ms=")
            .unwrap()
            .parse()
            .unwrap();
        assert!(before.as_millis() as u64 <= wall_ms && wall_ms <= after.as_millis() as u64);
        assert_eq!(event, json!({"event": "clock", "wallMs": wall_ms}));
        assert!(clock.started().elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn consent_granted_is_a_milestone_line_and_event() {
        let (line, event) = consent_said(1.234);
        assert_eq!(line, "milestone: consent-granted elapsed_s=1.23");
        assert_eq!(event["event"], "milestone");
        assert_eq!(event["milestone"], "consent-granted");
        assert_eq!(event["elapsedS"], 1.234);
    }

    #[test]
    fn the_search_milestone_follows_the_first_outcome_and_a_stop_says_nothing() {
        let started = Instant::now();
        let at = started + Duration::from_millis(2_500);
        assert_eq!(
            search_line(FirstSync::Published(at), started).as_deref(),
            Some("milestone: search-ready elapsed_s=2.50")
        );
        assert_eq!(
            search_line(FirstSync::Failed(at), started).as_deref(),
            Some("milestone: search-failed elapsed_s=2.50")
        );
        assert_eq!(search_line(FirstSync::Stopped, started), None);
    }
}
