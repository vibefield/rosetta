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
//! Document frames that carry a media claim (a sent image or video) are also
//! tallied by capture status, with the bytes captured: what became of each
//! sent file, as counts only — no path and no content. The text report adds
//! those lines only for a ledger that holds such frames; the JSON report
//! always carries a `media` object, per runtime and in `total`.
//!
//! Nothing is written and no lock is taken: a ledger with a live writer reads
//! as a snapshot whose newest segment may end torn. A torn segment is
//! reported, not an error, exactly as `ledger compact` treats one.

use std::path::Path;

use rosetta_capture::{EntryTally, LedgerVerify, verify_ledger};
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

/// The media frames of one tally or a sum of them, by capture status:
/// `count` evaluates one field over whatever is being reported.
fn json_media(count: impl Fn(fn(&EntryTally) -> u64) -> u64) -> Value {
    json!({
        "frames": count(EntryTally::media_frames),
        "captured": count(|tally| tally.media_captured),
        "capturedBytes": count(|tally| tally.media_captured_bytes),
        "missing": count(|tally| tally.media_missing),
        "tooLarge": count(|tally| tally.media_too_large),
        "unsupported": count(|tally| tally.media_unsupported),
        "changedDuringRead": count(|tally| tally.media_changed_during_read),
    })
}

/// The same tally as text, or nothing where there is no media frame: a
/// ledger without one prints exactly what it always did.
fn media_text(count: impl Fn(fn(&EntryTally) -> u64) -> u64) -> Option<String> {
    (count(EntryTally::media_frames) > 0).then(|| {
        format!(
            "captured {} ({} bytes) missing {} too_large {} unsupported {} changed_during_read {}",
            count(|tally| tally.media_captured),
            count(|tally| tally.media_captured_bytes),
            count(|tally| tally.media_missing),
            count(|tally| tally.media_too_large),
            count(|tally| tally.media_unsupported),
            count(|tally| tally.media_changed_during_read),
        )
    })
}

/// One tally field summed over every runtime.
fn total(report: &[RuntimeVerify]) -> impl Fn(fn(&EntryTally) -> u64) -> u64 + '_ {
    move |field| {
        report
            .iter()
            .map(|runtime| field(&runtime.ledger.tally))
            .sum()
    }
}

