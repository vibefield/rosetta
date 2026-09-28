//! The file reads of the metadata phase. Each source is pure bytes in, facts
//! out: bounded head and tail windows (or a stable document snapshot) read
//! from disk, handed to the adapter's skim. Nothing here touches the store,
//! the ledger, or another source, so the reads run on a bounded pool while
//! metadata admission merges their results strictly in inventory order.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;

use rosetta_adapter_support::{AgentAdapter, FileStat, SkimResult, StreamSpec};
use rosetta_capture::ScannedFile;

use crate::instrument::{Instrument, Metric};
use crate::{IngestOptions, PipelineError};

/// Concurrent source reads per metadata pass. The skim is bound by file
/// latency, not CPU: 6,032 Claude Code heads and tails took 6.1 s on one
/// thread and 1.06 s on eight, and 0.81 s on 32 (cold-start audit, E4).
const SKIM_THREADS: usize = 8;

/// [`SKIM_THREADS`], never more than the machine's cores.
pub(crate) fn skim_threads() -> usize {
    std::thread::available_parallelism()
        .map_or(1, std::num::NonZeroUsize::get)
        .min(SKIM_THREADS)
}

/// What reading one source yielded.
#[derive(Debug)]
pub(crate) enum Skimmed {
    /// The file disappeared (or never held still) before it could be read.
    Vanished,
    /// A transcript's skim, after any bounded identity or title fallback.
    Transcript(Box<SkimResult>),
    /// The sessions a stable document snapshot enumerates.
    Document(Vec<SkimResult>),
}

/// Read one JSONL transcript: bounded head and tail windows, then the
/// adapter's skim. A file whose opening record outgrows the head window gets
/// exactly that one bounded record before it can count as unidentified, and
/// an adapter may ask for a bounded run of complete records after the head
/// when an optional fact is still missing.
pub(crate) fn skim_transcript(
    adapter: &dyn AgentAdapter,
    root: &Path,
    file: &ScannedFile,
    stream: &StreamSpec,
    opts: &IngestOptions,
    instrument: &Instrument,
) -> Result<Skimmed, PipelineError> {
    let path = root.join(&file.relative_path);
    let Some(windows) = instrument.time(Metric::SkimNanos, || {
        rosetta_capture::head_tail(&path, opts.skim_head_bytes, opts.skim_tail_bytes)
    })?
    else {
        return Ok(Skimmed::Vanished);
    };
    instrument.add(Metric::FilesSkimmed, 1);
    instrument.add(
        Metric::BytesRead,
        (windows.head.len() + windows.tail.len()) as u64,
    );

    let stat = FileStat {
        relative_path: file.relative_path.clone(),
        size: file.size,
        modified_ms: file.modified_ms,
    };
    let mut skim = adapter.skim(&stream.id, &stat, &windows.head, &windows.tail);
    // Some opening records legitimately exceed the UI skim window (Codex
    // session_meta can embed AGENTS.md). Read exactly one bounded record,
    // never the transcript body, before declaring the file unidentified.
    if skim.session_id.is_none()
        && let Some(mut first) = instrument.time(Metric::SkimNanos, || {
            rosetta_capture::first_record(
                &path,
                opts.tail.delimiter,
                crate::MAX_PROJECTED_RECORD_BYTES,
                opts.tail.normalize_crlf,
            )
        })?
    {
        first.push(opts.tail.delimiter);
        instrument.add(Metric::BytesRead, first.len() as u64);
        skim = adapter.skim(&stream.id, &stat, &first, &windows.tail);
    }
    if opts.skim_fallback_bytes > 0
        && opts.skim_fallback_records > 0
        && adapter.wants_skim_fallback(&stream.id, &skim)
    {
        // Rewind to the beginning of the record clipped by the head so
        // every byte handed to the adapter is a complete JSONL record.
        let start = windows
            .head
            .iter()
            .rposition(|byte| *byte == opts.tail.delimiter)
            .map_or(0, |at| at + 1) as u64;
        if let Some(scan) = instrument.time(Metric::SkimFallbackNanos, || {
            rosetta_capture::scan_records(
                &path,
                start,
                opts.skim_fallback_bytes,
                opts.skim_fallback_records,
                opts.tail.delimiter,
                crate::MAX_PROJECTED_RECORD_BYTES,
                opts.tail.normalize_crlf,
            )
        })? {
            instrument.add(Metric::SkimFallbackBytes, scan.bytes_read);
            instrument.add(Metric::BytesRead, scan.bytes_read);
            for record in scan.records {
                if adapter.absorb_skim_fallback_record(&stream.id, &record, &mut skim) {
                    break;
                }
            }
        }
    }
    Ok(Skimmed::Transcript(Box::new(skim)))
}

