//! What a fresh build can no longer read, or read cleanly, no longer fails it.
//! A source gone before capture reached it closes skipped — a transcript, or
//! every member of a snapshot database or document that is gone or never
//! reads stably; one gone after part of it was read ends its generation where
//! the file stopped; a sibling gone before its heal keeps the generation it
//! was held with; and an unclean replacement generation activates. Every such
//! session is held Streaming, as a live pass leaves it, and every
//! configuration leaves exactly what the serial walk leaves.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use rosetta_adapter_claude_code::ClaudeCodeAdapter;
use rosetta_adapter_codex::CodexAdapter;
use rosetta_adapter_copilot::CopilotAdapter;
use rosetta_adapter_support::AgentAdapter;
use rosetta_capture::{LedgerEntry, scan_ledger};
use rusqlite::types::Value;
use tempfile::TempDir;

use super::{
    Captured, Config, SERIAL, assert_every_config, assert_restart_and_heal, capture_into,
    capture_into_after, claude_home, heal_options, reset_heal_transcript, session_records,
    stream_state, tiny_batches,
};
use crate::closed::tests::{HEAL_JOURNAL, HEAL_TRANSCRIPT, copy_tree, fixture_roots};
use crate::meta::SessionFile;
use crate::progress::ColdProgressObserver;
use crate::{IngestOptions, expand_data_root};

/// Moves one roster source out of the data root the first time capture
/// reports bytes for it — after a batch was framed, before the next read —
/// so it vanishes as a deleted file does; a move keeps its inode and
/// modification time for the move back.
#[derive(Debug)]
struct VanishOnce {
    path: PathBuf,
    stash: PathBuf,
    done: AtomicBool,
}

impl VanishOnce {
    fn new(path: &Path, stash: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
            stash: stash.to_path_buf(),
            done: AtomicBool::new(false),
        }
    }
}

impl ColdProgressObserver for VanishOnce {
    fn source_bytes(&self, file: &SessionFile, _bytes: u64) {
        if self.path.ends_with(&file.relative_path) && !self.done.swap(true, Ordering::SeqCst) {
            std::fs::rename(&self.path, &self.stash).unwrap();
        }
    }

    fn reading_finished(&self) {}
}

/// Two observers, in order.
#[derive(Debug)]
struct Both(Arc<dyn ColdProgressObserver>, Arc<dyn ColdProgressObserver>);

impl ColdProgressObserver for Both {
    fn source_bytes(&self, file: &SessionFile, bytes: u64) {
        self.0.source_bytes(file, bytes);
        self.1.source_bytes(file, bytes);
    }

    fn reading_finished(&self) {
        self.0.reading_finished();
        self.1.reading_finished();
    }
}

/// Move a stashed source back where it was.
fn restore(path: &Path, stash: &Path) {
    if stash.exists() {
        std::fs::rename(stash, path).unwrap();
    }
}

/// The one closed source emitted for `path`.
fn emitted<'a>(captured: &'a Captured, path: &str) -> &'a str {
    let sources: Vec<_> = captured
        .emitted
        .iter()
        .filter(|source| source.contains(&format!("relative_path: \"{path}\"")))
        .collect();
    assert_eq!(sources.len(), 1, "one terminal emission for {path}");
    sources[0]
}

/// Message rows whose content is exactly `content`.
fn messages_with(captured: &Captured, content: &str) -> usize {
    let field = format!("content=Text(\"{content}\")");
    captured.canonical["message"]
        .iter()
        .filter(|row| row.split(" | ").any(|column| column == field))
        .count()
}

/// Each planner cursor's `(path, active, highest)` generations.
fn generations(captured: &Captured) -> Vec<(String, Value, Value)> {
    captured
        .cursors
        .iter()
        .map(|cursor| {
            let Value::Text(path) = &cursor[3] else {
                panic!("cursor path is not text: {cursor:?}");
            };
            (path.clone(), cursor[5].clone(), cursor[6].clone())
        })
        .collect()
}

