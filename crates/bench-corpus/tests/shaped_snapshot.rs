//! A shaped snapshot (`bench-corpus snapshot`) at test size: the frozen format
//! `tools/r1-bench/verify_r1_snapshot.js` checks, the layout globs, the real
//! claude-code parsers, timestamps before mtimes, and byte-identical
//! regeneration.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use rosetta_adapter_support::{Drift, GroundTruthDoc, glob_match};
use rosetta_bench_corpus::shape::{ShapeSpec, plan};
use rosetta_bench_corpus::snapshot::{files_below, materialize, mtime_ms, sha256_file};
use serde_json::Value;
use sha2::{Digest, Sha256};

fn scratch(label: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join("rosetta-bench-corpus-shaped")
        .join(label);
    remove(&dir);
    fs::create_dir_all(dir.parent().unwrap()).unwrap();
    dir
}

/// A frozen snapshot's directories are 0555: open them before deleting.
fn remove(dir: &Path) {
    if !dir.exists() {
        return;
    }
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        fs::set_permissions(&d, fs::Permissions::from_mode(0o755)).unwrap();
        for entry in fs::read_dir(&d).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                stack.push(entry.path());
            }
        }
    }
    fs::remove_dir_all(dir).unwrap();
}

fn build(label: &str, seed: u64) -> (PathBuf, Value) {
    let out = scratch(label);
    let summary = materialize(&plan(&ShapeSpec::tiny(), seed), &out, 3).expect("materialize");
    (out, summary)
}

fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn dirs(root: &Path) -> Vec<PathBuf> {
    let mut out = vec![root.to_path_buf()];
    let mut i = 0;
    while i < out.len() {
        for entry in fs::read_dir(&out[i]).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                out.push(entry.path());
            }
        }
        i += 1;
    }
    out
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort_by(|l, r| l.as_bytes().cmp(r.as_bytes()));
    v
}

