//! `rosettad ledger verify` — read-only evidence about a projection's raw
//! ledger, for proving that two builds of the same inputs wrote the same one.
//!
//! Per runtime and segment it reports the container, disk and decoded bytes,
//! whether the frame walk ended cleanly, and a digest of the decoded frame
//! bytes — what `ledger compact` proves its copies against, so a container
//! change keeps it. Per runtime it adds the entry digest: blake3 over a
//! canonical encoding of every decoded entry in visit order, which also
//! ignores segment boundaries, file ids and batch packing
//! (`rosetta_capture::verify_ledger`). Equal entry digests mean a replay sees
//! the same entry stream; equal frame digests additionally mean the decoded
//! segments are byte-identical. Metadata entries carry run-dependent values
//! (discovery times), so the entry digest is also reported split: a source
//! digest over the raw, document (without its observation time) and seal
//! entries — what two builds of the same read-only inputs must share — and a
//! meta digest. The source digest also
//! follows the order sources interleave in, which only a path walk fixes; the
//! source set digest folds each source's own entries in path order instead,
//! so it is the equality measure for root-tree runtimes, whose walk may frame
//! sources in another order on every run. The root digests fold every
//! runtime's, in directory-name order.
//!
//! Nothing is written and no lock is taken: a ledger with a live writer reads
//! as a snapshot whose newest segment may end torn. A torn segment is
//! reported, not an error, exactly as `ledger compact` treats one.

use std::path::Path;

use rosetta_capture::{LedgerVerify, verify_ledger};
use serde_json::{Value, json};

use crate::bench::emit;
use crate::ledger_cmd::runtime_dirs;
use crate::parse_flags;

pub(crate) fn cmd_verify(args: &[String]) -> Result<(), String> {
    let flags = parse_flags(args)?;
    let root = match (&flags.ledger, &flags.db) {
        (Some(ledger), _) => ledger.clone(),
        (None, Some(db)) => db.with_extension("ledger"),
        (None, None) => return Err("ledger verify needs --db PATH or --ledger DIR".to_string()),
    };
    if !root.is_dir() {
        return Err(format!("no ledger at {}", root.display()));
    }
    let report = verify_root(&root)?;
    if flags.json {
        emit(&json_report(&root, &report));
    } else {
        print!("{}", render(&report));
    }
    Ok(())
}

struct RuntimeVerify {
    runtime: String,
    ledger: LedgerVerify,
}

fn verify_root(root: &Path) -> Result<Vec<RuntimeVerify>, String> {
    runtime_dirs(root)?
        .into_iter()
        .map(|(runtime, dir)| {
            let ledger = verify_ledger(&dir)
                .map_err(|error| format!("verifying {}: {error}", dir.display()))?;
            Ok(RuntimeVerify { runtime, ledger })
        })
        .collect()
}

/// Fold one digest per runtime, keyed by its directory name.
fn fold(report: &[RuntimeVerify], digest: impl Fn(&LedgerVerify) -> blake3::Hash) -> String {
    let mut hasher = blake3::Hasher::new();
    for runtime in report {
        hasher.update(&(runtime.runtime.len() as u64).to_be_bytes());
        hasher.update(runtime.runtime.as_bytes());
        hasher.update(digest(&runtime.ledger).as_bytes());
    }
    hasher.finalize().to_hex().to_string()
}

fn container(compressed: Option<bool>) -> &'static str {
    match compressed {
        Some(true) => "zstd",
        Some(false) => "plain",
        None => "unknown",
    }
}

fn disk_bytes(ledger: &LedgerVerify) -> u64 {
    ledger
        .segments
        .iter()
        .map(|segment| segment.disk_bytes)
        .sum()
}

fn decoded_bytes(ledger: &LedgerVerify) -> u64 {
    ledger
        .segments
        .iter()
        .map(|segment| segment.decoded_bytes)
        .sum()
}

