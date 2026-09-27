//! Verified, atomic container replacement for immutable ledger segments.

use std::fs::File;
use std::io::{self, ErrorKind, Read, Write};
use std::path::Path;

use crate::segment::{SegmentCodec, SegmentReader, SegmentWriter};
use crate::{CaptureError, io_error, ledger};

const COPY_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecompressOutcome {
    Rewritten {
        before: u64,
        after: u64,
    },
    Unchanged {
        bytes: u64,
    },
    /// Torn/corrupt evidence is never rewritten or truncated.
    Torn {
        bytes: u64,
    },
}

/// Rewrite one closed segment, preserving every decoded frame byte. The
/// caller holds the projection lock and excludes this segment from writers.
/// Readers may keep the old file open across the atomic replacement.
pub fn recompress_segment(
    path: &Path,
    codec: SegmentCodec,
    force: bool,
) -> Result<RecompressOutcome, CaptureError> {
    recompress_segment_controlled(path, codec, force, &mut || Ok(()))
}

/// Like [`recompress_segment`], with cancellation/pacing checkpoints before
/// bounded reads in all three passes: source verification, copy/compression,
/// and decoded-copy verification. Returning an error leaves the original
/// untouched and removes any incomplete temporary copy. Use a non-Interrupted
/// error for cancellation, because `Read::read_exact` retries Interrupted.
pub fn recompress_segment_controlled(
    path: &Path,
    codec: SegmentCodec,
    force: bool,
    checkpoint: &mut dyn FnMut() -> io::Result<()>,
) -> Result<RecompressOutcome, CaptureError> {
    checkpoint().map_err(|error| io_error("starting segment compression", path, error))?;
    let original =
        std::fs::metadata(path).map_err(|error| io_error("stating segment", path, error))?;
    let before = original.len();
    let Some(mut source) =
        SegmentReader::open(path).map_err(|error| io_error("opening segment", path, error))?
    else {
        return Ok(RecompressOutcome::Torn { bytes: before });
    };
    if !force && source.is_compressed() == codec.is_compressed() {
        return Ok(RecompressOutcome::Unchanged { bytes: before });
    }
    let expected = ledger::frame_prefix(&mut ControlledReader {
        reader: &mut source,
        checkpoint,
    })
    .map_err(|error| io_error("reading segment", path, error))?;
    if !expected.clean || source.corrupt() {
        return Ok(RecompressOutcome::Torn { bytes: before });
    }
    drop(source);

    let temporary = path.with_extension("rledger.compact");
    match std::fs::remove_file(&temporary) {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => {
            return Err(io_error(
                "removing stale compaction file",
                &temporary,
                error,
            ));
        }
    }
    let verified = write_copy(path, &temporary, codec, expected.bytes, checkpoint)
        .and_then(|()| verify_copy(&temporary, &expected, checkpoint))
        .and_then(|()| {
            checkpoint().map_err(|error| io_error("finishing segment compression", path, error))?;
            let current = std::fs::metadata(path)
                .map_err(|error| io_error("restating segment", path, error))?;
            if current.len() != before || current.modified().ok() != original.modified().ok() {
                return Err(CaptureError::InvalidConfig(format!(
                    "segment {} changed under compaction",
                    path.display()
                )));
            }
            Ok(())
        });
    if let Err(error) = verified {
        let _ = std::fs::remove_file(&temporary);
        return Err(error);
    }
    if let Err(error) = std::fs::rename(&temporary, path) {
        let _ = std::fs::remove_file(&temporary);
        return Err(io_error("installing compacted segment", path, error));
    }
    if let Some(parent) = path.parent() {
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| io_error("syncing segment directory", parent, error))?;
    }
    let after = std::fs::metadata(path)
        .map_err(|error| io_error("stating segment", path, error))?
        .len();
    Ok(RecompressOutcome::Rewritten { before, after })
}

struct ControlledReader<'a, R> {
    reader: R,
    checkpoint: &'a mut dyn FnMut() -> io::Result<()>,
}

impl<R: Read> Read for ControlledReader<'_, R> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        (self.checkpoint)()?;
        let length = bytes.len().min(COPY_BYTES);
        self.reader.read(&mut bytes[..length])
    }
}

fn write_copy(
    path: &Path,
    temporary: &Path,
    codec: SegmentCodec,
    bytes: u64,
    checkpoint: &mut dyn FnMut() -> io::Result<()>,
) -> Result<(), CaptureError> {
    let Some(mut source) =
        SegmentReader::open(path).map_err(|error| io_error("opening segment", path, error))?
    else {
        return Err(CaptureError::InvalidConfig(format!(
            "segment {} changed under compaction",
            path.display()
        )));
    };
    // Bound each compression burst as well as the raw copy buffer.
    let mut writer = SegmentWriter::create(temporary, codec, COPY_BYTES)
        .map_err(|error| io_error("creating compacted segment", temporary, error))?;
    let mut buffer = [0_u8; COPY_BYTES];
    let mut remaining = bytes;
    while remaining != 0 {
        checkpoint().map_err(|error| io_error("pacing segment compression", path, error))?;
        let length = remaining.min(COPY_BYTES as u64) as usize;
        source
            .read_exact(&mut buffer[..length])
            .map_err(|error| io_error("copying segment", path, error))?;
        writer
            .write_all(&buffer[..length])
            .map_err(|error| io_error("writing compacted segment", temporary, error))?;
        remaining -= length as u64;
    }
    checkpoint().map_err(|error| io_error("pacing segment compression", path, error))?;
    writer
        .finish()
        .map_err(|error| io_error("closing compacted segment", temporary, error))?;
    File::open(temporary)
        .and_then(|file| file.sync_data())
        .map_err(|error| io_error("syncing compacted segment", temporary, error))?;
    Ok(())
}

fn verify_copy(
    temporary: &Path,
    expected: &ledger::FramePrefix,
    checkpoint: &mut dyn FnMut() -> io::Result<()>,
) -> Result<(), CaptureError> {
    let Some(mut copy) = SegmentReader::open(temporary)
        .map_err(|error| io_error("opening compacted segment", temporary, error))?
    else {
        return Err(CaptureError::InvalidConfig(format!(
            "compacted segment {} has no readable header",
            temporary.display()
        )));
    };
    let actual = ledger::frame_prefix(&mut ControlledReader {
        reader: &mut copy,
        checkpoint,
    })
    .map_err(|error| io_error("verifying compacted segment", temporary, error))?;
    if copy.corrupt()
        || !actual.clean
        || actual.bytes != expected.bytes
        || actual.digest != expected.digest
    {
        return Err(CaptureError::InvalidConfig(format!(
            "compacted segment {} does not decode to its source ({} of {} bytes, clean {})",
            temporary.display(),
            actual.bytes,
            expected.bytes,
            actual.clean
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
