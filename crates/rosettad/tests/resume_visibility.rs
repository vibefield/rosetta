//! A resumed start never shows a half-reconciled session. It replays its
//! staged ledger and catches up into the canonical RAM generation the read
//! routes already target, so until it has reconciled and finalized every
//! session stays Pending: metadata is listed, no payload is served. A pause
//! during the resumed catch-up is the deterministic probe point — the RAM
//! reader outlives the cancelled build — and the projection the resume then
//! publishes equals a clean build's, tool rows and timeline order included.
//! A runtime whose catch-up already finished stays Pending, and uncounted on
//! the progress board, while a later runtime reads. An interrupted source that
//! has since vanished ends at its staged prefix, sealed, like a file a fresh
//! start loses mid-read, so the published archive still rebuilds.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use rosetta_adapter_claude_code::ClaudeCodeAdapter;
use rosetta_adapter_codex::CodexAdapter;
use rosetta_adapter_support::AgentAdapter;
use rosetta_bench_corpus::{Agent, CorpusSpec, generate};
use rosetta_capture::{LedgerEntry, scan_ledger};
use rosetta_pipeline::expand_data_root;
use serde_json::{Value, json};

/// A transcript the interrupted start never saw, long enough that the
/// resumed catch-up is still reading it when the test pauses.
const LONG_SESSION: &str = "aaaaaaaa-bbbb-4000-8000-cccccccccccc";
const TOOL_PROJECT: &str = "projects/-private-tmp-rosetta-farm-tool-use-project";
const CLAUDE: &[&str] = &["claude-code"];

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

/// Claude Code's tool-use fixture plus generated sessions with tool calls.
fn claude_home(home: &Path) -> PathBuf {
    let claude = expand_data_root(&ClaudeCodeAdapter.layout().data_root, home);
    copy_tree(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../groundtruth/claude-code/fixtures/2.1.241/tool-use/claude-config"),
        &claude,
    );
    generate(&CorpusSpec::new(Agent::ClaudeCode, 6, 260, 42), &claude).unwrap();
    claude
}

fn long_transcript(claude: &Path) -> PathBuf {
    let path = claude
        .join(TOOL_PROJECT)
        .join(format!("{LONG_SESSION}.jsonl"));
    let mut file = std::io::BufWriter::new(std::fs::File::create(&path).unwrap());
    let text = "resume visibility fixture ".repeat(40);
    for i in 0..10_000 {
        writeln!(
            file,
            "{}",
            json!({"type":"user", "isSidechain":false, "version":"2.1.241", "userType":"external",
            "gitBranch":"HEAD", "uuid":format!("resume-{i}"), "parentUuid":null,
            "sessionId":LONG_SESSION, "cwd":"/private/tmp/rosetta-farm/tool-use/project",
            "timestamp":"2026-10-29T11:00:00Z", "message":{"role":"user","content":text}})
        )
        .unwrap();
    }
    file.flush().unwrap();
    path
}

fn agent_args(agents: &[&str]) -> Vec<String> {
    agents
        .iter()
        .flat_map(|agent| ["--agent".to_string(), agent.to_string()])
        .collect()
}

