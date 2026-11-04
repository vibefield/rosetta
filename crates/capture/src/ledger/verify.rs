//! Offline evidence about one runtime ledger: what every segment decodes to,
//! and one digest of the entry stream the whole ledger carries.
//!
//! The two digests answer different questions. A segment's frame digest is
//! blake3 over its container-decoded frame bytes — the prefix walk segment
//! recompression proves its copy against — so it survives a change of
//! container but not a change of framing: file ids, define placement and
//! raw-batch packing are all inside it. The entry digest is blake3 over a
//! canonical encoding of every decoded [`LedgerEntry`] in visit order, so it
//! ignores containers, segment boundaries, file ids and batch packing alike
//! and changes only when the entry stream a replay would see changes.
//!
//! Metadata entries carry run-dependent values (a runtime row's discovery
//! time, for one), so two builds of the same inputs never share an entry
//! digest. The same encoding is therefore also split in two: a source digest
//! over the raw, document and seal entries — the captured content, which two
//! builds of read-only inputs must reproduce — and a meta digest over the
//! metadata entries alone.
//!
//! The source digest still depends on the order sources interleave in, which
//! only a path walk fixes. A root-tree walk captures trees newest first and
//! interleaves its lanes, so the same inputs may frame their sources in
//! another order on every run (R1 decision U-2). The source set digest is the
//! order-free measure: each source's own entries, in their order, hashed
//! apart and folded in path order. Reordering whole sources keeps it; a
//! dropped, duplicated or reordered entry of one source changes it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::segment::SegmentReader;
use crate::segment_index::INDEX_FILE;
use crate::{CaptureError, io_error};

use super::{FramePrefix, LedgerEntry, SourceDef, frame_prefix, scan_segment, segment_files};

// Entry kinds in the canonical encoding (not frame tags).
const ENTRY_RAW: u8 = 1;
const ENTRY_SEAL: u8 = 2;
const ENTRY_META: u8 = 3;
const ENTRY_DOCUMENT: u8 = 4;

/// What one segment decodes to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentVerify {
    pub sequence: u64,
    pub path: PathBuf,
    /// The container its magic names; `None` for a short or unknown header,
    /// which is torn from its first byte.
    pub compressed: Option<bool>,
    pub disk_bytes: u64,
    /// Container-decoded bytes of the intact frame prefix, magic excluded.
    pub decoded_bytes: u64,
    /// The frame walk ended on a frame boundary, the container never rejected
    /// its input, and every frame decoded to entries.
    pub clean: bool,
    /// blake3 over those decoded bytes.
    pub frame_digest: blake3::Hash,
    /// Entries a scan of this segment visits.
    pub entries: u64,
}

/// Entry counts by kind, alongside the entry digest.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EntryTally {
    pub entries: u64,
    pub raw_records: u64,
    pub raw_payload_bytes: u64,
    pub documents: u64,
    pub document_bytes: u64,
    pub seals: u64,
    pub metas: u64,
}

/// One runtime ledger's evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerVerify {
    pub segments: Vec<SegmentVerify>,
    pub tally: EntryTally,
    pub entry_digest: blake3::Hash,
    /// Source content (raw records, document observations, seals). Document
    /// observation timestamps are excluded so stable inputs compare across captures.
    pub source_digest: blake3::Hash,
    /// The same content per source, folded in path order: equal for any
    /// interleaving of whole sources ([`EntryDigest::source_set_digest`]).
    pub source_set_digest: blake3::Hash,
    /// The entry encoding of the metadata entries alone.
    pub meta_digest: blake3::Hash,
    /// blake3 over the `.segments` index file's bytes, when there is one.
    pub index_digest: Option<blake3::Hash>,
}

impl LedgerVerify {
    /// One value for every segment's sequence, verdict and frame digest, so
    /// two ledgers with identical decoded segments compare in one step.
    pub fn frame_digest(&self) -> blake3::Hash {
        let mut hasher = blake3::Hasher::new();
        for segment in &self.segments {
            hasher.update(&segment.sequence.to_be_bytes());
            hasher.update(&segment.decoded_bytes.to_be_bytes());
            hasher.update(&[u8::from(segment.clean)]);
            hasher.update(segment.frame_digest.as_bytes());
        }
        hasher.finalize()
    }

