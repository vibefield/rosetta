//! Pre-consent session estimates come from the existing layout walk, never
//! transcript content or database queries. Fixture roots are read-only;
//! adversarial inventories live in temporary directories.

use std::path::Path;

use rosetta_adapter_antigravity::AntigravityAdapter;
use rosetta_adapter_claude_code::ClaudeCodeAdapter;
use rosetta_adapter_codex::CodexAdapter;
use rosetta_adapter_copilot::CopilotAdapter;
use rosetta_adapter_grok::GrokAdapter;
use rosetta_adapter_hermes::HermesAdapter;
use rosetta_adapter_kimi_code::KimiCodeAdapter;
use rosetta_adapter_opencode::OpenCodeAdapter;
use rosetta_adapter_pi::PiAdapter;
use rosetta_adapter_support::AgentAdapter;
use rosetta_pipeline::{Discovery, IngestOptions, Instrument, Metric, discover_root};
use tempfile::TempDir;

fn census(adapter: &dyn AgentAdapter, root: &Path, max_files: usize) -> Discovery {
    let instrument = Instrument::new();
    let discovery = discover_root(
        adapter,
        &adapter.layout().data_root,
        root,
        &IngestOptions {
            max_files,
            ..IngestOptions::default()
        },
        &instrument,
    )
    .unwrap();
    assert_eq!(
        instrument.get(Metric::FilesWalked),
        discovery.file_count as u64
    );
    for metric in [Metric::FilesSkimmed, Metric::BytesRead, Metric::LinesParsed] {
        assert_eq!(instrument.get(metric), 0, "{}: {metric:?}", adapter.kind());
    }
    discovery
}

fn write(root: &Path, relative: &str) {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    // Deliberately not a valid transcript or database: discovery must count
    // paths even when a later skim would reject their content.
    std::fs::write(path, b"not a transcript\n").unwrap();
}

#[test]
fn session_estimates_exclude_metadata_duplicates_and_auxiliary_logs() {
    struct Case {
        adapter: &'static dyn AgentAdapter,
        files: &'static [&'static str],
        sessions: Option<usize>,
    }
    let cases = [
        Case {
            adapter: &AntigravityAdapter,
            files: &[
                "brain/a/.system_generated/logs/transcript.jsonl",
                "brain/b/.system_generated/logs/transcript.jsonl",
                "brain/a/.system_generated/logs/transcript_full.jsonl",
                "brain/a/.system_generated/logs/chunks/transcript/1.jsonl",
                "brain/a/.system_generated/logs/chunks/transcript_full/1.jsonl",
                "conversations/a.db",
                "conversations/a.db-wal",
                "conversation_summaries.db",
            ],
            sessions: Some(2),
        },
        Case {
            adapter: &ClaudeCodeAdapter,
            files: &[
                "projects/p/main.jsonl",
                "projects/p/main/subagents/agent-child.jsonl",
                "projects/p/main/subagents/nested/agent-grandchild.jsonl",
                "projects/p/main/subagents/agent-child.meta.json",
                "projects/p/main/subagents/workflows/run/journal.jsonl",
                "projects/p/main/workflows/wf_run.json",
                "projects/p/sessions-index.json",
                "history.jsonl",
            ],
            sessions: Some(3),
        },
        Case {
            adapter: &CodexAdapter,
            files: &[
                "sessions/2026/08/24/rollout-main.jsonl",
                "sessions/2026/08/24/rollout-child.jsonl",
                "state_5.sqlite",
                "state_5.sqlite-wal",
                "history.jsonl",
                "config.toml",
            ],
            sessions: Some(2),
        },
        Case {
            adapter: &CopilotAdapter,
            files: &[
                "session-state/a/events.jsonl",
                "session-state/b/events.jsonl",
                "session-state/a/workspace.yaml",
                "session-state/a/session.db",
                "session-store.db",
                "session-store.db-shm",
            ],
            sessions: Some(2),
        },
        Case {
            adapter: &GrokAdapter,
            files: &[
                "sessions/project/a/updates.jsonl",
                "sessions/project/b/updates.jsonl",
                "sessions/project/a/chat_history.jsonl",
                "sessions/project/a/events.jsonl",
                "sessions/project/a/summary.json",
                "sessions/project/prompt_history.jsonl",
            ],
            sessions: Some(2),
        },
        Case {
            adapter: &HermesAdapter,
            files: &["state.db", "state.db-wal", "profiles/work/state.db"],
            sessions: None,
        },
        Case {
            adapter: &KimiCodeAdapter,
            files: &[
                "sessions/project/root/agents/main/wire.jsonl",
                "sessions/project/root/agents/agent-0/wire.jsonl",
                "sessions/project/root/state.json",
                "session_index.jsonl",
            ],
            sessions: Some(2),
        },
        Case {
            adapter: &OpenCodeAdapter,
            files: &["opencode.db", "opencode.db-wal", "opencode-next.db"],
            sessions: None,
        },
        Case {
            adapter: &PiAdapter,
            files: &[
                "agent/sessions/project/2026-08-24_a.jsonl",
                "agent/sessions/project/2026-08-24_b.jsonl",
                "agent/settings.json",
            ],
            sessions: Some(2),
        },
    ];

    for case in cases {
        let root = TempDir::new().unwrap();
        for path in case.files {
            write(root.path(), path);
        }
        write(root.path(), "unclaimed/noise.bin");
        let found = census(case.adapter, root.path(), 100);
        assert_eq!(
            found.file_count,
            case.files.len(),
            "{} inventory",
            case.adapter.kind()
        );
        assert_eq!(
            found.session_count,
            case.sessions,
            "{} sessions",
            case.adapter.kind()
        );
        assert_eq!(
            found.total_bytes,
            (case.files.len() * b"not a transcript\n".len()) as u64
        );
        assert!(found.installed && !found.walk_truncated);
    }
}

