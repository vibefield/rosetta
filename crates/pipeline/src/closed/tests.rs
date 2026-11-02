//! Eager roster cursors and roster-keyed ledger ids against the lazy creation
//! and dense ids they replaced, on every recorded fixture of every adapter
//! (between them all three stream drivers, and rosters that mix them) and on
//! a home whose session restarts mid-capture. Against a lazy walk
//! with the same keys installed, everything is identical down to the ledger
//! bytes; against the dense ledger, the decoded entries are.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use rosetta_adapter_antigravity::AntigravityAdapter;
use rosetta_adapter_claude_code::ClaudeCodeAdapter;
use rosetta_adapter_codex::CodexAdapter;
use rosetta_adapter_copilot::CopilotAdapter;
use rosetta_adapter_grok::GrokAdapter;
use rosetta_adapter_hermes::HermesAdapter;
use rosetta_adapter_kimi_code::KimiCodeAdapter;
use rosetta_adapter_opencode::OpenCodeAdapter;
use rosetta_adapter_pi::PiAdapter;
use rosetta_adapter_support::StreamDriver;
use rosetta_capture::{Ledger, LedgerEntry, SegmentCodec, TailConfig, Tailer, scan_ledger};
use rusqlite::types::Value;
use tempfile::TempDir;

use super::*;
use crate::progress::ColdProgressObserver;
use crate::{discover, expand_data_root, meta_ingest_inventory_into};

pub(crate) fn adapters() -> Vec<&'static dyn AgentAdapter> {
    vec![
        &AntigravityAdapter,
        &ClaudeCodeAdapter,
        &CodexAdapter,
        &CopilotAdapter,
        &GrokAdapter,
        &HermesAdapter,
        &KimiCodeAdapter,
        &OpenCodeAdapter,
        &PiAdapter,
    ]
}

pub(crate) fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    let mut entries = std::fs::read_dir(from)
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let source = entry.path();
        let destination = to.join(entry.file_name());
        if source.is_dir() {
            copy_tree(&source, &destination);
        } else {
            std::fs::copy(source, destination).unwrap();
        }
    }
}

/// Every recorded fixture scenario of one adapter: `(name, data root)`.
pub(crate) fn fixture_roots(kind: &str) -> Vec<(String, PathBuf)> {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../groundtruth")
        .join(kind)
        .join("fixtures");
    let mut roots = Vec::new();
    for version in std::fs::read_dir(&fixtures).unwrap() {
        let version = version.unwrap().path();
        for scenario in std::fs::read_dir(&version).unwrap() {
            let scenario = scenario.unwrap().path();
            let data = std::fs::read_dir(&scenario)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .find(|path| path.is_dir())
                .unwrap_or_else(|| panic!("{} has no data root", scenario.display()));
            roots.push((
                scenario.file_name().unwrap().to_string_lossy().into_owned(),
                data,
            ));
        }
    }
    roots.sort();
    roots
}

/// Everything one capture leaves behind that eager creation could change.
#[derive(Debug, PartialEq)]
struct Captured {
    /// Every planner cursor column, in id order.
    cursors: Vec<Vec<Value>>,
    /// Each closed source as emitted, in emission order.
    emitted: Vec<String>,
    report: Vec<u64>,
    /// Every ledger file's bytes, in name order.
    ledger: Vec<(String, Vec<u8>)>,
    /// The decoded ledger, in scan order.
    entries: Vec<LedgerEntry>,
}

#[derive(Clone, Copy, PartialEq)]
enum Walk {
    /// Cursors at each source's turn, dense ledger ids.
    Lazy,
    /// Cursors at each source's turn, roster keys installed by hand.
    LazyKeyed,
    /// [`stream_closed_sources_into`] itself.
    Eager,
}