    pub fn torn_segments(&self) -> usize {
        self.segments
            .iter()
            .filter(|segment| !segment.clean)
            .count()
    }
}

/// A canonical encoding of decoded entries: one tag per entry kind,
/// fixed-width big-endian integers and length-prefixed byte strings, so two
/// different entry streams never encode alike. Each entry also feeds either
/// the source or the meta digest, and a source entry also feeds its own
/// source's digest.
#[derive(Debug, Clone, Default)]
pub struct EntryDigest {
    all: blake3::Hasher,
    sources: blake3::Hasher,
    metas: blake3::Hasher,
    /// Per source `relative_path`: exactly the bytes its entries feed
    /// `sources`, in their order.
    per_source: BTreeMap<String, blake3::Hasher>,
    tally: EntryTally,
}

impl EntryDigest {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn update(&mut self, entry: &LedgerEntry) {
        let Self {
            all,
            sources,
            metas,
            per_source,
            tally,
        } = self;
        tally.entries += 1;
        match entry {
            LedgerEntry::Raw {
                source,
                generation,
                start_offset,
                payload,
            } => {
                tally.raw_records += 1;
                tally.raw_payload_bytes += payload.len() as u64;
                let mut out = Encoder::for_source(all, sources, per_source, source);
                out.put(&[ENTRY_RAW]);
                out.source(source);
                out.put(&generation.to_be_bytes());
                out.put(&start_offset.to_be_bytes());
                out.bytes(payload);
            }
            LedgerEntry::Document {
                source,
                generation,
                start_offset,
                document,
            } => {
                tally.documents += 1;
                tally.document_bytes += document.bytes.len() as u64;
                let mut out = Encoder::for_source(all, sources, per_source, source);
                out.put(&[ENTRY_DOCUMENT]);
                out.source(source);
                out.put(&generation.to_be_bytes());
                out.put(&start_offset.to_be_bytes());
                out.bytes(document.path.as_bytes());
                // Observation time is run-dependent: the entry digest alone sees it.
                out.entry_only(&document.captured_at.to_be_bytes());
                out.bytes(
                    serde_json::to_string(&document.status)
                        .expect("enum")
                        .as_bytes(),
                );
                out.bytes(&document.bytes);
            }
            LedgerEntry::GenerationComplete {
                source,
                generation,
                normalizer_version,
                normalized_clean,
            } => {
                tally.seals += 1;
                let mut out = Encoder::for_source(all, sources, per_source, source);
                out.put(&[ENTRY_SEAL]);
                out.source(source);
                out.put(&generation.to_be_bytes());
                out.put(&normalizer_version.to_be_bytes());
                out.put(&[u8::from(*normalized_clean)]);
            }
            LedgerEntry::Meta {
                schema_version,
                payload,
            } => {
                tally.metas += 1;
                let mut out = Encoder {
                    all,
                    side: metas,
                    source: None,
                };
                out.put(&[ENTRY_META]);
                out.put(&schema_version.to_be_bytes());
                out.bytes(payload.as_bytes());
            }
        }
    }

    pub fn tally(&self) -> EntryTally {
        self.tally
    }

    /// Every entry.
    pub fn finish(&self) -> blake3::Hash {
        self.all.finalize()
    }

    /// The raw, document and seal entries alone, in visit order.
    pub fn source_digest(&self) -> blake3::Hash {
        self.sources.finalize()
    }

    /// The raw, document and seal entries per source, folded in path order:
    /// blake3 over `(path length, path, the source's digest)` for every
    /// source. Equal for any interleaving of whole sources; each source's
    /// own entries still count in their order and multiplicity.
    pub fn source_set_digest(&self) -> blake3::Hash {
        let mut folded = blake3::Hasher::new();
        for (path, digest) in &self.per_source {
            folded.update(&(path.len() as u64).to_be_bytes());
            folded.update(path.as_bytes());
            folded.update(digest.finalize().as_bytes());
        }
        folded.finalize()
    }

