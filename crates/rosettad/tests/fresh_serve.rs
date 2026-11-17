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

/// The first `milestone: <name>` line whose fields include every `with`
/// pair: its position in `stdout` and its `elapsed_s`.
fn milestone(stdout: &str, name: &str, with: &[&str]) -> (usize, f64) {
    stdout
        .lines()
        .enumerate()
        .find_map(|(index, line)| {
            let rest = line.strip_prefix("milestone: ")?.strip_prefix(name)?;
            let fields = rest.split_whitespace().collect::<Vec<_>>();
            if !(rest.is_empty() || rest.starts_with(' '))
                || !with.iter().all(|pair| fields.contains(pair))
            {
                return None;
            }
            let elapsed = fields
                .iter()
                .find_map(|field| field.strip_prefix("elapsed_s="))?
                .parse()
                .ok()?;
            Some((index, elapsed))
        })
        .unwrap_or_else(|| panic!("no milestone {name} {with:?}:\n{stdout}"))
}

fn set_modified(path: &Path, modified: std::time::SystemTime) {
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(modified)
        .unwrap();
}

/// Set every file's modification time under `root`.
fn age_tree(root: &Path, modified: std::time::SystemTime) {
    for entry in std::fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            age_tree(&path, modified);
        } else {
            set_modified(&path, modified);
        }
    }
}

const DAY: Duration = Duration::from_secs(24 * 60 * 60);

/// The transcripts of two fixtures in one home: basic-chat's written now,
/// resume's three days ago.
fn two_session_home(home: &Path) {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../groundtruth/claude-code/fixtures/2.1.241");
    let claude = home.join(".claude");
    copy_tree(&fixtures.join("basic-chat/claude-config"), &claude);
    let resume = fixtures.join("resume/claude-config/projects");
    copy_tree(&resume, &claude.join("projects"));
    let now = std::time::SystemTime::now();
    age_tree(&claude, now);
    let relative = find_project_jsonl(&resume);
    let relative = relative.strip_prefix(&resume).unwrap();
    set_modified(&claude.join("projects").join(relative), now - 3 * DAY);
}

