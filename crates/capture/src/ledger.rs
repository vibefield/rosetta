//! The append-only raw ledger — the FIRST of the two data representations
//! (iron rule 4). The projection is disposable because this is not: raw
//! records land here, durably flushed, BEFORE the SQLite transaction that
//! projects them (docs/ingest-engine.html, crash protocol). Recovery may
//! find an orphan ledger tail (harmless — the projection never referenced
//! it); it must never find a projection row whose raw bytes were not
//! flushed first. That ordering is the caller's contract: `append*` then
//! `flush()` then the store tx.
//!
//! Layout: one directory per runtime, numbered segment files, rotated by
//! size. Segments are self-contained: every source a segment references is
//! (re)defined inside it, so a scan never needs another segment's state and
//! deleting old segments (future GC) cannot dangle references.
//!
//! Frames: `tag ∙ u32 body-len ∙ body ∙ 8-byte blake3 checksum`. A torn or
//! corrupt frame ends that segment's scan — everything before it is intact.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use crate::{CaptureError, io_error};

const SEGMENT_MAGIC: &[u8] = b"RLG1";
const CHECKSUM_BYTES: usize = 8;

const TAG_DEFINE: u8 = 1;
const TAG_RAW: u8 = 2;
const TAG_META: u8 = 3;

/// Everything replay needs to rebuild a normalization context for a source
/// file. Written into each segment the first time the source appears there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceDef {
    pub relative_path: String,
    pub stream_id: String,
    pub session_id: String,
    pub native_session_id: String,
    pub agent_version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LedgerEntry {
    /// One raw source record, exactly as framed from the agent's file.
    Raw {
        source: SourceDef,
        generation: u64,
        start_offset: u64,
        payload: Vec<u8>,
    },
    /// A serialized batch of meta-phase ops (JSON `Vec<IngestOp>` — the
    /// golden-snapshot format). Raw-first purists averted: without these,
    /// a rebuild after the agent GC'd its files would lose session titles,
    /// projects, and runtime rows that only bounded skims ever saw.
    Meta(String),
}

/// Append side. One ledger per runtime; the pipeline owns the write order.
pub struct Ledger {
    dir: PathBuf,
    max_segment_bytes: u64,
    segment_seq: u64,
    writer: Option<BufWriter<File>>,
    written: u64,
    /// Sources already defined in the CURRENT segment → their local id.
    defined: HashMap<String, u32>,
}

impl Ledger {
    /// Open (or create) the runtime's ledger directory and continue after
    /// the highest existing segment — existing segments are never reopened
    /// for writing, so a previous crash's torn tail stays untouched.
    pub fn open(dir: &Path) -> Result<Self, CaptureError> {
        std::fs::create_dir_all(dir).map_err(|error| io_error("creating", dir, error))?;
        let next = segment_files(dir)?
            .last()
            .map(|(seq, _)| seq + 1)
            .unwrap_or(0);
        Ok(Self {
            dir: dir.to_path_buf(),
            max_segment_bytes: 128 * 1024 * 1024,
            segment_seq: next,
            writer: None,
            written: 0,
            defined: HashMap::new(),
        })
    }

    pub fn with_max_segment_bytes(mut self, max: u64) -> Self {
        self.max_segment_bytes = max.max(1);
        self
    }

    /// Append one batch of raw records for one source. Buffered — nothing is
    /// durable until [`Ledger::flush`] returns.
    pub fn append_raw(
        &mut self,
        source: &SourceDef,
        generation: u64,
        records: impl IntoIterator<Item = (u64, impl AsRef<[u8]>)>,
    ) -> Result<(), CaptureError> {
        let file_id = self.define(source)?;
        for (start_offset, payload) in records {
            let payload = payload.as_ref();
            let mut body = Vec::with_capacity(20 + payload.len());
            body.extend_from_slice(&file_id.to_be_bytes());
            body.extend_from_slice(&generation.to_be_bytes());
            body.extend_from_slice(&start_offset.to_be_bytes());
            body.extend_from_slice(payload);
            self.frame(TAG_RAW, &body)?;
        }
        Ok(())
    }

    /// Append a serialized meta-ops batch (JSON `Vec<IngestOp>`).
    pub fn append_meta(&mut self, ops_json: &str) -> Result<(), CaptureError> {
        self.frame(TAG_META, ops_json.as_bytes())
    }

