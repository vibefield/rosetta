//! A file an agent sent the user by path is copied into the ledger when its
//! record is captured, shown as an asset, and replayed from the ledger alone:
//! through every replay entry point, the snapshot driver, a pass that
//! re-reads a source after the file is gone, and append growth that sends a
//! captured file again.
//!
//! The adapters here are fakes: Codex plus two synthetic record types, and a
//! database-first agent. No real agent's sent-file shape is involved.
//!
//! This file holds the shared fixture; the tests are in `presented_media/`,
//! one module per subject.

use std::any::Any;
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use rosetta_adapter_codex::CodexAdapter;
use rosetta_adapter_support::presented_media::{PresentedFile, push};
use rosetta_adapter_support::{
    AgentAdapter, ColdAdmission, FileStat, GroundTruthDoc, LineOutcome, LineStatus, NormalizeCtx,
    SkimResult, SnapshotPlan, SnapshotQuery, StreamDriver,
};
use rosetta_capture::{
    CapturedDocument, DocumentCaptureStatus as Status, Ledger, LedgerEntry, MediaClaim, SourceDef,
    scan_ledger,
};
use rosetta_core::rows::{MessageRow, RuntimeRow};
use rosetta_core::timeline::{AttachmentKind, AttachmentRef, AttachmentSource, TimelineBody};
use rosetta_core::{IngestOp, ids};
use rosetta_pipeline::meta::SessionFile;
use rosetta_pipeline::progress::ColdProgressObserver;
use rosetta_pipeline::{
    IngestOptions, Instrument, PipeCheckpoint, RebuildOptions, activate_stopped_source,
    apply_prepared_runtime, discover, expand_data_root, meta_ingest_into, prepare_runtime,
    rebuild_runtime_with_options, stream_backfill_into,
};
use rosetta_store::html_artifacts::{self, ArtifactListRequest};
use rosetta_store::{Store, assets, queries};
use serde_json::{Value, json};
use tempfile::TempDir;

const SESSION: &str = "media-session";

/// A tool row the synthetic `sent_files` record attaches to.
fn opener(ctx: &mut NormalizeCtx, call: &str) -> Vec<IngestOp> {
    let row: MessageRow = serde_json::from_value(json!({
        "timeline_id": ids::timeline_id(&ctx.session_id, &format!("open-{call}")),
        "session_id": ctx.session_id, "ordinal": ctx.take_ordinal(), "kind": "tool_use",
        "ts": 10, "attachments": [], "citations": [],
        "src_cursor": ctx.src_cursor, "src_offset": ctx.src_offset,
        "tool": {
            "correlation": {"key": call, "derivation": "native_id"},
            "native": {"id": call, "name": "Send"},
            "status": {"value": "succeeded", "basis": "native_terminal_record"},
            "action": {"type": "shell", "command": {"type": "inline", "text": "send"},
                "source_truncated": false, "background": false}
        }
    }))
    .unwrap();
    IngestOp::project_message(row)
}

