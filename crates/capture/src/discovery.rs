//! Data-root walking and bounded skim reads. Capture knows nothing about
//! agents: it reports every regular file under a root; the pipeline filters
//! against adapter layouts. Symlinks are never followed.

use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::Path;
use std::time::UNIX_EPOCH;

use crate::{CaptureError, io_error};

/// One regular file under a data root. `relative_path` uses `/` separators to
/// match layout globs regardless of platform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannedFile {
    pub relative_path: String,
    pub size: u64,
    pub modified_ms: i64,
    /// The caller's verdict on this path from [`walk_classified`] (a layout
    /// walk records which stream claimed it); `None` from walks that only
    /// filter.
    pub class: Option<usize>,
}

#[derive(Debug, Default)]
pub struct WalkReport {
    pub files: Vec<ScannedFile>,
    /// The walk stopped at `max_files` — coverage is incomplete.
    pub truncated: bool,
}

/// Walk every regular file under `root`, sorted by relative path for
/// deterministic reports. A missing root is an empty report, not an error —
/// discovery probes runtimes that may not be installed.
pub fn walk(root: &Path, max_files: usize) -> Result<WalkReport, CaptureError> {
    walk_filtered(root, max_files, |_| true)
}

/// Walk regular files accepted by `include`. The cap counts accepted files,
/// not unrelated layout noise. This keeps a large cache/debug directory from
/// starving transcript discovery before the pipeline can filter it.
pub fn walk_filtered(
    root: &Path,
    max_files: usize,
    include: impl FnMut(&str) -> bool,
) -> Result<WalkReport, CaptureError> {
    walk_pruned(root, max_files, |_| true, include)
}

/// [`walk_filtered`] that never enters a directory `descend` rejects. The
/// caller answers from the directory's portable relative path (the root
/// itself is always entered) and must reject only subtrees where `include`
/// can accept nothing: then the accepted files, their order, and the cap —
/// which counts accepted files only — are exactly the unpruned walk's. A
/// rejected directory is never read, so it cannot fail the walk either.
pub fn walk_pruned(
    root: &Path,
    max_files: usize,
    descend: impl FnMut(&str) -> bool,
    mut include: impl FnMut(&str) -> bool,
) -> Result<WalkReport, CaptureError> {
    walk_with(root, max_files, descend, |path| {
        include(path).then_some(None)
    })
}

/// [`walk_pruned`] where the include answers with a class to keep on each
/// accepted file (`None` rejects), so later phases need not ask again.
pub fn walk_classified(
    root: &Path,
    max_files: usize,
    descend: impl FnMut(&str) -> bool,
    mut classify: impl FnMut(&str) -> Option<usize>,
) -> Result<WalkReport, CaptureError> {
    walk_with(root, max_files, descend, |path| classify(path).map(Some))
}

fn walk_with(
    root: &Path,
    max_files: usize,
    mut descend: impl FnMut(&str) -> bool,
    mut accept: impl FnMut(&str) -> Option<Option<usize>>,
) -> Result<WalkReport, CaptureError> {
    let mut report = WalkReport::default();
    if !root.is_dir() {
        return Ok(report);
    }
    let entries = walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| {
            if entry.depth() == 0 || !entry.file_type().is_dir() {
                return true;
            }
            // A directory without a portable path only holds files without
            // one, which the walk skips anyway.
            entry
                .path()
                .strip_prefix(root)
                .ok()
                .and_then(portable_relative)
                .is_some_and(|relative| descend(&relative))
        });
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            // A directory vanishing mid-walk is normal on live roots.
            Err(error) if error.io_error().is_some_and(is_not_found) => continue,
            Err(error) => {
                let path = error.path().unwrap_or(root).to_path_buf();
                return Err(io_error(
                    "walking",
                    &path,
                    error
                        .into_io_error()
                        .unwrap_or_else(|| std::io::Error::other("walk failed")),
                ));
            }
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let Ok(relative) = entry.path().strip_prefix(root) else {
            continue;
        };
        let Some(relative_path) = portable_relative(relative) else {
            continue;
        };
        let Some(class) = accept(&relative_path) else {
            continue;
        };
        if report.files.len() == max_files {
            report.truncated = true;
            break;
        }
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(error) if error.io_error().is_some_and(is_not_found) => continue,
            Err(error) => {
                return Err(io_error(
                    "reading metadata for",
                    entry.path(),
                    error
                        .into_io_error()
                        .unwrap_or_else(|| std::io::Error::other("metadata failed")),
                ));
            }
        };
        report.files.push(ScannedFile {
            relative_path,
            size: metadata.len(),
            modified_ms: modified_ms(&metadata),
            class,
        });
    }
    report
        .files
        .sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
    Ok(report)
}

