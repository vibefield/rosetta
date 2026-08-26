//! Data-root walking and bounded skim reads. Capture knows nothing about
//! agents: it reports every regular file under a root; the pipeline filters
//! against adapter layouts. Symlinks are never followed.

use std::io::{Read, Seek, SeekFrom};
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
    mut include: impl FnMut(&str) -> bool,
) -> Result<WalkReport, CaptureError> {
    let mut report = WalkReport::default();
    if !root.is_dir() {
        return Ok(report);
    }
    for entry in walkdir::WalkDir::new(root).follow_links(false) {
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
        if !include(&relative_path) {
            continue;
        }
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
        });
    }
    report
        .files
        .sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
    Ok(report)
}

/// Read only the first delimiter-terminated record, bounded by
/// `max_record_bytes`. A small or newline-free whole file is also a complete
/// first record; an oversized, missing, or concurrently vanished file yields
/// `None` and is reported as unidentified by the caller.
pub fn first_record(
    path: &Path,
    delimiter: u8,
    max_record_bytes: usize,
    normalize_crlf: bool,
) -> Result<Option<Vec<u8>>, CaptureError> {
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
            return (payload.len() <= max_record_bytes)
                .then_some(payload)
                .map_or(Ok(None), |payload| Ok(Some(payload)));
        }
        payload.extend_from_slice(&buffer[..count]);
    }
    if normalize_crlf && payload.last() == Some(&b'\r') {
        payload.pop();
    }
    Ok((!payload.is_empty() && payload.len() <= max_record_bytes).then_some(payload))
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
        assert_eq!(record.len(), 80_014);
        assert!(first_record(&path, b'\n', 1_000, true).unwrap().is_none());
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
}