/// What an adapter does with a proven sent-files result: the reference
/// update, then one fact per presentable file. Pure.
fn sent(ctx: &NormalizeCtx, call: &str, occurrence: &str, files: &Value) -> Vec<IngestOp> {
    let listed: Vec<(String, u64, String)> = files
        .as_array()
        .into_iter()
        .flatten()
        .map(|file| {
            (
                file["path"].as_str().unwrap_or_default().to_owned(),
                file["size"].as_u64().unwrap_or_default(),
                file["media_type"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect();
    let files: Vec<PresentedFile<'_>> = listed
        .iter()
        .map(|(path, len, media_type)| PresentedFile {
            path,
            cwd: None,
            len: *len,
            media_type,
        })
        .collect();
    let mut ops = Vec::new();
    let facts = push(ctx, call, occurrence, &files, Some(20), &mut ops);
    ops.extend(facts.into_iter().map(IngestOp::PresentedMedia));
    ops
}

/// Codex, plus `sent_open` (a tool row) and `sent_files` (its sent files).
struct Sender;

impl AgentAdapter for Sender {
    fn kind(&self) -> &'static str {
        CodexAdapter.kind()
    }

    fn layout(&self) -> &GroundTruthDoc {
        CodexAdapter.layout()
    }

    fn stream_driver(&self, stream_id: &str) -> Option<StreamDriver> {
        CodexAdapter.stream_driver(stream_id)
    }

    fn census_session_streams(&self) -> Option<&'static [&'static str]> {
        CodexAdapter.census_session_streams()
    }

    fn cold_admission(&self) -> ColdAdmission {
        CodexAdapter.cold_admission()
    }

    fn normalizer_version(&self) -> u32 {
        CodexAdapter.normalizer_version()
    }

    fn html_artifact_hints(
        &self,
        ctx: &NormalizeCtx,
        publication: &rosetta_core::html_artifacts::HtmlArtifactPublication,
        data_root: &str,
    ) -> rosetta_adapter_support::html_artifacts::CaptureHints {
        CodexAdapter.html_artifact_hints(ctx, publication, data_root)
    }

    fn html_artifact_record(&self, record: &[u8], path: &str) -> Option<Vec<u8>> {
        CodexAdapter.html_artifact_record(record, path)
    }

    fn snapshot_plan(&self, stream_id: &str) -> Option<SnapshotPlan> {
        CodexAdapter.snapshot_plan(stream_id)
    }

    fn snapshot_session(&self, stream_id: &str, row: &Value) -> SkimResult {
        CodexAdapter.snapshot_session(stream_id, row)
    }

    fn document_sessions(
        &self,
        stream_id: &str,
        stat: &FileStat,
        document: &[u8],
    ) -> Vec<SkimResult> {
        CodexAdapter.document_sessions(stream_id, stat, document)
    }

    fn document_size_limit(&self, stream_id: &str, pipeline_limit: usize) -> usize {
        CodexAdapter.document_size_limit(stream_id, pipeline_limit)
    }

    fn document_records(
        &self,
        stream_id: &str,
        native_session_id: &str,
        document: &[u8],
    ) -> Vec<Vec<u8>> {
        CodexAdapter.document_records(stream_id, native_session_id, document)
    }

    fn init_state(&self) -> Option<Box<dyn Any + Send>> {
        CodexAdapter.init_state()
    }

    fn checkpoint_state(&self, state: Option<&(dyn Any + Send)>) -> Result<Option<Value>, String> {
        CodexAdapter.checkpoint_state(state)
    }

    fn restore_state(
        &self,
        checkpoint: Option<&Value>,
    ) -> Result<Option<Box<dyn Any + Send>>, String> {
        CodexAdapter.restore_state(checkpoint)
    }

    fn skim(&self, stream_id: &str, stat: &FileStat, head: &[u8], tail: &[u8]) -> SkimResult {
        CodexAdapter.skim(stream_id, stat, head, tail)
    }

    fn wants_skim_fallback(&self, stream_id: &str, skim: &SkimResult) -> bool {
        CodexAdapter.wants_skim_fallback(stream_id, skim)
    }

    fn absorb_skim_fallback_record(
        &self,
        stream_id: &str,
        record: &[u8],
        skim: &mut SkimResult,
    ) -> bool {
        CodexAdapter.absorb_skim_fallback_record(stream_id, record, skim)
    }

    fn ingest_line(&self, ctx: &mut NormalizeCtx, stream_id: &str, line: &[u8]) -> LineOutcome {
        let record = serde_json::from_slice::<Value>(line).ok();
        let kind = record.as_ref().and_then(|record| record["type"].as_str());
        match (kind, &record) {
            (Some("sent_open"), Some(record)) => {
                LineOutcome::parsed(opener(ctx, record["call"].as_str().unwrap_or_default()))
            }
            (Some("sent_files"), Some(record)) => LineOutcome::parsed(sent(
                ctx,
                record["call"].as_str().unwrap_or_default(),
                record["id"].as_str().unwrap_or_default(),
                &record["files"],
            )),
            _ => CodexAdapter.ingest_line(ctx, stream_id, line),
        }
    }
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

fn png() -> Vec<u8> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(
            "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+ip1sAAAAASUVORK5CYII=",
        )
        .unwrap()
}

fn options() -> IngestOptions {
    IngestOptions {
        audit_files_per_run: 0,
        ..IngestOptions::default()
    }
}

struct Fixture {
    temp: TempDir,
    root: PathBuf,
    transcript: PathBuf,
    db: PathBuf,
    ledger_dir: PathBuf,
    runtime: RuntimeRow,
    store: Store,
    ledger: Ledger,
}

fn header(cwd: &Path) -> Value {
    json!({"timestamp":"2026-10-01T00:00:00Z","type":"session_meta","payload":{"id":SESSION,"timestamp":"2026-10-01T00:00:00Z","cwd":cwd,"originator":"codex_exec","cli_version":"0.150.1","source":"cli","model_provider":"openai"}})
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("codex");
        let transcript = root.join("sessions/2026/10/01/rollout-media.jsonl");
        std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
        std::fs::create_dir_all(temp.path().join("media")).unwrap();
        let db = temp.path().join("projection.sqlite");
        let ledger_dir = temp.path().join("ledger");
        let runtime = RuntimeRow {
            id: ids::runtime_id("codex", root.to_str().unwrap()),
            kind: "codex".into(),
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
        std::fs::write(&transcript, format!("{}\n", header(temp.path()))).unwrap();
        let store = Store::open(&db).unwrap();
        let ledger = Ledger::open(&ledger_dir).unwrap();
        Self {
            temp,
            root,
            transcript,
            db,
            ledger_dir,
            runtime,
            store,
            ledger,
        }
    }

    /// Append one record; returns the offset it starts at.
    fn append(&self, record: Value) -> u64 {
        let offset = std::fs::metadata(&self.transcript).unwrap().len();
        writeln!(
            std::fs::OpenOptions::new()
                .append(true)
                .open(&self.transcript)
                .unwrap(),
            "{record}"
        )
        .unwrap();
        offset
    }

    fn open_call(&self, call: &str) -> u64 {
        self.append(json!({"type":"sent_open","call":call}))
    }

    /// A sent-files result for `call`; `files` are `(path, size, type)`.
    fn send(&self, call: &str, id: &str, files: &[(&str, u64, &str)]) -> u64 {
        let files: Vec<Value> = files
            .iter()
            .map(
                |(path, size, media_type)| json!({"path":path,"size":size,"media_type":media_type}),
            )
            .collect();
        self.append(json!({"type":"sent_files","call":call,"id":id,"files":files}))
    }

    fn media(&self, name: &str, bytes: &[u8]) -> String {
        let path = self.temp.path().join("media").join(name);
        std::fs::write(&path, bytes).unwrap();
        path.to_str().unwrap().to_owned()
    }

    fn ingest(&mut self) {
        self.ingest_with(&options());
    }

    fn ingest_with(&mut self, options: &IngestOptions) {
        ingest(
            &self.runtime,
            &self.root,
            &mut self.store,
            &mut self.ledger,
            options,
        );
    }

    /// A fresh projection rebuilt from the ledger alone.
    fn rebuild(&self, name: &str, options: RebuildOptions) -> (Store, PathBuf) {
        rebuild(
            &self.runtime,
            &self.ledger_dir,
            self.temp.path(),
            name,
            options,
        )
    }
}

fn ingest(
    runtime: &RuntimeRow,
    root: &Path,
    store: &mut Store,
    ledger: &mut Ledger,
    options: &IngestOptions,
) {
    let instrument = Instrument::new();
    let (_, files) = meta_ingest_into(
        &Sender,
        runtime,
        root,
        store,
        options,
        &instrument,
        Some(&mut *ledger),
    )
    .unwrap();
    let report = stream_backfill_into(
        &Sender,
        runtime,
        root,
        store,
        &files,
        options,
        &instrument,
        Some(&mut *ledger),
    )
    .unwrap();
    assert_eq!(report.hard_errors, 0);
    ledger.flush().unwrap();
}

fn rebuild(
    runtime: &RuntimeRow,
    ledger_dir: &Path,
    temp: &Path,
    name: &str,
    options: RebuildOptions,
) -> (Store, PathBuf) {
    let db = temp.join(format!("{name}.sqlite"));
    let mut store = Store::open(&db).unwrap();
    rebuild_runtime_with_options(
        &Sender,
        runtime,
        ledger_dir,
        &mut store,
        &Instrument::new(),
        options,
    )
    .unwrap();
    (store, db)
}

fn session_id(runtime: &RuntimeRow) -> String {
    ids::session_id(&runtime.id, SESSION)
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

/// The stored `tool_detail` of every tool row, in timeline order.
fn tool_details(store: &Store, session: &str) -> Vec<String> {
    store
        .conn()
        .prepare(
            "SELECT tool_detail FROM message
             WHERE session_id = ?1 AND tool_detail IS NOT NULL ORDER BY ordinal",
        )
        .unwrap()
        .query_map([session], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

/// Every member of a projection, by file name.
fn members(db: &Path) -> BTreeMap<String, Vec<u8>> {
    let Ok(entries) = std::fs::read_dir(assets::asset_root(db)) else {
        return BTreeMap::new();
    };
    entries
        .map(Result::unwrap)
        .filter(|entry| entry.file_type().unwrap().is_file())
        .map(|entry| {
            (
                entry.file_name().to_str().unwrap().to_owned(),
                std::fs::read(entry.path()).unwrap(),
            )
        })
        .collect()
}

/// The media frames of a ledger: `(generation, offset, frame)` in ledger order.
fn media_frames(ledger_dir: &Path) -> Vec<(u64, u64, CapturedDocument)> {
    scan_ledger(ledger_dir)
        .unwrap()
        .entries
        .into_iter()
        .filter_map(|entry| match entry {
            LedgerEntry::Document {
                generation,
                start_offset,
                document,
                ..
            } if document.media.is_some() => Some((generation, start_offset, document)),
            _ => None,
        })
        .collect()
}

fn asset_digest(attachment: &AttachmentRef) -> Option<&str> {
    match &attachment.source {
        AttachmentSource::Asset { asset } => Some(&asset.digest),
        _ => None,
    }
}

fn digest(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

fn frame(path: &str, status: Status, bytes: &[u8], expected_len: u64) -> CapturedDocument {
    CapturedDocument {
        path: path.to_owned(),
        captured_at: 77,
        status,
        recovered_from: None,
        media: Some(MediaClaim {
            media_type: "video/mp4".into(),
            expected_len,
        }),
        bytes: bytes.to_vec(),
    }
}

/// Copy a ledger entry by entry. `after_raw` adds frames right behind a raw
/// record, where capture writes them; `seal` keeps the generation markers.
fn copy_ledger(
    from: &Path,
    to: &Path,
    seal: bool,
    after_raw: &dyn Fn(u64) -> Vec<CapturedDocument>,
) -> Ledger {
    let mut ledger = Ledger::open(to).unwrap();
    for entry in scan_ledger(from).unwrap().entries {
        match entry {
            LedgerEntry::Meta {
                schema_version,
                payload,
            } => ledger.append_meta(schema_version, &payload).unwrap(),
            LedgerEntry::Raw {
                source,
                generation,
                start_offset,
                payload,
            } => {
                ledger
                    .append_raw(&source, generation, [(start_offset, payload.as_slice())])
                    .unwrap();
                for document in after_raw(start_offset) {
                    ledger
                        .append_document(&source, generation, start_offset, document)
                        .unwrap();
                }
            }
            LedgerEntry::Document {
                source,
                generation,
                start_offset,
                document,
            } => ledger
                .append_document(&source, generation, start_offset, document)
                .unwrap(),
            LedgerEntry::GenerationComplete {
                source,
                generation,
                normalizer_version,
                normalized_clean,
            } => {
                if seal {
                    ledger
                        .complete_generation(
                            &source,
                            generation,
                            normalizer_version,
                            normalized_clean,
                        )
                        .unwrap();
                }
            }
        }
    }
    ledger.flush().unwrap();
    ledger
}

#[path = "presented_media/append_growth.rs"]
mod append_growth;

#[path = "presented_media/replay.rs"]
mod replay;

#[path = "presented_media/reread.rs"]
mod reread;

#[path = "presented_media/snapshot.rs"]
mod snapshot;