/// A file's first record, and whether a delimiter ended it: one that runs
/// to the end of the file may still be being written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirstRecord {
    pub payload: Vec<u8>,
    pub terminated: bool,
}

/// Read only the first delimiter-terminated record, bounded by
/// `max_record_bytes`. A small or newline-free whole file — an empty one
/// included — is also a first record, reported unterminated; an oversized,
/// missing, or concurrently vanished file yields `None` and is reported as
/// unidentified by the caller.
pub fn first_record(
    path: &Path,
    delimiter: u8,
    max_record_bytes: usize,
    normalize_crlf: bool,
) -> Result<Option<FirstRecord>, CaptureError> {
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io_error("opening", path, error)),
    };
    let mut payload = Vec::with_capacity(max_record_bytes.min(64 * 1024));
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let remaining = max_record_bytes
            .saturating_add(1)
            .saturating_sub(payload.len());
        if remaining == 0 {
            return Ok(None);
        }
        let wanted = remaining.min(buffer.len());
        let count = file
            .read(&mut buffer[..wanted])
            .map_err(|error| io_error("reading the first record of", path, error))?;
        if count == 0 {
            break;
        }
        if let Some(at) = buffer[..count].iter().position(|byte| *byte == delimiter) {
            payload.extend_from_slice(&buffer[..at]);
            if normalize_crlf && payload.last() == Some(&b'\r') {
                payload.pop();
            }
            return Ok((payload.len() <= max_record_bytes).then_some(FirstRecord {
                payload,
                terminated: true,
            }));
        }
        payload.extend_from_slice(&buffer[..count]);
    }
    if normalize_crlf && payload.last() == Some(&b'\r') {
        payload.pop();
    }
    Ok((payload.len() <= max_record_bytes).then_some(FirstRecord {
        payload,
        terminated: false,
    }))
}

/// A bounded run of complete delimiter-framed records plus the actual bytes
/// pulled from the file. This is a metadata fallback, not a second tailer: it
/// starts at a caller-chosen record boundary and never reads past either cap.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct RecordScan {
    pub records: Vec<Vec<u8>>,
    pub bytes_read: u64,
}

#[allow(clippy::too_many_arguments)]
pub fn scan_records(
    path: &Path,
    start_offset: u64,
    byte_budget: usize,
    record_budget: usize,
    delimiter: u8,
    max_record_bytes: usize,
    normalize_crlf: bool,
) -> Result<Option<RecordScan>, CaptureError> {
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io_error("opening", path, error)),
    };
    if byte_budget == 0 || record_budget == 0 {
        return Ok(Some(RecordScan::default()));
    }
    file.seek(SeekFrom::Start(start_offset))
        .map_err(|error| io_error("seeking in", path, error))?;

    let limit = byte_budget as u64;
    let mut reader = BufReader::new(file.take(limit));
    let mut records = Vec::new();
    while records.len() < record_budget {
        let mut record = Vec::new();
        let count = reader
            .read_until(delimiter, &mut record)
            .map_err(|error| io_error("scanning records from", path, error))?;
        if count == 0 {
            break;
        }
        // A budget- or EOF-truncated record is not a record. The stream phase
        // will eventually parse it from byte zero; metadata never guesses.
        if record.last() != Some(&delimiter) {
            break;
        }
        record.pop();
        if normalize_crlf && record.last() == Some(&b'\r') {
            record.pop();
        }
        if record.len() <= max_record_bytes {
            records.push(record);
        }
    }
    let remaining = reader.into_inner().limit();
    Ok(Some(RecordScan {
        records,
        bytes_read: limit - remaining,
    }))
}

pub fn modified_ms(metadata: &std::fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |duration| duration.as_millis() as i64)
}

fn is_not_found(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::NotFound
}

fn portable_relative(path: &Path) -> Option<String> {
    let mut out = String::new();
    for component in path.components() {
        match component {
            std::path::Component::Normal(value) => {
                if !out.is_empty() {
                    out.push('/');
                }
                out.push_str(value.to_str()?);
            }
            std::path::Component::CurDir => {}
            _ => return None,
        }
    }
    (!out.is_empty()).then_some(out)
}

/// The two bounded skim windows of one file.
#[derive(Debug, Default)]
pub struct HeadTail {
    pub head: Vec<u8>,
    pub tail: Vec<u8>,
}