fn assert_published(label: &str, captured: &Captured) {
    assert!(
        !captured.canonical.contains_key("<refused>"),
        "{label}: {:?}",
        captured.canonical.get("<refused>")
    );
}

/// Two roster transcripts lost to the build — one moved away after the
/// metadata phase, before capture reached it, the other after its first
/// batch — leave it publishable in every configuration. The first closes
/// skipped: its cursor stays fresh and nothing of it reaches the ledger. The
/// second ends its first generation at the last record read, sealed clean.
/// Both sessions are held Streaming beside an Indexed one, exactly as a live
/// pass that finds its file gone leaves them.
#[test]
fn sources_lost_before_or_during_capture_publish_their_sessions_streaming() {
    let home = claude_home(
        &[
            (
                "a-plain".to_string(),
                session_records("a-plain", 3, "plain"),
            ),
            ("b-gone".to_string(), session_records("b-gone", 4, "gone")),
            ("c-cut".to_string(), session_records("c-cut", 6, "cut")),
        ],
        None,
    );
    let project = expand_data_root(&ClaudeCodeAdapter.layout().data_root, home.path())
        .join("projects/-w-pool");
    let stash = home.path().join("stash");
    std::fs::create_dir_all(&stash).unwrap();
    let (gone, gone_stash) = (project.join("b-gone.jsonl"), stash.join("b-gone.jsonl"));
    let (cut, cut_stash) = (project.join("c-cut.jsonl"), stash.join("c-cut.jsonl"));
    let run = |config: Config, ledger: &Path| {
        restore(&gone, &gone_stash);
        restore(&cut, &cut_stash);
        let opts = IngestOptions {
            progress: Some(Arc::new(VanishOnce::new(&cut, &cut_stash))),
            ..tiny_batches(2)
        };
        capture_into_after(
            &ClaudeCodeAdapter,
            home.path(),
            &opts,
            config,
            ledger,
            &|| {
                std::fs::rename(&gone, &gone_stash).unwrap();
            },
        )
    };
    let serial = assert_every_config("lost sources", |config| {
        run(config, TempDir::new().unwrap().path())
    });
    assert_published("lost sources", &serial);
    // Files missing, hard errors, files incomplete, files paused.
    assert_eq!(
        (
            serial.report[1],
            serial.report[8],
            serial.report[13],
            serial.report[14]
        ),
        (2, 0, 2, 0),
        "{:?}",
        serial.report
    );
    for (native, state) in [
        ("a-plain", "indexed"),
        ("b-gone", "streaming"),
        ("c-cut", "streaming"),
    ] {
        assert_eq!(stream_state(&serial.canonical, native), state, "{native}");
    }
    assert_eq!(
        (
            messages_with(&serial, "plain"),
            messages_with(&serial, "gone"),
            messages_with(&serial, "cut")
        ),
        (3, 0, 2)
    );
    let one = || Value::Integer(1);
    assert_eq!(
        generations(&serial),
        [
            ("projects/-w-pool/a-plain.jsonl".to_string(), one(), one()),
            (
                "projects/-w-pool/b-gone.jsonl".to_string(),
                Value::Integer(0),
                Value::Integer(0)
            ),
            ("projects/-w-pool/c-cut.jsonl".to_string(), one(), one()),
        ]
    );
    let skipped = emitted(&serial, "projects/-w-pool/b-gone.jsonl");
    for field in ["generation: 0,", "ops: [],", "complete: false,"] {
        assert!(skipped.contains(field), "{field}: {skipped}");
    }
    let cut_short = emitted(&serial, "projects/-w-pool/c-cut.jsonl");
    for field in ["generation: 1,", "activate: true,", "complete: false,"] {
        assert!(cut_short.contains(field), "{field}: {cut_short}");
    }

    // The ledger holds nothing of the skipped source, and the cut one's
    // generation sealed clean at its last record.
    let ledger = TempDir::new().unwrap();
    run(SERIAL, ledger.path());
    let entries = scan_ledger(ledger.path()).unwrap().entries;
    let of = |name: &str| {
        entries
            .iter()
            .filter(|entry| match entry {
                LedgerEntry::Raw { source, .. }
                | LedgerEntry::GenerationComplete { source, .. } => {
                    source.relative_path.ends_with(name)
                }
                _ => false,
            })
            .collect::<Vec<_>>()
    };
    assert!(of("b-gone.jsonl").is_empty());
    let seals: Vec<_> = of("c-cut.jsonl")
        .into_iter()
        .filter_map(|entry| match entry {
            LedgerEntry::GenerationComplete {
                generation,
                normalized_clean,
                ..
            } => Some((*generation, *normalized_clean)),
            _ => None,
        })
        .collect();
    assert_eq!(seals, [(1, true)]);
    restore(&gone, &gone_stash);
    restore(&cut, &cut_stash);
}

