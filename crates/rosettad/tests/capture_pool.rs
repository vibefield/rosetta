//! Fresh serve's capture pool, end to end: the default pooled build installs
//! the serial build's (`--capture-workers 0`) projection, its ledger holds the
//! serial build's source entries, it reports its pool, and a staged ledger the
//! pool built resumes on the next boot. With `--cold-ledger-codec
//! zstd-frames:N` the workers compress the ledger instead: the same
//! projection and source entries, and nothing left to compress.

use std::path::Path;
use std::process::Command;

use rosetta_adapter_claude_code::ClaudeCodeAdapter;
use rosetta_adapter_support::AgentAdapter;
use rosetta_bench_corpus::{Agent, CorpusSpec, generate};
use rosetta_pipeline::{CapturePool, expand_data_root};

fn rosettad(args: &[&str]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_rosettad"))
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{args:?}\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn serve(home: &Path, db: &Path, extra: &[&str]) -> String {
    let mut args = vec![
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
    ];
    args.extend_from_slice(extra);
    rosettad(&args)
}

/// A Claude home of generated sessions.
fn claude_home(root: &Path) {
    let data = expand_data_root(&ClaudeCodeAdapter.layout().data_root, root);
    std::fs::create_dir_all(&data).unwrap();
    generate(&CorpusSpec::new(Agent::ClaudeCode, 16, 3_000, 41), &data).unwrap();
}

/// Every message and session a projection shows, in a stable order.
fn projection(db: &Path) -> (Vec<String>, Vec<String>) {
    let conn = rusqlite::Connection::open(db).unwrap();
    let rows = |sql: &str| {
        conn.prepare(sql)
            .unwrap()
            .query_map([], |row| {
                (0..row.as_ref().column_count())
                    .map(|index| row.get::<_, rusqlite::types::Value>(index))
                    .collect::<Result<Vec<_>, _>>()
                    .map(|values| format!("{values:?}"))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };
    (
        rows(
            "SELECT timeline_id, session_id, ordinal, type, content, detail, ts FROM message ORDER BY timeline_id",
        ),
        rows("SELECT id, title, started_at, updated_at, stream_state FROM session ORDER BY id"),
    )
}

/// The root source digest of `ledger verify`: raw and seal entries only.
fn source_digest(db: &Path) -> String {
    let report: serde_json::Value = serde_json::from_str(
        rosettad(&["ledger", "verify", "--db", db.to_str().unwrap(), "--json"]).trim(),
    )
    .unwrap();
    report["sourceDigest"].as_str().unwrap().to_owned()
}

#[test]
fn a_pooled_fresh_serve_installs_the_serial_projection_and_ledger() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    claude_home(&home);
    let serial_db = temp.path().join("serial/rosetta.db");
    let pooled_db = temp.path().join("pooled/rosetta.db");

    serve(&home, &serial_db, &["--capture-workers", "0"]);
    let pooled = serve(&home, &pooled_db, &["--profile"]);
    assert!(pooled.contains("milestone: canonical-ready"), "{pooled}");
    let pool = pooled
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|event| event["stage"] == "capture-pool")
        .expect("the pool's stage profile");
    assert_eq!(pool["workers"], CapturePool::default_workers());
    assert!(pool["steps"].as_u64().unwrap() > 0, "{pool}");

    let (messages, sessions) = projection(&serial_db);
    assert!(messages.len() > 1_000, "{} messages", messages.len());
    assert_eq!(projection(&pooled_db), (messages, sessions));
    assert_eq!(source_digest(&pooled_db), source_digest(&serial_db));
}

#[test]
fn a_pooled_staged_ledger_rebuilds_and_finishes_on_next_boot() {
    staged_ledger_rebuilds_and_finishes_on_next_boot(&[]);
}

#[test]
fn a_zstd_frames_staged_ledger_rebuilds_and_finishes_on_next_boot() {
    staged_ledger_rebuilds_and_finishes_on_next_boot(&["--cold-ledger-codec", "zstd-frames:3"]);
}

fn staged_ledger_rebuilds_and_finishes_on_next_boot(first_boot: &[&str]) {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    claude_home(&home);
    let db = temp.path().join("archive/rosetta.db");
    serve(&home, &db, first_boot);
    let finished = projection(&db);

    // Turn the pool-built ledger back into a staged generation, as an
    // interrupted first boot leaves it.
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
        "runtime_roots": { "claude-code": home.join(".claude").to_string_lossy() },
    });
    std::fs::write(
        staged.join(".cold-generation.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
    let runtime_dir = std::fs::read_dir(&staged)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("claude-code"))
        })
        .expect("the runtime's ledger directory");
    let first_segment = std::fs::read(runtime_dir.join("00000000.rledger")).unwrap();
    let container = if first_boot.is_empty() {
        b"RLG1"
    } else {
        b"RLZ1"
    };
    assert_eq!(&first_segment[..4], container);

    let resumed = serve(&home, &db, &[]);
    assert!(resumed.contains("resuming cold generation"), "{resumed}");
    assert!(resumed.contains("milestone: canonical-ready"), "{resumed}");
    assert!(!staged.exists());
    assert_eq!(projection(&db), finished);
}

