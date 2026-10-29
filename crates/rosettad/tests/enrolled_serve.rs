//! Supervisor-enrolled runtimes end to end (2026-09-12): the real binary,
//! ground-truth fixture trees as roots, and the daemon's own HTTP surface as
//! the witness. Nothing here reads a live home.
//!
//! The native root is the `basic-chat` fixture under `<home>/.claude`; the
//! enrolled persona root is the `tool-use` fixture under an absolute
//! directory — two distinct sessions of ONE kind.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use rosetta_core::ids;

const NATIVE_SESSION: &str = "665e3e71-7c12-4810-925a-e8b92cb6f29d";
const KIM_SESSION: &str = "13f3a252-bd0a-415b-a20b-d6d6b139aaa1";

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

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../groundtruth/claude-code/fixtures/2.1.241")
        .join(name)
        .join("claude-config")
}

/// A workbench: a home with the native root, one persona root, and a
/// projection path — plus the enrollment file the supervisor would write.
struct Bench {
    _temp: tempfile::TempDir,
    home: PathBuf,
    kim: PathBuf,
    db: PathBuf,
    enroll: PathBuf,
}

impl Bench {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        copy_tree(&fixture("basic-chat"), &home.join(".claude"));
        let kim = temp.path().join("users/u1/agents/roots/claude-code/9a3d");
        copy_tree(&fixture("tool-use"), &kim);
        let db = temp.path().join("archive/rosetta.db");
        let enroll = temp.path().join("enroll.json");
        let bench = Self {
            _temp: temp,
            home,
            kim,
            db,
            enroll,
        };
        bench.write_enrollment(&["claude-code"], true);
        bench
    }

    /// `enabled_kinds` plus, when `kim` is true, the persona root labelled Kim.
    fn write_enrollment(&self, enabled_kinds: &[&str], kim: bool) {
        let mut runtimes = Vec::new();
        if kim {
            runtimes.push(serde_json::json!({
                "kind": "claude-code",
                "data_root": self.kim.to_string_lossy(),
                "label": "Kim",
            }));
        }
        let file = serde_json::json!({
            "format": 1,
            "enabled_kinds": enabled_kinds,
            "runtimes": runtimes,
            "supervisor": { "ignored": "unknown top-level fields are tolerated" },
        });
        std::fs::write(&self.enroll, serde_json::to_vec_pretty(&file).unwrap()).unwrap();
    }

    fn native_id(&self) -> String {
        ids::runtime_id("claude-code", "~/.claude")
    }

    fn kim_id(&self) -> String {
        ids::runtime_id("claude-code", &self.kim.to_string_lossy())
    }

    fn kim_ledger_dir(&self) -> PathBuf {
        self.db.with_extension("ledger").join(format!(
            "claude-code~{}",
            ids::root_digest(&self.kim.to_string_lossy())
        ))
    }

    fn native_ledger_dir(&self) -> PathBuf {
        self.db.with_extension("ledger").join("claude-code")
    }

    fn command(&self, subcommand: &str, extra: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_rosettad"));
        command.arg(subcommand).args([
            "--db",
            self.db.to_str().unwrap(),
            "--home",
            self.home.to_str().unwrap(),
        ]);
        command.args(extra);
        command
    }

    /// One finite serve (`--ticks 0`) with the enrollment file.
    fn serve_once(&self, extra: &[&str]) -> std::process::Output {
        let mut args = vec![
            "--enroll",
            self.enroll.to_str().unwrap(),
            "--addr",
            "127.0.0.1:0",
            "--ticks",
            "0",
        ];
        args.extend_from_slice(extra);
        self.command("serve", &args).output().unwrap()
    }

    fn runtimes(&self) -> Vec<(String, String, String, i64)> {
        let conn = rusqlite::Connection::open(&self.db).unwrap();
        let mut statement = conn
            .prepare("SELECT id, kind, data_root, enabled FROM agent_runtime ORDER BY data_root")
            .unwrap();
        statement
            .query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    }

    fn has_session(&self, native_session_id: &str) -> bool {
        rusqlite::Connection::open(&self.db)
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM session WHERE native_session_id = ?1",
                [native_session_id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap()
            != 0
    }

    fn session_runtime(&self, native_session_id: &str) -> String {
        rusqlite::Connection::open(&self.db)
            .unwrap()
            .query_row(
                "SELECT runtime_id FROM session WHERE native_session_id = ?1",
                [native_session_id],
                |row| row.get(0),
            )
            .unwrap()
    }

    fn session_count(&self) -> i64 {
        rusqlite::Connection::open(&self.db)
            .unwrap()
            .query_row("SELECT COUNT(*) FROM session", [], |row| row.get(0))
            .unwrap()
    }

    fn enabled(&self, runtime_id: &str) -> i64 {
        self.runtimes()
            .into_iter()
            .find(|(id, _, _, _)| id == runtime_id)
            .unwrap_or_else(|| panic!("{runtime_id} is not a persisted runtime"))
            .3
    }

    fn staged_ledger(&self) -> PathBuf {
        let ledger = self.db.with_extension("ledger");
        ledger.with_file_name(format!(
            "{}.staged",
            ledger.file_name().unwrap().to_string_lossy()
        ))
    }

    /// Rewind a finished serve into the state a killed one leaves behind: the
    /// projection gone, its ledger staged, and a format-2 cold-generation
    /// manifest naming the runtimes that start had consented. This is the
    /// interruption the real daemon recovers from, staged deterministically
    /// instead of racing a SIGKILL against a fixture-sized backfill.
    fn stage_cold_generation(&self, enrolled_kim: bool) {
        let ledger = self.db.with_extension("ledger");
        let staged = self.staged_ledger();
        std::fs::remove_file(&self.db).unwrap();
        std::fs::rename(&ledger, &staged).unwrap();
        let mut runtimes = vec![serde_json::json!({
            "id": self.native_id(),
            "kind": "claude-code",
            "data_root": "~/.claude",
            "ledger_dir": "claude-code",
        })];
        if enrolled_kim {
            runtimes.push(serde_json::json!({
                "id": self.kim_id(),
                "kind": "claude-code",
                "data_root": self.kim.to_string_lossy(),
                "ledger_dir": self.kim_ledger_dir().file_name().unwrap().to_string_lossy(),
            }));
        }
        let manifest = serde_json::json!({
            "format": 2,
            "projection": self.db.file_name().unwrap().to_string_lossy(),
            "ledger": ledger.file_name().unwrap().to_string_lossy(),
            "enabled_agents": ["claude-code"],
            "runtime_roots": { "claude-code": self.home.join(".claude").to_string_lossy() },
            "runtimes": runtimes,
        });
        std::fs::write(
            staged.join(".cold-generation.json"),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
    }
}

fn assert_success(output: &std::process::Output) -> String {
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stamp(dir: &Path) -> serde_json::Value {
    let path = dir.join("runtime.json");
    let bytes = std::fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    serde_json::from_slice(&bytes).unwrap()
}

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A long-running serve; the returned origin answers HTTP until the guard drops.
fn spawn_serve(bench: &Bench) -> (ChildGuard, String) {
    spawn_serve_with_flags(bench, &[])
}

fn spawn_serve_with_flags(bench: &Bench, extra: &[&str]) -> (ChildGuard, String) {
    let mut child = bench
        .command(
            "serve",
            &[
                "--enroll",
                bench.enroll.to_str().unwrap(),
                "--addr",
                "127.0.0.1:0",
            ],
        )
        .args(extra)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let child = ChildGuard(child);
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let _ = sender.send(line);
        }
    });
    let origin = loop {
        let line = receiver.recv_timeout(Duration::from_secs(20)).unwrap();
        if let Some(origin) = line
            .split_whitespace()
            .find(|word| word.starts_with("http://"))
        {
            break origin.to_string();
        }
    };
    (child, origin)
}

