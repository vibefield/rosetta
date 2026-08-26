//! Golden ops: the committed fixture corpus, normalized.
//!
//! `fixtures_replay.rs` proves every committed line PARSES; this proves what
//! each line BECOMES. The snapshots are the reviewable artifact — a diff here
//! is a change in what the projection would store, so read it before blessing
//! it.
//!
//! Regenerate: `UPDATE_GOLDEN=1 cargo test -p rosetta-adapter-codex`.

use std::path::{Path, PathBuf};

use rosetta_adapter_codex::CodexAdapter;
use rosetta_adapter_support::adapter::{AgentAdapter, FileStat, LineStatus, NormalizeCtx};
use rosetta_core::IngestOp;
use rosetta_core::ids;

/// The canonical data root, NOT a local one: the runtime id (and through it
/// every session and timeline id in the goldens) must not depend on whose
/// machine ran the test.
const DATA_ROOT: &str = "~/.codex";

/// Head window for the meta phase. The opening `session_meta` carries the full
/// base-instruction text, so the window has to clear ~22 KB.
const HEAD: usize = 64 * 1024;

fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../groundtruth/codex/fixtures")
}

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

/// Every rollout file in the corpus, keyed by the `<version>/<scenario>` pair
/// the fixture farm wrote it under.
fn scenarios() -> Vec<(String, String, PathBuf)> {
    let root = fixtures_root();
    let mut found: Vec<_> = walkdir::WalkDir::new(&root)
        .sort_by_file_name()
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| entry.into_path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "jsonl")
                && path.to_string_lossy().contains("codex-home/sessions/")
        })
        .map(|path| {
            let relative = path.strip_prefix(&root).unwrap();
            let mut parts = relative.components();
            let version = parts.next().expect("version dir").as_os_str();
            let scenario = parts.next().expect("scenario dir").as_os_str();
            (
                version.to_string_lossy().into_owned(),
                scenario.to_string_lossy().into_owned(),
                path.clone(),
            )
        })
        .collect();
    found.sort();
    found
}

/// Run one rollout file through the adapter exactly as the pipeline would:
/// meta phase for the native session id, then every line through the stream
/// phase with provenance stamped.
fn normalize(path: &Path) -> (Vec<IngestOp>, Vec<String>) {
    let adapter = CodexAdapter;
    let text = std::fs::read_to_string(path).unwrap();
    let relative = path
        .to_string_lossy()
        .split_once("codex-home/")
        .expect("fixture lives under codex-home/")
        .1
        .to_owned();

    let stat = FileStat {
        relative_path: relative,
        size: text.len() as u64,
        modified_ms: 0,
    };
    let head = &text.as_bytes()[..text.len().min(HEAD)];
    let native = adapter
        .skim("rollout", &stat, head, b"")
        .session_id
        .expect("a rollout opens with its session_meta");

    let runtime_id = ids::runtime_id(adapter.kind(), DATA_ROOT);
    let session_id = ids::session_id(&runtime_id, &native);
    let mut ctx = NormalizeCtx::new(runtime_id, session_id);
    ctx.state = adapter.init_state();

    let mut ops = Vec::new();
    let mut unhealthy = Vec::new();
    let mut offset = 0i64;
    for (index, line) in text.lines().enumerate() {
        ctx.src_cursor = Some(index as i64 + 1);
        ctx.src_offset = Some(offset);
        offset += line.len() as i64 + 1;

        let outcome = adapter.ingest_line(&mut ctx, "rollout", line.as_bytes());
        if outcome.status != LineStatus::Parsed {
            unhealthy.push(format!("line {}: {:?}", index + 1, outcome.status));
        }
        ops.extend(outcome.ops);
    }
    (ops, unhealthy)
}

#[test]
fn corpus_exists() {
    assert!(
        !scenarios().is_empty(),
        "no rollout fixtures under {} — run: python3 -m tools.groundtruth farm codex",
        fixtures_root().display()
    );
}

#[test]
fn golden_ops_match() {
    let update = std::env::var_os("UPDATE_GOLDEN").is_some();
    for (version, scenario, path) in scenarios() {
        let (ops, unhealthy) = normalize(&path);
        // Every line of the committed corpus must project cleanly: a hard error
        // is a schema claim that moved, an unknown type is a vocabulary that
        // grew, and both mean this adapter is behind its ground truth.
        assert!(
            unhealthy.is_empty(),
            "{version}/{scenario} did not fully normalize:\n{}",
            unhealthy.join("\n")
        );
        assert!(!ops.is_empty(), "{version}/{scenario} produced no ops");

        let golden = golden_dir().join(&version).join(format!("{scenario}.json"));
        let rendered = format!("{}\n", serde_json::to_string_pretty(&ops).unwrap());
        if update {
            std::fs::create_dir_all(golden.parent().unwrap()).unwrap();
            std::fs::write(&golden, &rendered).unwrap();
            continue;
        }
        let expected = std::fs::read_to_string(&golden).unwrap_or_else(|error| {
            panic!(
                "{}: {error} — regenerate with UPDATE_GOLDEN=1 cargo test -p rosetta-adapter-codex",
                golden.display()
            )
        });
        assert_eq!(
            rendered, expected,
            "normalized ops changed for {version}/{scenario} — review the diff, \
             then bless it with UPDATE_GOLDEN=1"
        );
    }
}

#[test]
fn replay_is_deterministic() {
    // `normalize()` is pure (CLAUDE.md): same bytes in, same ops out, so
    // re-ingesting a file is idempotent rather than duplicating rows.
    for (version, scenario, path) in scenarios() {
        let (first, _) = normalize(&path);
        let (second, _) = normalize(&path);
        assert_eq!(first, second, "{version}/{scenario} replayed differently");
    }
}

#[test]
fn goldens_carry_no_local_paths() {
    // The corpus is machine-independent by rule; a home directory leaking into
    // a committed snapshot means something read a live directory.
    // A missing directory yields nothing here; `golden_ops_match` is the test
    // that fails when the snapshots are absent.
    for entry in walkdir::WalkDir::new(golden_dir())
        .sort_by_file_name()
        .into_iter()
        .filter_map(Result::ok)
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let text = std::fs::read_to_string(entry.path()).unwrap();
        for needle in ["/Users/", "/home/", "C:\\\\Users"] {
            assert!(
                !text.contains(needle),
                "{} contains a local path ({needle})",
                entry.path().display()
            );
        }
    }
}
