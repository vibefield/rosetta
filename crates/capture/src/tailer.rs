//! Append-delimited tailer — the battle-tested spaghetti port
//! (docs/ingest-engine.html, "What gets ported from spaghetti").
//!
//! The checkpoint carries generation · file identity · committed offset ·
//! boundary prefix anchor, so every way an "append-only" file can lie is a
//! detected transition, not silent corruption: truncation, same-size rewrite,
//! atomic replacement, and a moved/recreated path all bump the generation and
//! reframe from zero. Deterministic row ids upstream make that replay
//! idempotent. All IO is bounded by an explicit byte reservation.
//!
//! Deliberately not ported: spaghetti's per-component `openat` confinement
//! (rosetta tails roots its own adapters declare, not arbitrary user config)
//! and its catalog origin plumbing (rosetta keys provenance by cursor row +
//! byte offset instead).

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use crate::{CaptureError, io_error};

const CHECKPOINT_MAGIC: &[u8] = b"RSTC";
const CHECKPOINT_VERSION: u8 = 2;
/// Oldest checkpoint version `decode` still accepts. Version 1 predates the
/// committed-content chain; its checkpoints resume with `committed_chain:
/// None` and the audit lane establishes the chain on its first pass.
const CHECKPOINT_VERSION_COMPAT: u8 = 1;

/// FNV-1a basis/prime for the committed-content chain: a byte-sequential,
/// resumable fold over exactly the bytes `0..committed_offset`. It exists to
/// let an audit re-read a file and answer "is this still the content the
/// checkpoint ingested?" without replaying — corruption and rewrite
/// detection, deliberately not a cryptographic claim.
pub const CONTENT_CHAIN_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const CONTENT_CHAIN_PRIME: u64 = 0x0000_0100_0000_01B3;

#[inline]
fn chain_byte(chain: u64, byte: u8) -> u64 {
    (chain ^ byte as u64).wrapping_mul(CONTENT_CHAIN_PRIME)
}

/// Fold bytes into the committed-content chain.
pub fn chain_bytes(chain: u64, bytes: &[u8]) -> u64 {
    bytes
        .iter()
        .fold(chain, |chain, byte| chain_byte(chain, *byte))
}
const HASH_BYTES: usize = 32;
const READ_BUFFER_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TailConfig {
    pub delimiter: u8,
    pub normalize_crlf: bool,
    pub max_record_bytes: usize,
    pub max_batch_bytes: usize,
    pub max_records_per_batch: usize,
    pub prefix_anchor_bytes: usize,
    /// Close the batch at the first record boundary at or past this many
    /// payload bytes. Unlike `max_batch_bytes` — a hard cap that must admit a
    /// maximum-size record and therefore cannot sit below it — the soft cap
    /// always admits the record that crosses it, so it may be arbitrarily
    /// small. This is how a budgeted slice gets byte-sized batches without
    /// shrinking `max_record_bytes` and turning large valid records into
    /// oversized drift. `None` (the default) leaves batching to the hard
    /// bounds alone.
    pub soft_batch_bytes: Option<usize>,
}

impl TailConfig {
    pub fn json_lines() -> Self {
        Self {
            delimiter: b'\n',
            normalize_crlf: true,
            // Transcript records can legitimately carry pasted images or
            // other inline media. Keep them bounded, but above the largest
            // fixture/live-corpus records so valid user messages are not
            // converted into irreversible oversized drift markers.
            max_record_bytes: 32 * 1024 * 1024,
            max_batch_bytes: 32 * 1024 * 1024,
            max_records_per_batch: 1_024,
            prefix_anchor_bytes: 4 * 1024,
            soft_batch_bytes: None,
        }
    }
}

/// Native file identity when the platform exposes one; a stable path key
/// otherwise. Same inode after rename → same identity; recreated file → new.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileIdentity {
    Unix { device: u64, inode: u64 },
    Path(Vec<u8>),
}

impl FileIdentity {
    fn encode_into(&self, out: &mut Vec<u8>) {
        match self {
            Self::Unix { device, inode } => {
                out.push(1);
                out.extend_from_slice(&device.to_be_bytes());
                out.extend_from_slice(&inode.to_be_bytes());
            }
            Self::Path(key) => {
                out.push(2);
                out.extend_from_slice(&(key.len() as u64).to_be_bytes());
                out.extend_from_slice(key);
            }
        }
    }

    fn decode_from(reader: &mut ByteReader<'_>) -> Result<Self, CaptureError> {
        match reader.byte()? {
            1 => Ok(Self::Unix {
                device: reader.u64()?,
                inode: reader.u64()?,
            }),
            2 => {
                let len = reader.len_u64()?;
                Ok(Self::Path(reader.take(len)?.to_vec()))
            }
            tag => Err(CaptureError::InvalidCheckpoint(format!(
                "unknown file identity tag {tag}"
            ))),
        }
    }
}

#[cfg(unix)]
pub fn file_identity(_path: &Path, metadata: &std::fs::Metadata) -> FileIdentity {
    use std::os::unix::fs::MetadataExt;
    FileIdentity::Unix {
        device: metadata.dev(),
        inode: metadata.ino(),
    }
}

#[cfg(not(unix))]
pub fn file_identity(path: &Path, _metadata: &std::fs::Metadata) -> FileIdentity {
    FileIdentity::Path(path.to_string_lossy().into_owned().into_bytes())
}

/// How this read relates to the previous checkpoint. Anything but `Continued`
/// starts a new generation: the file's history was rewritten (or never seen),
/// so framing restarts from byte zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transition {
    Initial,
    Continued,
    Truncated,
    IdentityChanged,
    PrefixMismatch,
    ForcedReplay,
}

impl Transition {
    pub fn starts_new_generation(self) -> bool {
        !matches!(self, Self::Continued)
    }
}