fn http_get(origin: &str, path: &str) -> serde_json::Value {
    let address = origin.strip_prefix("http://").unwrap();
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

/// Witness (a): one native root plus one enrolled root of the same kind →
/// two runtimes with distinct ids, every session on the right one, and two
/// stamped ledger directories.
#[test]
fn an_enrolled_root_is_a_second_runtime_of_its_kind_with_its_own_ledger() {
    let bench = Bench::new();
    let stdout = assert_success(&bench.serve_once(&[]));
    assert!(stdout.contains("claude-code: metadata public"), "{stdout}");
    assert!(
        stdout.contains("claude-code «Kim»: metadata public"),
        "{stdout}"
    );
    assert!(stdout.contains("milestone: canonical-ready"), "{stdout}");
    assert!(!stdout.contains("no enrolled agents"), "{stdout}");

    let runtimes = bench.runtimes();
    assert_eq!(runtimes.len(), 2, "{runtimes:?}");
    let (kim_id, kim_kind, kim_root, kim_enabled) = &runtimes[0];
    let (native_id, native_kind, native_root, native_enabled) = &runtimes[1];
    assert_eq!(
        (kim_kind.as_str(), native_kind.as_str()),
        ("claude-code", "claude-code")
    );
    assert_eq!(kim_root, &bench.kim.to_string_lossy());
    assert_eq!(native_root, "~/.claude");
    assert_eq!(kim_id, &bench.kim_id());
    assert_eq!(native_id, &bench.native_id());
    assert_ne!(kim_id, native_id);
    assert_eq!((*kim_enabled, *native_enabled), (1, 1));
    assert_eq!(bench.session_runtime(NATIVE_SESSION), bench.native_id());
    assert_eq!(bench.session_runtime(KIM_SESSION), bench.kim_id());

    let native_stamp = stamp(&bench.native_ledger_dir());
    assert_eq!(native_stamp["format"], 1);
    assert_eq!(native_stamp["id"], bench.native_id());
    assert_eq!(native_stamp["kind"], "claude-code");
    assert_eq!(native_stamp["data_root"], "~/.claude");
    let kim_stamp = stamp(&bench.kim_ledger_dir());
    assert_eq!(kim_stamp["id"], bench.kim_id());
    assert_eq!(kim_stamp["kind"], "claude-code");
    assert_eq!(kim_stamp["data_root"], bench.kim.to_string_lossy().as_ref());
    assert!(bench.native_ledger_dir().join("00000000.rledger").is_file());
    assert!(bench.kim_ledger_dir().join("00000000.rledger").is_file());
}

/// The roll call is published once this start's consent is settled — after
/// the projection's `enabled` flags have been re-derived from the file — so a
/// non-empty `/api/discoveries` is the barrier both consent routes are read
/// behind.
fn wait_for_roll_call(origin: &str) -> serde_json::Value {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let discoveries = http_get(origin, "/api/discoveries");
        if discoveries["discoveries"]
            .as_array()
            .is_some_and(|rows| !rows.is_empty())
        {
            return discoveries;
        }
        assert!(Instant::now() < deadline, "roll call timed out");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn roll_call_row<'a>(discoveries: &'a serde_json::Value, kind: &str) -> &'a serde_json::Value {
    discoveries["discoveries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["kind"] == kind)
        .unwrap_or_else(|| panic!("{discoveries}"))
}

fn runtime_row<'a>(runtimes: &'a serde_json::Value, id: &str) -> &'a serde_json::Value {
    runtimes["runtimes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == id)
        .unwrap_or_else(|| panic!("{runtimes}"))
}