/// Read one rewrite-in-place document as a stable whole-file snapshot and
/// let the adapter enumerate the native sessions it describes. An unstable
/// read is retried a bounded number of times, then counts as vanished.
pub(crate) fn read_document(
    adapter: &dyn AgentAdapter,
    root: &Path,
    file: &ScannedFile,
    stream: &StreamSpec,
    opts: &IngestOptions,
    instrument: &Instrument,
) -> Result<Skimmed, PipelineError> {
    let path = root.join(&file.relative_path);
    let document_limit = adapter.document_size_limit(&stream.id, crate::MAX_PROJECTED_RECORD_BYTES);
    let mut retries = 0_u32;
    let snapshot = loop {
        match instrument.time(Metric::SkimNanos, || {
            rosetta_capture::snapshot_document(&path, document_limit)
        })? {
            rosetta_capture::DocumentOutcome::Ready(snapshot) => break snapshot,
            rosetta_capture::DocumentOutcome::Missing => return Ok(Skimmed::Vanished),
            rosetta_capture::DocumentOutcome::Unstable => {
                retries += 1;
                if retries > opts.max_retries {
                    return Ok(Skimmed::Vanished);
                }
            }
            rosetta_capture::DocumentOutcome::Oversized { size, limit } => {
                return Err(PipelineError::Config(format!(
                    "document snapshot {} is {size} bytes, above the {limit}-byte limit",
                    file.relative_path
                )));
            }
        }
    };

    instrument.add(Metric::FilesSkimmed, 1);
    instrument.add(Metric::BytesRead, snapshot.bytes().len() as u64);
    let stat = FileStat {
        relative_path: file.relative_path.clone(),
        size: file.size,
        modified_ms: file.modified_ms,
    };
    Ok(Skimmed::Document(adapter.document_sessions(
        &stream.id,
        &stat,
        snapshot.bytes(),
    )))
}

/// Map `work` over `items` on up to `threads` scoped threads while `consume`
/// takes the results strictly in item order, one per call of the `next` it
/// is handed. Workers claim items in order, so the merge rarely waits long;
/// with a single thread `next` simply computes each result on demand. When
/// `consume` returns, early or not, workers stop after their current item.
pub(crate) fn in_order<T, R, O>(
    items: &[T],
    threads: usize,
    work: impl Fn(&T) -> R + Sync,
    consume: impl FnOnce(&mut dyn FnMut() -> R) -> O,
) -> O
where
    T: Sync,
    R: Send,
{
    let threads = threads.min(items.len());
    if threads <= 1 {
        let mut pending = items.iter();
        return consume(&mut || work(pending.next().expect("one result is taken per item")));
    }
    let claimed = AtomicUsize::new(0);
    let finished = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let (sender, receiver) = mpsc::channel();
        for _ in 0..threads {
            let sender = sender.clone();
            let (claimed, finished, work) = (&claimed, &finished, &work);
            scope.spawn(move || {
                while !finished.load(Ordering::Relaxed) {
                    let index = claimed.fetch_add(1, Ordering::Relaxed);
                    let Some(item) = items.get(index) else {
                        break;
                    };
                    if sender.send((index, work(item))).is_err() {
                        break;
                    }
                }
            });
        }
        drop(sender);
        let mut ready: Vec<Option<R>> = std::iter::repeat_with(|| None).take(items.len()).collect();
        let mut position = 0;
        let output = consume(&mut || {
            while ready[position].is_none() {
                let (index, result) = receiver
                    .recv()
                    .expect("a skim worker stopped before its item was merged");
                ready[index] = Some(result);
            }
            position += 1;
            ready[position - 1].take().expect("received above")
        });
        finished.store(true, Ordering::Relaxed);
        output
    })
}

#[cfg(test)]
#[path = "skim_tests.rs"]
mod tests;
