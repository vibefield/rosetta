//! Golden ops: every committed fixture transcript replayed through the adapter,
//! with the resulting op stream frozen on disk.
//!
//! `fixtures_replay.rs` proves the raw types still PARSE what the agent writes;
//! this proves the normalize stage still MEANS the same thing. A diff here is
//! either a deliberate change to the flattening or a regression — never noise,
//! because ids are deterministic and the replay does no IO of its own.
//!
//! Regenerate after an intended change: `UPDATE_GOLDEN=1 cargo test -p
//! rosetta-adapter-claude-code --test golden_ops`, then read the diff.

use std::path::{Path, PathBuf};

use rosetta_adapter_claude_code::ClaudeCodeAdapter;
use rosetta_adapter_support::{AgentAdapter, LineStatus, NormalizeCtx};
use rosetta_core::{IngestOp, ids};
use serde_json::{Value, json};

/// The harvest marker the fixture farm writes under: everything below it is the
/// agent's own data root.
const MARKER: &str = "claude-config/";

fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../groundtruth/claude-code/fixtures")
}

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

struct Transcript {
    /// `<agent version>-<scenario>` — one golden file per fixture scenario.
    scenario: String,
    /// Path within the scenario's harvested data root.
    relative: String,
    path: PathBuf,
}

/// Fixture files the layout manifest routes to the transcript stream — the
/// adapter's own ground truth decides what it replays, not a hard-coded glob.
fn transcripts() -> Vec<Transcript> {
    let layout = ClaudeCodeAdapter.layout();
    let root = fixtures_root();
    let mut found = Vec::new();
    for entry in walkdir::WalkDir::new(&root).sort_by_file_name() {
        let entry = entry.unwrap();
        if !entry.file_type().is_file() {
            continue;
        }
        let full = entry.path().to_string_lossy().into_owned();
        let Some((scenario_dir, relative)) = full.split_once(MARKER) else {
            continue;
        };
        if layout.matching_stream(relative).map(|s| s.id.as_str()) != Some("transcript") {
            continue;
        }
        let scenario = Path::new(scenario_dir)
            .strip_prefix(&root)
            .unwrap()
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("-");
        found.push(Transcript {
            scenario,
            relative: relative.to_owned(),
            path: entry.path().to_owned(),
        });
    }
    found
}

fn context(transcript: &Transcript) -> (String, NormalizeCtx) {
    let adapter = ClaudeCodeAdapter;
    let runtime_id = ids::runtime_id(adapter.kind(), &adapter.layout().data_root);
    let native = transcript
        .relative
        .rsplit('/')
        .next()
        .and_then(|name| name.strip_suffix(".jsonl"))
        .expect("transcript file name is <session-uuid>.jsonl")
        .to_owned();
    let mut ctx = NormalizeCtx::new(&runtime_id, ids::session_id(&runtime_id, &native));
    ctx.state = adapter.init_state();
    (native, ctx)
}

fn ingest_range(
    transcript: &Transcript,
    ctx: &mut NormalizeCtx,
    text: &str,
    range: std::ops::Range<usize>,
) -> (Vec<IngestOp>, Vec<String>) {
    let adapter = ClaudeCodeAdapter;
    let lines: Vec<_> = text.lines().collect();
    let mut offsets = Vec::with_capacity(lines.len());
    let mut offset = 0i64;
    for line in &lines {
        offsets.push(offset);
        offset += line.len() as i64 + 1;
    }

    let mut ops = Vec::new();
    let mut hard_errors = Vec::new();
    for index in range {
        let line = lines[index];
        // Stand-ins for the ledger rowid and byte offset the pipeline stamps;
        // deterministic, so they can be frozen.
        ctx.src_cursor = Some(index as i64 + 1);
        ctx.src_offset = Some(offsets[index]);

        let outcome = adapter.ingest_line(ctx, "transcript", line.as_bytes());
        if let LineStatus::HardError { code } = &outcome.status {
            hard_errors.push(format!("{}:{}: {code}", transcript.relative, index + 1));
        }
        ops.extend(outcome.ops);
    }
    (ops, hard_errors)
}

/// Replay one transcript exactly as the pipeline would: a fresh context per
/// file, adapter-owned state, and provenance stamped on every record.
fn replay(transcript: &Transcript) -> (String, Vec<IngestOp>, Vec<String>) {
    let (native, mut ctx) = context(transcript);
    let text = std::fs::read_to_string(&transcript.path).unwrap();
    let (ops, hard_errors) = ingest_range(transcript, &mut ctx, &text, 0..text.lines().count());
    (native, ops, hard_errors)
}

