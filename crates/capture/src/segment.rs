//! Segment containers for the raw ledger.
//!
//! A segment is the frame stream `ledger.rs` describes — `tag ∙ u32 body-len ∙
//! body ∙ 8-byte blake3` — landing either plain (`RLG1`) or inside one zstd
//! stream (`RLZ1`). The magic decides; the frame loop never learns which.
//! Readers keep both forever, so one ledger may mix containers segment by
//! segment and nothing is ever rebuilt for the format's sake.
//!
//! Why one stream per segment, flushed at recovery boundaries, rather than one
//! compressed unit per record: on real ledgers the median frame is under a
//! kilobyte, and compressing it alone yields about half the ratio (2.1× against
//! 4.6× on claude-code segments, 3.9× against 7.2× on codex). A stream keeps
//! its match window across records, and a flush at each checkpoint closes a
//! zstd block without closing the frame, so everything up to the last
//! checkpoint is decodable while the window survives. Measured at the
//! checkpoint cadence the stream gives up nothing against whole-segment
//! compression.
//! Live checkpoints always drain this stream. The private async cold writer
//! coalesces intermediate raw-batch checkpoints, draining at metadata, source
//! seals, rotation and its final durable publication barrier.
//!
//! The crash contract is the plain one. A process that dies after a checkpoint
//! leaves a stream without its epilogue; the decoder yields every complete
//! block and then reports end of input, and the frame loop applies the
//! frame-boundary rule exactly as it does to a plain file. A corrupt block is
//! different: the decoder rejects it while input remains, and the segment is
//! reported torn wherever the decoded prefix happens to end — which is what a
//! checksum mismatch inside a plain frame reports.
//!
//! A cold build's capture workers may compress instead ([`SegmentCodec::ZstdFrames`]):
//! each capture batch's finished frames become one independent zstd frame
//! where they were framed, and the ledger writes it verbatim; its own small
//! frames (defines, metadata, seals written inline, batches under
//! [`SMALLEST_COMPRESSED_BATCH`]) are staged and leave as one zstd frame at the
//! next flush or precompressed frame. The container is still `RLZ1`: a zstd
//! reader concatenates frames until end of file, and a crash truncates at
//! most the last frame, whose complete blocks still decode. Per-batch frames
//! give up the stream's window across neighbouring sources — on the frozen
//! corpus 5.3× against 5.6× at level 3, almost all of it Claude Code's many
//! one-batch sessions — and cost less CPU per byte than the stream encoder.

use std::fs::{File, OpenOptions};
use std::io::{self, BufReader, BufWriter, ErrorKind, Read, Write};
use std::path::Path;

pub const MAGIC_PLAIN: &[u8; 4] = b"RLG1";
pub const MAGIC_ZSTD: &[u8; 4] = b"RLZ1";
pub const MAGIC_LEN: usize = 4;
/// The write-path level: 1.0–1.6 GB/s on an M1 Max, which is dozens of times
/// the rate the pipeline produces ledger bytes, at 5.6× on a real archive.
pub const DEFAULT_ZSTD_LEVEL: i32 = 3;
/// A capture batch with fewer frame bytes than this stays plain in a
/// zstd-frames segment and is staged with the ledger's own frames: alone it
/// would compress about 2× and pay a frame header.
pub const SMALLEST_COMPRESSED_BATCH: usize = 4 * 1024;
/// Staged plain bytes of a zstd-frames segment leave as one frame at the
/// latest when this many are staged.
const STAGED_FRAME_BYTES: usize = 1024 * 1024;
/// The widest window a stock zstd decoder accepts without configuration
/// (`ZSTD_WINDOWLOG_LIMIT_DEFAULT`). Offline recompression may use it; the
/// write path never does — it costs 128 MiB per open stream and half the speed
/// for about 7% of ratio. The window is paid on the read side too: every
/// decoder of such a segment allocates it, where a level-3 segment needs 2 MiB.
pub const MAX_WINDOW_LOG: u32 = 27;

