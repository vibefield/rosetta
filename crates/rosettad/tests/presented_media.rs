//! A video Claude Code sent the user by path, through `rosettad serve`: a
//! fresh import copies it into the ledger and the projection, the session's
//! tool row names the member, and a read grant fetches its bytes. With
//! `ROSETTA_PRESENTED_MEDIA=off` the row keeps the path and nothing is copied.
//!
//! The home, the transcript and the sent file are synthetic and temporary.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use rosetta_capture::{DocumentCaptureStatus, LedgerEntry, scan_ledger};
use rosetta_core::ids;
use rosetta_server::grant::{self, GrantClaims};
use serde_json::{Value, json};

mod support;
use support::fixtures::{claude_root, write_lines};

const TOKEN: &str = "rosettad-sent-video-test-bearer-0123456789";
const KEY: [u8; 32] = *b"sent video grant key, 32 bytes..";
const SWITCH: &str = "ROSETTA_PRESENTED_MEDIA";

/// A well-formed MP4: an `ftyp` box of major brand `isom`, then one `free`
/// box whose padding differs along the file.
fn mp4(len: usize) -> Vec<u8> {
    let mut bytes = 20_u32.to_be_bytes().to_vec();
    bytes.extend_from_slice(b"ftypisom");
    bytes.extend_from_slice(&[0, 0, 2, 0]);
    bytes.extend_from_slice(b"mp41");
    bytes.extend_from_slice(&u32::try_from(len - 20).unwrap().to_be_bytes());
    bytes.extend_from_slice(b"free");
    bytes.extend((bytes.len()..len).map(|index| (index % 251) as u8));
    bytes
}

/// A home whose Claude root holds one session that sent one video.
struct Bench {
    dir: tempfile::TempDir,
    home: PathBuf,
    clip: PathBuf,
    video: Vec<u8>,
    session: String,
}

impl Bench {
    fn new() -> Self {
        let dir = tempfile::TempDir::new().unwrap();
        let home = dir.path().join("home");
        let clip = dir.path().join("out/screen recording.mp4");
        std::fs::create_dir_all(clip.parent().unwrap()).unwrap();
        let video = mp4(96 * 1024);
        std::fs::write(&clip, &video).unwrap();
        let record = |kind: &str, uuid: &str, message: Value| {
            json!({"type":kind,"parentUuid":null,"isSidechain":false,"uuid":uuid,
                "timestamp":"2026-10-01T12:00:00Z","sessionId":"native","version":"2.1.283",
                "cwd":dir.path(),"gitBranch":"main","userType":"external","message":message})
        };
        let mut result = record(
            "user",
            "result",
            json!({"role":"user","content":[{"type":"tool_result","tool_use_id":"send-1",
                "content":"sent","is_error":false}]}),
        );
        result["toolUseResult"] = json!({"caption":"The recording","display":"inline",
            "attachments":[{"path":clip,"size":video.len(),"media_type":"video/mp4",
                "isImage":false,"pathValidated":true,
                "file_uuid":"00000000-0000-4000-8000-000000000000"}]});
        let lines = [
            record(
                "user",
                "prompt",
                json!({"role":"user","content":"record the screen"}),
            ),
            record(
                "assistant",
                "opener",
                json!({"id":"response-1","type":"message","role":"assistant",
                    "model":"claude-opus-5","stop_reason":"tool_use",
                    "content":[{"type":"tool_use","id":"send-1","name":"SendUserFile",
                        "input":{"files":[clip],"caption":"The recording"}}]}),
            ),
            result,
        ];
        write_lines(
            &claude_root(&home).join("projects/synthetic/native.jsonl"),
            &lines,
        );
        // The discovered Claude runtime's identity is its layout root.
        let session = ids::session_id(&ids::runtime_id("claude-code", "~/.claude"), "native");
        Self {
            dir,
            home,
            clip,
            video,
            session,
        }
    }

    fn db(&self, name: &str) -> PathBuf {
        self.dir.path().join(name).join("projection.sqlite")
    }

    fn digest(&self) -> String {
        blake3::hash(&self.video).to_hex().to_string()
    }

    /// `serve` of this home into `db`, with the capture switch as given and
    /// nothing else of the caller's environment deciding what is captured.
    fn serve(&self, db: &Path, switch: Option<&str>, extra: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_rosettad"));
        let (db, home) = (db.to_str().unwrap(), self.home.to_str().unwrap());
        command.args([
            "serve",
            "--db",
            db,
            "--home",
            home,
            "--agent",
            "claude-code",
        ]);
        command.args(["--addr", "127.0.0.1:0", "--json"]);
        command.args(extra);
        command.env_remove("ROSETTA_ARTIFACT_ROOTS");
        match switch {
            Some(value) => command.env(SWITCH, value),
            None => command.env_remove(SWITCH),
        };
        command
    }

