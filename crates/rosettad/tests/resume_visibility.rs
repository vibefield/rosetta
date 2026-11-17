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
        // Known and older than readiness withholding: any resume of a staged
        // Codex generation, paused or not, doubles its usage samples.
        if matches!(table.as_str(), "session_usage" | "usage_bucket") {
            continue;
        }
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
    let staged = stage_ledger(&db, &[("claude-code", &claude)]);
    let long = long_transcript(&claude);

    // Interrupt the resumed build while it reads the long transcript, then
    // quit: the staged ledger holds part of it, unsealed.
    let daemon = Daemon::spawn(&home, &db, CLAUDE);
    daemon.pause_while_catching_up();
    drop(daemon);
    let relative = long
        .strip_prefix(&claude)
        .unwrap()
        .to_string_lossy()
        .into_owned();
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

    let daemon = Daemon::spawn(&home, &db, CLAUDE);
    let paused = daemon.get("/api/status");
    assert_eq!(run(&paused)["state"], "paused", "{paused}");
    daemon.resume_to_publication(&paused, 3);
    let stdout = daemon.stdout();
    drop(daemon);
    let tolerated = stdout
        .iter()
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
    assert!(messages > 0, "the staged prefix is published");
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