    /// Durably flush everything appended so far. MUST complete before the
    /// store transaction that projects those records commits.
    pub fn flush(&mut self) -> Result<(), CaptureError> {
        let Some(writer) = &mut self.writer else {
            return Ok(());
        };
        let path = self.dir.join(segment_name(self.segment_seq));
        writer
            .flush()
            .map_err(|error| io_error("flushing", &path, error))?;
        writer
            .get_ref()
            .sync_data()
            .map_err(|error| io_error("syncing", &path, error))?;
        // Rotate AFTER the durable point so a segment never splits a batch.
        if self.written >= self.max_segment_bytes {
            self.writer = None;
            self.defined.clear();
            self.segment_seq += 1;
            self.written = 0;
        }
        Ok(())
    }

    fn define(&mut self, source: &SourceDef) -> Result<u32, CaptureError> {
        // Ensure the segment exists before deciding the id: rotation clears
        // the map, and ids are segment-local.
        self.ensure_writer()?;
        if let Some(id) = self.defined.get(&source.relative_path) {
            return Ok(*id);
        }
        let id = self.defined.len() as u32;
        let body = serde_encode(&[
            ("file_id", &id.to_string()),
            ("relative_path", &source.relative_path),
            ("stream_id", &source.stream_id),
            ("session_id", &source.session_id),
            ("native_session_id", &source.native_session_id),
            (
                "agent_version",
                source.agent_version.as_deref().unwrap_or(""),
            ),
        ]);
        self.frame(TAG_DEFINE, body.as_bytes())?;
        self.defined.insert(source.relative_path.clone(), id);
        Ok(id)
    }

    fn frame(&mut self, tag: u8, body: &[u8]) -> Result<(), CaptureError> {
        self.ensure_writer()?;
        let path = self.dir.join(segment_name(self.segment_seq));
        let writer = self.writer.as_mut().expect("ensure_writer just ran");
        let mut head = [0_u8; 5];
        head[0] = tag;
        head[1..5].copy_from_slice(&(body.len() as u32).to_be_bytes());
        let mut hasher = blake3::Hasher::new();
        hasher.update(&head);
        hasher.update(body);
        let digest = hasher.finalize();
        let checksum = &digest.as_bytes()[..CHECKSUM_BYTES];
        writer
            .write_all(&head)
            .and_then(|_| writer.write_all(body))
            .and_then(|_| writer.write_all(checksum))
            .map_err(|error| io_error("appending to", &path, error))?;
        self.written += (head.len() + body.len() + CHECKSUM_BYTES) as u64;
        Ok(())
    }

    fn ensure_writer(&mut self) -> Result<(), CaptureError> {
        if self.writer.is_some() {
            return Ok(());
        }
        let path = self.dir.join(segment_name(self.segment_seq));
        let mut file = OpenOptions::new()
            .create_new(true)
            .append(true)
            .open(&path)
            .map_err(|error| io_error("creating segment", &path, error))?;
        file.write_all(SEGMENT_MAGIC)
            .map_err(|error| io_error("writing segment header to", &path, error))?;
        self.written = SEGMENT_MAGIC.len() as u64;
        self.writer = Some(BufWriter::new(file));
        Ok(())
    }
}

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
    let mut out = LedgerScan {
        entries: Vec::new(),
        segments: 0,
        torn_segments: 0,
    };
    for (_, path) in segment_files(dir)? {
        out.segments += 1;
        let mut bytes = Vec::new();
        File::open(&path)
            .and_then(|mut file| file.read_to_end(&mut bytes))
            .map_err(|error| io_error("reading segment", &path, error))?;
        if !scan_segment(&bytes, &mut out.entries) {
            out.torn_segments += 1;
        }
    }
    Ok(out)
}

