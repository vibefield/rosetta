//! Synthetic content: text bodies, workspace paths, tool arguments.
//!
//! Every string produced here is obviously fake and machine-independent. No
//! value is ever read from the environment, so `grep -rE 'jamesyong|/Users/'`
//! over a generated corpus is empty by construction — the only paths that
//! appear are the `/tmp/bench/project-N` roots minted below.

use crate::prng::Rng;

/// The word pool. Deliberately drawn from this repo's own vocabulary so a
/// human skimming a bench corpus can never mistake it for a real transcript.
const WORDS: &[&str] = &[
    "adapter",
    "alpha",
    "anchor",
    "batch",
    "bench",
    "beta",
    "block",
    "bucket",
    "capture",
    "checkpoint",
    "corpus",
    "cursor",
    "delta",
    "drift",
    "epoch",
    "fixture",
    "fold",
    "frame",
    "gamma",
    "ledger",
    "normalize",
    "ordinal",
    "parse",
    "pipeline",
    "projection",
    "record",
    "rosetta",
    "segment",
    "session",
    "stream",
    "synthetic",
    "timeline",
    "token",
    "watermark",
    "widget",
    "zeta",
];

/// A synthetic prose body: a labelled header followed by `words` pool words,
/// broken into sentences. Length is the knob that makes generated files the
/// right ORDER OF MAGNITUDE for throughput work — real transcript lines run
/// from a few hundred bytes to several KB.
pub fn text(rng: &mut Rng, label: &str, index: u64, min_words: u64, max_words: u64) -> String {
    let words = rng.range(min_words, max_words + 1);
    let mut out = String::with_capacity((words as usize + 4) * 8);
    out.push_str("synthetic ");
    out.push_str(label);
    out.push(' ');
    out.push_str(&index.to_string());
    for position in 0..words {
        out.push(' ');
        out.push_str(rng.pick(WORDS));
        if position % 13 == 12 {
            out.push('.');
        }
    }
    out.push('.');
    out
}

/// A multi-paragraph blob, for the places agents write large fixed preambles
/// (codex's `base_instructions`, world-state instruction bodies).
pub fn blob(rng: &mut Rng, label: &str, paragraphs: u64) -> String {
    let mut out = String::new();
    for index in 0..paragraphs {
        if index > 0 {
            out.push_str("\n\n");
        }
        out.push_str(&text(rng, label, index, 30, 90));
    }
    out
}

/// The synthetic workspace root for project `index`.
pub fn project_cwd(index: u64) -> String {
    format!("/tmp/bench/project-{index}")
}

/// claude-code's project directory name: the cwd with separators flattened.
pub fn project_slug(cwd: &str) -> String {
    cwd.replace('/', "-")
}

/// A plausible source path inside a synthetic workspace.
pub fn workspace_file(rng: &mut Rng, cwd: &str) -> String {
    let dir = rng.pick(&["src", "src/core", "crates/thing/src", "tests", "docs"]);
    let stem = rng.pick(&["module", "handler", "record", "walker", "index"]);
    let ext = rng.pick(&["rs", "ts", "toml", "md", "json"]);
    format!("{cwd}/{dir}/{stem}-{}.{ext}", rng.below(64))
}

/// A synthetic shell command — never anything that would do work if run.
pub fn shell_command(rng: &mut Rng, cwd: &str) -> String {
    match rng.below(4) {
        0 => format!("echo synthetic-{}", rng.below(10_000)),
        1 => format!("ls {cwd}/src"),
        2 => format!("grep -n synthetic {}", workspace_file(rng, cwd)),
        _ => format!("cat {}", workspace_file(rng, cwd)),
    }
}

#[cfg(test)]
mod tests {
    use super::{blob, project_cwd, project_slug, shell_command, text, workspace_file};
    use crate::prng::Rng;

    #[test]
    fn content_is_obviously_synthetic_and_machine_independent() {
        let mut rng = Rng::new(9);
        let cwd = project_cwd(3);
        assert_eq!(project_slug(&cwd), "-tmp-bench-project-3");
        let samples = [
            text(&mut rng, "prompt", 1, 5, 40),
            blob(&mut rng, "instructions", 3),
            workspace_file(&mut rng, &cwd),
            shell_command(&mut rng, &cwd),
        ];
        for sample in samples {
            assert!(!sample.contains("/Users/"), "{sample}");
            assert!(!sample.contains("/home/"), "{sample}");
            assert!(sample.is_ascii(), "{sample}");
        }
    }

    #[test]
    fn text_length_tracks_the_word_budget() {
        let mut rng = Rng::new(4);
        let short = text(&mut rng, "prompt", 0, 4, 4);
        let long = text(&mut rng, "prompt", 0, 400, 400);
        assert!(short.starts_with("synthetic prompt 0 "));
        assert!(long.len() > short.len() * 10);
    }
}
