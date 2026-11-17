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

mod checkpoint_bytes;
mod verification;
use checkpoint_bytes::ByteReader;
use verification::verify_chain;

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
    /// The capture limit: a longer record is `Oversized` — skipped with its
    /// length, its payload never held.
    pub max_record_bytes: usize,
    /// Payload bytes one batch of several records never exceeds. A single
    /// record larger than this is a batch of its own.
    pub max_batch_bytes: usize,
    pub max_records_per_batch: usize,
    pub prefix_anchor_bytes: usize,
    /// Close the batch at the first record boundary at or past this many
    /// payload bytes. Unlike `max_batch_bytes`, which closes a batch before
    /// a record that would push it past the cap, the soft cap always admits
    /// the record that crosses it, so it may be arbitrarily small. This is how
    /// a budgeted slice gets byte-sized batches without shrinking
    /// `max_record_bytes` and turning large valid records into oversized
    /// drift. `None` (the default) leaves batching to the hard bounds alone.
    pub soft_batch_bytes: Option<usize>,
}

impl TailConfig {
    pub fn json_lines() -> Self {
        Self {
            delimiter: b'\n',
            normalize_crlf: true,
            // Transcript records can legitimately carry pasted images or
            // other inline media. Capture anything the raw ledger can keep
            // whole, so a valid record is never an irreversible oversized
            // drift marker; what the pipeline normalizes is its own, smaller
            // limit.
            max_record_bytes: crate::ledger::MAX_RAW_RECORD_BYTES,
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
    /// The length this read treated as the file's: its physical length, or
    /// the logical end [`Tailer::read_cut`] was given when that is shorter.
    pub snapshot_len: u64,
    /// Exact bytes read from the file, including the continuity verify and
    /// checkpoint anchor reads. Metadata stats are not byte payloads.
    pub bytes_read: u64,
    /// The logical end bound this read: the file held bytes past it, which
    /// stay unread as ordinary append growth.
    pub cut_applied: bool,
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
        if config.max_records_per_batch > u32::MAX as usize {
            return Err(CaptureError::InvalidConfig(
                "max_records_per_batch exceeds ordinal range".to_string(),
            ));
        }
        Ok(Self { config })
    }

    // @atlas step live.read
    pub fn read(
        &self,
        path: &Path,
        previous: Option<&TailCheckpoint>,
        force_replay: bool,
    ) -> Result<TailRead, CaptureError> {
        self.read_within(path, previous, force_replay, u64::MAX, None)
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
        self.read_within(path, previous, force_replay, max_read_bytes, None)
    }

    /// [`Tailer::read`] with a logical end of file: an `Initial`,
    /// `Continued` or `ForcedReplay` read treats `min(len, end)` as the
    /// file's length, so it frames, commits and observes nothing past `end`
    /// (a record that straddles it is an incomplete suffix). Any other
    /// transition is a rewrite and reads to the physical length. `None` is
    /// [`Tailer::read`].
    ///
    /// This is how a root-tree cold build stops each transcript at its
    /// metadata cut: the checkpoint's `observed_len` is the cut, so the live
    /// runner reads the growth past it as ordinary append growth.
    pub fn read_cut(
        &self,
        path: &Path,
        previous: Option<&TailCheckpoint>,
        force_replay: bool,
        end: Option<u64>,
    ) -> Result<TailRead, CaptureError> {
        self.read_within(path, previous, force_replay, u64::MAX, end)
    }

    fn read_within(
        &self,
        path: &Path,
        previous: Option<&TailCheckpoint>,
        force_replay: bool,
        max_read_bytes: u64,
        end: Option<u64>,
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
        let metadata = file
            .metadata()
            .map_err(|e| io_error("reading metadata for", path, e))?;
        let identity = file_identity(path, &metadata);
        self.read_handle(
            &mut file,
            path,
            identity,
            previous,
            force_replay,
            max_read_bytes,
            end,
            false,
            None,
        )
    }

    /// A pinned decoded input. Path replacement cannot invalidate this open
    /// snapshot. Representation changes verify the entire committed prefix
    /// before continuing the same generation; older chain-less cursors replay.
    pub fn read_input(
        &self,
        input: &mut crate::input::OpenedInput,
        previous: Option<&TailCheckpoint>,
        force_replay: bool,
        end: Option<u64>,
    ) -> Result<TailRead, CaptureError> {
        self.read_handle(
            &mut input.file,
            &input.path,
            input.identity.clone(),
            previous,
            force_replay,
            u64::MAX,
            end,
            true,
            input.cancel.as_deref(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn read_handle(
        &self,
        file: &mut File,
        path: &Path,
        identity: FileIdentity,
        previous: Option<&TailCheckpoint>,
        force_replay: bool,
        max_read_bytes: u64,
        end: Option<u64>,
        pinned: bool,
        cancel: Option<&std::sync::atomic::AtomicBool>,
    ) -> Result<TailRead, CaptureError> {
        let mut accounting = ReadAccounting::new(max_read_bytes);

        let initial_metadata = file
            .metadata()
            .map_err(|error| io_error("reading metadata for", path, error))?;
        let physical_len = initial_metadata.len();

        let mut equivalent = None;
        if pinned
            && let Some(previous) = previous
            && previous.identity != identity
            && previous.committed_chain.is_some()
            && physical_len >= previous.committed_offset
            && matches!(
                verify_chain(file, path, previous, cancel)?,
                CommittedVerification::Verified
            )
        {
            let mut rebound = previous.clone();
            rebound.identity = identity.clone();
            equivalent = Some(rebound);
        }
        let previous = equivalent.as_ref().or(previous);
        let (generation, start_offset, transition) = self.resume_point(
            file,
            path,
            physical_len,
            &identity,
            previous,
            force_replay,
            &mut accounting,
        )?;
        // The logical end binds only reads of the content it was taken
        // over; a rewrite's new content has no cut. It never ends a read
        // before its own resume point.
        let snapshot_len = match end {
            Some(end)
                if matches!(
                    transition,
                    Transition::Initial | Transition::Continued | Transition::ForcedReplay
                ) =>
            {
                physical_len.min(end.max(start_offset))
            }
            _ => physical_len,
        };
        let cut_applied = snapshot_len < physical_len;
        // Continuations resume the chain the checkpoint carries (None on a
        // pre-chain blob); every read-from-zero transition restarts it.
        let resume_chain = match transition {
            Transition::Continued => previous.and_then(|prev| prev.committed_chain),
            _ => Some(CONTENT_CHAIN_BASIS),
        };

        let framed = self.frame(
            file,
            path,
            start_offset,
            resume_chain,
            snapshot_len,
            generation,
            &mut accounting,
        )?;
        let prefix = self.prefix_anchor(file, path, framed.committed_offset, &mut accounting)?;

        // Stability recheck against both the open handle and the path: a
        // shrink or identity change under our feet means the snapshot cannot
        // be trusted — retry with unchanged state. Growth past a logical end
        // is not a shrink: those bytes were never part of this snapshot.
        let handle_metadata = file
            .metadata()
            .map_err(|error| io_error("rechecking metadata for", path, error))?;
        if handle_metadata.len() < snapshot_len {
            return Ok(TailRead::Retry);
        }
        if !pinned {
            let path_metadata = match std::fs::metadata(path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    return Ok(TailRead::Retry);
                }
                Err(error) => return Err(io_error("rechecking path for", path, error)),
            };
            if file_identity(path, &path_metadata) != identity || path_metadata.len() < snapshot_len
            {
                return Ok(TailRead::Retry);
            }
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
            cut_applied,
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
        verify_chain(&mut file, path, previous, None)
    }

    pub fn verify_input(
        &self,
        input: &mut crate::input::OpenedInput,
        previous: &TailCheckpoint,
    ) -> Result<CommittedVerification, CaptureError> {
        if previous.committed_chain.is_none() && previous.identity != input.identity {
            return Ok(CommittedVerification::IdentityChanged);
        }
        verify_chain(
            &mut input.file,
            &input.path,
            previous,
            input.cancel.as_deref(),
        )
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
                // A record that would push the batch past its cap waits for
                // the next one; a record larger than a whole batch is a batch
                // of its own.
                if !pending.oversized()
                    && !output.items.is_empty()
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
                    || batch_payload_bytes >= self.config.max_batch_bytes
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

#[cfg(test)]
mod cut_tests;

#[cfg(test)]
mod tests;