/// Witness (a): the file REVOKES. A start whose `enabled_kinds` no longer
/// names a kind does not watch it, and both consent routes say so — the
/// persisted flag is a projection of the file, never a second source that
/// could only ever add. The rows stay public: this retires a runtime, it
/// never purges one.
#[test]
fn a_kind_the_file_stops_naming_is_revoked_and_reads_disabled_on_both_routes() {
    let bench = Bench::new();
    bench.write_enrollment(&["claude-code"], false);
    assert_success(&bench.serve_once(&[]));
    assert_eq!(bench.enabled(&bench.native_id()), 1);
    let sessions_before = bench.session_count();
    assert!(sessions_before > 0);

    // The user un-checks the kind in the app; the supervisor rewrites the file
    // and restarts the daemon.
    bench.write_enrollment(&[], false);
    let (_child, origin) = spawn_serve(&bench);
    let discoveries = wait_for_roll_call(&origin);
    let native = roll_call_row(&discoveries, "claude-code");
    assert_eq!(native["installed"], true);
    assert_eq!(native["enabled"], false, "{discoveries}");

    let runtimes = http_get(&origin, "/api/runtimes");
    let row = runtime_row(&runtimes, &bench.native_id());
    assert_eq!(row["data_root"], "~/.claude");
    assert_eq!(row["enabled"], false, "{runtimes}");

    // Nothing is watched, and the standing projection is untouched and public.
    let status = http_get(&origin, "/api/status");
    assert_eq!(status["daemon"]["ingest"]["phase"], "live", "{status}");
    assert_eq!(
        status["daemon"]["runtimes"].as_array().map(Vec::len),
        Some(0),
        "{status}"
    );
    assert_eq!(bench.session_count(), sessions_before);
    assert_eq!(bench.enabled(&bench.native_id()), 0);
    assert_eq!(bench.runtimes().len(), 1);
}

