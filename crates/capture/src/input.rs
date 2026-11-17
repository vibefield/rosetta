//! Read-only physical representations of a logical decoded transcript.
//!
//! A compressed input is verified to EOF into an anonymous seekable spool
//! once per pass, never once per tail batch. Native files are never restored,
//! renamed or locked. All ranges, limits and cursors refer to decoded bytes.

use std::fs::File;
use std::io::{Read, Seek, Write};
use std::path::{Component, Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};

use crate::tailer::{FileIdentity, file_identity};
use crate::{CaptureError, io_error};

const BUFFER_BYTES: usize = 128 * 1024;
/// A bad frame cannot consume unbounded scratch space. This is a decoded
/// per-source limit, independent of the normal per-record framing limit.
pub const MAX_DECODED_BYTES: u64 = 64 * 1024 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InputEncoding {
    Plain,
    Zstd,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputCandidate {
    pub path: String,
    pub encoding: InputEncoding,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputSegment {
    /// Preference order; a present but invalid preferred file never falls
    /// back to an older representation.
    pub candidates: Vec<InputCandidate>,
    pub start: u64,
    pub end: Option<u64>,
    /// Bounded header evidence protects a planned lineage from migration
    /// between metadata admission and capture.
    pub header_hash: Option<[u8; 32]>,
    /// Optional native record-index boundary, interpreted by the adapter.
    pub end_record_index: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputSource {
    /// Exact bounded native header, when a selected view must configure its
    /// normalizer before replaying ancestors. Validated against the selected
    /// segment's header before this spool can be published.
    pub prefix: Vec<u8>,
    pub segments: Vec<InputSegment>,
}

impl InputSource {
    pub fn single(candidates: Vec<InputCandidate>) -> Self {
        Self {
            prefix: Vec::new(),
            segments: vec![InputSegment {
                candidates,
                start: 0,
                end: None,
                header_hash: None,
                end_record_index: None,
            }],
        }
    }

    pub fn headers_match(&self, root: &Path) -> Result<bool, CaptureError> {
        for segment in &self.segments {
            if let Some(expected) = segment.header_hash {
                let source = Self::single(segment.candidates.clone());
                if source
                    .head(root, 32 * 1024 * 1024)?
                    .is_none_or(|head| *blake3::hash(&head.bytes).as_bytes() != expected)
                {
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }

    pub fn claims(&self, hint: &str) -> bool {
        self.segments.iter().any(|segment| {
            segment.candidates.iter().any(|candidate| {
                candidate.path == hint
                    || candidate
                        .path
                        .strip_prefix(hint)
                        .is_some_and(|s| s.starts_with('/'))
            })
        })
    }

    /// Content-free gate. Includes inode, size, nanosecond mtime and ctime,
    /// plus the range plan, so relocation and timestamp-preserving atomic
    /// replacement nominate verification without decoding quiet sources.
    pub fn stamp(&self, root: &Path) -> Result<Option<String>, CaptureError> {
        if self.segments.is_empty() {
            return Ok(None);
        }
        let mut hash = blake3::Hasher::new();
        hash.update(&serde_json::to_vec(&self.segments).expect("input plan serializes"));
        hash.update(&self.prefix);
        for segment in &self.segments {
            let Some((candidate, path, file)) = open_candidate(root, &segment.candidates)? else {
                return Ok(None);
            };
            hash_metadata(&mut hash, candidate, &path, &file)?;
        }
        Ok(Some(hash.finalize().to_hex().to_string()))
    }

    /// Read only the first complete record, with a hard decoded bound. No
    /// content-size header is assumed: old and concatenated frames work too.
    pub fn head(&self, root: &Path, limit: usize) -> Result<Option<InputHead>, CaptureError> {
        let Some(segment) = self.segments.first() else {
            return Ok(None);
        };
        let Some((candidate, path, file)) = open_candidate(root, &segment.candidates)? else {
            return Ok(None);
        };
        let metadata = file
            .metadata()
            .map_err(|e| io_error("reading input metadata", &path, e))?;
        let len = (candidate.encoding == InputEncoding::Plain).then_some(metadata.len());
        let modified_ms = metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |t| t.as_millis() as i64);
        let reader = decoder(file, candidate.encoding, &path)?;
        let mut reader = std::io::BufReader::new(reader.take(limit as u64 + 1));
        let mut bytes = Vec::new();
        std::io::BufRead::read_until(&mut reader, b'\n', &mut bytes).map_err(|e| {
            decode_error("reading decoded input header", &path, candidate.encoding, e)
        })?;
        if bytes.len() > limit {
            return Err(CaptureError::LimitExceeded(
                "decoded transcript header".into(),
            ));
        }
        Ok(Some(InputHead {
            bytes,
            len,
            modified_ms,
        }))
    }

    pub fn open(
        &self,
        root: &Path,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<OpenedInput>, CaptureError> {
        if self.segments.is_empty() {
            return Ok(None);
        }
        let mut opened = Vec::new();
        let mut stamp = blake3::Hasher::new();
        stamp.update(&serde_json::to_vec(&self.segments).expect("input plan serializes"));
        stamp.update(&self.prefix);
        for segment in &self.segments {
            if cancelled() {
                return Err(cancel_error(root));
            }
            let Some((candidate, path, file)) = open_candidate(root, &segment.candidates)? else {
                return Ok(None);
            };
            hash_metadata(&mut stamp, candidate, &path, &file)?;
            opened.push((segment, candidate, path, file));
        }
        let stamp = stamp.finalize().to_hex().to_string();
        // Preserve old inode checkpoints for an ordinary uncompressed file.
        if let [(segment, candidate, path, file)] = opened.as_slice()
            && candidate.encoding == InputEncoding::Plain
            && self.prefix.is_empty()
            && segment.start == 0
            && segment.end.is_none()
        {
            let identity = file_identity(
                path,
                &file
                    .metadata()
                    .map_err(|e| io_error("reading input metadata", path, e))?,
            );
            let expected = segment.header_hash;
            let (_, _, path, mut file) = opened.pop().expect("one input");
            if let Some(expected) = expected {
                let mut header = Vec::new();
                let mut reader = std::io::BufReader::new((&mut file).take(32 * 1024 * 1024 + 1));
                std::io::BufRead::read_until(&mut reader, b'\n', &mut header)
                    .map_err(|e| io_error("verifying pinned header", &path, e))?;
                if *blake3::hash(&header).as_bytes() != expected {
                    return Err(invalid(&path, "transcript header changed during capture"));
                }
                file.rewind()
                    .map_err(|e| io_error("rewinding transcript", &path, e))?;
            }
            return Ok(Some(OpenedInput {
                file,
                path,
                identity,
                stamp,
                boundaries: Vec::new(),
                cancel: None,
            }));
        }
        let mut spool =
            tempfile::tempfile().map_err(|e| io_error("creating decoded spool", root, e))?;
        if self.prefix.len() > 32 * 1024 * 1024 {
            return Err(CaptureError::LimitExceeded(
                "transcript prefix exceeds 32 MiB".into(),
            ));
        }
        spool
            .write_all(&self.prefix)
            .map_err(|e| io_error("writing selected header", root, e))?;
        let mut total = self.prefix.len() as u64;
        let mut boundaries = Vec::new();
        for (segment, candidate, path, file) in opened {
            let monitor = file
                .try_clone()
                .map_err(|e| io_error("pinning transcript", &path, e))?;
            let before = monitor
                .metadata()
                .map_err(|e| io_error("reading input metadata", &path, e))?;
            let mut reader = decoder(file, candidate.encoding, &path)?;
            let mut boundary_record = Vec::new();
            let mut buffer = vec![0; BUFFER_BYTES];
            let mut offset = 0_u64;
            let mut last = None;
            let mut header = blake3::Hasher::new();
            let mut in_header = true;
            let mut header_len = 0_usize;
            loop {
                if cancelled() {
                    return Err(cancel_error(&path));
                }
                let count = reader.read(&mut buffer).map_err(|e| {
                    decode_error("decoding transcript", &path, candidate.encoding, e)
                })?;
                if count == 0 {
                    break;
                }
                let next = offset
                    .checked_add(count as u64)
                    .ok_or_else(|| CaptureError::LimitExceeded("decoded offset overflow".into()))?;
                if next > MAX_DECODED_BYTES {
                    return Err(CaptureError::LimitExceeded(
                        "decoded transcript exceeds 64 GiB".into(),
                    ));
                }
                if in_header && segment.header_hash.is_some() {
                    let end = buffer[..count]
                        .iter()
                        .position(|b| *b == b'\n')
                        .map(|i| i + 1);
                    let header_bytes = &buffer[..end.unwrap_or(count)];
                    header_len += header_bytes.len();
                    if header_len > 32 * 1024 * 1024 {
                        return Err(CaptureError::LimitExceeded(
                            "decoded transcript header".into(),
                        ));
                    }
                    header.update(header_bytes);
                    if end.is_some() {
                        in_header = false;
                        if Some(*header.finalize().as_bytes()) != segment.header_hash {
                            return Err(invalid(&path, "transcript header changed during capture"));
                        }
                    }
                }
                let start = offset.max(segment.start);
                let end = next.min(segment.end.unwrap_or(u64::MAX));
                if start < end {
                    let bytes = &buffer[(start - offset) as usize..(end - offset) as usize];
                    total = total.checked_add(bytes.len() as u64).ok_or_else(|| {
                        CaptureError::LimitExceeded("decoded source overflow".into())
                    })?;
                    if total > MAX_DECODED_BYTES {
                        return Err(CaptureError::LimitExceeded(
                            "composed transcript exceeds 64 GiB".into(),
                        ));
                    }
                    spool
                        .write_all(bytes)
                        .map_err(|e| io_error("writing decoded spool", &path, e))?;
                    last = bytes.last().copied();
                    if segment.end_record_index.is_some() {
                        // Keep only the final complete record of this fixed
                        // range, bounded like every projected native record.
                        for chunk in bytes.split_inclusive(|b| *b == b'\n') {
                            if boundary_record.ends_with(b"\n") {
                                boundary_record.clear();
                            }
                            if boundary_record.len() + chunk.len() > 32 * 1024 * 1024 {
                                return Err(CaptureError::LimitExceeded(
                                    "referenced boundary record exceeds 32 MiB".into(),
                                ));
                            }
                            boundary_record.extend_from_slice(chunk);
                        }
                    }
                }
                offset = next;
                // Bounded plain prefixes need not read a parent's later
                // growth. Zstd is always drained to validate its checksum.
                if candidate.encoding == InputEncoding::Plain
                    && segment.end.is_some_and(|end| offset >= end)
                {
                    break;
                }
            }
            let after = monitor
                .metadata()
                .map_err(|e| io_error("rechecking decoded input", &path, e))?;
            if after.len() < before.len()
                || (after.len() == before.len() && after.modified().ok() != before.modified().ok())
                || (candidate.encoding == InputEncoding::Zstd && after.len() != before.len())
            {
                return Err(invalid(&path, "transcript changed while decoding"));
            }
            if let Some(expected) = segment.end_record_index {
                boundaries.push((expected, boundary_record));
            }
            if offset < segment.start || segment.end.is_some_and(|end| offset < end) {
                return Err(invalid(&path, "referenced decoded range is truncated"));
            }
            if segment.end.is_some() && last != Some(b'\n') && segment.end != Some(segment.start) {
                return Err(invalid(
                    &path,
                    "referenced range does not end at a complete record",
                ));
            }
            if segment.header_hash.is_some() && in_header {
                return Err(invalid(&path, "referenced header is incomplete"));
            }
        }
        spool
            .rewind()
            .map_err(|e| io_error("rewinding decoded spool", root, e))?;
        // The temporary inode is never durable identity. A different physical
        // stamp asks the tailer to verify the committed decoded prefix.
        Ok(Some(OpenedInput {
            file: spool,
            path: root.to_path_buf(),
            identity: FileIdentity::Path(stamp.as_bytes().to_vec()),
            stamp,
            boundaries,
            cancel: None,
        }))
    }
}

pub struct InputHead {
    pub bytes: Vec<u8>,
    pub len: Option<u64>,
    pub modified_ms: i64,
}

#[derive(Debug)]
pub struct OpenedInput {
    pub file: File,
    pub path: PathBuf,
    pub identity: FileIdentity,
    pub stamp: String,
    pub boundaries: Vec<(u64, Vec<u8>)>,
    pub cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}

fn decoder(
    file: File,
    encoding: InputEncoding,
    path: &Path,
) -> Result<Box<dyn Read>, CaptureError> {
    match encoding {
        InputEncoding::Plain => Ok(Box::new(file)),
        InputEncoding::Zstd => zstd::stream::read::Decoder::new(file)
            .map(|r| Box::new(r) as Box<dyn Read>)
            .map_err(|e| decode_error("opening zstd transcript", path, InputEncoding::Zstd, e)),
    }
}

fn open_candidate<'a>(
    root: &Path,
    candidates: &'a [InputCandidate],
) -> Result<Option<(&'a InputCandidate, PathBuf, File)>, CaptureError> {
    for candidate in candidates {
        let relative = Path::new(&candidate.path);
        if relative.is_absolute()
            || relative
                .components()
                .any(|c| !matches!(c, Component::Normal(_)))
        {
            return Err(CaptureError::InvalidConfig(
                "input candidate must be root-relative".into(),
            ));
        }
        let path = root.join(relative);
        match File::open(&path) {
            Ok(file) => return Ok(Some((candidate, path, file))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(io_error("opening transcript representation", &path, e)),
        }
    }
    Ok(None)
}

fn hash_metadata(
    hash: &mut blake3::Hasher,
    candidate: &InputCandidate,
    path: &Path,
    file: &File,
) -> Result<(), CaptureError> {
    let metadata = file
        .metadata()
        .map_err(|e| io_error("reading input metadata", path, e))?;
    hash.update(candidate.path.as_bytes());
    hash.update(
        format!(
            "{:?}:{}:{:?}",
            file_identity(path, &metadata),
            metadata.len(),
            metadata.modified().ok()
        )
        .as_bytes(),
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        hash.update(&metadata.ctime().to_le_bytes());
        hash.update(&metadata.ctime_nsec().to_le_bytes());
    }
    Ok(())
}

fn invalid(path: &Path, message: &str) -> CaptureError {
    io_error(
        "validating decoded transcript",
        path,
        std::io::Error::new(std::io::ErrorKind::InvalidData, message),
    )
}
fn cancel_error(path: &Path) -> CaptureError {
    io_error(
        "decoding transcript",
        path,
        std::io::Error::new(std::io::ErrorKind::Interrupted, "capture cancelled"),
    )
}

// zstd reports malformed frame/checksum failures as ErrorKind::Other with
// no OS errno. Classify these as data faults, while retaining filesystem IO
// errors (permission, storage failure, etc.) as actual operational failures.
fn decode_error(
    operation: &'static str,
    path: &Path,
    encoding: InputEncoding,
    error: std::io::Error,
) -> CaptureError {
    let error = if encoding == InputEncoding::Zstd
        && error.kind() == std::io::ErrorKind::Other
        && error.raw_os_error().is_none()
    {
        std::io::Error::new(std::io::ErrorKind::InvalidData, error)
    } else {
        error
    };
    io_error(operation, path, error)
}
