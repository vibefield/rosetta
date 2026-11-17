//! Fresh serve's handoff to the live runner: a transcript caught mid-append,
//! a record over the projection limit, a warm daemon's append reaching
//! SQLite, the raw ledger and a WebSocket, and a stdout reader that went
//! away (moved from `fresh_serve.rs`, which keeps the shared helpers).

use super::*;

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

/// A record over the 32 MiB projection limit no longer fails a fresh start:
/// it is captured into the raw ledger whole, projects one drift fact, and its
/// session is published Indexed.
#[test]
fn a_record_over_the_projection_limit_publishes_as_a_drift_fact() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let claude = home.join(".claude");
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../groundtruth/claude-code/fixtures/2.1.241/basic-chat/claude-config");
    copy_tree(&fixture, &claude);
    let transcript = find_project_jsonl(&claude.join("projects"));
    let native = transcript
        .file_stem()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let huge = format!(
        "{{\"type\":\"user\",\"attachment\":\"{}\"}}\n",
        "q".repeat(33 * 1024 * 1024)
    );
    std::fs::OpenOptions::new()
        .append(true)
        .open(&transcript)
        .unwrap()
        .write_all(huge.as_bytes())
        .unwrap();
    let db = temp.path().join("archive/rosetta.db");

    let stdout = assert_success(&run_serve(&home, &db));
    let prepared = stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|event| event["event"] == "stream-prepared")
        .expect("a stream-prepared event");
    assert_eq!(
        (&prepared["quarantined"], &prepared["oversized"]),
        (&serde_json::json!(1), &serde_json::json!(0)),
        "{prepared}"
    );
    let state = format!("SELECT stream_state FROM session WHERE native_session_id = '{native}'");
    assert_eq!(scalar(&db, &state).as_deref(), Some("indexed"));
    assert_eq!(
        scalar(
            &db,
            "SELECT error_code FROM drift_agg WHERE error_code = 'over_projection_limit'"
        )
        .as_deref(),
        Some("over_projection_limit")
    );
    let scan = scan_ledger(&db.with_extension("ledger").join("claude-code")).unwrap();
    assert!(scan.entries.iter().any(|entry| matches!(
        entry,
        LedgerEntry::Raw { payload, .. } if payload.len() == huge.len() - 1
    )));
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

/// Serve's stdout is a report (ready line, milestones, census, profiles),
/// read through a pipe by the app, the trace kit (`| tee | grep`) and
/// harnesses. A reader that goes away must not panic the sole canonical
/// writer mid-import (std's `println!` panics on EPIPE: exit 101, nothing
/// published): the cold build still publishes and serve exits cleanly.
#[test]
fn fresh_serve_survives_a_stdout_reader_that_went_away() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    copy_tree(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../groundtruth/claude-code/fixtures/2.1.241/basic-chat/claude-config"),
        &home.join(".claude"),
    );
    let db = temp.path().join("rosetta.sqlite");
    // The reader is gone before serve writes its first line.
    let (reader, writer) = std::io::pipe().unwrap();
    drop(reader);
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
            "--json",
            "--profile",
        ])
        .stdout(writer)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && !stderr.contains("panicked"),
        "status {:?}\nstderr:\n{stderr}",
        output.status
    );
    let conn = rusqlite::Connection::open(&db).unwrap();
    let (ready, sessions): (i64, i64) = conn
        .query_row(
            "SELECT canonical_ready, (SELECT COUNT(*) FROM session)
               FROM projection_state WHERE singleton = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        (ready, sessions),
        (1, 1),
        "the cold build published its one session"
    );
}