/// The text report: one row per runtime, each runtime's source set digest,
/// then the root digests.
fn render(report: &[RuntimeVerify]) -> String {
    let mut out = String::new();
    let mut line = |text: String| {
        out.push_str(&text);
        out.push('\n');
    };
    line(format!(
        "{:<16} {:>8} {:>5} {:>10} {:>14} {:>14}  entry digest",
        "runtime", "segments", "torn", "entries", "disk", "decoded"
    ));
    for runtime in report {
        let ledger = &runtime.ledger;
        line(format!(
            "{:<16} {:>8} {:>5} {:>10} {:>14} {:>14}  {}",
            runtime.runtime,
            ledger.segments.len(),
            ledger.torn_segments(),
            ledger.tally.entries,
            disk_bytes(ledger),
            decoded_bytes(ledger),
            ledger.entry_digest.to_hex(),
        ));
    }
    for runtime in report {
        line(format!(
            "{:<16} source set digest {}",
            runtime.runtime,
            runtime.ledger.source_set_digest.to_hex()
        ));
    }
    line(format!(
        "entry digest  {}",
        fold(report, |ledger| ledger.entry_digest)
    ));
    line(format!(
        "source digest {}",
        fold(report, |ledger| ledger.source_digest)
    ));
    line(format!(
        "source set digest {}",
        fold(report, |ledger| ledger.source_set_digest)
    ));
    line(format!(
        "meta digest   {}",
        fold(report, |ledger| ledger.meta_digest)
    ));
    line(format!(
        "frame digest  {}",
        fold(report, LedgerVerify::frame_digest)
    ));
    let torn: usize = report
        .iter()
        .map(|runtime| runtime.ledger.torn_segments())
        .sum();
    if torn > 0 {
        line(format!(
            "{torn} torn segment(s): digests cover their intact prefixes"
        ));
    }
    out
}

fn json_report(root: &Path, report: &[RuntimeVerify]) -> Value {
    let sum = |value: fn(&LedgerVerify) -> u64| -> u64 {
        report.iter().map(|runtime| value(&runtime.ledger)).sum()
    };
    json!({
        "event": "ledger-verify",
        "ledger": root.display().to_string(),
        "entryDigest": fold(report, |ledger| ledger.entry_digest),
        "sourceDigest": fold(report, |ledger| ledger.source_digest),
        "sourceSetDigest": fold(report, |ledger| ledger.source_set_digest),
        "metaDigest": fold(report, |ledger| ledger.meta_digest),
        "frameDigest": fold(report, LedgerVerify::frame_digest),
        "runtimes": report.iter().map(json_runtime).collect::<Vec<_>>(),
        "total": {
            "segments": sum(|ledger| ledger.segments.len() as u64),
            "tornSegments": sum(|ledger| ledger.torn_segments() as u64),
            "entries": sum(|ledger| ledger.tally.entries),
            "rawRecords": sum(|ledger| ledger.tally.raw_records),
            "rawPayloadBytes": sum(|ledger| ledger.tally.raw_payload_bytes),
            "documents": sum(|ledger| ledger.tally.documents),
            "documentBytes": sum(|ledger| ledger.tally.document_bytes),
            "seals": sum(|ledger| ledger.tally.seals),
            "metas": sum(|ledger| ledger.tally.metas),
            "diskBytes": sum(disk_bytes),
            "decodedBytes": sum(decoded_bytes),
        },
    })
}

