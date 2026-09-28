use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use rosetta_capture::{LedgerEntry, scan_ledger};

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

fn run_serve(home: &Path, db: &Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_rosettad"))
        .args([
            "serve",
            "--db",
            db.to_str().unwrap(),
            "--home",
            home.to_str().unwrap(),
            "--agent",
            "claude-code",
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
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let greeting = stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|value| value["event"] == "ready")
        .expect("structured readiness event");
    assert_eq!(
        greeting["protocol_version"],
        rosetta_server::PROTOCOL_VERSION
    );
    assert!(
        greeting["origin"]
            .as_str()
            .unwrap()
            .starts_with("http://127.0.0.1:")
    );
    stdout
}

fn ledger_segments(root: &Path) -> usize {
    std::fs::read_dir(root)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.ends_with(".rledger"))
        })
        .count()
}

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn find_project_jsonl(root: &Path) -> std::path::PathBuf {
    fn find(root: &Path) -> Option<std::path::PathBuf> {
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
    find(root).unwrap_or_else(|| panic!("no project jsonl under {}", root.display()))
}

fn websocket_session(origin: &str, session_id: &str) -> BufReader<TcpStream> {
    let address = origin.strip_prefix("http://").unwrap();
    let stream = TcpStream::connect(address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(45)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut reader = BufReader::new(stream);
    write!(
        reader.get_mut(),
        "GET /ws/sessions/{session_id} HTTP/1.1\r\nHost: {address}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n"
    )
    .unwrap();
    reader.get_mut().flush().unwrap();
    let mut status = String::new();
    reader.read_line(&mut status).unwrap();
    assert!(status.contains("101 Switching Protocols"), "{status}");
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        if line == "\r\n" {
            break;
        }
    }
    reader
}

fn read_ws_text(reader: &mut BufReader<TcpStream>) -> String {
    loop {
        let mut header = [0_u8; 2];
        reader.read_exact(&mut header).unwrap();
        let opcode = header[0] & 0x0f;
        let masked = header[1] & 0x80 != 0;
        let mut length = u64::from(header[1] & 0x7f);
        if length == 126 {
            let mut extended = [0_u8; 2];
            reader.read_exact(&mut extended).unwrap();
            length = u64::from(u16::from_be_bytes(extended));
        } else if length == 127 {
            let mut extended = [0_u8; 8];
            reader.read_exact(&mut extended).unwrap();
            length = u64::from_be_bytes(extended);
        }
        let mask = if masked {
            let mut mask = [0_u8; 4];
            reader.read_exact(&mut mask).unwrap();
            Some(mask)
        } else {
            None
        };
        let mut payload = vec![0_u8; usize::try_from(length).unwrap()];
        reader.read_exact(&mut payload).unwrap();
        if let Some(mask) = mask {
            for (index, byte) in payload.iter_mut().enumerate() {
                *byte ^= mask[index % 4];
            }
        }
        match opcode {
            1 => return String::from_utf8(payload).unwrap(),
            8 => panic!("websocket closed before the expected text frame"),
            _ => continue,
        }
    }
}

#[test]
fn fresh_serve_publishes_metadata_then_installs_valid_projection() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let claude = home.join(".claude");
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../groundtruth/claude-code/fixtures/2.1.241/basic-chat/claude-config");
    copy_tree(&fixture, &claude);
    let db = temp.path().join("archive/projection.sqlite");

    let output = Command::new(env!("CARGO_BIN_EXE_rosettad"))
        .args([
            "serve",
            "--db",
            db.to_str().unwrap(),
            "--home",
            home.to_str().unwrap(),
            "--agent",
            "claude-code",
            "--addr",
            "127.0.0.1:0",
            "--ticks",
            "0",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("metadata public"), "{stdout}");
    assert!(stdout.contains("milestone: metadata-ready"), "{stdout}");
    assert!(stdout.contains("milestone: canonical-ready"), "{stdout}");

    let conn = rusqlite::Connection::open(&db).unwrap();
    let ready: i64 = conn
        .query_row(
            "SELECT canonical_ready FROM projection_state WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let sessions: i64 = conn
        .query_row("SELECT COUNT(*) FROM session", [], |row| row.get(0))
        .unwrap();
    let messages: i64 = conn
        .query_row("SELECT COUNT(*) FROM message", [], |row| row.get(0))
        .unwrap();
    assert_eq!(ready, 1);
    assert!(sessions > 0);
    assert!(messages > 0);
    assert!(db.with_extension("ledger").is_dir());
}

#[test]
fn interrupted_first_serve_is_preserved_and_restarted_through_ram() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let claude = home.join(".claude");
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../groundtruth/claude-code/fixtures/2.1.241/basic-chat/claude-config");
    copy_tree(&fixture, &claude);
    let db = temp.path().join("archive/rosetta.db");
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();

    // A consented DISCOVERED runtime carries the layout's `~` root; only such
    // a row is kind-granular consent the archive can hand on (an absolute
    // root would be a supervisor-enrolled runtime, whose consent is the
    // `--enroll` file, never the archive — 2026-09-12).
    let store = rosetta_store::Store::open_progressive(&db).unwrap();
    store
        .conn()
        .execute(
            "INSERT INTO agent_runtime
               (id, kind, data_root, enabled, discovered_at)
             VALUES ('old-claude', 'claude-code', '~/.claude', 1, 0)",
            [],
        )
        .unwrap();
    drop(store);
    let old_ledger = db.with_extension("ledger");
    std::fs::create_dir(&old_ledger).unwrap();
    std::fs::write(old_ledger.join("old-evidence"), b"preserve me").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_rosettad"))
        .args([
            "serve",
            "--db",
            db.to_str().unwrap(),
            "--home",
            home.to_str().unwrap(),
            "--addr",
            "127.0.0.1:0",
            "--restart-incomplete",
            "--ticks",
            "0",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("archived incomplete projection"),
        "{stdout}"
    );
    assert!(stdout.contains("retained 1 enrolled runtimes"), "{stdout}");
    assert!(stdout.contains("milestone: canonical-ready"), "{stdout}");

    let recoveries = std::fs::read_dir(db.parent().unwrap().join("recovery"))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(recoveries.len(), 1);
    let recovered = recoveries[0].path();
    assert_eq!(
        std::fs::read(recovered.join("rosetta.ledger/old-evidence")).unwrap(),
        b"preserve me"
    );
    assert!(recovered.join("rosetta.db").is_file());
    assert!(recovered.join("recovery.json").is_file());

    let conn = rusqlite::Connection::open(&db).unwrap();
    let ready: i64 = conn
        .query_row(
            "SELECT canonical_ready FROM projection_state WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(ready, 1);
    assert!(db.with_extension("ledger").is_dir());
}

#[test]
fn completed_second_boot_is_live_incremental_catch_up() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let claude = home.join(".claude");
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../groundtruth/claude-code/fixtures/2.1.241/basic-chat/claude-config");
    copy_tree(&fixture, &claude);
    let db = temp.path().join("archive/rosetta.db");

    let first = assert_success(&run_serve(&home, &db));
    assert!(first.contains("milestone: canonical-ready"), "{first}");
    let runtime_ledger = db.with_extension("ledger").join("claude-code");

    let second = assert_success(&run_serve(&home, &db));
    assert!(!second.contains("canonical RAM public"), "{second}");
    assert!(!second.contains("milestone: canonical-ready"), "{second}");
    assert!(!second.contains("metadata public"), "{second}");
    assert!(second.contains("1 ticks"), "{second}");
    let after_first_warm = ledger_segments(&runtime_ledger);

    let third = assert_success(&run_serve(&home, &db));
    assert!(!third.contains("milestone: canonical-ready"), "{third}");
    assert_eq!(
        ledger_segments(&runtime_ledger),
        after_first_warm,
        "unchanged warm metadata must not open another ledger segment"
    );

    let conn = rusqlite::Connection::open(&db).unwrap();
    assert_eq!(
        conn.query_row("SELECT canonical_ready FROM projection_state", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap(),
        1
    );
}

#[test]
fn staged_cold_ledger_rebuilds_and_finishes_on_next_boot() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let claude = home.join(".claude");
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../groundtruth/claude-code/fixtures/2.1.241/basic-chat/claude-config");
    copy_tree(&fixture, &claude);
    let db = temp.path().join("archive/rosetta.db");
    assert_success(&run_serve(&home, &db));

    let ledger = db.with_extension("ledger");
    let staged = ledger.with_file_name(format!(
        "{}.staged",
        ledger.file_name().unwrap().to_string_lossy()
    ));
    std::fs::remove_file(&db).unwrap();
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

    let resumed = assert_success(&run_serve(&home, &db));
    assert!(resumed.contains("resuming cold generation"), "{resumed}");
    assert!(resumed.contains("recovered"), "{resumed}");
    assert!(resumed.contains("milestone: canonical-ready"), "{resumed}");
    assert!(db.is_file());
    assert!(ledger.is_dir());
    assert!(!staged.exists());
    assert!(!ledger.join(".cold-generation.json").exists());
    let ready: i64 = rusqlite::Connection::open(&db)
        .unwrap()
        .query_row("SELECT canonical_ready FROM projection_state", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(ready, 1);
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

fn scalar(db: &Path, sql: &str) -> Option<String> {
    rusqlite::Connection::open(db)
        .and_then(|conn| conn.query_row(sql, [], |row| row.get(0)))
        .ok()
}

/// A transcript caught mid-append at a fresh start — its file ends inside a
/// tool result its writer has not finished — no longer fails the start: the
/// clean prefix publishes with its session held Streaming, and once the
/// writer finishes, the live runner reads only the growth. The published
/// projection then holds exactly the rows a start over the finished file
/// publishes, and the raw ledger holds every record once, in one sealed
/// generation.
#[test]
fn a_transcript_caught_mid_append_publishes_and_the_live_runner_finishes_it() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../groundtruth/claude-code/fixtures/2.1.241/tool-use/claude-config");
    let temp = tempfile::tempdir().unwrap();

    // The transcript, cut inside the tool result.
    let home = temp.path().join("live");
    let claude = home.join(".claude");
    copy_tree(&fixture, &claude);
    let transcript = find_project_jsonl(&claude.join("projects"));
    let text = std::fs::read(&transcript).unwrap();
    let result = text
        .windows(br#""type":"tool_result""#.len())
        .position(|window| window == br#""type":"tool_result""#)
        .expect("the fixture holds a tool result");
    std::fs::write(&transcript, &text[..result]).unwrap();
    let cut_at = std::fs::metadata(&transcript).unwrap().modified().unwrap();
    let suffix = text[result..].to_vec();
    let native = transcript
        .file_stem()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let db = temp.path().join("live/rosetta.db");

    let mut child = Command::new(env!("CARGO_BIN_EXE_rosettad"))
        .args([
            "serve",
            "--db",
            db.to_str().unwrap(),
            "--home",
            home.to_str().unwrap(),
            "--agent",
            "claude-code",
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

    // Published with the unfinished record left out: the prefix is active
    // (one generation, no staged one) and the session is Streaming.
    let state = format!("SELECT stream_state FROM session WHERE native_session_id = '{native}'");
    assert_eq!(scalar(&db, &state).as_deref(), Some("streaming"), "{log:?}");
    let generations = scalar(
        &db,
        "SELECT active_generation || '/' || highest_generation FROM ingest_cursor
         WHERE path LIKE '%.jsonl'",
    );
    assert_eq!(generations.as_deref(), Some("1/1"));
    let published: i64 = scalar(&db, "SELECT CAST(COUNT(*) AS TEXT) FROM message")
        .unwrap()
        .parse()
        .unwrap();
    assert!(published > 0);

    // The writer finishes the record and the session. FSEvents may be
    // suppressed in a sandbox; the 30-second stat poll must converge anyway.
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&transcript)
        .unwrap();
    file.write_all(&suffix).unwrap();
    file.sync_data().unwrap();
    drop(file);
    let deadline = Instant::now() + Duration::from_secs(45);
    while scalar(&db, &state).as_deref() != Some("indexed") {
        assert_eq!(child.0.try_wait().unwrap(), None, "daemon exited early");
        assert!(
            Instant::now() < deadline,
            "the live runner never finished it"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    child.0.kill().unwrap();
    child.0.wait().unwrap();

    // The reference: a fresh start over the finished transcript as it would
    // have stood at the start — modified when the cut one was, since a
    // session's `updated_at` starts from the mtime its metadata skim reads.
    let whole_home = temp.path().join("whole");
    copy_tree(&fixture, &whole_home.join(".claude"));
    let whole_transcript = find_project_jsonl(&whole_home.join(".claude/projects"));
    std::fs::File::options()
        .write(true)
        .open(&whole_transcript)
        .unwrap()
        .set_modified(cut_at)
        .unwrap();
    let whole_db = temp.path().join("whole/rosetta.db");
    assert_success(&run_serve(&whole_home, &whole_db));
    let whole = canonical_rows(&whole_db);
    assert!(published < whole["message"].len() as i64);
    let live = canonical_rows(&db);
    for (table, rows) in &whole {
        assert_eq!(&live[table], rows, "{table}");
    }

    // Every record reached the ledger once, in the one generation its seal
    // closed at the cut: the growth was appended, the prefix never replayed.
    let scan = scan_ledger(&db.with_extension("ledger").join("claude-code")).unwrap();
    let mut offsets = Vec::new();
    let mut seals = Vec::new();
    for entry in scan.entries {
        match entry {
            LedgerEntry::Raw {
                source,
                generation,
                start_offset,
                ..
            } if source.relative_path.ends_with(".jsonl") => {
                offsets.push((generation, start_offset));
            }
            LedgerEntry::GenerationComplete {
                source,
                generation,
                normalized_clean,
                ..
            } if source.relative_path.ends_with(".jsonl") => {
                seals.push((generation, normalized_clean));
            }
            _ => {}
        }
    }
    let lines = text.split(|byte| *byte == b'\n').count() - 1;
    let mut unique = offsets.clone();
    unique.dedup();
    assert_eq!(unique, offsets, "no record was framed twice");
    assert_eq!(offsets.len(), lines);
    assert!(offsets.iter().all(|(generation, _)| *generation == 1));
    assert_eq!(seals, [(1, true)]);
}

#[test]
fn warm_daemon_append_reaches_sqlite_raw_ledger_and_websocket() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let claude = home.join(".claude");
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../groundtruth/claude-code/fixtures/2.1.241/basic-chat/claude-config");
    copy_tree(&fixture, &claude);
    let db = temp.path().join("archive/rosetta.db");
    assert_success(&run_serve(&home, &db));

    let mut child = Command::new(env!("CARGO_BIN_EXE_rosettad"))
        .args([
            "serve",
            "--db",
            db.to_str().unwrap(),
            "--home",
            home.to_str().unwrap(),
            "--agent",
            "claude-code",
            "--addr",
            "127.0.0.1:0",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut child = ChildGuard(child);
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let _ = sender.send(line);
        }
    });
    let origin = loop {
        let line = receiver.recv_timeout(Duration::from_secs(10)).unwrap();
        if let Some(origin) = line
            .split_whitespace()
            .find(|word| word.starts_with("http://"))
        {
            break origin.to_string();
        }
    };

    let runtime_ledger = db.with_extension("ledger").join("claude-code");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !runtime_ledger.join(".meta-head").is_file() {
        assert!(
            Instant::now() < deadline,
            "warm watcher/meta setup timed out"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    std::thread::sleep(Duration::from_millis(100));
    let transcript = find_project_jsonl(&claude.join("projects"));
    let native = transcript.file_stem().unwrap().to_string_lossy();
    let conn = rusqlite::Connection::open(&db).unwrap();
    let session_id: String = conn
        .query_row(
            "SELECT id FROM session WHERE native_session_id = ?1",
            [native.as_ref()],
            |row| row.get(0),
        )
        .unwrap();
    let before: i64 = conn
        .query_row("SELECT COUNT(*) FROM message", [], |row| row.get(0))
        .unwrap();
    drop(conn);

    let mut socket = websocket_session(&origin, &session_id);
    let hello = read_ws_text(&mut socket);
    let snapshot = read_ws_text(&mut socket);
    assert!(hello.contains("\"frame\":\"hello\""), "{hello}");
    assert!(
        snapshot.contains("\"frame\":\"timeline_snapshot\""),
        "{snapshot}"
    );

    let sentinel = "quetzalcoatl daemon websocket append";
    let line = serde_json::json!({
        "type": "user",
        "sessionId": native,
        "uuid": "00000000-0000-4000-8000-000000009999",
        "parentUuid": null,
        "isSidechain": false,
        "userType": "external",
        "timestamp": "2026-08-31T20:00:00.000Z",
        "cwd": "/tmp/rosetta-live-test",
        "gitBranch": "main",
        "version": "2.1.241",
        "message": {"role": "user", "content": sentinel},
    });
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&transcript)
        .unwrap();
    writeln!(file, "{line}").unwrap();
    file.sync_data().unwrap();

    // CI/sandbox filesystems may suppress FSEvents. The daemon's mandatory
    // 30-second stat-poll lane must still converge end to end.
    let deadline = Instant::now() + Duration::from_secs(40);
    loop {
        assert_eq!(
            child.0.try_wait().unwrap(),
            None,
            "warm daemon exited early"
        );
        let count = rusqlite::Connection::open(&db)
            .and_then(|conn| conn.query_row("SELECT COUNT(*) FROM message", [], |row| row.get(0)))
            .unwrap_or(before);
        if count == before + 1 {
            break;
        }
        assert!(Instant::now() < deadline, "live SQLite append timed out");
        std::thread::sleep(Duration::from_millis(25));
    }
    let mutation = read_ws_text(&mut socket);
    assert!(mutation.contains("\"frame\":\"mutation\""), "{mutation}");
    assert!(mutation.contains(sentinel), "{mutation}");
    let scan = scan_ledger(&runtime_ledger).unwrap();
    assert!(scan.entries.iter().any(|entry| matches!(
        entry,
        LedgerEntry::Raw { payload, .. }
            if String::from_utf8_lossy(payload).contains(sentinel)
    )));

    child.0.kill().unwrap();
    child.0.wait().unwrap();
}
