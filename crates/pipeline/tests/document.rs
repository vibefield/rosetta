//! DocumentSnapshot integration: one rewrite-in-place index can own several
//! virtual sessions, change membership, and disappear entirely without
//! leaving stale projection rows behind.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use rosetta_adapter_support::{
    AgentAdapter, FileStat, GroundTruthDoc, LineOutcome, NormalizeCtx, SkimResult, StreamDriver,
};
use rosetta_capture::ScriptedEvents;
use rosetta_core::rows::{MessageKind, MessageRow, RuntimeRow};
use rosetta_core::{IngestOp, ids};
use rosetta_pipeline::{
    IngestOptions, Instrument, LiveOptions, LiveRunner, discover, expand_data_root,
    meta_ingest_into, stream_backfill_into,
};
use rosetta_store::{Store, queries};
use serde_json::{Value, json};
use tempfile::TempDir;

struct FakeDocumentAgent;

impl AgentAdapter for FakeDocumentAgent {
    fn kind(&self) -> &'static str {
        "fakedoc"
    }

    fn layout(&self) -> &GroundTruthDoc {
        static LAYOUT: OnceLock<GroundTruthDoc> = OnceLock::new();
        LAYOUT.get_or_init(|| {
            GroundTruthDoc::parse(
                r#"{"agent":"fakedoc","data_root":"~/.fakedoc","streams":[
                    {"id":"index","glob":"index.json","file_format":"json",
                     "discipline":"rewrite_in_place","evidence":"fixture"}]}"#,
            )
            .unwrap()
        })
    }

    fn stream_driver(&self, stream_id: &str) -> Option<StreamDriver> {
        (stream_id == "index").then_some(StreamDriver::DocumentSnapshot)
    }

    fn normalizer_version(&self) -> u32 {
        1
    }

    fn skim(&self, _stream_id: &str, _stat: &FileStat, _head: &[u8], _tail: &[u8]) -> SkimResult {
        SkimResult::default()
    }

    fn document_sessions(
        &self,
        stream_id: &str,
        _stat: &FileStat,
        document: &[u8],
    ) -> Vec<SkimResult> {
        if stream_id != "index" {
            return Vec::new();
        }
        let Ok(value) = serde_json::from_slice::<Value>(document) else {
            return Vec::new();
        };
        value["sessions"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|session| {
                Some(SkimResult {
                    session_id: Some(session["id"].as_str()?.to_owned()),
                    cwd: session["cwd"].as_str().map(str::to_owned),
                    title: session["title"].as_str().map(str::to_owned),
                    started_at: session["started_at"].as_i64(),
                    ..SkimResult::default()
                })
            })
            .collect()
    }

    fn document_records(
        &self,
        stream_id: &str,
        native_session_id: &str,
        document: &[u8],
    ) -> Vec<Vec<u8>> {
        if stream_id != "index" {
            return Vec::new();
        }
        let Ok(value) = serde_json::from_slice::<Value>(document) else {
            return Vec::new();
        };
        value["sessions"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|session| session["id"].as_str() == Some(native_session_id))
            .and_then(|session| session["messages"].as_array())
            .into_iter()
            .flatten()
            .filter_map(|message| serde_json::to_vec(message).ok())
            .collect()
    }

    fn ingest_line(&self, ctx: &mut NormalizeCtx, _stream_id: &str, line: &[u8]) -> LineOutcome {
        let Ok(value) = serde_json::from_slice::<Value>(line) else {
            return LineOutcome::parsed(Vec::new());
        };
        let Some(native_id) = value["id"].as_str() else {
            return LineOutcome::parsed(Vec::new());
        };
        let message = MessageRow {
            timeline_id: ids::timeline_id(&ctx.session_id, native_id),
            session_id: ctx.session_id.clone(),
            ordinal: ctx.take_ordinal(),
            kind: if value["role"] == "user" {
                MessageKind::User
            } else {
                MessageKind::Agent
            },
            subtype: None,
            uuid: Some(native_id.to_owned()),
            parent_uuid: None,
            ts: value["at"].as_i64(),
            model: None,
            effort: None,
            content: value["body"].as_str().map(str::to_owned),
            tool: None,
            attachments: Vec::new(),
            citations: Vec::new(),
            group: None,
            response: None,
            block_index: None,
            branch_tool_id: None,
            usage: None,
            agent_version: None,
            src_cursor: ctx.src_cursor,
            src_offset: ctx.src_offset,
        };
        LineOutcome::parsed(IngestOp::project_message(message))
    }
}

struct Harness {
    _home: TempDir,
    root: PathBuf,
    document: PathBuf,
    runtime: RuntimeRow,
    store: Store,
    opts: IngestOptions,
    instrument: Instrument,
}

fn write_document(path: &Path, sessions: Value) {
    std::fs::write(
        path,
        serde_json::to_vec(&json!({ "sessions": sessions })).unwrap(),
    )
    .unwrap();
}

fn initial_sessions() -> Value {
    json!([
        {
            "id": "a", "title": "Alpha", "cwd": "/work/alpha", "started_at": 1000,
            "messages": [
                {"id": "a1", "role": "user", "body": "quetzalcoatl alpha", "at": 1000},
                {"id": "a2", "role": "agent", "body": "alpha reply", "at": 2000}
            ]
        },
        {
            "id": "b", "title": "Beta", "cwd": "/work/beta", "started_at": 3000,
            "messages": [{"id": "b1", "role": "user", "body": "xolotl beta", "at": 3000}]
        }
    ])
}

