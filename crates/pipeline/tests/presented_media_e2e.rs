//! Files Claude Code sent the user by path (`SendUserFile`), end to end with
//! the real Claude adapter: captured into the ledger when their record is,
//! shown as assets, and replayed from the ledger alone, through every pass
//! that can meet a sent file again.
//!
//! The transcripts are synthetic and every sent file is a temp file; nothing
//! here reads a live home. `presented_media.rs` covers the same seam with
//! fake adapters and hand-built ledgers. This file keeps the shared fixture;
//! the tests are in `presented_media_e2e/`, by the kind of pass they drive.

use std::any::Any;
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use base64::Engine;
use rosetta_adapter_claude_code::ClaudeCodeAdapter;
use rosetta_adapter_support::{
    AgentAdapter, FileStat, GroundTruthDoc, LineOutcome, NormalizeCtx, SkimResult, StreamDriver,
};
use rosetta_capture::{
    CapturedDocument, DocumentCaptureStatus as Status, Ledger, LedgerEntry, MediaClaim,
    ScriptedEvents, scan_ledger, verify_ledger,
};
use rosetta_core::ids;
use rosetta_core::rows::RuntimeRow;
use rosetta_core::timeline::AttachmentKind::{self, Image, Video};
use rosetta_core::timeline::{AttachmentRef, AttachmentSource, TimelineBody};
use rosetta_pipeline::meta::SessionFile;
use rosetta_pipeline::progress::ColdProgressObserver;
use rosetta_pipeline::{
    BlockingEmit, CapturePool, ColdWalkOptions, CwdExclusions, IngestOptions, Instrument,
    LiveOptions, LiveRunner, PipeCheckpoint, PipelineError, PoolBudgets, RebuildOptions,
    StreamedPreparer, StreamedRuntime, apply_prepared_runtime, meta_ingest_into, prepare_runtime,
    rebuild_runtime_with_options, stream_backfill_into, stream_closed_sources_into,
    stream_closed_sources_pooled, streamed_setup,
};
use rosetta_store::{Store, assets, queries};
use serde_json::{Value, json};
use tempfile::TempDir;

const NATIVE: &str = "native";
const MP4: &str = "video/mp4";
const PNG: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+ip1sAAAAASUVORK5CYII=";
/// A one-pixel GIF: an image whose bytes are not the PNG's.
const GIF: &str = "R0lGODlhAQABAIAAAP///wAAACH5BAEAAAAALAAAAAABAAEAAAICRAEAOw==";

fn decoded(base64: &str) -> Vec<u8> {
    base64::engine::general_purpose::STANDARD
        .decode(base64)
        .unwrap()
}

/// A well-formed MP4 of exactly `len` bytes: an `ftyp` box of major brand
/// `isom`, then one `free` box of padding. `seed` makes its content its own.
fn mp4(len: usize, seed: u8) -> Vec<u8> {
    assert!(len >= 28);
    let mut bytes = 20_u32.to_be_bytes().to_vec();
    bytes.extend_from_slice(b"ftypisom");
    bytes.extend_from_slice(&[0, 0, 2, 0]);
    bytes.extend_from_slice(b"mp41");
    bytes.extend_from_slice(&u32::try_from(len - 20).unwrap().to_be_bytes());
    bytes.extend_from_slice(b"free");
    bytes.resize(len, seed);
    bytes
}

fn options() -> IngestOptions {
    IngestOptions {
        audit_files_per_run: 0,
        ..IngestOptions::default()
    }
}

/// One file a `SendUserFile` result lists: `(path, size, media type)`.
type Sent<'a> = (&'a str, u64, &'a str);

fn line(kind: &str, native: &str, cwd: &Path, uuid: String, message: Value) -> Value {
    json!({"type":kind,"parentUuid":null,"isSidechain":false,"uuid":uuid,
        "timestamp":"2026-10-01T12:00:00Z","sessionId":native,"version":"2.1.283","cwd":cwd,
        "gitBranch":"main","userType":"external","message":message})
}

