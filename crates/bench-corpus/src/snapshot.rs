//! Materialize a [`ShapePlan`] as a frozen snapshot in the format the R1 kit
//! verifies (`tools/r1-bench/verify_r1_snapshot.js`): `OUT/home/<data roots>`,
//! every file 0444 and every directory 0555 (`OUT` included), `SHA256SUMS`
//! (`<sha256>  home/...` per file, in UTF-8 byte order of path) and
//! `snapshot.json` with the walk-inputs digest `r1_snapshot_lib.js` computes,
//! the manifest's sha256, per-agent totals and the shape's own summary.
//!
//! Deterministic: same plan, same bytes and mtimes, same `snapshot.json` (it
//! records no creation time and no path).

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::claude_code::{ShapedTranscript, write_shaped};
use crate::shape::{ShapePlan, ShapeSpec};
use crate::{Agent, CorpusSpec};

const CLAUDE_ROOT: &str = ".claude";
const CODEX_ROOT: &str = ".codex";
const HOUR_MS: i64 = 3_600_000;

/// `r1_snapshot_lib.js`'s `WALK_INPUTS_METHOD`, verbatim.
pub const WALK_INPUTS_METHOD: &str = "sha256 over lines \"agent\\trank\\tsize\\tmtime_ms\\n\", agents in byte order, sources in UTF-8 byte order of their data-root-relative path, rank 1-based per agent, mtime_ms = floor(mtime_ns / 1e6); every regular file under each agent data root of the frozen home";

enum Job {
    Transcript(PathBuf, ShapedTranscript),
    Document(PathBuf, String, i64),
}

impl Job {
    fn bytes(&self) -> u64 {
        match self {
            Job::Transcript(_, t) => t.bytes,
            Job::Document(_, d, _) => d.len() as u64,
        }
    }
}

