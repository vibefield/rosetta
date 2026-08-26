//! Flat golden `Vec<IngestOp>` snapshots for every committed Copilot events
//! fixture. Regenerate with:
//! `UPDATE_GOLDEN=1 cargo test -p rosetta-adapter-copilot`.

use std::path::{Path, PathBuf};

use rosetta_adapter_copilot::CopilotAdapter;
use rosetta_adapter_support::adapter::{AgentAdapter, FileStat, LineStatus, NormalizeCtx};
use rosetta_core::{IngestOp, ids};

const DATA_ROOT: &str = "~/.copilot";
const HEAD: usize = 8 * 1024;

fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../groundtruth/copilot/fixtures")
}

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

fn scenarios() -> Vec<(String, String, PathBuf)> {
    let root = fixtures_root();
    let mut found: Vec<_> = walkdir::WalkDir::new(&root)
        .sort_by_file_name()
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| entry.into_path())
        .filter(|path| {
            path.file_name().is_some_and(|name| name == "events.jsonl")
                && path
                    .to_string_lossy()
                    .contains("copilot-home/session-state/")
        })
        .map(|path| {
            let relative = path.strip_prefix(&root).expect("fixture below root");
            let mut parts = relative.components();
            let version = parts.next().expect("version directory");
            let scenario = parts.next().expect("scenario directory");
            (
                version.as_os_str().to_string_lossy().into_owned(),
                scenario.as_os_str().to_string_lossy().into_owned(),
                path,
            )
        })
        .collect();
    found.sort();
    found
}

fn normalize(path: &Path) -> (Vec<IngestOp>, Vec<String>) {
    let adapter = CopilotAdapter;
    let text = std::fs::read_to_string(path).expect("read events fixture");
    let relative = path
        .to_string_lossy()
        .split_once("copilot-home/")
        .expect("fixture below copilot-home")
        .1
        .to_owned();
    let stat = FileStat {
        relative_path: relative,
        size: text.len() as u64,
        modified_ms: 0,
    };
    let head = &text.as_bytes()[..text.len().min(HEAD)];
    let skim = adapter.skim("events", &stat, head, b"");
    let native = skim
        .session_id
        .expect("events fixture has native session identity");

    // The directory and session.start carry the same stable native id in every
    // committed fixture. This is the pinned identity rule, not a UUID guess.
    let directory = stat
        .relative_path
        .strip_prefix("session-state/")
        .and_then(|path| path.strip_suffix("/events.jsonl"))
        .expect("canonical events path");
    assert_eq!(native, directory, "session.start and directory disagree");

    let runtime_id = ids::runtime_id(adapter.kind(), DATA_ROOT);
    let session_id = ids::session_id(&runtime_id, &native);
    let mut ctx = NormalizeCtx::new(runtime_id, session_id);
    ctx.agent_version = skim.agent_version;
    ctx.state = adapter.init_state();

    let mut ops = Vec::new();
    let mut unhealthy = Vec::new();
    let mut offset = 0i64;
    for (index, line) in text.lines().enumerate() {
        ctx.src_cursor = Some(index as i64 + 1);
        ctx.src_offset = Some(offset);
        offset += line.len() as i64 + 1;

        let outcome = adapter.ingest_line(&mut ctx, "events", line.as_bytes());
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
        "no events fixtures under {}",
        fixtures_root().display()
    );
}

#[test]
fn golden_ops_match() {
    let update = std::env::var_os("UPDATE_GOLDEN").is_some();
    for (version, scenario, path) in scenarios() {
        let (ops, unhealthy) = normalize(&path);
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
                "{}: {error} — regenerate with UPDATE_GOLDEN=1 cargo test -p rosetta-adapter-copilot",
                golden.display()
            )
        });
        assert_eq!(
            rendered, expected,
            "normalized ops changed for {version}/{scenario}; review then bless"
        );
    }
}

#[test]
fn replay_is_deterministic() {
    for (version, scenario, path) in scenarios() {
        let (first, _) = normalize(&path);
        let (second, _) = normalize(&path);
        assert_eq!(first, second, "{version}/{scenario} replayed differently");
    }
}

#[test]
fn goldens_carry_no_local_paths() {
    for entry in walkdir::WalkDir::new(golden_dir())
        .sort_by_file_name()
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
    {
        let text = std::fs::read_to_string(entry.path()).unwrap();
        let mac_home = ["/", "Users", "/"].concat();
        for needle in [mac_home.as_str(), "/home/", "C:\\\\Users"] {
            assert!(
                !text.contains(needle),
                "{} contains a local path ({needle})",
                entry.path().display()
            );
        }
    }
}
