//! Filesystem change notifications behind a trait — the recovery rules from
//! spaghetti (docs/ingest-engine.html, ports list):
//!
//! - **Notifications are hints.** An event never carries truth about what
//!   changed; it only nominates a path. The tailer's checkpoint decides what
//!   actually happened, so a spurious, duplicated, or lost event can never
//!   corrupt state — only delay or waste a read.
//! - **Watch before scan.** Callers start the watcher before their initial
//!   backfill scan so a write landing mid-scan is never missed entirely.
//! - **Polling is the fallback, not the watcher.** FS watching is the
//!   flakiest OS surface we touch; any watcher error degrades to `Rescan`,
//!   which tells the consumer to stat the world instead of trusting events.
//!
//! Tests inject [`ScriptedEvents`]; only `NotifyEvents` touches the OS.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, SyncSender, channel, sync_channel};
use std::time::Duration;

use notify::{RecursiveMode, Watcher};

use crate::CaptureError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceEvent {
    /// Something happened at this root-relative `/`-separated path. A hint.
    Changed(String),
    /// Events may have been lost (overflow, watcher error, unmappable path):
    /// the consumer should fall back to a stat scan.
    Rescan,
}

/// A stream of change hints. `next_event` blocks up to `timeout` — `None`
/// means a quiet interval, which consumers use as their poll/audit tick.
pub trait SourceEvents: Send {
    fn next_event(&mut self, timeout: Duration) -> Option<SourceEvent>;
}

/// One coalesced wake-up for a writer serving several filesystem watchers.
/// Hints remain in each watcher's own queue; this channel only tells the
/// writer to make a non-blocking pass over those queues. A signal sent before
/// `wait` is retained, including one arriving during the preceding pass.
pub struct SourceWake {
    sender: SyncSender<()>,
    receiver: Receiver<()>,
}

impl Default for SourceWake {
    fn default() -> Self {
        let (sender, receiver) = sync_channel(1);
        Self { sender, receiver }
    }
}

impl SourceWake {
    /// Wait once for any attached watcher, bounded by the poll/shutdown tick.
    /// Returns whether a watcher signalled; a timeout still requires a pass
    /// so runners can perform their periodic recovery polls and audits.
    pub fn wait(&self, timeout: Duration) -> bool {
        self.receiver.recv_timeout(timeout).is_ok()
    }
}

/// Deterministic scripted source for tests and replay harnesses. Each queued
/// `None` simulates one quiet timeout; when the script runs out, the source
/// stays quiet forever.
#[derive(Debug, Default)]
pub struct ScriptedEvents {
    script: VecDeque<Option<SourceEvent>>,
}

impl ScriptedEvents {
    pub fn new(script: impl IntoIterator<Item = Option<SourceEvent>>) -> Self {
        Self {
            script: script.into_iter().collect(),
        }
    }

    pub fn push_changed(&mut self, relative_path: &str) {
        self.script
            .push_back(Some(SourceEvent::Changed(relative_path.to_string())));
    }

    pub fn push_quiet(&mut self) {
        self.script.push_back(None);
    }

    pub fn push_rescan(&mut self) {
        self.script.push_back(Some(SourceEvent::Rescan));
    }
}

impl SourceEvents for ScriptedEvents {
    fn next_event(&mut self, _timeout: Duration) -> Option<SourceEvent> {
        self.script.pop_front().flatten()
    }
}

/// Production source: the `notify` recommended watcher for this platform,
/// recursive over one data root. Construction succeeds only if the watch was
/// actually registered; every later failure degrades to `Rescan`.
pub struct NotifyEvents {
    // Field order is drop order: close the channel after the watcher thread
    // can no longer send into it.
    _watcher: notify::RecommendedWatcher,
    receiver: Receiver<SourceEvent>,
    disconnected: bool,
}

impl NotifyEvents {
    pub fn watch(root: &Path) -> Result<Self, CaptureError> {
        Self::watch_inner(root, None)
    }

    /// Register before the initial scan, retaining hints throughout backfill
    /// and waking the shared writer as soon as any runtime has queued work.
    pub fn watch_with_wake(root: &Path, wake: &SourceWake) -> Result<Self, CaptureError> {
        Self::watch_inner(root, Some(wake.sender.clone()))
    }

    fn watch_inner(root: &Path, wake: Option<SyncSender<()>>) -> Result<Self, CaptureError> {
        let (sender, receiver) = channel();
        // The OS reports resolved paths (macOS FSEvents says /private/var for
        // a /var root); strip against the canonical root or every hint would
        // degrade to Rescan.
        let event_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        let mut watcher = notify::recommended_watcher(move |event| {
            forward(&event_root, &sender, event, wake.as_ref());
        })
        .map_err(|error| CaptureError::InvalidConfig(format!("starting watcher: {error}")))?;
        watcher
            .watch(root, RecursiveMode::Recursive)
            .map_err(|error| {
                CaptureError::InvalidConfig(format!("watching {}: {error}", root.display()))
            })?;
        Ok(Self {
            receiver,
            _watcher: watcher,
            disconnected: false,
        })
    }
}

impl SourceEvents for NotifyEvents {
    fn next_event(&mut self, timeout: Duration) -> Option<SourceEvent> {
        if self.disconnected {
            std::thread::sleep(timeout);
            return None;
        }
        match self.receiver.recv_timeout(timeout) {
            Ok(event) => Some(event),
            Err(RecvTimeoutError::Timeout) => None,
            // Request recovery once, then rely on periodic polling. Repeating
            // Rescan forever would keep a shared non-blocking writer spinning.
            Err(RecvTimeoutError::Disconnected) => {
                self.disconnected = true;
                Some(SourceEvent::Rescan)
            }
        }
    }
}

