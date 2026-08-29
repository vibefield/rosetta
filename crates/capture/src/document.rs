//! Stable, bounded snapshots of rewrite-in-place document sidecars.
//!
//! The caller retries [`DocumentOutcome::Unstable`]. Keeping the primitive in
//! capture means JSON, TOML, Markdown, and plain-text adapters all share the
//! same race and size-limit behavior.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use crate::{CaptureError, io_error};

#[derive(Debug)]
pub struct DocumentSnapshot {
    bytes: Vec<u8>,
    stamp: String,
}

impl DocumentSnapshot {
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn stamp(&self) -> &str {
        &self.stamp
    }
}

#[derive(Debug)]
pub enum DocumentOutcome {
    Ready(DocumentSnapshot),
    Missing,
    Unstable,
    Oversized { size: u64, limit: usize },
}

/// Read a document only when its metadata is stable across the bounded read.
pub fn snapshot(path: &Path, max_bytes: usize) -> Result<DocumentOutcome, CaptureError> {
    let before = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(DocumentOutcome::Missing);
        }
        Err(error) => return Err(io_error("statting document", path, error)),
    };
    if before.len() > max_bytes as u64 {
        return Ok(DocumentOutcome::Oversized {
            size: before.len(),
            limit: max_bytes,
        });
    }

    let file = File::open(path).map_err(|error| io_error("opening document", path, error))?;
    let mut bytes = Vec::with_capacity(before.len() as usize);
    file.take(max_bytes as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| io_error("reading document", path, error))?;
    if bytes.len() > max_bytes {
        return Ok(DocumentOutcome::Oversized {
            size: bytes.len() as u64,
            limit: max_bytes,
        });
    }

    let after = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(DocumentOutcome::Missing);
        }
        Err(error) => return Err(io_error("restatting document", path, error)),
    };
    let before_modified = crate::discovery::modified_ms(&before);
    let after_modified = crate::discovery::modified_ms(&after);
    if before.len() != after.len()
        || before_modified != after_modified
        || after.len() != bytes.len() as u64
    {
        return Ok(DocumentOutcome::Unstable);
    }

    Ok(DocumentOutcome::Ready(DocumentSnapshot {
        bytes,
        stamp: format!("{}:{after_modified}", after.len()),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_bounded_document() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(&path, br#"{"session_id":"s1"}"#).unwrap();
        let DocumentOutcome::Ready(snapshot) = snapshot(&path, 1024).unwrap() else {
            panic!("expected ready document");
        };
        assert_eq!(snapshot.bytes(), br#"{"session_id":"s1"}"#);
        assert!(snapshot.stamp().starts_with("19:"));
    }

    #[test]
    fn rejects_oversized_document_without_reading_it() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("plan.md");
        std::fs::write(&path, b"too large").unwrap();
        assert!(matches!(
            snapshot(&path, 3).unwrap(),
            DocumentOutcome::Oversized { size: 9, limit: 3 }
        ));
    }
}