    /// A fresh import that publishes and exits.
    fn import(&self, db: &Path, switch: Option<&str>, extra: &[&str]) {
        let mut command = self.serve(db, switch, extra);
        let output = command.args(["--ticks", "0"]).output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "stdout:\n{stdout}\nstderr:\n{stderr}"
        );
        assert!(stdout.contains("milestone: canonical-ready"), "{stdout}");
    }
}

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// One raw HTTP/1.1 exchange: the status and the body.
fn get(origin: &str, path: &str, authorization: Option<&str>) -> (u16, Vec<u8>) {
    let address = origin.strip_prefix("http://").unwrap();
    let mut stream = TcpStream::connect(address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    let credential = authorization
        .map(|value| format!("Authorization: {value}\r\n"))
        .unwrap_or_default();
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {address}\r\n{credential}Connection: close\r\n\r\n"
    )
    .unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).unwrap();
    let split = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .unwrap_or_else(|| panic!("{path}: {}", String::from_utf8_lossy(&response)));
    let head = String::from_utf8_lossy(&response[..split]).to_ascii_lowercase();
    let status = head.split_whitespace().nth(1).unwrap().parse().unwrap();
    assert!(
        !head.contains("transfer-encoding: chunked"),
        "{path}: a body of known length is sent whole"
    );
    (status, response[split + 4..].to_vec())
}

/// The stored `tool_detail` of the published projection's one tool row.
fn tool_detail(db: &Path) -> Value {
    let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY;
    let conn = rusqlite::Connection::open_with_flags(db, flags).unwrap();
    let mut statement = conn
        .prepare("SELECT tool_detail FROM message WHERE tool_detail IS NOT NULL")
        .unwrap();
    let details: Vec<String> = statement
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(details.len(), 1, "one tool row: {details:?}");
    serde_json::from_str(&details[0]).unwrap()
}

/// The member files beside a projection.
fn member_names(db: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(rosetta_store::assets::asset_root(db)) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .map(|entry| entry.unwrap())
        .filter(|entry| entry.file_type().unwrap().is_file())
        .map(|entry| entry.file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

/// The Claude ledger's media frames: `(status, captured bytes)`.
fn media_frames(db: &Path) -> Vec<(DocumentCaptureStatus, Vec<u8>)> {
    let ledger = db.with_extension("ledger").join("claude-code");
    let entries = scan_ledger(&ledger).unwrap().entries;
    let frames = entries.into_iter().filter_map(|entry| match entry {
        LedgerEntry::Document { document, .. } => {
            assert!(document.media.is_some(), "no page was published");
            Some((document.status, document.bytes))
        }
        _ => None,
    });
    frames.collect()
}

/// `rosettad ledger verify --json` of a projection's ledger.
fn verify(db: &Path) -> Value {
    let output = Command::new(env!("CARGO_BIN_EXE_rosettad"))
        .args(["ledger", "verify", "--db", db.to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{stdout}");
    serde_json::from_str(stdout.trim()).unwrap()
}

fn media_tally(frames: u64, captured: u64, captured_bytes: usize) -> Value {
    json!({"frames": frames, "captured": captured, "capturedBytes": captured_bytes,
        "missing": 0, "tooLarge": 0, "unsupported": 0, "changedDuringRead": 0})
}

#[test]
fn a_fresh_serve_shows_the_sent_video_as_an_asset_and_serves_it_to_a_read_grant() {
    let bench = Bench::new();
    let db = bench.db("served");
    let token = bench.dir.path().join("token");
    let key = bench.dir.path().join("grant.key");
    for (path, bytes) in [(&token, TOKEN.as_bytes()), (&key, &KEY[..])] {
        std::fs::write(path, bytes).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
    }
    let secrets = [
        "--token-file",
        token.to_str().unwrap(),
        "--grant-key-file",
        key.to_str().unwrap(),
    ];
    let mut child = bench
        .serve(&db, None, &secrets)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let daemon = ChildGuard(child);
    let (lines, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let _ = lines.send(line);
        }
    });
    // Bound before discovery; the session's rows are public at publication.
    let mut origin = None;
    let mut log = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(120);
    while !log
        .iter()
        .any(|line: &String| line.contains("milestone: canonical-ready"))
    {
        let wait = deadline.saturating_duration_since(Instant::now());
        let line = receiver
            .recv_timeout(wait)
            .unwrap_or_else(|_| panic!("no publication:\n{}", log.join("\n")));
        if let Ok(event) = serde_json::from_str::<Value>(&line)
            && event["event"] == "ready"
        {
            origin = event["origin"].as_str().map(str::to_owned);
        }
        log.push(line);
    }
    let origin = origin.expect("the ready event precedes publication");
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let grant = |session: &str| {
        let claims = GrantClaims::read(session, "fieldd/sent-video-test", now_ms + 600_000);
        format!("Rosetta-Grant {}", grant::mint(&KEY, &claims))
    };
    let read = grant(&bench.session);

    // The session's tool row names the member.
    let timeline = format!("/api/sessions/{}/timeline", bench.session);
    let (status, body) = get(&origin, &timeline, Some(&read));
    let page = String::from_utf8(body).unwrap();
    assert_eq!(status, 200, "{page}");
    let member = format!("/api/assets/{}.mp4", bench.digest());
    assert!(page.contains(&format!("\"url\":\"{member}\"")), "{page}");

    // A read grant fetches it: 200 and the file's bytes.
    let (status, bytes) = get(&origin, &member, Some(&read));
    assert_eq!(status, 200);
    assert!(bytes == bench.video, "the body is exactly the sent file");
    assert_eq!(get(&origin, &member, None).0, 401, "a credential is needed");
    assert_eq!(
        get(&origin, &member, Some(&format!("Bearer {TOKEN}"))).0,
        200
    );
    // The accepted exposure: a member is opened by its digest, so a read
    // grant minted for any session fetches it.
    assert_eq!(
        get(&origin, &member, Some(&grant("another-session"))).0,
        200
    );
    assert_eq!(
        get(&origin, &timeline, Some(&grant("another-session"))).0,
        401
    );

    // The member is the projection's own copy: the sent file can go.
    std::fs::remove_file(&bench.clip).unwrap();
    let (status, bytes) = get(&origin, &member, Some(&read));
    assert_eq!(status, 200);
    assert!(bytes == bench.video);
    drop(daemon);

    // What the import left: the member, one captured frame, and the tally.
    assert_eq!(member_names(&db), [format!("{}.mp4", bench.digest())]);
    let detail = tool_detail(&db);
    let attachment = &detail["attachments"][0];
    assert_eq!(attachment["kind"], "video");
    assert_eq!(attachment["name"], "screen recording.mp4");
    assert_eq!(attachment["source"]["asset"]["url"], member);
    let frames = media_frames(&db);
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].0, DocumentCaptureStatus::Captured);
    assert!(frames[0].1 == bench.video);
    let report = verify(&db);
    assert_eq!(
        report["total"]["media"],
        media_tally(1, 1, bench.video.len())
    );
    assert_eq!(report["runtimes"][0]["media"], report["total"]["media"]);
}

