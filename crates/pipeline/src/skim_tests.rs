//! The pooled skim is the serial skim: for real fixture inventories,
//! generated corpora and a tree with every Claude Code source shape, any pool
//! size yields the same report, sources and metadata batch, op for op.

use std::path::{Path, PathBuf};

use rosetta_adapter_antigravity::AntigravityAdapter;
use rosetta_adapter_claude_code::ClaudeCodeAdapter;
use rosetta_adapter_codex::CodexAdapter;
use rosetta_adapter_copilot::CopilotAdapter;
use rosetta_adapter_support::AgentAdapter;
use rosetta_bench_corpus::{Agent, CorpusSpec, generate};
use rosetta_store::Store;
use tempfile::TempDir;

use super::in_order;
use crate::{Discovery, IngestOptions, Instrument, discover, expand_data_root};

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let destination = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &destination);
        } else {
            std::fs::copy(entry.path(), destination).unwrap();
        }
    }
}

/// The harvested data root of every recorded fixture scenario of one agent.
fn fixture_roots(kind: &str) -> Vec<PathBuf> {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../groundtruth")
        .join(kind)
        .join("fixtures");
    let mut roots = Vec::new();
    for version in std::fs::read_dir(&fixtures).unwrap() {
        for scenario in std::fs::read_dir(version.unwrap().path()).unwrap() {
            let scenario = scenario.unwrap().path();
            roots.extend(
                std::fs::read_dir(&scenario)
                    .unwrap()
                    .map(|entry| entry.unwrap().path())
                    .filter(|path| path.is_dir()),
            );
        }
    }
    roots.sort();
    assert!(!roots.is_empty(), "{kind} has fixture scenarios");
    roots
}

fn discovered(adapter: &dyn AgentAdapter, home: &Path) -> Discovery {
    let discovery = discover(
        &[adapter],
        home,
        &IngestOptions::default(),
        &Instrument::new(),
    )
    .unwrap()
    .pop()
    .unwrap();
    assert!(
        discovery.installed,
        "{} corpus is discovered",
        adapter.kind()
    );
    discovery
}

/// Everything one metadata pass decides, rendered for comparison: the
/// report, the sources in stream order, and the batch in apply order.
fn planned(adapter: &dyn AgentAdapter, discovery: &Discovery, threads: usize) -> String {
    let mut runtime = discovery.runtime.clone();
    runtime.enabled = true;
    let store = Store::open_in_memory_for_cold_planning().unwrap();
    match crate::meta::plan_inventory(
        adapter,
        &runtime,
        &discovery.root,
        &store,
        &discovery.inventory,
        discovery.walk_truncated,
        &IngestOptions::default(),
        &Instrument::new(),
        threads,
    ) {
        Ok((report, files, ops)) => format!(
            "{report:?}\n{files:?}\n{}",
            serde_json::to_string(&ops).unwrap()
        ),
        Err(error) => format!("error: {error}"),
    }
}

fn assert_pool_invariant(label: &str, adapter: &dyn AgentAdapter, home: &Path) -> String {
    let discovery = discovered(adapter, home);
    let serial = planned(adapter, &discovery, 1);
    for threads in [2, 3, 8] {
        assert_eq!(
            planned(adapter, &discovery, threads),
            serial,
            "{label}: {threads} skim threads"
        );
    }
    serial
}

#[test]
fn every_fixture_inventory_skims_identically_on_the_pool() {
    let adapters: [(&dyn AgentAdapter, &str); 4] = [
        (&AntigravityAdapter, "antigravity"),
        (&ClaudeCodeAdapter, "claude-code"),
        (&CodexAdapter, "codex"),
        (&CopilotAdapter, "copilot"),
    ];
    for (adapter, kind) in adapters {
        for data in fixture_roots(kind) {
            let home = TempDir::new().unwrap();
            copy_tree(
                &data,
                &expand_data_root(&adapter.layout().data_root, home.path()),
            );
            assert_pool_invariant(&data.display().to_string(), adapter, home.path());
        }
    }
}

#[test]
fn generated_corpora_skim_identically_on_the_pool() {
    for (adapter, agent) in [
        (&ClaudeCodeAdapter as &dyn AgentAdapter, Agent::ClaudeCode),
        (&CodexAdapter, Agent::Codex),
    ] {
        let home = TempDir::new().unwrap();
        let root = expand_data_root(&adapter.layout().data_root, home.path());
        std::fs::create_dir_all(&root).unwrap();
        generate(&CorpusSpec::new(agent, 40, 12, 11), &root).unwrap();
        let serial = assert_pool_invariant(adapter.kind(), adapter, home.path());
        assert!(!serial.contains("files_skimmed: 0,"), "{serial}");
    }
}

fn record(session: &str, uuid: &str, extra: serde_json::Value) -> String {
    let mut value = serde_json::json!({
        "type": "user",
        "message": {"role": "user", "content": format!("{uuid} {}", "x".repeat(480))},
        "uuid": uuid,
        "timestamp": "2026-09-27T10:00:00.000Z",
        "cwd": format!("/w/{session}"),
        "sessionId": session,
        "version": "2.1.283",
    });
    if let serde_json::Value::Object(fields) = extra {
        value.as_object_mut().unwrap().extend(fields);
    }
    value.to_string()
}