/// Durable resume state for one tailed file. `encode`/`decode` are the
/// versioned wire form persisted (hex) inside the pipeline's cursor blob;
/// an undecodable blob means "replay from scratch", never a crash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TailCheckpoint {
    pub generation: u64,
    pub identity: FileIdentity,
    pub committed_offset: u64,
    pub observed_len: u64,
    pub unread_len: u64,
    pub incomplete_suffix_len: u64,
    /// Resumable FNV-1a fold over exactly `0..committed_offset` — the audit
    /// lane's full-content verification. `None` on checkpoints written
    /// before the chain existed; the first audit establishes it.
    pub committed_chain: Option<u64>,
    prefix_start: u64,
    prefix_len: u64,
    prefix_hash: [u8; HASH_BYTES],
}

impl TailCheckpoint {
    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(128);
        bytes.extend_from_slice(CHECKPOINT_MAGIC);
        bytes.push(CHECKPOINT_VERSION);
        bytes.extend_from_slice(&self.generation.to_be_bytes());
        self.identity.encode_into(&mut bytes);
        bytes.extend_from_slice(&self.committed_offset.to_be_bytes());
        bytes.extend_from_slice(&self.observed_len.to_be_bytes());
        bytes.extend_from_slice(&self.unread_len.to_be_bytes());
        bytes.extend_from_slice(&self.incomplete_suffix_len.to_be_bytes());
        bytes.extend_from_slice(&self.prefix_start.to_be_bytes());
        bytes.extend_from_slice(&self.prefix_len.to_be_bytes());
        bytes.extend_from_slice(&self.prefix_hash);
        match self.committed_chain {
            Some(chain) => {
                bytes.push(1);
                bytes.extend_from_slice(&chain.to_be_bytes());
            }
            None => bytes.push(0),
        }
        bytes
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, CaptureError> {
        let (mut reader, version) = ByteReader::versioned(
            bytes,
            CHECKPOINT_MAGIC,
            CHECKPOINT_VERSION_COMPAT..=CHECKPOINT_VERSION,
        )?;
        let mut checkpoint = Self {
            generation: reader.u64()?,
            identity: FileIdentity::decode_from(&mut reader)?,
            committed_offset: reader.u64()?,
            observed_len: reader.u64()?,
            unread_len: reader.u64()?,
            incomplete_suffix_len: reader.u64()?,
            prefix_start: reader.u64()?,
            prefix_len: reader.u64()?,
            prefix_hash: reader.hash()?,
            committed_chain: None,
        };
        if version >= 2 {
            checkpoint.committed_chain = match reader.byte()? {
                0 => None,
                1 => Some(reader.u64()?),
                _ => {
                    return Err(CaptureError::InvalidCheckpoint(
                        "invalid committed-chain marker".to_string(),
                    ));
                }
            };
        }
        reader.finish()?;
        checkpoint.validate()?;
        Ok(checkpoint)
    }

    pub fn encode_hex(&self) -> String {
        let bytes = self.encode();
        let mut out = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            out.push_str(&format!("{byte:02x}"));
        }
        out
    }

    pub fn decode_hex(text: &str) -> Result<Self, CaptureError> {
        if !text.len().is_multiple_of(2) {
            return Err(CaptureError::InvalidCheckpoint(
                "hex checkpoint has odd length".to_string(),
            ));
        }
        let bytes = (0..text.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(&text[at..at + 2], 16))
            .collect::<Result<Vec<u8>, _>>()
            .map_err(|_| {
                CaptureError::InvalidCheckpoint("hex checkpoint has non-hex bytes".to_string())
            })?;
        Self::decode(&bytes)
    }

    fn validate(&self) -> Result<(), CaptureError> {
        if self.generation == 0 {
            return Err(CaptureError::InvalidCheckpoint(
                "tail generation must be greater than zero".to_string(),
            ));
        }
        if self.committed_offset > self.observed_len
            || self.unread_len != self.observed_len - self.committed_offset
            || self.incomplete_suffix_len > self.unread_len
            || self.prefix_start.saturating_add(self.prefix_len) != self.committed_offset
        {
            return Err(CaptureError::InvalidCheckpoint(
                "tail checkpoint ranges are inconsistent".to_string(),
            ));
        }
        Ok(())
    }
}

/// What [`Tailer::verify_committed`] found at the path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommittedVerification {
    /// The file is gone; the ordinary read path owns that outcome.
    Missing,
    /// A different file sits at the path.
    IdentityChanged,
    /// The file is shorter than the committed range.
    Truncated,
    /// The committed content is exactly what the checkpoint ingested.
    Verified,
    /// The committed content changed under a quiet stat — recovery time.
    Mismatch,
    /// The checkpoint predates the chain; here is the freshly computed
    /// value for the caller to persist, after which audits verify cheaply.
    Established(u64),
}

