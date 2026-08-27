//! Benchmark-only Tantivy sidecar prototype. This binary is intentionally
//! behind the `tantivy-prototype` feature and performs no live dual writes.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::ExitCode;

use rosetta_search_bench::{
    TantivyRunOptions, TantivyRunReport, run_tantivy_prototype, write_json_new,
};

const USAGE: &str = "\
tantivy-bench — frozen-projection Tantivy prototype

USAGE:
    tantivy-bench run --campaign MANIFEST.json --ledger DIR --queries FILE \\
        --db DB --baseline FTS-RUN.json --index NEW-DIR \\
        --implementation REV --machine LABEL --out RUN.json \\
        [--threads N] [--memory-mb N] [--warmup N] [--cycles N]

RULES:
    The ledger and database must be permission-read-only and match the FTS report.
    The index path must not exist; prototype builds never overwrite artifacts.
    Index data is sealed read-only before at least 100 measured cycles; only
    Tantivy's persistent empty metadata-lock file remains writable.
    Reports contain labels, timings, counts, overlap, and digests — never content or IDs.
";

fn main() -> ExitCode {
    match execute(std::env::args().skip(1).collect()) {
        Ok(output) => {
            print!("{output}");
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("tantivy-bench: {message}\n\n{USAGE}");
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

fn optional_usize(
    options: &mut BTreeMap<String, String>,
    name: &str,
    default: usize,
) -> Result<usize, String> {
    match options.remove(name) {
        None => Ok(default),
        Some(text) => text
            .parse::<usize>()
            .ok()
            .filter(|value| *value > 0)
            .ok_or_else(|| format!("`{name}` needs a positive integer, got `{text}`")),
    }
}

fn optional_u32(
    options: &mut BTreeMap<String, String>,
    name: &str,
    default: u32,
) -> Result<u32, String> {
    optional_usize(options, name, default as usize).and_then(|value| {
        u32::try_from(value).map_err(|_| format!("`{name}` is too large: {value}"))
    })
}

fn reject_unknown(options: BTreeMap<String, String>) -> Result<(), String> {
    match options.keys().next() {
        None => Ok(()),
        Some(flag) => Err(format!("unknown option `{flag}`")),
    }
}

fn millis(nanos: u64) -> f64 {
    nanos as f64 / 1_000_000.0
}

fn gib(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0 * 1024.0)
}

fn render(report: &TantivyRunReport, out: &std::path::Path) -> String {
    let mut text = format!(
        "campaign verified\nimplementation {}\nmachine {} / {} / {}\n\
         build {} docs in {:.3} s, {} segments, {:.3} GiB index / {:.3} GiB SQLite FTS\n\
         resident-reader        {:>8} samples  p50 {:>9.3} ms  p95 {:>9.3} ms  p99 {:>9.3} ms\n",
        report.implementation,
        report.machine,
        report.os,
        report.arch,
        report.build.documents,
        report.build.wall_nanos as f64 / 1_000_000_000.0,
        report.build.segments,
        gib(report.build.index.bytes),
        gib(report.build.sqlite_fts_bytes),
        report.lane.latency.samples,
        millis(report.lane.latency.p50_nanos),
        millis(report.lane.latency.p95_nanos),
        millis(report.lane.latency.p99_nanos),
    );
    for class in &report.lane.classes {
        text.push_str(&format!(
            "  class: {:<16} p50 {:>9.3} ms  p95 {:>9.3} ms  p99 {:>9.3} ms\n",
            class.class,
            millis(class.latency.p50_nanos),
            millis(class.latency.p95_nanos),
            millis(class.latency.p99_nanos),
        ));
    }
    for case in &report.lane.cases {
        text.push_str(&format!(
            "  case: {:<18} overlap {:>3}/{:<3} ({:>6.2}%) top10 {:>2} top1 {} filters {}\n",
            case.label,
            case.overlap,
            case.fts_hits,
            case.overlap_percent_of_fts,
            case.top_10_overlap,
            case.top_1_same,
            case.filter_violations,
        ));
    }
    text.push_str(&format!("report {}\n", out.display()));
    text
}

fn run(args: &[String]) -> Result<String, String> {
    let mut values = parse_options(args)?;
    let campaign = PathBuf::from(required(&mut values, "--campaign")?);
    let ledger = PathBuf::from(required(&mut values, "--ledger")?);
    let queries = PathBuf::from(required(&mut values, "--queries")?);
    let database = PathBuf::from(required(&mut values, "--db")?);
    let baseline = PathBuf::from(required(&mut values, "--baseline")?);
    let index = PathBuf::from(required(&mut values, "--index")?);
    let implementation = required(&mut values, "--implementation")?;
    let machine = required(&mut values, "--machine")?;
    let out = PathBuf::from(required(&mut values, "--out")?);
    let threads = optional_usize(&mut values, "--threads", 4)?;
    let memory_mb = optional_usize(&mut values, "--memory-mb", 1024)?;
    let warmup_cycles = optional_u32(&mut values, "--warmup", 5)?;
    let measured_cycles = optional_u32(&mut values, "--cycles", 100)?;
    reject_unknown(values)?;
    let memory_bytes = memory_mb
        .checked_mul(1024 * 1024)
        .ok_or("--memory-mb overflows this machine")?;
    let options = TantivyRunOptions {
        campaign,
        ledger,
        queries,
        database,
        baseline,
        index,
        implementation,
        machine,
        threads,
        memory_bytes,
        warmup_cycles,
        measured_cycles,
    };
    let report = run_tantivy_prototype(&options)?;
    write_json_new(&out, &report)?;
    Ok(render(&report, &out))
}

fn execute(args: Vec<String>) -> Result<String, String> {
    let Some(command) = args.first() else {
        return Ok(USAGE.into());
    };
    match command.as_str() {
        "help" | "-h" | "--help" => Ok(USAGE.into()),
        "run" => run(&args[1..]),
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
        assert!(execute(args("run --campaign x")).is_err());
        assert!(execute(args("unknown")).is_err());
    }
}