/// Write `plan` into `out` (which must not exist) with `threads` writers and
/// return `snapshot.json`'s value.
pub fn materialize(plan: &ShapePlan, out: &Path, threads: usize) -> io::Result<Value> {
    if out.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "output directory exists; refusing to overwrite",
        ));
    }
    let home = out.join("home");
    let claude = home.join(CLAUDE_ROOT);
    let codex = home.join(CODEX_ROOT);
    fs::create_dir_all(&claude)?;
    fs::create_dir_all(&codex)?;

    let mut jobs = Vec::new();
    for tree in &plan.trees {
        let dir = claude.join(tree.project_dir());
        let subagents = dir.join(&tree.root_id).join("subagents");
        fs::create_dir_all(
            if tree.children.is_empty() && tree.sidecar_only.is_empty() {
                &dir
            } else {
                &subagents
            },
        )?;
        let cwd = tree.cwd();
        jobs.push(Job::Transcript(
            dir.join(format!("{}.jsonl", tree.root_id)),
            ShapedTranscript {
                session_id: tree.root_id.clone(),
                agent_id: None,
                cwd: cwd.clone(),
                seed: tree.root_seed,
                bytes: tree.root_bytes,
                mtime_ms: tree.root_mtime_ms,
            },
        ));
        for child in &tree.children {
            jobs.push(Job::Transcript(
                subagents.join(format!("agent-{}.jsonl", child.agent_id)),
                ShapedTranscript {
                    session_id: tree.root_id.clone(),
                    agent_id: Some(child.agent_id.clone()),
                    cwd: cwd.clone(),
                    seed: child.seed,
                    bytes: child.bytes,
                    mtime_ms: child.mtime_ms,
                },
            ));
            if let Some(doc) = &child.sidecar {
                jobs.push(Job::Document(
                    subagents.join(format!("agent-{}.meta.json", child.agent_id)),
                    doc.clone(),
                    child.mtime_ms,
                ));
            }
        }
        for only in &tree.sidecar_only {
            jobs.push(Job::Document(
                subagents.join(format!("agent-{}.meta.json", only.agent_id)),
                only.document.clone(),
                only.mtime_ms,
            ));
        }
    }
    // Largest first, so the giants do not end the run on one thread.
    jobs.sort_by_key(|job| std::cmp::Reverse(job.bytes()));
    run_parallel(&jobs, threads, |job| match job {
        Job::Transcript(path, t) => {
            write_shaped(t, path)?;
            set_mtime(path, t.mtime_ms)
        }
        Job::Document(path, doc, mtime) => {
            fs::write(path, doc)?;
            set_mtime(path, *mtime)
        }
    })?;

    write_codex(&plan.spec, &codex)?;

    // Hash, list, freeze.
    let mut files = Vec::new();
    for (agent, root) in [("claude-code", CLAUDE_ROOT), ("codex", CODEX_ROOT)] {
        let base = home.join(root);
        let mut rels = files_below(&base)?;
        rels.sort_by(|l, r| l.as_bytes().cmp(r.as_bytes()));
        for rel in rels {
            let abs = base.join(&rel);
            let meta = fs::metadata(&abs)?;
            files.push(Entry {
                agent,
                manifest_path: format!("home/{root}/{rel}"),
                rel,
                abs,
                size: meta.len(),
                mtime_ms: mtime_ms(&meta)?,
                sha256: String::new(),
            });
        }
    }
    let hashes = Mutex::new(vec![String::new(); files.len()]);
    let indexed: Vec<usize> = (0..files.len()).collect();
    run_parallel(&indexed, threads, |&i| {
        let sha = sha256_file(&files[i].abs)?;
        hashes.lock().expect("hash slot")[i] = sha;
        Ok(())
    })?;
    for (entry, sha) in files.iter_mut().zip(hashes.into_inner().expect("hashes")) {
        entry.sha256 = sha;
    }
    let mut manifest_order: Vec<&Entry> = files.iter().collect();
    manifest_order.sort_by(|l, r| l.manifest_path.as_bytes().cmp(r.manifest_path.as_bytes()));
    let sums: String = manifest_order
        .iter()
        .map(|e| format!("{}  {}\n", e.sha256, e.manifest_path))
        .collect();
    let summary = summary(plan, &files, &sums);
    for entry in &files {
        fs::set_permissions(&entry.abs, fs::Permissions::from_mode(0o444))?;
    }
    write_frozen(&out.join("SHA256SUMS"), sums.as_bytes())?;
    let text = serde_json::to_string_pretty(&summary).map_err(io::Error::other)?;
    write_frozen(&out.join("snapshot.json"), format!("{text}\n").as_bytes())?;
    let mut dirs = directories_below(out)?;
    dirs.sort_by_key(|d| std::cmp::Reverse(d.components().count()));
    for dir in dirs {
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o555))?;
    }
    Ok(summary)
}

/// The codex corpus (line mode), each rollout's mtime from the spec's ages
/// in path order.
fn write_codex(spec: &ShapeSpec, codex: &Path) -> io::Result<()> {
    let c = &spec.codex;
    let corpus = CorpusSpec::new(Agent::Codex, c.sessions, c.lines, c.seed);
    crate::generate(&corpus, codex)?;
    let mut rels = files_below(codex)?;
    rels.sort_by(|l, r| l.as_bytes().cmp(r.as_bytes()));
    if rels.len() != c.ages_h.len() {
        return Err(io::Error::other(format!(
            "codex: {} rollouts but {} ages",
            rels.len(),
            c.ages_h.len()
        )));
    }
    for (rel, age) in rels.iter().zip(&c.ages_h) {
        set_mtime(
            &codex.join(rel),
            spec.newest_ms - (age * HOUR_MS as f64) as i64,
        )?;
    }
    Ok(())
}

struct Entry {
    agent: &'static str,
    rel: String,
    manifest_path: String,
    abs: PathBuf,
    size: u64,
    mtime_ms: i64,
    sha256: String,
}

/// `walkInputs` of `r1_snapshot_lib.js`: entries are already per agent in
/// byte order of agent ("claude-code" < "codex") and of relative path.
fn walk_inputs_digest(files: &[Entry]) -> String {
    let mut hash = Sha256::new();
    let mut rank: BTreeMap<&str, u64> = BTreeMap::new();
    for e in files {
        let r = rank.entry(e.agent).or_insert(0);
        *r += 1;
        hash.update(format!("{}\t{}\t{}\t{}\n", e.agent, r, e.size, e.mtime_ms));
    }
    hex(&hash.finalize())
}