/// Discover, plan metadata and set the stream up — fresh serve's steps
/// before capture — then capture one way.
fn capture(
    adapter: &'static dyn AgentAdapter,
    home: &Path,
    opts: &IngestOptions,
    walk: Walk,
) -> Captured {
    let instrument = Instrument::new();
    let discovery = discover(&[adapter], home, opts, &instrument)
        .unwrap()
        .pop()
        .unwrap();
    let mut runtime = discovery.runtime;
    runtime.enabled = true;
    let mut planner = Store::open_in_memory_for_cold_planning().unwrap();
    let (_, files) = meta_ingest_inventory_into(
        adapter,
        &runtime,
        &discovery.root,
        &mut planner,
        &discovery.inventory,
        discovery.walk_truncated,
        opts,
        &instrument,
        None,
    )
    .unwrap();
    streamed_setup(&files, &mut planner).unwrap();

    let ledger_dir = TempDir::new().unwrap();
    let mut ledger = Ledger::open(ledger_dir.path())
        .unwrap()
        .with_codec(SegmentCodec::Plain)
        .with_max_segment_bytes(4 * 1024);
    let mut emitted = Vec::new();
    let mut emit = BlockingEmit(|_, sources: Vec<ClosedSource>| {
        emitted.extend(sources.iter().map(|source| format!("{source:?}")));
        Ok(())
    });
    if walk == Walk::LazyKeyed {
        let keys = roster_keys(&path_ordered(&files)).unwrap();
        ledger.set_source_keys(Arc::new(keys)).unwrap();
    }
    let report = if walk == Walk::Eager {
        stream_closed_sources_into(
            adapter,
            &runtime,
            &discovery.root,
            &mut planner,
            &files,
            opts,
            &instrument,
            Some(&mut ledger),
            None,
            &ColdWalkOptions::default(),
            &mut emit,
        )
    } else {
        let ordered = path_ordered(&files);
        super::walk::Walker::new(
            adapter,
            &runtime,
            &discovery.root,
            &mut planner,
            &ordered,
            opts,
            &instrument,
        )
        .and_then(|walker| walker.walk_path(&ordered, Some(&mut ledger), None, &mut emit))
    }
    .unwrap();
    ledger.flush().unwrap();
    drop(ledger);

    let cursors = planner
        .conn()
        .prepare("SELECT * FROM ingest_cursor ORDER BY id")
        .unwrap()
        .query_map([], |row| {
            (0..row.as_ref().column_count())
                .map(|index| row.get::<_, Value>(index))
                .collect::<Result<Vec<_>, _>>()
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let mut ledger = std::fs::read_dir(ledger_dir.path())
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            (
                path.file_name().unwrap().to_string_lossy().into_owned(),
                std::fs::read(&path).unwrap(),
            )
        })
        .collect::<Vec<_>>();
    ledger.sort();
    let entries = scan_ledger(ledger_dir.path()).unwrap().entries;
    Captured {
        cursors,
        emitted,
        report: vec![
            report.files as u64,
            report.files_missing as u64,
            report.batches,
            report.records,
            report.payload_bytes,
            report.oversized,
            report.parsed,
            report.unknown,
            report.hard_errors,
            report.generations_restarted,
            report.files_audited,
            report.conflicts,
            report.retries_exhausted,
            report.files_incomplete,
            report.files_paused,
            report.sessions_indexed,
            report.siblings_healed,
        ],
        ledger,
        entries,
    }
}

/// Eager equals the keyed lazy walk exactly, and the dense lazy walk in
/// everything but the ledger's file ids. Returns the eager capture and
/// whether those ids changed any ledger byte.
fn assert_equivalent(label: &str, run: impl Fn(Walk) -> Captured) -> (Captured, bool) {
    let dense = run(Walk::Lazy);
    let keyed = run(Walk::LazyKeyed);
    let eager = run(Walk::Eager);
    assert_eq!(eager, keyed, "{label}");
    assert_eq!(eager.cursors, dense.cursors, "{label}");
    assert_eq!(eager.emitted, dense.emitted, "{label}");
    assert_eq!(eager.report, dense.report, "{label}");
    assert_eq!(eager.entries, dense.entries, "{label}");
    assert_path_numbered(label, &eager);
    let rekeyed = eager.ledger != dense.ledger;
    (eager, rekeyed)
}

