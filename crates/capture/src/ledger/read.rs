//! The raw ledger's read side: every consumer walks segments through
//! [`visit_segments`], whose frame loop resolves each raw, batch and seal frame
//! through the defines of its own segment. A torn or corrupt frame ends only
//! that segment's walk.

use std::collections::HashMap;
use std::io::{ErrorKind, Read};
use std::path::Path;

use super::{
    CHECKSUM_BYTES, LedgerEntry, MAX_FRAME_BYTES, SourceDef, TAG_DEFINE, TAG_GENERATION_COMPLETE,
    TAG_META, TAG_META_VERSIONED, TAG_RAW, TAG_RAW_BATCH, segment_files,
};
use crate::segment::SegmentReader;
use crate::{CaptureError, io_error};

/// Scan every entry across all segments in order. Torn or corrupt tails end
/// their segment silently (`report.torn_segments` says how many); corruption
/// BEFORE the end is still just a stop — the ledger is append-only, so
/// everything prior remains trustworthy.
pub struct LedgerScan {
    pub entries: Vec<LedgerEntry>,
    pub segments: usize,
    pub torn_segments: usize,
}

pub fn scan(dir: &Path) -> Result<LedgerScan, CaptureError> {
    let mut entries = Vec::new();
    let report = visit::<CaptureError, _>(dir, |entry| {
        entries.push(entry);
        Ok(())
    })?;
    Ok(LedgerScan {
        entries,
        segments: report.segments,
        torn_segments: report.torn_segments,
    })
}

/// Aggregate-free scan report. [`visit`] retains at most one frame body and
/// one buffered filesystem page, so rebuild memory is independent of ledger
/// lifetime and segment size.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LedgerVisit {
    pub segments: usize,
    pub torn_segments: usize,
    pub entries: u64,
}

/// Visit every ledger entry in segment order without retaining payloads.
/// Torn/corrupt tails stop only their segment; callback errors are propagated.
pub fn visit<E, F>(dir: &Path, visitor: F) -> Result<LedgerVisit, E>
where
    E: From<CaptureError>,
    F: FnMut(LedgerEntry) -> Result<(), E>,
{
    visit_segments(dir, |_| true, visitor)
}

/// [`visit`] restricted to the segments `keep` admits by sequence number,
/// still in ascending order. `report.segments` counts only the visited ones.
pub fn visit_segments<E, F>(
    dir: &Path,
    keep: impl Fn(u64) -> bool,
    mut visitor: F,
) -> Result<LedgerVisit, E>
where
    E: From<CaptureError>,
    F: FnMut(LedgerEntry) -> Result<(), E>,
{
    let mut report = LedgerVisit::default();
    for (seq, path) in segment_files(dir).map_err(E::from)? {
        if !keep(seq) {
            continue;
        }
        report.segments += 1;
        let mut counted = |entry| {
            report.entries += 1;
            visitor(entry)
        };
        if !scan_segment(&path, &mut counted)? {
            report.torn_segments += 1;
        }
    }
    Ok(report)
}

/// Returns false if the segment ended in a torn/corrupt frame.
pub(crate) fn scan_segment<E, F>(path: &Path, visitor: &mut F) -> Result<bool, E>
where
    E: From<CaptureError>,
    F: FnMut(LedgerEntry) -> Result<(), E>,
{
    let Some(mut reader) = SegmentReader::open(path)
        .map_err(|error| E::from(io_error("opening segment", path, error)))?
    else {
        return Ok(false);
    };
    let clean = scan_frames(&mut reader, path, visitor)?;
    Ok(clean && !reader.corrupt())
}

/// What a frame walk finds before the first torn or corrupt frame: how many
/// container-decoded bytes were intact, whether the walk ended cleanly on a
/// frame boundary, and a digest of those bytes — segment recompression proves
/// its copy against it.
pub(crate) struct FramePrefix {
    pub bytes: u64,
    pub clean: bool,
    pub digest: blake3::Hash,
}