const READ_BUFFER_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentCodec {
    Plain,
    /// One zstd stream per segment.
    Zstd {
        level: i32,
        long_window: bool,
    },
    /// Independent zstd frames: one per capture batch, compressed by
    /// whoever framed it, plus the ledger's own staged small frames.
    ZstdFrames {
        level: i32,
    },
}

impl Default for SegmentCodec {
    fn default() -> Self {
        Self::Zstd {
            level: DEFAULT_ZSTD_LEVEL,
            long_window: false,
        }
    }
}

impl SegmentCodec {
    pub fn zstd(level: i32) -> Self {
        Self::Zstd {
            level,
            long_window: false,
        }
    }

    pub fn magic(self) -> &'static [u8; 4] {
        match self {
            Self::Plain => MAGIC_PLAIN,
            Self::Zstd { .. } | Self::ZstdFrames { .. } => MAGIC_ZSTD,
        }
    }

    pub fn is_compressed(self) -> bool {
        !matches!(self, Self::Plain)
    }

    /// The level capture batches are compressed at before they reach the
    /// ledger, when segments are zstd frames.
    pub fn batch_level(self) -> Option<i32> {
        match self {
            Self::ZstdFrames { level } => Some(level),
            Self::Plain | Self::Zstd { .. } => None,
        }
    }
}

/// Compresses bytes into one zstd frame, from a context reused across calls
/// and owned by one thread — a capture worker, or a zstd-frames segment's
/// writer. The frame is a pure function of the bytes and the level, so a
/// batch a worker compressed is byte-identical to the same batch compressed
/// by the ledger.
pub struct FrameCompressor {
    level: i32,
    context: zstd::bulk::Compressor<'static>,
}

impl FrameCompressor {
    pub fn new(level: i32) -> io::Result<Self> {
        Ok(Self {
            level,
            context: zstd::bulk::Compressor::new(level)?,
        })
    }

    pub fn level(&self) -> i32 {
        self.level
    }

    /// One zstd frame holding `bytes`, in an allocation of its own size.
    pub fn frame(&mut self, bytes: &[u8]) -> io::Result<Vec<u8>> {
        let mut frame = self.context.compress(bytes)?;
        frame.shrink_to_fit();
        Ok(frame)
    }
}

/// Counts bytes reaching the file so write stats can report on-disk size.
pub(crate) struct CountingFile {
    file: File,
    bytes: u64,
}

impl Write for CountingFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let written = self.file.write(buf)?;
        self.bytes = self.bytes.saturating_add(written as u64);
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

/// The append side of one segment. The raw-byte buffer sits in front of the
/// container so a compressed segment sees the same amortized writes a plain
/// one does; `flush` drains it and, for zstd, ends the current block.
pub(crate) enum SegmentWriter {
    Plain(BufWriter<CountingFile>),
    Zstd(BufWriter<zstd::Encoder<'static, CountingFile>>),
    Frames(FramesWriter),
}

/// A zstd-frames segment: precompressed frames go to the file in order, and
/// plain bytes are staged until they leave as one frame of their own.
pub(crate) struct FramesWriter {
    file: BufWriter<CountingFile>,
    staged: Vec<u8>,
    compressor: FrameCompressor,
}

impl FramesWriter {
    fn stage(&mut self, mut bytes: &[u8]) -> io::Result<()> {
        while !bytes.is_empty() {
            let take = (STAGED_FRAME_BYTES - self.staged.len()).min(bytes.len());
            self.staged.extend_from_slice(&bytes[..take]);
            bytes = &bytes[take..];
            if self.staged.len() == STAGED_FRAME_BYTES {
                self.emit_staged()?;
            }
        }
        Ok(())
    }

    /// Staged bytes first, so the decoded order is the order of the calls.
    fn emit_staged(&mut self) -> io::Result<()> {
        if self.staged.is_empty() {
            return Ok(());
        }
        let frame = self.compressor.frame(&self.staged)?;
        self.staged.clear();
        self.file.write_all(&frame)
    }