#[test]
fn with_the_switch_off_the_row_keeps_the_path_and_the_ledger_holds_no_media_frame() {
    let bench = Bench::new();
    let clip = bench.clip.to_str().unwrap();

    // Off, in any letter case: a path reference, no member, no frame.
    for (name, value) in [("off", "off"), ("off-upper", "OFF")] {
        let db = bench.db(name);
        bench.import(&db, Some(value), &[]);
        let detail = tool_detail(&db);
        let attachments = detail["attachments"].as_array().unwrap();
        assert_eq!(attachments.len(), 1, "{name}: {detail}");
        let attachment = &attachments[0];
        assert_eq!(attachment["kind"], "video", "{name}");
        assert_eq!(attachment["byte_len"], bench.video.len(), "{name}");
        assert_eq!(attachment["media_type"], "video/mp4", "{name}");
        assert_eq!(attachment["source"]["reference"]["path"], clip, "{name}");
        assert!(
            !detail.to_string().contains("/api/assets/"),
            "{name}: {detail}"
        );
        assert!(member_names(&db).is_empty(), "{name}");
        assert!(media_frames(&db).is_empty(), "{name}");
        assert_eq!(
            verify(&db)["total"]["media"],
            media_tally(0, 0, 0),
            "{name}"
        );
    }
    assert_eq!(
        std::fs::read(&bench.clip).unwrap(),
        bench.video,
        "untouched"
    );

    // Any other value, or none, leaves capture on, on the pooled build and
    // on the serial one: the same row and the same source content.
    let builds: [(&str, Option<&str>, &[&str]); 3] = [
        ("pooled", None, &[]),
        ("serial", None, &["--capture-workers", "0"]),
        ("other-value", Some("0"), &[]),
    ];
    let member = format!("/api/assets/{}.mp4", bench.digest());
    let mut reference: Option<(Value, Value)> = None;
    for (name, value, extra) in builds {
        let db = bench.db(name);
        bench.import(&db, value, extra);
        let detail = tool_detail(&db);
        assert_eq!(
            detail["attachments"][0]["source"]["asset"]["url"], member,
            "{name}"
        );
        assert_eq!(
            member_names(&db),
            [format!("{}.mp4", bench.digest())],
            "{name}"
        );
        let report = verify(&db);
        assert_eq!(
            report["total"]["media"],
            media_tally(1, 1, bench.video.len()),
            "{name}"
        );
        let built = (detail, report["sourceSetDigest"].clone());
        assert_eq!(
            &built,
            reference.get_or_insert_with(|| built.clone()),
            "{name}"
        );
    }
    let off = verify(&bench.db("off"))["sourceSetDigest"].clone();
    assert_ne!(off, reference.unwrap().1, "a media frame is source content");
}
