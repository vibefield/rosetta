//! Bounded recovery, driven by recorded identity and explicit search roots.
//! An equal basename is never identity. Indexes are RAM-only and shared by
//! capture workers; selected bytes and their origin go into the raw ledger.

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use rosetta_adapter_support::{AgentAdapter, html_artifacts::CaptureHints};
use rosetta_capture::{CapturedDocument, DocumentCaptureStatus as Status};
use rosetta_core::html_artifacts::{HTML_MAX_BYTES, HtmlArtifactPublication, is_html_path};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use super::{observe, utf8};

const MAX_ROOTS: usize = 16;
const MAX_ENTRIES: usize = 20_000;
const MAX_DOCS: usize = 128;
const MAX_MARKDOWN: usize = 512 * 1024;
const MAX_CANDIDATES: usize = 32;

type ArchiveCache = BTreeMap<PathBuf, (Instant, Arc<ArchiveIndex>)>;

#[derive(Debug, Clone, Default)]
pub struct ArtifactRecovery {
    /// Extra archive directories, including directories on external drives.
    /// Default recovery also uses the recorded cwd and its linked worktrees.
    pub search_roots: Vec<PathBuf>,
    cache: Arc<Mutex<ArchiveCache>>,
}

#[derive(Debug, Default)]
struct ArchiveIndex {
    links: BTreeMap<String, Vec<CopySource>>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum CopySource {
    File(PathBuf),
    Git {
        repo: PathBuf,
        revision: String,
        path: PathBuf,
    },
}

impl ArtifactRecovery {
    pub(super) fn recover(
        &self,
        adapter: &dyn AgentAdapter,
        transcript: Option<&Path>,
        publication: &HtmlArtifactPublication,
        hints: CaptureHints,
    ) -> Option<CapturedDocument> {
        let original = publication.path.as_ref()?;
        // A successful Write is re-read by coordinate, checksum, and the one
        // native Rust parser. Never persist its body in a checkpoint.
        if let Some(record) = hints
            .record
            .as_ref()
            .filter(|r| r.offset < publication.source_offset)
            && let Some(transcript) = transcript
            && let Some(raw) = recorded_line(transcript, record.offset, &record.digest)
            && let Some(bytes) = adapter.html_artifact_record(&raw, original)
            && utf8(&bytes).is_some()
        {
            return Some(recovered(
                original,
                bytes,
                format!("transcript:{}#{}", transcript.display(), record.offset),
            ));
        }
        for candidate in hints.files.iter().take(MAX_CANDIDATES) {
            if let Some(document) = CopySource::File(PathBuf::from(candidate)).read(original) {
                return Some(document);
            }
        }

        let mut roots: Vec<PathBuf> = hints.roots.iter().map(PathBuf::from).collect();
        if let Some(repo) = repository(Path::new(original)) {
            roots.push(repo);
        }
        let repositories: BTreeSet<_> = roots.iter().filter_map(|p| repository(p)).collect();
        for repo in repositories.iter().take(4) {
            // A matching repository-relative path in a linked checkout is
            // stronger evidence than a filename search in an unrelated tree.
            let worktrees = worktrees(repo);
            if let Ok(relative) = Path::new(original).strip_prefix(repo) {
                let copies: Vec<_> = worktrees
                    .iter()
                    .map(|p| CopySource::File(p.join(relative)))
                    .collect();
                if let Some(document) = unique_copy(original, &copies).ok()? {
                    return Some(document);
                }
                if let Some(revision) = history(repo, relative, 1).first()
                    && let Some(document) = (CopySource::Git {
                        repo: repo.clone(),
                        revision: revision.clone(),
                        path: relative.to_path_buf(),
                    })
                    .read(original)
                {
                    return Some(document);
                }
            }
            roots.push(repo.clone());
            roots.extend(worktrees.into_iter().take(4));
        }
        // Explicit roots get priority over duplicate linked checkouts.
        let mut search = self.search_roots.clone();
        search.extend(roots);
        let mut visited = BTreeSet::new();
        let mut candidates = BTreeSet::new();
        let urls: BTreeSet<_> = hints.urls.iter().map(|u| url_key(u)).collect();
        if urls.is_empty() {
            return None;
        }
        for root in search
            .into_iter()
            .filter(|p| p.is_absolute())
            .filter(|p| visited.insert(p.clone()))
            .take(MAX_ROOTS)
        {
            let index = self.index(&root);
            for url in &urls {
                if let Some(found) = index.links.get(url) {
                    candidates.extend(found.iter().cloned());
                }
            }
            if candidates.len() > MAX_CANDIDATES {
                return None;
            }
        }
        unique_copy(original, &candidates.into_iter().collect::<Vec<_>>())
            .ok()
            .flatten()
    }

