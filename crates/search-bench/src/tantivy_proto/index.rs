use std::fs::{self, File};
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::time::Instant;

use rosetta_search::{Fts5SidecarSearch, SearchBackend, SearchReadiness};
use rusqlite::{Connection, OpenFlags};
use tantivy::schema::{FAST, Field, INDEXED, STORED, STRING, Schema, TEXT, TantivyDocument};
use tantivy::{Index, IndexWriter};

use super::model::{IndexFingerprint, TantivyBuildReport};

const HASH_BUFFER_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy)]
pub(crate) struct Fields {
    pub timeline_id: Field,
    pub session_id: Field,
    pub runtime: Field,
    pub agent: Field,
    pub project: Field,
    pub kind: Field,
    pub is_error: Field,
    pub ts: Field,
    pub text: [Field; 5],
}

pub(crate) fn prototype_schema() -> (Schema, Fields) {
    let mut builder = Schema::builder();
    // FAST supports the required score-then-timeline-id ordering inside the
    // top-K collector, including ties at the LIMIT boundary. Sorting only the
    // returned K rows would leave segment-layout-dependent membership.
    let timeline_id = builder.add_text_field("timeline_id", STRING | STORED | FAST);
    let session_id = builder.add_text_field("session_id", STRING | STORED);
    let runtime = builder.add_text_field("runtime", STRING | STORED);
    let agent = builder.add_text_field("agent", STRING | STORED);
    let project = builder.add_text_field("project", STRING | STORED);
    let kind = builder.add_text_field("kind", STRING | STORED);
    let is_error = builder.add_text_field("is_error", STRING | STORED);
    // Missing timestamps remain absent so any explicit range excludes them.
    let ts = builder.add_i64_field("ts", INDEXED | FAST | STORED);
    // Use the same materialized semantic lanes as the production FTS sidecar.
    let text = ["prose", "target", "intent", "change", "tool"]
        .map(|name| builder.add_text_field(name, TEXT | STORED));
    let schema = builder.build();
    (
        schema,
        Fields {
            timeline_id,
            session_id,
            runtime,
            agent,
            project,
            kind,
            is_error,
            ts,
            text,
        },
    )
}

fn open_read(db: &Path) -> Result<Connection, String> {
    let conn = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|error| format!("opening {} read-only: {error}", db.display()))?;
    conn.execute_batch("PRAGMA query_only = ON;")
        .map_err(|error| format!("configuring read-only projection: {error}"))?;
    Ok(conn)
}

fn sqlite_fts_bytes(conn: &Connection) -> Result<u64, String> {
    let bytes: i64 = conn
        .query_row(
            "SELECT COALESCE(SUM(pgsize), 0) FROM dbstat WHERE name GLOB 'message_fts*'",
            [],
            |row| row.get(0),
        )
        .map_err(|error| format!("measuring SQLite FTS pages: {error}"))?;
    u64::try_from(bytes).map_err(|_| format!("SQLite FTS byte count is negative: {bytes}"))
}

#[allow(clippy::too_many_arguments)]
fn add_row(
    writer: &IndexWriter<TantivyDocument>,
    fields: Fields,
    timeline_id: String,
    session_id: String,
    runtime: String,
    agent: String,
    project: Option<String>,
    kind: String,
    is_error: Option<bool>,
    ts: Option<i64>,
    text: [String; 5],
) -> Result<u64, String> {
    let mut doc = TantivyDocument::default();
    doc.add_text(fields.timeline_id, timeline_id);
    doc.add_text(fields.session_id, session_id);
    doc.add_text(fields.runtime, runtime);
    doc.add_text(fields.agent, agent);
    doc.add_text(fields.project, project.unwrap_or_default());
    doc.add_text(fields.kind, kind);
    doc.add_text(
        fields.is_error,
        is_error.map_or("unknown", |value| if value { "true" } else { "false" }),
    );
    if let Some(ts) = ts {
        doc.add_i64(fields.ts, ts);
    }
    let mut text_bytes = 0_u64;
    for (field, value) in fields.text.into_iter().zip(text) {
        text_bytes = text_bytes
            .checked_add(value.len() as u64)
            .ok_or("source text byte count overflow")?;
        doc.add_text(field, value);
    }
    writer
        .add_document(doc)
        .map_err(|error| format!("adding Tantivy document: {error}"))?;
    Ok(text_bytes)
}

