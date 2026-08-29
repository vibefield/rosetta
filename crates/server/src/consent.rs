//! The consent gate and the boost ledger — the two places the transport
//! hands the reader's intent back to the synchronous ingest thread.
//!
//! Consent (discover.rs's contract, finally honored end to end): discovery
//! fills a board of census rows, the app shows it, and the USER decides
//! which runtimes enroll. The gate is one-shot per serve run: the writer
//! thread arms it and blocks; the first successful grant releases it.
//!
//! Boost (attention-directed ingestion): the reader opened a session still
//! in the backfill queue, so its file jumps to the front. Advisory by
//! design — an id the queue never meets simply expires with the run.

use std::sync::Mutex;
use std::sync::mpsc::{Receiver, Sender, channel};

use rosetta_protocol::DiscoveryRow;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantError {
    /// The gate is not armed: consent was already granted (or this daemon
    /// never asked). Mid-life enrollment is a restart away, not a race.
    NotOpen,
    /// Empty request, an unknown kind, or a kind that is not installed.
    NotGrantable,
}

pub struct ConsentGate {
    board: Mutex<Vec<DiscoveryRow>>,
    sender: Mutex<Option<Sender<Vec<String>>>>,
}

impl Default for ConsentGate {
    fn default() -> Self {
        Self::new()
    }
}

impl ConsentGate {
    pub fn new() -> Self {
        Self {
            board: Mutex::new(Vec::new()),
            sender: Mutex::new(None),
        }
    }

    /// Publish the census. Replaces the whole board.
    pub fn publish(&self, rows: Vec<DiscoveryRow>) {
        *self.board.lock().expect("consent board poisoned") = rows;
    }

    pub fn rows(&self) -> Vec<DiscoveryRow> {
        self.board.lock().expect("consent board poisoned").clone()
    }

    /// Arm the gate: the writer thread holds the receiver and blocks on it.
    pub fn arm(&self) -> Receiver<Vec<String>> {
        let (tx, rx) = channel();
        *self.sender.lock().expect("consent sender poisoned") = Some(tx);
        rx
    }

    /// Grant consent for `kinds`. Validates against the board (installed
    /// only), marks the rows enabled, and releases the armed writer exactly
    /// once. Duplicate kinds collapse; order is preserved.
    pub fn grant(&self, kinds: &[String]) -> Result<Vec<String>, GrantError> {
        let mut board = self.board.lock().expect("consent board poisoned");
        let mut granted: Vec<String> = Vec::new();
        for kind in kinds {
            let row = board
                .iter()
                .find(|row| &row.kind == kind && row.installed)
                .ok_or(GrantError::NotGrantable)?;
            if !granted.contains(&row.kind) {
                granted.push(row.kind.clone());
            }
        }
        if granted.is_empty() {
            return Err(GrantError::NotGrantable);
        }
        let sender = self
            .sender
            .lock()
            .expect("consent sender poisoned")
            .take()
            .ok_or(GrantError::NotOpen)?;
        for row in board.iter_mut() {
            if granted.contains(&row.kind) {
                row.enabled = true;
            }
        }
        // A hung-up receiver means the writer already moved on (shutdown);
        // the grant still stands on the board.
        let _ = sender.send(granted.clone());
        Ok(granted)
    }
}

/// Session ids the reader has opened while their files were still queued,
/// oldest request first. The serve loop drains between files.
#[derive(Default)]
pub struct BoostLedger {
    queue: Mutex<Vec<String>>,
}

impl BoostLedger {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&self, session_id: &str) {
        let mut queue = self.queue.lock().expect("boost queue poisoned");
        if !queue.iter().any(|queued| queued == session_id) {
            queue.push(session_id.to_owned());
        }
    }

    pub fn drain(&self) -> Vec<String> {
        std::mem::take(&mut *self.queue.lock().expect("boost queue poisoned"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(kind: &str, installed: bool) -> DiscoveryRow {
        DiscoveryRow {
            kind: kind.into(),
            data_root: format!("~/.{kind}"),
            installed,
            files: 10,
            bytes: 1_000,
            newest_modified_ms: 5,
            enabled: false,
        }
    }

    #[test]
    fn grant_validates_releases_once_and_marks_the_board() {
        let gate = ConsentGate::new();
        gate.publish(vec![row("claude-code", true), row("codex", false)]);
        let rx = gate.arm();

        // Not installed and unknown kinds are not grantable.
        assert_eq!(gate.grant(&["codex".into()]), Err(GrantError::NotGrantable));
        assert_eq!(gate.grant(&[]), Err(GrantError::NotGrantable));

        let granted = gate
            .grant(&["claude-code".into(), "claude-code".into()])
            .expect("grantable");
        assert_eq!(granted, vec!["claude-code".to_string()]);
        assert_eq!(
            rx.recv().expect("released"),
            vec!["claude-code".to_string()]
        );
        assert!(gate.rows()[0].enabled);

        // One-shot: the gate is spent.
        assert_eq!(
            gate.grant(&["claude-code".into()]),
            Err(GrantError::NotOpen)
        );
    }

    #[test]
    fn boost_dedupes_and_drains_in_arrival_order() {
        let ledger = BoostLedger::new();
        ledger.push("s2");
        ledger.push("s1");
        ledger.push("s2");
        assert_eq!(ledger.drain(), vec!["s2".to_string(), "s1".to_string()]);
        assert!(ledger.drain().is_empty());
    }
}