/// An unclean replacement generation activates in a private build. The
/// journal of a session that restarts mid-capture holds a malformed record,
/// so the heal replays it into a replacement generation as unclean as its
/// first; with nothing of the source published for it to protect, the build
/// activates its parseable rows and holds the session Streaming.
#[test]
fn an_unclean_replacement_generation_activates_in_a_private_build() {
    let (serial, _home) = assert_restart_and_heal("restart, dirty journal", "{not json\n");
    assert_published("dirty journal", &serial);
    // Hard errors and files incomplete: the journal's pass and its heal.
    assert_eq!(
        (serial.report[8], serial.report[13]),
        (2, 2),
        "{:?}",
        serial.report
    );
    let journal = emitted(&serial, HEAL_JOURNAL);
    for field in ["generation: 2,", "activate: true,", "complete: false,"] {
        assert!(journal.contains(field), "{field}: {journal}");
    }
    assert_eq!(stream_state(&serial.canonical, "heal-session"), "streaming");
    assert_eq!(serial.canonical["workflow_journal"].len(), 2);
}

/// A sibling the heal cannot read again — gone after its own pass, before the
/// heal replays it — keeps the generation it was held with, no longer as
/// complete: the build publishes it, and its session holds Streaming, as the
/// live heal that finds the file gone leaves it.
#[test]
fn a_sibling_lost_before_its_heal_keeps_its_generation_streaming() {
    let home = TempDir::new().unwrap();
    let root = expand_data_root(&ClaudeCodeAdapter.layout().data_root, home.path());
    let journal = root.join(HEAL_JOURNAL);
    std::fs::create_dir_all(journal.parent().unwrap()).unwrap();
    // One record: its pass ends at the end of the file, with no further read.
    std::fs::write(
        &journal,
        concat!(
            r#"{"type":"started","key":"worker-1","agentId":"agent-1"}"#,
            "\n"
        ),
    )
    .unwrap();
    let stash = home.path().join("journal.jsonl");
    let serial = assert_every_config("sibling lost before its heal", |config| {
        restore(&journal, &stash);
        reset_heal_transcript(&root);
        let mut opts = heal_options(&root);
        let rewrite = opts.progress.take().expect("the heal options rewrite");
        opts.progress = Some(Arc::new(Both(
            rewrite,
            Arc::new(VanishOnce::new(&journal, &stash)),
        )));
        capture_into(
            &ClaudeCodeAdapter,
            home.path(),
            &opts,
            config,
            TempDir::new().unwrap().path(),
        )
    });
    restore(&journal, &stash);
    assert_published("sibling lost", &serial);
    // Files missing, restarts, files incomplete, siblings healed: only the
    // transcript restarted, and only the heal found its file gone.
    assert_eq!(
        (
            serial.report[1],
            serial.report[9],
            serial.report[13],
            serial.report[16]
        ),
        (1, 1, 1, 1),
        "{:?}",
        serial.report
    );
    let kept = emitted(&serial, HEAL_JOURNAL);
    for field in ["generation: 1,", "activate: true,", "complete: false,"] {
        assert!(kept.contains(field), "{field}: {kept}");
    }
    let transcript = emitted(&serial, HEAL_TRANSCRIPT);
    for field in ["generation: 2,", "activate: true,", "complete: true,"] {
        assert!(transcript.contains(field), "{field}: {transcript}");
    }
    assert_eq!(stream_state(&serial.canonical, "heal-session"), "streaming");
    assert_eq!(serial.canonical["workflow_journal"].len(), 1);
}