/// One complete delimiter-terminated record. Offsets are file positions of
/// the record start and one past its delimiter — the provenance the pipeline
/// stamps into `src_offset`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawRecord {
    pub generation: u64,
    pub start_offset: u64,
    pub end_offset: u64,
    pub ordinal_in_batch: u32,
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TailItem {
    Record(RawRecord),
    /// A complete record over `max_record_bytes`: the cursor advances past it
    /// (so one pathological line can't wedge the stream) but the payload is
    /// withheld. The pipeline records it as drift.
    Oversized {
        start_offset: u64,
        end_offset: u64,
        ordinal_in_batch: u32,
        payload_len: u64,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub enum TailRead {
    /// The file does not exist. State never advances on missing.
    Missing,
    /// The file changed under the read (shrank, replaced mid-read) — retry
    /// with the SAME previous checkpoint; the next read will classify it.
    Retry,
    Batch(TailBatch),
}

#[derive(Debug, PartialEq, Eq)]
pub struct TailBatch {
    pub items: Vec<TailItem>,
    pub checkpoint: TailCheckpoint,
    pub transition: Transition,
    /// More complete records (or a growing suffix) remain — call again.
    pub needs_retry: bool,
    pub more_available: bool,
    pub snapshot_len: u64,
    /// Exact bytes read from the file, including the continuity verify and
    /// checkpoint anchor reads. Metadata stats are not byte payloads.
    pub bytes_read: u64,
}

#[derive(Debug, Clone)]
pub struct Tailer {
    config: TailConfig,
}

impl Tailer {
    pub fn new(config: TailConfig) -> Result<Self, CaptureError> {
        if config.max_record_bytes == 0
            || config.max_batch_bytes == 0
            || config.max_records_per_batch == 0
            || config.prefix_anchor_bytes == 0
            || config.soft_batch_bytes == Some(0)
        {
            return Err(CaptureError::InvalidConfig(
                "tailer bounds must all be greater than zero".to_string(),
            ));
        }
        if config.max_record_bytes > config.max_batch_bytes {
            return Err(CaptureError::InvalidConfig(
                "max_record_bytes must not exceed max_batch_bytes".to_string(),
            ));
        }
        if config.max_records_per_batch > u32::MAX as usize {
            return Err(CaptureError::InvalidConfig(
                "max_records_per_batch exceeds ordinal range".to_string(),
            ));
        }
        Ok(Self { config })
    }

    pub fn read(
        &self,
        path: &Path,
        previous: Option<&TailCheckpoint>,
        force_replay: bool,
    ) -> Result<TailRead, CaptureError> {
        self.read_bounded(path, previous, force_replay, u64::MAX)
    }

    /// Read with an exact physical byte ceiling, enforced before each OS read.
    /// The ceiling covers framing plus continuity/anchor reads.
    pub fn read_bounded(
        &self,
        path: &Path,
        previous: Option<&TailCheckpoint>,
        force_replay: bool,
        max_read_bytes: u64,
    ) -> Result<TailRead, CaptureError> {
        if max_read_bytes == 0 {
            return Err(CaptureError::InvalidConfig(
                "physical read limit must be greater than zero".to_string(),
            ));
        }
        let mut file = match File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(TailRead::Missing);
            }
            Err(error) => return Err(io_error("opening", path, error)),
        };
        let mut accounting = ReadAccounting::new(max_read_bytes);

        let initial_metadata = file
            .metadata()
            .map_err(|error| io_error("reading metadata for", path, error))?;
        let snapshot_len = initial_metadata.len();
        let identity = file_identity(path, &initial_metadata);

        let (generation, start_offset, transition) = self.resume_point(
            &mut file,
            path,
            snapshot_len,
            &identity,
            previous,
            force_replay,
            &mut accounting,
        )?;
        // Continuations resume the chain the checkpoint carries (None on a
        // pre-chain blob); every read-from-zero transition restarts it.
        let resume_chain = match transition {
            Transition::Continued => previous.and_then(|prev| prev.committed_chain),
            _ => Some(CONTENT_CHAIN_BASIS),
        };

        let framed = self.frame(
            &mut file,
            path,
            start_offset,
            resume_chain,
            snapshot_len,
            generation,
            &mut accounting,
        )?;
        let prefix =
            self.prefix_anchor(&mut file, path, framed.committed_offset, &mut accounting)?;

        // Stability recheck against both the open handle and the path: a
        // shrink or identity change under our feet means the snapshot cannot
        // be trusted — retry with unchanged state.
        let handle_metadata = file
            .metadata()
            .map_err(|error| io_error("rechecking metadata for", path, error))?;
        let path_metadata = match std::fs::metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(TailRead::Retry);
            }
            Err(error) => return Err(io_error("rechecking path for", path, error)),
        };
        if file_identity(path, &handle_metadata) != identity
            || file_identity(path, &path_metadata) != identity
            || handle_metadata.len() < snapshot_len
            || path_metadata.len() < snapshot_len
        {
            return Ok(TailRead::Retry);
        }

        let checkpoint = TailCheckpoint {
            generation,
            identity,
            committed_offset: framed.committed_offset,
            observed_len: snapshot_len,
            unread_len: snapshot_len - framed.committed_offset,
            incomplete_suffix_len: framed.incomplete_suffix_len,
            committed_chain: framed.committed_chain,
            prefix_start: prefix.start,
            prefix_len: prefix.len,
            prefix_hash: prefix.hash,
        };
        checkpoint.validate()?;

        Ok(TailRead::Batch(TailBatch {
            needs_retry: framed.incomplete_suffix_len > 0 || framed.more_available,
            more_available: framed.more_available,
            items: framed.items,
            checkpoint,
            transition,
            snapshot_len,
            bytes_read: accounting.consumed,
        }))
    }

    /// The audit lane's question, answered without a replay: is the file
    /// still byte-for-byte the `0..committed_offset` content `previous`
    /// ingested? One sequential read of the committed range, folded through
    /// the content chain — no framing, no normalization, no generation, no
    /// writes. The caller escalates a [`CommittedVerification::Mismatch`]
    /// to a forced replay; everything else is already answered.
    pub fn verify_committed(
        &self,
        path: &Path,
        previous: &TailCheckpoint,
    ) -> Result<CommittedVerification, CaptureError> {
        let mut file = match File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(CommittedVerification::Missing);
            }
            Err(error) => return Err(io_error("opening", path, error)),
        };
        let metadata = file
            .metadata()
            .map_err(|error| io_error("reading metadata for", path, error))?;
        if file_identity(path, &metadata) != previous.identity {
            return Ok(CommittedVerification::IdentityChanged);
        }
        if metadata.len() < previous.committed_offset {
            return Ok(CommittedVerification::Truncated);
        }
        let mut remaining = previous.committed_offset;
        let mut chain = CONTENT_CHAIN_BASIS;
        let mut buffer = [0_u8; READ_BUFFER_BYTES];
        while remaining > 0 {
            let wanted = usize::try_from(remaining.min(READ_BUFFER_BYTES as u64))
                .expect("bounded read length always fits usize");
            let count = file
                .read(&mut buffer[..wanted])
                .map_err(|error| io_error("verifying", path, error))?;
            if count == 0 {
                return Ok(CommittedVerification::Truncated);
            }
            chain = chain_bytes(chain, &buffer[..count]);
            remaining -= count as u64;
        }
        Ok(match previous.committed_chain {
            Some(expected) if expected == chain => CommittedVerification::Verified,
            Some(_) => CommittedVerification::Mismatch,
            None => CommittedVerification::Established(chain),
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn resume_point(
        &self,
        file: &mut File,
        path: &Path,
        snapshot_len: u64,
        identity: &FileIdentity,
        previous: Option<&TailCheckpoint>,
        force_replay: bool,
        accounting: &mut ReadAccounting,
    ) -> Result<(u64, u64, Transition), CaptureError> {
        let Some(previous) = previous else {
            return Ok((1, 0, Transition::Initial));
        };
        previous.validate()?;
        if force_replay {
            return Ok((
                next_generation(previous.generation)?,
                0,
                Transition::ForcedReplay,
            ));
        }
        if &previous.identity != identity {
            return Ok((
                next_generation(previous.generation)?,
                0,
                Transition::IdentityChanged,
            ));
        }
        if snapshot_len < previous.committed_offset {
            return Ok((
                next_generation(previous.generation)?,
                0,
                Transition::Truncated,
            ));
        }
        if !self.verify_prefix(file, path, previous, accounting)? {
            return Ok((
                next_generation(previous.generation)?,
                0,
                Transition::PrefixMismatch,
            ));
        }
        Ok((
            previous.generation,
            previous.committed_offset,
            Transition::Continued,
        ))
    }

    #[allow(clippy::too_many_arguments)]
    fn frame(
        &self,
        file: &mut File,
        path: &Path,
        start_offset: u64,
        resume_chain: Option<u64>,
        snapshot_len: u64,
        generation: u64,
        accounting: &mut ReadAccounting,
    ) -> Result<FramedBatch, CaptureError> {
        file.seek(SeekFrom::Start(start_offset))
            .map_err(|error| io_error("seeking", path, error))?;

        let mut output = FramedBatch::new(start_offset, resume_chain);
        let mut pending = PendingRecord::new(self.config.max_record_bytes);
        let mut buffer = [0_u8; READ_BUFFER_BYTES];
        let mut read_offset = start_offset;
        let mut batch_payload_bytes = 0_usize;
        // The content chain folds every scanned byte in order; the snapshot
        // taken at each record commit is the value for `0..committed`.
        let mut running_chain = resume_chain;

        'read: while read_offset < snapshot_len {
            let wanted =
                usize::try_from((snapshot_len - read_offset).min(READ_BUFFER_BYTES as u64))
                    .expect("bounded read length always fits usize");
            let count = accounting.read(file, &mut buffer[..wanted], path)?;
            if count == 0 {
                // The file shrank mid-read; the stability recheck will retry.
                output.more_available = true;
                return Ok(output);
            }

            let chunk = &buffer[..count];
            let mut segment_start = 0;
            for (index, byte) in chunk.iter().enumerate() {
                if let Some(chain) = running_chain.as_mut() {
                    *chain = chain_byte(*chain, *byte);
                }
                if *byte != self.config.delimiter {
                    continue;
                }
                pending.push(&chunk[segment_start..index]);
                let record_end = read_offset + index as u64 + 1;
                let payload_len = pending.delivered_len(self.config.normalize_crlf);
                if !pending.oversized()
                    && batch_payload_bytes.saturating_add(payload_len) > self.config.max_batch_bytes
                {
                    output.more_available = true;
                    break 'read;
                }

                let ordinal = u32::try_from(output.items.len())
                    .expect("max_records_per_batch was validated against u32");
                let item = pending.finish(
                    generation,
                    output.committed_offset,
                    record_end,
                    ordinal,
                    self.config.normalize_crlf,
                );
                if let TailItem::Record(record) = &item {
                    batch_payload_bytes += record.payload.len();
                }
                output.items.push(item);
                output.committed_offset = record_end;
                output.committed_chain = running_chain;
                pending = PendingRecord::new(self.config.max_record_bytes);
                segment_start = index + 1;

                if output.items.len() == self.config.max_records_per_batch
                    || batch_payload_bytes == self.config.max_batch_bytes
                    || self
                        .config
                        .soft_batch_bytes
                        .is_some_and(|cap| batch_payload_bytes >= cap)
                {
                    output.more_available = record_end < snapshot_len;
                    break 'read;
                }
            }
            pending.push(&chunk[segment_start..]);
            read_offset += count as u64;
        }

        if !output.more_available && read_offset >= snapshot_len {
            output.incomplete_suffix_len = pending.len;
        }
        Ok(output)
    }

    fn prefix_anchor(
        &self,
        file: &mut File,
        path: &Path,
        committed_offset: u64,
        accounting: &mut ReadAccounting,
    ) -> Result<PrefixAnchor, CaptureError> {
        let len = committed_offset.min(self.config.prefix_anchor_bytes as u64);
        let start = committed_offset - len;
        if len == 0 {
            return Ok(PrefixAnchor {
                start,
                len,
                hash: [0; HASH_BYTES],
            });
        }
        file.seek(SeekFrom::Start(start))
            .map_err(|error| io_error("seeking to prefix anchor in", path, error))?;
        let mut bytes = vec![0; len as usize];
        accounting.read_exact(file, &mut bytes, "reading prefix anchor from", path)?;
        Ok(PrefixAnchor {
            start,
            len,
            hash: *blake3::hash(&bytes).as_bytes(),
        })
    }

    fn verify_prefix(
        &self,
        file: &mut File,
        path: &Path,
        checkpoint: &TailCheckpoint,
        accounting: &mut ReadAccounting,
    ) -> Result<bool, CaptureError> {
        if checkpoint.prefix_len == 0 {
            return Ok(true);
        }
        file.seek(SeekFrom::Start(checkpoint.prefix_start))
            .map_err(|error| io_error("seeking to verify prefix in", path, error))?;
        let mut bytes = vec![0; checkpoint.prefix_len as usize];
        accounting.read_exact(file, &mut bytes, "verifying prefix in", path)?;
        Ok(*blake3::hash(&bytes).as_bytes() == checkpoint.prefix_hash)
    }
}