fn summary(plan: &ShapePlan, files: &[Entry], sums: &str) -> Value {
    let spec = &plan.spec;
    let agent = |name: &'static str| files.iter().filter(move |e| e.agent == name);
    let total = |it: &mut dyn Iterator<Item = &Entry>| {
        it.fold((0u64, 0u64), |(n, b), e| (n + 1, b + e.size))
    };
    let (claude_files, claude_bytes) = total(&mut agent("claude-code"));
    let (codex_files, codex_bytes) = total(&mut agent("codex"));
    let stream = |e: &&Entry| {
        if e.rel.ends_with(".meta.json") {
            "subagent-metadata:document-snapshot"
        } else if e.rel.contains("/subagents/") {
            "subagent-transcript:json-lines"
        } else {
            "transcript:json-lines"
        }
    };
    let mut by_stream: BTreeMap<&str, (u64, u64)> = BTreeMap::new();
    for e in agent("claude-code") {
        let s = by_stream.entry(stream(&e)).or_default();
        s.0 += 1;
        s.1 += e.size;
    }
    let by_stream: BTreeMap<&str, Value> = by_stream
        .into_iter()
        .map(|(k, (n, b))| (k, json!({"files": n, "bytes": b})))
        .collect();
    let ages = |name: &'static str| {
        let newest = agent(name).map(|e| e.mtime_ms).max().unwrap_or(0);
        let mut out = BTreeMap::new();
        for (key, hours) in [("le1h", 1), ("le24h", 24), ("le7d", 168), ("le30d", 720)] {
            let (n, b) = total(&mut agent(name).filter(|e| newest - e.mtime_ms <= hours * HOUR_MS));
            out.insert(key, json!({"files": n, "bytes": b}));
        }
        let (n, b) = total(&mut agent(name).filter(|e| newest - e.mtime_ms > 720 * HOUR_MS));
        out.insert("older", json!({"files": n, "bytes": b}));
        out
    };
    let mut ring_names = vec!["newest-day"];
    ring_names.extend(spec.rings.iter().map(|r| r.name));
    let rings: Vec<Value> = ring_names
        .iter()
        .enumerate()
        .map(|(k, name)| {
            let trees: Vec<_> = plan.trees.iter().filter(|t| t.ring == k).collect();
            json!({
                "ring": name,
                "trees": trees.len(),
                "big": trees.iter().filter(|t| t.big).count(),
                "files": trees.iter().map(|t| t.files()).sum::<u64>(),
                "bytes": trees.iter().map(|t| t.bytes()).sum::<u64>(),
            })
        })
        .collect();
    let giant = &plan.trees[0];
    let spec_text = serde_json::to_string(spec).expect("spec serializes");
    let subagents: u64 = plan.trees.iter().map(|t| t.children.len() as u64).sum();
    let sidecar_only: u64 = plan.trees.iter().map(|t| t.sidecar_only.len() as u64).sum();
    json!({
        "format": "bench-corpus-snapshot/1",
        "shape": spec.name,
        "specSha256": hex(&Sha256::digest(spec_text.as_bytes())),
        "seed": plan.seed,
        "generator": "crates/bench-corpus `bench-corpus snapshot` (shape.rs plan, snapshot.rs writer); deterministic from shape + seed",
        "privatePathsRecorded": false,
        "agents": {
            "claude-code": {"dataRoot": "~/.claude", "files": claude_files, "bytes": claude_bytes, "byStream": by_stream,
                "ageBucketsFromAgentNewest": ages("claude-code")},
            "codex": {"dataRoot": "~/.codex", "files": codex_files, "bytes": codex_bytes,
                "ageBucketsFromAgentNewest": ages("codex")},
        },
        "totals": {"files": claude_files + codex_files, "bytes": claude_bytes + codex_bytes},
        "shapeSummary": {
            "roots": plan.trees.len(),
            "bigTrees": plan.trees.iter().filter(|t| t.max_file() > spec.big_line_bytes).count(),
            "bigLineBytes": spec.big_line_bytes,
            "rosterSessions": plan.trees.len() as u64 + subagents,
            "subagentsWithTranscript": subagents,
            "sidecarOnlySubagents": sidecar_only,
            "metadataSessions": plan.trees.len() as u64 + subagents + sidecar_only,
            "projects": plan.trees.iter().map(|t| t.project).collect::<std::collections::BTreeSet<_>>().len(),
            "singleFileTrees": plan.trees.iter().filter(|t| t.files() == 1).count(),
            "newestGiant": {"treeBytes": giant.bytes(), "files": giant.files(), "maxFileBytes": giant.max_file(),
                "rootIsLargestFile": giant.max_file() == giant.root_bytes},
            "rings": rings,
            "codexRollouts": codex_files,
        },
        "scaling": {
            "countsKept": "roots, big trees (largest source over the big line), subagents with and without transcripts, sidecars, claude-code files, projects, the age rings' tree counts and big counts",
            "bytesKept": "the newest day (ring newest-day) at full scale: the newest giant exactly, the other big trees and the small trees as synthetic draws summing to the census's newest-day bytes",
            "bytesScaled": format!("every older tree: a big tree's root transcript is the big line plus [{}, {}) bytes (it stays big), its other files and every older small tree are a heavy-tailed synthetic size x {} (floored at {} bytes a transcript)",
                spec.tail_big_root_extra.0, spec.tail_big_root_extra.1, spec.tail_scale, spec.min_transcript_bytes),
            "tailScale": spec.tail_scale,
            "fileMtimes": "a root transcript carries its tree's newest mtime; subagent transcripts are older (25 h to 7 d before the root in the newest day, else 1 h to 7 d); a sidecar takes its transcript's mtime",
            "notModelled": "workflow journals and runs, todos and every other non-transcript stream; per-file content cost is synthetic, not organic",
        },
        "manifestFile": "SHA256SUMS",
        "manifestSha256": hex(&Sha256::digest(sums.as_bytes())),
        "walkInputsDigest": walk_inputs_digest(files),
        "walkInputsMethod": WALK_INPUTS_METHOD,
    })
}

