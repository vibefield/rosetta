//! Serve's measurement-only lines (docs/ARCHITECTURE.md, "Cold-build
//! measurement") through the real binary over a fixture home: the clock
//! anchor every `elapsed_s` maps onto, `consent-granted` when a roll-call
//! grant ends the consent wait, and `search-ready` once the fresh or the
//! resumed RAM path's detached search worker publishes its first generation
//! after canonical readiness — printed by an observer the serve path never
//! waits for.

use std::time::{SystemTime, UNIX_EPOCH};

use super::*;

fn basic_home(temp: &Path) -> std::path::PathBuf {
    let home = temp.join("home");
    copy_tree(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../groundtruth/claude-code/fixtures/2.1.241/basic-chat/claude-config"),
        &home.join(".claude"),
    );
    home
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

/// Spawn `serve` with `args`; its stdout lines (and stderr, prefixed)
/// arrive on the returned channel.
fn spawn(args: &[&str]) -> (ChildGuard, mpsc::Receiver<String>) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_rosettad"))
        .arg("serve")
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
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
    (ChildGuard(child), receiver)
}

/// Read lines into `log` until one satisfies `until`.
fn read_until(receiver: &mpsc::Receiver<String>, log: &mut Vec<String>, until: &str) {
    loop {
        let line = receiver
            .recv_timeout(Duration::from_secs(90))
            .unwrap_or_else(|_| panic!("no {until:?}:\n{}", log.join("\n")));
        let done = line.contains(until);
        log.push(line);
        if done {
            return;
        }
    }
}

fn elapsed(log: &[String], milestone: &str) -> (usize, f64) {
    log.iter()
        .enumerate()
        .find_map(|(index, line)| {
            let rest = line.strip_prefix(&format!("milestone: {milestone} "))?;
            let value = rest.split("elapsed_s=").nth(1)?.parse().ok()?;
            Some((index, value))
        })
        .unwrap_or_else(|| panic!("no milestone {milestone}:\n{}", log.join("\n")))
}

#[test]
fn serve_anchors_its_clock_and_reports_search_after_canonical_readiness() {
    let temp = tempfile::tempdir().unwrap();
    let home = basic_home(temp.path());
    let db = temp.path().join("rosetta.sqlite");
    let before = now_ms();
    let (mut child, lines) = spawn(&[
        "--db",
        db.to_str().unwrap(),
        "--home",
        home.to_str().unwrap(),
        "--agent",
        "claude-code",
        "--addr",
        "127.0.0.1:0",
        "--json",
    ]);
    let mut log = Vec::new();
    read_until(&lines, &mut log, "clock: wall_ms=");
    let after = now_ms();
    // The anchor is serve's first line, its event the second.
    assert_eq!(log.len(), 1, "{log:?}");
    let wall_ms: u64 = log[0]
        .strip_prefix("clock: wall_ms=")
        .unwrap()
        .parse()
        .unwrap();
    assert!(
        before <= wall_ms && wall_ms <= after,
        "{before} {wall_ms} {after}"
    );
    let event: serde_json::Value =
        serde_json::from_str(&lines.recv_timeout(Duration::from_secs(30)).unwrap()).unwrap();
    assert_eq!(
        event,
        serde_json::json!({"event": "clock", "wallMs": wall_ms})
    );

    // The live loop runs on; the observer prints once search publishes.
    read_until(&lines, &mut log, "milestone: search-");
    let canonical = elapsed(&log, "canonical-ready");
    let search = elapsed(&log, "search-ready");
    assert!(canonical.0 < search.0 && canonical.1 <= search.1, "{log:?}");
    // The wall clock maps onto elapsed_s through the anchor.
    let mapped = wall_ms as f64 + search.1 * 1_000.0;
    assert!(mapped <= now_ms() as f64 + 10.0, "{mapped}");
    assert_eq!(child.0.try_wait().unwrap(), None, "serve keeps running");
    child.0.kill().unwrap();
    child.0.wait().unwrap();
}

