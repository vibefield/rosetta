//! Cross-adapter evidence for shared ingest optimizations.
//!
//! Per-adapter tests prove parsing details. This test freezes the facts that
//! matter to generic pipeline/store work so a Codex-shaped optimization cannot
//! silently become a universal assumption.

use std::any::Any;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use rosetta_adapter_antigravity::AntigravityAdapter;
use rosetta_adapter_claude_code::ClaudeCodeAdapter;
use rosetta_adapter_codex::CodexAdapter;
use rosetta_adapter_copilot::CopilotAdapter;
use rosetta_adapter_grok::GrokAdapter;
use rosetta_adapter_hermes::HermesAdapter;
use rosetta_adapter_kimi_code::KimiCodeAdapter;
use rosetta_adapter_opencode::OpenCodeAdapter;
use rosetta_adapter_pi::PiAdapter;
use rosetta_adapter_support::{AgentAdapter, FileDiscipline, FileFormat, StreamDriver};
use serde_json::Value;

fn adapters() -> Vec<&'static dyn AgentAdapter> {
    vec![
        &AntigravityAdapter,
        &ClaudeCodeAdapter,
        &CodexAdapter,
        &CopilotAdapter,
        &GrokAdapter,
        &HermesAdapter,
        &KimiCodeAdapter,
        &OpenCodeAdapter,
        &PiAdapter,
    ]
}

#[derive(Debug, Default)]
struct OpEvidence {
    files: usize,
    messages: usize,
    tool_updates: usize,
    usage_merges: usize,
    inline_usage: usize,
    result_before_call: usize,
    usage_before_message: usize,
    tool_calls_seen: HashSet<(String, String)>,
    messages_seen: HashSet<(String, String)>,
}

impl OpEvidence {
    fn absorb(&mut self, value: &Value) {
        match value {
            Value::Array(values) => {
                for value in values {
                    self.absorb(value);
                }
            }
            Value::Object(object) => {
                if let Some(op) = object.get("op").and_then(Value::as_str) {
                    match op {
                        "message" => {
                            self.messages += 1;
                            if object
                                .get("usage")
                                .and_then(Value::as_object)
                                // `scope` always serializes; usage is present
                                // when any count or fact was reported.
                                .is_some_and(|usage| {
                                    usage
                                        .iter()
                                        .any(|(key, value)| key != "scope" && !value.is_null())
                                })
                            {
                                self.inline_usage += 1;
                            }
                            if let (Some(session), Some(timeline)) = (
                                object.get("session_id").and_then(Value::as_str),
                                object.get("timeline_id").and_then(Value::as_str),
                            ) {
                                self.messages_seen
                                    .insert((session.to_owned(), timeline.to_owned()));
                                if let Some(correlation_key) = object
                                    .get("body")
                                    .and_then(Value::as_object)
                                    .and_then(|body| body.get("tool"))
                                    .and_then(Value::as_object)
                                    .and_then(|tool| tool.get("correlation"))
                                    .and_then(Value::as_object)
                                    .and_then(|correlation| correlation.get("key"))
                                    .and_then(Value::as_str)
                                {
                                    self.tool_calls_seen
                                        .insert((session.to_owned(), correlation_key.to_owned()));
                                }
                            }
                        }
                        "tool_update" => {
                            self.tool_updates += 1;
                            if let (Some(session), Some(correlation_key)) = (
                                object.get("session_id").and_then(Value::as_str),
                                object.get("correlation_key").and_then(Value::as_str),
                            ) && !self
                                .tool_calls_seen
                                .contains(&(session.to_owned(), correlation_key.to_owned()))
                            {
                                self.result_before_call += 1;
                            }
                        }
                        "usage" => {
                            self.usage_merges += 1;
                            if let (Some(session), Some(response)) = (
                                object.get("session_id").and_then(Value::as_str),
                                object.get("response").and_then(Value::as_str),
                            ) && !self
                                .messages_seen
                                .contains(&(session.to_owned(), response.to_owned()))
                            {
                                self.usage_before_message += 1;
                            }
                        }
                        _ => {}
                    }
                    return;
                }
                for value in object.values() {
                    self.absorb(value);
                }
            }
            _ => {}
        }
    }
}

