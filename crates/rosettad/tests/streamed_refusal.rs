//! R1 brief §10 test 8, refusal, on the real daemon: the oldest tree holds a
//! record past the raw frame limit, so the fresh build refuses — after the
//! newer trees have flipped. The refusal installs nothing and the run parks
//! failed, yet the RAM generation's keeper stays alive behind the read
//! router: every tree that flipped earlier is still served, its full
//! command still read from the staged raw ledger, and the refused tree
//! stays hidden. (Mutation: a build that publishes despite its failures
//! installs a projection, and the test fails.)

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

mod support;

use serde_json::Value;
use support::fixtures::{ClaudeTranscript, T0, claude_root, filler};

const HOUR: i64 = 3_600_000;
/// The raw ledger's frame limit less a raw frame's fixed header
/// (`rosetta_capture::ledger::MAX_RAW_RECORD_BYTES`).
const MAX_RAW_RECORD_BYTES: usize = 256 * 1024 * 1024 - 20;

struct Daemon {
    child: Child,
    origin: String,
    lines: mpsc::Receiver<String>,
    log: Vec<String>,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Daemon {
    fn spawn(home: &Path, db: &Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_rosettad"))
            .args(["serve", "--db", db.to_str().unwrap(), "--home"])
            .arg(home)
            .args(["--agent", "claude-code", "--addr", "127.0.0.1:0"])
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
        let mut log = Vec::new();
        let origin = loop {
            let line = lines.recv_timeout(Duration::from_secs(30)).unwrap();
            let origin = line
                .split_whitespace()
                .find(|word| word.starts_with("http://"))
                .map(str::to_owned);
            log.push(line);
            if let Some(origin) = origin {
                break origin;
            }
        };
        Self {
            child,
            origin,
            lines,
            log,
        }
    }

    fn stdout(&mut self) -> String {
        self.log.extend(self.lines.try_iter());
        self.log.join("\n")
    }

    fn get(&self, path: &str) -> (u16, Value) {
        let address = self.origin.strip_prefix("http://").unwrap();
        let mut stream = TcpStream::connect(address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(20)))
            .unwrap();
        write!(
            stream,
            "GET {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        let status = response
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse().ok())
            .unwrap_or(0);
        let body = response
            .split_once("\r\n\r\n")
            .map(|(_, body)| body)
            .unwrap_or_default();
        (status, serde_json::from_str(body).unwrap_or(Value::Null))
    }
}

/// `alpha`, the newest tree, runs a long shell command; three middle trees;
/// the oldest tree `refused` ends in one record past the raw frame limit.
fn home(home: &Path) -> String {
    let projects = claude_root(home).join("projects");
    let command = format!("echo {}", filler("full", 0, 3_000));
    let mut alpha = ClaudeTranscript::new("alpha", None, T0);
    alpha
        .prompt("run it")
        .tool(&command, "done")
        .reply("ran it");
    alpha.write(&projects.join("-w-alpha/alpha.jsonl"), T0 + 100 * HOUR);
    for index in 0..3 {
        let id = format!("middle{index}");
        let mut middle = ClaudeTranscript::new(&id, None, T0);
        middle.prompt("a question").bulk(4, 512);
        middle.write(
            &projects.join(format!("-w-{id}/{id}.jsonl")),
            T0 + (50 - index) * HOUR,
        );
    }
    let mut refused = ClaudeTranscript::new("refused", None, T0);
    refused.prompt("an old question").reply("an old answer");
    let path = projects.join("-w-refused/refused.jsonl");
    refused.write(&path, T0);
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    let opening = r#"{"type":"user","sessionId":"refused","attachment":""#;
    let closing = "\"}\n";
    file.write_all(opening.as_bytes()).unwrap();
    let pad = MAX_RAW_RECORD_BYTES + 1 - opening.len() - closing.len() + 1;
    let chunk = vec![b'q'; 1024 * 1024];
    let mut left = pad;
    while left > 0 {
        let take = left.min(chunk.len());
        file.write_all(&chunk[..take]).unwrap();
        left -= take;
    }
    file.write_all(closing.as_bytes()).unwrap();
    drop(file);
    support::fixtures::set_mtime(&path, T0);
    command
}

#[test]
fn a_refused_build_parks_with_its_flipped_trees_still_served() {
    let temp = tempfile::TempDir::new().unwrap();
    let home_dir = temp.path().join("home");
    let command = home(&home_dir);
    let db = temp.path().join("archive/rosetta.db");
    let mut daemon = Daemon::spawn(&home_dir, &db);
    let deadline = Instant::now() + Duration::from_secs(300);
    let status = loop {
        let (_, status) = daemon.get("/api/status");
        if status["daemon"]["ingest"]["run"]["state"] == "failed" {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "the build never refused: {status}\n{}",
            daemon.stdout()
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    let run = &status["daemon"]["ingest"]["run"];
    assert!(
        run["error"].to_string().contains("oversized"),
        "{run}\n{}",
        daemon.stdout()
    );
    // Publication refused: nothing installed, the ledger still staged.
    assert_eq!(status["canonical_ready"], false, "{status}");
    assert!(!db.exists());
    let ledger = db.with_extension("ledger");
    let staged = ledger.with_file_name("rosetta.ledger.staged");
    assert!(staged.is_dir() && !ledger.exists());

    // Parked with the RAM keeper alive: the earlier flips are served, now
    // and a while later; the refused tree is not.
    for round in 0..2 {
        if round == 1 {
            std::thread::sleep(Duration::from_millis(500));
        }
        let (status, sessions) = daemon.get("/api/sessions?limit=500");
        assert_eq!(status, 200);
        let sessions = sessions["sessions"].as_array().unwrap().clone();
        assert_eq!(sessions.len(), 5, "{sessions:?}");
        for session in &sessions {
            let id = session["id"].as_str().unwrap();
            let (status, page) = daemon.get(&format!("/api/sessions/{id}/timeline"));
            assert_eq!(status, 200);
            let entries = page["entries"].as_array().map_or(0, Vec::len);
            let refused = session["native_session_id"] == "refused";
            if refused {
                assert_eq!(session["stream_state"], "pending", "{session}");
                assert_eq!(entries, 0, "the refused tree stays hidden");
            } else {
                assert_ne!(session["stream_state"], "pending", "{session}");
                assert!(entries > 0, "{id} flipped before the refusal");
            }
        }
    }

    // A flipped tree's full command is read from the staged raw ledger.
    let (_, sessions) = daemon.get("/api/sessions?limit=500");
    let alpha = sessions["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|session| session["native_session_id"] == "alpha")
        .expect("alpha listed")["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let (_, page) = daemon.get(&format!("/api/sessions/{alpha}/timeline"));
    let timeline = page["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry.to_string().contains("echo full0"))
        .expect("the shell row")["timeline_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let (status, body) = daemon.get(&format!(
        "/api/sessions/{alpha}/timeline/{timeline}/command"
    ));
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["command"], command.as_str());
}
