//! A fresh start — and a resumed one — publishes past what the live path
//! already absorbs of a single file: a malformed record, a transcript that
//! vanished between metadata and capture, a rollout still writing its first
//! record. Each such session is published Streaming (or, not yet a session,
//! withheld) and the live runner takes it from there. What no later read can
//! repair still refuses (`record_fresh_failures`' own tests).

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use rosetta_adapter_claude_code::ClaudeCodeAdapter;
use rosetta_adapter_support::AgentAdapter;
use rosetta_capture::{LedgerEntry, ScriptedEvents, scan_ledger};
use rosetta_pipeline::progress::ColdProgressObserver;
use rosetta_pipeline::{
    IngestOptions, Instrument, LiveOptions, LiveRunner, PollEvidence, SessionFile,
    StreamedPreparer, StreamedRuntime, discover, expand_data_root, meta_ingest_inventory_into,
    stream_closed_sources_into, streamed_setup,
};
use rosetta_store::Store;
use tempfile::TempDir;

fn copy_tree(from: &Path, to: &Path) {
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

fn fixture(agent: &str, version: &str, scenario: &str, root: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
        "../../groundtruth/{agent}/fixtures/{version}/{scenario}/{root}"
    ))
}

/// A home holding Claude Code's basic-chat fixture.
fn claude_home(home: &Path) -> PathBuf {
    let claude = home.join(".claude");
    copy_tree(
        &fixture("claude-code", "2.1.241", "basic-chat", "claude-config"),
        &claude,
    );
    claude
}

fn run_serve(home: &Path, db: &Path, agent: &str) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_rosettad"))
        .args([
            "serve",
            "--db",
            db.to_str().unwrap(),
            "--home",
            home.to_str().unwrap(),
            "--agent",
            agent,
            "--addr",
            "127.0.0.1:0",
            "--ticks",
            "0",
            "--json",
        ])
        .output()
        .unwrap()
}

