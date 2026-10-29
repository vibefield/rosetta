//! One cold writer, durable intent, and a publication fence shared with HTTP.
//! The receipt contains control identities only. Sealed raw evidence and
//! canonical cursors remain the recovery authority.

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use rosetta_server::ingestion::{
    ApiError, ErrorCode, IngestPhase, Ingestion, IngestionAction, IngestionConflict,
    IngestionControl, IngestionControlRequest, IngestionPhase, IngestionReply, IngestionState,
    control_error,
};
use serde::{Deserialize, Serialize};

use crate::engine::now_ms;

const RETAIN_MS: i64 = 24 * 60 * 60 * 1_000;
const CAPACITY: usize = 4096;

#[derive(Clone, Serialize, Deserialize)]
struct Receipt {
    key: String,
    request: String,
    accepted_at: i64,
}

#[derive(Clone, Serialize, Deserialize)]
struct Record {
    format: u32,
    binding: String,
    id: String,
    index_id: String,
    revision: u64,
    state: IngestionState,
    phase: Option<IngestionPhase>,
    created_at: i64,
    updated_at: i64,
    runtime_ids: Vec<String>,
    error: Option<String>,
    receipts: Vec<Receipt>,
}

struct Inner {
    record: Record,
    epoch: u64,
}

pub(crate) struct ColdControl {
    path: PathBuf,
    inner: Mutex<Inner>,
    changed: Condvar,
    /// Distinct from shutdown: cancelling cold work keeps the listener up.
    cancel: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
}

impl ColdControl {
    pub(crate) fn open(
        db: &Path,
        ledger: &Path,
        published: bool,
        shutdown: Arc<AtomicBool>,
    ) -> Result<Option<Arc<Self>>, String> {
        let path = db.with_file_name(format!(
            "{}.ingestion.json",
            db.file_name()
                .ok_or("projection needs a filename")?
                .to_string_lossy()
        ));
        if published && !path.exists() {
            return Ok(None);
        }
        let binding = digest(&format!("{}\n{}", absolute(db)?, absolute(ledger)?));
        let now = now_ms();
        let mut record: Record = if path.exists() {
            let bytes = std::fs::read(&path).map_err(|e| format!("reading cold control: {e}"))?;
            if bytes.len() > 2 * 1024 * 1024 {
                return Err("cold control receipt is too large".into());
            }
            let record: Record =
                serde_json::from_slice(&bytes).map_err(|e| format!("reading cold control: {e}"))?;
            if record.format != 1 || record.binding != binding || record.receipts.len() > CAPACITY {
                return Err(
                    "cold control receipt does not belong to this projection/ledger".into(),
                );
            }
            record
        } else {
            let parent = path
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            // tempfile's random, exclusively created name supplies a nonce;
            // the opaque id exposes neither this path nor source identities.
            let nonce = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
            let seed = format!("{}:{now}:{}", nonce.path().display(), std::process::id());
            Record {
                format: 1,
                binding,
                id: format!("ing_{}", digest(&seed)),
                index_id: format!("idx_{}", digest(&format!("index:{seed}"))),
                revision: 1,
                state: IngestionState::Running,
                phase: Some(IngestionPhase::Discovering),
                created_at: now,
                updated_at: now,
                runtime_ids: vec![],
                error: None,
                receipts: vec![],
            }
        };
        if published {
            if record.state != IngestionState::Succeeded
                && record.phase != Some(IngestionPhase::Publishing)
            {
                return Err(
                    "published projection conflicts with unfinished cold control receipt".into(),
                );
            }
            record.state = IngestionState::Succeeded;
            record.phase = None;
            record.error = None;
        } else if record.state == IngestionState::Succeeded {
            return Err(
                "completed cold run has no published projection; restore the archive explicitly"
                    .into(),
            );
        } else if record.state == IngestionState::Pausing {
            // No worker survives process restart. Never turn accepted pause
            // intent into new capture, including a crash before its reply.
            record.state = IngestionState::Paused;
        } else if record.phase == Some(IngestionPhase::Publishing) {
            // The projection lock and cold-stage manifest own reconciliation
            // of a ledger already promoted before SQLite was installed.
            record.state = IngestionState::Running;
            record.phase = Some(IngestionPhase::Restoring);
        }
        record.revision += 1;
        record.updated_at = now;
        persist(&path, &record)?;
        let cancelled = matches!(
            record.state,
            IngestionState::Paused | IngestionState::Failed
        );
        Ok(Some(Arc::new(Self {
            path,
            inner: Mutex::new(Inner {
                record,
                epoch: now as u64,
            }),
            changed: Condvar::new(),
            cancel: Arc::new(AtomicBool::new(cancelled)),
            shutdown,
        })))
    }

