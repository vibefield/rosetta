//! Flat canonical op goldens for every committed Pi fixture scenario.
//!
//! Regenerate with `UPDATE_GOLDEN=1 cargo test -p rosetta-adapter-pi
//! --test golden_ops`, then review the resulting op-stream diff.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rosetta_adapter_pi::PiAdapter;
use rosetta_adapter_support::adapter::{AgentAdapter, FileStat, LineStatus, NormalizeCtx};
use rosetta_core::{IngestOp, ids};

const DATA_ROOT: &str = "~/.pi";
const HEAD: usize = 64 * 1024;

fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../groundtruth/pi/fixtures")
}

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

fn scenarios() -> BTreeMap<(String, String), Vec<PathBuf>> {
    let root = fixtures_root();
    let mut scenarios: BTreeMap<(String, String), Vec<PathBuf>> = BTreeMap::new();
    for entry in walkdir::WalkDir::new(&root)
        .sort_by_file_name()
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
    {
        let path = entry.into_path();
        if path
            .extension()
            .is_none_or(|extension| extension != "jsonl")
            || !path.to_string_lossy().contains("pi-home/agent/sessions/")
        {
            continue;
        }
        let relative = path.strip_prefix(&root).expect("fixture under root");
        let mut parts = relative.components();
        let version = parts
            .next()
            .expect("version directory")
            .as_os_str()
            .to_string_lossy()
            .into_owned();
        let scenario = parts
            .next()
            .expect("scenario directory")
            .as_os_str()
            .to_string_lossy()
            .into_owned();
        scenarios.entry((version, scenario)).or_default().push(path);
    }
    for paths in scenarios.values_mut() {
        paths.sort();
    }
    scenarios
}

fn replay(path: &Path) -> (Vec<IngestOp>, Vec<String>) {
    let adapter = PiAdapter;
    let text = std::fs::read_to_string(path).expect("fixture is readable");
    let relative = path
        .to_string_lossy()
        .split_once("pi-home/")
        .expect("fixture lives under pi-home/")
        .1
        .to_owned();
    let stat = FileStat {
        relative_path: relative,
        size: text.len() as u64,
        modified_ms: 0,
    };
    let head = &text.as_bytes()[..text.len().min(HEAD)];
    let native = adapter
        .skim("session", &stat, head, b"")
        .session_id
        .expect("Pi session identity comes from its header or path");
    let runtime_id = ids::runtime_id(adapter.kind(), DATA_ROOT);
    let mut ctx = NormalizeCtx::new(&runtime_id, ids::session_id(&runtime_id, &native));
    ctx.state = adapter.init_state();

    let mut ops = Vec::new();
    let mut unhealthy = Vec::new();
    let mut offset = 0i64;
    for (index, line) in text.lines().enumerate() {
        ctx.src_cursor = Some(index as i64 + 1);
        ctx.src_offset = Some(offset);
        offset += line.len() as i64 + 1;
        let outcome = adapter.ingest_line(&mut ctx, "session", line.as_bytes());
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
        "no Pi session fixtures under {}",
        fixtures_root().display()
    );
}

#[test]
fn flat_golden_ops_match() {
    let update = std::env::var_os("UPDATE_GOLDEN").is_some();
    for ((version, scenario), paths) in scenarios() {
        let mut ops = Vec::new();
        let mut unhealthy = Vec::new();
        for path in paths {
            let (file_ops, file_unhealthy) = replay(&path);
            ops.extend(file_ops);
            unhealthy.extend(file_unhealthy);
        }
        assert!(
            unhealthy.is_empty(),
            "{version}/{scenario} did not fully normalize:\n{}",
            unhealthy.join("\n")
        );
        assert!(!ops.is_empty(), "{version}/{scenario} produced no ops");

        // Deliberately a bare Vec<IngestOp>, matching the Codex golden shape.
        let rendered = format!("{}\n", serde_json::to_string_pretty(&ops).unwrap());
        let golden = golden_dir().join(&version).join(format!("{scenario}.json"));
        if update {
            std::fs::create_dir_all(golden.parent().unwrap()).unwrap();
            std::fs::write(&golden, &rendered).unwrap();
            continue;
        }
        let expected = std::fs::read_to_string(&golden).unwrap_or_else(|error| {
            panic!(
                "{}: {error} — regenerate with UPDATE_GOLDEN=1 cargo test -p rosetta-adapter-pi --test golden_ops",
                golden.display()
            )
        });
        assert_eq!(
            rendered, expected,
            "normalized ops changed for {version}/{scenario}; review and regenerate"
        );
    }
}

#[test]
fn replay_is_deterministic() {
    for ((version, scenario), paths) in scenarios() {
        for path in paths {
            let (first, _) = replay(&path);
            let (second, _) = replay(&path);
            assert_eq!(first, second, "{version}/{scenario} replayed differently");
        }
    }
}

#[test]
fn goldens_carry_no_personal_paths() {
    for entry in walkdir::WalkDir::new(golden_dir())
        .sort_by_file_name()
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
    {
        let text = std::fs::read_to_string(entry.path()).unwrap();
        for needle in [concat!("/", "Users/"), "/home/", concat!("C:\\\\", "Users")] {
            assert!(
                !text.contains(needle),
                "{} contains a local path ({needle})",
                entry.path().display()
            );
        }
    }
}
