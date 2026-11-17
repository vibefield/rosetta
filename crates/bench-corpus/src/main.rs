//! `bench-corpus` — generate a deterministic synthetic agent corpus.
//!
//! Argument parsing is hand-rolled on purpose: this binary is a bench fixture,
//! and its dependency list should stay at serde/serde_json (plus sha2, already
//! built for the daemon, for snapshot checksums) so it never becomes
//! a reason a bench run is slow to build.

use std::path::PathBuf;
use std::process::ExitCode;

use rosetta_bench_corpus::shape::{self, ShapeSpec};
use rosetta_bench_corpus::{Agent, CorpusSpec, generate, snapshot};

const USAGE: &str = "\
bench-corpus — deterministic synthetic corpora for rosetta's benches

USAGE:
    bench-corpus gen <agent> --sessions N --lines M --seed S --out DIR [options]
    bench-corpus snapshot --shape NAME --out DIR [--seed S] [--threads N] [--plan] [--json]

ARGS:
    <agent>              claude-code | codex

OPTIONS:
    --sessions N         number of sessions to synthesize
    --lines M            TOTAL lines across all sessions (accepts 500k, 1M)
    --seed S             PRNG seed; same seed + params = byte-identical corpus
    --out DIR            output root; the agent's layout is created underneath
    --tool-ratio F       fraction of turns that call a tool (default 0.30)
    --json               print the report as JSON instead of a table
    -h, --help           print this help

SNAPSHOT:
    --shape NAME         macbook-shape-v1 (the MacBook-shaped UI corpus) | tiny-shape
    --out DIR            the snapshot (must not exist): DIR/home, SHA256SUMS, snapshot.json,
                         files 0444 and directories 0555, as tools/r1-bench verifies
    --seed S             plan seed (default 1)
    --threads N          writers and hashers (default 4)
    --plan               print the plan's summary only; write nothing

EXAMPLES:
    bench-corpus gen claude-code --sessions 2000 --lines 1M --seed 42 --out /tmp/cc
    bench-corpus gen codex --sessions 500 --lines 250k --seed 42 --out /tmp/cx
    bench-corpus snapshot --shape macbook-shape-v1 --out ~/frozen/macbook-shape-v1
";

