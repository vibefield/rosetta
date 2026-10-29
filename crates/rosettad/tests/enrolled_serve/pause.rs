use super::*;
use serde_json::{Value, json};

fn transcript(root: &Path) -> PathBuf {
    for entry in std::fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            if let Some(path) = find_transcript(&path) {
                return path;
            }
        } else if path.extension().is_some_and(|ext| ext == "jsonl") {
            return path;
        }
    }
    panic!("no transcript")
}
fn find_transcript(root: &Path) -> Option<PathBuf> {
    for entry in std::fs::read_dir(root).ok()? {
        let path = entry.ok()?.path();
        if path.is_dir() {
            if let Some(path) = find_transcript(&path) {
                return Some(path);
            }
        } else if path.extension().is_some_and(|ext| ext == "jsonl") {
            return Some(path);
        }
    }
    None
}
fn append(path: &Path, id: &str, text: &str) {
    let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    writeln!(
        file,
        "{}",
        json!({"type":"user", "isSidechain":false, "version":"2.1.241", "userType":"external", "gitBranch":"HEAD", "uuid":id, "parentUuid":null,
        "sessionId":NATIVE_SESSION, "cwd":"/work/pause", "timestamp":"2026-10-29T12:00:00Z",
        "message":{"role":"user","content":text}})
    )
    .unwrap();
}
fn large_history(bench: &Bench) -> PathBuf {
    let path = transcript(&bench.home.join(".claude"));
    let mut file = std::io::BufWriter::new(
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap(),
    );
    let text = "pause recovery fixture ".repeat(40);
    for i in 0..10_000 {
        writeln!(
            file,
            "{}",
            json!({"type":"user", "isSidechain":false, "version":"2.1.241", "userType":"external", "gitBranch":"HEAD", "uuid":format!("pause-{i}"), "parentUuid":null,
            "sessionId":NATIVE_SESSION,"cwd":"/work/pause", "timestamp":"2026-10-29T11:00:00Z",
            "message":{"role":"user","content":text}})
        )
        .unwrap();
    }
    file.flush().unwrap();
    path
}

