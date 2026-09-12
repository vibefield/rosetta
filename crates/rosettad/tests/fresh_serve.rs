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