    fn write_frame(&mut self, frame: &[u8]) -> io::Result<()> {
        self.emit_staged()?;
        self.file.write_all(frame)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.emit_staged()?;
        self.file.flush()
    }
}

impl SegmentWriter {
    /// Create the segment (it must not exist) and write its magic.
    pub(crate) fn create(
        path: &Path,
        codec: SegmentCodec,
        buffer_bytes: usize,
    ) -> io::Result<Self> {
        let mut file = OpenOptions::new()
            .create_new(true)
            .append(true)
            .open(path)?;
        file.write_all(codec.magic())?;
        let file = CountingFile {
            file,
            bytes: MAGIC_LEN as u64,
        };
        match codec {
            SegmentCodec::Plain => Ok(Self::Plain(BufWriter::with_capacity(buffer_bytes, file))),
            SegmentCodec::Zstd { level, long_window } => {
                let mut encoder = zstd::Encoder::new(file, level)?;
                if long_window {
                    encoder.long_distance_matching(true)?;
                    encoder.window_log(MAX_WINDOW_LOG)?;
                }
                Ok(Self::Zstd(BufWriter::with_capacity(buffer_bytes, encoder)))
            }
            SegmentCodec::ZstdFrames { level } => Ok(Self::Frames(FramesWriter {
                file: BufWriter::with_capacity(buffer_bytes, file),
                staged: Vec::new(),
                compressor: FrameCompressor::new(level)?,
            })),
        }
    }

    /// The compressor capture batches of this segment use, when they are
    /// compressed at all: a batch that reaches the ledger plain is compressed
    /// here exactly as a capture worker would have.
    pub(crate) fn batch_compressor(&mut self) -> Option<&mut FrameCompressor> {
        match self {
            Self::Frames(writer) => Some(&mut writer.compressor),
            Self::Plain(_) | Self::Zstd(_) => None,
        }
    }

    /// Append one zstd frame compressed ahead of the ledger. Only a
    /// zstd-frames segment takes one.
    pub(crate) fn write_zstd_frame(&mut self, frame: &[u8]) -> io::Result<()> {
        match self {
            Self::Frames(writer) => writer.write_frame(frame),
            Self::Plain(_) | Self::Zstd(_) => Err(io::Error::new(
                ErrorKind::InvalidInput,
                "a compressed capture batch needs a zstd-frames segment",
            )),
        }
    }

    /// Bytes that have reached the file so far (not necessarily durable).
    pub(crate) fn disk_bytes(&self) -> u64 {
        match self {
            Self::Plain(writer) => writer.get_ref().bytes,
            Self::Zstd(writer) => writer.get_ref().get_ref().bytes,
            Self::Frames(writer) => writer.file.get_ref().bytes,
        }
    }

    /// Close the container: a plain segment drains, a zstd segment writes its
    /// frame epilogue, a zstd-frames segment emits its staged frame. Returns
    /// the segment's final size on disk.
    pub(crate) fn finish(self) -> io::Result<u64> {
        match self {
            Self::Plain(mut writer) => {
                writer.flush()?;
                Ok(writer.get_ref().bytes)
            }
            Self::Zstd(writer) => {
                let encoder = writer.into_inner().map_err(|error| error.into_error())?;
                let file = encoder.finish()?;
                Ok(file.bytes)
            }
            Self::Frames(mut writer) => {
                writer.flush()?;
                Ok(writer.file.get_ref().bytes)
            }
        }
    }
}

impl Write for SegmentWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Self::Plain(writer) => writer.write(buf),
            Self::Zstd(writer) => writer.write(buf),
            Self::Frames(writer) => writer.stage(buf).map(|()| buf.len()),
        }
    }

    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        match self {
            Self::Plain(writer) => writer.write_all(buf),
            Self::Zstd(writer) => writer.write_all(buf),
            Self::Frames(writer) => writer.stage(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Plain(writer) => writer.flush(),
            Self::Zstd(writer) => writer.flush(),
            Self::Frames(writer) => writer.flush(),
        }
    }
}