    /// The metadata entries alone.
    pub fn meta_digest(&self) -> blake3::Hash {
        self.metas.finalize()
    }
}

/// Where one entry's encoding goes: the entry digest, the source or meta
/// digest, and for a source entry its own source's digest.
struct Encoder<'a> {
    all: &'a mut blake3::Hasher,
    side: &'a mut blake3::Hasher,
    source: Option<&'a mut blake3::Hasher>,
}

impl<'a> Encoder<'a> {
    fn for_source(
        all: &'a mut blake3::Hasher,
        sources: &'a mut blake3::Hasher,
        per_source: &'a mut BTreeMap<String, blake3::Hasher>,
        source: &SourceDef,
    ) -> Self {
        let path = source.relative_path.as_str();
        if !per_source.contains_key(path) {
            per_source.insert(path.to_owned(), blake3::Hasher::new());
        }
        Self {
            all,
            side: sources,
            source: per_source.get_mut(path),
        }
    }

    fn put(&mut self, bytes: &[u8]) {
        self.all.update(bytes);
        self.side.update(bytes);
        if let Some(source) = self.source.as_deref_mut() {
            source.update(bytes);
        }
    }

    fn entry_only(&mut self, bytes: &[u8]) {
        self.all.update(bytes);
    }

    fn bytes(&mut self, bytes: &[u8]) {
        self.put(&(bytes.len() as u64).to_be_bytes());
        self.put(bytes);
    }

    fn source(&mut self, source: &SourceDef) {
        self.put(&[u8::from(source.ledger_only)]);
        self.bytes(source.relative_path.as_bytes());
        self.bytes(source.stream_id.as_bytes());
        self.bytes(source.session_id.as_bytes());
        self.bytes(source.native_session_id.as_bytes());
        match &source.agent_version {
            None => self.put(&[0]),
            Some(version) => {
                self.put(&[1]);
                self.bytes(version.as_bytes());
            }
        }
    }
}

/// Walk every segment of one runtime ledger twice — once for its decoded
/// frame bytes, once for its entries — and report both, plus the segment
/// index's bytes. Read-only; a torn segment is evidence, not an error. A
/// ledger a live writer is appending to reads as a snapshot whose last
/// segment may be torn.
pub fn verify(dir: &Path) -> Result<LedgerVerify, CaptureError> {
    let mut digest = EntryDigest::new();
    let mut segments = Vec::new();
    for (sequence, path) in segment_files(dir)? {
        let disk_bytes = std::fs::metadata(&path)
            .map_err(|error| io_error("stating segment", &path, error))?
            .len();
        let opened = SegmentReader::open(&path)
            .map_err(|error| io_error("opening segment", &path, error))?;
        let (compressed, prefix, container_clean) = match opened {
            None => (
                None,
                FramePrefix {
                    bytes: 0,
                    clean: false,
                    digest: blake3::Hasher::new().finalize(),
                },
                false,
            ),
            Some(mut reader) => {
                let prefix = frame_prefix(&mut reader)
                    .map_err(|error| io_error("reading segment", &path, error))?;
                (Some(reader.is_compressed()), prefix, !reader.corrupt())
            }
        };
        let mut entries = 0_u64;
        let scanned_clean = scan_segment::<CaptureError, _>(&path, &mut |entry| {
            entries += 1;
            digest.update(&entry);
            Ok(())
        })?;
        segments.push(SegmentVerify {
            sequence,
            path,
            compressed,
            disk_bytes,
            decoded_bytes: prefix.bytes,
            clean: prefix.clean && container_clean && scanned_clean,
            frame_digest: prefix.digest,
            entries,
        });
    }
    let index = dir.join(INDEX_FILE);
    let index_digest = match std::fs::read(&index) {
        Ok(bytes) => Some(blake3::hash(&bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(io_error("reading segment index", &index, error)),
    };
    Ok(LedgerVerify {
        segments,
        tally: digest.tally(),
        entry_digest: digest.finish(),
        source_digest: digest.source_digest(),
        source_set_digest: digest.source_set_digest(),
        meta_digest: digest.meta_digest(),
        index_digest,
    })
}

#[cfg(test)]
mod tests;