/// The Copilot basic-chat session's workspace document, a document-driver
/// group of one member.
const COPILOT_WORKSPACE: &str = "session-state/8f0e1895-c442-4ac2-828d-6b2ea7b5421d/workspace.yaml";

/// A home holding `adapter`'s recorded basic-chat scenario, and its data root.
fn basic_chat_home(adapter: &dyn AgentAdapter) -> (TempDir, PathBuf) {
    let (_, data) = fixture_roots(adapter.kind())
        .into_iter()
        .find(|(scenario, _)| scenario == "basic-chat")
        .expect("a basic-chat fixture");
    let home = TempDir::new().unwrap();
    let root = expand_data_root(&adapter.layout().data_root, home.path());
    copy_tree(&data, &root);
    (home, root)
}

/// Every emitted member of the group read from `physical` — a source named
/// `<physical>#<native id>` — with the native session id it names.
fn group_members<'a>(captured: &'a Captured, physical: &str) -> Vec<(&'a str, &'a str)> {
    let prefix = format!("relative_path: \"{physical}#");
    let marker = "native_session_id: \"";
    captured
        .emitted
        .iter()
        .filter(|source| source.contains(&prefix))
        .map(|source| {
            let start = source.find(marker).expect("a native session id") + marker.len();
            let end = start + source[start..].find('"').expect("a closing quote");
            (source.as_str(), &source[start..end])
        })
        .collect()
}

/// Every member of `physical`'s group closed skipped — no generation, no
/// ops, incomplete, its planner cursor still fresh — with its session held
/// Streaming. Returns the group's size.
fn assert_group_skipped(label: &str, captured: &Captured, physical: &str) -> u64 {
    let members = group_members(captured, physical);
    assert!(!members.is_empty(), "{label}: no member of {physical}");
    for (source, native) in &members {
        for field in ["generation: 0,", "ops: [],", "complete: false,"] {
            assert!(source.contains(field), "{label}: {field}: {source}");
        }
        assert_eq!(
            stream_state(&captured.canonical, native),
            "streaming",
            "{label}: {native}"
        );
    }
    for (path, active, highest) in generations(captured) {
        if path.starts_with(&format!("{physical}#")) {
            assert_eq!(
                (active, highest),
                (Value::Integer(0), Value::Integer(0)),
                "{label}: {path}"
            );
        }
    }
    members.len() as u64
}

/// Nothing of `physical`'s group reached the ledger at `ledger`.
fn assert_group_unledgered(label: &str, ledger: &Path, physical: &str) {
    let prefix = format!("{physical}#");
    let entries = scan_ledger(ledger).unwrap().entries;
    assert!(
        !entries.iter().any(|entry| match entry {
            LedgerEntry::Raw { source, .. } | LedgerEntry::GenerationComplete { source, .. } => {
                source.relative_path.starts_with(&prefix)
            }
            _ => false,
        }),
        "{label}: {physical} reached the ledger"
    );
}

