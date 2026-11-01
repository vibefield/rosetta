//! Raw bytes observed at an explicitly presented document locator. The parent
//! source/generation/offset ties this evidence to its publication record.

use serde::{Deserialize, Serialize};

use super::worker::AsyncCommand;
use super::{Ledger, MAX_FRAME_BYTES, PrebuiltFrames, SourceDef};
use crate::CaptureError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentCaptureStatus {
    Captured,
    Missing,
    TooLarge,
    Unsupported,
    ChangedDuringRead,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedDocument {
    pub path: String,
    pub captured_at: i64,
    pub status: DocumentCaptureStatus,
    pub recovered_from: Option<String>,
    pub bytes: Vec<u8>,
}

impl CapturedDocument {
    pub(super) fn encode(&self) -> Result<Vec<u8>, CaptureError> {
        let metadata = if self.recovered_from.is_some() {
            serde_json::to_vec(&(
                &self.path,
                self.captured_at,
                self.status,
                &self.recovered_from,
            ))
        } else {
            serde_json::to_vec(&(&self.path, self.captured_at, self.status))
        }
        .map_err(|e| CaptureError::InvalidConfig(e.to_string()))?;
        let size = 4 + metadata.len() + self.bytes.len();
        if size > MAX_FRAME_BYTES - 20 {
            return Err(CaptureError::LimitExceeded(
                "captured document exceeds ledger frame".into(),
            ));
        }
        let mut encoded = Vec::with_capacity(size);
        encoded.extend_from_slice(&(metadata.len() as u32).to_be_bytes());
        encoded.extend_from_slice(&metadata);
        encoded.extend_from_slice(&self.bytes);
        Ok(encoded)
    }

    pub(super) fn decode(bytes: &[u8]) -> Option<Self> {
        let length = u32::from_be_bytes(bytes.get(..4)?.try_into().ok()?) as usize;
        let end = 4_usize.checked_add(length)?;
        // Old document frames remain readable without rewriting the raw ledger.
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Metadata {
            Current(String, i64, DocumentCaptureStatus, Option<String>),
            Legacy(String, i64, DocumentCaptureStatus),
        }
        let (path, captured_at, status, recovered_from) =
            match serde_json::from_slice(bytes.get(4..end)?).ok()? {
                Metadata::Current(path, at, status, from) => (path, at, status, from),
                Metadata::Legacy(path, at, status) => (path, at, status, None),
            };
        let bytes = bytes.get(end..)?.to_vec();
        if path.is_empty()
            || path.len() > 16384
            || path.contains('\0')
            || recovered_from
                .as_ref()
                .is_some_and(|p| p.is_empty() || p.len() > 32768 || p.contains('\0'))
            || (status != DocumentCaptureStatus::Captured && !bytes.is_empty())
            || (status != DocumentCaptureStatus::Captured && recovered_from.is_some())
        {
            return None;
        }
        Some(Self {
            path,
            captured_at,
            status,
            recovered_from,
            bytes,
        })
    }
}

impl Ledger {
    pub fn append_document(
        &mut self,
        source: &SourceDef,
        generation: u64,
        start_offset: u64,
        document: CapturedDocument,
    ) -> Result<(), CaptureError> {
        if self.async_backend.is_some() {
            return self.send_async(AsyncCommand::Document {
                source: source.clone(),
                generation,
                start_offset,
                document,
            });
        }
        let key = self.define(source)?;
        let mut frames = PrebuiltFrames::new(key, generation);
        frames.push_document(start_offset, &document)?;
        self.write_frames(key, frames)
    }
}
