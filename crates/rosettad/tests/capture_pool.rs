//! Fresh serve's capture pool, end to end: the default pooled build installs
//! the serial build's (`--capture-workers 0`) projection, its ledger holds the
//! serial build's source entries, it reports its pool, and a staged ledger the
//! pool built resumes on the next boot.

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
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    claude_home(&home);
    let db = temp.path().join("archive/rosetta.db");
    serve(&home, &db, &[]);
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

    let resumed = serve(&home, &db, &[]);
    assert!(resumed.contains("resuming cold generation"), "{resumed}");
    assert!(resumed.contains("milestone: canonical-ready"), "{resumed}");
    assert!(!staged.exists());
    assert_eq!(projection(&db), finished);
}