/// Turns a zstd decoder's end-of-data conditions into what the frame loop
/// expects. A truncated stream reads as end of file (the decoded prefix ends
/// at the last complete block). A corrupt stream also reads as end of file
/// but is flagged, so the scan reports the segment torn wherever the prefix
/// ends. Genuine I/O errors pass through untouched.
pub(crate) struct DecodeGuard<R> {
    inner: R,
    corrupt: bool,
}

impl<R: Read> Read for DecodeGuard<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.corrupt {
            return Ok(0);
        }
        match self.inner.read(buf) {
            Ok(read) => Ok(read),
            Err(error) if error.kind() == ErrorKind::UnexpectedEof => Ok(0),
            // zstd reports its own failures as `Other`; the operating system
            // never does for a read that was going fine until now.
            Err(error) if error.kind() == ErrorKind::Other => {
                self.corrupt = true;
                Ok(0)
            }
            Err(error) => Err(error),
        }
    }
}

/// The read side of one segment, container decided by its magic.
pub(crate) enum SegmentReader {
    Plain(BufReader<File>),
    Zstd(BufReader<DecodeGuard<zstd::Decoder<'static, BufReader<File>>>>),
}

impl SegmentReader {
    /// `Ok(None)` when the header is short or unknown: the segment is torn
    /// from its first byte and nothing in it can be trusted.
    pub(crate) fn open(path: &Path) -> io::Result<Option<Self>> {
        let mut file = File::open(path)?;
        let mut magic = [0_u8; MAGIC_LEN];
        match file.read_exact(&mut magic) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::UnexpectedEof => return Ok(None),
            Err(error) => return Err(error),
        }
        if &magic == MAGIC_PLAIN {
            return Ok(Some(Self::Plain(BufReader::with_capacity(
                READ_BUFFER_BYTES,
                file,
            ))));
        }
        if &magic == MAGIC_ZSTD {
            let decoder =
                zstd::Decoder::with_buffer(BufReader::with_capacity(READ_BUFFER_BYTES, file))?;
            // Small frame heads read from this buffer; a body larger than it
            // bypasses straight into the caller's allocation.
            return Ok(Some(Self::Zstd(BufReader::with_capacity(
                READ_BUFFER_BYTES,
                DecodeGuard {
                    inner: decoder,
                    corrupt: false,
                },
            ))));
        }
        Ok(None)
    }

    pub(crate) fn is_compressed(&self) -> bool {
        matches!(self, Self::Zstd(_))
    }

    /// True once the decoder rejected its input: the scan must report torn.
    pub(crate) fn corrupt(&self) -> bool {
        match self {
            Self::Plain(_) => false,
            Self::Zstd(reader) => reader.get_ref().corrupt,
        }
    }
}

impl Read for SegmentReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Plain(reader) => reader.read(buf),
            Self::Zstd(reader) => reader.read(buf),
        }
    }
}

/// Which container a segment file uses, from its magic alone. `None` when the
/// header is short or unknown. Cheap: four bytes, no decoding.
pub fn sniff_segment(path: &Path) -> io::Result<Option<SegmentCodec>> {
    let mut file = File::open(path)?;
    let mut magic = [0_u8; MAGIC_LEN];
    match file.read_exact(&mut magic) {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error),
    }
    Ok(if &magic == MAGIC_PLAIN {
        Some(SegmentCodec::Plain)
    } else if &magic == MAGIC_ZSTD {
        Some(SegmentCodec::default())
    } else {
        None
    })
}

// Retain the original public module path for offline callers.
pub use crate::recompress::{RecompressOutcome, recompress_segment};

#[cfg(test)]
mod frames_tests;
#[cfg(test)]
mod tests {
    use std::fs::OpenOptions;
    use std::io::Write;

    use tempfile::TempDir;

    use super::*;
    use crate::ledger::{Ledger, LedgerEntry, SourceDef, scan};