/// Bounded head + tail windows for the meta-phase skim. Never reads more than
/// `head_bytes + tail_bytes`; the windows never overlap. `None` = missing file.
pub fn head_tail(
    path: &Path,
    head_bytes: usize,
    tail_bytes: usize,
) -> Result<Option<HeadTail>, CaptureError> {
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io_error("opening", path, error)),
    };
    let len = file
        .metadata()
        .map_err(|error| io_error("reading metadata for", path, error))?
        .len();

    let head_len = (head_bytes as u64).min(len);
    let mut head = vec![0; head_len as usize];
    file.read_exact(&mut head)
        .map_err(|error| io_error("reading the head of", path, error))?;

    let tail_start = len.saturating_sub(tail_bytes as u64).max(head_len);
    let mut tail = vec![0; (len - tail_start) as usize];
    if !tail.is_empty() {
        file.seek(SeekFrom::Start(tail_start))
            .map_err(|error| io_error("seeking to the tail of", path, error))?;
        file.read_exact(&mut tail)
            .map_err(|error| io_error("reading the tail of", path, error))?;
    }
    Ok(Some(HeadTail { head, tail }))
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn walk_reports_relative_portable_paths_sorted() {
        let root = TempDir::new().unwrap();
        std::fs::create_dir_all(root.path().join("b/nested")).unwrap();
        std::fs::write(root.path().join("b/nested/z.jsonl"), b"z\n").unwrap();
        std::fs::write(root.path().join("a.jsonl"), b"a\n").unwrap();

        let report = walk(root.path(), 100).unwrap();
        assert!(!report.truncated);
        let paths: Vec<&str> = report
            .files
            .iter()
            .map(|file| file.relative_path.as_str())
            .collect();
        assert_eq!(paths, ["a.jsonl", "b/nested/z.jsonl"]);
        assert_eq!(report.files[0].size, 2);
    }

    #[test]
    fn walk_of_a_missing_root_is_empty_not_an_error() {
        let root = TempDir::new().unwrap();
        let report = walk(&root.path().join("not-installed"), 10).unwrap();
        assert!(report.files.is_empty() && !report.truncated);
    }

    #[test]
    fn walk_stops_at_the_file_cap_and_says_so() {
        let root = TempDir::new().unwrap();
        for index in 0..5 {
            std::fs::write(root.path().join(format!("f{index}.jsonl")), b"x").unwrap();
        }
        let report = walk(root.path(), 3).unwrap();
        assert_eq!(report.files.len(), 3);
        assert!(report.truncated);
    }

    #[test]
    fn pruned_walk_never_enters_a_rejected_directory() {
        let root = TempDir::new().unwrap();
        for dir in ["keep/nested", "skip/nested", "skipped"] {
            std::fs::create_dir_all(root.path().join(dir)).unwrap();
        }
        for file in [
            "top.jsonl",
            "keep/a.jsonl",
            "keep/nested/b.jsonl",
            "skip/c.jsonl",
            "skip/nested/d.jsonl",
            "skipped/e.jsonl",
        ] {
            std::fs::write(root.path().join(file), b"x\n").unwrap();
        }
        let mut asked = Vec::new();
        let report = walk_pruned(
            root.path(),
            100,
            |dir| {
                asked.push(dir.to_owned());
                !dir.starts_with("skip")
            },
            |_| true,
        )
        .unwrap();
        let paths: Vec<&str> = report
            .files
            .iter()
            .map(|file| file.relative_path.as_str())
            .collect();
        assert_eq!(paths, ["keep/a.jsonl", "keep/nested/b.jsonl", "top.jsonl"]);
        asked.sort();
        assert_eq!(asked, ["keep", "keep/nested", "skip", "skipped"]);
    }

    #[test]
    fn classified_walk_keeps_each_files_class_through_the_sort() {
        let root = TempDir::new().unwrap();
        std::fs::create_dir_all(root.path().join("b")).unwrap();
        for file in ["c.jsonl", "b/a.json", "a.txt"] {
            std::fs::write(root.path().join(file), b"x\n").unwrap();
        }
        let report = walk_classified(
            root.path(),
            100,
            |_| true,
            |path| match path.rsplit_once('.').map(|(_, extension)| extension) {
                Some("jsonl") => Some(7),
                Some("json") => Some(3),
                _ => None,
            },
        )
        .unwrap();
        let classes: Vec<(&str, Option<usize>)> = report
            .files
            .iter()
            .map(|file| (file.relative_path.as_str(), file.class))
            .collect();
        assert_eq!(classes, [("b/a.json", Some(3)), ("c.jsonl", Some(7))]);
        assert!(
            walk(root.path(), 100)
                .unwrap()
                .files
                .iter()
                .all(|file| file.class.is_none())
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_rejected_unreadable_directory_cannot_fail_the_walk() {
        use std::os::unix::fs::PermissionsExt;

        let root = TempDir::new().unwrap();
        let locked = root.path().join("locked");
        std::fs::create_dir_all(locked.join("inner")).unwrap();
        std::fs::write(root.path().join("a.jsonl"), b"x\n").unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        // A superuser reads mode-000 directories; the contrast needs a user.
        let enforced = std::fs::read_dir(&locked).is_err();
        let pruned = walk_pruned(root.path(), 100, |dir| dir != "locked", |_| true);
        let unpruned = walk(root.path(), 100);
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(pruned.unwrap().files.len(), 1);
        if enforced {
            assert!(unpruned.is_err());
        }
    }

    #[test]
    fn filtered_walk_caps_matches_not_unrelated_files() {
        let root = TempDir::new().unwrap();
        for index in 0..5 {
            std::fs::write(root.path().join(format!("noise-{index}.txt")), b"x").unwrap();
        }
        std::fs::write(root.path().join("session.jsonl"), b"x\n").unwrap();
        let report = walk_filtered(root.path(), 1, |path| path.ends_with(".jsonl")).unwrap();
        assert_eq!(report.files.len(), 1);
        assert_eq!(report.files[0].relative_path, "session.jsonl");
        assert!(!report.truncated);
    }

    #[test]
    fn first_record_reads_past_the_skim_window_but_stays_bounded() {
        let root = TempDir::new().unwrap();
        let path = root.path().join("rollout.jsonl");
        let long = format!("{{\"padding\":\"{}\"}}\nsecond\n", "x".repeat(80_000));
        std::fs::write(&path, &long).unwrap();
        let record = first_record(&path, b'\n', 100_000, true).unwrap().unwrap();
        assert_eq!(record.payload.len(), 80_014);
        assert!(record.terminated);
        assert!(first_record(&path, b'\n', 1_000, true).unwrap().is_none());
    }

    #[test]
    fn a_first_record_that_runs_to_the_end_of_the_file_is_unterminated() {
        let root = TempDir::new().unwrap();
        let path = root.path().join("rollout.jsonl");
        std::fs::write(&path, b"{\"type\":\"session_meta\",\"pay").unwrap();
        let record = first_record(&path, b'\n', 1_000, true).unwrap().unwrap();
        assert_eq!(record.payload, b"{\"type\":\"session_meta\",\"pay");
        assert!(!record.terminated);
        std::fs::write(&path, b"").unwrap();
        let empty = first_record(&path, b'\n', 1_000, true).unwrap().unwrap();
        assert!(empty.payload.is_empty() && !empty.terminated);
    }

    #[cfg(unix)]
    #[test]
    fn walk_does_not_follow_symlinks() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("root");
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.jsonl"), b"secret\n").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("link-dir")).unwrap();
        std::os::unix::fs::symlink(outside.join("secret.jsonl"), root.join("link.jsonl")).unwrap();

        let report = walk(&root, 100).unwrap();
        assert!(report.files.is_empty());
    }

    #[test]
    fn head_and_tail_windows_never_overlap() {
        let root = TempDir::new().unwrap();
        let path = root.path().join("s.jsonl");
        std::fs::write(&path, b"0123456789").unwrap();

        // Windows would overlap on a small file: tail yields only the bytes
        // past the head.
        let windows = head_tail(&path, 6, 6).unwrap().unwrap();
        assert_eq!(windows.head, b"012345");
        assert_eq!(windows.tail, b"6789");

        // File smaller than the head window: whole file, empty tail.
        let windows = head_tail(&path, 64, 8).unwrap().unwrap();
        assert_eq!(windows.head, b"0123456789");
        assert!(windows.tail.is_empty());

        // Distinct windows on a large-enough file.
        let windows = head_tail(&path, 3, 3).unwrap().unwrap();
        assert_eq!(windows.head, b"012");
        assert_eq!(windows.tail, b"789");

        assert!(
            head_tail(&root.path().join("missing"), 4, 4)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn record_scan_is_complete_and_strictly_bounded() {
        let root = TempDir::new().unwrap();
        let path = root.path().join("s.jsonl");
        std::fs::write(
            &path,
            b"partial-record\nalpha\r\nbeta\ngamma-without-newline",
        )
        .unwrap();

        // The caller rewinds from a clipped head to the start of its crossing
        // record. The unterminated EOF fragment is deliberately excluded.
        let scan = scan_records(&path, 0, 64, 10, b'\n', 64, true)
            .unwrap()
            .unwrap();
        assert_eq!(
            scan.records,
            [
                b"partial-record".to_vec(),
                b"alpha".to_vec(),
                b"beta".to_vec()
            ]
        );
        assert!(scan.bytes_read <= 64);

        let capped = scan_records(&path, 15, 5, 10, b'\n', 64, true)
            .unwrap()
            .unwrap();
        assert!(capped.bytes_read <= 5);
        assert!(
            capped.records.is_empty(),
            "the clipped record is not parsed"
        );
    }
}