/// A snapshot database and a document gone before capture reaches them —
/// moved away after the metadata phase, as a deleted file is — close every
/// member of their group skipped, identically in every configuration: no
/// generation and a fresh cursor, nothing ledgered, each member's session
/// held Streaming, and the build publishes.
#[test]
fn snapshot_and_document_groups_lost_before_capture_close_skipped() {
    for (adapter, physical) in [
        (&CodexAdapter as &'static dyn AgentAdapter, "state_5.sqlite"),
        (&CopilotAdapter, COPILOT_WORKSPACE),
    ] {
        let label = format!("{} {physical} lost", adapter.kind());
        let (home, root) = basic_chat_home(adapter);
        let (path, stash) = (root.join(physical), home.path().join("stash"));
        let run = |config: Config, ledger: &Path| {
            restore(&path, &stash);
            capture_into_after(
                adapter,
                home.path(),
                &IngestOptions::default(),
                config,
                ledger,
                &|| std::fs::rename(&path, &stash).unwrap(),
            )
        };
        let serial =
            assert_every_config(&label, |config| run(config, TempDir::new().unwrap().path()));
        assert_published(&label, &serial);
        let members = assert_group_skipped(&label, &serial, physical);
        // Files missing: the whole group.
        assert_eq!(serial.report[1], members, "{label}: {:?}", serial.report);
        let ledger = TempDir::new().unwrap();
        assert_eq!(run(SERIAL, ledger.path()).emitted, serial.emitted);
        assert_group_unledgered(&label, ledger.path(), physical);
        restore(&path, &stash);
    }
}

/// Stands in for a document every read of which is unstable: a FIFO at its
/// path whose writer hands each reader one byte its stated length never
/// shows, however many times the read is retried.
struct UnstableDocument {
    path: PathBuf,
    stop: Arc<AtomicBool>,
    writer: Option<std::thread::JoinHandle<()>>,
}

impl UnstableDocument {
    fn at(path: &Path) -> Self {
        let made = std::process::Command::new("mkfifo")
            .arg(path)
            .status()
            .unwrap();
        assert!(made.success(), "mkfifo {}", path.display());
        let stop = Arc::new(AtomicBool::new(false));
        let writer = {
            let (path, stop) = (path.to_path_buf(), stop.clone());
            std::thread::spawn(move || {
                loop {
                    // Opening blocks until a reader opens the other end.
                    let mut fifo = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
                    if stop.load(Ordering::SeqCst) {
                        break;
                    }
                    let _ = std::io::Write::write_all(&mut fifo, b"x");
                }
            })
        };
        Self {
            path: path.to_path_buf(),
            stop,
            writer: Some(writer),
        }
    }
}

impl Drop for UnstableDocument {
    fn drop(&mut self) {
        // The writer only ever stops inside an open, so one last reader
        // releases it.
        self.stop.store(true, Ordering::SeqCst);
        let _ = std::fs::read(&self.path);
        if let Some(writer) = self.writer.take() {
            writer.join().unwrap();
        }
        std::fs::remove_file(&self.path).unwrap();
    }
}

/// A document whose every read is unstable — its length changes across the
/// read, under every retry — closes its group skipped like one that is gone,
/// identically in every configuration: the build publishes with the member's
/// session held Streaming.
#[test]
fn a_document_unstable_under_every_retry_closes_its_group_skipped() {
    let label = "copilot workspace unstable";
    let (home, root) = basic_chat_home(&CopilotAdapter);
    let (path, stash) = (root.join(COPILOT_WORKSPACE), home.path().join("stash"));
    let run = |config: Config, ledger: &Path| {
        restore(&path, &stash);
        let unstable = std::cell::RefCell::new(None);
        let captured = capture_into_after(
            &CopilotAdapter,
            home.path(),
            &IngestOptions::default(),
            config,
            ledger,
            &|| {
                std::fs::rename(&path, &stash).unwrap();
                *unstable.borrow_mut() = Some(UnstableDocument::at(&path));
            },
        );
        drop(unstable.into_inner());
        captured
    };
    let serial = assert_every_config(label, |config| run(config, TempDir::new().unwrap().path()));
    assert_published(label, &serial);
    let members = assert_group_skipped(label, &serial, COPILOT_WORKSPACE);
    // Files missing, retries exhausted: the whole group, never missing.
    assert_eq!(
        (serial.report[1], serial.report[12]),
        (0, members),
        "{:?}",
        serial.report
    );
    let ledger = TempDir::new().unwrap();
    assert_eq!(run(SERIAL, ledger.path()).emitted, serial.emitted);
    assert_group_unledgered(label, ledger.path(), COPILOT_WORKSPACE);
    restore(&path, &stash);
}