    fn source(path: &str) -> SourceDef {
        SourceDef {
            ledger_only: false,
            relative_path: path.to_string(),
            stream_id: format!("stream:{path}"),
            session_id: format!("session:{path}"),
            native_session_id: format!("native:{path}"),
            agent_version: Some("1.0".to_string()),
        }
    }

    fn payloads(dir: &Path) -> (Vec<Vec<u8>>, usize, usize) {
        let scanned = scan(dir).unwrap();
        let raw = scanned
            .entries
            .iter()
            .filter_map(|entry| match entry {
                LedgerEntry::Raw { payload, .. } => Some(payload.clone()),
                _ => None,
            })
            .collect();
        (raw, scanned.segments, scanned.torn_segments)
    }

    /// Records with enough repetition that the container actually shrinks.
    fn record(index: u64) -> Vec<u8> {
        format!(
            "{{\"type\":\"assistant\",\"index\":{index},\"text\":\"the quick brown fox jumps over the lazy dog {index}\"}}\n"
        )
        .into_bytes()
    }

    fn magic_of(path: &Path) -> [u8; 4] {
        let bytes = std::fs::read(path).unwrap();
        bytes[..MAGIC_LEN].try_into().unwrap()
    }

    #[test]
    fn default_container_is_zstd_and_round_trips_every_entry_kind() {
        let dir = TempDir::new().unwrap();
        let mut ledger = Ledger::open(dir.path()).unwrap();
        for index in 0..200_u64 {
            ledger
                .append_raw(&source("a.jsonl"), 1, [(index * 100, record(index))])
                .unwrap();
            if index % 50 == 0 {
                ledger.checkpoint().unwrap();
            }
        }
        ledger.append_meta(7, "{\"ops\":[]}").unwrap();
        ledger
            .complete_generation(&source("a.jsonl"), 1, 3, true)
            .unwrap();
        ledger.flush().unwrap();
        let stats = ledger.take_write_stats();
        drop(ledger);

        let segment = dir.path().join("00000000.rledger");
        assert_eq!(&magic_of(&segment), MAGIC_ZSTD);
        let on_disk = std::fs::metadata(&segment).unwrap().len();
        assert!(
            on_disk * 3 < stats.frame_bytes,
            "expected real compression: {on_disk} on disk for {} frame bytes",
            stats.frame_bytes
        );
        // The stats were sampled at the barrier; the drop then wrote the
        // stream's epilogue (a 3-byte empty block), which is the only thing a
        // still-open segment has not reported yet.
        assert!(
            stats.disk_bytes <= on_disk && on_disk - stats.disk_bytes <= 8,
            "disk_bytes {} should trail the container {on_disk} by at most an epilogue",
            stats.disk_bytes
        );

        let scanned = scan(dir.path()).unwrap();
        assert_eq!(scanned.torn_segments, 0);
        let raw: Vec<_> = scanned
            .entries
            .iter()
            .filter(|entry| matches!(entry, LedgerEntry::Raw { .. }))
            .collect();
        assert_eq!(raw.len(), 200);
        assert!(matches!(
            raw[199],
            LedgerEntry::Raw {
                start_offset: 19_900,
                ..
            }
        ));
        assert!(scanned.entries.iter().any(|entry| matches!(
            entry,
            LedgerEntry::Meta {
                schema_version: 7,
                ..
            }
        )));
        assert!(scanned.entries.iter().any(|entry| matches!(
            entry,
            LedgerEntry::GenerationComplete {
                normalizer_version: 3,
                normalized_clean: true,
                ..
            }
        )));
    }