/// Witness (b): the same revocation with a persona present. The gate never
/// arms (the persona is consented by construction), the persona goes live
/// alone, and the revoked kind reads disabled on both routes.
#[test]
fn revoking_a_kind_beside_a_persona_leaves_the_persona_live_and_the_kind_retired() {
    let bench = Bench::new();
    assert_success(&bench.serve_once(&[]));
    let sessions_before = bench.session_count();
    assert_eq!(bench.enabled(&bench.native_id()), 1);
    assert_eq!(bench.enabled(&bench.kim_id()), 1);

    bench.write_enrollment(&[], true);
    let (_child, origin) = spawn_serve(&bench);
    let discoveries = wait_for_roll_call(&origin);
    assert_eq!(
        roll_call_row(&discoveries, "claude-code")["enabled"],
        false,
        "{discoveries}"
    );

    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        let status = http_get(&origin, "/api/status");
        assert_ne!(
            status["daemon"]["ingest"]["phase"], "awaiting_consent",
            "{status}"
        );
        if status["daemon"]["runtimes"]
            .as_array()
            .is_some_and(|rows| !rows.is_empty())
        {
            break status;
        }
        assert!(Instant::now() < deadline, "live health rows timed out");
        std::thread::sleep(Duration::from_millis(50));
    };
    let health = status["daemon"]["runtimes"].as_array().unwrap();
    assert_eq!(health.len(), 1, "only the persona is watched: {status}");
    assert_eq!(health[0]["runtime_id"], bench.kim_id());
    assert_eq!(health[0]["label"], "Kim");

    let runtimes = http_get(&origin, "/api/runtimes");
    assert_eq!(runtimes["runtimes"].as_array().map(Vec::len), Some(2));
    assert_eq!(runtime_row(&runtimes, &bench.kim_id())["enabled"], true);
    assert_eq!(
        runtime_row(&runtimes, &bench.native_id())["enabled"],
        false,
        "{runtimes}"
    );
    assert_eq!(bench.session_count(), sessions_before);
    assert_eq!(bench.session_runtime(NATIVE_SESSION), bench.native_id());
}

