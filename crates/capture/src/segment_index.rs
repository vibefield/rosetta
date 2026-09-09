//! The segment index: a per-runtime sidecar saying which closed segments hold
//! which (session, source path, generation), so a read scoped to one source
//! generation opens the segments that matter instead of the whole ledger.
//!
//! Derived and disposable. The writer appends one line when it closes a
//! segment; `rebuild_segment_index` regenerates the file from the segments
//! themselves. The index only ever narrows a read: a segment it does not
//! list — the open one, one closed by a crash, or all of them when the file
//! is missing or a line was cut mid-write — is always a candidate. A wrong
//! listing costs a scan; there is no way for it to hide data.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::ledger::{self, LedgerEntry, SourceDef};
use crate::{CaptureError, io_error};

pub const INDEX_FILE: &str = ".segments";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexedSource {
    pub session_id: String,
    pub relative_path: String,
    /// Inclusive generation range the segment holds for this source.
    pub generations: [u64; 2],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SegmentIndexLine {
    pub segment: u64,
    pub sources: Vec<IndexedSource>,
}

/// Append one line. No sync: the line is a cache entry, and losing it only
/// means the segment is scanned rather than skipped.
pub(crate) fn append_line(dir: &Path, line: &SegmentIndexLine) -> std::io::Result<()> {
    let mut encoded = serde_json::to_vec(line).map_err(std::io::Error::other)?;
    encoded.push(b'\n');
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(INDEX_FILE))?;
    file.write_all(&encoded)
}

fn read_lines(dir: &Path) -> Result<Vec<SegmentIndexLine>, CaptureError> {
    let path = dir.join(INDEX_FILE);
    let file = match File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(io_error("opening segment index", &path, error)),
    };
    let mut lines = Vec::new();
    for line in BufReader::new(file).split(b'\n') {
        let line = line.map_err(|error| io_error("reading segment index", &path, error))?;
        if line.is_empty() {
            continue;
        }
        // A line cut mid-append or of an unknown shape is skipped; its
        // segment then reads as unindexed, which is the safe direction.
        if let Ok(parsed) = serde_json::from_slice::<SegmentIndexLine>(&line) {
            lines.push(parsed);
        }
    }
    Ok(lines)
}

/// What the index says about one source generation: the segments it lists
/// as holding it, and the segments it knows at all. A walk applies this to
/// its own directory listing, so a segment that appears between reading the
/// index and listing the directory — the writer rotating meanwhile — is
/// unknown to the filter and therefore visited.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SegmentFilter {
    matched: BTreeSet<u64>,
    indexed: BTreeSet<u64>,
}

impl SegmentFilter {
    /// True when the read must visit `segment`: the index lists it for the
    /// source generation, or does not know it.
    pub fn admits(&self, segment: u64) -> bool {
        self.matched.contains(&segment) || !self.indexed.contains(&segment)
    }
}

/// Read the index for one source generation. Cheap: one small file, no
/// segment is opened.
pub fn segment_filter(
    dir: &Path,
    session_id: &str,
    relative_path: &str,
    generation: u64,
) -> Result<SegmentFilter, CaptureError> {
    let mut filter = SegmentFilter::default();
    for line in read_lines(dir)? {
        filter.indexed.insert(line.segment);
        let holds = line.sources.iter().any(|source| {
            source.session_id == session_id
                && source.relative_path == relative_path
                && source.generations[0] <= generation
                && generation <= source.generations[1]
        });
        if holds {
            filter.matched.insert(line.segment);
        }
    }
    Ok(filter)
}

/// The segments on disk right now that a read scoped to one source
/// generation must visit. A walk should prefer [`segment_filter`] against its
/// own listing; this is the same answer for a directory nobody is writing.
pub fn candidate_segments(
    dir: &Path,
    session_id: &str,
    relative_path: &str,
    generation: u64,
) -> Result<BTreeSet<u64>, CaptureError> {
    let filter = segment_filter(dir, session_id, relative_path, generation)?;
    Ok(ledger::segment_files(dir)?
        .into_iter()
        .map(|(seq, _)| seq)
        .filter(|seq| filter.admits(*seq))
        .collect())
}