/// The resumed RAM path wakes its worker the same way: a staged generation
/// replays, and `search-ready` follows its canonical readiness while serve
/// runs on.
#[test]
fn a_resumed_start_reports_search_after_canonical_readiness() {
    let temp = tempfile::tempdir().unwrap();
    let home = basic_home(temp.path());
    let db = temp.path().join("archive/rosetta.db");
    assert_success(&run_serve(&home, &db));
    stage_ledger(&db, &home.join(".claude"));
    let (mut child, lines) = spawn(&[
        "--db",
        db.to_str().unwrap(),
        "--home",
        home.to_str().unwrap(),
        "--agent",
        "claude-code",
        "--addr",
        "127.0.0.1:0",
    ]);
    let mut log = Vec::new();
    read_until(&lines, &mut log, "milestone: search-");
    assert!(log[0].starts_with("clock: wall_ms="), "{log:?}");
    assert!(
        log.iter()
            .any(|line| line.contains("resuming cold generation")),
        "{log:?}"
    );
    let canonical = elapsed(&log, "canonical-ready");
    let search = elapsed(&log, "search-ready");
    assert!(canonical.0 < search.0 && canonical.1 <= search.1, "{log:?}");
    assert_eq!(child.0.try_wait().unwrap(), None, "serve keeps running");
    child.0.kill().unwrap();
    child.0.wait().unwrap();
}

/// POST `body` to `path` on the daemon at `origin`; the status code.
fn post(origin: &str, path: &str, body: &str) -> u16 {
    let address = origin.strip_prefix("http://").unwrap();
    let mut stream = TcpStream::connect(address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(20)))
        .unwrap();
    write!(
        stream,
        "POST {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or(0)
}

#[test]
fn a_roll_call_grant_prints_consent_granted_before_anything_is_read() {
    let temp = tempfile::tempdir().unwrap();
    let home = basic_home(temp.path());
    let db = temp.path().join("rosetta.sqlite");
    // No --agent: nothing is consented, so serve waits for the roll call.
    let (mut child, lines) = spawn(&[
        "--db",
        db.to_str().unwrap(),
        "--home",
        home.to_str().unwrap(),
        "--addr",
        "127.0.0.1:0",
        "--ticks",
        "0",
        "--json",
    ]);
    let mut log = Vec::new();
    read_until(&lines, &mut log, "no enrolled agents");
    assert!(
        !log.iter().any(|line| line.contains("consent-granted")),
        "{log:?}"
    );
    let origin = log
        .iter()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|event| event["event"] == "ready")
        .and_then(|event| event["origin"].as_str().map(str::to_owned))
        .expect("the ready event precedes the consent wait");
    assert_eq!(
        post(
            &origin,
            "/api/runtimes/enable",
            r#"{"kinds":["claude-code"]}"#
        ),
        200
    );
    read_until(&lines, &mut log, "milestone: canonical-ready");
    let waiting = log
        .iter()
        .position(|line| line.contains("no enrolled agents"))
        .unwrap();
    let granted = elapsed(&log, "consent-granted");
    let metadata = elapsed(&log, "runtime-metadata-public");
    assert!(waiting < granted.0 && granted.0 < metadata.0, "{log:?}");
    assert!(granted.1 <= metadata.1, "{log:?}");
    let events: Vec<serde_json::Value> = log
        .iter()
        .filter_map(|line| serde_json::from_str(line).ok())
        .filter(|event: &serde_json::Value| event["milestone"] == "consent-granted")
        .collect();
    assert_eq!(events.len(), 1, "{log:?}");
    assert_eq!(events[0]["event"], "milestone");
    let event_s = events[0]["elapsedS"].as_f64().unwrap();
    assert!(
        (event_s - granted.1).abs() <= 0.005 + 1e-9,
        "{event_s} {granted:?}"
    );
    assert!(child.0.wait().unwrap().success());
}