pub(crate) fn build_index(
    db: &Path,
    destination: &Path,
    threads: usize,
    memory_bytes: usize,
) -> Result<(TantivyBuildReport, Index, Fields), String> {
    if threads == 0 {
        return Err("Tantivy build needs at least one worker thread".into());
    }
    if destination.exists() {
        return Err(format!(
            "{} already exists; prototype builds never overwrite an index",
            destination.display()
        ));
    }
    fs::create_dir(destination)
        .map_err(|error| format!("creating {}: {error}", destination.display()))?;

    let search = Fts5SidecarSearch::new(db);
    if search.state().readiness != SearchReadiness::Ready {
        return Err("Tantivy build requires a ready production search generation".into());
    }
    let generation = search
        .current_generation_path()
        .map_err(|error| error.to_string())?
        .ok_or("Tantivy build requires a published search generation")?;
    let conn = open_read(&generation)?;
    conn.execute_batch("BEGIN")
        .map_err(|error| error.to_string())?;
    let fts_bytes = sqlite_fts_bytes(&conn)?;
    let (schema, fields) = prototype_schema();
    let started = Instant::now();
    let index = Index::create_in_dir(destination, schema)
        .map_err(|error| format!("creating Tantivy index: {error}"))?;
    let mut writer: IndexWriter<TantivyDocument> = index
        .writer_with_num_threads(threads, memory_bytes)
        .map_err(|error| format!("opening Tantivy writer: {error}"))?;
    let mut stmt = conn
        .prepare(
            "SELECT d.timeline_id, d.session_id, d.runtime_id, d.agent, d.project_id,
                    d.kind, d.is_error, d.ts, f.prose, f.target, f.intent, f.change, f.tool
             FROM search_document d
             JOIN message_fts f ON f.rowid = d.id
             ORDER BY d.id",
        )
        .map_err(|error| format!("preparing canonical document scan: {error}"))?;
    let mut rows = stmt
        .query([])
        .map_err(|error| format!("starting canonical document scan: {error}"))?;
    let mut documents = 0_u64;
    let mut source_text_bytes = 0_u64;
    while let Some(row) = rows
        .next()
        .map_err(|error| format!("reading canonical document: {error}"))?
    {
        source_text_bytes = source_text_bytes
            .checked_add(add_row(
                &writer,
                fields,
                row.get(0).map_err(|error| error.to_string())?,
                row.get(1).map_err(|error| error.to_string())?,
                row.get(2).map_err(|error| error.to_string())?,
                row.get(3).map_err(|error| error.to_string())?,
                row.get(4).map_err(|error| error.to_string())?,
                row.get(5).map_err(|error| error.to_string())?,
                row.get(6).map_err(|error| error.to_string())?,
                row.get(7).map_err(|error| error.to_string())?,
                [
                    row.get(8).map_err(|error| error.to_string())?,
                    row.get(9).map_err(|error| error.to_string())?,
                    row.get(10).map_err(|error| error.to_string())?,
                    row.get(11).map_err(|error| error.to_string())?,
                    row.get(12).map_err(|error| error.to_string())?,
                ],
            )?)
            .ok_or("source text byte count overflow")?;
        documents += 1;
        if documents.is_multiple_of(100_000) {
            eprintln!("indexed {documents} canonical messages");
        }
    }
    writer
        .commit()
        .map_err(|error| format!("committing Tantivy index: {error}"))?;
    writer
        .wait_merging_threads()
        .map_err(|error| format!("joining Tantivy merge threads: {error}"))?;
    let wall_nanos = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);

    let reader = index
        .reader()
        .map_err(|error| format!("opening completed Tantivy reader: {error}"))?;
    let searcher = reader.searcher();
    let indexed = searcher.num_docs();
    if indexed != documents {
        return Err(format!(
            "Tantivy committed {indexed} documents but scanned {documents}"
        ));
    }
    let segments = searcher.segment_readers().len() as u64;
    drop(searcher);
    drop(reader);

    seal_read_only(destination)?;
    let fingerprint = fingerprint_directory(destination, true)?;
    Ok((
        TantivyBuildReport {
            version: "0.26.1".into(),
            threads,
            memory_bytes: memory_bytes as u64,
            documents,
            source_text_bytes,
            wall_nanos,
            segments,
            index: fingerprint,
            sqlite_fts_bytes: fts_bytes,
        },
        index,
        fields,
    ))
}

fn collect_files(root: &Path, directory: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let mut entries = fs::read_dir(directory)
        .map_err(|error| format!("listing {}: {error}", directory.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("listing {}: {error}", directory.display()))?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| format!("stating {}: {error}", path.display()))?;
        if metadata.file_type().is_symlink() {
            return Err(format!("index contains symlink {}", path.display()));
        }
        if metadata.is_dir() {
            collect_files(root, &path, out)?;
        } else if metadata.is_file() {
            out.push(path);
        } else {
            return Err(format!(
                "index contains non-file artifact {}",
                path.display()
            ));
        }
    }
    directory
        .strip_prefix(root)
        .map_err(|_| format!("{} escaped the index root", directory.display()))?;
    Ok(())
}

