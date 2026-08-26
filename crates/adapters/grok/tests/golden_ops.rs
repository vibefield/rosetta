//! Flat canonical-op goldens for Grok's authoritative `updates` stream.
//!
//! Regenerate after reviewing an intentional normalizer change with:
//! `UPDATE_GOLDEN=1 cargo test -p rosetta-adapter-grok --test golden_ops`.

use std::path::{Path, PathBuf};

use rosetta_adapter_grok::{GrokAdapter, spec};
use rosetta_adapter_support::adapter::{
    AgentAdapter, FileStat, LineStatus, NormalizeCtx, StreamDriver,
};
use rosetta_core::{IngestOp, ids};
use serde_json::json;

const DATA_ROOT: &str = "~/.grok";
const HEAD: usize = 4 * 1024;

fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../groundtruth/grok/fixtures")
}

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

fn fixture_files(name: &str) -> Vec<PathBuf> {
    walkdir::WalkDir::new(fixtures_root())
        .sort_by_file_name()
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| entry.into_path())
        .filter(|path| {
            path.file_name().is_some_and(|file| file == name)
                && path.to_string_lossy().contains("grok-home/sessions/")
        })
        .collect()
}

fn scenarios() -> Vec<(String, String, PathBuf)> {
    let root = fixtures_root();
    fixture_files("updates.jsonl")
        .into_iter()
        .map(|path| {
            let relative = path.strip_prefix(&root).expect("fixture below root");
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
            (version, scenario, path)
        })
        .collect()
}

fn relative_path(path: &Path) -> String {
    path.to_string_lossy()
        .split_once("grok-home/")
        .expect("fixture lives below grok-home")
        .1
        .to_owned()
}

fn context(adapter: &GrokAdapter, native_session_id: &str) -> NormalizeCtx {
    let runtime_id = ids::runtime_id(adapter.kind(), DATA_ROOT);
    let session_id = ids::session_id(&runtime_id, native_session_id);
    let mut ctx = NormalizeCtx::new(runtime_id, session_id);
    ctx.state = adapter.init_state();
    ctx
}

fn native_session(adapter: &GrokAdapter, path: &Path, text: &str) -> String {
    let stat = FileStat {
        relative_path: relative_path(path),
        size: text.len() as u64,
        modified_ms: 0,
    };
    let head = &text.as_bytes()[..text.len().min(HEAD)];
    adapter
        .skim(spec::UPDATES, &stat, head, b"")
        .session_id
        .expect("updates has a native session id")
}

fn offsets(text: &str) -> Vec<i64> {
    let mut offset = 0i64;
    text.lines()
        .map(|line| {
            let at = offset;
            offset += line.len() as i64 + 1;
            at
        })
        .collect()
}

fn ingest_range(
    adapter: &GrokAdapter,
    ctx: &mut NormalizeCtx,
    text: &str,
    start: usize,
    end: usize,
) -> (Vec<IngestOp>, Vec<String>) {
    let lines: Vec<_> = text.lines().collect();
    let offsets = offsets(text);
    let mut ops = Vec::new();
    let mut unhealthy = Vec::new();
    for index in start..end {
        ctx.src_cursor = Some(index as i64 + 1);
        ctx.src_offset = Some(offsets[index]);
        let outcome = adapter.ingest_line(ctx, spec::UPDATES, lines[index].as_bytes());
        if outcome.status != LineStatus::Parsed {
            unhealthy.push(format!("line {}: {:?}", index + 1, outcome.status));
        }
        ops.extend(outcome.ops);
    }
    (ops, unhealthy)
}

fn normalize(path: &Path) -> (Vec<IngestOp>, Vec<String>) {
    let adapter = GrokAdapter;
    let text = std::fs::read_to_string(path).expect("read fixture");
    let native = native_session(&adapter, path, &text);
    let mut ctx = context(&adapter, &native);
    ingest_range(&adapter, &mut ctx, &text, 0, text.lines().count())
}