fn golden_root(kind: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../adapters")
        .join(kind)
        .join("tests/golden")
}

fn json_files(root: &Path) -> Vec<PathBuf> {
    fn visit(path: &Path, files: &mut Vec<PathBuf>) {
        let mut entries = std::fs::read_dir(path)
            .unwrap_or_else(|error| panic!("reading {}: {error}", path.display()))
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            let path = entry.path();
            if path.is_dir() {
                visit(&path, files);
            } else if path
                .extension()
                .is_some_and(|extension| extension == "json")
            {
                files.push(path);
            }
        }
    }

    let mut files = Vec::new();
    visit(root, &mut files);
    files
}

fn op_evidence(kind: &str) -> OpEvidence {
    let root = golden_root(kind);
    let files = json_files(&root);
    let mut evidence = OpEvidence {
        files: files.len(),
        ..OpEvidence::default()
    };
    for path in files {
        let text = std::fs::read_to_string(&path).unwrap();
        let value: Value = serde_json::from_str(&text)
            .unwrap_or_else(|error| panic!("parsing {}: {error}", path.display()));
        evidence.absorb(&value);
    }
    evidence
}

#[test]
fn every_driver_matches_its_declared_storage_contract() {
    let expected = [
        // The summary database and the per-conversation database.
        ("antigravity", 1, 0, 2),
        ("claude-code", 3, 2, 0),
        ("codex", 1, 0, 1),
        // The session-store database carries the per-request usage.
        ("copilot", 1, 1, 1),
        ("grok", 1, 2, 0),
        // The session database and the state database.
        ("hermes", 0, 0, 2),
        ("kimi-code", 1, 1, 0),
        ("opencode", 0, 0, 1),
        ("pi", 1, 0, 0),
    ];

    for (adapter, (kind, expected_jsonl, expected_document, expected_sqlite)) in
        adapters().into_iter().zip(expected)
    {
        assert_eq!(adapter.kind(), kind);
        assert_eq!(adapter.layout().agent, kind);
        assert!(adapter.normalizer_version() > 0, "{kind}");
        assert!(adapter.stream_driver("<not-a-stream>").is_none(), "{kind}");

        let mut jsonl = 0;
        let mut document = 0;
        let mut sqlite = 0;
        for stream in &adapter.layout().streams {
            match adapter.stream_driver(&stream.id) {
                Some(StreamDriver::JsonLines) => {
                    jsonl += 1;
                    assert_eq!(
                        stream.file_format,
                        FileFormat::Jsonl,
                        "{kind}/{}",
                        stream.id
                    );
                    assert_eq!(
                        stream.discipline,
                        FileDiscipline::AppendOnly,
                        "{kind}/{}",
                        stream.id
                    );
                    assert!(
                        adapter.snapshot_plan(&stream.id).is_none(),
                        "JSONL stream {kind}/{} exposed a snapshot plan",
                        stream.id
                    );
                }
                Some(StreamDriver::DocumentSnapshot) => {
                    document += 1;
                    assert!(
                        matches!(
                            stream.file_format,
                            FileFormat::Json
                                | FileFormat::Toml
                                | FileFormat::Markdown
                                | FileFormat::Text
                        ),
                        "{kind}/{}",
                        stream.id
                    );
                    assert_eq!(
                        stream.discipline,
                        FileDiscipline::RewriteInPlace,
                        "{kind}/{}",
                        stream.id
                    );
                    assert!(
                        adapter.snapshot_plan(&stream.id).is_none(),
                        "document stream {kind}/{} exposed a SQLite snapshot plan",
                        stream.id
                    );
                }
                Some(StreamDriver::SqliteSnapshot) => {
                    sqlite += 1;
                    assert_eq!(
                        stream.file_format,
                        FileFormat::Sqlite,
                        "{kind}/{}",
                        stream.id
                    );
                    assert_eq!(
                        stream.discipline,
                        FileDiscipline::Database,
                        "{kind}/{}",
                        stream.id
                    );
                    let plan = adapter.snapshot_plan(&stream.id).unwrap_or_else(|| {
                        panic!("SQLite stream {kind}/{} has no snapshot plan", stream.id)
                    });
                    assert!(!plan.sessions_sql.trim().is_empty(), "{kind}/{}", stream.id);
                    // A snapshot may be metadata-only (Antigravity's summary
                    // database or Codex's state database) and therefore have
                    // no timeline queries.
                    assert!(
                        plan.record_queries.iter().all(|query| {
                            !query.name.is_empty() && !query.sql.trim().is_empty()
                        }),
                        "{kind}/{} has an incomplete snapshot query",
                        stream.id
                    );
                }
                None => {
                    assert!(
                        adapter.snapshot_plan(&stream.id).is_none(),
                        "inventory-only stream {kind}/{} exposed a snapshot plan",
                        stream.id
                    );
                }
            }
        }
        assert_eq!(
            (jsonl, document, sqlite),
            (expected_jsonl, expected_document, expected_sqlite),
            "{kind}"
        );
    }
}