#[cfg(unix)]
fn is_read_only(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o222 == 0
}

#[cfg(not(unix))]
fn is_read_only(metadata: &fs::Metadata) -> bool {
    metadata.permissions().readonly()
}

pub(crate) fn fingerprint_directory(
    root: &Path,
    require_read_only: bool,
) -> Result<IndexFingerprint, String> {
    let root = root
        .canonicalize()
        .map_err(|error| format!("resolving {}: {error}", root.display()))?;
    if require_read_only && !is_read_only(&fs::metadata(&root).map_err(|e| e.to_string())?) {
        return Err(format!("{} is writable", root.display()));
    }
    let mut files = Vec::new();
    collect_files(&root, &root, &mut files)?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"rosetta-tantivy-prototype-v1\0");
    let mut total = 0_u64;
    for path in &files {
        let metadata = fs::metadata(path).map_err(|error| error.to_string())?;
        let is_meta_lock =
            path.file_name().and_then(|name| name.to_str()) == Some(".tantivy-meta.lock");
        if is_meta_lock {
            if metadata.len() != 0 {
                return Err(format!("{} is not an empty lock file", path.display()));
            }
        } else if require_read_only && !is_read_only(&metadata) {
            return Err(format!("{} is writable", path.display()));
        }
        let relative = path
            .strip_prefix(&root)
            .map_err(|_| "index path escaped root")?
            .to_str()
            .ok_or("index path is not UTF-8")?
            .replace(std::path::MAIN_SEPARATOR, "/");
        let mut file_hasher = blake3::Hasher::new();
        let mut reader = BufReader::with_capacity(
            HASH_BUFFER_BYTES,
            File::open(path).map_err(|error| error.to_string())?,
        );
        let mut buffer = vec![0_u8; HASH_BUFFER_BYTES];
        let mut bytes = 0_u64;
        loop {
            let read = reader
                .read(&mut buffer)
                .map_err(|error| error.to_string())?;
            if read == 0 {
                break;
            }
            file_hasher.update(&buffer[..read]);
            bytes += read as u64;
        }
        if bytes != metadata.len() {
            return Err(format!("{} changed while hashing", path.display()));
        }
        total = total
            .checked_add(bytes)
            .ok_or("index byte count overflow")?;
        hasher.update(&(relative.len() as u64).to_be_bytes());
        hasher.update(relative.as_bytes());
        hasher.update(&bytes.to_be_bytes());
        hasher.update(file_hasher.finalize().as_bytes());
    }
    Ok(IndexFingerprint {
        files: files.len() as u64,
        bytes: total,
        blake3: hasher.finalize().to_hex().to_string(),
    })
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .map_err(|error| format!("sealing {}: {error}", path.display()))
}

#[cfg(not(unix))]
fn set_mode(path: &Path, _mode: u32) -> Result<(), String> {
    let mut permissions = fs::metadata(path)
        .map_err(|error| error.to_string())?
        .permissions();
    permissions.set_readonly(true);
    fs::set_permissions(path, permissions).map_err(|error| error.to_string())
}

fn seal_read_only(root: &Path) -> Result<(), String> {
    let mut files = Vec::new();
    collect_files(root, root, &mut files)?;
    for path in files {
        // Tantivy's mmap reader takes an OS lock by opening this persistent,
        // empty coordination file for write. Keep exactly this file writable;
        // all index data and the containing generation directory are sealed.
        let mode = if path.file_name().and_then(|name| name.to_str()) == Some(".tantivy-meta.lock")
        {
            0o644
        } else {
            0o444
        };
        set_mode(&path, mode)?;
    }
    // The prototype index is flat today. Walk child directories bottom-up if
    // Tantivy adds any in a later compatible release.
    let mut directories = Vec::new();
    fn collect_dirs(directory: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
        for entry in fs::read_dir(directory).map_err(|error| error.to_string())? {
            let path = entry.map_err(|error| error.to_string())?.path();
            if path.is_dir() {
                collect_dirs(&path, out)?;
                out.push(path);
            }
        }
        Ok(())
    }
    collect_dirs(root, &mut directories)?;
    for directory in directories {
        set_mode(&directory, 0o555)?;
    }
    set_mode(root, 0o555)
}

#[cfg(test)]
pub(crate) fn unseal_for_test(root: &Path) {
    let mut files = Vec::new();
    collect_files(root, root, &mut files).unwrap();
    for path in files {
        set_mode(&path, 0o644).unwrap();
    }
    set_mode(root, 0o755).unwrap();
}