/// The cursors of a path-ordered roster are numbered 1, 2, … in path order.
fn assert_path_numbered(label: &str, captured: &Captured) {
    let mut paths = Vec::new();
    for (index, row) in captured.cursors.iter().enumerate() {
        assert_eq!(row[0], Value::Integer(index as i64 + 1), "{label}");
        let Value::Text(path) = &row[3] else {
            panic!("{label}: cursor path is not text: {row:?}");
        };
        paths.push(path.clone());
    }
    let mut sorted = paths.clone();
    sorted.sort();
    assert_eq!(paths, sorted, "{label}: cursor ids follow path order");
}

#[test]
fn eager_cursors_and_roster_keys_match_the_lazy_dense_walk_on_every_fixture() {
    let mut scenarios = 0;
    let mut rekeyed = 0;
    for adapter in adapters() {
        for (scenario, data) in fixture_roots(adapter.kind()) {
            let label = format!("{} {scenario}", adapter.kind());
            let home = TempDir::new().unwrap();
            copy_tree(
                &data,
                &expand_data_root(&adapter.layout().data_root, home.path()),
            );
            let opts = IngestOptions::default();
            let (_, keys_changed_bytes) =
                assert_equivalent(&label, |walk| capture(adapter, home.path(), &opts, walk));
            scenarios += 1;
            rekeyed += usize::from(keys_changed_bytes);
        }
    }
    assert!(scenarios >= 9, "only {scenarios} fixture scenarios");
    // Small segments rotate, where dense ids restart and keys do not.
    assert!(rekeyed > 0, "no scenario's ledger bytes depend on its ids");
}

/// Rewrites one source the first time capture reports bytes for it — after
/// its first batch was framed and before the next read — so the tailer sees
/// a changed anchor and restarts the generation mid-pass.
#[derive(Debug)]
pub(crate) struct RewriteOnce {
    pub(crate) root: PathBuf,
    pub(crate) target: String,
    pub(crate) replacement: String,
    pub(crate) done: AtomicBool,
}

impl ColdProgressObserver for RewriteOnce {
    fn source_bytes(&self, file: &SessionFile, _bytes: u64) {
        if file.relative_path == self.target && !self.done.swap(true, Ordering::SeqCst) {
            std::fs::write(self.root.join(&self.target), &self.replacement).unwrap();
        }
    }

    fn reading_finished(&self) {}
}

pub(crate) fn claude_user(uuid: &str, content: &str) -> String {
    serde_json::json!({
        "parentUuid": null,
        "isSidechain": false,
        "type": "user",
        "message": {"role": "user", "content": content},
        "uuid": uuid,
        "timestamp": "2026-08-25T02:35:19.054Z",
        "userType": "external",
        "cwd": "/w/heal",
        "sessionId": "heal-session",
        "version": "2.1.241",
        "gitBranch": "main"
    })
    .to_string()
}

pub(crate) const HEAL_TRANSCRIPT: &str = "projects/-w-heal/heal-session.jsonl";
pub(crate) const HEAL_JOURNAL: &str =
    "projects/-w-heal/heal-session/subagents/workflows/wf_heal/journal.jsonl";