/// Returns false if the segment ended in a torn/corrupt frame.
fn scan_segment(bytes: &[u8], entries: &mut Vec<LedgerEntry>) -> bool {
    let Some(mut rest) = bytes.strip_prefix(SEGMENT_MAGIC) else {
        return false;
    };
    let mut defined: HashMap<u32, SourceDef> = HashMap::new();
    while !rest.is_empty() {
        if rest.len() < 5 {
            return false;
        }
        let tag = rest[0];
        let body_len = u32::from_be_bytes(rest[1..5].try_into().expect("4 bytes")) as usize;
        let frame_len = 5 + body_len + CHECKSUM_BYTES;
        if rest.len() < frame_len {
            return false;
        }
        let body = &rest[5..5 + body_len];
        let mut hasher = blake3::Hasher::new();
        hasher.update(&rest[..5]);
        hasher.update(body);
        if hasher.finalize().as_bytes()[..CHECKSUM_BYTES] != rest[5 + body_len..frame_len] {
            return false;
        }
        match tag {
            TAG_DEFINE => {
                let Some((id, source)) = decode_define(body) else {
                    return false;
                };
                defined.insert(id, source);
            }
            TAG_RAW => {
                if body_len < 20 {
                    return false;
                }
                let file_id = u32::from_be_bytes(body[0..4].try_into().expect("4 bytes"));
                let generation = u64::from_be_bytes(body[4..12].try_into().expect("8 bytes"));
                let start_offset = u64::from_be_bytes(body[12..20].try_into().expect("8 bytes"));
                let Some(source) = defined.get(&file_id) else {
                    // A raw record before its define: not a valid segment.
                    return false;
                };
                entries.push(LedgerEntry::Raw {
                    source: source.clone(),
                    generation,
                    start_offset,
                    payload: body[20..].to_vec(),
                });
            }
            TAG_META => {
                let Ok(text) = std::str::from_utf8(body) else {
                    return false;
                };
                entries.push(LedgerEntry::Meta(text.to_string()));
            }
            _ => return false,
        }
        rest = &rest[frame_len..];
    }
    true
}

fn segment_name(seq: u64) -> String {
    format!("{seq:08}.rledger")
}

fn segment_files(dir: &Path) -> Result<Vec<(u64, PathBuf)>, CaptureError> {
    let mut out = Vec::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(error) => return Err(io_error("listing", dir, error)),
    };
    for entry in entries {
        let entry = entry.map_err(|error| io_error("listing", dir, error))?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(stem) = name.strip_suffix(".rledger") else {
            continue;
        };
        let Ok(seq) = stem.parse::<u64>() else {
            continue;
        };
        out.push((seq, entry.path()));
    }
    out.sort();
    Ok(out)
}

/// Tiny flat JSON encoder for defines — string fields only, no serde dep in
/// this crate; the decoder below is the only consumer.
fn serde_encode(fields: &[(&str, &str)]) -> String {
    let mut out = String::from("{");
    for (index, (key, value)) in fields.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push('"');
        out.push_str(key);
        out.push_str("\":\"");
        for ch in value.chars() {
            match ch {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                ch if (ch as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", ch as u32)),
                ch => out.push(ch),
            }
        }
        out.push('"');
    }
    out.push('}');
    out
}