fn serve_once(home: &Path, db: &Path, agents: &[&str]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_rosettad"))
        .args(["serve", "--db", db.to_str().unwrap(), "--home"])
        .arg(home)
        .args(agent_args(agents))
        .args(["--addr", "127.0.0.1:0", "--ticks", "0"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

struct Daemon {
    child: Child,
    origin: String,
    lines: mpsc::Receiver<String>,
    log: std::cell::RefCell<Vec<String>>,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Daemon {
    /// A long-running serve; it answers HTTP until dropped.
    fn spawn(home: &Path, db: &Path, agents: &[&str]) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_rosettad"))
            .args(["serve", "--db", db.to_str().unwrap(), "--home"])
            .arg(home)
            .args(agent_args(agents))
            .args(["--addr", "127.0.0.1:0"])
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (sender, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let _ = sender.send(line);
            }
        });
        let mut daemon = Self {
            child,
            origin: String::new(),
            lines,
            log: Default::default(),
        };
        daemon.origin = loop {
            let line = daemon.lines.recv_timeout(Duration::from_secs(20)).unwrap();
            let origin = line
                .split_whitespace()
                .find(|word| word.starts_with("http://"))
                .map(str::to_owned);
            daemon.log.borrow_mut().push(line);
            if let Some(origin) = origin {
                break origin;
            }
        };
        daemon
    }

    /// Everything printed so far.
    fn stdout(&self) -> Vec<String> {
        let mut log = self.log.borrow_mut();
        log.extend(self.lines.try_iter());
        log.clone()
    }

    fn get(&self, path: &str) -> Value {
        let address = self.origin.strip_prefix("http://").unwrap();
        let mut stream = TcpStream::connect(address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        write!(
            stream,
            "GET {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        let body = response
            .split_once("\r\n\r\n")
            .map(|(_, body)| body)
            .unwrap_or_default();
        serde_json::from_str(body).unwrap_or_else(|error| panic!("{path}: {error}\n{response}"))
    }

    fn post(&self, status: &Value, action: &str, key: usize) -> u16 {
        let run = run(status);
        let address = self.origin.strip_prefix("http://").unwrap();
        let path = format!("/api/ingestions/{}/{action}", run["id"].as_str().unwrap());
        let body = json!({"expected_revision": run["revision"]}).to_string();
        let mut stream = TcpStream::connect(address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(15)))
            .unwrap();
        write!(
            stream,
            "POST {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\nContent-Type: application/json\r\nX-Rosetta-Index-Id: {}\r\nIdempotency-Key: 00000000-0000-4000-8000-{key:012}\r\nContent-Length: {}\r\n\r\n{body}",
            run["index_id"].as_str().unwrap(),
            body.len()
        )
        .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response.split_whitespace().nth(1).unwrap().parse().unwrap()
    }

    fn wait(&self, what: &str, predicate: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            let status = self.get("/api/status");
            if predicate(&status) {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "{what} timed out: {status}\n{}",
                self.stdout().join("\n")
            );
            assert_ne!(
                run(&status)["state"],
                "failed",
                "{what}: {status}\n{}",
                self.stdout().join("\n")
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Pause the resumed build while its catch-up is reading sources: after
    /// the staged ledger's replay, before any reconciliation or finalization.
    fn pause_while_catching_up(&self) -> Value {
        let reading = self.wait("resumed catch-up", |status| {
            let ingest = &status["daemon"]["ingest"];
            ingest["phase"] == "backfilling"
                && ingest["runtimes"].as_array().is_some_and(|rows| {
                    rows.iter().any(|row| {
                        row["state"] == "streaming"
                            && row["bytes_streamed"].as_u64().unwrap_or(0) > 0
                    })
                })
        });
        assert_eq!(
            run(&reading)["state"],
            "running",
            "fixture finished before pause: {reading}"
        );
        assert_eq!(self.post(&reading, "pause", 1), 202);
        let paused = self.wait("pause", |status| run(status)["state"] == "paused");
        assert_eq!(paused["canonical_ready"], false, "{paused}");
        paused
    }

    fn resume_to_publication(&self, paused: &Value, key: usize) {
        assert_eq!(self.post(paused, "resume", key), 202);
        self.wait("publication", |status| {
            status["canonical_ready"] == true && run(status)["state"] == "succeeded"
        });
    }
}

fn run(status: &Value) -> &Value {
    &status["daemon"]["ingest"]["run"]
}

/// Turn a finished start back into an interrupted one: the projection is
/// gone and its ledger is the named staged generation, with its manifest.
fn stage_ledger(db: &Path, roots: &[(&str, &Path)]) -> PathBuf {
    std::fs::remove_file(db.with_file_name("rosetta.db.ingestion.json")).unwrap();
    let ledger = db.with_extension("ledger");
    let staged = ledger.with_file_name(format!(
        "{}.staged",
        ledger.file_name().unwrap().to_string_lossy()
    ));
    std::fs::remove_file(db).unwrap();
    std::fs::rename(&ledger, &staged).unwrap();
    let manifest = json!({
        "format": 1,
        "projection": db.file_name().unwrap().to_string_lossy(),
        "ledger": ledger.file_name().unwrap().to_string_lossy(),
        "enabled_agents": roots.iter().map(|(agent, _)| agent).collect::<Vec<_>>(),
        "runtime_roots": roots
            .iter()
            .map(|(agent, root)| (agent.to_string(), json!(root.to_string_lossy())))
            .collect::<serde_json::Map<_, _>>(),
    });
    std::fs::write(
        staged.join(".cold-generation.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
    staged
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

/// Every canonical row a reader can see, with surrogate ids mapped to stable
/// identities (cursor ids to paths, message ids to timeline ids) and rows
/// sorted. Runtime rows (discovery time), cursors (their checkpoints name
/// file identities) and the search/readiness bookkeeping are left out.
fn canonical_rows(db: &Path) -> std::collections::BTreeMap<String, Vec<String>> {
    let conn = rusqlite::Connection::open(db).unwrap();
    let lookup = |sql: &str| -> std::collections::HashMap<i64, String> {
        conn.prepare(sql)
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    let cursors = lookup("SELECT id, path FROM ingest_cursor");
    let messages = lookup("SELECT id, timeline_id FROM message");
    let mut out = std::collections::BTreeMap::new();
    for table in [
        "project",
        "session",
        "plan",
        "task",
        "workflow_run",
        "workflow_journal",
        "message",
        "file_edit",
        "git_commit",
        "drift_agg",
        "drift_seen",
        "session_usage",
        "usage_bucket",
    ] {
        let mut statement = conn.prepare(&format!("SELECT * FROM {table}")).unwrap();
        let columns: Vec<String> = statement
            .column_names()
            .into_iter()
            .map(str::to_owned)
            .collect();
        let mut rows = statement
            .query_map([], |row| {
                let mut fields = Vec::new();
                for (index, column) in columns.iter().enumerate() {
                    if table == "message" && column == "id" {
                        continue;
                    }
                    let value: rusqlite::types::Value = row.get(index)?;
                    let shown = match (&value, column.as_str()) {
                        (rusqlite::types::Value::Integer(id), name) if name.ends_with("cursor") => {
                            cursors[id].clone()
                        }
                        (rusqlite::types::Value::Integer(id), "message_id") => messages[id].clone(),
                        _ => format!("{value:?}"),
                    };
                    fields.push(format!("{column}={shown}"));
                }
                Ok(fields.join(" | "))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        rows.sort();
        out.insert(table.to_string(), rows);
    }
    out
}

/// Every reconciled tool row the projection holds.
fn tool_rows(db: &Path) -> usize {
    column(
        db,
        "SELECT timeline_id FROM message WHERE type = 'tool_use' AND tool_status IS NOT NULL",
    )
    .len()
}

/// Before it publishes, a resumed build lists sessions but serves no payload
/// for any of them; what it then publishes is the clean build's projection.
#[test]
fn a_resumed_build_serves_no_payload_until_it_publishes_the_clean_projection() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let claude = claude_home(&home);
    let db = temp.path().join("archive/rosetta.db");
    serve_once(&home, &db, CLAUDE);
    let replayed = column(&db, "SELECT id FROM session ORDER BY id");
    assert!(replayed.len() > 6, "{replayed:?}");
    assert!(tool_rows(&db) > 0);
    let staged = stage_ledger(&db, &[("claude-code", &claude)]);
    long_transcript(&claude);

    let daemon = Daemon::spawn(&home, &db, CLAUDE);
    let paused = daemon.pause_while_catching_up();
    assert!(!db.exists(), "an accepted pause must prevent publication");
    assert!(
        daemon
            .stdout()
            .iter()
            .any(|line| line.contains("resuming cold generation")),
        "this start resumed the staged generation"
    );
    let listed = daemon.get("/api/sessions?limit=500");
    let listed = listed["sessions"].as_array().unwrap();
    assert!(!listed.is_empty(), "replayed metadata stays public");
    for session in listed {
        assert_eq!(session["stream_state"], "pending", "{session}");
    }
    for id in &replayed {
        let page = daemon.get(&format!("/api/sessions/{id}/timeline?limit=500"));
        assert_eq!(
            page["entries"],
            json!([]),
            "{id} is readable before publication"
        );
    }

    daemon.resume_to_publication(&paused, 2);
    drop(daemon);
    assert!(!staged.exists());
    let clean_db = temp.path().join("clean/rosetta.db");
    serve_once(&home, &clean_db, CLAUDE);
    let resumed = canonical_rows(&db);
    for (table, rows) in canonical_rows(&clean_db) {
        assert_eq!(resumed[&table], rows, "{table}");
    }
    assert!(tool_rows(&db) > 0);
}

/// Catch-up withholds too, and so does the progress board: while a later
/// runtime is still reading, a runtime whose resumed catch-up has finished
/// still serves no payload, and no project counts a session done.
#[test]
fn a_finished_runtime_catch_up_stays_pending_until_the_resume_publishes() {
    const BOTH: &[&str] = &["claude-code", "codex"];
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let claude = claude_home(&home);
    let codex = expand_data_root(&CodexAdapter.layout().data_root, &home);
    generate(&CorpusSpec::new(Agent::Codex, 4, 200, 7), &codex).unwrap();
    let db = temp.path().join("archive/rosetta.db");
    serve_once(&home, &db, BOTH);
    let claude_sessions = column(
        &db,
        "SELECT s.id FROM session s JOIN agent_runtime r ON r.id = s.runtime_id
         WHERE r.kind = 'claude-code' ORDER BY s.id",
    );
    assert!(!claude_sessions.is_empty());
    let staged = stage_ledger(&db, &[("claude-code", &claude), ("codex", &codex)]);
    // Codex catches up after Claude Code, and now has a long session to read.
    generate(&CorpusSpec::new(Agent::Codex, 1, 40_000, 8), &codex).unwrap();

    let daemon = Daemon::spawn(&home, &db, BOTH);
    let reading = daemon.wait("codex catch-up after claude-code's", |status| {
        let rows = status["daemon"]["ingest"]["runtimes"].as_array().cloned();
        let row = |kind: &str| {
            rows.iter()
                .flatten()
                .find(|row| row["kind"] == kind)
                .cloned()
                .unwrap_or_default()
        };
        row("claude-code")["state"] == "done"
            && row("codex")["state"] == "streaming"
            && row("codex")["bytes_streamed"].as_u64().unwrap_or(0) > 0
    });
    assert_eq!(run(&reading)["state"], "running", "{reading}");
    // A cross-launch resume opens nothing before it publishes, and says so.
    for row in reading["daemon"]["ingest"]["runtimes"].as_array().unwrap() {
        assert_eq!(row["admission_course"], "at_publication", "{row}");
    }
    assert_eq!(daemon.post(&reading, "pause", 1), 202);
    let paused = daemon.wait("pause", |status| run(status)["state"] == "paused");
    assert_eq!(paused["canonical_ready"], false, "{paused}");
    for project in paused["daemon"]["ingest"]["projects"].as_array().unwrap() {
        assert_eq!(project["sessions_done"], 0, "{project}");
    }
    let listed = daemon.get("/api/sessions?limit=500");
    for session in listed["sessions"].as_array().unwrap() {
        assert_eq!(session["stream_state"], "pending", "{session}");
    }
    for id in &claude_sessions {
        let page = daemon.get(&format!("/api/sessions/{id}/timeline?limit=500"));
        assert_eq!(
            page["entries"],
            json!([]),
            "{id} is readable before publication"
        );
    }

    daemon.resume_to_publication(&paused, 2);
    drop(daemon);
    assert!(!staged.exists());
    let clean_db = temp.path().join("clean/rosetta.db");
    serve_once(&home, &clean_db, BOTH);
    let resumed = canonical_rows(&db);
    for (table, rows) in canonical_rows(&clean_db) {
        assert_eq!(resumed[&table], rows, "{table}");
    }
}

/// Rebuild the published archive's ledger into a sibling projection.
fn rebuild(db: &Path, into: &Path) {
    let output = Command::new(env!("CARGO_BIN_EXE_rosettad"))
        .args(["rebuild", "--db", into.to_str().unwrap(), "--ledger"])
        .arg(db.with_extension("ledger"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// An interrupted source whose file vanished while no daemon ran no longer
/// refuses the resume. It closes like a file a fresh build loses mid-read:
/// the start reports a missing source, the staged prefix activates at its
/// generation and is sealed, its session publishes Streaming with those
/// rows, every other session publishes as a clean build of what remains, and
/// the published archive rebuilds to the same projection.
#[test]
fn a_resumed_start_tolerates_an_interrupted_source_that_vanished() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let claude = claude_home(&home);
    let db = temp.path().join("archive/rosetta.db");
    serve_once(&home, &db, CLAUDE);
    let runtime = column(&db, "SELECT DISTINCT runtime_id FROM session")
        .pop()
        .unwrap();
    let staged = stage_ledger(&db, &[("claude-code", &claude)]);
    // Persist exactly the first of two native records, at the same durable
    // boundary as a process killed before the generation seal. Polling a
    // progress counter can race the seal on a fast disk.
    let relative = format!("{TOOL_PROJECT}/{LONG_SESSION}.jsonl");
    let first = message("prefix", "captured").replace("heal-session", LONG_SESSION);
    let second = message("suffix", "still unread").replace("heal-session", LONG_SESSION);
    let long = write_fixture(&claude, &relative, &(first.clone() + &second));
    let source = rosetta_capture::SourceDef {
        relative_path: relative.clone(),
        stream_id: "transcript".into(),
        session_id: rosetta_core::ids::session_id(&runtime, LONG_SESSION),
        native_session_id: LONG_SESSION.into(),
        agent_version: Some("2.1.241".into()),
        ledger_only: false,
    };
    let mut ledger = rosetta_capture::Ledger::open(&staged.join("claude-code")).unwrap();
    ledger
        .append_raw(&source, 1, [(0, first.trim_end())])
        .unwrap();
    ledger.flush().unwrap();
    drop(ledger);
    let entries = scan_ledger(&staged.join("claude-code")).unwrap().entries;
    let generations = entries
        .iter()
        .filter_map(|entry| match entry {
            LedgerEntry::Raw {
                source, generation, ..
            } if source.relative_path == relative => Some(*generation),
            _ => None,
        })
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        generations.len(),
        1,
        "the interrupted source left raw evidence of one generation"
    );
    let generation = *generations.first().unwrap();
    assert!(
        !entries.iter().any(|entry| matches!(entry,
            LedgerEntry::GenerationComplete { source, .. } if source.relative_path == relative)),
        "and no seal"
    );
    std::fs::remove_file(&long).unwrap();

    let stdout = serve_once(&home, &db, CLAUDE);
    let tolerated = stdout
        .lines()
        .filter(|line| line.contains("tolerated per-file imperfections"))
        .collect::<Vec<_>>();
    assert_eq!(tolerated.len(), 1, "{stdout:?}");
    assert!(tolerated[0].contains("missing=1"), "{}", tolerated[0]);
    assert!(!staged.exists());

    let session = |sql: &str| column(&db, &sql.replace("{native}", LONG_SESSION));
    assert_eq!(
        session("SELECT stream_state FROM session WHERE native_session_id = '{native}'"),
        ["streaming"]
    );
    let messages: usize = session(
        "SELECT CAST(COUNT(*) AS TEXT) FROM message m JOIN session s ON s.id = m.session_id
         WHERE s.native_session_id = '{native}'",
    )[0]
    .parse()
    .unwrap();
    assert_eq!(messages, 1, "only the staged prefix is published");
    assert_eq!(
        column(
            &db,
            &format!(
                "SELECT active_generation || '/' || highest_generation FROM ingest_cursor
                 WHERE path = '{relative}'"
            )
        ),
        [format!("{generation}/{generation}")],
        "the prefix activates at the generation it was captured under"
    );
    let published = scan_ledger(&db.with_extension("ledger").join("claude-code"))
        .unwrap()
        .entries;
    assert!(
        published.iter().any(|entry| matches!(entry,
            LedgerEntry::GenerationComplete { source, generation: sealed, .. }
                if source.relative_path == relative && *sealed == generation)),
        "and its seal is in the published ledger"
    );

    let clean_db = temp.path().join("clean/rosetta.db");
    serve_once(&home, &clean_db, CLAUDE);
    let long_id = session("SELECT id FROM session WHERE native_session_id = '{native}'")
        .pop()
        .unwrap();
    let resumed = canonical_rows(&db);
    for (table, rows) in canonical_rows(&clean_db) {
        let kept = resumed[&table]
            .iter()
            .filter(|row| !row.contains(&long_id) && !row.contains(LONG_SESSION))
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(kept, rows, "{table}");
    }
    assert!(tool_rows(&db) > 0);

    // The archive stays rebuildable, to the same rows. Rebuild sees only a
    // sealed clean generation, so the cut-short session reads Indexed there,
    // as it does after a fresh build that lost a file mid-read.
    let rebuilt_db = temp.path().join("rebuilt/rosetta.db");
    rebuild(&db, &rebuilt_db);
    let resumed_state = format!(
        "stream_state={:?}",
        rusqlite::types::Value::Text("streaming".into())
    );
    let rebuilt_state = format!(
        "stream_state={:?}",
        rusqlite::types::Value::Text("indexed".into())
    );
    let mut rebuilt = canonical_rows(&rebuilt_db);
    for row in rebuilt.get_mut("session").unwrap() {
        if row.contains(&long_id) {
            assert!(row.contains(&rebuilt_state), "{row}");
            *row = row.replace(&rebuilt_state, &resumed_state);
        }
    }
    rebuilt.get_mut("session").unwrap().sort();
    for (table, rows) in resumed {
        assert_eq!(rebuilt[&table], rows, "{table}");
    }
}

#[test]
fn codex_catch_up_after_ledger_replay_counts_each_request_once() {
    let temp = tempfile::TempDir::new().unwrap();
    let home = temp.path().join("home");
    let root = expand_data_root(&CodexAdapter.layout().data_root, &home);
    generate(&CorpusSpec::new(Agent::Codex, 4, 1200, 118), &root).unwrap();
    let db = temp.path().join("archive/rosetta.db");
    serve_once(&home, &db, &["codex"]);
    let rows = canonical_rows(&db);
    assert!(!rows["session_usage"].is_empty());
    let positions = "SELECT path || '/' || json_extract(checkpoint, '$.next_ordinal') FROM ingest_cursor ORDER BY path";
    let ordinals = column(&db, positions);

    stage_ledger(&db, &[("codex", &root)]);
    let daemon = Daemon::spawn(&home, &db, &["codex"]);
    let published = daemon.wait("restored import counters", |status| {
        status["daemon"]["ingest"]["phase"] == "live" && status["canonical_ready"] == true
    });
    let row = &published["daemon"]["ingest"]["runtimes"][0];
    assert_eq!(
        row["records"], 1200,
        "the import sign includes the verified source prefix"
    );
    assert!(row["bytes_streamed"].as_u64().unwrap() > 0);
    drop(daemon);
    assert_eq!(ordinals, column(&db, positions));
    let raw_count = scan_ledger(&db.with_extension("ledger").join("codex"))
        .unwrap()
        .entries
        .into_iter()
        .filter(|entry| matches!(entry, LedgerEntry::Raw { .. }))
        .count();
    assert_eq!(raw_count, 1200, "resume re-appended sealed raw records");
    let resumed = canonical_rows(&db);
    for (table, expected) in rows {
        assert_eq!(resumed[&table], expected, "{table}");
    }
}

#[test]
fn a_transcript_shortened_while_stopped_replaces_its_replayed_generation() {
    let temp = tempfile::TempDir::new().unwrap();
    let home = temp.path().join("home");
    let root = expand_data_root(&CodexAdapter.layout().data_root, &home);
    generate(&CorpusSpec::new(Agent::Codex, 2, 400, 118), &root).unwrap();
    let db = temp.path().join("archive/rosetta.db");
    serve_once(&home, &db, &["codex"]);
    let file = root.join(&column(&db, "SELECT path FROM ingest_cursor ORDER BY path LIMIT 1")[0]);
    stage_ledger(&db, &[("codex", &root)]);
    let data = std::fs::read(&file).unwrap();
    let start = data
        .iter()
        .take(data.len() / 2)
        .rposition(|b| *b == b'\n')
        .unwrap();
    std::fs::write(file, &data[..=start]).unwrap();
    let clean = temp.path().join("clean/rosetta.db");
    serve_once(&home, &clean, &["codex"]);
    serve_once(&home, &db, &["codex"]);
    let resumed = canonical_rows(&db);
    for (table, expected) in canonical_rows(&clean) {
        assert_eq!(resumed[&table], expected, "{table}");
    }
}

fn message(id: &str, content: &str) -> String {
    format!(
        "{}\n",
        json!({
            "type":"user","isSidechain":false,"parentUuid":null,"userType":"external",
            "cwd":"/w/recovery","sessionId":"heal-session","version":"2.1.241","gitBranch":"main",
            "timestamp":"2026-08-25T02:35:19.054Z","uuid":id,"message":{"role":"user","content":content}
        })
    )
}

fn write_fixture(root: &Path, path: &str, contents: &str) -> PathBuf {
    let file = root.join(path);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, contents).unwrap();
    file
}

const MESSAGES: &str = "SELECT coalesce(content, '') FROM message ORDER BY content";
const GENERATIONS: &str =
    "SELECT active_generation || '/' || highest_generation FROM ingest_cursor";

#[cfg(unix)]
#[test]
fn quit_during_a_large_live_append_stops_at_a_committed_batch_and_resumes() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let db = temp.path().join("archive/rosetta.db");
    let file = write_fixture(
        &home.join(".claude"),
        "projects/a/heal-session.jsonl",
        &message("0", "0"),
    );
    serve_once(&home, &db, CLAUDE);
    let total = 25_000;
    {
        let mut writer =
            std::io::BufWriter::new(std::fs::OpenOptions::new().append(true).open(file).unwrap());
        for i in 1..total {
            let id = i.to_string();
            writer.write_all(message(&id, &id).as_bytes()).unwrap();
        }
        writer.flush().unwrap();
    }
    let count = |conn: &rusqlite::Connection| {
        conn.query_row("SELECT COUNT(*) FROM message", [], |r| r.get::<_, i64>(0))
            .unwrap()
    };
    let reader =
        rusqlite::Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    let mut daemon = Daemon::spawn(&home, &db, CLAUDE);
    let deadline = Instant::now() + Duration::from_secs(45);
    while count(&reader) == 1 {
        assert!(
            Instant::now() < deadline,
            "waiting for the first append commit"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        Command::new("kill")
            .args(["-INT", &daemon.child.id().to_string()])
            .status()
            .unwrap()
            .success()
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while daemon.child.try_wait().unwrap().is_none() {
        assert!(
            Instant::now() < deadline,
            "the app would have forced this daemon to quit"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(daemon.child.wait().unwrap().success());
    assert!(
        count(&reader) < total,
        "Quit drained the entire native append"
    );
    drop(daemon);
    drop(reader);
    serve_once(&home, &db, CLAUDE);
    let rebuilt = temp.path().join("rebuilt/rosetta.db");
    rebuild(&db, &rebuilt);
    for projection in [&db, &rebuilt] {
        assert_eq!(
            count(&rusqlite::Connection::open(projection).unwrap()),
            total
        );
        assert_eq!(
            column(
                projection,
                "SELECT CAST(COUNT(DISTINCT content) AS TEXT) FROM message"
            ),
            [total.to_string()]
        );
    }
}

#[test]
fn a_replacement_restores_a_shared_message_from_its_remaining_source() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let root = home.join(".claude");
    for name in ["original", "copy"] {
        write_fixture(
            &root,
            &format!("projects/{name}/heal-session.jsonl"),
            &(message(name, name) + &message("shared", "shared")),
        );
    }
    let db = temp.path().join("archive/rosetta.db");
    serve_once(&home, &db, CLAUDE);
    let owner = column(&db,"SELECT c.path FROM message m JOIN ingest_cursor c ON c.id=m.src_cursor WHERE m.content='shared'").pop().unwrap();
    let name = Path::new(&owner)
        .parent()
        .unwrap()
        .file_name()
        .unwrap()
        .to_str()
        .unwrap();
    stage_ledger(&db, &[("claude-code", &root)]);
    write_fixture(
        &root,
        &owner,
        &(message(name, name) + &message("new", "new")),
    );
    let clean = temp.path().join("clean/rosetta.db");
    serve_once(&home, &clean, CLAUDE);
    let owners = "SELECT m.content || ':' || c.path FROM message m JOIN ingest_cursor c ON c.id=m.src_cursor ORDER BY m.content";
    assert_eq!(
        column(&clean, MESSAGES),
        ["copy", "new", "original", "shared"]
    );
    for _ in 0..2 {
        serve_once(&home, &db, CLAUDE);
        assert_eq!(column(&db, owners), column(&clean, owners));
    }
    let rebuilt = temp.path().join("rebuilt/rosetta.db");
    rebuild(&db, &rebuilt);
    assert_eq!(column(&rebuilt, owners), column(&clean, owners));
}

#[test]
fn conflicting_sibling_messages_keep_the_fresh_imports_owner_and_value() {
    for changed in ["a", "b", "both", "new-a"] {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let root = home.join(".claude");
        let db = temp.path().join("archive/rosetta.db");
        let put = |name: &str, value: &str| {
            write_fixture(
                &root,
                &format!("projects/{name}/heal-session.jsonl"),
                &(message(name, name) + &message("shared", value)),
            );
        };
        if changed != "new-a" {
            put("a", "a-old");
        }
        put("b", "b-old");
        serve_once(&home, &db, CLAUDE);
        stage_ledger(&db, &[("claude-code", &root)]);
        if matches!(changed, "a" | "both" | "new-a") {
            put("a", "a-new");
        }
        if matches!(changed, "b" | "both") {
            put("b", "b-new");
        }
        let clean = temp.path().join("clean/rosetta.db");
        let rebuilt = temp.path().join("rebuilt/rosetta.db");
        serve_once(&home, &db, CLAUDE);
        serve_once(&home, &clean, CLAUDE);
        rebuild(&db, &rebuilt);
        let owners = "SELECT m.content || ':' || c.path FROM message m JOIN ingest_cursor c ON c.id=m.src_cursor ORDER BY m.content";
        assert_eq!(column(&db, owners), column(&clean, owners), "{changed}");
        assert_eq!(
            column(&rebuilt, owners),
            column(&clean, owners),
            "{changed} archive"
        );
    }
}

#[test]
fn a_corrected_source_uses_a_generation_above_the_rejected_seal() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let root = home.join(".claude");
    let db = temp.path().join("archive/rosetta.db");
    for (i, text) in ["A", "B", "C"].into_iter().enumerate() {
        if i > 0 {
            stage_ledger(&db, &[("claude-code", &root)]);
        }
        let mut contents = message("header", "header") + &message(text, text);
        if i == 1 {
            contents.push_str("{bad-record}\n");
        }
        write_fixture(&root, "projects/a/heal-session.jsonl", &contents);
        serve_once(&home, &db, CLAUDE);
        let generation = i + 1;
        assert_eq!(
            column(&db, GENERATIONS),
            [format!("{generation}/{generation}")]
        );
        assert_eq!(column(&db, MESSAGES), [text, "header"]);
        assert_eq!(
            column(&db, "SELECT stream_state FROM session"),
            [if i == 1 { "streaming" } else { "indexed" }]
        );
    }
    let rebuilt = temp.path().join("rebuilt/rosetta.db");
    rebuild(&db, &rebuilt);
    assert_eq!(column(&rebuilt, MESSAGES), ["C", "header"]);
    assert_eq!(column(&rebuilt, GENERATIONS), ["3/3"]);
}

#[test]
fn rebuild_recognizes_a_clean_restart_of_a_legacy_reused_generation() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let root = home.join(".claude");
    let db = temp.path().join("archive/rosetta.db");
    write_fixture(&root, "projects/a/heal-session.jsonl", &message("A", "A"));
    serve_once(&home, &db, CLAUDE);
    let path = db.with_extension("ledger").join("claude-code");
    let (source, version) = scan_ledger(&path)
        .unwrap()
        .entries
        .into_iter()
        .find_map(|entry| match entry {
            LedgerEntry::GenerationComplete {
                source,
                normalizer_version,
                ..
            } => Some((source, normalizer_version)),
            _ => None,
        })
        .unwrap();
    let mut ledger = rosetta_capture::Ledger::open(&path).unwrap();
    let b = message("B", "B");
    ledger
        .append_raw(
            &source,
            2,
            [(0, b.trim_end()), (b.len() as u64, "{broken}")],
        )
        .unwrap();
    ledger
        .complete_generation(&source, 2, version, false)
        .unwrap();
    // A second crash can leave a prefix under the same reused number.
    ledger
        .append_raw(
            &source,
            2,
            [(0, message("abandoned", "abandoned").trim_end())],
        )
        .unwrap();
    ledger
        .append_raw(&source, 2, [(0, message("C", "C").trim_end())])
        .unwrap();
    ledger
        .complete_generation(&source, 2, version, true)
        .unwrap();
    ledger.flush().unwrap();
    drop(ledger);
    let rebuilt = temp.path().join("rebuilt/rosetta.db");
    rebuild(&db, &rebuilt);
    assert_eq!(column(&rebuilt, MESSAGES), ["C"]);
    assert_eq!(column(&rebuilt, GENERATIONS), ["2/2"]);
}

#[test]
fn a_missing_unfinished_journal_holds_readiness_until_it_returns() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let root = home.join(".claude");
    let db = temp.path().join("archive/rosetta.db");
    let transcript = write_fixture(
        &root,
        "projects/a/heal-session.jsonl",
        &message("m", "hello"),
    );
    let journal = write_fixture(
        &root,
        "projects/a/heal-session/subagents/workflows/wf_heal/journal.jsonl",
        "{\"type\":\"started\",\"key\":\"worker\",\"agentId\":\"agent-1\"}\n",
    );
    let state = || {
        column(
            &db,
            "SELECT stream_state FROM session WHERE native_session_id='heal-session'",
        )
    };
    serve_once(&home, &db, CLAUDE);
    assert_eq!(state(), ["indexed"]);
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&journal)
        .unwrap();
    file.write_all(br#"{"type":"result","key":"worker""#)
        .unwrap();
    drop(file);
    serve_once(&home, &db, CLAUDE);
    assert_eq!(state(), ["streaming"]);
    let offline = temp.path().join("offline");
    std::fs::rename(&journal, &offline).unwrap();
    std::fs::write(
        &transcript,
        message("m", "hello") + &message("n", "changed sibling"),
    )
    .unwrap();
    for _ in 0..2 {
        serve_once(&home, &db, CLAUDE);
        assert_eq!(state(), ["streaming"]);
        assert_eq!(column(&db, MESSAGES), ["changed sibling", "hello"]);
    }
    std::fs::rename(offline, &journal).unwrap();
    writeln!(
        std::fs::OpenOptions::new()
            .append(true)
            .open(journal)
            .unwrap(),
        ",\"result\":{{\"ok\":true}}}}"
    )
    .unwrap();
    serve_once(&home, &db, CLAUDE);
    assert_eq!(state(), ["indexed"]);
}

#[test]
fn restoring_unchanged_sqlite_snapshots_preserves_raw_records_and_import_progress() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let root = home.join(".local/share/opencode");
    std::fs::create_dir_all(&root).unwrap();
    let native = root.join("opencode.db");
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../groundtruth/opencode/fixtures/1.18.22/tool-use/opencode-home/opencode.db"),
        &native,
    )
    .unwrap();
    let db = temp.path().join("archive/rosetta.db");
    let entries = || {
        scan_ledger(&db.with_extension("ledger").join("opencode"))
            .unwrap()
            .entries
    };
    serve_once(&home, &db, &["opencode"]);
    let original = column(&db, MESSAGES);
    assert_eq!(original.len(), 3);
    let raw = entries()
        .into_iter()
        .filter_map(|entry| match entry {
            LedgerEntry::Raw { payload, .. } => Some(payload),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(raw.len(), 7);
    for _ in 0..2 {
        stage_ledger(&db, &[("opencode", &root)]);
        let daemon = Daemon::spawn(&home, &db, &["opencode"]);
        let published = daemon.wait("restored snapshot", |s| {
            s["canonical_ready"] == true && s["daemon"]["ingest"]["phase"] == "live"
        });
        let status = &published["daemon"]["ingest"]["runtimes"][0];
        assert_eq!(status["records"], raw.len());
        assert_eq!(
            status["bytes_streamed"],
            raw.iter().map(Vec::len).sum::<usize>()
        );
        drop(daemon);
        assert_eq!(column(&db, MESSAGES), original);
        assert_eq!(column(&db, GENERATIONS), ["1/1"]);
        assert_eq!(
            entries()
                .into_iter()
                .filter(|e| matches!(e, LedgerEntry::Raw { .. }))
                .count(),
            raw.len()
        );
    }
    let native = rusqlite::Connection::open(&native).unwrap();
    let changed = native.execute("UPDATE part SET data=json_set(data,'$.text','snapshot edit') WHERE json_extract(data,'$.type')='text'", []).unwrap();
    assert!(changed > 0);
    drop(native);
    stage_ledger(&db, &[("opencode", &root)]);
    serve_once(&home, &db, &["opencode"]);
    assert_eq!(column(&db, GENERATIONS), ["2/2"]);
    let clean = temp.path().join("clean/rosetta.db");
    let rebuilt = temp.path().join("rebuilt/rosetta.db");
    serve_once(&home, &clean, &["opencode"]);
    rebuild(&db, &rebuilt);
    assert_ne!(original, column(&db, MESSAGES));
    assert_eq!(column(&db, MESSAGES), column(&clean, MESSAGES));
    assert_eq!(column(&db, MESSAGES), column(&rebuilt, MESSAGES));
}

#[test]
fn restores_document_snapshots_including_a_seal_with_no_records() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let root = home.join(".claude");
    let db = temp.path().join("archive/rosetta.db");
    write_fixture(
        &root,
        "projects/a/heal-session.jsonl",
        &message("main", "main"),
    );
    write_fixture(
        &root,
        "projects/a/heal-session/subagents/agent-child.meta.json",
        r#"{"agentType":"Explore"}"#,
    );
    write_fixture(
        &root,
        "projects/a/heal-session/workflows/wf_review.json",
        r#"{"runId":"wf_review","workflowName":"Review","status":"completed","summary":"done"}"#,
    );
    serve_once(&home, &db, CLAUDE);
    let generations = "SELECT stream || ':' || active_generation FROM ingest_cursor WHERE stream IN ('subagent-metadata','workflow-run') ORDER BY stream";
    let expected = ["subagent-metadata:1", "workflow-run:1"];
    assert_eq!(column(&db, generations), expected);
    let capture = || {
        scan_ledger(&db.with_extension("ledger").join("claude-code"))
            .unwrap()
            .entries
            .into_iter()
            .filter_map(|e| match e {
                LedgerEntry::Raw { source, .. }
                    if source.stream_id == "subagent-metadata"
                        || source.stream_id == "workflow-run" =>
                {
                    Some(source.stream_id)
                }
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(capture(), ["workflow-run"]);
    for _ in 0..2 {
        stage_ledger(&db, &[("claude-code", &root)]);
        serve_once(&home, &db, CLAUDE);
        assert_eq!(column(&db, generations), expected);
        assert_eq!(capture(), ["workflow-run"]);
    }
}