/// A two-transcript session (transcript + workflow journal) whose transcript
/// is rewritten between its first and second capture batch, so the session
/// restarts and its sibling heals before either source is emitted.
#[test]
fn eager_cursors_and_roster_keys_match_the_lazy_dense_walk_across_a_restart_and_heal() {
    let home = TempDir::new().unwrap();
    let root = expand_data_root(&ClaudeCodeAdapter.layout().data_root, home.path());
    let lines = |prefix: &str| {
        (1..=5)
            .map(|index| {
                claude_user(
                    &format!("{prefix}-{index}"),
                    &format!("{prefix} line {index}"),
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
            + "\n"
    };
    let journal = root.join(HEAL_JOURNAL);
    std::fs::create_dir_all(journal.parent().unwrap()).unwrap();
    std::fs::write(
        &journal,
        [
            r#"{"type":"started","key":"worker-1","agentId":"agent-1"}"#,
            r#"{"type":"result","key":"worker-1","result":{"ok":true}}"#,
        ]
        .join("\n")
            + "\n",
    )
    .unwrap();
    let transcript = root.join(HEAL_TRANSCRIPT);
    std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    // One home for both runs; each run starts from the original transcript,
    // with the modification time discovery reads pinned, and rewrites it once.
    let run = |walk: Walk| {
        std::fs::write(&transcript, lines("old")).unwrap();
        std::fs::File::options()
            .write(true)
            .open(&transcript)
            .unwrap()
            .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_790_000_000))
            .unwrap();
        let mut tail = TailConfig::json_lines();
        tail.max_records_per_batch = 2;
        let opts = IngestOptions {
            tail,
            progress: Some(Arc::new(RewriteOnce {
                root: root.clone(),
                target: HEAL_TRANSCRIPT.into(),
                replacement: lines("new"),
                done: AtomicBool::new(false),
            })),
            ..IngestOptions::default()
        };
        capture(&ClaudeCodeAdapter, home.path(), &opts, walk)
    };
    let (eager, _) = assert_equivalent("restart", run);
    // The rewrite restarts the transcript; the heal replays its sibling into
    // a replacement generation too.
    let restarted = eager.report[9];
    let healed = eager.report[16];
    assert_eq!((restarted, healed), (2, 1), "{:?}", eager.report);
}

/// Closing a source tolerates exactly one thing its pass could not produce —
/// a generation, for a source it could not read at all — and still refuses
/// everything else that only a bug leaves behind.
#[test]
fn close_file_skips_an_unread_source_and_refuses_what_only_a_bug_leaves() {
    let file = SessionFile {
        ledger_only: false,
        relative_path: "projects/-w/close.jsonl".into(),
        stream_id: "transcript".into(),
        file_format: rosetta_adapter_support::FileFormat::Jsonl,
        discipline: rosetta_adapter_support::FileDiscipline::AppendOnly,
        driver: StreamDriver::JsonLines,
        session_id: "session".into(),
        native_session_id: "close".into(),
        size: 0,
        modified_ms: 0,
        agent_version: None,
        source_missing: false,
        tree_parent: None,
        cut_len: None,
        cut_modified_ms: None,
    };
    let cursor = |active: u64, highest: u64, checkpoint: &str| queries::CursorState {
        id: 7,
        checkpoint: checkpoint.into(),
        active_generation: active,
        highest_generation: highest,
        session_id: Some("session".into()),
    };
    let batch = |ops: usize, first_sequence: u64, mode: SourceMode| Batch {
        ops: (0..ops)
            .map(|index| {
                IngestOp::Drift(rosetta_core::rows::DriftFact {
                    runtime_id: "runtime".into(),
                    agent_version: String::new(),
                    stream: "transcript".into(),
                    record_kind: "raw_record".into(),
                    error_code: "test".into(),
                    shape_hash: format!("{first_sequence}-{index}"),
                    at: None,
                    sample_cursor: Some(7),
                    sample_offset: None,
                })
            })
            .collect(),
        checkpoints: Vec::new(),
        source: Some(rosetta_store::SourceWrite {
            cursor_id: 7,
            session_id: "session".into(),
            generation: 1,
            first_sequence,
            mode,
        }),
    };
    let close = |cursor, batches, lost| {
        close_file(&ClaudeCodeAdapter, &file, cursor, batches, 0, false, lost)
            .map_err(|error| error.to_string())
    };

    let skipped = close(cursor(0, 0, "fresh"), Vec::new(), true).unwrap();
    assert!(skipped.skipped() && !skipped.complete && skipped.ops.is_empty());
    for (label, closed, refusal) in [
        (
            "unread, yet not lost",
            close(cursor(0, 0, "fresh"), Vec::new(), false),
            "never started a generation",
        ),
        (
            "lost, yet holding batches",
            close(
                cursor(0, 0, "fresh"),
                vec![batch(1, 0, SourceMode::Activate)],
                true,
            ),
            "never started a generation",
        ),
        (
            "a gap in the operation sequence",
            close(
                cursor(1, 1, "fresh"),
                vec![
                    batch(1, 0, SourceMode::Stage),
                    batch(1, 2, SourceMode::Activate),
                ],
                false,
            ),
            "non-contiguous operation sequence",
        ),
        (
            "a staged generation",
            close(
                cursor(0, 1, "fresh"),
                vec![batch(1, 0, SourceMode::Stage)],
                true,
            ),
            "stopped without a terminal generation",
        ),
        (
            "an undecodable final checkpoint",
            close(
                cursor(1, 1, "{not a checkpoint"),
                vec![batch(1, 0, SourceMode::Activate)],
                false,
            ),
            "undecodable checkpoint",
        ),
    ] {
        let error = closed.expect_err(label);
        assert!(error.contains(refusal), "{label}: {error}");
    }
}

/// A frontier that never let a source go names it, and its lane.
#[test]
fn a_frontier_that_never_closed_names_its_lane() {
    let file = |path: &str| SessionFile {
        ledger_only: false,
        relative_path: path.into(),
        stream_id: "transcript".into(),
        file_format: rosetta_adapter_support::FileFormat::Jsonl,
        discipline: rosetta_adapter_support::FileDiscipline::AppendOnly,
        driver: StreamDriver::JsonLines,
        session_id: path.into(),
        native_session_id: path.into(),
        size: 0,
        modified_ms: 0,
        agent_version: None,
        source_missing: false,
        tree_parent: None,
        cut_len: None,
        cut_modified_ms: None,
    };
    let (first, second) = (file("p/b.jsonl"), file("p/a.jsonl"));
    let lane_order = [&first, &second];
    for (span, expected) in [
        (
            LaneId::Small,
            "cold source p/b.jsonl in lane S never closed",
        ),
        (
            LaneId::Big(0),
            "cold source p/b.jsonl in lane B0 never closed",
        ),
        (LaneId::Path, "cold source p/b.jsonl never closed"),
    ] {
        let frontier = super::frontier::Frontier::new(span, &lane_order);
        assert!(!frontier.finished());
        let error = frontier.check_finished().unwrap_err().to_string();
        assert!(error.ends_with(expected), "{error}");
    }
}

/// Closing a unit drains only its own cursor's retained batches and drops
/// the roster's source-less setup batch; what another cursor still retains
/// must be an open lane head's, or the planner broke an invariant.
#[test]
fn a_unit_drains_its_own_cursor_and_refuses_unknown_retained_ones() {
    let home = TempDir::new().unwrap();
    let root = expand_data_root(&ClaudeCodeAdapter.layout().data_root, home.path());
    let project = root.join("projects/-w-drain");
    std::fs::create_dir_all(&project).unwrap();
    for session in ["drain-a", "drain-b"] {
        let body: String = (0..6)
            .map(|index| claude_user(&format!("{session}-{index}"), "hello") + "\n")
            .collect();
        std::fs::write(project.join(format!("{session}.jsonl")), body).unwrap();
    }
    let mut tail = TailConfig::json_lines();
    tail.max_records_per_batch = 2;
    let opts = IngestOptions {
        tail,
        private_build: true,
        ..IngestOptions::default()
    };
    let capture = || {
        let instrument = Instrument::new();
        let adapter: &dyn AgentAdapter = &ClaudeCodeAdapter;
        let discovery = discover(&[adapter], home.path(), &opts, &instrument)
            .unwrap()
            .pop()
            .unwrap();
        let mut runtime = discovery.runtime;
        runtime.enabled = true;
        let mut planner = Store::open_in_memory_for_cold_planning().unwrap();
        let (_, files) = meta_ingest_inventory_into(
            adapter,
            &runtime,
            &discovery.root,
            &mut planner,
            &discovery.inventory,
            discovery.walk_truncated,
            &opts,
            &instrument,
            None,
        )
        .unwrap();
        streamed_setup(&files, &mut planner).unwrap();
        let ordered = path_ordered(&files);
        set_up_roster(adapter, &runtime, &mut planner, &ordered, &instrument, None).unwrap();
        let tailer = Tailer::new(opts.tail.clone()).unwrap();
        let mut report = StreamReport::default();
        for file in &ordered {
            crate::stream::ingest_file(
                adapter,
                &runtime,
                &discovery.root,
                &mut planner,
                file,
                &tailer,
                &opts,
                &instrument,
                &mut report,
                crate::stream::PassMode::Normal,
                None,
            )
            .unwrap();
        }
        let ids: Vec<i64> = ordered
            .iter()
            .map(|file| {
                queries::cursor_state(planner.conn(), &runtime.id, &file.relative_path)
                    .unwrap()
                    .unwrap()
                    .id
            })
            .collect();
        let files: Vec<SessionFile> = ordered.into_iter().cloned().collect();
        (runtime, planner, files, ids, instrument)
    };

    // (a) One cursor's batches, in order; the other's stay.
    let (_, mut planner, _, ids, _) = capture();
    assert_eq!(
        planner.planned_batch_cursors().unwrap(),
        BTreeSet::from([ids[0], ids[1]])
    );
    let drained = planner.drain_planned_batches_for(ids[0]).unwrap();
    assert!(drained.len() > 1, "{} batches", drained.len());
    let mut next = 0;
    for batch in &drained {
        let source = batch.source.as_ref().unwrap();
        assert_eq!(source.cursor_id, ids[0]);
        assert_eq!(source.first_sequence, next);
        next += batch.ops.len() as u64;
    }
    assert_eq!(
        planner.planned_batch_cursors().unwrap(),
        BTreeSet::from([ids[1]])
    );
    // (b) The roster's source-less setup batch went with the first drain.
    let rest = planner.drain_planned_batches().unwrap();
    assert!(!rest.is_empty());
    assert!(
        rest.iter()
            .all(|batch| batch.source.as_ref().map(|source| source.cursor_id) == Some(ids[1])),
        "only the other cursor's batches remain"
    );

    // A group's drain: every member's batches in one pass, each in order,
    // at its member's position; the setup batch goes with it.
    let (_, mut planner, _, ids, _) = capture();
    let drained = planner
        .drain_planned_batches_for_cursors(&[ids[1], ids[0]])
        .unwrap();
    assert_eq!(drained.len(), 2);
    for (batches, id) in drained.iter().zip([ids[1], ids[0]]) {
        assert!(batches.len() > 1, "{} batches", batches.len());
        let mut next = 0;
        for batch in batches {
            let source = batch.source.as_ref().unwrap();
            assert_eq!(source.cursor_id, id);
            assert_eq!(source.first_sequence, next);
            next += batch.ops.len() as u64;
        }
    }
    assert!(planner.planned_batch_cursors().unwrap().is_empty());
    assert!(planner.drain_planned_batches().unwrap().is_empty());

    // (c) A retained batch of a cursor that is not an open head.
    let (runtime, mut planner, files, ids, instrument) = capture();
    let health = BTreeMap::from([(files[0].relative_path.clone(), true)]);
    let refused = close_unit(
        &ClaudeCodeAdapter,
        &runtime,
        &mut planner,
        &[&files[0]],
        0,
        &health,
        false,
        &BTreeSet::new(),
        &instrument,
    )
    .unwrap_err()
    .to_string();
    assert!(
        refused.ends_with(&format!("cold planner retained unknown cursor {}", ids[1])),
        "{refused}"
    );
    // The same close is fine while that cursor is an open head.
    let (runtime, mut planner, files, ids, instrument) = capture();
    let closed = close_unit(
        &ClaudeCodeAdapter,
        &runtime,
        &mut planner,
        &[&files[0]],
        0,
        &health,
        false,
        &BTreeSet::from([ids[1]]),
        &instrument,
    )
    .unwrap();
    assert_eq!(closed.len(), 1);
    assert!(!closed[0].skipped());
}