fn assert_success(output: &std::process::Output) -> String {
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// The one line a start prints for what it tolerated.
fn tolerated(stdout: &str) -> &str {
    let lines: Vec<_> = stdout
        .lines()
        .filter(|line| line.contains("tolerated per-file imperfections"))
        .collect();
    assert_eq!(lines.len(), 1, "{stdout}");
    lines[0]
}

fn find_jsonl(root: &Path) -> PathBuf {
    fn find(root: &Path) -> Option<PathBuf> {
        for entry in std::fs::read_dir(root).ok()? {
            let path = entry.ok()?.path();
            if path.is_dir() {
                if let Some(found) = find(&path) {
                    return Some(found);
                }
            } else if path
                .extension()
                .is_some_and(|extension| extension == "jsonl")
            {
                return Some(path);
            }
        }
        None
    }
    find(root).unwrap_or_else(|| panic!("no jsonl under {}", root.display()))
}

fn native_id(transcript: &Path) -> String {
    transcript
        .file_stem()
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

fn scalar(db: &Path, sql: &str) -> Option<String> {
    rusqlite::Connection::open(db)
        .and_then(|conn| conn.query_row(sql, [], |row| row.get(0)))
        .ok()
}

fn column(db: &Path, sql: &str) -> Vec<String> {
    let conn = rusqlite::Connection::open(db).unwrap();
    conn.prepare(sql)
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn stream_state(db: &Path, native: &str) -> Option<String> {
    scalar(
        db,
        &format!("SELECT stream_state FROM session WHERE native_session_id = '{native}'"),
    )
}

/// The seals of every generation of the runtime's JSONL sources.
fn jsonl_seals(ledger: &Path) -> Vec<(u64, bool)> {
    scan_ledger(ledger)
        .unwrap()
        .entries
        .into_iter()
        .filter_map(|entry| match entry {
            LedgerEntry::GenerationComplete {
                source,
                generation,
                normalized_clean,
                ..
            } if source.relative_path.ends_with(".jsonl") => Some((generation, normalized_clean)),
            _ => None,
        })
        .collect()
}

/// Turn a finished start back into an interrupted one: the projection is
/// gone and its ledger is the named staged generation, with its manifest.
fn stage_ledger(db: &Path, claude: &Path) -> PathBuf {
    // Model a legacy interrupted start, without a completed run receipt.
    std::fs::remove_file(db.with_file_name("rosetta.db.ingestion.json")).unwrap();
    let ledger = db.with_extension("ledger");
    let staged = ledger.with_file_name(format!(
        "{}.staged",
        ledger.file_name().unwrap().to_string_lossy()
    ));
    std::fs::remove_file(db).unwrap();
    std::fs::rename(&ledger, &staged).unwrap();
    let manifest = serde_json::json!({
        "format": 1,
        "projection": db.file_name().unwrap().to_string_lossy(),
        "ledger": ledger.file_name().unwrap().to_string_lossy(),
        "enabled_agents": ["claude-code"],
        "runtime_roots": { "claude-code": claude.to_string_lossy() },
    });
    std::fs::write(
        staged.join(".cold-generation.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
    staged
}

fn claude_user(native: &str, uuid: &str, content: &str) -> String {
    serde_json::json!({
        "type": "user",
        "sessionId": native,
        "uuid": uuid,
        "parentUuid": null,
        "isSidechain": false,
        "userType": "external",
        "timestamp": "2026-08-31T20:00:00.000Z",
        "cwd": "/tmp/rosetta-tolerance",
        "gitBranch": "main",
        "version": "2.1.241",
        "message": {"role": "user", "content": content},
    })
    .to_string()
}

/// A malformed record between a transcript's first two no longer fails a
/// fresh start: every parseable record publishes — the same rows a start over
/// the clean transcript publishes — its first generation seals unclean, and
/// the session is Streaming, as the dirty-first-generation rule leaves it.
#[test]
fn a_malformed_record_mid_transcript_publishes_its_session_streaming() {
    let temp = TempDir::new().unwrap();
    let clean_home = temp.path().join("clean");
    claude_home(&clean_home);
    let clean_db = temp.path().join("clean/rosetta.db");
    assert_success(&run_serve(&clean_home, &clean_db, "claude-code"));

    let home = temp.path().join("dirty");
    let claude = claude_home(&home);
    let transcript = find_jsonl(&claude.join("projects"));
    let text = std::fs::read(&transcript).unwrap();
    let first = text.iter().position(|byte| *byte == b'\n').unwrap() + 1;
    assert!(first < text.len(), "the transcript has a second record");
    let mut dirty = text[..first].to_vec();
    dirty.extend_from_slice(b"{not json\n");
    dirty.extend_from_slice(&text[first..]);
    std::fs::write(&transcript, dirty).unwrap();
    let db = temp.path().join("dirty/rosetta.db");

    let stdout = assert_success(&run_serve(&home, &db, "claude-code"));
    assert!(stdout.contains("milestone: canonical-ready"), "{stdout}");
    let line = tolerated(&stdout);
    assert!(
        line.contains("hard_errors=1") && line.contains("incomplete=1"),
        "{line}"
    );
    let timelines = "SELECT timeline_id FROM message ORDER BY timeline_id";
    let published = column(&db, timelines);
    assert!(!published.is_empty());
    assert_eq!(published, column(&clean_db, timelines));
    assert_eq!(
        stream_state(&db, &native_id(&transcript)).as_deref(),
        Some("streaming")
    );
    assert_eq!(
        stream_state(&clean_db, &native_id(&transcript)).as_deref(),
        Some("indexed")
    );
    assert_eq!(
        jsonl_seals(&db.with_extension("ledger").join("claude-code")),
        [(1, false)]
    );
}

/// A resumed start publishes past a malformed record the same way: the
/// interrupted generation replays, the catch-up reads the malformed record and
/// the good one after it, and the start publishes with the session Streaming.
#[test]
fn a_resumed_start_publishes_past_a_malformed_record() {
    let temp = TempDir::new().unwrap();
    let home = temp.path().join("home");
    let claude = claude_home(&home);
    let db = temp.path().join("archive/rosetta.db");
    assert_success(&run_serve(&home, &db, "claude-code"));
    let staged = stage_ledger(&db, &claude);

    // While no daemon ran, the agent wrote a malformed record, then a good one.
    let transcript = find_jsonl(&claude.join("projects"));
    let native = native_id(&transcript);
    let sentinel = "quetzalcoatl after the malformed record";
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&transcript)
        .unwrap();
    writeln!(file, "{{not json").unwrap();
    writeln!(
        file,
        "{}",
        claude_user(&native, "00000000-0000-4000-8000-000000000777", sentinel)
    )
    .unwrap();
    drop(file);

    let resumed = assert_success(&run_serve(&home, &db, "claude-code"));
    assert!(resumed.contains("resuming cold generation"), "{resumed}");
    assert!(resumed.contains("milestone: canonical-ready"), "{resumed}");
    assert!(tolerated(&resumed).contains("hard_errors=1"), "{resumed}");
    assert!(!staged.exists());
    assert_eq!(
        scalar(
            &db,
            &format!("SELECT CAST(COUNT(*) AS TEXT) FROM message WHERE content = '{sentinel}'")
        )
        .as_deref(),
        Some("1")
    );
    assert_eq!(stream_state(&db, &native).as_deref(), Some("streaming"));
    let generations = scalar(
        &db,
        "SELECT active_generation || '/' || highest_generation FROM ingest_cursor
         WHERE path LIKE '%.jsonl'",
    )
    .unwrap();
    let (active, highest) = generations.split_once('/').unwrap();
    assert_eq!(active, highest, "no generation is left staged");
}

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A Codex rollout caught half way through its opening record at a fresh
/// start is not yet a session: the start publishes without it, reporting it
/// unfinished, and once its writer finishes the rollout the live runner
/// admits it and streams every record — the same rows the fixture's own
/// session holds.
#[test]
fn a_rollout_caught_writing_its_first_record_is_admitted_live_once_written() {
    let temp = TempDir::new().unwrap();
    let home = temp.path().join("home");
    let codex = home.join(".codex");
    copy_tree(
        &fixture("codex", "0.149.1", "basic-chat", "codex-home"),
        &codex,
    );
    let original = find_jsonl(&codex.join("sessions"));
    let original_id = "01a036c5-6e18-73f3-8c07-d4430763840b";
    let fresh_id = "02b147d6-7f29-7404-9d18-e5541874951c";
    let text = std::fs::read_to_string(&original)
        .unwrap()
        .replace(original_id, fresh_id);
    let first = text.find('\n').unwrap();
    let rollout = codex.join(format!(
        "sessions/2026/08/25/rollout-2026-08-25T10-00-00-{fresh_id}.jsonl"
    ));
    std::fs::create_dir_all(rollout.parent().unwrap()).unwrap();
    std::fs::write(&rollout, &text.as_bytes()[..first / 2]).unwrap();
    let db = temp.path().join("archive/rosetta.db");

    let mut child = Command::new(env!("CARGO_BIN_EXE_rosettad"))
        .args([
            "serve",
            "--db",
            db.to_str().unwrap(),
            "--home",
            home.to_str().unwrap(),
            "--agent",
            "codex",
            "--addr",
            "127.0.0.1:0",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let mut child = ChildGuard(child);
    let (sender, receiver) = mpsc::channel();
    let errors = sender.clone();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let _ = sender.send(line);
        }
    });
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            let _ = errors.send(format!("stderr: {line}"));
        }
    });
    let mut log = Vec::new();
    loop {
        let line = receiver
            .recv_timeout(Duration::from_secs(60))
            .unwrap_or_else(|_| panic!("no canonical-ready:\n{}", log.join("\n")));
        let ready = line.contains("milestone: canonical-ready");
        log.push(line);
        if ready {
            break;
        }
    }
    let log = log.join("\n");
    assert!(tolerated(&log).contains("unfinished=1"), "{log}");
    let count = |native: &str| {
        scalar(
            &db,
            &format!(
                "SELECT CAST(COUNT(*) AS TEXT) FROM message m JOIN session s ON s.id = m.session_id
                 WHERE s.native_session_id = '{native}'"
            ),
        )
        .and_then(|count| count.parse::<i64>().ok())
    };
    let expected = count(original_id).unwrap();
    assert!(expected > 0);
    assert_eq!(stream_state(&db, fresh_id), None, "{log}");

    // The writer finishes the rollout. FSEvents may be suppressed in a
    // sandbox; the stat poll's movement evidence must admit it anyway.
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&rollout)
        .unwrap();
    file.write_all(&text.as_bytes()[first / 2..]).unwrap();
    file.sync_data().unwrap();
    drop(file);
    let deadline = Instant::now() + Duration::from_secs(45);
    while !(stream_state(&db, fresh_id).as_deref() == Some("indexed")
        && count(fresh_id) == Some(expected))
    {
        assert_eq!(child.0.try_wait().unwrap(), None, "daemon exited early");
        assert!(
            Instant::now() < deadline,
            "the live runner never admitted it: {:?} rows, {:?}",
            count(fresh_id),
            stream_state(&db, fresh_id)
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    child.0.kill().unwrap();
    child.0.wait().unwrap();
}

/// An unterminated opening record over the 32 MiB projection limit is more
/// than the metadata skim reads, so today its rollout counts as unidentified,
/// not unfinished. A fresh start publishes past it; the strict gate of a
/// finite `ingest` refuses it.
#[test]
fn an_unterminated_opening_record_over_the_projection_limit_is_tolerated_only_fresh() {
    let temp = TempDir::new().unwrap();
    let home = temp.path().join("home");
    let codex = home.join(".codex");
    copy_tree(
        &fixture("codex", "0.149.1", "basic-chat", "codex-home"),
        &codex,
    );
    let rollout = codex.join(
        "sessions/2026/08/25/rollout-2026-08-25T11-00-00-03c258e7-8a3a-7515-ae29-f6652985a62d.jsonl",
    );
    std::fs::create_dir_all(rollout.parent().unwrap()).unwrap();
    let mut record = String::from(
        "{\"timestamp\":\"2026-08-25T02:35:03.601Z\",\"type\":\"session_meta\",\
         \"payload\":{\"id\":\"03c258e7-8a3a-7515-ae29-f6652985a62d\",\"base_instructions\":\"",
    );
    record.push_str(&"q".repeat(33 * 1024 * 1024));
    std::fs::write(&rollout, record).unwrap();

    let stdout = assert_success(&run_serve(
        &home,
        &temp.path().join("fresh/rosetta.db"),
        "codex",
    ));
    let meta = stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|event| event["event"] == "meta" && event["agent"] == "codex")
        .expect("a codex meta event");
    assert_eq!(
        (&meta["filesUnidentified"], &meta["filesUnfinished"]),
        (&serde_json::json!(1), &serde_json::json!(0)),
        "{meta}"
    );
    assert!(
        tolerated(&stdout).contains("unfinished=0, unidentified=1"),
        "{stdout}"
    );

    let strict_db = temp.path().join("strict/rosetta.db");
    std::fs::create_dir_all(strict_db.parent().unwrap()).unwrap();
    let strict = Command::new(env!("CARGO_BIN_EXE_rosettad"))
        .args([
            "ingest",
            "--db",
            strict_db.to_str().unwrap(),
            "--home",
            home.to_str().unwrap(),
            "--agent",
            "codex",
        ])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&strict.stderr);
    assert!(!strict.status.success(), "{stderr}");
    assert!(
        stderr.contains("codex left 1 streamable files unidentified"),
        "{stderr}"
    );
}