    fn index(&self, root: &Path) -> Arc<ArchiveIndex> {
        let Ok(mut cache) = self.cache.lock() else {
            return Arc::default();
        };
        if let Some((at, index)) = cache.get(root)
            && at.elapsed() < Duration::from_secs(300)
        {
            return index.clone();
        }
        // One bounded index build per root across pooled capture workers.
        let index = Arc::new(ArchiveIndex::scan(root));
        if cache.len() >= 32 {
            cache.pop_first();
        }
        cache.insert(root.to_path_buf(), (Instant::now(), index.clone()));
        index
    }
}

fn unique_copy(original: &str, candidates: &[CopySource]) -> Result<Option<CapturedDocument>, ()> {
    let mut selected: Option<CapturedDocument> = None;
    for source in candidates.iter().take(MAX_CANDIDATES) {
        if let Some(document) = source.read(original) {
            if let Some(previous) = &selected {
                if previous.bytes != document.bytes {
                    return Err(());
                }
            } else {
                selected = Some(document);
            }
        }
    }
    Ok(selected)
}

impl CopySource {
    fn read(&self, original: &str) -> Option<CapturedDocument> {
        match self {
            Self::File(path) => {
                let mut document = observe(path.to_str()?.to_owned());
                if document.status != Status::Captured {
                    return None;
                }
                document.recovered_from = Some(document.path.clone());
                document.path = original.to_owned();
                Some(document)
            }
            Self::Git {
                repo,
                revision,
                path,
            } => {
                let mode = git(
                    repo,
                    &["ls-tree", "-z", revision, "--", path.to_str()?],
                    32768,
                )?;
                if !mode.starts_with(b"100644 ") && !mode.starts_with(b"100755 ") {
                    return None;
                }
                let object = format!("{revision}:{}", path.to_str()?);
                let bytes = git(repo, &["cat-file", "blob", &object], HTML_MAX_BYTES)?;
                utf8(&bytes)?;
                Some(recovered(
                    original,
                    bytes,
                    format!("git:{}@{revision}:{}", repo.display(), path.display()),
                ))
            }
        }
    }
}

fn recovered(original: &str, bytes: Vec<u8>, from: String) -> CapturedDocument {
    CapturedDocument {
        path: original.to_owned(),
        captured_at: super::now(),
        status: Status::Captured,
        recovered_from: Some(from),
        media: None,
        bytes,
    }
}

fn recorded_line(path: &Path, offset: u64, digest: &str) -> Option<Vec<u8>> {
    let mut file = File::open(path).ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    file.seek(SeekFrom::Start(offset)).ok()?;
    let limit = 64 * 1024 * 1024;
    let mut reader = BufReader::new(file.take(limit + 1));
    let mut bytes = Vec::new();
    reader.read_until(b'\n', &mut bytes).ok()?;
    if bytes.len() > limit as usize {
        return None;
    }
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
    }
    if blake3::hash(&bytes).to_hex().as_str() != digest {
        if bytes.last() != Some(&b'\r') {
            return None;
        }
        bytes.pop();
        if blake3::hash(&bytes).to_hex().as_str() != digest {
            return None;
        }
    }
    Some(bytes)
}

impl ArchiveIndex {
    fn scan(root: &Path) -> Self {
        let mut index = Self::default();
        let mut historic = 0;
        for entry in walkdir::WalkDir::new(root)
            .follow_links(false)
            .max_depth(12)
            .sort_by_file_name()
            .into_iter()
            .filter_entry(|e| {
                !matches!(
                    e.file_name().to_str(),
                    Some(".git" | "node_modules" | "target" | ".venv" | "vendor" | ".cache")
                )
            })
            .take(MAX_ENTRIES)
            .filter_map(Result::ok)
            .filter(|e| {
                e.file_type().is_file()
                    && e.path()
                        .extension()
                        .is_some_and(|x| x.eq_ignore_ascii_case("md"))
            })
            .take(MAX_DOCS)
        {
            let path = entry.path();
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.len() > MAX_MARKDOWN as u64 {
                continue;
            }
            let Ok(file) = File::open(path) else {
                continue;
            };
            let mut text = String::new();
            if file
                .take(MAX_MARKDOWN as u64 + 1)
                .read_to_string(&mut text)
                .is_err()
                || text.len() > MAX_MARKDOWN
            {
                continue;
            }
            let linked = index.markdown(&text, path, root, None);
            // Archives sometimes replace an old native URL when republishing.
            // Recover those associations from this same manifest's Git history.
            if linked
                && historic < 4
                && let Some(repo) = repository(path)
                && let Ok(relative) = path.strip_prefix(&repo)
            {
                historic += 1;
                for revision in history(&repo, relative, 4) {
                    let object = format!("{revision}:{}", relative.display());
                    if let Some(bytes) = git(&repo, &["cat-file", "blob", &object], MAX_MARKDOWN)
                        && let Ok(text) = std::str::from_utf8(&bytes)
                    {
                        index.markdown(text, path, root, Some((&repo, &revision)));
                    }
                }
            }
        }
        index
    }