fn post(origin: &str, run: &Value, action: &str, key: usize) -> (u16, Value) {
    let address = origin.strip_prefix("http://").unwrap();
    let path = format!("/api/ingestions/{}/{action}", run["id"].as_str().unwrap());
    let body = json!({"expected_revision":run["revision"]}).to_string();
    let mut stream = TcpStream::connect(address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(15)))
        .unwrap();
    write!(stream, "POST {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\nContent-Type: application/json\r\nX-Rosetta-Index-Id: {}\r\nIdempotency-Key: 00000000-0000-4000-8000-{key:012}\r\nContent-Length: {}\r\n\r\n{body}", run["index_id"].as_str().unwrap(), body.len()).unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    let (head, body) = response.split_once("\r\n\r\n").unwrap();
    assert!(
        head.to_ascii_lowercase()
            .contains("cache-control: no-store"),
        "{head}"
    );
    (
        head.split_whitespace().nth(1).unwrap().parse().unwrap(),
        serde_json::from_str(body).unwrap(),
    )
}
fn wait(origin: &str, predicate: impl Fn(&Value) -> bool) -> Value {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let status = http_get(origin, "/api/status");
        if predicate(&status) {
            return status;
        }
        assert!(Instant::now() < deadline, "status timed out: {status}");
        assert_ne!(
            status["daemon"]["ingest"]["run"]["state"], "failed",
            "{status}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}
fn run(status: &Value) -> &Value {
    &status["daemon"]["ingest"]["run"]
}

fn evidence(root: &Path) -> Vec<(PathBuf, u64)> {
    let mut out = Vec::new();
    if root.exists() {
        for entry in std::fs::read_dir(root).unwrap() {
            let entry = entry.unwrap();
            if entry.path().is_dir() {
                out.extend(evidence(&entry.path()));
            } else {
                out.push((entry.path(), entry.metadata().unwrap().len()));
            }
        }
    }
    out.sort();
    out
}

#[test]
fn cold_pause_stops_capture_keeps_reads_and_resumes_the_same_run() {
    for workers in ["0", "2"] {
        let bench = Bench::new();
        let source = large_history(&bench);
        let (child, origin) = spawn_serve_with_flags(&bench, &["--capture-workers", workers]);
        let before = wait(&origin, |s| {
            s["daemon"]["ingest"]["metadata_ready_at"].is_i64()
                && s["daemon"]["ingest"]["runtimes"]
                    .as_array()
                    .is_some_and(|rows| {
                        rows.iter()
                            .any(|r| r["bytes_streamed"].as_u64().unwrap_or(0) > 0)
                    })
        });
        assert_eq!(
            run(&before)["state"],
            "running",
            "fixture finished before pause: {before}"
        );
        let (code, accepted) = post(&origin, run(&before), "pause", 1);
        assert_eq!(code, 202, "{accepted}");
        assert_eq!(accepted["state"], "pausing");
        let paused = wait(&origin, |s| run(s)["state"] == "paused");
        assert!(
            !bench.db.exists(),
            "an accepted pause must prevent publication"
        );
        assert_eq!(
            paused["daemon"]["ingest"]["metadata_ready_at"],
            before["daemon"]["ingest"]["metadata_ready_at"]
        );
        let sessions = http_get(&origin, "/api/sessions");
        assert!(!sessions["sessions"].as_array().unwrap().is_empty());
        let retained = evidence(&bench.staged_ledger());
        assert!(!retained.is_empty());
        append(&source, "written-while-paused", "written while paused");
        // Watcher queues are disposable. A new source must be found by the
        // resume inventory, both in-process and after a supervisor restart.
        let added_id = "aaaaaaaa-bbbb-4000-8000-cccccccccccc";
        let added = source.with_file_name(format!("{added_id}.jsonl"));
        let original = std::fs::read_to_string(transcript(&fixture("basic-chat"))).unwrap();
        std::fs::write(&added, original.replace(NATIVE_SESSION, added_id)).unwrap();
        std::thread::sleep(Duration::from_millis(250));
        assert_eq!(
            retained,
            evidence(&bench.staged_ledger()),
            "paused means no further ledger writes"
        );
        assert_eq!(
            post(&origin, run(&before), "pause", 1).0,
            200,
            "lost replies are replayed before revisions"
        );
        assert_eq!(post(&origin, run(&before), "resume", 2).0, 409);

        let (child, origin, paused) = if workers == "2" {
            drop(child);
            bench.write_enrollment(&["claude-code"], false);
            let (child, origin) = spawn_serve(&bench);
            let recovered = http_get(&origin, "/api/status");
            assert_eq!(run(&recovered)["id"], run(&paused)["id"]);
            assert_eq!(run(&recovered)["state"], "paused");
            std::thread::sleep(Duration::from_millis(150));
            assert_eq!(retained, evidence(&bench.staged_ledger()));
            (child, origin, recovered)
        } else {
            (child, origin, paused)
        };
        let (code, resumed) = post(&origin, run(&paused), "resume", 3);
        assert_eq!(code, 202, "{resumed}");
        assert_eq!(resumed["id"], run(&paused)["id"]);
        assert_ne!(resumed["progress_epoch"], run(&paused)["progress_epoch"]);
        let done = wait(&origin, |s| {
            s["canonical_ready"] == true && run(s)["state"] == "succeeded"
        });
        if workers == "0" {
            assert_eq!(
                done["daemon"]["ingest"]["metadata_ready_at"],
                before["daemon"]["ingest"]["metadata_ready_at"]
            );
        }
        let conn = rusqlite::Connection::open(&bench.db).unwrap();
        assert!(bench.has_session(added_id));
        if workers == "2" {
            assert!(
                !bench.has_session(KIM_SESSION),
                "restart applies revoked enrollment before resuming capture"
            );
            assert_eq!(run(&done)["runtime_ids"], json!([bench.native_id()]));
        }
        let retained_messages: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM message WHERE content LIKE 'pause recovery fixture %'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            retained_messages, 10_000,
            "resume neither loses nor duplicates captured messages"
        );
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM message WHERE content = 'written while paused'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
        assert_eq!(post(&origin, run(&done), "pause", 4).0, 409);
        let detail = http_get(
            &origin,
            &format!("/api/ingestions/{}", run(&done)["id"].as_str().unwrap()),
        );
        assert_eq!(detail["state"], "succeeded");
        drop(child);
    }
}

#[test]
fn disk_progressive_pause_retains_cursors_and_finishes_after_resume() {
    let bench = Bench::new();
    let source = large_history(&bench);
    let (metadata, origin) = spawn_serve_with_flags(&bench, &["--stop-after-meta"]);
    wait(&origin, |s| {
        s["daemon"]["ingest"]["metadata_ready_at"].is_i64()
    });
    drop(metadata);
    let (_child, origin) = spawn_serve_with_flags(&bench, &["--slice-budget", "32768"]);
    let before = wait(&origin, |s| {
        s["daemon"]["ingest"]["runtimes"]
            .as_array()
            .is_some_and(|rows| {
                rows.iter()
                    .any(|r| r["bytes_streamed"].as_u64().unwrap_or(0) > 0)
            })
    });
    assert_eq!(post(&origin, run(&before), "pause", 10).0, 202);
    let paused = wait(&origin, |s| run(s)["state"] == "paused");
    let retained = evidence(&bench.db.with_extension("ledger"));
    append(&source, "disk-while-paused", "disk written while paused");
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(retained, evidence(&bench.db.with_extension("ledger")));
    assert_eq!(post(&origin, run(&paused), "resume", 11).0, 202);
    wait(&origin, |s| {
        s["canonical_ready"] == true && run(s)["state"] == "succeeded"
    });
    let conn = rusqlite::Connection::open(&bench.db).unwrap();
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM message WHERE content = 'disk written while paused'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
}
