//! The kill switch end to end (R1 invariant I5, evidence X): `rosettad serve
//! --experiment-cold-admission runtime` walks a root-tree runtime in path
//! order and admits it whole, so its raw ledger's order-sensitive source
//! digest is the pre-R1 path walk's — the in-process harness's runtime
//! admission over the `path_ordered` code path, which is HEAD's walk —
//! while the declared build (newest trees first, in lanes) frames the same
//! sources in another order: an equal source *set* digest, a different
//! source digest. Both installs hold the same logical projection.
//!
//! The switch has two halves, runtime admission (no tree map) and the walk's
//! `admission_override`, and either alone forces the path walk. (Mutation: a
//! switch that is parsed but sets neither leaves the recency walk in place,
//! and the digest differs.)

mod support;

use std::path::Path;
use std::process::Command;

use rosetta_adapter_claude_code::ClaudeCodeAdapter;
use rusqlite::Connection;
use tempfile::TempDir;

use support::fixtures::giant_newest;
use support::harness::cold_store;
use support::projection_dump::{Dump, assert_same_dump, dump_conn};
use support::writer::{BuildSpec, build};

fn serve(home: &Path, db: &Path, extra: &[&str]) {
    let output = Command::new(env!("CARGO_BIN_EXE_rosettad"))
        .args(["serve", "--db", db.to_str().unwrap(), "--home"])
        .arg(home)
        .args([
            "--agent",
            "claude-code",
            "--addr",
            "127.0.0.1:0",
            "--ticks",
            "0",
        ])
        .args(extra)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The one runtime's `ledger verify` digests: (source, source set).
fn digests(db: &Path) -> (String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_rosettad"))
        .args(["ledger", "verify", "--db", db.to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let runtimes = report["runtimes"].as_array().unwrap();
    assert_eq!(runtimes.len(), 1, "{report}");
    (
        runtimes[0]["sourceDigest"].as_str().unwrap().to_owned(),
        runtimes[0]["sourceSetDigest"].as_str().unwrap().to_owned(),
    )
}

/// An installed projection's logical dump, without the readiness and
/// search bookkeeping or the runtime row's discovery time.
fn installed(db: &Path) -> Dump {
    let mut dump = dump_conn(&Connection::open(db).unwrap(), &[]);
    for table in ["agent_runtime", "projection_state", "search_dirty"] {
        dump.remove(table);
    }
    dump
}

#[test]
fn the_kill_switch_reproduces_the_path_walks_ledger_source_digest() {
    let home = TempDir::new().unwrap();
    // Newest trees first is not path order here: the giant is the newest.
    giant_newest(home.path(), 12, 1024 * 1024, 3);
    let head = build(
        &ClaudeCodeAdapter,
        home.path(),
        &BuildSpec::reference(),
        &mut cold_store(),
        None,
    );
    assert_eq!(head.result, Ok(()));
    assert!(head.path_walk, "the reference walks paths");

    let dir = TempDir::new().unwrap();
    let switched = dir.path().join("switched/rosetta.db");
    serve(
        home.path(),
        &switched,
        &["--experiment-cold-admission", "runtime"],
    );
    let declared = dir.path().join("declared/rosetta.db");
    serve(home.path(), &declared, &[]);

    let (switched_source, switched_set) = digests(&switched);
    let (declared_source, declared_set) = digests(&declared);
    assert_eq!(
        switched_source, head.source_digest,
        "the kill switch frames the pre-R1 path walk's ledger"
    );
    assert_eq!(switched_set, head.source_set_digest);
    assert_ne!(
        declared_source, head.source_digest,
        "the declared recency walk frames another order"
    );
    assert_eq!(declared_set, switched_set, "the same sources either way");
    assert_same_dump(
        "kill switch vs declared",
        &installed(&switched),
        &installed(&declared),
    );
}
