//! Flat golden op streams for every committed Kimi wire file.
//!
//! Regenerate after reviewing an intentional projection change with:
//! `UPDATE_GOLDEN=1 cargo test -p rosetta-adapter-kimi-code --test golden_ops`.

use std::path::{Path, PathBuf};

use rosetta_adapter_kimi_code::KimiCodeAdapter;
use rosetta_adapter_support::{AgentAdapter, FileStat, LineStatus, NormalizeCtx};
use rosetta_core::{IngestOp, ids};

const DATA_ROOT: &str = "~/.kimi-code";
const MARKER: &str = "kimi-home/";
const WINDOW: usize = 64 * 1024;

fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../groundtruth/kimi-code/fixtures")
}

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

struct WireFixture {
    version: String,
    scenario: String,
    agent: String,
    relative: String,
    path: PathBuf,
}

fn fixtures() -> Vec<WireFixture> {
    let root = fixtures_root();
    let mut fixtures: Vec<_> = walkdir::WalkDir::new(&root)
        .sort_by_file_name()
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .filter_map(|entry| {
            let path = entry.into_path();
            if path.file_name().is_none_or(|name| name != "wire.jsonl") {
                return None;
            }
            let full = path.to_string_lossy();
            let (fixture_prefix, relative) = full.split_once(MARKER)?;
            if KimiCodeAdapter
                .layout()
                .matching_stream(relative)
                .map(|stream| stream.id.as_str())
                != Some("wire")
            {
                return None;
            }
            let fixture_path = Path::new(fixture_prefix).strip_prefix(&root).ok()?;
            let mut parts = fixture_path.components();
            let version = parts.next()?.as_os_str().to_string_lossy().into_owned();
            let scenario = parts.next()?.as_os_str().to_string_lossy().into_owned();
            let agent = relative
                .split('/')
                .nth(4)
                .expect("wire glob has an agent segment")
                .to_owned();
            Some(WireFixture {
                version,
                scenario,
                agent,
                relative: relative.to_owned(),
                path,
            })
        })
        .collect();
    fixtures.sort_by(|left, right| {
        (&left.version, &left.scenario, &left.agent).cmp(&(
            &right.version,
            &right.scenario,
            &right.agent,
        ))
    });
    fixtures
}

fn normalize(fixture: &WireFixture) -> (Vec<IngestOp>, Vec<String>) {
    let adapter = KimiCodeAdapter;
    let text = std::fs::read_to_string(&fixture.path).unwrap();
    let bytes = text.as_bytes();
    let head = &bytes[..bytes.len().min(WINDOW)];
    let tail = &bytes[bytes.len().saturating_sub(WINDOW)..];
    let stat = FileStat {
        relative_path: fixture.relative.clone(),
        size: bytes.len() as u64,
        modified_ms: 0,
    };
    let skim = adapter.skim("wire", &stat, head, tail);
    let native = skim.session_id.expect("wire path has a native session id");
    let runtime_id = ids::runtime_id(adapter.kind(), DATA_ROOT);
    let mut ctx = NormalizeCtx::new(&runtime_id, ids::session_id(&runtime_id, &native));
    ctx.native_session_id = Some(native);
    ctx.agent_version = skim.agent_version;
    ctx.state = adapter.init_state();

    let mut ops = Vec::new();
    let mut unhealthy = Vec::new();
    let mut offset = 0i64;
    for (index, line) in text.lines().enumerate() {
        ctx.src_cursor = Some(index as i64 + 1);
        ctx.src_offset = Some(offset);
        offset += line.len() as i64 + 1;
        let outcome = adapter.ingest_line(&mut ctx, "wire", line.as_bytes());
        if outcome.status != LineStatus::Parsed {
            unhealthy.push(format!("line {}: {:?}", index + 1, outcome.status));
        }
        ops.extend(outcome.ops);
    }
    (ops, unhealthy)
}

#[test]
fn corpus_exists_and_contains_both_agent_roles() {
    let fixtures = fixtures();
    assert!(!fixtures.is_empty());
    assert!(fixtures.iter().any(|fixture| fixture.agent == "main"));
    assert!(fixtures.iter().any(|fixture| fixture.agent != "main"));
}

#[test]
fn fixture_ops_match_flat_goldens() {
    let update = std::env::var_os("UPDATE_GOLDEN").is_some();
    for fixture in fixtures() {
        let (ops, unhealthy) = normalize(&fixture);
        assert!(
            unhealthy.is_empty(),
            "{}/{}-{} did not fully normalize:\n{}",
            fixture.version,
            fixture.scenario,
            fixture.agent,
            unhealthy.join("\n")
        );
        assert!(!ops.is_empty(), "fixture produced no canonical operations");

        let path = golden_dir()
            .join(&fixture.version)
            .join(format!("{}-{}.json", fixture.scenario, fixture.agent));
        let rendered = format!("{}\n", serde_json::to_string_pretty(&ops).unwrap());
        if update {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, rendered).unwrap();
            continue;
        }
        let expected = std::fs::read_to_string(&path).unwrap_or_else(|error| {
            panic!(
                "{}: {error} — regenerate with UPDATE_GOLDEN=1 cargo test -p \
                 rosetta-adapter-kimi-code --test golden_ops",
                path.display()
            )
        });
        assert_eq!(
            rendered, expected,
            "normalized ops changed for {}/{}-{}",
            fixture.version, fixture.scenario, fixture.agent
        );
    }
}

#[test]
fn replay_is_deterministic() {
    for fixture in fixtures() {
        let (first, _) = normalize(&fixture);
        let (second, _) = normalize(&fixture);
        assert_eq!(
            first, second,
            "{}/{}-{} replayed differently",
            fixture.version, fixture.scenario, fixture.agent
        );
    }
}

#[test]
fn goldens_carry_no_home_paths() {
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