    #[test]
    fn a_kill_after_a_checkpoint_keeps_every_checkpointed_record() {
        let dir = TempDir::new().unwrap();
        let mut ledger = Ledger::open(dir.path()).unwrap();
        for index in 0..100_u64 {
            ledger
                .append_raw(&source("a.jsonl"), 1, [(index * 100, record(index))])
                .unwrap();
        }
        ledger.checkpoint().unwrap();
        for index in 100..140_u64 {
            ledger
                .append_raw(&source("a.jsonl"), 1, [(index * 100, record(index))])
                .unwrap();
        }
        ledger.checkpoint().unwrap();
        // Buffered, never checkpointed — and the process "dies" here: no
        // finish, no epilogue.
        for index in 140..160_u64 {
            ledger
                .append_raw(&source("a.jsonl"), 1, [(index * 100, record(index))])
                .unwrap();
        }
        std::mem::forget(ledger);

        let (raw, segments, torn) = payloads(dir.path());
        assert_eq!(segments, 1);
        assert_eq!(torn, 0, "a stream cut at a flush ends at a frame boundary");
        assert_eq!(raw.len(), 140);
        assert_eq!(raw[139], record(139));
    }

    #[test]
    fn a_truncated_stream_keeps_a_prefix_and_a_corrupt_one_is_torn() {
        let dir = TempDir::new().unwrap();
        let mut ledger = Ledger::open(dir.path()).unwrap();
        for index in 0..400_u64 {
            ledger
                .append_raw(&source("a.jsonl"), 1, [(index * 100, record(index))])
                .unwrap();
            if index % 40 == 39 {
                ledger.checkpoint().unwrap();
            }
        }
        ledger.flush().unwrap();
        drop(ledger);

        let segment = dir.path().join("00000000.rledger");
        let whole = std::fs::read(&segment).unwrap();
        let (all, _, torn) = payloads(dir.path());
        assert_eq!(torn, 0);
        assert_eq!(all.len(), 400);

        // A crash right after the segment was created leaves the magic and
        // nothing else: an empty stream, clean, holding no records.
        std::fs::write(&segment, &whole[..MAGIC_LEN]).unwrap();
        let (raw, segments, torn) = payloads(dir.path());
        assert_eq!((raw.len(), segments, torn), (0, 1, 0));

        // Cut the container at several points: the visible records are always
        // a prefix, in order, and never fabricated.
        for cut in [
            MAGIC_LEN + 1,
            MAGIC_LEN + 4,
            whole.len() / 3,
            whole.len() / 2,
            whole.len() - 3,
        ] {
            std::fs::write(&segment, &whole[..cut]).unwrap();
            let (raw, _, _) = payloads(dir.path());
            assert!(raw.len() <= 400);
            assert_eq!(raw, all[..raw.len()], "cut at {cut}");
        }

        // Flip a byte deep inside the stream: whatever survives is a prefix,
        // and the segment is torn even though the decoded bytes may end on a
        // frame boundary.
        let mut flipped = whole.clone();
        let at = whole.len() / 2;
        flipped[at] ^= 0x5A;
        std::fs::write(&segment, &flipped).unwrap();
        let (raw, _, torn) = payloads(dir.path());
        assert_eq!(torn, 1);
        assert!(raw.len() < 400);
        assert_eq!(raw, all[..raw.len()]);
    }

    #[test]
    fn rotation_finishes_each_container_and_readers_accept_mixed_ledgers() {
        let dir = TempDir::new().unwrap();
        let mut ledger = Ledger::open(dir.path())
            .unwrap()
            .with_codec(SegmentCodec::Plain)
            .with_max_segment_bytes(256);
        for index in 0..6_u64 {
            ledger
                .append_raw(&source("a.jsonl"), 1, [(index * 100, record(index))])
                .unwrap();
            ledger.flush().unwrap();
        }
        drop(ledger);
        let plain_segments = scan(dir.path()).unwrap().segments;
        assert!(plain_segments > 1);

        let mut ledger = Ledger::open(dir.path())
            .unwrap()
            .with_max_segment_bytes(256);
        for index in 6..12_u64 {
            ledger
                .append_raw(&source("a.jsonl"), 1, [(index * 100, record(index))])
                .unwrap();
            ledger.flush().unwrap();
        }
        drop(ledger);

        let (raw, segments, torn) = payloads(dir.path());
        assert!(segments > plain_segments);
        assert_eq!(torn, 0, "every rotated container carries its epilogue");
        assert_eq!(raw, (0..12).map(record).collect::<Vec<_>>());
        let magics: Vec<[u8; 4]> = (0..segments)
            .map(|seq| magic_of(&dir.path().join(format!("{seq:08}.rledger"))))
            .collect();
        assert_eq!(&magics[0], MAGIC_PLAIN);
        assert_eq!(magics.last().unwrap(), MAGIC_ZSTD);
    }