    pub(crate) fn cancel(&self) -> Arc<AtomicBool> {
        self.cancel.clone()
    }

    pub(crate) fn configure(&self, mut ids: Vec<String>) -> Result<(), String> {
        ids.sort();
        ids.dedup();
        let mut inner = self.inner.lock().expect("cold control poisoned");
        if inner.record.runtime_ids != ids {
            let mut next = inner.record.clone();
            next.runtime_ids = ids;
            advance(&mut next);
            self.save(&mut inner, next)?;
            inner.epoch += 1;
        }
        Ok(())
    }

    /// Called only after all cold workers have joined and evidence is synced.
    pub(crate) fn stopped(&self, failure: Option<&str>) -> Result<(), String> {
        let mut inner = self.inner.lock().expect("cold control poisoned");
        if failure.is_none() && inner.record.state != IngestionState::Pausing {
            return Ok(());
        }
        let mut next = inner.record.clone();
        if let Some(error) = failure {
            next.state = IngestionState::Failed;
            next.error = Some(error.chars().take(512).collect());
        } else {
            next.state = IngestionState::Paused;
            next.error = None;
        }
        advance(&mut next);
        self.save(&mut inner, next)?;
        self.changed.notify_all();
        Ok(())
    }

    pub(crate) fn pause_requested(&self) -> bool {
        matches!(
            self.inner
                .lock()
                .expect("cold control poisoned")
                .record
                .state,
            IngestionState::Pausing | IngestionState::Paused
        )
    }

    /// Waits with the HTTP service alive. A process shutdown never changes
    /// the desired run state; a paused process exits without resuming it.
    pub(crate) fn wait(&self) -> Result<(), String> {
        let mut inner = self.inner.lock().expect("cold control poisoned");
        loop {
            if self.shutdown.load(Ordering::SeqCst) {
                return Err("cold run stopped for process shutdown".into());
            }
            if inner.record.state == IngestionState::Running {
                return Ok(());
            }
            inner = self
                .changed
                .wait_timeout(inner, Duration::from_millis(100))
                .expect("cold control poisoned")
                .0;
        }
    }

    /// Shares command admission's mutex. A pause that won this lock prevents
    /// publication; after this durable fence, controls return a conflict.
    pub(crate) fn publishing(&self) -> Result<(), String> {
        let mut inner = self.inner.lock().expect("cold control poisoned");
        if self.shutdown.load(Ordering::SeqCst) || inner.record.state != IngestionState::Running {
            return Err("cold publication interrupted before its fence".into());
        }
        let mut next = inner.record.clone();
        next.phase = Some(IngestionPhase::Publishing);
        advance(&mut next);
        self.save(&mut inner, next)
    }

    pub(crate) fn succeeded(&self) -> Result<(), String> {
        let mut inner = self.inner.lock().expect("cold control poisoned");
        let mut next = inner.record.clone();
        next.state = IngestionState::Succeeded;
        next.phase = None;
        next.error = None;
        advance(&mut next);
        self.save(&mut inner, next)
    }

    fn save(&self, inner: &mut Inner, next: Record) -> Result<(), String> {
        persist(&self.path, &next)?;
        inner.record = next;
        Ok(())
    }
}

impl IngestionControl for ColdControl {
    fn snapshot(&self) -> Ingestion {
        snapshot(&self.inner.lock().expect("cold control poisoned"))
    }

    fn observe_phase(&self, phase: IngestPhase) {
        let mut inner = self.inner.lock().expect("cold control poisoned");
        if inner.record.phase == Some(IngestionPhase::Publishing)
            || !matches!(
                inner.record.state,
                IngestionState::Running | IngestionState::Pausing
            )
        {
            return;
        }
        inner.record.phase = Some(match phase {
            IngestPhase::Discovering | IngestPhase::AwaitingConsent => IngestionPhase::Discovering,
            IngestPhase::Restoring => IngestionPhase::Restoring,
            IngestPhase::Finalizing => IngestionPhase::Finalizing,
            IngestPhase::Publishing => return, // only the durable fence may set this
            IngestPhase::Live => return,
            _ => IngestionPhase::Capturing,
        });
    }