#[test]
fn a_zstd_frames_cold_ledger_installs_the_plain_projection_with_nothing_to_compress() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    claude_home(&home);
    let plain_db = temp.path().join("plain/rosetta.db");
    let frames_db = temp.path().join("frames/rosetta.db");

    serve(&home, &plain_db, &[]);
    let frames = serve(
        &home,
        &frames_db,
        &["--cold-ledger-codec", "zstd-frames:1", "--profile"],
    );
    assert!(frames.contains("milestone: canonical-ready"), "{frames}");
    assert_eq!(projection(&frames_db), projection(&plain_db));
    assert_eq!(source_digest(&frames_db), source_digest(&plain_db));

    let verify: serde_json::Value = serde_json::from_str(
        rosettad(&[
            "ledger",
            "verify",
            "--db",
            frames_db.to_str().unwrap(),
            "--json",
        ])
        .trim(),
    )
    .unwrap();
    let segments: Vec<&serde_json::Value> = verify["runtimes"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|runtime| runtime["segments"].as_array().unwrap())
        .collect();
    assert!(!segments.is_empty());
    for segment in &segments {
        assert_eq!(segment["container"], "zstd", "{segment}");
        assert_eq!(segment["clean"], true, "{segment}");
    }
    let disk: u64 = segments
        .iter()
        .map(|segment| segment["diskBytes"].as_u64().unwrap())
        .sum();
    let decoded: u64 = segments
        .iter()
        .map(|segment| segment["decodedBytes"].as_u64().unwrap())
        .sum();
    assert!(disk * 3 < decoded, "{disk} on disk for {decoded} decoded");

    // What the background pass would do: nothing.
    let compact: serde_json::Value = serde_json::from_str(
        rosettad(&[
            "ledger",
            "compact",
            "--db",
            frames_db.to_str().unwrap(),
            "--json",
        ])
        .trim(),
    )
    .unwrap();
    for runtime in compact["runtimes"].as_array().unwrap() {
        assert_eq!(runtime["rewritten"], 0, "{runtime}");
        assert_eq!(runtime["unchanged"], runtime["segments"], "{runtime}");
    }
    assert_eq!(source_digest(&frames_db), source_digest(&plain_db));
}

#[test]
fn the_cold_ledger_codec_flag_takes_plain_or_a_zstd_frames_level() {
    for bad in [
        "zstd",
        "zstd-frames",
        "zstd-frames:0",
        "zstd-frames:20",
        "zstd-frames:x",
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_rosettad"))
            .args([
                "serve",
                "--db",
                "/nonexistent/rosetta.db",
                "--cold-ledger-codec",
                bad,
            ])
            .output()
            .unwrap();
        assert!(!output.status.success(), "{bad}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("--cold-ledger-codec wants"),
            "{bad}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