struct FramedBatch {
    items: Vec<TailItem>,
    committed_offset: u64,
    /// Content chain at `committed_offset`; `None` when resuming a
    /// pre-chain checkpoint whose prefix bytes were never folded.
    committed_chain: Option<u64>,
    incomplete_suffix_len: u64,
    more_available: bool,
}

impl FramedBatch {
    fn new(committed_offset: u64, committed_chain: Option<u64>) -> Self {
        Self {
            items: Vec::new(),
            committed_offset,
            committed_chain,
            incomplete_suffix_len: 0,
            more_available: false,
        }
    }
}

/// Accumulates one record across chunk boundaries. Oversized records keep
/// counting length but stop retaining bytes, so a pathological line costs
/// `max_record_bytes + 1` memory, not its own size.
struct PendingRecord {
    retained: Vec<u8>,
    len: u64,
    max_record_bytes: usize,
}

impl PendingRecord {
    fn new(max_record_bytes: usize) -> Self {
        Self {
            retained: Vec::new(),
            len: 0,
            max_record_bytes,
        }
    }

    fn push(&mut self, bytes: &[u8]) {
        self.len = self.len.saturating_add(bytes.len() as u64);
        if self.retained.len() <= self.max_record_bytes {
            let remaining = self
                .max_record_bytes
                .saturating_add(1)
                .saturating_sub(self.retained.len());
            self.retained
                .extend_from_slice(&bytes[..bytes.len().min(remaining)]);
        }
    }