    fn command(
        &self,
        id: &str,
        index: &str,
        key: &str,
        action: IngestionAction,
        request: IngestionControlRequest,
    ) -> Result<IngestionReply, ApiError> {
        let mut inner = self.inner.lock().expect("cold control poisoned");
        let error = |code, message: &str| ApiError {
            code,
            message: message.into(),
            details: Some(IngestionConflict {
                ingestion_id: inner.record.id.clone(),
                current_revision: inner.record.revision,
            }),
        };
        if index != inner.record.index_id {
            return Err(error(ErrorCode::IndexChanged, "the logical index changed"));
        }
        if id != inner.record.id {
            return Err(error(ErrorCode::NotFound, "unknown ingestion"));
        }
        let key = digest(&key.to_ascii_lowercase());
        let fingerprint = digest(&format!(
            "POST:{id}:{action:?}:{}",
            request.expected_revision
        ));
        if let Some(receipt) = inner.record.receipts.iter().find(|r| r.key == key) {
            if receipt.request != fingerprint {
                return Err(error(
                    ErrorCode::IdempotencyConflict,
                    "the operation key names a different request",
                ));
            }
            return Ok(IngestionReply {
                run: snapshot(&inner),
                accepted: false,
            });
        }
        if self.shutdown.load(Ordering::SeqCst) {
            return Err(error(
                ErrorCode::ShuttingDown,
                "the process is shutting down",
            ));
        }
        if inner.record.revision != request.expected_revision {
            return Err(error(
                ErrorCode::RevisionConflict,
                "the run changed; refetch before submitting a new command",
            ));
        }
        if inner.record.phase == Some(IngestionPhase::Publishing) {
            return Err(error(
                ErrorCode::PublicationInProgress,
                "publication already owns the writer",
            ));
        }
        let next_state = match (action, inner.record.state) {
            (IngestionAction::Pause, IngestionState::Running) => IngestionState::Pausing,
            (IngestionAction::Pause, IngestionState::Pausing | IngestionState::Paused) => {
                inner.record.state
            }
            (IngestionAction::Resume, IngestionState::Paused | IngestionState::Failed) => {
                IngestionState::Running
            }
            (IngestionAction::Resume, IngestionState::Running) => IngestionState::Running,
            _ => {
                return Err(error(
                    ErrorCode::InvalidTransition,
                    "this action is not available in the current run state",
                ));
            }
        };
        let now = now_ms();
        let mut next = inner.record.clone();
        next.receipts
            .retain(|r| now.saturating_sub(r.accepted_at) < RETAIN_MS);
        let limit = CAPACITY - usize::from(action == IngestionAction::Resume);
        if next.receipts.len() >= limit {
            return Err(error(
                ErrorCode::ControlCapacity,
                "control receipt capacity is exhausted; retry later",
            ));
        }
        let accepted = next.state != next_state;
        next.state = next_state;
        if accepted {
            next.error = None;
            advance(&mut next);
        }
        next.receipts.push(Receipt {
            key,
            request: fingerprint,
            accepted_at: now,
        });
        self.save(&mut inner, next)
            .map_err(|message| control_error(ErrorCode::Internal, message))?;
        if accepted {
            self.cancel
                .store(next_state != IngestionState::Running, Ordering::SeqCst);
            if next_state == IngestionState::Running {
                inner.epoch += 1;
            }
        }
        let run = snapshot(&inner);
        self.changed.notify_all();
        Ok(IngestionReply { run, accepted })
    }
}

fn snapshot(inner: &Inner) -> Ingestion {
    let r = &inner.record;
    let allowed_actions = if r.phase == Some(IngestionPhase::Publishing) {
        vec![]
    } else {
        match r.state {
            IngestionState::Running => vec![IngestionAction::Pause],
            IngestionState::Paused | IngestionState::Failed => vec![IngestionAction::Resume],
            _ => vec![],
        }
    };
    Ingestion {
        id: r.id.clone(),
        index_id: r.index_id.clone(),
        revision: r.revision,
        state: r.state,
        phase: r.phase,
        created_at: r.created_at,
        updated_at: r.updated_at,
        progress_epoch: format!("{}:{}", r.id, inner.epoch),
        runtime_ids: r.runtime_ids.clone(),
        allowed_actions,
        error: r.error.clone(),
    }
}

fn advance(record: &mut Record) {
    record.revision += 1;
    record.updated_at = now_ms();
}
fn digest(text: &str) -> String {
    blake3::hash(text.as_bytes()).to_hex().to_string()
}
fn absolute(path: &Path) -> Result<String, String> {
    std::path::absolute(path)
        .map(|p| p.to_string_lossy().into_owned())
        .map_err(|e| e.to_string())
}
fn persist(path: &Path, record: &Record) -> Result<(), String> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut file = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
    serde_json::to_writer(&mut file, record).map_err(|e| e.to_string())?;
    file.flush()
        .and_then(|()| file.as_file().sync_all())
        .map_err(|e| e.to_string())?;
    file.persist(path).map_err(|e| e.to_string())?;
    File::open(parent)
        .and_then(|file| file.sync_all())
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests;