#[test]
fn every_adapter_state_has_a_stable_checkpoint_round_trip() {
    for adapter in adapters() {
        let kind = adapter.kind();
        let initial = adapter.init_state();
        let checkpoint = adapter
            .checkpoint_state(initial.as_deref())
            .unwrap_or_else(|error| panic!("{kind} cannot checkpoint fresh state: {error}"));
        assert_eq!(initial.is_some(), checkpoint.is_some(), "{kind}");

        let restored = adapter
            .restore_state(checkpoint.as_ref())
            .unwrap_or_else(|error| panic!("{kind} cannot restore fresh state: {error}"));
        let round_trip = adapter
            .checkpoint_state(restored.as_deref())
            .unwrap_or_else(|error| panic!("{kind} cannot checkpoint restored state: {error}"));
        assert_eq!(round_trip, checkpoint, "{kind}");

        let wrong_state = 7_i32;
        let wrong_state = &wrong_state as &(dyn Any + Send);
        assert!(
            adapter.checkpoint_state(Some(wrong_state)).is_err(),
            "{kind} accepted another adapter's state type"
        );
    }
}

#[test]
fn canonical_goldens_characterize_shared_store_work() {
    // Separate Usage merge ops are deliberately not universal. Inline usage
    // and absent fixture usage still travel through Message application.
    let expected = [
        ("antigravity", false, false),
        // Response usage rides a `Usage` op behind its row so copies across
        // files dedupe by identity (§4.1.3, counting rule 2).
        ("claude-code", true, false),
        ("codex", true, false),
        // Per-model shutdown intervals are session samples (§4.1.3).
        ("copilot", false, false),
        ("grok", true, false),
        ("hermes", false, false),
        ("kimi-code", false, true),
        // One Usage merge per step-finish part (§4.1.3).
        ("opencode", true, false),
        ("pi", false, true),
    ];

    for (kind, expects_usage_merge, expects_inline_usage) in expected {
        let evidence = op_evidence(kind);
        assert!(evidence.files > 0, "{kind} has no canonical-op goldens");
        assert!(evidence.messages > 0, "{kind} has no Message evidence");
        assert!(
            evidence.tool_updates > 0,
            "{kind} has no typed tool-update evidence"
        );
        assert_eq!(
            evidence.result_before_call, 0,
            "{kind} fixture results precede their calls: {evidence:?}"
        );
        assert_eq!(
            evidence.usage_before_message, 0,
            "{kind} fixture usage precedes its response: {evidence:?}"
        );
        assert_eq!(
            evidence.usage_merges > 0,
            expects_usage_merge,
            "{kind} Usage merge classification changed: {evidence:?}"
        );
        assert_eq!(
            evidence.inline_usage > 0,
            expects_inline_usage,
            "{kind} inline usage classification changed: {evidence:?}"
        );
    }
}
