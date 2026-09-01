use std::path::Path;
use std::process::Command;

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

    let store = rosetta_store::Store::open_progressive(&db).unwrap();
    store
        .conn()
        .execute(
            "INSERT INTO agent_runtime
               (id, kind, data_root, enabled, discovered_at)
             VALUES ('old-claude', 'claude-code', '/old/.claude', 1, 0)",
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