fn forward(
    root: &Path,
    sender: &Sender<SourceEvent>,
    event: notify::Result<notify::Event>,
    wake: Option<&SyncSender<()>>,
) {
    match event {
        Ok(event) => {
            if event.need_rescan() {
                let _ = sender.send(SourceEvent::Rescan);
            } else {
                for path in &event.paths {
                    let _ = sender.send(to_hint(root, path));
                }
            }
        }
        Err(_) => {
            let _ = sender.send(SourceEvent::Rescan);
        }
    }
    if let Some(wake) = wake {
        // Queue the hints first. A full wake channel already promises a
        // pass, so producers never block or accumulate wake tokens.
        let _ = wake.try_send(());
    }
}

fn to_hint(root: &Path, path: &Path) -> SourceEvent {
    let Ok(relative) = path.strip_prefix(root) else {
        return SourceEvent::Rescan;
    };
    let mut out = String::new();
    for component in relative.components() {
        match component {
            std::path::Component::Normal(value) => {
                let Some(value) = value.to_str() else {
                    return SourceEvent::Rescan;
                };
                if !out.is_empty() {
                    out.push('/');
                }
                out.push_str(value);
            }
            std::path::Component::CurDir => {}
            _ => return SourceEvent::Rescan,
        }
    }
    if out.is_empty() {
        SourceEvent::Rescan
    } else {
        SourceEvent::Changed(out)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use tempfile::TempDir;

    use super::*;

    #[test]
    fn shared_wake_coalesces_runtimes_without_losing_their_hints() {
        let wake = SourceWake::default();
        let root = Path::new("/sources");
        let queues: Vec<_> = (0..9).map(|_| channel()).collect();
        assert!(!wake.wait(Duration::ZERO));
        for (index, (sender, _)) in queues.iter().enumerate() {
            forward(
                root,
                sender,
                Ok(notify::Event::new(notify::EventKind::Any)
                    .add_path(root.join(format!("{index}.jsonl")))),
                Some(&wake.sender),
            );
        }
        assert!(wake.wait(Duration::ZERO));
        assert!(
            !wake.wait(Duration::ZERO),
            "only one wake token accumulates"
        );
        for (index, (_, receiver)) in queues.iter().enumerate() {
            assert_eq!(
                receiver.try_recv().unwrap(),
                SourceEvent::Changed(format!("{index}.jsonl"))
            );
        }

        // A watcher error arriving during the sweep must also survive until
        // the next wait and nominate a recovery scan.
        forward(
            root,
            &queues[8].0,
            Err(notify::Error::generic("lost events")),
            Some(&wake.sender),
        );
        assert!(wake.wait(Duration::ZERO));
        assert_eq!(queues[8].1.try_recv().unwrap(), SourceEvent::Rescan);
    }

    #[test]
    fn disconnected_watcher_requests_recovery_without_an_endless_ready_queue() {
        let root = TempDir::new().unwrap();
        let mut events = NotifyEvents::watch(root.path()).unwrap();
        let (sender, receiver) = channel();
        events.receiver = receiver;
        drop(sender);
        assert_eq!(events.next_event(Duration::ZERO), Some(SourceEvent::Rescan));
        assert_eq!(events.next_event(Duration::ZERO), None);
    }

    #[test]
    fn scripted_events_replay_in_order_with_quiet_gaps() {
        let mut events = ScriptedEvents::default();
        events.push_changed("a.jsonl");
        events.push_quiet();
        events.push_rescan();

        let timeout = Duration::from_millis(1);
        assert_eq!(
            events.next_event(timeout),
            Some(SourceEvent::Changed("a.jsonl".into()))
        );
        assert_eq!(events.next_event(timeout), None);
        assert_eq!(events.next_event(timeout), Some(SourceEvent::Rescan));
        // Exhausted scripts stay quiet forever rather than panicking.
        assert_eq!(events.next_event(timeout), None);
    }

    /// Opt-in smoke test of the real OS watcher. Sandboxes commonly suppress
    /// FSEvents/inotify delivery entirely, so deterministic correctness lives
    /// in the scripted-event and live polling/audit tests.
    #[test]
    #[ignore = "requires host filesystem notification delivery"]
    fn notify_watcher_hints_about_a_write() {
        let root = TempDir::new().unwrap();
        std::fs::create_dir_all(root.path().join("projects")).unwrap();
        let wake = SourceWake::default();
        let mut events = NotifyEvents::watch_with_wake(root.path(), &wake).unwrap();

        // Some backends finish installing their recursive watch on a helper
        // thread after `watch` returns.
        std::thread::sleep(Duration::from_millis(100));
        std::fs::write(root.path().join("projects/s.jsonl"), b"{}\n").unwrap();

        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if !wake.wait(Duration::from_millis(200)) {
                continue;
            }
            match events.next_event(Duration::ZERO) {
                // FSEvents commonly reports only the containing directory;
                // Linux and Windows usually report the file. Both are valid
                // hints because the live runner re-stats declared sources.
                Some(_) => return,
                None => continue,
            }
        }
        panic!("no hint for the written file within the deadline");
    }
}
