//! Deterministic synthetic corpora for rosetta's benches and profiles.
//!
//! L1 fixtures are correctness-sized — dozens of lines. Performance work needs
//! millions, and CLAUDE.md iron rule 3 forbids depending on anyone's organic
//! data. So the load is SYNTHESIZED from the ground-truth shapes: line
//! templates derived from `groundtruth/<agent>/groundtruth.json` and the raw
//! types in `crates/adapters/<agent>`, laid out on disk under the agent's real
//! layout globs.
//!
//! The contract is `seed + params -> byte-identical corpus`, on every machine,
//! forever. That means: no `rand`, no system clock, no environment lookups, no
//! filesystem-order dependence. [`prng`] and [`clock`] are the only sources of
//! variation, and both are pure functions of the seed.
//!
//! ```no_run
//! use rosetta_bench_corpus::{Agent, CorpusSpec, generate};
//! use std::path::Path;
//!
//! let spec = CorpusSpec::new(Agent::ClaudeCode, 20, 5_000, 42);
//! let report = generate(&spec, Path::new("/tmp/rosetta-bench/cc")).unwrap();
//! println!("{}", report.table());
//! ```

pub mod claude_code;
pub mod clock;
pub mod codex;
pub mod prng;
pub mod report;
pub mod synth;

use std::io;
use std::path::Path;

pub use report::{CorpusReport, StreamReport};

use prng::Rng;

/// Which agent's on-disk shape to synthesize.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Agent {
    ClaudeCode,
    Codex,
}

impl Agent {
    pub const ALL: [Agent; 2] = [Agent::ClaudeCode, Agent::Codex];

    pub fn parse(name: &str) -> Option<Agent> {
        match name {
            "claude-code" => Some(Agent::ClaudeCode),
            "codex" => Some(Agent::Codex),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Agent::ClaudeCode => "claude-code",
            Agent::Codex => "codex",
        }
    }

    /// The stream id (from the agent's `layout.toml`) this generator writes.
    pub fn stream(self) -> &'static str {
        match self {
            Agent::ClaudeCode => "transcript",
            Agent::Codex => "rollout",
        }
    }

    /// The shortest session that still forms a coherent record sequence:
    /// claude-code needs a latch, a turn and a leaf marker; codex needs its
    /// six-record prelude, one turn and a completion event.
    pub fn min_session_lines(self) -> u64 {
        match self {
            Agent::ClaudeCode => 4,
            Agent::Codex => 12,
        }
    }
}

/// What to generate. `lines` is the TOTAL across all sessions — the split is
/// heavy-tailed, so raising it lengthens a few sessions far more than the rest.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CorpusSpec {
    pub agent: Agent,
    pub sessions: u64,
    pub lines: u64,
    pub seed: u64,
    /// Fraction of turns that call a tool. 0.3 matches the shape of organic
    /// coding-agent traffic closely enough for parser throughput work.
    pub tool_ratio: f64,
}

impl CorpusSpec {
    pub fn new(agent: Agent, sessions: u64, lines: u64, seed: u64) -> Self {
        Self {
            agent,
            sessions,
            lines,
            seed,
            tool_ratio: 0.3,
        }
    }

    pub fn with_tool_ratio(mut self, ratio: f64) -> Self {
        self.tool_ratio = ratio.clamp(0.0, 1.0);
        self
    }
}

/// Pareto tail index. 1.16 is the classic 80/20 exponent: a handful of sessions
/// carry most of the lines, which is what an organic corpus looks like and what
/// makes per-file scheduling in the pipeline worth measuring.
const PARETO_ALPHA: f64 = 1.16;

/// The smallest quantile the tail is allowed to reach. Without a floor, a
/// single session can swallow ~90% of a small corpus and the run stops
/// exercising multi-file work.
const PARETO_FLOOR: f64 = 0.02;

/// How many lines each session gets.
///
/// The vector sums to exactly `spec.lines` whenever that is reachable —
/// every session is floored at [`Agent::min_session_lines`], so a spec asking
/// for fewer lines than sessions yields fewer, longer sessions rather than
/// incoherent ones.
pub fn session_line_counts(spec: &CorpusSpec) -> Vec<u64> {
    let floor = spec.agent.min_session_lines();
    let affordable = (spec.lines / floor).max(1);
    let sessions = spec.sessions.clamp(1, affordable) as usize;

    let mut rng = Rng::stream(spec.seed, 0x5DEE_CE66);
    let weights: Vec<f64> = (0..sessions)
        .map(|_| {
            let quantile = PARETO_FLOOR + rng.unit() * (1.0 - PARETO_FLOOR);
            quantile.powf(-1.0 / PARETO_ALPHA)
        })
        .collect();

    let spare = spec.lines.saturating_sub(floor * sessions as u64);
    let total_weight: f64 = weights.iter().sum();
    let mut counts: Vec<u64> = weights
        .iter()
        .map(|weight| floor + (weight / total_weight * spare as f64) as u64)
        .collect();

    // Hand the floor-rounding remainder back, largest session first, so the
    // totals are exact and the distribution stays deterministic.
    let mut order: Vec<usize> = (0..sessions).collect();
    order.sort_by(|&a, &b| counts[b].cmp(&counts[a]).then(a.cmp(&b)));
    let assigned: u64 = counts.iter().sum();
    let mut shortfall = spec.lines.saturating_sub(assigned);
    let mut cursor = 0;
    while shortfall > 0 {
        counts[order[cursor % sessions]] += 1;
        cursor += 1;
        shortfall -= 1;
    }
    counts
}

