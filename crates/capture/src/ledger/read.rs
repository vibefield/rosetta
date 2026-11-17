//! The raw ledger's read side: every consumer walks segments through
//! [`visit_segments`], whose frame loop resolves each raw, batch, document and
//! seal frame through the defines of its own segment. A torn or corrupt frame
//! ends only that segment's walk.

use std::collections::HashMap;
use std::io::{ErrorKind, Read};
use std::path::Path;

use super::{
    CHECKSUM_BYTES, CapturedDocument, LedgerEntry, MAX_FRAME_BYTES, SourceDef, TAG_DEFINE,
    TAG_DOCUMENT, TAG_GENERATION_COMPLETE, TAG_META, TAG_META_VERSIONED, TAG_RAW, TAG_RAW_BATCH,
    segment_files,
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

/// Every frame tag a segment may hold: exactly the tags `scan_frames`
/// decodes. The prefix walk stops at any other, so a tag missing here makes
/// every segment holding such a frame read as torn — never verified clean,
/// never compacted or recompressed.
const fn is_frame_tag(tag: u8) -> bool {
    matches!(
        tag,
        TAG_DEFINE
            | TAG_RAW
            | TAG_META
            | TAG_GENERATION_COMPLETE
            | TAG_META_VERSIONED
            | TAG_RAW_BATCH
            | TAG_DOCUMENT
    )
}

/// Walk frames by head, length and checksum alone, in bounded memory whatever
/// a frame holds (a document frame may carry a whole captured video). Bodies
/// are not decoded: a checksum-valid frame `scan_frames` would reject still
/// counts here, and the copy recompression proves against this walk keeps it
/// byte for byte.
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
        if !is_frame_tag(head[0]) || body_len > MAX_FRAME_BYTES {
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
            TAG_RAW | TAG_DOCUMENT => {
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
                if tag == TAG_DOCUMENT {
                    let Some(document) = CapturedDocument::decode(&body[20..]) else {
                        return Ok(false);
                    };
                    visitor(LedgerEntry::Document {
                        source: source.clone(),
                        generation,
                        start_offset,
                        document,
                    })?;
                } else {
                    visitor(LedgerEntry::Raw {
                        source: source.clone(),
                        generation,
                        start_offset,
                        payload: body[20..].to_vec(),
                    })?;
                }
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

/// Document frames on the read side, and the one fixture segment the verify
/// tests share with these.
#[cfg(test)]
pub(super) mod tests {
    use std::path::PathBuf;

    use tempfile::TempDir;

    use super::*;
    use crate::ledger::{DocumentCaptureStatus, Ledger, MediaClaim, scan};
    use crate::segment::{MAGIC_PLAIN, SegmentCodec};
    use crate::{RecompressOutcome, recompress_segment};

    pub(in crate::ledger) const STATUSES: [DocumentCaptureStatus; 5] = [
        DocumentCaptureStatus::Captured,
        DocumentCaptureStatus::Missing,
        DocumentCaptureStatus::TooLarge,
        DocumentCaptureStatus::Unsupported,
        DocumentCaptureStatus::ChangedDuringRead,
    ];

    const RECORD: &[u8] = b"{\"type\":\"assistant\",\"text\":\"a synthetic line\"}\n";
    /// The fixture segment's three raw records, in payload bytes.
    pub(in crate::ledger) const RAW_PAYLOAD_BYTES: u64 = 3 * RECORD.len() as u64;
    /// One captured media frame's bytes: long enough to cross the prefix
    /// walk's 64 KiB buffer several times.
    pub(in crate::ledger) const CLIP_BYTES: u64 = 200_000;
    pub(in crate::ledger) const HTML_BYTES: u64 = 15;

    pub(in crate::ledger) fn html(recovered_from: Option<&str>) -> CapturedDocument {
        CapturedDocument {
            path: "/fixture/review.html".to_string(),
            captured_at: 1_790_000_000_000,
            status: DocumentCaptureStatus::Captured,
            recovered_from: recovered_from.map(str::to_string),
            media: None,
            bytes: b"<h1>Review</h1>".to_vec(),
        }
    }

    /// A media frame as capture writes one: bytes only when captured, and
    /// then exactly the claimed size. The bytes are synthetic; no file is read.
    pub(in crate::ledger) fn media(
        status: DocumentCaptureStatus,
        media_type: &str,
        expected_len: u64,
    ) -> CapturedDocument {
        let bytes = if status == DocumentCaptureStatus::Captured {
            (0..expected_len as u32)
                .map(|index| (index.wrapping_mul(2_654_435_761) >> 24) as u8)
                .collect()
        } else {
            Vec::new()
        };
        CapturedDocument {
            path: "/fixture/out/clip.mp4".to_string(),
            captured_at: 1_790_000_000_123,
            status,
            recovered_from: None,
            media: Some(MediaClaim {
                media_type: media_type.to_string(),
                expected_len,
            }),
            bytes,
        }
    }

    /// One segment of 14 entries: a meta batch, then one source's raw
    /// records, HTML documents in both metadata forms, a media frame of every
    /// status, its seal, and a second generation that carries the capture
    /// forward.
    pub(in crate::ledger) fn write_document_segment(dir: &Path, codec: SegmentCodec) -> PathBuf {
        let source = SourceDef {
            ledger_only: false,
            relative_path: "a.jsonl".to_string(),
            stream_id: "transcript".to_string(),
            session_id: "session:a".to_string(),
            native_session_id: "native:a".to_string(),
            agent_version: Some("2.0".to_string()),
        };
        let mut ledger = Ledger::open(dir).unwrap().with_codec(codec);
        ledger.append_meta(7, "[{\"op\":\"runtime\"}]").unwrap();
        ledger.append_raw(&source, 1, [(0, RECORD)]).unwrap();
        ledger.append_document(&source, 1, 0, html(None)).unwrap();
        ledger
            .append_document(&source, 1, 0, html(Some("/archive/review.html")))
            .unwrap();
        ledger.append_raw(&source, 1, [(100, RECORD)]).unwrap();
        for (status, media_type, expected_len) in [
            (STATUSES[0], "video/mp4", CLIP_BYTES),
            (STATUSES[1], "video/quicktime", 77),
            (STATUSES[2], "video/mp4", 80 * 1024 * 1024),
            (STATUSES[3], "image/png", 12),
            (STATUSES[4], "image/jpeg", 34),
        ] {
            ledger
                .append_document(&source, 1, 100, media(status, media_type, expected_len))
                .unwrap();
        }
        ledger.complete_generation(&source, 1, 3, true).unwrap();
        ledger.append_raw(&source, 2, [(0, RECORD)]).unwrap();
        let mut carried = media(STATUSES[0], "video/mp4", CLIP_BYTES);
        carried.recovered_from = Some("ledger:1:100".to_string());
        ledger.append_document(&source, 2, 0, carried).unwrap();
        ledger.complete_generation(&source, 2, 3, true).unwrap();
        ledger.flush().unwrap();
        drop(ledger);
        let path = dir.join("00000000.rledger");
        assert_eq!(segment_files(dir).unwrap(), vec![(0, path.clone())]);
        path
    }

    /// Each frame of a plain segment's decoded bytes as `(tag, start)`.
    fn frame_starts(decoded: &[u8]) -> Vec<(u8, usize)> {
        let mut frames = Vec::new();
        let mut at = 0;
        while at < decoded.len() {
            frames.push((decoded[at], at));
            let body = u32::from_be_bytes(decoded[at + 1..at + 5].try_into().unwrap()) as usize;
            at += 5 + body + CHECKSUM_BYTES;
        }
        assert_eq!(at, decoded.len());
        frames
    }

    /// A segment holding document frames used to read torn at its first one.
    /// The prefix walk now covers the whole segment, and still stops at the
    /// last intact frame when a document frame is cut short.
    #[test]
    fn the_prefix_walk_covers_document_frames() {
        let temp = TempDir::new().unwrap();
        let path = write_document_segment(temp.path(), SegmentCodec::Plain);
        let file = std::fs::read(&path).unwrap();
        let decoded = &file[MAGIC_PLAIN.len()..];
        let frames = frame_starts(decoded);
        let documents: Vec<usize> = frames
            .iter()
            .filter(|(tag, _)| *tag == TAG_DOCUMENT)
            .map(|(_, start)| *start)
            .collect();
        assert_eq!(documents.len(), 8);
        assert!(frames.iter().all(|(tag, _)| is_frame_tag(*tag)));
        assert!(frames.iter().any(|(tag, _)| *tag != TAG_DOCUMENT));

        let mut reader = SegmentReader::open(&path).unwrap().unwrap();
        let prefix = frame_prefix(&mut reader).unwrap();
        assert!(prefix.clean && !reader.corrupt());
        assert_eq!(prefix.bytes, decoded.len() as u64);
        assert_eq!(prefix.digest, blake3::hash(decoded));

        // The same frames through a compressed container.
        let compressed = temp.path().join("compressed");
        let path = write_document_segment(&compressed, SegmentCodec::zstd(3));
        let mut reader = SegmentReader::open(&path).unwrap().unwrap();
        let through = frame_prefix(&mut reader).unwrap();
        assert!(through.clean && !reader.corrupt() && reader.is_compressed());
        assert_eq!(through.bytes, prefix.bytes);
        assert_eq!(through.digest, prefix.digest);

        // Cut inside the first captured media frame's body (the third
        // document frame): the walk ends at that frame's start, and the
        // segment is still never rewritten.
        let start = documents[2];
        let torn_dir = temp.path().join("torn");
        std::fs::create_dir_all(&torn_dir).unwrap();
        let torn_path = torn_dir.join("00000000.rledger");
        let cut = MAGIC_PLAIN.len() + start + CLIP_BYTES as usize / 2;
        std::fs::write(&torn_path, &file[..cut]).unwrap();
        let mut reader = SegmentReader::open(&torn_path).unwrap().unwrap();
        let torn = frame_prefix(&mut reader).unwrap();
        assert!(!torn.clean);
        assert_eq!(torn.bytes, start as u64);
        assert_eq!(torn.digest, blake3::hash(&decoded[..start]));
        assert_eq!(
            recompress_segment(&torn_path, SegmentCodec::default(), true).unwrap(),
            RecompressOutcome::Torn { bytes: cut as u64 }
        );
        assert!(std::fs::read(&torn_path).unwrap() == file[..cut]);

        // An unknown tag still ends the walk where it stands.
        assert!(!is_frame_tag(0) && !is_frame_tag(8) && !is_frame_tag(u8::MAX));
    }

    /// Compaction and background recompression skip a segment the prefix walk
    /// calls torn, so no segment with a document frame was ever rewritten.
    #[test]
    fn recompression_keeps_document_frames() {
        let temp = TempDir::new().unwrap();
        let path = write_document_segment(temp.path(), SegmentCodec::Plain);
        let original = std::fs::read(&path).unwrap();
        let entries = scan(temp.path()).unwrap().entries;
        assert_eq!(entries.len(), 14);
        let documents = entries
            .iter()
            .filter(|entry| matches!(entry, LedgerEntry::Document { .. }))
            .count();
        assert_eq!(documents, 8);

        let outcome = recompress_segment(&path, SegmentCodec::default(), false).unwrap();
        let RecompressOutcome::Rewritten { before, after } = outcome else {
            panic!("a clean segment is rewritten, not {outcome:?}");
        };
        assert_eq!(before, original.len() as u64);
        assert_eq!(after, std::fs::metadata(&path).unwrap().len());
        assert!(!path.with_extension("rledger.compact").exists());
        let mut reader = SegmentReader::open(&path).unwrap().unwrap();
        assert!(reader.is_compressed());
        let prefix = frame_prefix(&mut reader).unwrap();
        assert!(prefix.clean && !reader.corrupt());
        assert_eq!(prefix.digest, blake3::hash(&original[MAGIC_PLAIN.len()..]));
        // A full visit of the result yields the same entries.
        let scanned = scan(temp.path()).unwrap();
        assert_eq!(scanned.torn_segments, 0);
        assert!(scanned.entries == entries);

        // And back out: the plain container holds exactly the original bytes.
        let outcome = recompress_segment(&path, SegmentCodec::Plain, false).unwrap();
        assert!(
            matches!(outcome, RecompressOutcome::Rewritten { .. }),
            "{outcome:?}"
        );
        assert!(std::fs::read(&path).unwrap() == original);
        assert!(scan(temp.path()).unwrap().entries == entries);
    }
}