/// Removes one roster source the first time capture reports bytes for
/// another — `trigger`, earlier in path order — so the serial walk finds it
/// gone when its turn comes: deleted between metadata and capture.
#[derive(Debug)]
struct DeleteWhenReading {
    trigger: String,
    target: PathBuf,
    done: AtomicBool,
}

impl ColdProgressObserver for DeleteWhenReading {
    fn source_bytes(&self, file: &SessionFile, _bytes: u64) {
        if file.relative_path == self.trigger && !self.done.swap(true, Ordering::SeqCst) {
            std::fs::remove_file(&self.target).unwrap();
        }
    }

    fn reading_finished(&self) {}
}

/// A roster transcript deleted after the metadata phase named its session,
/// before capture read it, no longer fails the build: its session publishes
/// Streaming with no rows and its cursor still fresh, beside an Indexed one —
/// what a live pass that finds the file gone leaves — and when the file comes
/// back the ordinary live pass reads it whole.
#[test]
fn a_transcript_deleted_between_metadata_and_capture_publishes_its_session_streaming() {
    let home = TempDir::new().unwrap();
    let adapter: &'static dyn AgentAdapter = &ClaudeCodeAdapter;
    let root = expand_data_root(&adapter.layout().data_root, home.path());
    let project = root.join("projects/-w-tolerance");
    std::fs::create_dir_all(&project).unwrap();
    let write = |native: &str, tag: usize, count: usize| {
        let lines: Vec<String> = (0..count)
            .map(|index| {
                claude_user(
                    native,
                    &format!("00000000-0000-4000-8{tag:03}-{index:012}"),
                    &format!("{native} says {index}"),
                )
            })
            .collect();
        let path = project.join(format!("{native}.jsonl"));
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();
        path
    };
    write("a-kept", 1, 3);
    let gone = write("b-gone", 2, 4);
    let gone_text = std::fs::read(&gone).unwrap();

    let opts = IngestOptions {
        progress: Some(Arc::new(DeleteWhenReading {
            trigger: "projects/-w-tolerance/a-kept.jsonl".into(),
            target: gone.clone(),
            done: AtomicBool::new(false),
        })),
        ..IngestOptions::default()
    };
    let instrument = Instrument::new();
    let discovery = discover(&[adapter], home.path(), &opts, &instrument)
        .unwrap()
        .pop()
        .unwrap();
    let mut runtime = discovery.runtime;
    runtime.enabled = true;
    let mut planner = Store::open_in_memory_for_cold_planning().unwrap();
    let (meta, files) = meta_ingest_inventory_into(
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
    assert_eq!(meta.sessions, 2);
    let mut canonical = Store::open_in_memory_for_rebuild().unwrap();
    canonical.use_precomputed_bulk_ordinals().unwrap();
    for batch in planner.planned_metadata_batches().unwrap() {
        canonical.apply(&batch).unwrap();
    }
    let setup = streamed_setup(&files, &mut planner).unwrap();
    let mut writer =
        StreamedRuntime::open(adapter, &runtime, setup, &mut canonical, &instrument).unwrap();
    let mut preparer = StreamedPreparer::new(&files);
    let report = stream_closed_sources_into(
        adapter,
        &runtime,
        &discovery.root,
        &mut planner,
        &files,
        &opts,
        &instrument,
        None,
        &mut |mut sources| {
            preparer.prepare(&mut sources)?;
            writer
                .apply(&mut canonical, sources, &instrument)
                .map(|_| ())
        },
    )
    .unwrap();
    assert_eq!((report.files_missing, report.files_incomplete), (1, 1));
    let evidence = writer.take_poll_evidence();
    writer
        .admit(&mut canonical, &instrument, &preparer.finish().unwrap())
        .unwrap();
    canonical.finish_bulk_load().unwrap();
    // The live runner trusts the writer's seed as its cursor rows: the
    // skipped source's is its fresh cursor's.
    assert_eq!(evidence.len(), 2);
    for (path, seeded) in &evidence {
        assert_eq!(
            PollEvidence::read(&canonical, &runtime.id, path).unwrap(),
            *seeded,
            "{path}"
        );
    }

    let query = |store: &Store, sql: &str| -> String {
        store.conn().query_row(sql, [], |row| row.get(0)).unwrap()
    };
    let session = |store: &Store, native: &str| {
        (
            query(
                store,
                &format!(
                    "SELECT CAST(COUNT(*) AS TEXT) FROM message m
                     JOIN session s ON s.id = m.session_id
                     WHERE s.native_session_id = '{native}'"
                ),
            ),
            query(
                store,
                &format!("SELECT stream_state FROM session WHERE native_session_id = '{native}'"),
            ),
        )
    };
    let expect = |rows: &str, state: &str| (rows.to_string(), state.to_string());
    assert_eq!(session(&canonical, "a-kept"), expect("3", "indexed"));
    assert_eq!(session(&canonical, "b-gone"), expect("0", "streaming"));
    assert_eq!(
        query(
            &canonical,
            "SELECT active_generation || '/' || highest_generation FROM ingest_cursor
             WHERE path LIKE '%b-gone.jsonl'"
        ),
        "0/0"
    );

    // The file comes back: the live runner's startup poll finds it behind
    // its fresh cursor and reads it whole.
    std::fs::write(&gone, gone_text).unwrap();
    let mut runner = LiveRunner::resume_from_roster(
        adapter,
        runtime,
        discovery.root.clone(),
        Box::new(ScriptedEvents::default()),
        files,
        IngestOptions::default(),
        LiveOptions {
            tick: Duration::ZERO,
            poll_interval: Duration::from_secs(3600),
            max_coalesced: 8,
        },
        None,
    )
    .with_poll_evidence(evidence);
    runner.catch_up(&mut canonical, &instrument).unwrap();
    assert_eq!(session(&canonical, "a-kept"), expect("3", "indexed"));
    assert_eq!(session(&canonical, "b-gone"), expect("4", "indexed"));
}