fn json_runtime(runtime: &RuntimeVerify) -> Value {
    let ledger = &runtime.ledger;
    json!({
        "runtime": runtime.runtime,
        "entryDigest": ledger.entry_digest.to_hex().to_string(),
        "sourceDigest": ledger.source_digest.to_hex().to_string(),
        "sourceSetDigest": ledger.source_set_digest.to_hex().to_string(),
        "metaDigest": ledger.meta_digest.to_hex().to_string(),
        "frameDigest": ledger.frame_digest().to_hex().to_string(),
        "indexDigest": ledger.index_digest.map(|digest| digest.to_hex().to_string()),
        "entries": ledger.tally.entries,
        "rawRecords": ledger.tally.raw_records,
        "rawPayloadBytes": ledger.tally.raw_payload_bytes,
        "documents": ledger.tally.documents,
        "documentBytes": ledger.tally.document_bytes,
        "seals": ledger.tally.seals,
        "metas": ledger.tally.metas,
        "tornSegments": ledger.torn_segments(),
        "diskBytes": disk_bytes(ledger),
        "decodedBytes": decoded_bytes(ledger),
        "segments": ledger
            .segments
            .iter()
            .map(|segment| {
                json!({
                    "segment": segment.sequence,
                    "container": container(segment.compressed),
                    "diskBytes": segment.disk_bytes,
                    "decodedBytes": segment.decoded_bytes,
                    "clean": segment.clean,
                    "entries": segment.entries,
                    "digest": segment.frame_digest.to_hex().to_string(),
                })
            })
            .collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use std::fs::OpenOptions;
    use std::io::Write;

    use rosetta_capture::{Ledger, SegmentCodec, SourceDef};

    use super::*;
    use crate::ledger_cmd::cmd_ledger;

    fn source(path: &str) -> SourceDef {
        SourceDef {
            ledger_only: false,
            relative_path: path.to_string(),
            stream_id: format!("stream:{path}"),
            session_id: format!("session:{path}"),
            native_session_id: format!("native:{path}"),
            agent_version: Some("1.0".to_string()),
        }
    }

    /// Synthetic records; nothing here comes from a live home.
    fn fixture(root: &Path, kind: &str) {
        let mut ledger = Ledger::open(&root.join(kind))
            .unwrap()
            .with_codec(SegmentCodec::Plain)
            .with_max_segment_bytes(4096);
        for index in 0..200_u64 {
            let payload = format!("{{\"type\":\"user\",\"index\":{index},\"kind\":\"{kind}\"}}\n");
            ledger
                .append_raw(&source("a.jsonl"), 1, [(index * 100, payload.into_bytes())])
                .unwrap();
            if index % 10 == 9 {
                ledger.checkpoint().unwrap();
            }
        }
        ledger
            .complete_generation(&source("a.jsonl"), 1, 1, true)
            .unwrap();
        ledger.flush().unwrap();
    }

    fn args(subcommand: &str, db: &Path, extra: &[&str]) -> Vec<String> {
        let mut args = vec![
            subcommand.to_string(),
            "--db".to_string(),
            db.display().to_string(),
        ];
        args.extend(extra.iter().map(|value| (*value).to_string()));
        args
    }

    #[test]
    fn verification_survives_compaction_and_reports_torn_segments() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("projection.sqlite");
        let root = db.with_extension("ledger");
        fixture(&root, "claude-code");
        fixture(&root, "codex");

        let plain = json_report(&root, &verify_root(&root).unwrap());
        assert_eq!(plain["event"], json!("ledger-verify"));
        assert_eq!(plain["total"]["tornSegments"], json!(0));
        assert_eq!(plain["total"]["entries"], json!(402));
        assert_eq!(plain["total"]["seals"], json!(2));
        let runtimes = plain["runtimes"].as_array().unwrap();
        assert_eq!(runtimes[0]["runtime"], json!("claude-code"));
        assert_eq!(runtimes[1]["runtime"], json!("codex"));
        assert!(runtimes[0]["segments"].as_array().unwrap().len() > 2);
        assert_eq!(runtimes[0]["segments"][0]["container"], json!("plain"));
        assert_ne!(runtimes[0]["entryDigest"], runtimes[1]["entryDigest"]);

        // Through the command surface, text and JSON.
        cmd_ledger(&args("verify", &db, &[])).unwrap();
        cmd_ledger(&args("verify", &db, &["--json"])).unwrap();

        cmd_ledger(&args("compact", &db, &["--force"])).unwrap();
        let compacted = json_report(&root, &verify_root(&root).unwrap());
        assert_eq!(compacted["entryDigest"], plain["entryDigest"]);
        assert_eq!(compacted["sourceSetDigest"], plain["sourceSetDigest"]);
        assert_eq!(compacted["frameDigest"], plain["frameDigest"]);
        assert_eq!(
            compacted["total"]["decodedBytes"],
            plain["total"]["decodedBytes"]
        );
        assert_eq!(
            compacted["runtimes"][0]["segments"][0]["container"],
            json!("zstd")
        );
        assert!(
            compacted["total"]["diskBytes"].as_u64().unwrap()
                < plain["total"]["diskBytes"].as_u64().unwrap()
        );

        // A truncated frame header after the durable tail of one runtime.
        let last = root.join("codex").join(format!(
            "{:08}.rledger",
            runtimes[1]["segments"].as_array().unwrap().len() - 1
        ));
        let mut file = OpenOptions::new().append(true).open(&last).unwrap();
        file.write_all(&[2, 0, 0, 1]).unwrap();
        drop(file);
        let torn = json_report(&root, &verify_root(&root).unwrap());
        assert_eq!(torn["total"]["tornSegments"], json!(1));
        assert_eq!(torn["runtimes"][1]["tornSegments"], json!(1));
        // The intact prefix is what a replay reads, so the entries still agree.
        assert_eq!(torn["entryDigest"], plain["entryDigest"]);
        assert_eq!(torn["sourceSetDigest"], plain["sourceSetDigest"]);
        cmd_ledger(&args("verify", &db, &[])).unwrap();
    }

    /// Two sources whose records `interleave` or not, in one runtime ledger.
    fn two_sources(dir: &Path, interleave: bool) {
        let mut ledger = Ledger::open(dir).unwrap();
        let order: Vec<(&str, u64)> = if interleave {
            (0..4)
                .flat_map(|index| [("b.jsonl", index), ("a.jsonl", index)])
                .collect()
        } else {
            ["a.jsonl", "b.jsonl"]
                .into_iter()
                .flat_map(|path| (0..4).map(move |index| (path, index)))
                .collect()
        };
        for (path, index) in order {
            let payload = format!("{{\"type\":\"user\",\"index\":{index}}}\n");
            ledger
                .append_raw(&source(path), 1, [(index * 100, payload.into_bytes())])
                .unwrap();
        }
        for path in ["a.jsonl", "b.jsonl"] {
            ledger
                .complete_generation(&source(path), 1, 1, true)
                .unwrap();
        }
        ledger.flush().unwrap();
    }

    #[test]
    fn ledger_verify_prints_source_set_digest() {
        let temp = tempfile::tempdir().unwrap();
        let grouped = temp.path().join("grouped.ledger");
        let interleaved = temp.path().join("interleaved.ledger");
        for (root, interleave) in [(&grouped, false), (&interleaved, true)] {
            fixture(root, "claude-code");
            two_sources(&root.join("codex"), interleave);
        }
        let report = verify_root(&grouped).unwrap();
        let json = json_report(&grouped, &report);
        let text = render(&report);

        // Per runtime, text and JSON, exactly the ledger's own digest.
        let mut root = blake3::Hasher::new();
        for (index, runtime) in ["claude-code", "codex"].into_iter().enumerate() {
            let digest = verify_ledger(&grouped.join(runtime))
                .unwrap()
                .source_set_digest;
            let hex = digest.to_hex().to_string();
            assert_eq!(json["runtimes"][index]["runtime"], json!(runtime));
            assert_eq!(json["runtimes"][index]["sourceSetDigest"], json!(hex));
            assert!(
                text.lines()
                    .any(|line| line == format!("{runtime:<16} source set digest {hex}")),
                "{text}"
            );
            root.update(&(runtime.len() as u64).to_be_bytes());
            root.update(runtime.as_bytes());
            root.update(digest.as_bytes());
        }
        // At the root: the runtimes folded in id order.
        let root = root.finalize().to_hex().to_string();
        assert_eq!(json["sourceSetDigest"], json!(root));
        assert!(
            text.lines()
                .any(|line| line == format!("source set digest {root}")),
            "{text}"
        );

        // Another interleaving of the same sources: only the order-free
        // digest still agrees.
        let other = json_report(&interleaved, &verify_root(&interleaved).unwrap());
        assert_eq!(other["sourceSetDigest"], json["sourceSetDigest"]);
        assert_ne!(other["sourceDigest"], json["sourceDigest"]);
        assert_eq!(
            other["runtimes"][0]["sourceSetDigest"],
            json["runtimes"][0]["sourceSetDigest"]
        );
        assert_eq!(
            other["runtimes"][0]["sourceDigest"],
            json["runtimes"][0]["sourceDigest"]
        );
        assert_ne!(
            other["runtimes"][1]["sourceDigest"],
            json["runtimes"][1]["sourceDigest"]
        );
    }

    #[test]
    fn a_ledger_directory_can_be_named_directly() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("elsewhere.ledger");
        fixture(&root, "codex");
        cmd_ledger(&[
            "verify".to_string(),
            "--ledger".to_string(),
            root.display().to_string(),
            "--json".to_string(),
        ])
        .unwrap();
        assert!(
            cmd_ledger(&["verify".to_string()])
                .unwrap_err()
                .contains("needs --db PATH or --ledger DIR")
        );
        assert!(
            cmd_ledger(&args("verify", &temp.path().join("absent.sqlite"), &[]))
                .unwrap_err()
                .contains("no ledger at")
        );
    }
}