/// Generate a corpus under `out_dir`, laid out per the agent's layout globs.
///
/// Directories are created as needed; files with colliding names are
/// overwritten. Callers that want a clean corpus should hand over a fresh
/// directory.
pub fn generate(spec: &CorpusSpec, out_dir: &Path) -> io::Result<CorpusReport> {
    let counts = session_line_counts(spec);
    let stream = match spec.agent {
        Agent::ClaudeCode => claude_code::generate(spec, &counts, out_dir)?,
        Agent::Codex => codex::generate(spec, &counts, out_dir)?,
    };
    Ok(CorpusReport {
        agent: spec.agent.name().to_string(),
        root: out_dir.display().to_string(),
        seed: spec.seed,
        sessions: counts.len() as u64,
        tool_ratio: spec.tool_ratio,
        streams: vec![stream],
    })
}

/// How many distinct project workspaces `sessions` sessions are spread across.
/// Sessions cluster into projects the way real ones do — several transcripts
/// per workspace, which is what makes project-scoped queries meaningful.
pub(crate) fn project_count(sessions: usize) -> u64 {
    ((sessions as f64).sqrt().ceil() as u64).max(1)
}

#[cfg(test)]
mod tests {
    use super::{Agent, CorpusSpec, project_count, session_line_counts};

    fn counts(agent: Agent, sessions: u64, lines: u64, seed: u64) -> Vec<u64> {
        session_line_counts(&CorpusSpec::new(agent, sessions, lines, seed))
    }

    #[test]
    fn totals_are_exact_and_floors_respected() {
        for (sessions, lines) in [(20u64, 5_000u64), (1, 10), (7, 97), (2_000, 100_000)] {
            for agent in Agent::ALL {
                let split = counts(agent, sessions, lines, 42);
                // The floor wins when the request is too small to be coherent
                // — a corpus of half-sessions would prove nothing.
                let floor_total = agent.min_session_lines() * split.len() as u64;
                assert_eq!(
                    split.iter().sum::<u64>(),
                    lines.max(floor_total),
                    "{agent:?} {sessions}x{lines}"
                );
                assert!(split.iter().all(|&c| c >= agent.min_session_lines()));
                assert!(split.len() as u64 <= sessions);
            }
        }
    }

    #[test]
    fn an_unaffordable_session_count_shrinks_rather_than_starving_sessions() {
        // 10 lines cannot make 40 coherent codex sessions; it makes one.
        let split = counts(Agent::Codex, 40, 10, 1);
        assert_eq!(split, vec![12]);
    }

    #[test]
    fn the_split_is_heavy_tailed() {
        let mut split = counts(Agent::ClaudeCode, 200, 100_000, 42);
        split.sort_unstable();
        let total: u64 = split.iter().sum();
        // Uniform would put 10% of the lines in the top decile; the tail keeps
        // it far above that, which is what gives a corpus a few big files.
        let top_decile: u64 = split.iter().rev().take(20).sum();
        assert!(
            top_decile * 3 > total,
            "top decile carried {top_decile} of {total}"
        );
        assert!(split[0] * 4 < split[split.len() - 1]);
    }

    #[test]
    fn the_split_is_seed_stable() {
        assert_eq!(
            counts(Agent::Codex, 30, 9_000, 7),
            counts(Agent::Codex, 30, 9_000, 7)
        );
        assert_ne!(
            counts(Agent::Codex, 30, 9_000, 7),
            counts(Agent::Codex, 30, 9_000, 8)
        );
    }

    #[test]
    fn projects_cluster_sessions() {
        assert_eq!(project_count(1), 1);
        assert_eq!(project_count(20), 5);
        assert_eq!(project_count(2_000), 45);
    }

    #[test]
    fn agent_names_round_trip() {
        for agent in Agent::ALL {
            assert_eq!(Agent::parse(agent.name()), Some(agent));
        }
        assert_eq!(Agent::parse("hermes"), None);
    }
}
