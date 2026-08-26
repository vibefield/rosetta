//! Golden normalization of the sole canonical transcript owner.
//!
//! Regenerate with:
//! `UPDATE_GOLDEN=1 cargo test -p rosetta-adapter-antigravity --test golden_ops`.

use std::path::{Path, PathBuf};

use rosetta_adapter_antigravity::{AntigravityAdapter, spec};
use rosetta_adapter_support::adapter::{AgentAdapter, FileStat, LineStatus, NormalizeCtx};
use rosetta_core::{IngestOp, ids};

const DATA_ROOT: &str = "~/.gemini/antigravity-cli";
const HEAD: usize = 16 * 1024;

fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../groundtruth/antigravity/fixtures")
}

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

/// One file per native conversation. The full and chunk paths are proven
/// byte-identical inventory and are intentionally absent from this list.
fn scenarios() -> Vec<(String, String, PathBuf)> {
    let root = fixtures_root();
    let mut found: Vec<_> = walkdir::WalkDir::new(&root)
        .sort_by_file_name()
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| entry.into_path())
        .filter(|path| {
            path.to_string_lossy()
                .ends_with("/.system_generated/logs/transcript.jsonl")
        })
        .map(|path| {
            let relative = path.strip_prefix(&root).unwrap();
            let mut parts = relative.components();
            let version = parts.next().expect("version directory").as_os_str();
            let scenario = parts.next().expect("scenario directory").as_os_str();
            (
                version.to_string_lossy().into_owned(),
                scenario.to_string_lossy().into_owned(),
                path,
            )
        })
        .collect();
    found.sort();
    found
}

fn source(path: &Path) -> (String, String, String) {
    let text = std::fs::read_to_string(path).unwrap();
    let relative = path
        .to_string_lossy()
        .split_once("antigravity-cli/")
        .expect("fixture is under antigravity-cli/")
        .1
        .to_owned();
    let stat = FileStat {
        relative_path: relative,
        size: text.len() as u64,
        modified_ms: 0,
    };
    let native = AntigravityAdapter
        .skim(
            spec::TRANSCRIPT,
            &stat,
            &text.as_bytes()[..text.len().min(HEAD)],
            b"",
        )
        .session_id
        .expect("brain path carries the conversation id");
    let runtime = ids::runtime_id("antigravity", DATA_ROOT);
    let session = ids::session_id(&runtime, &native);
    (text, runtime, session)
}

fn ingest_lines(
    ctx: &mut NormalizeCtx,
    text: &str,
    range: std::ops::Range<usize>,
) -> (Vec<IngestOp>, Vec<String>) {
    let lines: Vec<_> = text.lines().collect();
    let mut offsets = Vec::with_capacity(lines.len());
    let mut offset = 0i64;
    for line in &lines {
        offsets.push(offset);
        offset += line.len() as i64 + 1;
    }

    let mut ops = Vec::new();
    let mut unhealthy = Vec::new();
    for index in range {
        let line = lines[index];
        ctx.src_cursor = Some(index as i64 + 1);
        ctx.src_offset = Some(offsets[index]);
        let outcome = AntigravityAdapter.ingest_line(ctx, spec::TRANSCRIPT, line.as_bytes());
        if outcome.status != LineStatus::Parsed {
            unhealthy.push(format!("line {}: {:?}", index + 1, outcome.status));
        }
        ops.extend(outcome.ops);
    }
    (ops, unhealthy)
}

fn normalize(path: &Path) -> (Vec<IngestOp>, Vec<String>) {
    let (text, runtime, session) = source(path);
    let mut ctx = NormalizeCtx::new(runtime, session);
    ctx.state = AntigravityAdapter.init_state();
    ingest_lines(&mut ctx, &text, 0..text.lines().count())
}

#[test]
fn corpus_exists() {
    assert!(
        !scenarios().is_empty(),
        "no primary transcript fixtures under {}",
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
                "{}: {error} — regenerate with UPDATE_GOLDEN=1 cargo test \
                 -p rosetta-adapter-antigravity --test golden_ops",
                golden.display()
            )
        });
        assert_eq!(
            rendered, expected,
            "normalized ops changed for {version}/{scenario}"
        );
    }
}

#[test]
fn replay_is_deterministic() {
    for (version, scenario, path) in scenarios() {
        assert_eq!(
            normalize(&path).0,
            normalize(&path).0,
            "{version}/{scenario} replayed differently"
        );
    }
}

#[test]
fn split_resume_equals_one_shot() {
    let adapter = AntigravityAdapter;
    for (version, scenario, path) in scenarios() {
        let one_shot = normalize(&path).0;
        let (text, runtime, session) = source(&path);
        let count = text.lines().count();

        for split in 0..=count {
            let mut before = NormalizeCtx::new(runtime.clone(), session.clone());
            before.state = adapter.init_state();
            let (mut split_ops, unhealthy) = ingest_lines(&mut before, &text, 0..split);
            assert!(unhealthy.is_empty());

            let checkpoint = adapter
                .checkpoint_state(before.state.as_deref())
                .unwrap()
                .expect("adapter state");
            let mut after = NormalizeCtx::new(runtime.clone(), session.clone());
            // The pipeline owns the generic ordinal; the adapter checkpoint owns
            // only its private pending-tool queue.
            after.next_ordinal = before.next_ordinal;
            after.state = adapter.restore_state(Some(&checkpoint)).unwrap();
            let (tail_ops, unhealthy) = ingest_lines(&mut after, &text, split..count);
            assert!(unhealthy.is_empty());
            split_ops.extend(tail_ops);

            assert_eq!(
                split_ops, one_shot,
                "{version}/{scenario} differed at split {split}"
            );
        }
    }
}

#[test]
fn goldens_carry_no_personal_paths() {
    for entry in walkdir::WalkDir::new(golden_dir())
        .sort_by_file_name()
        .into_iter()
        .filter_map(Result::ok)
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let text = std::fs::read_to_string(entry.path()).unwrap();
        for needle in [concat!("james", "yong"), concat!("/", "Users/")] {
            assert!(
                !text.contains(needle),
                "{} contains personal data ({needle})",
                entry.path().display()
            );
        }
    }
}