/// Witness (c), the discovered half: un-consenting mid-backfill. A pending
/// cold generation holds the kind, and the file no longer does. The daemon
/// STARTS — it finishes the generation without that kind instead of refusing
/// to run until a human rebuilds — and the staged evidence is kept.
#[test]
fn an_unconsented_kind_is_dropped_from_a_resumed_cold_generation() {
    let bench = Bench::new();
    assert_success(&bench.serve_once(&[]));
    bench.stage_cold_generation(true);
    let staged_native_ledger = bench.staged_ledger().join("claude-code");
    assert!(staged_native_ledger.join("00000000.rledger").is_file());

    bench.write_enrollment(&[], true);
    let stdout = assert_success(&bench.serve_once(&[]));
    assert!(stdout.contains("resuming cold generation"), "{stdout}");
    assert!(
        stdout.contains(
            "claude-code: this start's --enroll file no longer consents it; dropped from the resumed cold generation"
        ),
        "{stdout}"
    );
    assert!(stdout.contains("milestone: canonical-ready"), "{stdout}");

    // The generation finished for the persona alone.
    assert!(bench.db.is_file());
    assert!(!bench.staged_ledger().exists());
    assert!(bench.has_session(KIM_SESSION));
    assert!(!bench.has_session(NATIVE_SESSION));
    assert_eq!(bench.session_runtime(KIM_SESSION), bench.kim_id());
    // The drop costs rows, not bytes: the dropped kind's raw evidence rides
    // the publish untouched, so `rosettad rebuild` can still recover it.
    assert!(bench.native_ledger_dir().join("00000000.rledger").is_file());
}

/// Witness (c), the enrolled half: the supervisor retires a persona while its
/// backfill is pending. The same mechanism applies — the persona is dropped
/// from the resume and the daemon starts — instead of the start failing
/// closed and stranding every other runtime behind a manual rebuild.
#[test]
fn a_persona_the_file_stops_naming_is_dropped_from_a_resumed_cold_generation() {
    let bench = Bench::new();
    assert_success(&bench.serve_once(&[]));
    bench.stage_cold_generation(true);
    let staged_kim_ledger = bench
        .staged_ledger()
        .join(bench.kim_ledger_dir().file_name().unwrap());
    assert!(staged_kim_ledger.join("00000000.rledger").is_file());

    bench.write_enrollment(&["claude-code"], false);
    let stdout = assert_success(&bench.serve_once(&[]));
    assert!(
        stdout.contains(&format!(
            "claude-code ({}): this start's --enroll file no longer names it; dropped from the resumed cold generation",
            bench.kim.to_string_lossy()
        )),
        "{stdout}"
    );
    assert!(stdout.contains("milestone: canonical-ready"), "{stdout}");

    assert!(bench.db.is_file());
    assert!(!bench.staged_ledger().exists());
    assert!(bench.has_session(NATIVE_SESSION));
    assert!(!bench.has_session(KIM_SESSION));
    assert!(bench.kim_ledger_dir().join("00000000.rledger").is_file());
}

/// Witness (c), the gate: a pending generation, a file that consents nothing,
/// and no persona. The daemon publishes the roll call and waits for the app —
/// it never exits on the generation, and it never publishes a projection it
/// has no consent to build.
#[test]
fn a_pending_generation_with_nothing_consented_awaits_consent_instead_of_exiting() {
    let bench = Bench::new();
    bench.write_enrollment(&["claude-code"], false);
    assert_success(&bench.serve_once(&[]));
    bench.stage_cold_generation(false);

    bench.write_enrollment(&[], false);
    let (_child, origin) = spawn_serve(&bench);
    let discoveries = wait_for_roll_call(&origin);
    assert_eq!(
        roll_call_row(&discoveries, "claude-code")["enabled"],
        false,
        "{discoveries}"
    );
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let status = http_get(&origin, "/api/status");
        if status["daemon"]["ingest"]["phase"] == "awaiting_consent" {
            break;
        }
        assert!(Instant::now() < deadline, "awaiting_consent timed out");
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        bench.staged_ledger().is_dir(),
        "the evidence is still staged"
    );
    assert!(!bench.db.exists());
}