pub(crate) fn frame_prefix(reader: &mut impl Read) -> std::io::Result<FramePrefix> {
    let mut digest = blake3::Hasher::new();
    let mut bytes = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    let torn = |bytes, digest: blake3::Hasher| FramePrefix {
        bytes,
        clean: false,
        digest: digest.finalize(),
    };
    loop {
        let mut head = [0_u8; 5];
        if reader.read(&mut head[..1])? == 0 {
            return Ok(FramePrefix {
                bytes,
                clean: true,
                digest: digest.finalize(),
            });
        }
        if !read_exact_or_short(reader, &mut head[1..])? {
            return Ok(torn(bytes, digest));
        }
        let body_len = u32::from_be_bytes(head[1..5].try_into().expect("4 bytes")) as usize;
        let known = matches!(
            head[0],
            TAG_DEFINE
                | TAG_RAW
                | TAG_META
                | TAG_GENERATION_COMPLETE
                | TAG_META_VERSIONED
                | TAG_RAW_BATCH
        );
        if !known || body_len > MAX_FRAME_BYTES {
            return Ok(torn(bytes, digest));
        }
        let mut hasher = blake3::Hasher::new();
        hasher.update(&head);
        let mut frame_digest = digest.clone();
        frame_digest.update(&head);
        let mut remaining = body_len;
        while remaining != 0 {
            let length = remaining.min(buffer.len());
            if !read_exact_or_short(reader, &mut buffer[..length])? {
                return Ok(torn(bytes, digest));
            }
            hasher.update(&buffer[..length]);
            frame_digest.update(&buffer[..length]);
            remaining -= length;
        }
        let mut checksum = [0_u8; CHECKSUM_BYTES];
        if !read_exact_or_short(reader, &mut checksum)? {
            return Ok(torn(bytes, digest));
        }
        if hasher.finalize().as_bytes()[..CHECKSUM_BYTES] != checksum {
            return Ok(torn(bytes, digest));
        }
        frame_digest.update(&checksum);
        digest = frame_digest;
        bytes += (head.len() + body_len + CHECKSUM_BYTES) as u64;
    }
}

fn read_exact_or_short(reader: &mut impl Read, buffer: &mut [u8]) -> std::io::Result<bool> {
    match reader.read_exact(buffer) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == ErrorKind::UnexpectedEof => Ok(false),
        Err(error) => Err(error),
    }
}

