//! `search-bench` — frozen-input FTS5 latency campaigns.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::ExitCode;

use rosetta_search_bench::{
    CampaignManifest, RunReport, compare_reports, comparison_line, freeze_campaign, read_json,
    run_campaign, summary_line, write_json_new,
};

const USAGE: &str = "\
search-bench — checksummed, read-only FTS latency campaigns

USAGE:
    search-bench freeze --ledger DIR --queries FILE --out MANIFEST.json
    search-bench run --campaign MANIFEST.json --ledger DIR --queries FILE --db DB \\
        --implementation REV --machine LABEL --out RUN.json [--warmup N] [--cycles N]
    search-bench compare --before RUN.json --after RUN.json [--out COMPARISON.json]

RULES:
    freeze refuses a writable ledger tree.
    run refuses a writable database and fewer than 100 measured cycles.
    reports contain labels, counts, timings and digests — never query text or snippets.
    compare refuses different campaigns, machines, configs, or ranked results.
";

fn main() -> ExitCode {
    match execute(std::env::args().skip(1).collect()) {
        Ok(output) => {
            print!("{output}");
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("search-bench: {message}\n\n{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn parse_options(args: &[String]) -> Result<BTreeMap<String, String>, String> {
    let mut options = BTreeMap::new();
    let mut at = 0;
    while at < args.len() {
        let flag = &args[at];
        if !flag.starts_with("--") {
            return Err(format!("unexpected positional argument `{flag}`"));
        }
        let value = args
            .get(at + 1)
            .ok_or_else(|| format!("`{flag}` needs a value"))?;
        if value.starts_with("--") {
            return Err(format!("`{flag}` needs a value"));
        }
        if options.insert(flag.clone(), value.clone()).is_some() {
            return Err(format!("duplicate option `{flag}`"));
        }
        at += 2;
    }
    Ok(options)
}

fn required(options: &mut BTreeMap<String, String>, name: &str) -> Result<String, String> {
    options
        .remove(name)
        .ok_or_else(|| format!("`{name}` is required"))
}

fn optional_count(
    options: &mut BTreeMap<String, String>,
    name: &str,
    default: u32,
) -> Result<u32, String> {
    match options.remove(name) {
        None => Ok(default),
        Some(text) => text
            .parse()
            .map_err(|_| format!("`{name}` needs a positive integer, got `{text}`")),
    }
}

fn reject_unknown(options: BTreeMap<String, String>) -> Result<(), String> {
    match options.keys().next() {
        None => Ok(()),
        Some(flag) => Err(format!("unknown option `{flag}`")),
    }
}

fn freeze(args: &[String]) -> Result<String, String> {
    let mut options = parse_options(args)?;
    let ledger = PathBuf::from(required(&mut options, "--ledger")?);
    let queries = PathBuf::from(required(&mut options, "--queries")?);
    let out = PathBuf::from(required(&mut options, "--out")?);
    reject_unknown(options)?;

    let manifest = freeze_campaign(&ledger, &queries)?;
    write_json_new(&out, &manifest)?;
    Ok(format!(
        "frozen {} files / {} bytes\ncampaign {}\nsuite {} ({})\nmanifest {}\n",
        manifest.ledger.files.len(),
        manifest.ledger.bytes,
        manifest.campaign_id,
        manifest.suite_name,
        manifest.suite_basis,
        out.display(),
    ))
}

fn run(args: &[String]) -> Result<String, String> {
    let mut options = parse_options(args)?;
    let campaign_path = PathBuf::from(required(&mut options, "--campaign")?);
    let ledger = PathBuf::from(required(&mut options, "--ledger")?);
    let queries = PathBuf::from(required(&mut options, "--queries")?);
    let db = PathBuf::from(required(&mut options, "--db")?);
    let implementation = required(&mut options, "--implementation")?;
    let machine = required(&mut options, "--machine")?;
    let out = PathBuf::from(required(&mut options, "--out")?);
    let warmup = optional_count(&mut options, "--warmup", 5)?;
    let cycles = optional_count(&mut options, "--cycles", 100)?;
    reject_unknown(options)?;

    let manifest: CampaignManifest = read_json(&campaign_path)?;
    let report = run_campaign(
        &manifest,
        &ledger,
        &queries,
        &db,
        &implementation,
        &machine,
        warmup,
        cycles,
    )?;
    write_json_new(&out, &report)?;

    let mut text = format!(
        "campaign {}\nimplementation {}\nmachine {} / {} / {}\n",
        report.campaign_id, report.implementation, report.machine, report.os, report.arch
    );
    for lane in &report.lanes {
        text.push_str(&summary_line(&lane.lane, &lane.latency));
        text.push('\n');
        for class in &lane.classes {
            text.push_str(&summary_line(
                &format!("  class: {}", class.class),
                &class.latency,
            ));
            text.push('\n');
        }
    }
    text.push_str(&format!("report {}\n", out.display()));
    Ok(text)
}

fn compare(args: &[String]) -> Result<String, String> {
    let mut options = parse_options(args)?;
    let before_path = PathBuf::from(required(&mut options, "--before")?);
    let after_path = PathBuf::from(required(&mut options, "--after")?);
    let out = options.remove("--out").map(PathBuf::from);
    reject_unknown(options)?;

    let before: RunReport = read_json(&before_path)?;
    let after: RunReport = read_json(&after_path)?;
    let comparison = compare_reports(&before, &after)?;
    if let Some(path) = &out {
        write_json_new(path, &comparison)?;
    }
    let mut text = format!(
        "campaign {}\n{} -> {}\n",
        comparison.campaign_id, comparison.before_implementation, comparison.after_implementation,
    );
    for lane in &comparison.lanes {
        text.push_str(&comparison_line(&lane.lane, &lane.before, &lane.after));
        text.push('\n');
        for class in &lane.classes {
            text.push_str(&comparison_line(
                &format!("  class: {}", class.class),
                &class.before,
                &class.after,
            ));
            text.push('\n');
        }
    }
    if let Some(path) = out {
        text.push_str(&format!("comparison {}\n", path.display()));
    }
    Ok(text)
}

fn execute(args: Vec<String>) -> Result<String, String> {
    let Some(command) = args.first() else {
        return Ok(USAGE.into());
    };
    match command.as_str() {
        "help" | "-h" | "--help" => Ok(USAGE.into()),
        "freeze" => freeze(&args[1..]),
        "run" => run(&args[1..]),
        "compare" => compare(&args[1..]),
        other => Err(format!("unknown command `{other}`")),
    }
}

#[cfg(test)]
mod tests {
    use super::execute;

    fn args(text: &str) -> Vec<String> {
        text.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn help_and_argument_errors_are_stable() {
        assert!(execute(vec![]).unwrap().contains("USAGE"));
        assert!(execute(args("--help")).unwrap().contains("USAGE"));
        assert!(execute(args("freeze --ledger x")).is_err());
        assert!(execute(args("unknown")).is_err());
    }
}