/// Witness (d): re-consent. The kind is watched again, its sessions catch up,
/// and nothing duplicates — the revoked start really did leave the projection
/// alone.
#[test]
fn re_consenting_a_revoked_kind_watches_it_again_without_duplicating_rows() {
    let bench = Bench::new();
    bench.write_enrollment(&["claude-code"], false);
    assert_success(&bench.serve_once(&[]));
    let sessions_before = bench.session_count();

    // Revoked — and the agent writes a new session while it is.
    bench.write_enrollment(&[], false);
    copy_tree(&fixture("tool-use"), &bench.home.join(".claude"));
    assert_success(&bench.serve_once(&["--ticks", "1"]));
    assert_eq!(bench.session_count(), sessions_before);
    assert!(!bench.has_session(KIM_SESSION));
    assert_eq!(bench.enabled(&bench.native_id()), 0);

    bench.write_enrollment(&["claude-code"], false);
    assert_success(&bench.serve_once(&["--ticks", "1"]));
    assert_eq!(bench.enabled(&bench.native_id()), 1);
    assert_eq!(bench.session_count(), sessions_before + 1);
    assert_eq!(bench.session_runtime(KIM_SESSION), bench.native_id());
    assert_eq!(bench.runtimes().len(), 1, "{:?}", bench.runtimes());

    // A third start changes nothing: re-consent is not a second ingest.
    assert_success(&bench.serve_once(&["--ticks", "1"]));
    assert_eq!(bench.session_count(), sessions_before + 1);
}