#[test]
fn a_shaped_snapshot_is_frozen_checksummed_and_parses_clean() {
    let (out, summary) = build("format", 3);
    let home = out.join("home");

    // Modes: files 0444, directories 0555, the snapshot root included.
    for d in dirs(&out) {
        assert_eq!(mode(&d), 0o555, "{}", d.display());
    }
    for f in ["SHA256SUMS", "snapshot.json"] {
        assert_eq!(mode(&out.join(f)), 0o444);
    }
    let meta: Value =
        serde_json::from_str(&fs::read_to_string(out.join("snapshot.json")).unwrap()).unwrap();
    assert_eq!(meta, summary);

    // SHA256SUMS lists exactly every file under the data roots, with its hash,
    // and its own hash is the manifest's.
    let sums = fs::read_to_string(out.join("SHA256SUMS")).unwrap();
    assert_eq!(
        meta["manifestSha256"].as_str().unwrap(),
        hex(&Sha256::digest(sums.as_bytes()))
    );
    let mut listed = BTreeMap::new();
    for line in sums.lines() {
        let (sha, path) = line.split_once("  ").unwrap();
        listed.insert(path.to_string(), sha.to_string());
    }
    let mut on_disk = Vec::new();
    // walkInputs, recomputed independently of snapshot.rs.
    let mut walk = Sha256::new();
    let mut totals = (0u64, 0u64);
    for (agent, root) in [("claude-code", ".claude"), ("codex", ".codex")] {
        let base = home.join(root);
        for (rank, rel) in sorted(files_below(&base).unwrap()).iter().enumerate() {
            let abs = base.join(rel);
            let st = fs::metadata(&abs).unwrap();
            assert_eq!(st.permissions().mode() & 0o777, 0o444, "{rel}");
            let key = format!("home/{root}/{rel}");
            assert_eq!(listed.get(&key), Some(&sha256_file(&abs).unwrap()), "{key}");
            on_disk.push(key);
            walk.update(format!(
                "{agent}\t{}\t{}\t{}\n",
                rank + 1,
                st.len(),
                mtime_ms(&st).unwrap()
            ));
            totals.0 += 1;
            totals.1 += st.len();
        }
        assert_eq!(meta["agents"][agent]["dataRoot"], format!("~/{root}"));
    }
    assert_eq!(on_disk.len(), listed.len(), "unlisted or missing files");
    assert_eq!(
        meta["walkInputsDigest"].as_str().unwrap(),
        hex(&walk.finalize())
    );
    assert_eq!(meta["totals"]["files"], totals.0);
    assert_eq!(meta["totals"]["bytes"], totals.1);
    assert_eq!(meta["shapeSummary"]["newestGiant"]["treeBytes"], 600_000);
    assert_eq!(meta["shapeSummary"]["bigTrees"], 5);

    // Layout globs, parsers, stamps before mtimes.
    let gt = GroundTruthDoc::parse(
        &fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../groundtruth/claude-code/groundtruth.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let glob = |id: &str| {
        gt.stream(id)
            .unwrap_or_else(|| panic!("stream {id}"))
            .glob
            .clone()
    };
    let (transcript, subagent, sidecar) = (
        glob("transcript"),
        glob("subagent-transcript"),
        glob("subagent-metadata"),
    );
    let claude = home.join(".claude");
    let mut counts = BTreeMap::new();
    for rel in files_below(&claude).unwrap() {
        let abs = claude.join(&rel);
        let text = fs::read_to_string(&abs).unwrap();
        for needle in ["/Users/", "/home/", "jamesyong"] {
            assert!(
                !text.contains(needle) && !rel.contains(needle),
                "{needle} in {rel}"
            );
        }
        if rel.ends_with(".meta.json") {
            assert!(glob_match(&sidecar, &rel), "{rel}");
            let meta = rosetta_adapter_claude_code::raw::parse_subagent_meta(&text).unwrap();
            let mut drift = Vec::new();
            meta.drift("", &mut drift);
            assert!(drift.is_empty(), "{rel}: {drift:?}");
            *counts.entry("sidecar").or_insert(0) += 1;
            continue;
        }
        let is_subagent = rel.contains("/subagents/");
        assert!(
            glob_match(if is_subagent { &subagent } else { &transcript }, &rel),
            "{rel}"
        );
        let mtime = mtime_ms(&fs::metadata(&abs).unwrap()).unwrap();
        let mut last = String::new();
        for line in text.lines() {
            use rosetta_adapter_claude_code::raw::{ParsedLine, parse_line};
            match parse_line(line) {
                Ok(ParsedLine::Known(record)) => assert!(
                    record.drift_paths().is_empty(),
                    "{rel}: {:?}",
                    record.drift_paths()
                ),
                Ok(ParsedLine::UnknownType { rtype, .. }) => {
                    panic!("{rel}: unknown type {rtype:?}")
                }
                Err(error) => panic!("{rel}: {error}"),
            }
            let value: Value = serde_json::from_str(line).unwrap();
            if let Some(stamp) = value["timestamp"].as_str() {
                last = stamp.to_string();
            }
            if is_subagent && value["uuid"].is_string() {
                assert_eq!(value["isSidechain"], true, "{rel}");
                assert!(value["agentId"].is_string(), "{rel}");
            }
        }
        assert!(!last.is_empty());
        assert!(
            last.as_str() <= iso(mtime).as_str(),
            "{rel}: last record {last} after mtime {}",
            iso(mtime)
        );
        *counts
            .entry(if is_subagent { "subagent" } else { "root" })
            .or_insert(0) += 1;
    }
    let spec = ShapeSpec::tiny();
    assert_eq!(counts["root"], spec.roots);
    assert_eq!(counts["subagent"], spec.subagents);
    assert_eq!(counts["sidecar"], spec.sidecars + spec.sidecar_only);
    remove(&out);
}

#[test]
fn a_shaped_snapshot_regenerates_byte_identically() {
    let (a, sa) = build("determinism-a", 5);
    let (b, sb) = build("determinism-b", 5);
    assert_eq!(sa, sb, "snapshot.json differs");
    assert_eq!(
        fs::read(a.join("SHA256SUMS")).unwrap(),
        fs::read(b.join("SHA256SUMS")).unwrap()
    );
    let rels = sorted(files_below(&a.join("home")).unwrap());
    assert_eq!(rels, sorted(files_below(&b.join("home")).unwrap()));
    for rel in &rels {
        let (fa, fb) = (a.join("home").join(rel), b.join("home").join(rel));
        assert_eq!(
            mtime_ms(&fs::metadata(&fa).unwrap()).unwrap(),
            mtime_ms(&fs::metadata(&fb).unwrap()).unwrap(),
            "{rel}"
        );
    }
    let (c, sc) = build("determinism-c", 6);
    assert_ne!(
        sa["walkInputsDigest"], sc["walkInputsDigest"],
        "the seed had no effect"
    );
    for dir in [a, b, c] {
        remove(&dir);
    }
}

#[test]
fn materialize_refuses_an_existing_directory() {
    let out = scratch("exists");
    fs::create_dir_all(&out).unwrap();
    assert!(materialize(&plan(&ShapeSpec::tiny(), 1), &out, 1).is_err());
    remove(&out);
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn iso(ms: i64) -> String {
    rosetta_bench_corpus::clock::iso8601(ms)
}
