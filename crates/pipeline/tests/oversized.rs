//! A record over the projection limit on the live path. Appended to an active
//! generation, or written into a file that appears after publication, it is
//! ledgered whole and projects one drift fact — exactly what a cold build and
//! a rebuild from the ledger project — so the ledger and the projection agree,
//! and a new file holding one activates instead of being re-read from byte
//! zero on every later append.

use std::io::Write;
use std::path::PathBuf;

use rosetta_adapter_claude_code::ClaudeCodeAdapter;
use rosetta_adapter_support::AgentAdapter;
use rosetta_capture::{Ledger, LedgerEntry, scan_ledger};
use rosetta_core::ids;
use rosetta_core::rows::{RuntimeRow, StreamState};
use rosetta_pipeline::{
    IngestOptions, Instrument, MAX_PROJECTED_RECORD_BYTES, SessionFile, StreamReport,
    meta_ingest_into, rebuild_runtime, stream_backfill_into,
};
use rosetta_store::{Store, queries};
use tempfile::TempDir;

const TRANSCRIPT: &str = "projects/-w-live/s-live.jsonl";

fn runtime() -> RuntimeRow {
    let adapter = &ClaudeCodeAdapter;
    RuntimeRow {
        id: ids::runtime_id(adapter.kind(), &adapter.layout().data_root),
        kind: adapter.kind().to_owned(),
        data_root: adapter.layout().data_root.clone(),
        enabled: true,
        account: None,
        config: None,
        skills: None,
        versions_seen: None,
        discovered_at: 1,
        label: None,
        provenance: Default::default(),
    }
}

fn live_opts() -> IngestOptions {
    IngestOptions {
        audit_files_per_run: 0,
        ..IngestOptions::default()
    }
}

fn line(uuid: &str, content: &str) -> String {
    serde_json::json!({
        "parentUuid": null,
        "isSidechain": false,
        "type": "user",
        "message": {"role": "user", "content": content},
        "uuid": uuid,
        "timestamp": "2026-09-28T10:00:00.000Z",
        "userType": "external",
        "cwd": "/w/live",
        "sessionId": "s-live",
        "version": "2.1.241",
        "gitBranch": "main"
    })
    .to_string()
        + "\n"
}

fn huge() -> String {
    "q".repeat(MAX_PROJECTED_RECORD_BYTES + 1024)
}

/// One live runtime: a Claude root, its store and its raw ledger.
struct Live {
    root: TempDir,
    ledger_dir: TempDir,
    ledger: Ledger,
    store: Store,
    files: Vec<SessionFile>,
}

impl Live {
    fn start(lines: &[String]) -> Self {
        let root = TempDir::new().unwrap();
        let path = root.path().join(TRANSCRIPT);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, lines.concat()).unwrap();
        let ledger_dir = TempDir::new().unwrap();
        let mut ledger = Ledger::open(ledger_dir.path()).unwrap();
        let mut store = Store::open_in_memory().unwrap();
        let (_, files) = meta_ingest_into(
            &ClaudeCodeAdapter,
            &runtime(),
            root.path(),
            &mut store,
            &live_opts(),
            &Instrument::new(),
            Some(&mut ledger),
        )
        .unwrap();
        assert_eq!(files.len(), 1);
        Self {
            root,
            ledger_dir,
            ledger,
            store,
            files,
        }
    }

    fn transcript(&self) -> PathBuf {
        self.root.path().join(TRANSCRIPT)
    }

    fn session(&self) -> String {
        self.files[0].session_id.clone()
    }

    /// One ordinary live pass, as the live runner makes it.
    fn pass(&mut self) -> StreamReport {
        stream_backfill_into(
            &ClaudeCodeAdapter,
            &runtime(),
            self.root.path(),
            &mut self.store,
            &self.files,
            &live_opts(),
            &Instrument::new(),
            Some(&mut self.ledger),
        )
        .unwrap()
    }

    /// Append `lines`; returns the offset they start at.
    fn append(&self, lines: &[String]) -> u64 {
        let path = self.transcript();
        let offset = std::fs::metadata(&path).unwrap().len();
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        file.write_all(lines.concat().as_bytes()).unwrap();
        offset
    }

    fn cursor(&self) -> (u64, u64) {
        let cursor = queries::cursor_state(self.store.conn(), &runtime().id, TRANSCRIPT)
            .unwrap()
            .unwrap();
        (cursor.active_generation, cursor.highest_generation)
    }

    /// The raw ledger's `(generation, offset, payload length)` of the
    /// transcript's records, flushed first.
    fn ledgered(&mut self) -> Vec<(u64, u64, usize)> {
        self.ledger.flush().unwrap();
        scan_ledger(self.ledger_dir.path())
            .unwrap()
            .entries
            .into_iter()
            .filter_map(|entry| match entry {
                LedgerEntry::Raw {
                    source,
                    generation,
                    start_offset,
                    payload,
                } if source.relative_path == TRANSCRIPT => {
                    Some((generation, start_offset, payload.len()))
                }
                _ => None,
            })
            .collect()
    }

    /// Rebuild a projection from the raw ledger alone, and assert it is the
    /// live one: timeline, drift facts, stream state and usage.
    fn assert_rebuild_agrees(&mut self) {
        self.ledger.flush().unwrap();
        let mut rebuilt = Store::open_in_memory().unwrap();
        rebuild_runtime(
            &ClaudeCodeAdapter,
            &runtime(),
            self.ledger_dir.path(),
            &mut rebuilt,
            &Instrument::new(),
        )
        .unwrap();
        let session = self.session();
        assert_eq!(
            timeline(&rebuilt, &session),
            timeline(&self.store, &session)
        );
        assert_eq!(drift(&rebuilt), drift(&self.store));
        assert_eq!(state(&rebuilt, &session), state(&self.store, &session));
        assert_eq!(buckets(&rebuilt), buckets(&self.store));
    }
}

