//! The pipeline-owned cursor blob stored in `ingest_cursor.checkpoint`.
//!
//! Versioned and opaque to everything else (the autopsy correction): a blob
//! that fails to decode — unknown version, corrupt JSON, foreign format — is
//! EXPIRED, and the answer to an expired cursor is a full replay from byte
//! zero, never a crash and never a guess. Deterministic ids make that replay
//! idempotent.
//!
//! Wraps the capture-layer tail checkpoint with the normalize-side state a
//! resume needs: the next presentation ordinal and the last cumulative usage
//! reading (codex-style running counters must not re-count the whole session
//! as one delta after a restart).

use rosetta_capture::TailCheckpoint;
use rosetta_core::rows::TokenSnapshot;
use serde::{Deserialize, Serialize};

pub const PIPE_CHECKPOINT_VERSION: u32 = 2;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PipeCheckpoint {
    pub v: u32,
    pub session_id: String,
    pub stream_id: String,
    pub normalizer_version: u32,
    /// Store-owned source epoch. It advances on every full replay even when a
    /// capture checkpoint expired and its local tail generation restarted at 1.
    pub source_generation: u64,
    /// Next durable source-op sequence within `source_generation`.
    pub next_source_sequence: u64,
    /// Once false, this generation cannot replace the last clean projection.
    pub generation_clean: bool,
    pub next_ordinal: i64,
    /// Hex-encoded [`TailCheckpoint`]; `None` before the first framed batch.
    pub tail: Option<String>,
    /// Last cumulative usage reading, when the adapter tracks one.
    pub cumulative: Option<TokenSnapshot>,
    /// Adapter-owned, versioned by `normalizer_version` and opaque to pipeline.
    pub adapter_state: Option<serde_json::Value>,
    /// Snapshot sources only: the db file-set stamp at the last successful
    /// pass — equal stamps skip even the copy. Additive with a default so
    /// pre-existing blobs still decode.
    #[serde(default)]
    pub snapshot_stamp: Option<String>,
    /// Snapshot sources only: content revision (blake3 of the serialized
    /// records) of the projected generation — the writer touched the db but
    /// this session's content is unchanged when it matches.
    #[serde(default)]
    pub snapshot_revision: Option<String>,
}

impl PipeCheckpoint {
    pub fn fresh(session_id: &str, stream_id: &str, normalizer_version: u32) -> Self {
        Self {
            v: PIPE_CHECKPOINT_VERSION,
            session_id: session_id.to_string(),
            stream_id: stream_id.to_string(),
            normalizer_version,
            source_generation: 0,
            next_source_sequence: 0,
            generation_clean: true,
            next_ordinal: 0,
            tail: None,
            cumulative: None,
            adapter_state: None,
            snapshot_stamp: None,
            snapshot_revision: None,
        }
    }

    pub fn encode(&self) -> String {
        serde_json::to_string(self).expect("checkpoint serialization is infallible")
    }

    /// `None` = expired (any decode failure) → the caller replays from zero.
    pub fn decode(text: &str) -> Option<Self> {
        let decoded: Self = serde_json::from_str(text).ok()?;
        (decoded.v == PIPE_CHECKPOINT_VERSION).then_some(decoded)
    }

    /// The embedded tail checkpoint; `None` if absent or expired.
    pub fn tail_checkpoint(&self) -> Option<TailCheckpoint> {
        TailCheckpoint::decode_hex(self.tail.as_deref()?).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_expires_rather_than_erroring() {
        let mut checkpoint = PipeCheckpoint::fresh("s1", "transcript", 7);
        checkpoint.next_ordinal = 42;
        checkpoint.cumulative = Some(TokenSnapshot {
            input: Some(100),
            ..Default::default()
        });
        let encoded = checkpoint.encode();
        assert_eq!(PipeCheckpoint::decode(&encoded).unwrap(), checkpoint);

        // Corruption, foreign formats, and future versions all expire.
        assert!(PipeCheckpoint::decode("").is_none());
        assert!(PipeCheckpoint::decode("{\"not\":\"ours\"}").is_none());
        let future = encoded.replace("\"v\":2", "\"v\":999");
        assert!(PipeCheckpoint::decode(&future).is_none());

        // A fresh checkpoint has no tail to resume from.
        assert!(checkpoint.tail_checkpoint().is_none());
    }
}
