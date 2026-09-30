//! `rosettad serve` with its access flags, end to end (docs/AUTH.md): the
//! secret files are judged before anything binds, the ready event names the
//! modes, the files are read once, and the bound daemon refuses and admits.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use rosetta_server::grant::{self, GrantClaims};

const TOKEN: &str = "rosettad-cli-test-bearer-0123456789";
const KEY: [u8; 32] = *b"cli test grant key, 32 bytes....";

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Bench {
    _dir: tempfile::TempDir,
    home: PathBuf,
    db: PathBuf,
    token: PathBuf,
    key: PathBuf,
}

fn secret(path: &Path, bytes: &[u8], mode: u32) {
    std::fs::write(path, bytes).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }
    #[cfg(not(unix))]
    let _ = mode;
}

impl Bench {
    fn new() -> Self {
        let dir = tempfile::TempDir::new().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir(&home).unwrap();
        let token = dir.path().join("token");
        secret(&token, format!("{TOKEN}\n").as_bytes(), 0o600);
        let key = dir.path().join("grant.key");
        secret(&key, &KEY, 0o600);
        Self {
            db: dir.path().join("projection.sqlite"),
            _dir: dir,
            home,
            token,
            key,
        }
    }

    fn serve(&self, extra: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_rosettad"));
        command.args([
            "serve",
            "--db",
            self.db.to_str().unwrap(),
            "--home",
            self.home.to_str().unwrap(),
            "--addr",
            "127.0.0.1:0",
            "--json",
        ]);
        command.args(extra);
        command
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

/// One raw HTTP/1.1 exchange: the status and the `x-rosetta-auth` reason.
fn get(origin: &str, path: &str, authorization: Option<&str>) -> (u16, Option<String>) {
    let address = origin.strip_prefix("http://").unwrap();
    let mut stream = TcpStream::connect(address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let credential = authorization
        .map(|value| format!("Authorization: {value}\r\n"))
        .unwrap_or_default();
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {address}\r\n{credential}Connection: close\r\n\r\n"
    )
    .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    let status = response
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or_else(|| panic!("{path}: {response}"));
    let reason = response
        .lines()
        .take_while(|line| !line.is_empty())
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("x-rosetta-auth")
                .then(|| value.trim().to_string())
        });
    (status, reason)
}

#[test]
fn bad_secret_files_are_refused_before_anything_binds() {
    let bench = Bench::new();
    let exposed = bench.home.join("../exposed-token");
    secret(&exposed, TOKEN.as_bytes(), 0o644);
    let short_key = bench.home.join("../short.key");
    secret(&short_key, &KEY[..16], 0o600);
    let token = bench.token.to_str().unwrap();
    let cases: Vec<(Vec<&str>, &str)> = vec![
        (vec!["--token-file", exposed.to_str().unwrap()], "chmod 600"),
        (
            vec!["--grant-key-file", bench.key.to_str().unwrap()],
            "--grant-key-file needs --token-file",
        ),
        (
            vec![
                "--token-file",
                token,
                "--grant-key-file",
                short_key.to_str().unwrap(),
            ],
            "--grant-key-file",
        ),
        (
            vec!["--token-file", token, "--allow-origin", "null"],
            "--allow-origin",
        ),
    ];
    for (extra, expected) in cases {
        #[cfg(not(unix))]
        if expected == "chmod 600" {
            continue;
        }
        let output = bench.serve(&extra).output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{extra:?}: {stdout}");
        assert!(stderr.contains(expected), "{extra:?}: {stderr}");
        assert!(
            !stdout.contains("\"event\":\"ready\""),
            "{extra:?}: {stdout}"
        );
    }
    // Nothing bound means nothing was created either.
    assert!(!bench.db.exists());
}

#[test]
fn a_configured_daemon_needs_the_bearer_and_admits_a_grant_for_its_session() {
    let bench = Bench::new();
    let mut child = bench
        .serve(&[
            "--token-file",
            bench.token.to_str().unwrap(),
            "--grant-key-file",
            bench.key.to_str().unwrap(),
            "--allow-origin",
            "VibeField-App://Shell",
            "--debug-auth",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let _child = ChildGuard(child);
    let (lines, receiver) = mpsc::channel();
    let errors = lines.clone();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let _ = lines.send(line);
        }
    });
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            let _ = errors.send(format!("stderr: {line}"));
        }
    });
    let ready = loop {
        let line = receiver
            .recv_timeout(Duration::from_secs(30))
            .expect("the daemon announced readiness");
        if let Ok(event) = serde_json::from_str::<serde_json::Value>(&line)
            && event["event"] == "ready"
        {
            break event;
        }
    };
    assert_eq!(
        ready["auth"],
        serde_json::json!({
            "bearer": true, "grants": true, "origins": ["vibefield-app://shell"],
        })
    );
    // Read once at start: the supervisor may remove both files now.
    std::fs::remove_file(&bench.token).unwrap();
    std::fs::remove_file(&bench.key).unwrap();
    let origin = ready["origin"].as_str().unwrap();
    let bearer = format!("Bearer {TOKEN}");

    assert_eq!(
        get(origin, "/api/status", None),
        (401, Some("missing".into()))
    );
    assert_eq!(
        get(origin, "/api/status", Some("Bearer not-the-token")),
        (401, Some("bad-token".into()))
    );
    assert_eq!(get(origin, "/api/status", Some(&bearer)), (200, None));
    assert_eq!(get(origin, "/api/sessions", Some(&bearer)), (200, None));

    let grant = |sid: &str| {
        format!(
            "Rosetta-Grant {}",
            grant::mint(
                &KEY,
                &GrantClaims::read(sid, "fieldd/cli-test", now_ms() + 60_000)
            )
        )
    };
    // Past the credential check (the session does not exist here: 404)...
    assert_eq!(
        get(origin, "/api/sessions/s-one", Some(&grant("s-one"))),
        (404, None)
    );
    // ...and refused for another session, or on a list.
    assert_eq!(
        get(origin, "/api/sessions/s-two", Some(&grant("s-one"))),
        (401, Some("wrong-session".into()))
    );
    assert_eq!(
        get(origin, "/api/sessions", Some(&grant("s-one"))),
        (401, Some("wrong-route".into()))
    );

    // --debug-auth: the client id and the route template, nothing else.
    let mut logged = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    while logged.len() < 3 && Instant::now() < deadline {
        if let Ok(line) = receiver.recv_timeout(Duration::from_millis(200))
            && line.contains("rosettad: auth:")
        {
            logged.push(line);
        }
    }
    assert_eq!(
        logged,
        [
            r#"stderr: rosettad: auth: grant accepted client="fieldd/cli-test" route=/api/sessions/{id}"#,
            r#"stderr: rosettad: auth: grant refused (wrong-session) client="fieldd/cli-test" route=/api/sessions/{id}"#,
            "stderr: rosettad: auth: grant refused (wrong-route) route=/api/sessions",
        ]
    );
}

#[test]
fn without_access_flags_the_daemon_is_as_it_was() {
    let bench = Bench::new();
    let mut child = bench
        .serve(&[])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let _child = ChildGuard(child);
    let ready = BufReader::new(stdout)
        .lines()
        .map_while(Result::ok)
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(&line).ok())
        .find(|event| event["event"] == "ready")
        .expect("the daemon announced readiness");
    assert_eq!(
        ready["auth"],
        serde_json::json!({"bearer": false, "grants": false, "origins": []})
    );
    let origin = ready["origin"].as_str().unwrap();
    assert_eq!(get(origin, "/api/status", None), (200, None));
    assert_eq!(get(origin, "/api/sessions", None), (200, None));
}