fn timeline(store: &Store, session: &str) -> Vec<rosetta_core::TimelineEntry> {
    queries::timeline_page(store.conn(), session, None, 10_000).unwrap()
}

fn state(store: &Store, session: &str) -> StreamState {
    queries::session(store.conn(), session)
        .unwrap()
        .unwrap()
        .stream_state
}

type DriftRow = (String, String, Option<String>, Option<i64>, i64);

/// Every drift aggregate, its sample cursor as the source path.
fn drift(store: &Store) -> Vec<DriftRow> {
    let mut statement = store
        .conn()
        .prepare(
            "SELECT error_code, record_kind,
                    (SELECT path FROM ingest_cursor WHERE id = sample_cursor),
                    sample_offset, count
             FROM drift_agg ORDER BY error_code, record_kind",
        )
        .unwrap();
    statement
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

fn buckets(store: &Store) -> Vec<(i64, String, i64)> {
    let mut statement = store
        .conn()
        .prepare("SELECT hour_ts, model, message_count FROM usage_bucket ORDER BY hour_ts, model")
        .unwrap();
    statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

/// The one drift fact a record over the limit projects, at `offset`.
fn quarantine_at(offset: u64) -> DriftRow {
    (
        "over_projection_limit".to_string(),
        "raw_record".to_string(),
        Some(TRANSCRIPT.to_string()),
        Some(offset as i64),
        1,
    )
}

#[test]
fn a_live_append_over_the_projection_limit_is_ledgered_and_rebuilds_the_same() {
    let mut live = Live::start(&[line("a-1", "one"), line("a-2", "two")]);
    let first = live.pass();
    assert_eq!(first.records, 2);
    let session = live.session();
    assert_eq!(state(&live.store, &session), StreamState::Indexed);

    let at = live.append(&[line("a-big", &huge()), line("a-3", "three")]);
    let growth = live.pass();
    // Quarantined, oversized, hard errors, restarted, files incomplete.
    assert_eq!(
        (
            growth.quarantined,
            growth.oversized,
            growth.hard_errors,
            growth.generations_restarted,
            growth.files_incomplete
        ),
        (1, 0, 0, 0, 0)
    );
    assert_eq!(growth.records, 2, "only the growth is read");
    assert_eq!(live.cursor(), (1, 1));
    assert_eq!(
        state(&live.store, &session),
        StreamState::Indexed,
        "a quarantined record leaves the generation clean"
    );
    assert!(drift(&live.store).contains(&quarantine_at(at)));
    assert_eq!(timeline(&live.store, &session).len(), 3);

    // The record is in the raw ledger, whole, in the active generation.
    let ledgered = live.ledgered();
    assert_eq!(ledgered.len(), 4);
    assert!(ledgered.contains(&(1, at, MAX_PROJECTED_RECORD_BYTES + 1024 + huge_overhead())));
    live.assert_rebuild_agrees();
}

#[test]
fn a_new_file_over_the_projection_limit_activates_and_appends_read_only_the_growth() {
    let first_lines = [
        line("b-1", "one"),
        line("b-big", &huge()),
        line("b-2", "two"),
    ];
    let at = first_lines[0].len() as u64;
    let mut live = Live::start(&first_lines);
    let first = live.pass();
    assert_eq!(
        (
            first.records,
            first.quarantined,
            first.oversized,
            first.files_incomplete
        ),
        (3, 1, 0, 0)
    );
    assert_eq!(live.cursor(), (1, 1), "the first generation activates");
    let session = live.session();
    assert_eq!(state(&live.store, &session), StreamState::Indexed);
    assert!(drift(&live.store).contains(&quarantine_at(at)));

    // A later append resumes the active generation's checkpoint: only the
    // new record is read, never the file from byte zero again.
    live.append(&[line("b-3", "three")]);
    let growth = live.pass();
    assert_eq!(
        (
            growth.records,
            growth.generations_restarted,
            growth.quarantined
        ),
        (1, 0, 0)
    );
    assert_eq!(live.cursor(), (1, 1));
    assert_eq!(timeline(&live.store, &session).len(), 3);
    assert_eq!(live.ledgered().len(), 4);
    live.assert_rebuild_agrees();
}

/// The bytes a huge record's JSON line carries beyond its content.
fn huge_overhead() -> usize {
    line("a-big", "").len() - 1
}