fn golden_text(transcripts: &[&Transcript]) -> String {
    let files: Vec<Value> = transcripts
        .iter()
        .map(|transcript| {
            let (native, ops, _) = replay(transcript);
            json!({
                "transcript": transcript.relative,
                "native_session_id": native,
                "ops": ops,
            })
        })
        .collect();
    let mut text = serde_json::to_string_pretty(&files).unwrap();
    text.push('\n');
    text
}

#[test]
fn corpus_exists() {
    assert!(
        !transcripts().is_empty(),
        "no transcript fixtures under {} — run: python3 -m tools.groundtruth farm claude-code",
        fixtures_root().display()
    );
}

#[test]
fn every_scenario_matches_its_golden() {
    let all = transcripts();
    let mut scenarios: Vec<&str> = all.iter().map(|t| t.scenario.as_str()).collect();
    scenarios.dedup();

    let update = std::env::var_os("UPDATE_GOLDEN").is_some();
    if update {
        std::fs::create_dir_all(golden_dir()).unwrap();
    }
    for scenario in scenarios {
        let files: Vec<&Transcript> = all.iter().filter(|t| t.scenario == scenario).collect();
        let expected = golden_text(&files);
        let path = golden_dir().join(format!("{scenario}.json"));
        if update {
            std::fs::write(&path, &expected).unwrap();
            continue;
        }
        let actual = std::fs::read_to_string(&path).unwrap_or_else(|_| {
            panic!(
                "missing golden {} — regenerate with UPDATE_GOLDEN=1",
                path.display()
            )
        });
        assert!(
            actual == expected,
            "normalize output changed for scenario `{scenario}`.\n\
             Review the diff, then accept it with UPDATE_GOLDEN=1 if intended:\n{}",
            path.display()
        );
    }
}

#[test]
fn no_fixture_line_is_a_hard_error() {
    // A hard error means a record type the raw types claim to know no longer
    // parses — the drift alarm. The committed corpus must never trip it.
    let mut failures = Vec::new();
    for transcript in transcripts() {
        let (_, _, hard_errors) = replay(&transcript);
        failures.extend(hard_errors);
    }
    assert!(
        failures.is_empty(),
        "hard parse failures in the committed corpus:\n{}",
        failures.join("\n")
    );
}

#[test]
fn split_resume_matches_one_shot() {
    let adapter = ClaudeCodeAdapter;
    for transcript in transcripts() {
        let (_, one_shot, hard_errors) = replay(&transcript);
        assert!(hard_errors.is_empty());
        let text = std::fs::read_to_string(&transcript.path).unwrap();
        let count = text.lines().count();

        for split in 0..=count {
            let (_, mut before) = context(&transcript);
            let (mut split_ops, hard_errors) =
                ingest_range(&transcript, &mut before, &text, 0..split);
            assert!(hard_errors.is_empty());

            let checkpoint = adapter
                .checkpoint_state(before.state.as_deref())
                .unwrap()
                .expect("claude-code adapter state");
            let (_, mut after) = context(&transcript);
            after.next_ordinal = before.next_ordinal;
            after.state = adapter.restore_state(Some(&checkpoint)).unwrap();
            let (tail_ops, hard_errors) =
                ingest_range(&transcript, &mut after, &text, split..count);
            assert!(hard_errors.is_empty());
            split_ops.extend(tail_ops);

            assert_eq!(
                split_ops, one_shot,
                "{} differed at split {split}",
                transcript.relative
            );
        }
    }
}

#[test]
fn goldens_carry_no_personal_data() {
    // The fixture farm runs under a scratch root; a golden mentioning a home
    // directory would mean local data leaked into a committed artifact
    // (CLAUDE.md rule #3).
    let Ok(entries) = std::fs::read_dir(golden_dir()) else {
        return;
    };
    for entry in entries.flatten() {
        let text = std::fs::read_to_string(entry.path()).unwrap();
        for (no, line) in text.lines().enumerate() {
            assert!(
                !line.contains("/Users/") && !line.contains("/home/"),
                "{}:{}: home directory path in a committed golden",
                entry.path().display(),
                no + 1
            );
        }
    }
}