fn write(root: &Path, relative: &str, contents: &str) {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

/// Mains titled in the head, the tail, only between the windows (the
/// bounded fallback scan) or not at all; subagents with and without their
/// sidecar, in nested and workflow directories; workflow runs and journals;
/// an empty transcript; inventory-only history.
fn claude_tree(root: &Path) {
    for index in 0..24 {
        let session = format!("s{index:02}");
        let project = format!("projects/-w-{}", index % 5);
        let mut lines: Vec<String> = (0..200)
            .map(|line| {
                record(
                    &session,
                    &format!("{session}-{line}"),
                    serde_json::json!({}),
                )
            })
            .collect();
        match index % 4 {
            0 => lines.insert(1, serde_json::json!({"type":"custom-title","customTitle":format!("head {session}"),"sessionId":session}).to_string()),
            1 => lines.push(serde_json::json!({"type":"ai-title","aiTitle":format!("tail {session}"),"sessionId":session}).to_string()),
            2 => lines.insert(120, serde_json::json!({"type":"ai-title","aiTitle":format!("middle {session}"),"sessionId":session}).to_string()),
            _ => {}
        }
        write(
            root,
            &format!("{project}/{session}.jsonl"),
            &(lines.join("\n") + "\n"),
        );
        for child in 0..(index % 3) {
            let agent = format!("agent-{session}c{child}");
            let dir = if child == 1 {
                "subagents/nested/deeper"
            } else {
                "subagents"
            };
            let line = record(
                &session,
                &format!("{agent}-0"),
                serde_json::json!({"agentId": format!("{session}c{child}"), "isSidechain": true}),
            );
            write(
                root,
                &format!("{project}/{session}/{dir}/{agent}.jsonl"),
                &(line + "\n"),
            );
            if child != 2 {
                write(
                    root,
                    &format!("{project}/{session}/{dir}/{agent}.meta.json"),
                    r#"{"agentType":"Explore"}"#,
                );
            }
        }
        if index % 6 == 0 {
            let run = format!("wf_{session}");
            write(
                root,
                &format!("{project}/{session}/workflows/{run}.json"),
                "{}",
            );
            write(
                root,
                &format!("{project}/{session}/subagents/workflows/{run}/journal.jsonl"),
                "{\"type\":\"started\",\"key\":\"k\"}\n",
            );
        }
        write(root, &format!("file-history/{session}/v1"), "old\n");
    }
    write(root, "projects/-w-9/empty.jsonl", "");
}

#[test]
fn every_claude_source_shape_skims_identically_on_the_pool() {
    let home = TempDir::new().unwrap();
    let root = expand_data_root(&ClaudeCodeAdapter.layout().data_root, home.path());
    claude_tree(&root);
    let serial = assert_pool_invariant("claude tree", &ClaudeCodeAdapter, home.path());
    assert!(
        serial.contains("middle s02"),
        "the fallback scan found a title"
    );
    assert!(serial.contains("files_inventory_only: 24"), "{serial}");
}

#[test]
fn the_first_failing_source_in_inventory_order_fails_the_pass() {
    let home = TempDir::new().unwrap();
    let root = expand_data_root(&ClaudeCodeAdapter.layout().data_root, home.path());
    claude_tree(&root);
    // Two sidecars above the adapter's document limit: the pass fails on the
    // first in inventory order, however the pool happens to read them.
    let oversized = format!(r#"{{"agentType":"{}"}}"#, "x".repeat(70_000));
    write(
        &root,
        "projects/-w-1/s01/subagents/agent-s01c0.meta.json",
        &oversized,
    );
    write(
        &root,
        "projects/-w-3/s03/subagents/agent-s03c0.meta.json",
        &oversized,
    );
    let serial = assert_pool_invariant("oversized", &ClaudeCodeAdapter, home.path());
    assert!(serial.starts_with("error: "), "{serial}");
    assert!(
        serial.contains("-w-1/s01/subagents/agent-s01c0.meta.json"),
        "{serial}"
    );
}

#[test]
fn results_arrive_in_item_order_whatever_the_pool() {
    let items: Vec<u64> = (0..500).collect();
    for threads in [1, 2, 7, 64] {
        let seen = in_order(
            &items,
            threads,
            |item| {
                // Uneven work so later items often finish first.
                std::thread::sleep(std::time::Duration::from_micros((item * 7919) % 300));
                item * 2
            },
            |next| (0..items.len()).map(|_| next()).collect::<Vec<_>>(),
        );
        assert_eq!(seen, items.iter().map(|item| item * 2).collect::<Vec<_>>());
    }
    // Stopping early is allowed and leaves no worker behind.
    let first = in_order(&items, 4, |item| *item, |next| next());
    assert_eq!(first, 0);
}