/// The assistant line that calls `SendUserFile`.
fn opener(native: &str, cwd: &Path, call: &str, files: &[Sent<'_>]) -> Value {
    let paths: Vec<&str> = files.iter().map(|(path, _, _)| *path).collect();
    let message = json!({"id":format!("response-{call}"),"type":"message","role":"assistant",
        "model":"claude-opus-5","stop_reason":"tool_use",
        "content":[{"type":"tool_use","id":call,"name":"SendUserFile",
            "input":{"files":paths,"caption":"Here it is"}}]});
    line("assistant", native, cwd, format!("open-{call}"), message)
}

/// The user line that carries the call's result: `content` is the
/// `tool_result` block's own content, `files` the `toolUseResult` list.
fn result(native: &str, cwd: &Path, call: &str, content: Value, files: &[Sent<'_>]) -> Value {
    let attachments: Vec<Value> = files
        .iter()
        .map(|(path, size, media_type)| {
            json!({"path":path,"size":size,"media_type":media_type,
                "isImage":media_type.starts_with("image/"),"pathValidated":true,
                "file_uuid":"00000000-0000-4000-8000-000000000000"})
        })
        .collect();
    let message = json!({"role":"user","content":[
        {"type":"tool_result","tool_use_id":call,"content":content,"is_error":false}]});
    let mut record = line("user", native, cwd, format!("result-{call}"), message);
    record["toolUseResult"] =
        json!({"attachments":attachments,"caption":"Here it is","display":"inline"});
    record
}

/// Append one record to a transcript; returns the offset it starts at.
fn append_line(path: &Path, record: &Value) -> u64 {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    let offset = file.metadata().unwrap().len();
    writeln!(file, "{record}").unwrap();
    offset
}

struct Fixture {
    temp: TempDir,
    root: PathBuf,
    db: PathBuf,
    ledger_dir: PathBuf,
    runtime: RuntimeRow,
    store: Store,
    ledger: Ledger,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("claude");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(temp.path().join("media")).unwrap();
        let db = temp.path().join("projection.sqlite");
        let ledger_dir = temp.path().join("ledger");
        let runtime = RuntimeRow {
            id: ids::runtime_id("claude-code", root.to_str().unwrap()),
            kind: "claude-code".into(),
            data_root: root.to_str().unwrap().into(),
            enabled: true,
            account: None,
            config: None,
            skills: None,
            versions_seen: None,
            discovered_at: 1,
            label: None,
            provenance: Default::default(),
        };
        let store = Store::open(&db).unwrap();
        let ledger = Ledger::open(&ledger_dir).unwrap();
        Self {
            temp,
            root,
            db,
            ledger_dir,
            runtime,
            store,
            ledger,
        }
    }

    fn transcript(&self, native: &str) -> PathBuf {
        self.root.join(format!("projects/synthetic/{native}.jsonl"))
    }

    fn session(&self, native: &str) -> String {
        ids::session_id(&self.runtime.id, native)
    }

    fn open_call(&self, native: &str, call: &str, files: &[Sent<'_>]) -> u64 {
        let record = opener(native, self.temp.path(), call, files);
        append_line(&self.transcript(native), &record)
    }

    fn finish_call(&self, native: &str, call: &str, content: Value, files: &[Sent<'_>]) -> u64 {
        let record = result(native, self.temp.path(), call, content, files);
        append_line(&self.transcript(native), &record)
    }

    /// A user line of plain prose; returns the offset it starts at.
    fn prompt(&self, native: &str, uuid: &str, text: &str) -> u64 {
        let message = json!({"role":"user","content":text});
        let record = line("user", native, self.temp.path(), uuid.into(), message);
        append_line(&self.transcript(native), &record)
    }

    /// One whole `SendUserFile` call; returns the offset of its result.
    fn send(&self, native: &str, call: &str, files: &[Sent<'_>]) -> u64 {
        self.open_call(native, call, files);
        self.finish_call(native, call, json!("sent"), files)
    }

    /// A file an agent could send, under the fixture's own directory.
    fn media(&self, name: &str, bytes: &[u8]) -> String {
        let path = self.temp.path().join("media").join(name);
        std::fs::write(&path, bytes).unwrap();
        path.to_str().unwrap().to_owned()
    }

    fn ingest(&mut self) {
        self.ingest_with(&options());
    }

    fn ingest_with(&mut self, options: &IngestOptions) {
        let (runtime, root, ledger) = (&self.runtime, &self.root, Some(&mut self.ledger));
        try_ingest(
            &ClaudeCodeAdapter,
            runtime,
            root,
            &mut self.store,
            ledger,
            options,
        )
        .unwrap();
    }

    /// One ordinary pass of this root into another projection, such as a
    /// rebuilt one, over the fixture's own ledger handle.
    fn catch_up(&mut self, store: &mut Store) {
        let (runtime, root, ledger) = (&self.runtime, &self.root, Some(&mut self.ledger));
        try_ingest(&ClaudeCodeAdapter, runtime, root, store, ledger, &options()).unwrap();
    }

    /// The daemon's live runner over this root, started as a restart starts
    /// it: on a projection, with its own handle on a ledger and these hints.
    fn runner(&self, store: &mut Store, ledger_dir: &Path, hints: &[&str]) -> LiveRunner<'static> {
        let mut events = ScriptedEvents::default();
        for hint in hints {
            events.push_changed(hint);
        }
        let live = LiveOptions {
            tick: Duration::ZERO,
            poll_interval: Duration::from_secs(3600),
            max_coalesced: 16,
        };
        LiveRunner::start_with_ledger(
            &ClaudeCodeAdapter,
            self.runtime.clone(),
            self.root.clone(),
            Box::new(events),
            store,
            options(),
            live,
            Some(Ledger::open(ledger_dir).unwrap()),
            &Instrument::new(),
        )
        .unwrap()
    }

    /// Close the projection, change its files, and open it again.
    fn reopen(&mut self, between: impl FnOnce(&Path)) {
        self.store = Store::open_in_memory().unwrap();
        between(&self.db);
        self.store = Store::open(&self.db).unwrap();
    }

    /// A fresh projection rebuilt from a ledger alone.
    fn rebuild(&self, name: &str, ledger_dir: &Path, options: RebuildOptions) -> (Store, PathBuf) {
        let db = self.temp.path().join(format!("{name}.sqlite"));
        let mut store = Store::open(&db).unwrap();
        rebuild_runtime_with_options(
            &ClaudeCodeAdapter,
            &self.runtime,
            ledger_dir,
            &mut store,
            &Instrument::new(),
            options,
        )
        .unwrap();
        (store, db)
    }

    /// Rebuild this fixture's ledger and return what the projection shows.
    fn rebuilt(&self, name: &str) -> Shown {
        let (store, db) = self.rebuild(name, &self.ledger_dir, RebuildOptions::default());
        shown(&store, &db)
    }

    /// Remove everything a replay must not read: the sent files, the agent's
    /// data and the live projection's members.
    fn remove_every_source(&self) {
        std::fs::remove_dir_all(self.temp.path().join("media")).unwrap();
        std::fs::remove_dir_all(&self.root).unwrap();
        std::fs::remove_dir_all(assets::asset_root(&self.db)).unwrap();
    }
}

/// The metadata pass, then one stream pass over every source it listed.
fn try_ingest(
    adapter: &dyn AgentAdapter,
    runtime: &RuntimeRow,
    root: &Path,
    store: &mut Store,
    mut ledger: Option<&mut Ledger>,
    options: &IngestOptions,
) -> Result<(), PipelineError> {
    let instrument = Instrument::new();
    let (_, files) = meta_ingest_into(
        adapter,
        runtime,
        root,
        store,
        options,
        &instrument,
        ledger.as_deref_mut(),
    )?;
    let streamed = stream_backfill_into(
        adapter,
        runtime,
        root,
        store,
        &files,
        options,
        &instrument,
        ledger.as_deref_mut(),
    );
    if let Some(ledger) = ledger {
        ledger.flush().unwrap();
    }
    assert_eq!(streamed?.hard_errors, 0);
    Ok(())
}

/// The stored checkpoint of a projection's one source.
fn checkpoint(store: &Store) -> PipeCheckpoint {
    let sql = "SELECT checkpoint FROM ingest_cursor";
    let blob: String = store.conn().query_row(sql, [], |row| row.get(0)).unwrap();
    PipeCheckpoint::decode(&blob).unwrap()
}

/// Every tool row's attachments, in timeline order.
fn attachments(store: &Store, session: &str) -> Vec<AttachmentRef> {
    queries::timeline_page(store.conn(), session, None, 100)
        .unwrap()
        .into_iter()
        .flat_map(|row| match row.body {
            TimelineBody::ToolUse { tool } => tool.attachments,
            _ => Vec::new(),
        })
        .collect()
}

/// What a projection shows of sent files: the stored `tool_detail` of every
/// tool row (its attachments included), in session and timeline order, and
/// every member as `name → blake3 of its bytes`.
type Shown = (Vec<String>, BTreeMap<String, String>);

fn shown(store: &Store, db: &Path) -> Shown {
    let details = store
        .conn()
        .prepare(
            "SELECT tool_detail FROM message
             WHERE tool_detail IS NOT NULL ORDER BY session_id, ordinal",
        )
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let members = std::fs::read_dir(assets::asset_root(db))
        .into_iter()
        .flatten()
        .map(Result::unwrap)
        .filter(|entry| entry.file_type().unwrap().is_file())
        .map(|entry| {
            (
                entry.file_name().to_str().unwrap().to_owned(),
                digest(&std::fs::read(entry.path()).unwrap()),
            )
        })
        .collect();
    (details, members)
}

fn digest(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

/// The members a projection holds when exactly these files were captured.
fn members(files: &[(&str, &[u8])]) -> BTreeMap<String, String> {
    files
        .iter()
        .map(|(extension, bytes)| (format!("{}.{extension}", digest(bytes)), digest(bytes)))
        .collect()
}

/// A ledger's document frames that carry a media claim, in ledger order.
struct Frame {
    source: String,
    generation: u64,
    offset: u64,
    document: CapturedDocument,
}

fn media_frames(ledger_dir: &Path) -> Vec<Frame> {
    let entries = scan_ledger(ledger_dir).unwrap().entries;
    let documents = entries.into_iter().filter_map(|entry| match entry {
        LedgerEntry::Document {
            source,
            generation,
            start_offset,
            document,
        } => Some(Frame {
            source: source.relative_path,
            generation,
            offset: start_offset,
            document,
        }),
        _ => None,
    });
    let frames: Vec<Frame> = documents.collect();
    assert!(
        frames.iter().all(|frame| frame.document.media.is_some()),
        "these transcripts publish no page, so every document frame is a media frame"
    );
    frames
}

/// Each media frame as `(generation, record offset, status, recovered_from)`.
fn frame_rows(ledger_dir: &Path) -> Vec<(u64, u64, Status, Option<String>)> {
    let row = |frame: Frame| {
        let document = frame.document;
        let (status, recovered_from) = (document.status, document.recovered_from);
        (frame.generation, frame.offset, status, recovered_from)
    };
    media_frames(ledger_dir).into_iter().map(row).collect()
}

/// Each media frame as `(generation, record offset, status)`.
fn frame_statuses(ledger_dir: &Path) -> Vec<(u64, u64, Status)> {
    media_frames(ledger_dir)
        .iter()
        .map(|frame| (frame.generation, frame.offset, frame.document.status))
        .collect()
}

/// An attachment as `(kind, member digest, name, media type, size)`; the
/// digest is absent for a path reference.
type Slot = (
    AttachmentKind,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<u64>,
);

/// Every tool row's attachments of a session, each as its [`Slot`].
fn slots(store: &Store, session: &str) -> Vec<Slot> {
    let slot = |attachment: AttachmentRef| {
        let digest = match &attachment.source {
            AttachmentSource::Asset { asset } => Some(asset.digest.clone()),
            AttachmentSource::Reference { .. } => None,
            other => panic!("neither a member nor a path reference: {other:?}"),
        };
        let AttachmentRef {
            kind,
            name,
            media_type,
            byte_len,
            ..
        } = attachment;
        (kind, digest, name, media_type, byte_len)
    };
    attachments(store, session).into_iter().map(slot).collect()
}

/// A sent file that was captured: the member of exactly these bytes.
fn asset(kind: AttachmentKind, bytes: &[u8], name: &str, mime: &str) -> Slot {
    let (name, mime) = (Some(name.to_owned()), Some(mime.to_owned()));
    (
        kind,
        Some(digest(bytes)),
        name,
        mime,
        Some(bytes.len() as u64),
    )
}

/// A sent file that was not: its path, with what the transcript recorded.
fn reference(kind: AttachmentKind, name: &str, mime: &str, len: u64) -> Slot {
    (
        kind,
        None,
        Some(name.to_owned()),
        Some(mime.to_owned()),
        Some(len),
    )
}

#[path = "presented_media_e2e/append_growth.rs"]
mod append_growth;

#[path = "presented_media_e2e/capture.rs"]
mod capture;

#[path = "presented_media_e2e/cold.rs"]
mod cold;

#[path = "presented_media_e2e/reread.rs"]
mod reread;

#[path = "presented_media_e2e/standing.rs"]
mod standing;
