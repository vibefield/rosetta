//! `rosettad ledger` — offline maintenance on a projection's raw ledger.
//!
//! The one subcommand today is `compact`: it rewrites the closed segments of
//! an existing ledger into the zstd container (`RLZ1`). Readers keep both
//! containers forever and decide per segment from the magic, so a compaction
//! pass is invisible to every consumer — the ledger stays the same append-only
//! frame stream, byte for byte, once decoded.
//!
//! Two safety rules shape the walk. The projection lock is held for the whole
//! pass, so no daemon can be appending; and a reopened [`rosetta_capture::Ledger`]
//! always starts at `last + 1`, so every segment already on disk is closed as
//! far as any future writer is concerned. Torn segments are reported and left
//! exactly as they are: a rewrite could only keep the intact prefix, and
//! dropping a tail is a decision for a person.
//!
//! A real pass also regenerates each runtime's `.segments` index from the
//! segments (`rebuild_segment_index`), so a ledger written before the index
//! existed leaves compaction with one.
//!
//! `--dry-run` classifies from `sniff_segment`, which reads four magic bytes
//! and no frames. It therefore sees the container but not a torn tail: a
//! segment counted as `to rewrite` here can still come back `torn` from the
//! real pass.

use std::path::{Path, PathBuf};

use rosetta_capture::segment::DEFAULT_ZSTD_LEVEL;
use rosetta_capture::{RecompressOutcome, SegmentCodec, recompress_segment, sniff_segment};
use serde_json::{Value, json};

use crate::bench::emit;
use crate::{Flags, ledger_root, parse_flags, projection_lock};

/// zstd's compression range short of its `--ultra` tier. Levels 20–22 widen
/// the match window to 32–128 MiB, and a window is not a write-side cost
/// only: every later decoder of such a segment — a rebuild replay per runtime,
/// the transcript's exact-command fetch — allocates it for as long as the
/// segment is open. Levels 1–19 keep the window at or below 8 MiB. `--long`
/// is the explicit way to choose the 128 MiB window. The negative "fast"
/// levels below 1 trade ratio for speed an offline pass never needs.
const MIN_LEVEL: i32 = 1;
const MAX_LEVEL: i32 = 19;

const SEGMENT_SUFFIX: &str = ".rledger";
/// Segments are `NNNNNNNN.rledger`. Anything else in a runtime directory —
/// `.meta-head`, an abandoned `.rledger.compact`, a stray file — is not a
/// segment and is never opened.
const SEGMENT_DIGITS: usize = 8;

pub(crate) fn cmd_ledger(args: &[String]) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("compact") => cmd_compact(&args[1..]),
        Some(other) => Err(format!(
            "unknown ledger subcommand {other}; expected one of: compact"
        )),
        None => Err("ledger needs a subcommand: compact".to_string()),
    }
}