/// Witnesses (b) and (d) on the wire: a second start on the same projection
/// with the same file is live for both runtimes at once — no consent phase —
/// `/api/runtimes` carries the label and provenance for the enrolled row
/// only, and the roll call never lists the enrolled root.
#[test]
fn second_start_is_live_for_both_runtimes_with_labels_and_the_roll_call_stays_native() {
    let bench = Bench::new();
    assert_success(&bench.serve_once(&[]));
    // The file is the authority on every start, so a second start that still
    // wants the native kind keeps naming it. (Revocation — the same file
    // WITHOUT the kind — is its own witness below.)
    bench.write_enrollment(&["claude-code"], true);

    let (_child, origin) = spawn_serve(&bench);
    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        let status = http_get(&origin, "/api/status");
        let phase = status["daemon"]["ingest"]["phase"].as_str().unwrap_or("");
        assert_ne!(phase, "awaiting_consent", "{status}");
        if status["daemon"]["runtimes"]
            .as_array()
            .is_some_and(|rows| rows.len() == 2)
        {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "live health rows timed out: {status}"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(status["daemon"]["ingest"]["phase"], "live");
    let health = status["daemon"]["runtimes"].as_array().unwrap();
    let kim = health
        .iter()
        .find(|row| row["runtime_id"] == bench.kim_id())
        .unwrap_or_else(|| panic!("{status}"));
    assert_eq!(kim["kind"], "claude-code");
    assert_eq!(kim["label"], "Kim");
    let native = health
        .iter()
        .find(|row| row["runtime_id"] == bench.native_id())
        .unwrap_or_else(|| panic!("{status}"));
    assert_eq!(native["kind"], "claude-code");
    assert!(native["label"].is_null());

    let runtimes = http_get(&origin, "/api/runtimes");
    let rows = runtimes["runtimes"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "{runtimes}");
    let kim = rows.iter().find(|row| row["id"] == bench.kim_id()).unwrap();
    assert_eq!(kim["kind"], "claude-code");
    assert_eq!(kim["label"], "Kim");
    assert_eq!(kim["provenance"], "enrolled");
    assert_eq!(kim["enabled"], true);
    let native = rows
        .iter()
        .find(|row| row["id"] == bench.native_id())
        .unwrap();
    assert_eq!(native["data_root"], "~/.claude");
    assert!(native.get("label").is_none(), "{native}");
    assert!(native.get("provenance").is_none(), "{native}");

    // The roll call still shows EVERY registered adapter (the file never
    // narrows the registry), each at its native `~` root — and never the
    // enrolled root, which is not the user's to consent.
    let discoveries = http_get(&origin, "/api/discoveries");
    let board = discoveries["discoveries"].as_array().unwrap();
    assert_eq!(board.len(), 9, "{discoveries}");
    assert!(
        board
            .iter()
            .all(|row| row["data_root"].as_str().unwrap().starts_with("~/")),
        "{discoveries}"
    );
    let native = board
        .iter()
        .find(|row| row["kind"] == "claude-code")
        .unwrap_or_else(|| panic!("{discoveries}"));
    assert_eq!(native["data_root"], "~/.claude");
    assert_eq!(native["installed"], true);
    assert_eq!(native["enabled"], true);
    assert_eq!(
        board
            .iter()
            .filter(|row| row["kind"] == "claude-code")
            .count(),
        1,
        "one roll-call row per kind, never one per enrolled root: {discoveries}"
    );
    assert!(!discoveries.to_string().contains("agents/roots"));
}

/// Witness (c): the ledger alone reproduces both runtimes — the stamp is
/// what carries an enrolled runtime's identity and root.
#[test]
fn rebuild_from_the_ledger_alone_reproduces_both_runtimes() {
    let bench = Bench::new();
    assert_success(&bench.serve_once(&[]));
    let before = bench.runtimes();
    let sessions_before = bench.session_count();
    let ledger = bench.db.with_extension("ledger");

    let rebuilt = bench._temp.path().join("rebuilt/rosetta.db");
    let output = Command::new(env!("CARGO_BIN_EXE_rosettad"))
        .args([
            "rebuild",
            "--db",
            rebuilt.to_str().unwrap(),
            "--ledger",
            ledger.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let stdout = assert_success(&output);
    assert!(stdout.contains("rebuilt"), "{stdout}");

    let conn = rusqlite::Connection::open(&rebuilt).unwrap();
    let mut statement = conn
        .prepare("SELECT id, kind, data_root, enabled FROM agent_runtime ORDER BY data_root")
        .unwrap();
    let after = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(after, before);
    let sessions: i64 = conn
        .query_row("SELECT COUNT(*) FROM session", [], |row| row.get(0))
        .unwrap();
    assert_eq!(sessions, sessions_before);
    let kim_session_runtime: String = conn
        .query_row(
            "SELECT runtime_id FROM session WHERE native_session_id = ?1",
            [KIM_SESSION],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(kim_session_runtime, bench.kim_id());
}

/// Witness (f): a start whose file omits a previously enrolled runtime does
/// not watch it, and its projection rows remain.
#[test]
fn a_runtime_the_file_no_longer_names_is_not_watched_but_keeps_its_rows() {
    let bench = Bench::new();
    assert_success(&bench.serve_once(&[]));
    let sessions_before = bench.session_count();
    let kim_ledger_before = std::fs::read_dir(bench.kim_ledger_dir()).unwrap().count();

    bench.write_enrollment(&["claude-code"], false);
    let stdout = assert_success(&bench.serve_once(&[]));
    assert!(
        stdout.contains("enrolled on an earlier start but absent from --enroll; not watched"),
        "{stdout}"
    );
    assert!(!stdout.contains("«Kim»"), "{stdout}");
    assert_eq!(bench.session_count(), sessions_before);
    assert_eq!(bench.session_runtime(KIM_SESSION), bench.kim_id());
    assert_eq!(bench.runtimes().len(), 2);
    assert_eq!(
        std::fs::read_dir(bench.kim_ledger_dir()).unwrap().count(),
        kim_ledger_before,
        "an unwatched runtime's ledger is not touched"
    );
}

/// Witness (g): a per-kind ledger directory without a stamp — every ledger
/// written before 2026-09-12 — still rebuilds and serves as the discovered
/// runtime, and serving it stamps it additively.
#[test]
fn a_legacy_per_kind_ledger_without_a_stamp_rebuilds_and_serves_as_the_discovered_runtime() {
    let bench = Bench::new();
    let stdout = assert_success(
        &bench
            .command(
                "serve",
                &[
                    "--agent",
                    "claude-code",
                    "--addr",
                    "127.0.0.1:0",
                    "--ticks",
                    "0",
                ],
            )
            .output()
            .unwrap(),
    );
    assert!(stdout.contains("milestone: canonical-ready"), "{stdout}");
    let stamp_path = bench.native_ledger_dir().join("runtime.json");
    assert!(stamp_path.is_file());
    std::fs::remove_file(&stamp_path).unwrap();

    let rebuilt = bench._temp.path().join("rebuilt/rosetta.db");
    let output = Command::new(env!("CARGO_BIN_EXE_rosettad"))
        .args([
            "rebuild",
            "--db",
            rebuilt.to_str().unwrap(),
            "--ledger",
            bench.db.with_extension("ledger").to_str().unwrap(),
            "--agent",
            "claude-code",
        ])
        .output()
        .unwrap();
    assert_success(&output);
    let conn = rusqlite::Connection::open(&rebuilt).unwrap();
    let (id, data_root): (String, String) = conn
        .query_row("SELECT id, data_root FROM agent_runtime", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .unwrap();
    assert_eq!(id, bench.native_id());
    assert_eq!(data_root, "~/.claude");
    assert!(!stamp_path.exists(), "rebuild never writes the ledger");

    // A warm serve over the legacy directory keeps using it, and stamps it.
    let stdout = assert_success(
        &bench
            .command(
                "serve",
                &[
                    "--agent",
                    "claude-code",
                    "--addr",
                    "127.0.0.1:0",
                    "--ticks",
                    "0",
                ],
            )
            .output()
            .unwrap(),
    );
    assert!(!stdout.contains("milestone: canonical-ready"), "{stdout}");
    let restamped = stamp(&bench.native_ledger_dir());
    assert_eq!(restamped["id"], bench.native_id());
    assert_eq!(restamped["data_root"], "~/.claude");
}

/// The supervisor's real first start: the persona directory exists but the
/// agent has not written yet. The runtime is enrolled and watched from the
/// start, and its first session is admitted by the live lane.
#[test]
fn an_empty_enrolled_root_is_enrolled_at_once_and_its_first_session_arrives_live() {
    let bench = Bench::new();
    std::fs::remove_dir_all(&bench.kim).unwrap();
    std::fs::create_dir_all(&bench.kim).unwrap();
    let stdout = assert_success(&bench.serve_once(&[]));
    assert!(
        stdout.contains("claude-code «Kim»: metadata public — 0 sessions"),
        "{stdout}"
    );
    let runtimes = bench.runtimes();
    assert_eq!(runtimes.len(), 2, "{runtimes:?}");
    assert!(
        runtimes
            .iter()
            .any(|(id, _, _, enabled)| id == &bench.kim_id() && *enabled == 1)
    );
    assert!(bench.kim_ledger_dir().join("runtime.json").is_file());
    let sessions_before = bench.session_count();

    // The agent writes its first session; the next start's catch-up admits it.
    copy_tree(&fixture("tool-use"), &bench.kim);
    let stdout = assert_success(&bench.serve_once(&["--ticks", "1"]));
    assert!(!stdout.contains("milestone: canonical-ready"), "{stdout}");
    assert_eq!(bench.session_count(), sessions_before + 1);
    assert_eq!(bench.session_runtime(KIM_SESSION), bench.kim_id());
}

/// Witness (e) at the CLI: a refusal is a startup error naming the entry,
/// raised before the daemon binds or prints its ready line.
#[test]
fn a_bad_enrollment_entry_refuses_startup_before_binding() {
    let bench = Bench::new();
    std::fs::remove_dir_all(&bench.kim).unwrap();
    let output = bench.serve_once(&["--json"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("runtime #1"), "{stderr}");
    assert!(stderr.contains("not an existing directory"), "{stderr}");
    assert!(stderr.contains("claude-code"), "{stderr}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.contains("\"event\":\"ready\""), "{stdout}");
    assert!(!bench.db.exists());
}

#[path = "enrolled_serve/metadata_ready.rs"]
mod metadata_ready;