/// Regenerate the index from the segments (after compaction, or for a ledger
/// written before the index existed). Every segment must be closed. Written
/// beside the old file and renamed over it. Returns the number of segments
/// indexed.
pub fn rebuild_segment_index(dir: &Path) -> Result<usize, CaptureError> {
    let mut lines = Vec::new();
    for (seq, path) in ledger::segment_files(dir)? {
        let mut ranges: BTreeMap<(String, String), (u64, u64)> = BTreeMap::new();
        let mut note = |source: &SourceDef, generation: u64| {
            let range = ranges
                .entry((source.session_id.clone(), source.relative_path.clone()))
                .or_insert((generation, generation));
            range.0 = range.0.min(generation);
            range.1 = range.1.max(generation);
        };
        ledger::scan_segment::<CaptureError, _>(&path, &mut |entry| {
            match entry {
                LedgerEntry::Raw {
                    source, generation, ..
                }
                | LedgerEntry::GenerationComplete {
                    source, generation, ..
                } => note(&source, generation),
                LedgerEntry::Meta { .. } => {}
            }
            Ok(())
        })?;
        lines.push(SegmentIndexLine {
            segment: seq,
            sources: ranges
                .into_iter()
                .map(|((session_id, relative_path), (lo, hi))| IndexedSource {
                    session_id,
                    relative_path,
                    generations: [lo, hi],
                })
                .collect(),
        });
    }

    let path = dir.join(INDEX_FILE);
    let temporary = dir.join(format!("{INDEX_FILE}.tmp-{}", std::process::id()));
    let written = (|| {
        let mut file = File::create(&temporary)?;
        for line in &lines {
            let mut encoded = serde_json::to_vec(line).map_err(std::io::Error::other)?;
            encoded.push(b'\n');
            file.write_all(&encoded)?;
        }
        file.sync_data()
    })();
    if let Err(error) = written {
        let _ = std::fs::remove_file(&temporary);
        return Err(io_error("writing segment index", &temporary, error));
    }
    std::fs::rename(&temporary, &path)
        .map_err(|error| io_error("installing segment index", &path, error))?;
    Ok(lines.len())
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::ledger::Ledger;

    fn source(session: &str, path: &str) -> SourceDef {
        SourceDef {
            ledger_only: false,
            relative_path: path.to_string(),
            stream_id: "transcript".to_string(),
            session_id: session.to_string(),
            native_session_id: format!("native:{session}"),
            agent_version: Some("1.0".to_string()),
        }
    }

    /// One segment per flush: the cap is below any frame.
    fn open_ledger(dir: &Path) -> Ledger {
        Ledger::open(dir).unwrap().with_max_segment_bytes(1)
    }

    fn seqs(set: BTreeSet<u64>) -> Vec<u64> {
        set.into_iter().collect()
    }

    #[test]
    fn writer_indexes_each_closed_segment_and_reads_narrow_to_them() {
        let dir = TempDir::new().unwrap();
        let mut ledger = open_ledger(dir.path());
        // segment 0: A gen 1; segment 1: B gen 1 and 2; segment 2: A gen 3.
        ledger
            .append_raw(&source("A", "a.jsonl"), 1, [(0_u64, b"a1".as_slice())])
            .unwrap();
        ledger.flush().unwrap();
        ledger
            .append_raw(&source("B", "b.jsonl"), 1, [(0_u64, b"b1".as_slice())])
            .unwrap();
        ledger
            .append_raw(&source("B", "b.jsonl"), 2, [(0_u64, b"b2".as_slice())])
            .unwrap();
        ledger.flush().unwrap();
        ledger
            .append_raw(&source("A", "a.jsonl"), 3, [(0_u64, b"a3".as_slice())])
            .unwrap();
        drop(ledger); // the drop closes segment 2 and indexes it

        let lines = read_lines(dir.path()).unwrap();
        assert_eq!(
            lines.iter().map(|line| line.segment).collect::<Vec<_>>(),
            [0, 1, 2]
        );
        assert_eq!(
            lines[1].sources,
            [IndexedSource {
                session_id: "B".into(),
                relative_path: "b.jsonl".into(),
                generations: [1, 2],
            }]
        );

        let a =
            |generation| seqs(candidate_segments(dir.path(), "A", "a.jsonl", generation).unwrap());
        assert_eq!(a(1), [0]);
        assert_eq!(a(3), [2]);
        assert_eq!(a(2), Vec::<u64>::new(), "fully indexed: nothing to scan");
        assert_eq!(
            seqs(candidate_segments(dir.path(), "B", "b.jsonl", 2).unwrap()),
            [1]
        );
    }

    #[test]
    fn unindexed_and_cut_lines_widen_reads_safely() {
        let dir = TempDir::new().unwrap();
        let mut ledger = open_ledger(dir.path());
        for generation in 1..=3_u64 {
            ledger
                .append_raw(
                    &source("A", "a.jsonl"),
                    generation,
                    [(0_u64, b"a".as_slice())],
                )
                .unwrap();
            ledger.flush().unwrap();
        }
        drop(ledger);
        let index = dir.path().join(INDEX_FILE);
        let intact = std::fs::read(&index).unwrap();

        // No index at all: every segment is a candidate.
        std::fs::remove_file(&index).unwrap();
        assert_eq!(
            seqs(candidate_segments(dir.path(), "A", "a.jsonl", 2).unwrap()),
            [0, 1, 2]
        );

        // The last line cut mid-write: its segment is scanned, the others
        // still narrow.
        std::fs::write(&index, &intact[..intact.len() - 10]).unwrap();
        assert_eq!(
            seqs(candidate_segments(dir.path(), "A", "a.jsonl", 1).unwrap()),
            [0, 2]
        );

        // A segment that appears after the index was written is a candidate.
        std::fs::write(&index, &intact).unwrap();
        let mut ledger = open_ledger(dir.path());
        ledger
            .append_raw(&source("Z", "z.jsonl"), 1, [(0_u64, b"z".as_slice())])
            .unwrap();
        std::mem::forget(ledger); // no flush, no close, no index line: a crash
        assert_eq!(
            seqs(candidate_segments(dir.path(), "A", "a.jsonl", 1).unwrap()),
            [0, 3]
        );

        // The filter itself never consults the directory: a segment it has
        // not heard of — including one rotated in after it was read — is
        // admitted, and only a listed segment for another generation is not.
        let filter = segment_filter(dir.path(), "A", "a.jsonl", 1).unwrap();
        assert!(filter.admits(0));
        assert!(!filter.admits(1));
        assert!(filter.admits(3));
        assert!(
            filter.admits(99),
            "a segment the index never saw is a candidate"
        );
    }

    #[test]
    fn rebuild_reproduces_the_writer_index() {
        let dir = TempDir::new().unwrap();
        let mut ledger = open_ledger(dir.path());
        ledger
            .append_raw(&source("A", "a.jsonl"), 1, [(0_u64, b"a1".as_slice())])
            .unwrap();
        ledger
            .append_raw(&source("B", "b.jsonl"), 5, [(0_u64, b"b5".as_slice())])
            .unwrap();
        ledger.flush().unwrap();
        ledger
            .complete_generation(&source("B", "b.jsonl"), 6, 1, true)
            .unwrap();
        ledger.flush().unwrap();
        drop(ledger);
        let written = read_lines(dir.path()).unwrap();
        assert_eq!(written.len(), 2);

        std::fs::remove_file(dir.path().join(INDEX_FILE)).unwrap();
        assert_eq!(rebuild_segment_index(dir.path()).unwrap(), 2);
        assert_eq!(read_lines(dir.path()).unwrap(), written);
        assert!(std::fs::read_dir(dir.path()).unwrap().all(|entry| {
            let name = entry.unwrap().file_name();
            !name.to_string_lossy().contains(".tmp-")
        }));
    }
}