fn cmd_compact(args: &[String]) -> Result<(), String> {
    let flags = parse_flags(args)?;
    let db = flags.db.clone().ok_or("ledger compact needs --db PATH")?;
    let codec = target_codec(&flags)?;
    let root = ledger_root(&flags, &db);
    if !root.is_dir() {
        return Err(format!("no ledger at {}", root.display()));
    }
    let _projection_lock = projection_lock(&db).map_err(|error| {
        format!(
            "ledger compact: {} is locked — stop the daemon or app that owns it first ({error})",
            db.display()
        )
    })?;

    let report = compact_ledger(&root, codec, flags.dry_run, flags.force)?;
    if flags.json {
        emit(&json_report(&root, codec, &flags, &report));
    } else {
        render(&report, flags.dry_run);
    }
    Ok(())
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Tally {
    segments: u64,
    rewritten: u64,
    unchanged: u64,
    torn: u64,
    bytes_before: u64,
    /// Meaningless under `--dry-run` (nothing was written); the renderers
    /// print `-` there rather than a size no pass produced.
    bytes_after: u64,
}

impl Tally {
    fn record(&mut self, outcome: RecompressOutcome) {
        self.segments = self.segments.saturating_add(1);
        let (before, after) = match outcome {
            RecompressOutcome::Rewritten { before, after } => {
                self.rewritten = self.rewritten.saturating_add(1);
                (before, after)
            }
            RecompressOutcome::Unchanged { bytes } => {
                self.unchanged = self.unchanged.saturating_add(1);
                (bytes, bytes)
            }
            RecompressOutcome::Torn { bytes } => {
                self.torn = self.torn.saturating_add(1);
                (bytes, bytes)
            }
        };
        self.bytes_before = self.bytes_before.saturating_add(before);
        self.bytes_after = self.bytes_after.saturating_add(after);
    }

    fn merge(&mut self, other: &Self) {
        self.segments = self.segments.saturating_add(other.segments);
        self.rewritten = self.rewritten.saturating_add(other.rewritten);
        self.unchanged = self.unchanged.saturating_add(other.unchanged);
        self.torn = self.torn.saturating_add(other.torn);
        self.bytes_before = self.bytes_before.saturating_add(other.bytes_before);
        self.bytes_after = self.bytes_after.saturating_add(other.bytes_after);
    }

    fn ratio(&self) -> Option<f64> {
        (self.bytes_after > 0).then(|| self.bytes_before as f64 / self.bytes_after as f64)
    }
}

#[derive(Debug, Default)]
struct CompactReport {
    runtimes: Vec<(String, Tally)>,
    total: Tally,
}

/// Rewrite every segment of every runtime under `root`. The caller holds the
/// projection lock.
fn compact_ledger(
    root: &Path,
    codec: SegmentCodec,
    dry_run: bool,
    force: bool,
) -> Result<CompactReport, String> {
    let mut report = CompactReport::default();
    for (kind, dir) in runtime_dirs(root)? {
        let mut tally = Tally::default();
        for path in segment_files(&dir)? {
            let outcome = if dry_run {
                classify(&path, codec, force)?
            } else {
                recompress_segment(&path, codec, force)
                    .map_err(|error| format!("compacting {}: {error}", path.display()))?
            };
            tally.record(outcome);
        }
        if !dry_run {
            // Segments keep their numbers and contents, so an existing index
            // stays valid; a ledger written before the index existed gets one.
            rosetta_capture::rebuild_segment_index(&dir)
                .map_err(|error| format!("indexing {}: {error}", dir.display()))?;
        }
        report.total.merge(&tally);
        report.runtimes.push((kind, tally));
    }
    Ok(report)
}

/// What a real pass would do, from the four magic bytes alone. A torn tail is
/// invisible here — see the module header.
fn classify(path: &Path, codec: SegmentCodec, force: bool) -> Result<RecompressOutcome, String> {
    let bytes = std::fs::metadata(path)
        .map_err(|error| format!("stating {}: {error}", path.display()))?
        .len();
    match sniff_segment(path).map_err(|error| format!("sniffing {}: {error}", path.display()))? {
        // A short or unknown header is torn from the first byte.
        None => Ok(RecompressOutcome::Torn { bytes }),
        Some(current) if !force && current.is_compressed() == codec.is_compressed() => {
            Ok(RecompressOutcome::Unchanged { bytes })
        }
        Some(_) => Ok(RecompressOutcome::Rewritten {
            before: bytes,
            after: bytes,
        }),
    }
}

/// The immediate subdirectories of the ledger root, one per runtime kind, in
/// name order. A staged ledger is a `<ledger>.staged` sibling and a retired
/// projection set lives in a `recovery/` sibling of the projection, so neither
/// is reachable from here; a `recovery` child and every dotted entry are
/// skipped anyway so a hand-moved tree cannot be rewritten by accident.
fn runtime_dirs(root: &Path) -> Result<Vec<(String, PathBuf)>, String> {
    let mut out = Vec::new();
    for entry in read_dir(root)? {
        let entry = entry.map_err(|error| format!("listing {}: {error}", root.display()))?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name.starts_with('.') || name == "recovery" {
            continue;
        }
        if !is_dir(&entry)? {
            continue;
        }
        out.push((name.to_string(), entry.path()));
    }
    out.sort();
    Ok(out)
}

/// One runtime's segments in ascending sequence. The names are fixed-width
/// and zero padded, so lexical order is sequence order.
fn segment_files(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut out = Vec::new();
    for entry in read_dir(dir)? {
        let entry = entry.map_err(|error| format!("listing {}: {error}", dir.display()))?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(stem) = name.strip_suffix(SEGMENT_SUFFIX) else {
            continue;
        };
        if stem.len() != SEGMENT_DIGITS || !stem.bytes().all(|byte| byte.is_ascii_digit()) {
            continue;
        }
        if is_dir(&entry)? {
            continue;
        }
        out.push(entry.path());
    }
    out.sort();
    Ok(out)
}

fn read_dir(dir: &Path) -> Result<std::fs::ReadDir, String> {
    std::fs::read_dir(dir).map_err(|error| format!("listing {}: {error}", dir.display()))
}

/// `DirEntry::file_type` does not follow symlinks, so a symlinked runtime
/// directory or segment is left alone instead of being rewritten through.
fn is_dir(entry: &std::fs::DirEntry) -> Result<bool, String> {
    Ok(entry
        .file_type()
        .map_err(|error| format!("stating {}: {error}", entry.path().display()))?
        .is_dir())
}

fn target_codec(flags: &Flags) -> Result<SegmentCodec, String> {
    let level = flags.level.unwrap_or(DEFAULT_ZSTD_LEVEL);
    if !(MIN_LEVEL..=MAX_LEVEL).contains(&level) {
        return Err(format!(
            "--level wants {MIN_LEVEL}..={MAX_LEVEL}, got {level}"
        ));
    }
    Ok(SegmentCodec::Zstd {
        level,
        long_window: flags.long_window,
    })
}

fn render(report: &CompactReport, dry_run: bool) {
    println!(
        "{:<16} {:>8} {:>10} {:>9} {:>5} {:>14} {:>14} {:>8}",
        "runtime",
        "segments",
        if dry_run { "to rewrite" } else { "rewritten" },
        "unchanged",
        "torn",
        "before",
        "after",
        "ratio",
    );
    for (kind, tally) in &report.runtimes {
        print_row(kind, tally, dry_run);
    }
    print_row("total", &report.total, dry_run);
    if report.total.torn > 0 {
        println!(
            "{} torn segment(s) left untouched: a rewrite could only keep the intact prefix",
            report.total.torn
        );
    }
}

fn print_row(name: &str, tally: &Tally, dry_run: bool) {
    let after = if dry_run {
        "-".to_string()
    } else {
        tally.bytes_after.to_string()
    };
    let ratio = match tally.ratio() {
        Some(ratio) if !dry_run => format!("{ratio:.2}x"),
        _ => "-".to_string(),
    };
    println!(
        "{:<16} {:>8} {:>10} {:>9} {:>5} {:>14} {:>14} {:>8}",
        name,
        tally.segments,
        tally.rewritten,
        tally.unchanged,
        tally.torn,
        tally.bytes_before,
        after,
        ratio,
    );
}

fn json_report(root: &Path, codec: SegmentCodec, flags: &Flags, report: &CompactReport) -> Value {
    let (level, long_window) = match codec {
        SegmentCodec::Zstd { level, long_window } => (level, long_window),
        SegmentCodec::Plain => (0, false),
    };
    json!({
        "event": "ledger-compact",
        "ledger": root.display().to_string(),
        "dryRun": flags.dry_run,
        "force": flags.force,
        "codec": {
            "container": "zstd",
            "level": level,
            "longWindow": long_window,
        },
        "runtimes": report
            .runtimes
            .iter()
            .map(|(kind, tally)| {
                let mut value = json_tally(tally, flags.dry_run);
                value["runtime"] = json!(kind);
                value
            })
            .collect::<Vec<_>>(),
        "total": json_tally(&report.total, flags.dry_run),
    })
}

fn json_tally(tally: &Tally, dry_run: bool) -> Value {
    let bytes_after = if dry_run {
        Value::Null
    } else {
        json!(tally.bytes_after)
    };
    let ratio = match tally.ratio() {
        Some(ratio) if !dry_run => json!(ratio),
        _ => Value::Null,
    };
    json!({
        "segments": tally.segments,
        "rewritten": tally.rewritten,
        "unchanged": tally.unchanged,
        "torn": tally.torn,
        "bytesBefore": tally.bytes_before,
        "bytesAfter": bytes_after,
        "ratio": ratio,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs::OpenOptions;
    use std::io::Write;

    use rosetta_capture::{Ledger, LedgerEntry, SourceDef, scan_ledger};

    use super::*;

    const KINDS: [&str; 2] = ["claude-code", "codex"];

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

    /// A synthetic transcript line with the repetition a real one has, so the
    /// container actually shrinks. Nothing here comes from a live home.
    fn record(index: u64) -> Vec<u8> {
        format!(
            "{{\"type\":\"assistant\",\"index\":{index},\"session\":\"synthetic\",\"text\":\"the quick brown fox jumps over the lazy dog and keeps going for a while {index}\"}}\n"
        )
        .into_bytes()
    }

    /// One multi-segment plain ledger for a runtime kind.
    fn plain_ledger(dir: &Path, records: u64) {
        let mut ledger = Ledger::open(dir)
            .unwrap()
            .with_codec(SegmentCodec::Plain)
            .with_max_segment_bytes(4096);
        for index in 0..records {
            ledger
                .append_raw(&source("a.jsonl"), 1, [(index * 200, record(index))])
                .unwrap();
            if index % 10 == 9 {
                // Rotation is decided at checkpoints.
                ledger.checkpoint().unwrap();
            }
        }
        ledger.flush().unwrap();
        drop(ledger);
    }

    fn fixture(root: &Path, kinds: &[&str]) {
        for kind in kinds {
            plain_ledger(&root.join(kind), 300);
        }
    }

    fn magic_of(path: &Path) -> [u8; 4] {
        std::fs::read(path).unwrap()[..4].try_into().unwrap()
    }

    fn entries(root: &Path, kinds: &[&str]) -> BTreeMap<String, Vec<LedgerEntry>> {
        kinds
            .iter()
            .map(|kind| {
                (
                    (*kind).to_string(),
                    scan_ledger(&root.join(kind)).unwrap().entries,
                )
            })
            .collect()
    }

    fn segment_bytes(root: &Path, kinds: &[&str]) -> BTreeMap<PathBuf, Vec<u8>> {
        let mut out = BTreeMap::new();
        for kind in kinds {
            for path in segment_files(&root.join(kind)).unwrap() {
                out.insert(path.clone(), std::fs::read(&path).unwrap());
            }
        }
        out
    }

    fn compact_args(db: &Path, extra: &[&str]) -> Vec<String> {
        let mut args = vec![
            "compact".to_string(),
            "--db".to_string(),
            db.display().to_string(),
        ];
        args.extend(extra.iter().map(|value| (*value).to_string()));
        args
    }

    #[test]
    fn compaction_rewrites_every_runtime_and_keeps_each_entry() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("projection.sqlite");
        let root = db.with_extension("ledger");
        fixture(&root, &KINDS);
        let before = entries(&root, &KINDS);
        let before_bytes: u64 = segment_bytes(&root, &KINDS)
            .values()
            .map(|bytes| bytes.len() as u64)
            .sum();

        cmd_ledger(&compact_args(&db, &[])).unwrap();

        let mut segments = 0;
        for kind in KINDS {
            let dir = root.join(kind);
            let paths = segment_files(&dir).unwrap();
            assert!(
                paths.len() > 1,
                "{kind} needs several segments to be a test"
            );
            for path in &paths {
                assert_eq!(&magic_of(path), b"RLZ1", "{}", path.display());
                assert!(!path.with_extension("rledger.compact").exists());
            }
            segments += paths.len();
            assert_eq!(scan_ledger(&dir).unwrap().torn_segments, 0);
            assert!(
                dir.join(".segments").is_file(),
                "compaction leaves a segment index in {}",
                dir.display()
            );
        }
        assert_eq!(entries(&root, &KINDS), before, "compaction is transparent");

        let after_bytes: u64 = segment_bytes(&root, &KINDS)
            .values()
            .map(|bytes| bytes.len() as u64)
            .sum();
        assert!(
            after_bytes < before_bytes,
            "{segments} segments: {after_bytes} after vs {before_bytes} before"
        );
    }

    #[test]
    fn a_dry_run_writes_nothing() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("projection.sqlite");
        let root = db.with_extension("ledger");
        fixture(&root, &KINDS);
        let before = segment_bytes(&root, &KINDS);

        cmd_ledger(&compact_args(&db, &["--dry-run"])).unwrap();
        cmd_ledger(&compact_args(&db, &["--dry-run", "--json"])).unwrap();
        assert_eq!(segment_bytes(&root, &KINDS), before);

        // The classification is still the truth a real pass acts on.
        let planned = compact_ledger(&root, SegmentCodec::default(), true, false).unwrap();
        assert_eq!(planned.total.rewritten, planned.total.segments);
        assert_eq!(planned.total.unchanged, 0);
        assert_eq!(planned.runtimes.len(), KINDS.len());
    }

    #[test]
    fn the_json_report_states_the_pass_it_ran() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("projection.sqlite");
        let root = db.with_extension("ledger");
        fixture(&root, &["codex"]);

        let mut flags = parse_flags(&["--db".to_string(), db.display().to_string()]).unwrap();
        flags.dry_run = true;
        let planned = compact_ledger(&root, SegmentCodec::default(), true, false).unwrap();
        let value = json_report(&root, SegmentCodec::default(), &flags, &planned);
        assert_eq!(value["dryRun"], json!(true));
        assert_eq!(value["codec"]["level"], json!(DEFAULT_ZSTD_LEVEL));
        assert_eq!(value["runtimes"][0]["runtime"], json!("codex"));
        assert_eq!(value["total"]["bytesAfter"], Value::Null);
        assert_eq!(value["total"]["ratio"], Value::Null);
        assert!(value["total"]["rewritten"].as_u64().unwrap() > 1);

        flags.dry_run = false;
        let done = compact_ledger(&root, SegmentCodec::default(), false, false).unwrap();
        let value = json_report(&root, SegmentCodec::default(), &flags, &done);
        assert_eq!(value["dryRun"], json!(false));
        assert_eq!(value["total"]["torn"], json!(0));
        assert!(value["total"]["bytesAfter"].as_u64().unwrap() < done.total.bytes_before);
        assert!(value["total"]["ratio"].as_f64().unwrap() > 1.0);
    }

    #[test]
    fn a_second_pass_is_unchanged_unless_forced() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("projection.sqlite");
        let root = db.with_extension("ledger");
        fixture(&root, &KINDS);
        let before = entries(&root, &KINDS);

        cmd_ledger(&compact_args(&db, &[])).unwrap();

        let again = compact_ledger(&root, SegmentCodec::default(), false, false).unwrap();
        assert_eq!(again.total.rewritten, 0);
        assert_eq!(again.total.unchanged, again.total.segments);
        assert_eq!(again.total.bytes_before, again.total.bytes_after);

        // A wider window is a different container for the same bytes.
        let wide = SegmentCodec::Zstd {
            level: 10,
            long_window: true,
        };
        let forced = compact_ledger(&root, wide, false, true).unwrap();
        assert_eq!(forced.total.rewritten, forced.total.segments);
        assert_eq!(forced.total.unchanged, 0);
        assert_eq!(entries(&root, &KINDS), before);

        // And through the command surface, which is what an operator types.
        cmd_ledger(&compact_args(&db, &["--force", "--level", "9", "--long"])).unwrap();
        assert_eq!(entries(&root, &KINDS), before);
    }

    #[test]
    fn a_torn_segment_is_reported_and_left_alone() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("projection.sqlite");
        let root = db.with_extension("ledger");
        let dir = root.join("claude-code");
        plain_ledger(&dir, 300);
        let before = scan_ledger(&dir).unwrap();

        // A truncated frame header after the durable tail.
        let paths = segment_files(&dir).unwrap();
        let last = paths.last().unwrap().clone();
        let mut file = OpenOptions::new().append(true).open(&last).unwrap();
        file.write_all(&[2, 0, 0, 1]).unwrap();
        drop(file);

        let report = compact_ledger(&root, SegmentCodec::default(), false, false).unwrap();
        assert_eq!(report.total.torn, 1);
        assert_eq!(report.total.segments, paths.len() as u64);
        assert_eq!(report.total.rewritten, report.total.segments - 1);
        for path in &paths {
            let expected: &[u8; 4] = if path == &last { b"RLG1" } else { b"RLZ1" };
            assert_eq!(&magic_of(path), expected, "{}", path.display());
        }

        let after = scan_ledger(&dir).unwrap();
        assert_eq!(after.entries, before.entries);
        assert_eq!(after.torn_segments, 1);

        // A torn segment is a report, not a failure: the command still exits
        // clean and the compacted majority stays compacted.
        cmd_ledger(&compact_args(&db, &[])).unwrap();
        assert_eq!(&magic_of(&last), b"RLG1");
        assert_eq!(scan_ledger(&dir).unwrap().entries, before.entries);
    }

    #[test]
    fn a_locked_projection_is_refused() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("projection.sqlite");
        let root = db.with_extension("ledger");
        fixture(&root, &["codex"]);
        let before = segment_bytes(&root, &["codex"]);

        let owner = projection_lock(&db).unwrap();
        let error = cmd_ledger(&compact_args(&db, &[])).unwrap_err();
        assert!(
            error.contains("is locked — stop the daemon or app that owns it first"),
            "{error}"
        );
        assert_eq!(segment_bytes(&root, &["codex"]), before);

        drop(owner);
        cmd_ledger(&compact_args(&db, &[])).unwrap();
        assert_eq!(
            &magic_of(&segment_files(&root.join("codex")).unwrap()[0]),
            b"RLZ1"
        );
    }

    #[test]
    fn arguments_are_validated_before_anything_is_touched() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("projection.sqlite");
        let root = db.with_extension("ledger");
        fixture(&root, &["codex"]);
        let before = segment_bytes(&root, &["codex"]);

        assert!(cmd_ledger(&[]).unwrap_err().contains("needs a subcommand"));
        assert!(
            cmd_ledger(&["vacuum".to_string()])
                .unwrap_err()
                .contains("unknown ledger subcommand vacuum")
        );
        assert!(
            cmd_ledger(&["compact".to_string()])
                .unwrap_err()
                .contains("needs --db PATH")
        );
        assert!(
            cmd_ledger(&compact_args(&db, &["--level", "99"]))
                .unwrap_err()
                .contains("--level wants 1..=19")
        );
        assert!(
            cmd_ledger(&compact_args(&temp.path().join("absent.sqlite"), &[]))
                .unwrap_err()
                .contains("no ledger at")
        );
        assert_eq!(segment_bytes(&root, &["codex"]), before);
    }

    #[test]
    fn only_numbered_segments_are_considered() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("projection.sqlite");
        let root = db.with_extension("ledger");
        let dir = root.join("codex");
        plain_ledger(&dir, 60);
        let real = segment_files(&dir).unwrap().len();
        for name in [
            ".meta-head",
            "00000000.rledger.compact",
            "0000000.rledger",
            "notes.txt",
        ] {
            std::fs::write(dir.join(name), b"not a segment").unwrap();
        }
        std::fs::create_dir_all(root.join(".hidden")).unwrap();
        std::fs::create_dir_all(root.join("recovery")).unwrap();
        std::fs::write(root.join("stray.txt"), b"not a runtime").unwrap();

        assert_eq!(segment_files(&dir).unwrap().len(), real);
        let kinds: Vec<String> = runtime_dirs(&root)
            .unwrap()
            .into_iter()
            .map(|(kind, _)| kind)
            .collect();
        assert_eq!(kinds, vec!["codex".to_string()]);

        let report = compact_ledger(&root, SegmentCodec::default(), false, false).unwrap();
        assert_eq!(report.total.segments, real as u64);
        assert_eq!(
            std::fs::read(dir.join("notes.txt")).unwrap(),
            b"not a segment"
        );
    }
}