fn main() -> ExitCode {
    match run(std::env::args().skip(1).collect()) {
        Ok(output) => {
            print!("{output}");
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("bench-corpus: {message}");
            eprintln!("\n{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn run(args: Vec<String>) -> Result<String, String> {
    let Some(command) = args.first() else {
        return Ok(USAGE.to_string());
    };
    match command.as_str() {
        "-h" | "--help" | "help" => Ok(USAGE.to_string()),
        // `gen` is a reserved keyword in edition 2024 — the subcommand keeps
        // the name, the function cannot.
        "gen" => generate_command(&args[1..]),
        "snapshot" => snapshot_command(&args[1..]),
        other => Err(format!("unknown command `{other}`")),
    }
}

fn generate_command(args: &[String]) -> Result<String, String> {
    let agent_name = args
        .first()
        .filter(|value| !value.starts_with('-'))
        .ok_or("`gen` needs an agent (claude-code | codex)")?;
    let agent = Agent::parse(agent_name).ok_or_else(|| {
        format!(
            "unknown agent `{agent_name}` (known: {})",
            Agent::ALL
                .iter()
                .map(|a| a.name())
                .collect::<Vec<_>>()
                .join(", ")
        )
    })?;

    let mut sessions = None;
    let mut lines = None;
    let mut seed = None;
    let mut out: Option<PathBuf> = None;
    let mut tool_ratio = 0.3;
    let mut as_json = false;

    let mut rest = args[1..].iter();
    while let Some(flag) = rest.next() {
        let mut value = || {
            rest.next()
                .cloned()
                .ok_or_else(|| format!("`{flag}` needs a value"))
        };
        match flag.as_str() {
            "--sessions" => sessions = Some(parse_count(&value()?, flag)?),
            "--lines" => lines = Some(parse_count(&value()?, flag)?),
            "--seed" => seed = Some(parse_count(&value()?, flag)?),
            "--out" => out = Some(PathBuf::from(value()?)),
            "--tool-ratio" => {
                let text = value()?;
                tool_ratio = text
                    .parse::<f64>()
                    .map_err(|_| format!("`--tool-ratio` wants a number, got `{text}`"))?;
                if !(0.0..=1.0).contains(&tool_ratio) {
                    return Err("`--tool-ratio` must be between 0 and 1".to_string());
                }
            }
            "--json" => as_json = true,
            "-h" | "--help" => return Ok(USAGE.to_string()),
            other => return Err(format!("unknown option `{other}`")),
        }
    }

    let sessions = sessions.ok_or("`--sessions` is required")?;
    let lines = lines.ok_or("`--lines` is required")?;
    let seed = seed.ok_or("`--seed` is required")?;
    let out = out.ok_or("`--out` is required")?;
    if sessions == 0 || lines == 0 {
        return Err("`--sessions` and `--lines` must be positive".to_string());
    }

    let spec = CorpusSpec::new(agent, sessions, lines, seed).with_tool_ratio(tool_ratio);
    let report = generate(&spec, &out).map_err(|error| format!("{}: {error}", out.display()))?;

    if as_json {
        serde_json::to_string_pretty(&report)
            .map(|text| format!("{text}\n"))
            .map_err(|error| error.to_string())
    } else {
        Ok(report.table())
    }
}

fn snapshot_command(args: &[String]) -> Result<String, String> {
    let mut shape = None;
    let mut out: Option<PathBuf> = None;
    let mut seed = 1;
    let mut threads = 4;
    let mut plan_only = false;
    let mut as_json = false;
    let mut rest = args.iter();
    while let Some(flag) = rest.next() {
        let mut value = || {
            rest.next()
                .cloned()
                .ok_or_else(|| format!("`{flag}` needs a value"))
        };
        match flag.as_str() {
            "--shape" => {
                let name = value()?;
                shape = Some(ShapeSpec::by_name(&name).ok_or_else(|| {
                    format!("unknown shape `{name}` (known: macbook-shape-v1, tiny-shape)")
                })?);
            }
            "--out" => out = Some(PathBuf::from(value()?)),
            "--seed" => seed = parse_count(&value()?, flag)?,
            "--threads" => threads = parse_count(&value()?, flag)? as usize,
            "--plan" => plan_only = true,
            "--json" => as_json = true,
            "-h" | "--help" => return Ok(USAGE.to_string()),
            other => return Err(format!("unknown option `{other}`")),
        }
    }
    let spec = shape.ok_or("`--shape` is required")?;
    let plan = shape::plan(&spec, seed);
    let value = if plan_only {
        plan_summary(&plan)
    } else {
        let out = out.ok_or("`--out` is required")?;
        snapshot::materialize(&plan, &out, threads)
            .map_err(|error| format!("{}: {error}", out.display()))?
    };
    let text = serde_json::to_string_pretty(&value).map_err(|error| error.to_string())?;
    if as_json || plan_only {
        Ok(format!("{text}\n"))
    } else {
        Ok(format!(
            "snapshot written: {} files, {} bytes, walkInputsDigest {}, manifestSha256 {}\n",
            value["totals"]["files"],
            value["totals"]["bytes"],
            value["walkInputsDigest"],
            value["manifestSha256"]
        ))
    }
}

fn plan_summary(plan: &shape::ShapePlan) -> serde_json::Value {
    let giant = &plan.trees[0];
    serde_json::json!({
        "shape": plan.spec.name,
        "seed": plan.seed,
        "roots": plan.trees.len(),
        "bigTrees": plan.trees.iter().filter(|t| t.max_file() > plan.spec.big_line_bytes).count(),
        "claudeFiles": plan.claude_files(),
        "claudeBytes": plan.claude_bytes(),
        "newestGiant": {"treeBytes": giant.bytes(), "files": giant.files(), "maxFileBytes": giant.max_file()},
    })
}

/// Accept `5000`, `5_000`, `500k`, `1M` — the design doc writes corpus sizes
/// that way and typing seven zeros is how you get six.
fn parse_count(text: &str, flag: &str) -> Result<u64, String> {
    let trimmed = text.trim();
    let (digits, multiplier) = match trimmed.chars().last() {
        Some('k' | 'K') => (&trimmed[..trimmed.len() - 1], 1_000),
        Some('m' | 'M') => (&trimmed[..trimmed.len() - 1], 1_000_000),
        Some('g' | 'G') => (&trimmed[..trimmed.len() - 1], 1_000_000_000),
        _ => (trimmed, 1),
    };
    let base: u64 = digits
        .replace('_', "")
        .parse()
        .map_err(|_| format!("`{flag}` wants a count, got `{text}`"))?;
    base.checked_mul(multiplier)
        .ok_or_else(|| format!("`{flag}` value `{text}` overflows"))
}

#[cfg(test)]
mod tests {
    use super::{parse_count, run};

    fn args(line: &str) -> Vec<String> {
        line.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn counts_accept_scale_suffixes() {
        for (text, expected) in [
            ("5000", 5_000),
            ("5_000", 5_000),
            ("500k", 500_000),
            ("1M", 1_000_000),
            ("2G", 2_000_000_000),
        ] {
            assert_eq!(parse_count(text, "--lines").unwrap(), expected);
        }
        assert!(parse_count("many", "--lines").is_err());
        assert!(parse_count("99999999999999G", "--lines").is_err());
    }

    #[test]
    fn help_and_usage_errors() {
        assert!(run(vec![]).unwrap().contains("USAGE"));
        assert!(run(args("--help")).unwrap().contains("USAGE"));
        assert!(run(args("gen")).is_err());
        assert!(run(args("gen hermes --sessions 1")).is_err());
        assert!(run(args("gen codex --sessions 1 --lines 20 --seed 1")).is_err());
        assert!(
            run(args(
                "gen codex --sessions 1 --lines 20 --seed 1 --out /tmp/x --nope"
            ))
            .is_err()
        );
    }

    #[test]
    fn snapshot_usage_and_plan() {
        assert!(run(args("snapshot")).is_err());
        assert!(run(args("snapshot --shape nope --plan")).is_err());
        assert!(
            run(args("snapshot --shape tiny-shape")).is_err(),
            "--out is required"
        );
        let plan: serde_json::Value =
            serde_json::from_str(&run(args("snapshot --shape tiny-shape --plan")).unwrap())
                .unwrap();
        assert_eq!(plan["roots"], 12);
        assert_eq!(plan["newestGiant"]["treeBytes"], 600_000);
    }

    #[test]
    fn generates_into_a_temporary_directory() {
        let out = std::env::temp_dir().join("rosetta-bench-corpus-cli-smoke");
        let _ = std::fs::remove_dir_all(&out);
        let command = format!(
            "gen claude-code --sessions 3 --lines 60 --seed 5 --tool-ratio 0.5 --out {}",
            out.display()
        );
        let table = run(args(&command)).unwrap();
        assert!(table.contains("transcript"));
        assert!(table.contains("TOTAL"));

        let json = run(args(&format!("{command} --json"))).unwrap();
        let report: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(report["agent"], "claude-code");
        assert_eq!(report["streams"][0]["lines"], 60);
        let _ = std::fs::remove_dir_all(&out);
    }
}