    fn oversized(&self) -> bool {
        self.len > self.max_record_bytes as u64
    }

    fn delivered_len(&self, normalize_crlf: bool) -> usize {
        if self.oversized() {
            0
        } else if normalize_crlf && self.retained.last() == Some(&b'\r') {
            self.retained.len() - 1
        } else {
            self.retained.len()
        }
    }

    fn finish(
        mut self,
        generation: u64,
        start_offset: u64,
        end_offset: u64,
        ordinal: u32,
        normalize_crlf: bool,
    ) -> TailItem {
        if self.oversized() {
            return TailItem::Oversized {
                start_offset,
                end_offset,
                ordinal_in_batch: ordinal,
                payload_len: self.len,
            };
        }
        if normalize_crlf && self.retained.last() == Some(&b'\r') {
            self.retained.pop();
        }
        TailItem::Record(RawRecord {
            generation,
            start_offset,
            end_offset,
            ordinal_in_batch: ordinal,
            payload: self.retained,
        })
    }
}

struct PrefixAnchor {
    start: u64,
    len: u64,
    hash: [u8; HASH_BYTES],
}

struct ReadAccounting {
    limit: u64,
    consumed: u64,
}

impl ReadAccounting {
    fn new(limit: u64) -> Self {
        Self { limit, consumed: 0 }
    }

    fn read(
        &mut self,
        file: &mut File,
        buffer: &mut [u8],
        path: &Path,
    ) -> Result<usize, CaptureError> {
        let remaining = self.limit.saturating_sub(self.consumed);
        if remaining == 0 && !buffer.is_empty() {
            return Err(self.limit_error());
        }
        let allowed = buffer
            .len()
            .min(usize::try_from(remaining).unwrap_or(usize::MAX));
        let count = file
            .read(&mut buffer[..allowed])
            .map_err(|error| io_error("reading", path, error))?;
        self.consumed = self.consumed.saturating_add(count as u64);
        Ok(count)
    }

    fn read_exact(
        &mut self,
        file: &mut File,
        buffer: &mut [u8],
        operation: &'static str,
        path: &Path,
    ) -> Result<(), CaptureError> {
        if buffer.len() as u64 > self.limit.saturating_sub(self.consumed) {
            return Err(self.limit_error());
        }
        file.read_exact(buffer)
            .map_err(|error| io_error(operation, path, error))?;
        self.consumed = self.consumed.saturating_add(buffer.len() as u64);
        Ok(())
    }

    fn limit_error(&self) -> CaptureError {
        CaptureError::LimitExceeded(format!(
            "tail reads exceed {} byte access reservation",
            self.limit
        ))
    }
}

fn next_generation(current: u64) -> Result<u64, CaptureError> {
    current
        .checked_add(1)
        .ok_or_else(|| CaptureError::InvalidCheckpoint("tail generation overflowed".to_string()))
}

struct ByteReader<'a> {
    remaining: &'a [u8],
}

impl<'a> ByteReader<'a> {
    /// Accept any version in `versions`, returning the one found so the
    /// caller can decode the fields that version carries.
    fn versioned(
        bytes: &'a [u8],
        magic: &[u8],
        versions: std::ops::RangeInclusive<u8>,
    ) -> Result<(Self, u8), CaptureError> {
        let Some(rest) = bytes.strip_prefix(magic) else {
            return Err(CaptureError::InvalidCheckpoint(
                "checkpoint magic does not match".to_string(),
            ));
        };
        let mut reader = Self { remaining: rest };
        let version = reader.byte()?;
        if !versions.contains(&version) {
            return Err(CaptureError::InvalidCheckpoint(
                "unsupported checkpoint version".to_string(),
            ));
        }
        Ok((reader, version))
    }