    #[test]
    fn asynchronous_writer_compresses_and_joins_at_flush() {
        let dir = TempDir::new().unwrap();
        let mut ledger = Ledger::open(dir.path()).unwrap().into_async_bounded(2);
        for index in 0..50_u64 {
            ledger
                .append_raw_owned(&source("a.jsonl"), 1, vec![(index * 100, record(index))])
                .unwrap();
            ledger.checkpoint().unwrap();
        }
        ledger.flush().unwrap();
        drop(ledger);
        assert_eq!(&magic_of(&dir.path().join("00000000.rledger")), MAGIC_ZSTD);
        let (raw, _, torn) = payloads(dir.path());
        assert_eq!(torn, 0);
        assert_eq!(raw, (0..50).map(record).collect::<Vec<_>>());
    }

    #[test]
    fn recompression_rewrites_plain_segments_in_place_and_leaves_torn_ones() {
        let dir = TempDir::new().unwrap();
        let mut ledger = Ledger::open(dir.path())
            .unwrap()
            .with_codec(SegmentCodec::Plain)
            .with_max_segment_bytes(8 * 1024);
        for index in 0..300_u64 {
            ledger
                .append_raw(&source("a.jsonl"), 1, [(index * 100, record(index))])
                .unwrap();
            if index % 50 == 49 {
                ledger.checkpoint().unwrap(); // rotation happens at checkpoints
            }
        }
        ledger.append_meta(7, "{\"ops\":[]}").unwrap();
        ledger.flush().unwrap();
        drop(ledger);
        let before = scan(dir.path()).unwrap();
        assert!(before.segments > 1);

        // Tear the last segment: a truncated frame header after the durable tail.
        let last = dir
            .path()
            .join(format!("{:08}.rledger", before.segments - 1));
        let mut file = OpenOptions::new().append(true).open(&last).unwrap();
        file.write_all(&[2, 0, 0, 1]).unwrap();
        drop(file);

        let mut rewritten = 0;
        let mut torn = 0;
        for seq in 0..before.segments {
            let path = dir.path().join(format!("{seq:08}.rledger"));
            match recompress_segment(&path, SegmentCodec::default(), false).unwrap() {
                RecompressOutcome::Rewritten { before, after } => {
                    assert!(after < before, "{seq}: {after} vs {before}");
                    assert_eq!(&magic_of(&path), MAGIC_ZSTD);
                    rewritten += 1;
                }
                RecompressOutcome::Torn { .. } => {
                    assert_eq!(&magic_of(&path), MAGIC_PLAIN);
                    torn += 1;
                }
                RecompressOutcome::Unchanged { .. } => panic!("plain segments must be rewritten"),
            }
            assert!(!path.with_extension("rledger.compact").exists());
        }
        assert_eq!(torn, 1);
        assert_eq!(rewritten, before.segments - 1);

        let after = scan(dir.path()).unwrap();
        assert_eq!(
            after.entries, before.entries,
            "compaction is byte-transparent"
        );
        assert_eq!(after.torn_segments, 1);

        // Idempotent unless forced; forced rewrites again (e.g. a wider window).
        let first = dir.path().join("00000000.rledger");
        assert!(matches!(
            recompress_segment(&first, SegmentCodec::default(), false).unwrap(),
            RecompressOutcome::Unchanged { .. }
        ));
        let wide = SegmentCodec::Zstd {
            level: 10,
            long_window: true,
        };
        assert!(matches!(
            recompress_segment(&first, wide, true).unwrap(),
            RecompressOutcome::Rewritten { .. }
        ));
        assert_eq!(scan(dir.path()).unwrap().entries, before.entries);
    }
}
