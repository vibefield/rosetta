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
use rosetta_capture::{Ledger, LedgerEntry, SegmentCodec, TailConfig, scan_ledger};
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
    let mut emit = |sources: Vec<ClosedSource>| {
        emitted.extend(sources.iter().map(|source| format!("{source:?}")));
        Ok(())
    };
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
            &mut emit,
        )
    } else {
        walk_closed_sources(
            adapter,
            &runtime,
            &discovery.root,
            &mut planner,
            &path_ordered(&files),
            opts,
            &instrument,
            Some(&mut ledger),
            None,
            &mut emit,
        )
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
