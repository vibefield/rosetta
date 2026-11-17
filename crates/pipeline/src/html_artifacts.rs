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

pub(crate) fn now() -> i64 {
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
        media: None,
        bytes: Vec::new(),
    };
    let file = match open_regular(&document.path) {
        Ok(file) => file,
        Err(status) => {
            document.status = status;
            return document;
        }
    };
    document.status = read_stable(
        &mut &file,
        || FileStat::of(&file),
        HTML_MAX_BYTES as u64,
        None,
        &mut document.bytes,
    );
    if document.status == Status::Captured && utf8(&document.bytes).is_none() {
        document.status = Status::Unsupported;
    }
    if document.status != Status::Captured {
        document.bytes.clear();
    }
    document
}

/// Whether an absolute path is the device or process tree, or lies under
/// one. Letter case is ignored because the default macOS volume resolves
/// `/DEV` to `/dev`.
fn is_device_path(path: &str) -> bool {
    let root = path
        .strip_prefix('/')
        .and_then(|rest| rest.split('/').next())
        .unwrap_or_default();
    root.eq_ignore_ascii_case("dev") || root.eq_ignore_ascii_case("proc")
}

/// Open the file a transcript names, for one bounded observation. `Err` is
/// the status the observation ends with: `Missing` when nothing is there,
/// `Unsupported` for a path that is not absolute (or is a network path), a
/// device or process path, any other open error, and anything that is not a
/// regular file.
pub(crate) fn open_regular(path: &str) -> Result<File, Status> {
    if !Path::new(path).is_absolute() || path.starts_with("//") {
        return Err(Status::Unsupported);
    }
    if is_device_path(path) {
        // Refused before any open: opening one can act on this process.
        // `/dev/fd/N` duplicates its own descriptor N, which then reports
        // the regular file behind it and shares its read offset, and a
        // character device can act on being opened at all.
        return Err(Status::Unsupported);
    }
    // The same refusal for a path that only reaches those trees through a
    // symlinked directory: `O_NOFOLLOW` guards the last component alone, so
    // the directory is resolved first. A directory that does not resolve is
    // left to the open below, which reports what is wrong with it.
    if let Some(parent) = Path::new(path).parent()
        && let Ok(resolved) = std::fs::canonicalize(parent)
        && is_device_path(&resolved.to_string_lossy())
    {
        return Err(Status::Unsupported);
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
    let file = options.open(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            Status::Missing
        } else {
            Status::Unsupported
        }
    })?;
    match file.metadata() {
        Ok(metadata) if metadata.is_file() => Ok(file),
        _ => Err(Status::Unsupported),
    }
}

/// What a stable read compares before and after it reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FileStat {
    pub(crate) is_file: bool,
    pub(crate) len: u64,
    pub(crate) modified: Option<SystemTime>,
}

impl FileStat {
    pub(crate) fn of(file: &File) -> Option<Self> {
        let metadata = file.metadata().ok()?;
        Some(Self {
            is_file: metadata.is_file(),
            len: metadata.len(),
            modified: metadata.modified().ok(),
        })
    }
}

/// Read a whole file of at most `limit` bytes into `bytes`, and say whether
/// it held still: `Captured` only when the length and modification time
/// `stat` reports are the same before and after the read and the read
/// returned exactly that many bytes.
///
/// `expected_len` is the size a transcript recorded for the file. With one,
/// a file of any other length is `ChangedDuringRead` before a byte is read,
/// the buffer is reserved up front, and a short or long read is
/// `ChangedDuringRead` too. Without one, a file over `limit` is `TooLarge`.
/// The reader and `stat` are separate so a test can move the file under the
/// read; the caller clears `bytes` on any status but `Captured`.
pub(crate) fn read_stable<R: Read>(
    reader: &mut R,
    mut stat: impl FnMut() -> Option<FileStat>,
    limit: u64,
    expected_len: Option<u64>,
    bytes: &mut Vec<u8>,
) -> Status {
    let Some(before) = stat() else {
        return Status::Unsupported;
    };
    if !before.is_file {
        return Status::Unsupported;
    }
    if expected_len.is_some_and(|expected| before.len != expected) {
        return Status::ChangedDuringRead;
    }
    if before.len > limit {
        return Status::TooLarge;
    }
    if let Some(expected) = expected_len {
        // One allocation of the recorded size, refused instead of aborting
        // the daemon when the memory is not there.
        let reserved = usize::try_from(expected)
            .ok()
            .is_some_and(|len| bytes.try_reserve_exact(len).is_ok());
        if !reserved {
            return Status::Unsupported;
        }
    }
    if reader
        .take(limit.saturating_add(1))
        .read_to_end(bytes)
        .is_err()
    {
        return Status::Unsupported;
    }
    let read = bytes.len() as u64;
    if expected_len.is_some_and(|expected| read != expected) {
        return Status::ChangedDuringRead;
    }
    if read > limit {
        return Status::TooLarge;
    }
    let Some(after) = stat() else {
        return Status::ChangedDuringRead;
    };
    if before.len != after.len || after.len != read || before.modified != after.modified {
        return Status::ChangedDuringRead;
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