    fn finish(self) -> Result<(), CaptureError> {
        if self.remaining.is_empty() {
            Ok(())
        } else {
            Err(CaptureError::InvalidCheckpoint(
                "checkpoint contains trailing bytes".to_string(),
            ))
        }
    }

    fn byte(&mut self) -> Result<u8, CaptureError> {
        Ok(self.take(1)?[0])
    }

    fn u64(&mut self) -> Result<u64, CaptureError> {
        Ok(u64::from_be_bytes(self.take(8)?.try_into().map_err(
            |_| CaptureError::InvalidCheckpoint("truncated u64".to_string()),
        )?))
    }

    fn len_u64(&mut self) -> Result<usize, CaptureError> {
        usize::try_from(self.u64()?).map_err(|_| {
            CaptureError::InvalidCheckpoint("checkpoint length is too large".to_string())
        })
    }

    fn hash(&mut self) -> Result<[u8; HASH_BYTES], CaptureError> {
        self.take(HASH_BYTES)?
            .try_into()
            .map_err(|_| CaptureError::InvalidCheckpoint("truncated hash".to_string()))
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8], CaptureError> {
        if count > self.remaining.len() {
            return Err(CaptureError::InvalidCheckpoint(
                "checkpoint is truncated".to_string(),
            ));
        }
        let (value, rest) = self.remaining.split_at(count);
        self.remaining = rest;
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use std::fs::OpenOptions;
    use std::io::Write;

    use tempfile::{NamedTempFile, TempDir};

    use super::*;

    fn tailer() -> Tailer {
        let mut config = TailConfig::json_lines();
        config.max_record_bytes = 16;
        config.max_batch_bytes = 32;
        config.max_records_per_batch = 8;
        Tailer::new(config).unwrap()
    }

    fn batch(read: TailRead) -> TailBatch {
        match read {
            TailRead::Batch(batch) => batch,
            other => panic!("expected tail batch, got {other:?}"),
        }
    }

    fn payloads(items: &[TailItem]) -> Vec<Vec<u8>> {
        items
            .iter()
            .map(|item| match item {
                TailItem::Record(record) => record.payload.clone(),
                TailItem::Oversized { .. } => panic!("unexpected oversized record"),
            })
            .collect()
    }

    #[test]
    fn soft_cap_closes_at_the_first_boundary_past_it_and_admits_big_records() {
        let mut config = TailConfig::json_lines();
        config.soft_batch_bytes = Some(10);
        let tailer = Tailer::new(config).unwrap();
        let file = NamedTempFile::new().unwrap();
        // 4 + 4 crosses the 10-byte cap only on the third record; a record
        // larger than the cap still frames alone instead of going oversized.
        std::fs::write(file.path(), b"aaaa\nbbbb\ncccc\rdddd-oversoft\neeee\n").unwrap();

        let first = batch(tailer.read(file.path(), None, false).unwrap());
        assert_eq!(payloads(&first.items).len(), 3);
        assert!(first.more_available);

        let second = batch(
            tailer
                .read(file.path(), Some(&first.checkpoint), false)
                .unwrap(),
        );
        assert_eq!(payloads(&second.items), vec![b"eeee".to_vec()]);
        assert!(!second.more_available);

        assert!(
            Tailer::new(TailConfig {
                soft_batch_bytes: Some(0),
                ..TailConfig::json_lines()
            })
            .is_err(),
            "a zero soft cap is a configuration error, not an infinite loop"
        );
    }

    #[test]
    fn partial_final_record_is_not_emitted_or_advanced() {
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(b"{\"one\":1}\n{\"two\":").unwrap();
        file.flush().unwrap();

        let first = batch(tailer().read(file.path(), None, false).unwrap());
        assert_eq!(first.transition, Transition::Initial);
        assert_eq!(first.items.len(), 1);
        assert_eq!(first.checkpoint.committed_offset, 10);
        assert_eq!(first.checkpoint.incomplete_suffix_len, 7);
        assert!(first.needs_retry);

        let mut append = OpenOptions::new().append(true).open(file.path()).unwrap();
        append.write_all(b"2}\n").unwrap();
        append.flush().unwrap();
        let second = batch(
            tailer()
                .read(file.path(), Some(&first.checkpoint), false)
                .unwrap(),
        );
        assert_eq!(second.transition, Transition::Continued);
        assert_eq!(second.items.len(), 1);
        let TailItem::Record(record) = &second.items[0] else {
            panic!("expected ordinary record");
        };
        assert_eq!(record.payload, b"{\"two\":2}");
        assert_eq!(record.start_offset, 10);
        assert_eq!(record.end_offset, 20);
        assert_eq!(second.checkpoint.committed_offset, 20);
        assert!(!second.needs_retry);
    }

    #[test]
    fn crlf_is_normalized_after_byte_cursors_are_calculated() {
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(b"one\r\ntwo\r\n").unwrap();
        file.flush().unwrap();
        let result = batch(tailer().read(file.path(), None, false).unwrap());
        assert_eq!(payloads(&result.items), [b"one".to_vec(), b"two".to_vec()]);
        assert_eq!(result.checkpoint.committed_offset, 10);
    }

    #[test]
    fn complete_oversized_record_is_quarantined_and_cursor_advances() {
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(b"0123456789abcdefX\nok\n").unwrap();
        file.flush().unwrap();
        let result = batch(tailer().read(file.path(), None, false).unwrap());
        assert!(matches!(
            result.items[0],
            TailItem::Oversized {
                payload_len: 17,
                ..
            }
        ));
        assert!(matches!(result.items[1], TailItem::Record(_)));
        assert_eq!(result.checkpoint.committed_offset, 21);
    }

    #[test]
    fn truncate_and_rewrite_starts_a_new_generation() {
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(b"first-record\n").unwrap();
        file.flush().unwrap();
        let first = batch(tailer().read(file.path(), None, false).unwrap());

        std::fs::write(file.path(), b"new\n").unwrap();
        let second = batch(
            tailer()
                .read(file.path(), Some(&first.checkpoint), false)
                .unwrap(),
        );
        assert_eq!(second.transition, Transition::Truncated);
        assert_eq!(
            second.checkpoint.generation,
            first.checkpoint.generation + 1
        );
        assert_eq!(second.items.len(), 1);
    }

    #[test]
    fn same_size_prefix_rewrite_starts_a_new_generation() {
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(b"old\n").unwrap();
        file.flush().unwrap();
        let first = batch(tailer().read(file.path(), None, false).unwrap());
        std::fs::write(file.path(), b"new\n").unwrap();

        let second = batch(
            tailer()
                .read(file.path(), Some(&first.checkpoint), false)
                .unwrap(),
        );
        assert_eq!(second.transition, Transition::PrefixMismatch);
        assert_eq!(
            second.checkpoint.generation,
            first.checkpoint.generation + 1
        );
    }

    #[test]
    fn atomic_identity_replacement_starts_a_new_generation() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("stream.jsonl");
        std::fs::write(&path, b"old\n").unwrap();
        let first = batch(tailer().read(&path, None, false).unwrap());
        let replacement = directory.path().join("replacement");
        std::fs::write(&replacement, b"new\n").unwrap();
        std::fs::rename(replacement, &path).unwrap();

        let second = batch(
            tailer()
                .read(&path, Some(&first.checkpoint), false)
                .unwrap(),
        );
        assert_eq!(second.transition, Transition::IdentityChanged);
        assert_eq!(
            second.checkpoint.generation,
            first.checkpoint.generation + 1
        );
    }