    fn markdown(
        &mut self,
        text: &str,
        document: &Path,
        boundary: &Path,
        revision: Option<(&Path, &str)>,
    ) -> bool {
        let mut urls = BTreeSet::new();
        let mut files = BTreeSet::new();
        let mut linked = false;
        let mut in_table = false;
        let mut excluded = 0;
        for event in Parser::new_ext(text, Options::ENABLE_TABLES) {
            match event {
                Event::Start(Tag::CodeBlock(_) | Tag::BlockQuote(_)) => excluded += 1,
                Event::End(TagEnd::CodeBlock | TagEnd::BlockQuote(_)) => excluded -= 1,
                Event::Start(Tag::Table(_)) => in_table = true,
                Event::End(TagEnd::Table) => in_table = false,
                Event::Start(Tag::Link { dest_url, .. }) if excluded == 0 => {
                    let link = dest_url.as_ref();
                    if link.starts_with("https://") {
                        urls.insert(url_key(link));
                    } else if let Some(path) =
                        local_link(document, link).filter(|p| p.starts_with(boundary))
                        && (revision.is_some()
                            || path
                                .ancestors()
                                .find(|p| p.exists())
                                .and_then(|p| p.canonicalize().ok())
                                .zip(boundary.canonicalize().ok())
                                .is_some_and(|(p, root)| p.starts_with(root)))
                    {
                        files.insert(path);
                    }
                }
                Event::End(TagEnd::TableRow | TagEnd::Item) if excluded == 0 => {
                    linked |= self.associate(&mut urls, &mut files, revision);
                }
                Event::End(TagEnd::Paragraph) if excluded == 0 && !in_table => {
                    linked |= self.associate(&mut urls, &mut files, revision);
                }
                _ => {}
            }
        }
        linked
    }

    fn associate(
        &mut self,
        urls: &mut BTreeSet<String>,
        files: &mut BTreeSet<PathBuf>,
        revision: Option<(&Path, &str)>,
    ) -> bool {
        let linked = !urls.is_empty() && files.len() == 1;
        if linked {
            let path = files.first().unwrap();
            let source = if let Some((repo, revision)) = revision {
                path.strip_prefix(repo).ok().map(|path| CopySource::Git {
                    repo: repo.to_path_buf(),
                    revision: revision.to_owned(),
                    path: path.to_path_buf(),
                })
            } else {
                Some(CopySource::File(path.clone()))
            };
            if let Some(source) = source {
                for url in urls.iter() {
                    let candidates = self.links.entry(url.clone()).or_default();
                    if candidates.len() < MAX_CANDIDATES && !candidates.contains(&source) {
                        candidates.push(source.clone());
                    }
                }
            }
        }
        urls.clear();
        files.clear();
        linked
    }
}

fn local_link(document: &Path, link: &str) -> Option<PathBuf> {
    let decoded = percent_encoding::percent_decode_str(link)
        .decode_utf8()
        .ok()?;
    if !is_html_path(&decoded)
        || decoded.contains([':', '\\', '\0', '?', '#'])
        || decoded.starts_with('/')
    {
        return None;
    }
    let mut path = document.parent()?.to_path_buf();
    for component in Path::new(decoded.as_ref()).components() {
        match component {
            Component::Normal(p) => path.push(p),
            Component::CurDir => {}
            Component::ParentDir => {
                if !path.pop() {
                    return None;
                }
            }
            _ => return None,
        }
    }
    Some(path)
}

fn url_key(url: &str) -> String {
    url.trim_end_matches('/').to_owned()
}

fn repository(path: &Path) -> Option<PathBuf> {
    path.ancestors()
        .take(24)
        .find(|p| p.join(".git").exists())
        .map(Path::to_path_buf)
}

fn worktrees(repo: &Path) -> Vec<PathBuf> {
    git(repo, &["worktree", "list", "--porcelain"], 64 * 1024)
        .and_then(|b| String::from_utf8(b).ok())
        .map(|s| {
            s.lines()
                .filter_map(|l| l.strip_prefix("worktree "))
                .take(16)
                .map(PathBuf::from)
                .collect()
        })
        .unwrap_or_default()
}

fn history(repo: &Path, path: &Path, count: usize) -> Vec<String> {
    let Some(path) = path.to_str() else {
        return Vec::new();
    };
    git(
        repo,
        &[
            "log",
            "--all",
            "--format=%H",
            &format!("--max-count={count}"),
            "--diff-filter=AM",
            "--",
            path,
        ],
        4096,
    )
    .and_then(|b| String::from_utf8(b).ok())
    .map(|s| {
        s.lines()
            .filter(|l| l.len() == 40 && l.bytes().all(|b| b.is_ascii_hexdigit()))
            .map(str::to_owned)
            .collect()
    })
    .unwrap_or_default()
}

fn git(repo: &Path, args: &[&str], limit: usize) -> Option<Vec<u8>> {
    let mut child = Command::new("git")
        .args([
            "--no-pager",
            "--literal-pathspecs",
            "-c",
            "core.fsmonitor=false",
            "-C",
        ])
        .arg(repo)
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;
    let read = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.take(limit as u64 + 1).read_to_end(&mut bytes).ok()?;
        (bytes.len() <= limit).then_some(bytes)
    });
    let deadline = Instant::now() + Duration::from_millis(750);
    let success = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.success(),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break false;
            }
        }
    };
    let bytes = read.join().ok().flatten();
    if success { bytes } else { None }
}