#[test]
fn committed_fixtures_prove_estimates_across_every_adapter() {
    let cases: &[(&dyn AgentAdapter, &str, Option<usize>)] = &[
        (
            &AntigravityAdapter,
            "antigravity/fixtures/1.1.20/basic-chat/antigravity-cli",
            Some(1),
        ),
        (
            &ClaudeCodeAdapter,
            "claude-code/fixtures/2.1.241/basic-chat/claude-config",
            Some(1),
        ),
        (
            &CodexAdapter,
            "codex/fixtures/0.149.1/basic-chat/codex-home",
            Some(1),
        ),
        (
            &CopilotAdapter,
            "copilot/fixtures/1.0.80/basic-chat/copilot-home",
            Some(1),
        ),
        (
            &GrokAdapter,
            "grok/fixtures/1.0.5/basic-chat/grok-home",
            Some(1),
        ),
        (
            &HermesAdapter,
            "hermes/fixtures/0.20.5/basic-chat/hermes-home",
            None,
        ),
        (
            &KimiCodeAdapter,
            "kimi-code/fixtures/0.38.0/subagent/kimi-home",
            Some(2),
        ),
        (
            &OpenCodeAdapter,
            "opencode/fixtures/1.18.22/basic-chat/opencode-home",
            None,
        ),
        (&PiAdapter, "pi/fixtures/0.84.3/fork/pi-home", Some(2)),
    ];
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../groundtruth");
    for &(adapter, relative, expected) in cases {
        let found = census(adapter, &fixtures.join(relative), 10_000);
        assert!(found.installed && !found.walk_truncated, "{relative}");
        assert_eq!(found.session_count, expected, "{relative}");
    }
}

#[test]
fn zero_sessions_is_distinct_from_an_unknown_estimate() {
    let root = TempDir::new().unwrap();
    for path in [root.path().to_path_buf(), root.path().join("missing")] {
        let found = census(&ClaudeCodeAdapter, &path, 100);
        assert!(!found.installed);
        assert_eq!(found.session_count, Some(0));
        assert_eq!(census(&HermesAdapter, &path, 100).session_count, None);
    }
    // An agent with only settings is installed but has no transcripts yet.
    write(root.path(), "settings.json");
    let found = census(&ClaudeCodeAdapter, root.path(), 100);
    assert!(found.installed);
    assert_eq!(found.session_count, Some(0));
}

#[test]
fn a_truncated_census_never_advertises_its_partial_session_count() {
    let root = TempDir::new().unwrap();
    write(root.path(), "projects/p/a.jsonl");
    write(root.path(), "projects/p/b.jsonl");
    for max_files in [0, 1] {
        let found = census(&ClaudeCodeAdapter, root.path(), max_files);
        assert!(found.walk_truncated);
        assert_eq!(found.session_count, None);
    }
    let found = census(&ClaudeCodeAdapter, root.path(), 2);
    assert!(
        !found.walk_truncated,
        "exactly reaching the cap is complete"
    );
    assert_eq!(found.session_count, Some(2));
}

#[cfg(unix)]
#[test]
fn unreadable_transcripts_and_databases_are_counted_without_opening_them() {
    use std::os::unix::fs::PermissionsExt;

    let root = TempDir::new().unwrap();
    let paths = ["sessions/2026/08/24/rollout-main.jsonl", "state_5.sqlite"];
    for relative in paths {
        write(root.path(), relative);
        std::fs::set_permissions(
            root.path().join(relative),
            std::fs::Permissions::from_mode(0o000),
        )
        .unwrap();
    }
    let result = discover_root(
        &CodexAdapter,
        "~/.codex",
        root.path(),
        &IngestOptions::default(),
        &Instrument::new(),
    );
    for relative in paths {
        std::fs::set_permissions(
            root.path().join(relative),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
    }
    let found = result.unwrap();
    assert_eq!(found.session_count, Some(1));
    assert_eq!(found.file_count, 2);
    assert!(!root.path().join("state_5.sqlite-shm").exists());
}