/// Run `work` over `items` on up to `threads` threads; the first error wins.
fn run_parallel<T: Sync>(
    items: &[T],
    threads: usize,
    work: impl Fn(&T) -> io::Result<()> + Sync,
) -> io::Result<()> {
    let next = AtomicUsize::new(0);
    let failed: Mutex<Option<io::Error>> = Mutex::new(None);
    std::thread::scope(|scope| {
        for _ in 0..threads.max(1) {
            scope.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= items.len() || failed.lock().expect("error slot").is_some() {
                        return;
                    }
                    if let Err(error) = work(&items[i]) {
                        failed.lock().expect("error slot").get_or_insert(error);
                        return;
                    }
                }
            });
        }
    });
    match failed.into_inner().expect("error slot") {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

fn write_frozen(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o444))
}

pub fn set_mtime(path: &Path, ms: i64) -> io::Result<()> {
    let at = UNIX_EPOCH + Duration::from_millis(u64::try_from(ms).map_err(io::Error::other)?);
    fs::OpenOptions::new()
        .write(true)
        .open(path)?
        .set_modified(at)
}

pub fn mtime_ms(meta: &fs::Metadata) -> io::Result<i64> {
    let since = meta
        .modified()?
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_err(io::Error::other)?;
    Ok((since.as_nanos() / 1_000_000) as i64)
}

pub fn sha256_file(path: &Path) -> io::Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok(hex(&hash.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Every regular file under `root`, `/`-relative.
pub fn files_below(root: &Path) -> io::Result<Vec<String>> {
    let mut out = Vec::new();
    let mut stack = vec![(root.to_path_buf(), String::new())];
    while let Some((dir, rel)) = stack.pop() {
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let r = if rel.is_empty() {
                name
            } else {
                format!("{rel}/{name}")
            };
            let kind = entry.file_type()?;
            if kind.is_dir() {
                stack.push((entry.path(), r));
            } else if kind.is_file() {
                out.push(r);
            } else {
                return Err(io::Error::other("snapshot holds a non-regular entry"));
            }
        }
    }
    Ok(out)
}

fn directories_below(root: &Path) -> io::Result<Vec<PathBuf>> {
    let mut out = vec![root.to_path_buf()];
    let mut i = 0;
    while i < out.len() {
        for entry in fs::read_dir(&out[i])? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                out.push(entry.path());
            }
        }
        i += 1;
    }
    Ok(out)
}