    #[test]
    fn forced_replay_reframes_from_zero_in_a_new_generation() {
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(b"one\ntwo\n").unwrap();
        file.flush().unwrap();
        let first = batch(tailer().read(file.path(), None, false).unwrap());

        let replay = batch(
            tailer()
                .read(file.path(), Some(&first.checkpoint), true)
                .unwrap(),
        );
        assert_eq!(replay.transition, Transition::ForcedReplay);
        assert_eq!(
            replay.checkpoint.generation,
            first.checkpoint.generation + 1
        );
        assert_eq!(replay.items.len(), 2);
    }

    #[test]
    fn batch_record_limit_preserves_next_record_boundary() {
        let mut config = TailConfig::json_lines();
        config.max_record_bytes = 8;
        config.max_batch_bytes = 8;
        config.max_records_per_batch = 1;
        let tailer = Tailer::new(config).unwrap();
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(b"one\ntwo\n").unwrap();
        file.flush().unwrap();

        let first = batch(tailer.read(file.path(), None, false).unwrap());
        assert_eq!(first.items.len(), 1);
        assert_eq!(first.checkpoint.committed_offset, 4);
        assert!(first.needs_retry && first.more_available);
        let second = batch(
            tailer
                .read(file.path(), Some(&first.checkpoint), false)
                .unwrap(),
        );
        assert_eq!(second.items.len(), 1);
        assert_eq!(second.checkpoint.committed_offset, 8);
        assert!(!second.needs_retry && !second.more_available);
    }

    #[test]
    fn bounded_read_reports_physical_bytes_and_fails_before_overrun() {
        let root = TempDir::new().unwrap();
        let path = root.path().join("stream.jsonl");
        std::fs::write(&path, b"one\ntwo\n").unwrap();
        let tailer = tailer();

        // 8 framed bytes + an 8-byte checkpoint anchor need exactly 16.
        assert!(matches!(
            tailer.read_bounded(&path, None, false, 15),
            Err(CaptureError::LimitExceeded(_))
        ));
        let first = batch(tailer.read_bounded(&path, None, false, 16).unwrap());
        assert_eq!(first.bytes_read, 16);

        let mut append = OpenOptions::new().append(true).open(&path).unwrap();
        append.write_all(b"three\n").unwrap();
        append.flush().unwrap();
        // 8 verify + 6 appended + a 14-byte anchor over the new prefix.
        let second = batch(
            tailer
                .read_bounded(&path, Some(&first.checkpoint), false, 28)
                .unwrap(),
        );
        assert_eq!(second.bytes_read, 28);
    }

    #[test]
    fn exact_full_batch_stops_before_scanning_a_delimiter_free_tail() {
        const RECORD_PAYLOAD_BYTES: usize = 65_536;
        const FRAMING_READ_AHEAD_BYTES: usize = 65_536;
        const CHECKPOINT_ANCHOR_BYTES: usize = 4_096;
        const PHYSICAL_READ_CEILING: u64 =
            (RECORD_PAYLOAD_BYTES + FRAMING_READ_AHEAD_BYTES + CHECKPOINT_ANCHOR_BYTES) as u64;

        let root = TempDir::new().unwrap();
        let path = root.path().join("stream.jsonl");
        let mut bytes = vec![b'a'; RECORD_PAYLOAD_BYTES];
        bytes.push(b'\n');
        // The next record has no delimiter and exceeds both the logical
        // record bound and the remaining physical reservation. Once the
        // accepted first record exactly fills the batch, reading further
        // would turn a bounded prefix into a false failure.
        bytes.extend(std::iter::repeat_n(b'b', FRAMING_READ_AHEAD_BYTES * 2));
        std::fs::write(&path, bytes).unwrap();

        let tailer = Tailer::new(TailConfig {
            delimiter: b'\n',
            normalize_crlf: true,
            max_record_bytes: RECORD_PAYLOAD_BYTES,
            max_batch_bytes: RECORD_PAYLOAD_BYTES,
            max_records_per_batch: 128,
            prefix_anchor_bytes: CHECKPOINT_ANCHOR_BYTES,
            soft_batch_bytes: None,
        })
        .unwrap();
        let result = batch(
            tailer
                .read_bounded(&path, None, false, PHYSICAL_READ_CEILING)
                .unwrap(),
        );
        assert_eq!(result.items.len(), 1);
        let TailItem::Record(record) = &result.items[0] else {
            panic!("expected accepted first record");
        };
        assert_eq!(record.payload.len(), RECORD_PAYLOAD_BYTES);
        assert_eq!(
            result.checkpoint.committed_offset,
            RECORD_PAYLOAD_BYTES as u64 + 1
        );
        assert!(result.needs_retry && result.more_available);
        assert_eq!(result.bytes_read, PHYSICAL_READ_CEILING);
    }

