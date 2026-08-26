//! The self-check lane.
//!
//! A synthetic corpus is only worth benchmarking if the real parsers accept it,
//! so this test replays every generated line through the agent's own
//! `parse_line` and demands the same standard the L1 fixture-replay suites hold
//! committed fixtures to: no hard parse failures, no unknown record types, no
//! unknown fields. It also pins the two properties benches depend on —
//! determinism (same seed, byte-identical corpus) and the layout globs the
//! agent's `groundtruth.json` declares.

use std::fs;
use std::path::{Path, PathBuf};

use rosetta_adapter_support::{Drift, GroundTruthDoc, glob_match};
use rosetta_bench_corpus::{Agent, CorpusSpec, generate};

const SEED: u64 = 20_260_826;

/// The committed size is small enough to run on every `cargo test`. Raise it
/// with `ROSETTA_BENCH_CORPUS_TEST_SESSIONS` / `..._LINES` to replay a
/// bench-sized corpus through the parsers before trusting a benchmark run —
/// the generator's rarer branches (multi-day sessions, filler records) only
/// show up at scale.
fn sessions() -> u64 {
    from_env("ROSETTA_BENCH_CORPUS_TEST_SESSIONS", 5)
}

fn lines() -> u64 {
    from_env("ROSETTA_BENCH_CORPUS_TEST_LINES", 500)
}

fn from_env(name: &str, fallback: u64) -> u64 {
    match std::env::var(name) {
        Ok(value) => value
            .parse()
            .unwrap_or_else(|_| panic!("{name}: `{value}` is not a count")),
        Err(_) => fallback,
    }
}

/// A scratch directory that is emptied on the way in and out. Tests inside one
/// binary share a process, so each caller passes a distinct label.
fn scratch(label: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join("rosetta-bench-corpus-tests")
        .join(label);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// Every file under `root`, relative to it, in a stable order.
fn files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let mut entries: Vec<PathBuf> = fs::read_dir(&dir)
            .expect("readable dir")
            .map(|entry| entry.expect("dir entry").path())
            .collect();
        entries.sort();
        for entry in entries {
            if entry.is_dir() {
                stack.push(entry);
            } else {
                out.push(entry.strip_prefix(root).expect("under root").to_path_buf());
            }
        }
    }
    out.sort();
    out
}