fn decode_define(body: &[u8]) -> Option<(u32, SourceDef)> {
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
            relative_path: get("relative_path")?,
            stream_id: get("stream_id")?,
            session_id: get("session_id")?,
            native_session_id: get("native_session_id")?,
            agent_version: (!agent_version.is_empty()).then_some(agent_version),
        },
    ))
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    fn source(path: &str) -> SourceDef {
        SourceDef {
            relative_path: path.to_string(),
            stream_id: "transcript".to_string(),
            session_id: "s-canonical".to_string(),
            native_session_id: "s-native".to_string(),
            agent_version: Some("1.2.3".to_string()),
        }
    }

    #[test]
    fn round_trips_raw_and_meta_entries_in_order() {
        let dir = TempDir::new().unwrap();
        let mut ledger = Ledger::open(dir.path()).unwrap();
        ledger.append_meta("[{\"op\":\"runtime\"}]").unwrap();
        ledger
            .append_raw(
                &source("a.jsonl"),
                1,
                [(0_u64, b"one".as_slice()), (4, b"two")],
            )
            .unwrap();
        ledger
            .append_raw(&source("b.jsonl"), 3, [(10_u64, b"three".as_slice())])
            .unwrap();
        ledger.flush().unwrap();

        let scanned = scan(dir.path()).unwrap();
        assert_eq!(scanned.segments, 1);
        assert_eq!(scanned.torn_segments, 0);
        assert_eq!(scanned.entries.len(), 4);
        assert_eq!(
            scanned.entries[0],
            LedgerEntry::Meta("[{\"op\":\"runtime\"}]".to_string())
        );
        let LedgerEntry::Raw {
            source: from_scan,
            generation,
            start_offset,
            payload,
        } = &scanned.entries[2]
        else {
            panic!("expected a raw entry");
        };
        assert_eq!(from_scan, &source("a.jsonl"));
        assert_eq!((*generation, *start_offset), (1, 4));
        assert_eq!(payload, b"two");
    }

    #[test]
    fn reopening_continues_in_a_new_segment_and_redefines_sources() {
        let dir = TempDir::new().unwrap();
        let mut ledger = Ledger::open(dir.path()).unwrap();
        ledger
            .append_raw(&source("a.jsonl"), 1, [(0_u64, b"first".as_slice())])
            .unwrap();
        ledger.flush().unwrap();
        drop(ledger);

        let mut ledger = Ledger::open(dir.path()).unwrap();
        ledger
            .append_raw(&source("a.jsonl"), 1, [(6_u64, b"second".as_slice())])
            .unwrap();
        ledger.flush().unwrap();

        let scanned = scan(dir.path()).unwrap();
        assert_eq!(scanned.segments, 2);
        assert_eq!(scanned.torn_segments, 0);
        let raw: Vec<&[u8]> = scanned
            .entries
            .iter()
            .filter_map(|entry| match entry {
                LedgerEntry::Raw { payload, .. } => Some(payload.as_slice()),
                LedgerEntry::Meta(_) => None,
            })
            .collect();
        assert_eq!(raw, [b"first".as_slice(), b"second"]);
    }

    #[test]
    fn rotation_by_size_keeps_segments_self_contained() {
        let dir = TempDir::new().unwrap();
        let mut ledger = Ledger::open(dir.path()).unwrap().with_max_segment_bytes(64);
        for index in 0..8_u64 {
            ledger
                .append_raw(
                    &source("a.jsonl"),
                    1,
                    [(index * 8, format!("record-{index}").into_bytes())],
                )
                .unwrap();
            ledger.flush().unwrap();
        }

        let scanned = scan(dir.path()).unwrap();
        assert!(scanned.segments > 1, "64-byte cap must force rotation");
        assert_eq!(scanned.torn_segments, 0);
        let raw_count = scanned
            .entries
            .iter()
            .filter(|entry| matches!(entry, LedgerEntry::Raw { .. }))
            .count();
        assert_eq!(raw_count, 8, "rotation must not lose or split records");
    }

    #[test]
    fn a_torn_tail_is_harmless_and_prior_entries_survive() {
        let dir = TempDir::new().unwrap();
        let mut ledger = Ledger::open(dir.path()).unwrap();
        ledger
            .append_raw(&source("a.jsonl"), 1, [(0_u64, b"intact".as_slice())])
            .unwrap();
        ledger.flush().unwrap();
        drop(ledger);

        // Simulate a crash mid-append: garbage after the durable point.
        let segment = dir.path().join(segment_name(0));
        let mut file = OpenOptions::new().append(true).open(&segment).unwrap();
        file.write_all(&[TAG_RAW, 0, 0, 1]).unwrap(); // truncated header

        let scanned = scan(dir.path()).unwrap();
        assert_eq!(scanned.torn_segments, 1);
        assert_eq!(scanned.entries.len(), 1); // the intact record (defines resolve, they don't surface)

        // A checksum flip inside a frame also just stops the segment.
        let mut bytes = std::fs::read(&segment).unwrap();
        let flip_at = SEGMENT_MAGIC.len() + 7;
        bytes[flip_at] ^= 0xFF;
        std::fs::write(&segment, &bytes).unwrap();
        let scanned = scan(dir.path()).unwrap();
        assert_eq!(scanned.torn_segments, 1);
        assert!(scanned.entries.is_empty());
    }

    #[test]
    fn define_encoding_survives_hostile_strings() {
        let dir = TempDir::new().unwrap();
        let mut ledger = Ledger::open(dir.path()).unwrap();
        let hostile = SourceDef {
            relative_path: "we\"ird\\path\n.jsonl".to_string(),
            stream_id: "transcript".to_string(),
            session_id: "s".to_string(),
            native_session_id: "n".to_string(),
            agent_version: None,
        };
        ledger
            .append_raw(&hostile, 1, [(0_u64, b"x".as_slice())])
            .unwrap();
        ledger.flush().unwrap();

        let scanned = scan(dir.path()).unwrap();
        let LedgerEntry::Raw { source, .. } = &scanned.entries[0] else {
            panic!("expected raw");
        };
        assert_eq!(source, &hostile);
    }
}