/// The text report: one row per runtime, each runtime's source set digest
/// and media frames, then the root digests.
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
    for runtime in report {
        if let Some(media) = media_text(|field| field(&runtime.ledger.tally)) {
            line(format!("{:<16} media {media}", runtime.runtime));
        }
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
    if let Some(media) = media_text(total(report)) {
        line(format!("media frames  {media}"));
    }
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
            "media": json_media(total(report)),
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
        "media": json_media(|field| field(&ledger.tally)),
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

    use rosetta_capture::{
        CapturedDocument, DocumentCaptureStatus, Ledger, MediaClaim, SegmentCodec, SourceDef,
    };

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

    /// How many media frames of each status [`media_fixture`] writes, each
    /// count different so a transposed field shows.
    const MEDIA: [(DocumentCaptureStatus, u64); 5] = [
        (DocumentCaptureStatus::Captured, 2),
        (DocumentCaptureStatus::Missing, 3),
        (DocumentCaptureStatus::TooLarge, 1),
        (DocumentCaptureStatus::Unsupported, 4),
        (DocumentCaptureStatus::ChangedDuringRead, 5),
    ];

    /// One plain segment with an HTML document and media frames of every
    /// capture status. The bytes are synthetic; no file is read.
    fn media_fixture(root: &Path, kind: &str) {
        let mut ledger = Ledger::open(&root.join(kind))
            .unwrap()
            .with_codec(SegmentCodec::Plain);
        let source = source("a.jsonl");
        ledger
            .append_raw(&source, 1, [(0, b"{\"type\":\"user\"}\n".to_vec())])
            .unwrap();
        let document = |status, media: Option<u64>, bytes: &[u8]| CapturedDocument {
            path: "/fixture/out/clip.mp4".to_string(),
            captured_at: 1_790_000_000_000,
            status,
            recovered_from: None,
            media: media.map(|expected_len| MediaClaim {
                media_type: "video/mp4".to_string(),
                expected_len,
            }),
            bytes: bytes.to_vec(),
        };
        let html = document(
            DocumentCaptureStatus::Captured,
            None,
            b"<p>s3cret pages</p>",
        );
        ledger.append_document(&source, 1, 0, html).unwrap();
        for (status, count) in MEDIA {
            for index in 0..count {
                let media = if status == DocumentCaptureStatus::Captured {
                    document(status, Some(13), b"s3cret bytes!")
                } else {
                    document(status, Some(70 + index), b"")
                };
                ledger.append_document(&source, 1, 0, media).unwrap();
            }
        }
        ledger.complete_generation(&source, 1, 1, true).unwrap();
        ledger.flush().unwrap();
    }

    #[test]
    fn media_frames_are_tallied_by_status_and_their_segment_reads_clean() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("projection.sqlite");
        let root = db.with_extension("ledger");
        media_fixture(&root, "claude-code");
        fixture(&root, "codex");

        let report = verify_root(&root).unwrap();
        let plain = json_report(&root, &report);
        let media = json!({
            "frames": 15,
            "captured": 2,
            "capturedBytes": 26,
            "missing": 3,
            "tooLarge": 1,
            "unsupported": 4,
            "changedDuringRead": 5,
        });
        let none = json!({
            "frames": 0,
            "captured": 0,
            "capturedBytes": 0,
            "missing": 0,
            "tooLarge": 0,
            "unsupported": 0,
            "changedDuringRead": 0,
        });
        assert_eq!(plain["runtimes"][0]["runtime"], json!("claude-code"));
        assert_eq!(plain["runtimes"][0]["media"], media);
        assert_eq!(plain["runtimes"][1]["media"], none);
        assert_eq!(plain["total"]["media"], media);
        // Media frames are document frames too; the HTML page is the sixteenth.
        assert_eq!(plain["runtimes"][0]["documents"], json!(16));
        assert_eq!(plain["runtimes"][0]["documentBytes"], json!(26 + 19));
        // A segment holding document frames is whole, not torn at its first.
        assert_eq!(plain["total"]["tornSegments"], json!(0));
        assert_eq!(plain["runtimes"][0]["segments"][0]["clean"], json!(true));
        assert_eq!(plain["runtimes"][0]["segments"][0]["entries"], json!(18));

        // Text: one line for the runtime that holds media frames, one total.
        let text = render(&report);
        let counts =
            "captured 2 (26 bytes) missing 3 too_large 1 unsupported 4 changed_during_read 5";
        let lines: Vec<&str> = text.lines().filter(|line| line.contains("media")).collect();
        assert_eq!(
            lines,
            [
                format!("{:<16} media {counts}", "claude-code"),
                format!("media frames  {counts}"),
            ],
            "{text}"
        );
        // Counts only: neither report names a file or shows its bytes.
        for output in [text, plain.to_string()] {
            assert!(!output.contains("clip.mp4") && !output.contains("s3cret"));
        }
        cmd_ledger(&args("verify", &db, &[])).unwrap();
        cmd_ledger(&args("verify", &db, &["--json"])).unwrap();

        // So compaction rewrites it instead of skipping it, entries intact.
        cmd_ledger(&args("compact", &db, &[])).unwrap();
        let compacted = json_report(&root, &verify_root(&root).unwrap());
        assert_eq!(
            compacted["runtimes"][0]["segments"][0]["container"],
            json!("zstd")
        );
        assert_eq!(compacted["total"]["tornSegments"], json!(0));
        assert_eq!(compacted["total"]["media"], media);
        assert_eq!(compacted["entryDigest"], plain["entryDigest"]);
        assert_eq!(compacted["sourceSetDigest"], plain["sourceSetDigest"]);
        assert_eq!(compacted["frameDigest"], plain["frameDigest"]);
    }

    /// A ledger with no media frame prints the text it always did, and its
    /// JSON only gains the zeroed `media` objects.
    #[test]
    fn a_ledger_without_media_frames_prints_no_media_line() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("plain.ledger");
        fixture(&root, "claude-code");
        fixture(&root, "codex");
        let report = verify_root(&root).unwrap();
        let text = render(&report);
        assert!(!text.contains("media"), "{text}");
        assert_eq!(text.lines().count(), 1 + 2 + 2 + 5, "{text}");
        let json = json_report(&root, &report);
        assert_eq!(json["total"]["media"]["frames"], json!(0));
        assert_eq!(json["runtimes"][1]["media"]["captured"], json!(0));
        assert_eq!(json["total"]["media"].as_object().unwrap().len(), 7);
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