    #[test]
    fn checkpoint_encoding_round_trips_and_rejects_corruption() {
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(b"one\npartial").unwrap();
        file.flush().unwrap();
        let result = batch(tailer().read(file.path(), None, false).unwrap());
        let checkpoint = result.checkpoint;
        assert_eq!(
            TailCheckpoint::decode(&checkpoint.encode()).unwrap(),
            checkpoint
        );
        assert_eq!(
            TailCheckpoint::decode_hex(&checkpoint.encode_hex()).unwrap(),
            checkpoint
        );

        let mut corrupt = checkpoint.encode();
        corrupt.push(1);
        assert!(TailCheckpoint::decode(&corrupt).is_err());

        let mut wrong_version = checkpoint.encode();
        wrong_version[CHECKPOINT_MAGIC.len()] = 9;
        assert!(TailCheckpoint::decode(&wrong_version).is_err());

        assert!(TailCheckpoint::decode_hex("zz").is_err());
        assert!(TailCheckpoint::decode_hex("abc").is_err());
    }

    #[test]
    fn missing_file_is_reported_without_advancing_state() {
        let directory = TempDir::new().unwrap();
        assert_eq!(
            tailer()
                .read(&directory.path().join("missing"), None, false)
                .unwrap(),
            TailRead::Missing
        );
    }

    #[test]
    fn invalid_bounds_are_rejected_up_front() {
        let mut zero = TailConfig::json_lines();
        zero.max_record_bytes = 0;
        assert!(matches!(
            Tailer::new(zero),
            Err(CaptureError::InvalidConfig(_))
        ));

        let mut inverted = TailConfig::json_lines();
        inverted.max_record_bytes = inverted.max_batch_bytes + 1;
        assert!(matches!(
            Tailer::new(inverted),
            Err(CaptureError::InvalidConfig(_))
        ));
    }

    #[test]
    fn the_committed_chain_resumes_across_batches_and_verifies() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("chain.jsonl");
        let content = b"first record here\nsecond one\nthird, longer record\n";
        std::fs::write(&path, content).unwrap();

        // Two bounded reads: the chain must fold across the boundary to the
        // same value one straight fold produces.
        let tailer = Tailer::new(TailConfig {
            max_record_bytes: 24,
            max_batch_bytes: 24,
            ..TailConfig::json_lines()
        })
        .unwrap();
        let TailRead::Batch(first) = tailer.read(&path, None, false).unwrap() else {
            panic!("expected a batch");
        };
        assert!(
            first.more_available,
            "the tiny batch bound must split reads"
        );
        let mut checkpoint = first.checkpoint;
        loop {
            let TailRead::Batch(batch) = tailer.read(&path, Some(&checkpoint), false).unwrap()
            else {
                panic!("expected a batch");
            };
            let done = !batch.more_available;
            checkpoint = batch.checkpoint;
            if done {
                break;
            }
        }
        assert_eq!(
            checkpoint.committed_chain,
            Some(chain_bytes(CONTENT_CHAIN_BASIS, content)),
            "the chained fold equals one straight fold over the whole file"
        );

        // Verification answers off that chain without a replay.
        assert_eq!(
            tailer.verify_committed(&path, &checkpoint).unwrap(),
            CommittedVerification::Verified
        );
        let mut rewritten = content.to_vec();
        rewritten[3] = b'z'; // same length, out of any anchor window
        std::fs::write(&path, rewritten).unwrap();
        assert_eq!(
            tailer.verify_committed(&path, &checkpoint).unwrap(),
            CommittedVerification::Mismatch
        );
    }

    #[test]
    fn version_one_checkpoints_still_decode_and_establish_later() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("old.jsonl");
        std::fs::write(&path, b"one record\n").unwrap();
        let tailer = Tailer::new(TailConfig::json_lines()).unwrap();
        let TailRead::Batch(batch) = tailer.read(&path, None, false).unwrap() else {
            panic!("expected a batch");
        };

        // Reconstruct the version-1 wire form: strip the chain suffix and
        // stamp the old version byte.
        let mut v1 = batch.checkpoint.encode();
        match batch.checkpoint.committed_chain {
            Some(_) => v1.truncate(v1.len() - 9),
            None => v1.truncate(v1.len() - 1),
        }
        v1[CHECKPOINT_MAGIC.len()] = 1;
        let decoded = TailCheckpoint::decode(&v1).unwrap();
        assert_eq!(decoded.committed_offset, batch.checkpoint.committed_offset);
        assert_eq!(
            decoded.committed_chain, None,
            "pre-chain blobs carry no chain"
        );

        // The audit's first look at a pre-chain checkpoint establishes it.
        match tailer.verify_committed(&path, &decoded).unwrap() {
            CommittedVerification::Established(chain) => assert_eq!(
                Some(chain),
                batch.checkpoint.committed_chain,
                "the established value is the maintained one"
            ),
            other => panic!("expected Established, got {other:?}"),
        }
    }
}