/// Measurement is behaviour-neutral stdout (docs/ARCHITECTURE.md, cold-build
/// measurement): per-runtime metadata and admission milestones, first and
/// recent readiness in order before canonical readiness, and a census that
/// carries structure only.
#[test]
fn fresh_serve_reports_readiness_milestones_in_order_and_a_structure_only_census() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    two_session_home(&home);
    let db = temp.path().join("archive/rosetta.db");
    let stdout = assert_success(&run_serve(&home, &db));

    let metadata = milestone(&stdout, "runtime-metadata-public", &["agent=claude-code"]);
    let complete = milestone(&stdout, "first-session-complete", &[]);
    let admitted = milestone(&stdout, "runtime-readable", &["sessions=2"]);
    let first = milestone(&stdout, "first-readable", &[]);
    let day = milestone(
        &stdout,
        "recent-ready",
        &["horizon=24h", "horizon_sessions=1"],
    );
    let week = milestone(
        &stdout,
        "recent-ready",
        &["horizon=7d", "horizon_sessions=2"],
    );
    // Root trees (Claude Code admits per tree): two single-session trees,
    // three days apart, none over 64 MiB, each ready at its own flip.
    let relative: [(&str, &[&str]); 5] = [
        (
            "recent-ready",
            &["horizon=24h@newest", "horizon_sessions=1"],
        ),
        ("recent-ready", &["horizon=7d@newest", "horizon_sessions=2"]),
        ("newest-trees-ready", &["n=2"]),
        ("small-recent-ready", &["horizon=24h@newest", "trees=1"]),
        ("small-recent-ready", &["horizon=7d@newest", "trees=2"]),
    ];
    let relative = relative.map(|(name, with)| milestone(&stdout, name, with));
    assert!(
        !stdout.contains("milestone: newest-giant-ready"),
        "{stdout}"
    );
    let canonical = milestone(&stdout, "canonical-ready", &[]);
    for ready in relative {
        assert!(first.0 < ready.0 && ready.0 < admitted.0, "{stdout}");
    }
    let peak = milestone(&stdout, "memory-peak", &[]);
    let order = [
        metadata, complete, first, day, week, admitted, canonical, peak,
    ];
    for pair in order.windows(2) {
        assert!(
            pair[0].0 < pair[1].0 && pair[0].1 <= pair[1].1,
            "{pair:?}\n{stdout}"
        );
    }
    let (usage, line) = stdout
        .lines()
        .enumerate()
        .find(|(_, line)| line.starts_with("usage-copies: dropped="))
        .expect("usage-copies line");
    assert!(usage > canonical.0, "{stdout}");
    for field in [
        " displaced=",
        " owned=",
        " unclaimed_usage=",
        " displaced_visible=",
        " owner_bytes=",
    ] {
        assert!(line.contains(field), "{field}: {line}");
    }

    let census = stdout
        .lines()
        .filter(|line| line.starts_with("census: "))
        .collect::<Vec<_>>();
    assert_eq!(census.len(), 1, "{stdout}");
    let census = census[0];
    for field in [
        "files=2 ",
        " le1h=1/",
        " le30d=2/",
        " older=0/0 ",
        // Transcripts are their own files: nothing is shared.
        " shared=0/0 ",
        " sessions=2 ",
        " roots=2 ",
    ] {
        assert!(census.contains(field), "{field}: {census}");
    }
    // Structure only: every value is a number, a number list, a runtime id
    // or the agent kind, and only a tally (`files/bytes`) has a slash.
    for field in census.trim_start_matches("census: ").split_whitespace() {
        let (key, value) = field.split_once('=').expect("key=value fields");
        assert!(!field.contains('\\'), "{field}: {census}");
        match key {
            "agent" => assert_eq!(value, "claude-code"),
            "runtime" => assert!(value.bytes().all(|b| b.is_ascii_hexdigit())),
            "le1h" | "le24h" | "le7d" | "le30d" | "older" | "shared" => {
                let tally = value.split('/').collect::<Vec<_>>();
                assert!(tally.len() == 2 && tally.iter().all(|n| n.parse::<u64>().is_ok()));
            }
            _ => assert!(
                value.split(',').all(|n| n.parse::<u64>().is_ok()),
                "{field}: {census}"
            ),
        }
    }
    let conn = rusqlite::Connection::open(&db).unwrap();
    let mut ids = conn
        .prepare("SELECT id, native_session_id, root_session_id, project_id FROM session")
        .unwrap()
        .query_map([], |row| {
            Ok([
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?.unwrap_or_default(),
            ])
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
        .concat();
    ids.retain(|id| !id.is_empty());
    assert!(ids.len() >= 6, "two sessions' identities: {ids:?}");
    for id in ids {
        assert!(!census.contains(&id), "{id} leaked: {census}");
    }
}

/// A horizon holding no session is decided at the first admission.
#[test]
fn a_horizon_without_recent_sessions_is_ready_at_the_first_admission() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    two_session_home(&home);
    age_tree(
        &home.join(".claude/projects"),
        std::time::SystemTime::now() - 40 * DAY,
    );
    let db = temp.path().join("archive/rosetta.db");
    let stdout = assert_success(&run_serve(&home, &db));

    let first = milestone(&stdout, "first-readable", &[]);
    for horizon in ["horizon=24h", "horizon=7d"] {
        let ready = milestone(&stdout, "recent-ready", &[horizon, "horizon_sessions=0"]);
        assert!(ready.0 > first.0, "{stdout}");
    }
    // Measured from the newest session instead, both are still recent.
    for horizon in ["horizon=24h@newest", "horizon=7d@newest"] {
        let ready = milestone(&stdout, "recent-ready", &[horizon, "horizon_sessions=2"]);
        assert!(ready.0 > first.0, "{stdout}");
    }
    let census = stdout
        .lines()
        .find(|line| line.starts_with("census: "))
        .expect("census line");
    assert!(census.contains(" le30d=0/0 older=2/"), "{census}");
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

/// Turn a finished start back into an interrupted one: the projection is
/// gone and its ledger is the named staged generation, with its manifest.
/// Returns the staged directory.
fn stage_ledger(db: &Path, claude: &Path) -> std::path::PathBuf {
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
    let staged = stage_ledger(&db, &claude);

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

/// A resumed start closes a transcript caught mid-append the way a fresh one
/// does: the interrupted generation replays, the catch-up over the current
/// file stops at its last complete record, and the start publishes with the
/// session Streaming instead of failing on an incomplete file.
#[test]
fn a_resumed_start_closes_a_transcript_caught_mid_append() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let claude = home.join(".claude");
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../groundtruth/claude-code/fixtures/2.1.241/basic-chat/claude-config");
    copy_tree(&fixture, &claude);
    let db = temp.path().join("archive/rosetta.db");
    assert_success(&run_serve(&home, &db));
    let staged = stage_ledger(&db, &claude);

    // While no daemon ran, the agent went on writing and is mid-append now.
    let transcript = find_project_jsonl(&claude.join("projects"));
    let native = transcript
        .file_stem()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    std::fs::OpenOptions::new()
        .append(true)
        .open(&transcript)
        .unwrap()
        .write_all(br#"{"type":"user","message":{"role":"user","content":"unfin"#)
        .unwrap();

    let resumed = assert_success(&run_serve(&home, &db));
    assert!(resumed.contains("resuming cold generation"), "{resumed}");
    assert!(resumed.contains("milestone: canonical-ready"), "{resumed}");
    assert!(!staged.exists());
    let state = format!("SELECT stream_state FROM session WHERE native_session_id = '{native}'");
    assert_eq!(scalar(&db, &state).as_deref(), Some("streaming"));
    let generations = scalar(
        &db,
        "SELECT active_generation || '/' || highest_generation FROM ingest_cursor
         WHERE path LIKE '%.jsonl'",
    )
    .unwrap();
    let (active, highest) = generations.split_once('/').unwrap();
    assert_eq!(active, highest, "no generation is left staged");
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

#[path = "fresh_serve/live_handoff.rs"]
mod live_handoff;

#[path = "fresh_serve/measure_lines.rs"]
mod measure_lines;