fn ground_truth(agent: Agent) -> GroundTruthDoc {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../groundtruth")
        .join(agent.name())
        .join("groundtruth.json");
    GroundTruthDoc::parse(&fs::read_to_string(&path).expect("canonical ground truth"))
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn spec(agent: Agent) -> CorpusSpec {
    CorpusSpec::new(agent, sessions(), lines(), SEED)
}

/// One parse verdict per line, collapsed into counters the assertions read.
#[derive(Default)]
struct Verdict {
    lines: u64,
    failures: Vec<String>,
    unknown_types: Vec<String>,
    drift: Vec<String>,
}

impl Verdict {
    fn assert_clean(&self, agent: Agent, expected_lines: u64) {
        assert_eq!(
            self.lines,
            expected_lines,
            "{}: generated line count",
            agent.name()
        );
        assert!(
            self.failures.is_empty(),
            "{}: hard parse failures in the generated corpus:\n{}",
            agent.name(),
            self.failures.join("\n")
        );
        assert!(
            self.unknown_types.is_empty(),
            "{}: generated record types the adapter does not know:\n{}",
            agent.name(),
            self.unknown_types.join("\n")
        );
        assert!(
            self.drift.is_empty(),
            "{}: generated fields the raw types do not name:\n{}",
            agent.name(),
            self.drift.join("\n")
        );
    }
}

/// Walk a generated corpus, handing each line to `parse` and recording what it
/// said. The closure is the only agent-specific part.
fn replay(root: &Path, mut parse: impl FnMut(&str) -> LineVerdict) -> Verdict {
    let mut verdict = Verdict::default();
    for relative in files(root) {
        let text = fs::read_to_string(root.join(&relative)).expect("generated file");
        for (index, line) in text.lines().enumerate() {
            assert!(!line.trim().is_empty(), "generated a blank line");
            verdict.lines += 1;
            let where_ = format!("{}:{}", relative.display(), index + 1);
            match parse(line) {
                LineVerdict::Ok(paths) => {
                    if !paths.is_empty() {
                        verdict
                            .drift
                            .push(format!("{where_}: {}", paths.join(", ")));
                    }
                }
                LineVerdict::UnknownType(kind) => {
                    verdict.unknown_types.push(format!("{where_}: type={kind}"));
                }
                LineVerdict::Failed(error) => {
                    verdict.failures.push(format!("{where_}: {error}"));
                }
            }
        }
    }
    verdict
}

enum LineVerdict {
    Ok(Vec<String>),
    UnknownType(String),
    Failed(String),
}

#[test]
fn claude_code_corpus_parses_clean() {
    let root = scratch("claude-code-parse");
    let report = generate(&spec(Agent::ClaudeCode), &root).expect("generate");
    assert_eq!(report.lines(), lines());
    assert_eq!(report.files(), sessions());

    let verdict = replay(&root, |line| {
        use rosetta_adapter_claude_code::raw::{ParsedLine, parse_line};
        match parse_line(line) {
            Ok(ParsedLine::Known(record)) => LineVerdict::Ok(record.drift_paths()),
            Ok(ParsedLine::UnknownType { rtype, .. }) => {
                LineVerdict::UnknownType(format!("{rtype:?}"))
            }
            Err(error) => LineVerdict::Failed(error.to_string()),
        }
    });
    verdict.assert_clean(Agent::ClaudeCode, lines());
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn codex_corpus_parses_clean() {
    let root = scratch("codex-parse");
    let report = generate(&spec(Agent::Codex), &root).expect("generate");
    assert_eq!(report.lines(), lines());
    assert_eq!(report.files(), sessions());

    let verdict = replay(&root, |line| {
        use rosetta_adapter_codex::raw::{ParsedLine, parse_line};
        match parse_line(line) {
            Ok(ParsedLine::Known(record)) => LineVerdict::Ok(record.drift_paths()),
            Ok(ParsedLine::UnknownType { rtype, .. }) => {
                LineVerdict::UnknownType(format!("{rtype:?}"))
            }
            Err(error) => LineVerdict::Failed(error.to_string()),
        }
    });
    verdict.assert_clean(Agent::Codex, lines());
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn the_same_seed_produces_byte_identical_corpora() {
    for agent in Agent::ALL {
        let first = scratch(&format!("{}-determinism-a", agent.name()));
        let second = scratch(&format!("{}-determinism-b", agent.name()));
        generate(&spec(agent), &first).expect("generate");
        generate(&spec(agent), &second).expect("generate");

        let names = files(&first);
        assert_eq!(names, files(&second), "{}: file sets differ", agent.name());
        assert!(!names.is_empty());
        for name in &names {
            assert_eq!(
                fs::read(first.join(name)).unwrap(),
                fs::read(second.join(name)).unwrap(),
                "{}: {} differs between runs",
                agent.name(),
                name.display()
            );
        }

        // And a different seed must actually move: a "deterministic" generator
        // that ignores its seed would pass the check above.
        let other = scratch(&format!("{}-determinism-c", agent.name()));
        let mut changed = spec(agent);
        changed.seed += 1;
        generate(&changed, &other).expect("generate");
        assert_ne!(names, files(&other), "{}: seed had no effect", agent.name());

        for dir in [first, second, other] {
            let _ = fs::remove_dir_all(dir);
        }
    }
}

#[test]
fn corpora_land_on_the_declared_layout_globs() {
    for agent in Agent::ALL {
        let root = scratch(&format!("{}-layout", agent.name()));
        generate(&spec(agent), &root).expect("generate");

        let ground_truth = ground_truth(agent);
        let stream = ground_truth
            .stream(agent.stream())
            .unwrap_or_else(|| panic!("{} declares no `{}` stream", agent.name(), agent.stream()));
        for file in files(&root) {
            let relative = file.to_string_lossy().replace('\\', "/");
            assert!(
                glob_match(&stream.glob, &relative),
                "{}: `{relative}` is outside the declared glob `{}`",
                agent.name(),
                stream.glob
            );
        }
        let _ = fs::remove_dir_all(&root);
    }
}

#[test]
fn nothing_from_this_machine_leaks_into_a_corpus() {
    for agent in Agent::ALL {
        let root = scratch(&format!("{}-hermetic", agent.name()));
        generate(&spec(agent), &root).expect("generate");
        for file in files(&root) {
            let path = file.to_string_lossy().to_string();
            let text = fs::read_to_string(root.join(&file)).expect("generated file");
            for needle in [
                "/Users/",
                "/home/",
                "/private/tmp/rosetta-farm",
                "jamesyong",
            ] {
                assert!(
                    !text.contains(needle) && !path.contains(needle),
                    "{}: `{needle}` leaked into {path}",
                    agent.name()
                );
            }
        }
        let _ = fs::remove_dir_all(&root);
    }
}