#[test]
fn corpus_exists() {
    assert!(
        !scenarios().is_empty(),
        "no updates fixtures below {}",
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
            std::fs::write(&golden, rendered).unwrap();
            continue;
        }
        let expected = std::fs::read_to_string(&golden).unwrap_or_else(|error| {
            panic!(
                "{}: {error} — regenerate with UPDATE_GOLDEN=1 cargo test \
                 -p rosetta-adapter-grok --test golden_ops",
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
fn checkpoint_restore_matches_one_shot() {
    let adapter = GrokAdapter;
    let path = fixture_files("updates.jsonl")
        .into_iter()
        .find(|path| path.to_string_lossy().contains("/tool-use/"))
        .expect("tool-use updates fixture");
    let text = std::fs::read_to_string(&path).unwrap();
    let native = native_session(&adapter, &path, &text);
    let total = text.lines().count();
    let split = 3;

    let mut one_shot_ctx = context(&adapter, &native);
    let (one_shot, unhealthy) = ingest_range(&adapter, &mut one_shot_ctx, &text, 0, total);
    assert!(unhealthy.is_empty());

    let mut first_ctx = context(&adapter, &native);
    let (mut resumed, unhealthy) = ingest_range(&adapter, &mut first_ctx, &text, 0, split);
    assert!(unhealthy.is_empty());
    let checkpoint = adapter
        .checkpoint_state(first_ctx.state.as_deref())
        .unwrap()
        .expect("state checkpoint");

    let mut second_ctx = NormalizeCtx::new(first_ctx.runtime_id, first_ctx.session_id);
    second_ctx.next_ordinal = first_ctx.next_ordinal;
    second_ctx.agent_version = first_ctx.agent_version;
    second_ctx.state = adapter.restore_state(Some(&checkpoint)).unwrap();
    let (remaining, unhealthy) = ingest_range(&adapter, &mut second_ctx, &text, split, total);
    assert!(unhealthy.is_empty());
    resumed.extend(remaining);

    assert_eq!(resumed, one_shot);
}

#[test]
fn invalid_checkpoint_is_rejected() {
    let adapter = GrokAdapter;
    assert!(
        adapter
            .restore_state(Some(&json!({"messages": "not-a-map"})))
            .is_err()
    );
}

#[test]
fn chunks_refine_one_logical_message() {
    let adapter = GrokAdapter;
    let mut ctx = context(&adapter, "native");
    let lines = [
        r#"{"timestamp":1,"method":"session/update","params":{"sessionId":"native","_meta":{"eventId":"e1","agentTimestampMs":1000,"promptId":"p","streamStartMs":1000,"turnStartMs":999,"totalTokens":1,"updateType":"AgentMessageChunk","chunkId":0},"update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"hel"}}}}"#,
        r#"{"timestamp":1,"method":"session/update","params":{"sessionId":"native","_meta":{"eventId":"e2","agentTimestampMs":1001,"promptId":"p","streamStartMs":1000,"turnStartMs":999,"totalTokens":1,"updateType":"AgentMessageChunk","chunkId":1},"update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"lo"}}}}"#,
    ];
    let mut messages = Vec::new();
    for line in lines {
        let outcome = adapter.ingest_line(&mut ctx, spec::UPDATES, line.as_bytes());
        assert_eq!(outcome.status, LineStatus::Parsed);
        messages.extend(outcome.ops.into_iter().filter_map(|op| match op {
            IngestOp::Message(row) => Some(row),
            _ => None,
        }));
    }
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].timeline_id, messages[1].timeline_id);
    assert_eq!(messages[0].ordinal, messages[1].ordinal);
    assert_eq!(messages[0].content.as_deref(), Some("hel"));
    assert_eq!(messages[1].content.as_deref(), Some("hello"));
}

#[test]
fn overlapping_rails_are_inventory_and_emit_nothing() {
    let adapter = GrokAdapter;
    let mut replayed = 0usize;
    for (stream, name) in [
        (spec::CHAT_HISTORY, "chat_history.jsonl"),
        (spec::EVENTS, "events.jsonl"),
    ] {
        assert!(adapter.stream_driver(stream).is_none());
        for path in fixture_files(name) {
            let text = std::fs::read_to_string(&path).unwrap();
            let stat = FileStat {
                relative_path: relative_path(&path),
                size: text.len() as u64,
                modified_ms: 0,
            };
            let skim = adapter.skim(stream, &stat, text.as_bytes(), b"");
            assert!(skim.session_id.is_none());
            assert!(skim.ops.is_empty());

            let mut ctx = context(&adapter, "inventory");
            for line in text.lines().filter(|line| !line.trim().is_empty()) {
                replayed += 1;
                let outcome = adapter.ingest_line(&mut ctx, stream, line.as_bytes());
                assert_eq!(outcome.status, LineStatus::Parsed);
                assert!(outcome.ops.is_empty());
            }
        }
    }
    assert!(replayed > 0);
    assert_eq!(
        adapter.stream_driver(spec::UPDATES),
        Some(StreamDriver::JsonLines)
    );
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
        for needle in [
            concat!("/", "Users", "/"),
            "/home/",
            concat!("C:\\\\", "Users"),
        ] {
            assert!(
                !text.contains(needle),
                "{} contains a local path ({needle})",
                entry.path().display()
            );
        }
    }
}