/// The frame loop over any container. Returns false at a torn or corrupt
/// frame; the caller folds in the container's own verdict.
fn scan_frames<E, F>(reader: &mut impl Read, path: &Path, visitor: &mut F) -> Result<bool, E>
where
    E: From<CaptureError>,
    F: FnMut(LedgerEntry) -> Result<(), E>,
{
    let mut defined: HashMap<u32, SourceDef> = HashMap::new();
    loop {
        let mut head = [0_u8; 5];
        match reader.read(&mut head[..1]) {
            Ok(0) => return Ok(true),
            Ok(1) => {}
            Ok(_) => unreachable!("one-byte read returned more than one byte"),
            Err(error) => {
                return Err(E::from(io_error("reading segment", path, error)));
            }
        }
        if !read_frame_part(reader, &mut head[1..], path)? {
            return Ok(false);
        }
        let tag = head[0];
        let body_len = u32::from_be_bytes(head[1..5].try_into().expect("4 bytes")) as usize;
        if body_len > MAX_FRAME_BYTES {
            return Ok(false);
        }
        let mut body = vec![0_u8; body_len];
        if !read_frame_part(reader, &mut body, path)? {
            return Ok(false);
        }
        let mut checksum = [0_u8; CHECKSUM_BYTES];
        if !read_frame_part(reader, &mut checksum, path)? {
            return Ok(false);
        }
        let mut hasher = blake3::Hasher::new();
        hasher.update(&head);
        hasher.update(&body);
        if hasher.finalize().as_bytes()[..CHECKSUM_BYTES] != checksum {
            return Ok(false);
        }
        match tag {
            TAG_DEFINE => {
                let Some((id, source)) = decode_define(&body) else {
                    return Ok(false);
                };
                defined.insert(id, source);
            }
            TAG_RAW => {
                if body_len < 20 {
                    return Ok(false);
                }
                let file_id = u32::from_be_bytes(body[0..4].try_into().expect("4 bytes"));
                let generation = u64::from_be_bytes(body[4..12].try_into().expect("8 bytes"));
                let start_offset = u64::from_be_bytes(body[12..20].try_into().expect("8 bytes"));
                let Some(source) = defined.get(&file_id) else {
                    // A raw record before its define: not a valid segment.
                    return Ok(false);
                };
                visitor(LedgerEntry::Raw {
                    source: source.clone(),
                    generation,
                    start_offset,
                    payload: body[20..].to_vec(),
                })?;
            }
            TAG_RAW_BATCH => {
                if body_len < 16 {
                    return Ok(false);
                }
                let file_id = u32::from_be_bytes(body[0..4].try_into().expect("4 bytes"));
                let generation = u64::from_be_bytes(body[4..12].try_into().expect("8 bytes"));
                let count = u32::from_be_bytes(body[12..16].try_into().expect("4 bytes"));
                if count == 0 {
                    return Ok(false);
                }
                let Some(source) = defined.get(&file_id) else {
                    return Ok(false);
                };
                let mut at = 16_usize;
                for _ in 0..count {
                    let Some(header_end) = at.checked_add(12) else {
                        return Ok(false);
                    };
                    if header_end > body_len {
                        return Ok(false);
                    }
                    let start_offset =
                        u64::from_be_bytes(body[at..at + 8].try_into().expect("8 bytes"));
                    let payload_len =
                        u32::from_be_bytes(body[at + 8..header_end].try_into().expect("4 bytes"))
                            as usize;
                    let Some(payload_end) = header_end.checked_add(payload_len) else {
                        return Ok(false);
                    };
                    if payload_end > body_len {
                        return Ok(false);
                    }
                    visitor(LedgerEntry::Raw {
                        source: source.clone(),
                        generation,
                        start_offset,
                        payload: body[header_end..payload_end].to_vec(),
                    })?;
                    at = payload_end;
                }
                if at != body_len {
                    return Ok(false);
                }
            }
            TAG_GENERATION_COMPLETE => {
                if body_len != 17 {
                    return Ok(false);
                }
                let file_id = u32::from_be_bytes(body[0..4].try_into().expect("4 bytes"));
                let generation = u64::from_be_bytes(body[4..12].try_into().expect("8 bytes"));
                let normalizer_version =
                    u32::from_be_bytes(body[12..16].try_into().expect("4 bytes"));
                let normalized_clean = match body[16] {
                    0 => false,
                    1 => true,
                    _ => return Ok(false),
                };
                let Some(source) = defined.get(&file_id) else {
                    return Ok(false);
                };
                visitor(LedgerEntry::GenerationComplete {
                    source: source.clone(),
                    generation,
                    normalizer_version,
                    normalized_clean,
                })?;
            }
            TAG_META => {
                let Ok(text) = std::str::from_utf8(&body) else {
                    return Ok(false);
                };
                visitor(LedgerEntry::Meta {
                    schema_version: 0,
                    payload: text.to_string(),
                })?;
            }
            TAG_META_VERSIONED => {
                if body_len < 4 {
                    return Ok(false);
                }
                let schema_version = u32::from_be_bytes(body[0..4].try_into().expect("4 bytes"));
                let Ok(text) = std::str::from_utf8(&body[4..]) else {
                    return Ok(false);
                };
                visitor(LedgerEntry::Meta {
                    schema_version,
                    payload: text.to_string(),
                })?;
            }
            _ => return Ok(false),
        }
    }
}

fn read_frame_part<E: From<CaptureError>>(
    reader: &mut impl Read,
    buffer: &mut [u8],
    path: &Path,
) -> Result<bool, E> {
    match reader.read_exact(buffer) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == ErrorKind::UnexpectedEof => Ok(false),
        Err(error) => Err(E::from(io_error("reading segment", path, error))),
    }
}

pub(super) fn decode_define(body: &[u8]) -> Option<(u32, SourceDef)> {
    let text = std::str::from_utf8(body).ok()?;
    let get = |key: &str| -> Option<String> {
        let needle = format!("\"{key}\":\"");
        let start = text.find(&needle)? + needle.len();
        let mut out = String::new();
        let mut chars = text[start..].chars();
        loop {
            match chars.next()? {
                '"' => break,
                '\\' => match chars.next()? {
                    '"' => out.push('"'),
                    '\\' => out.push('\\'),
                    'n' => out.push('\n'),
                    'u' => {
                        let code: String = (0..4).filter_map(|_| chars.next()).collect();
                        out.push(char::from_u32(u32::from_str_radix(&code, 16).ok()?)?);
                    }
                    other => out.push(other),
                },
                ch => out.push(ch),
            }
        }
        Some(out)
    };
    let file_id = get("file_id")?.parse().ok()?;
    let agent_version = get("agent_version")?;
    Some((
        file_id,
        SourceDef {
            ledger_only: match get("ledger_only").as_deref() {
                None | Some("false") => false,
                Some("true") => true,
                _ => return None,
            },
            relative_path: get("relative_path")?,
            stream_id: get("stream_id")?,
            session_id: get("session_id")?,
            native_session_id: get("native_session_id")?,
            agent_version: (!agent_version.is_empty()).then_some(agent_version),
        },
    ))
}
