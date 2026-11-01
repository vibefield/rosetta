//! IO belongs at capture, never in adapters or replay. Raw file observations
//! are ledgered with the publication's source coordinate before projection.

use rosetta_adapter_support::{AgentAdapter, NormalizeCtx};
use rosetta_capture::{CapturedDocument, DocumentCaptureStatus as Status};
use rosetta_core::{
    IngestOp, ProjectionAsset,
    html_artifacts::{
        HTML_MAX_BYTES, HTML_MEDIA_TYPE, HtmlArtifactAvailability as Availability,
        HtmlArtifactContent,
    },
};
use std::{
    collections::BTreeSet,
    fs::{File, OpenOptions},
    io::Read,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

mod recovery;
pub use recovery::ArtifactRecovery;

pub(crate) fn capture(
    ops: &mut Vec<IngestOp>,
    ledgered_through: u64,
    adapter: &dyn AgentAdapter,
    ctx: &NormalizeCtx,
    data_root: &str,
    transcript: Option<&Path>,
    recovery: &ArtifactRecovery,
) -> Vec<(u64, CapturedDocument)> {
    let publications: Vec<_> = ops
        .iter()
        .filter_map(|op| match op {
            IngestOp::HtmlArtifact(row)
                if row.source_offset >= ledgered_through && row.path.is_some() =>
            {
                Some(row.clone())
            }
            _ => None,
        })
        .collect();
    let mut documents = Vec::new();
    let mut seen = BTreeSet::new();
    for publication in publications {
        let path = publication.path.as_ref().unwrap();
        if !seen.insert((
            publication.session_id.clone(),
            publication.source_offset,
            path.clone(),
        )) {
            continue;
        }
        let mut document = observe(path.clone());
        if document.status == Status::Missing {
            let hints = adapter.html_artifact_hints(ctx, &publication, data_root);
            if let Some(copy) = recovery.recover(adapter, transcript, &publication, hints) {
                document = copy;
            }
        }
        project(
            &publication.session_id,
            publication.source_offset,
            &document,
            ops,
        );
        documents.push((publication.source_offset, document));
    }
    documents
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn observe(path: String) -> CapturedDocument {
    let mut document = CapturedDocument {
        path,
        captured_at: now(),
        status: Status::Unsupported,
        recovered_from: None,
        bytes: Vec::new(),
    };
    let path = Path::new(&document.path);
    if !path.is_absolute() || document.path.starts_with("//") {
        return document;
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // A renamed symlink/FIFO must neither escape the file observation nor
        // stall the ingest worker. Reject non-regular descriptors below.
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut file = match options.open(path) {
        Ok(file) => file,
        Err(error) => {
            document.status = if error.kind() == std::io::ErrorKind::NotFound {
                Status::Missing
            } else {
                Status::Unsupported
            };
            return document;
        }
    };
    document.status = read_stable(&mut file, &mut document.bytes);
    if document.status != Status::Captured {
        document.bytes.clear();
    }
    document
}

fn read_stable(file: &mut File, bytes: &mut Vec<u8>) -> Status {
    let Ok(before) = file.metadata() else {
        return Status::Unsupported;
    };
    if !before.is_file() {
        return Status::Unsupported;
    }
    if before.len() > HTML_MAX_BYTES as u64 {
        return Status::TooLarge;
    }
    if file
        .take(HTML_MAX_BYTES as u64 + 1)
        .read_to_end(bytes)
        .is_err()
    {
        return Status::Unsupported;
    }
    if bytes.len() > HTML_MAX_BYTES {
        return Status::TooLarge;
    }
    let Ok(after) = file.metadata() else {
        return Status::ChangedDuringRead;
    };
    if before.len() != after.len()
        || after.len() != bytes.len() as u64
        || before.modified().ok() != after.modified().ok()
    {
        return Status::ChangedDuringRead;
    }
    if utf8(bytes).is_none() {
        return Status::Unsupported;
    }
    Status::Captured
}

fn utf8(bytes: &[u8]) -> Option<Vec<u8>> {
    if let Some(body) = bytes.strip_prefix(&[0xff, 0xfe]) {
        if !body.len().is_multiple_of(2) {
            return None;
        }
        let units: Vec<_> = body
            .chunks_exact(2)
            .map(|p| u16::from_le_bytes([p[0], p[1]]))
            .collect();
        let text = String::from_utf16(&units).ok()?;
        if text.len() > HTML_MAX_BYTES || text.contains('\0') {
            return None;
        }
        return Some(text.into_bytes());
    }
    let text = std::str::from_utf8(bytes).ok()?;
    (!text.contains('\0') && bytes.len() <= HTML_MAX_BYTES).then(|| bytes.to_vec())
}

/// Pure reconstruction from ledger evidence. No read of today's source file.
pub(crate) fn project(
    session_id: &str,
    offset: u64,
    document: &CapturedDocument,
    ops: &mut Vec<IngestOp>,
) {
    let mut availability = match document.status {
        Status::Captured => Availability::Captured,
        Status::Missing => Availability::Missing,
        Status::TooLarge => Availability::TooLarge,
        Status::Unsupported => Availability::Unsupported,
        Status::ChangedDuringRead => Availability::ChangedDuringRead,
    };
    let asset = (document.status == Status::Captured)
        .then(|| utf8(&document.bytes))
        .flatten()
        .and_then(|bytes| ProjectionAsset::new(HTML_MEDIA_TYPE, bytes));
    if availability == Availability::Captured && asset.is_none() {
        availability = Availability::Unsupported;
    }
    let reference = asset.as_ref().map(ProjectionAsset::reference);
    if let Some(asset) = asset {
        ops.push(IngestOp::ProjectionAsset { asset });
    }
    ops.push(IngestOp::HtmlArtifactContent(HtmlArtifactContent {
        session_id: session_id.to_owned(),
        source_offset: offset,
        path: Some(document.path.clone()),
        availability,
        captured_at: Some(document.captured_at),
        recovered_from: document.recovered_from.clone(),
        asset: reference,
    }));
}