fn changed_sessions() -> Value {
    json!([
        {
            "id": "a", "title": "Alpha", "cwd": "/work/alpha", "started_at": 1000,
            "messages": [{"id": "a1", "role": "user", "body": "quetzalcoatl corrected", "at": 1000}]
        },
        {
            "id": "c", "title": "Gamma", "cwd": "/work/gamma", "started_at": 4000,
            "messages": [{"id": "c1", "role": "user", "body": "tezcatlipoca gamma", "at": 4000}]
        }
    ])
}

fn harness() -> Harness {
    let adapter = &FakeDocumentAgent;
    let home = TempDir::new().unwrap();
    let root = expand_data_root(&adapter.layout().data_root, home.path());
    std::fs::create_dir_all(&root).unwrap();
    let document = root.join("index.json");
    write_document(&document, initial_sessions());
    let opts = IngestOptions::default();
    let instrument = Instrument::new();
    let discoveries = discover(&[adapter], home.path(), &opts, &instrument).unwrap();
    let mut runtime = discoveries[0].runtime.clone();
    runtime.enabled = true;
    Harness {
        _home: home,
        root,
        document,
        runtime,
        store: Store::open_in_memory().unwrap(),
        opts,
        instrument,
    }
}

fn backfill(h: &mut Harness) {
    let adapter = &FakeDocumentAgent;
    let (_, files) = meta_ingest_into(
        adapter,
        &h.runtime,
        &h.root,
        &mut h.store,
        &h.opts,
        &h.instrument,
        None,
    )
    .unwrap();
    stream_backfill_into(
        adapter,
        &h.runtime,
        &h.root,
        &mut h.store,
        &files,
        &h.opts,
        &h.instrument,
        None,
    )
    .unwrap();
}

#[test]
fn document_membership_and_whole_file_deletion_are_authoritative() {
    let adapter = &FakeDocumentAgent;
    let mut h = harness();
    backfill(&mut h);
    assert_eq!(
        queries::search(h.store.conn(), "quetzalcoatl", 5)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        queries::search(h.store.conn(), "xolotl", 5).unwrap().len(),
        1
    );

    write_document(&h.document, changed_sessions());
    let (_, files) = meta_ingest_into(
        adapter,
        &h.runtime,
        &h.root,
        &mut h.store,
        &h.opts,
        &h.instrument,
        None,
    )
    .unwrap();
    assert_eq!(
        files.len(),
        3,
        "two members plus the removed member tombstone"
    );
    stream_backfill_into(
        adapter,
        &h.runtime,
        &h.root,
        &mut h.store,
        &files,
        &h.opts,
        &h.instrument,
        None,
    )
    .unwrap();
    assert!(
        queries::search(h.store.conn(), "xolotl", 5)
            .unwrap()
            .is_empty()
    );
    assert!(
        queries::search(h.store.conn(), "reply", 5)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        queries::search(h.store.conn(), "corrected", 5)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        queries::search(h.store.conn(), "tezcatlipoca", 5)
            .unwrap()
            .len(),
        1
    );

    std::fs::remove_file(&h.document).unwrap();
    let (_, tombstones) = meta_ingest_into(
        adapter,
        &h.runtime,
        &h.root,
        &mut h.store,
        &h.opts,
        &h.instrument,
        None,
    )
    .unwrap();
    assert_eq!(tombstones.len(), 3);
    assert!(tombstones.iter().all(|file| file.source_missing));
    stream_backfill_into(
        adapter,
        &h.runtime,
        &h.root,
        &mut h.store,
        &tombstones,
        &h.opts,
        &h.instrument,
        None,
    )
    .unwrap();
    let messages: i64 = h
        .store
        .conn()
        .query_row("SELECT COUNT(*) FROM message", [], |row| row.get(0))
        .unwrap();
    assert_eq!(messages, 0);
}

#[test]
fn live_document_hint_reenumerates_and_then_sweeps_a_deleted_file() {
    let adapter = &FakeDocumentAgent;
    let mut h = harness();
    backfill(&mut h);
    let mut events = ScriptedEvents::default();
    events.push_changed("index.json");
    events.push_quiet();
    events.push_changed("index.json");
    let mut runner = LiveRunner::start(
        adapter,
        h.runtime.clone(),
        h.root.clone(),
        Box::new(events),
        &mut h.store,
        h.opts.clone(),
        LiveOptions {
            tick: Duration::ZERO,
            poll_interval: Duration::from_secs(3600),
            max_coalesced: 8,
        },
        &h.instrument,
    )
    .unwrap();

    write_document(&h.document, changed_sessions());
    runner.tick(&mut h.store, &h.instrument).unwrap();
    assert!(
        queries::search(h.store.conn(), "xolotl", 5)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        queries::search(h.store.conn(), "tezcatlipoca", 5)
            .unwrap()
            .len(),
        1
    );

    std::fs::remove_file(&h.document).unwrap();
    runner.tick(&mut h.store, &h.instrument).unwrap();
    let messages: i64 = h
        .store
        .conn()
        .query_row("SELECT COUNT(*) FROM message", [], |row| row.get(0))
        .unwrap();
    assert_eq!(messages, 0);
    assert_eq!(runner.report().admissions, 2);
}
