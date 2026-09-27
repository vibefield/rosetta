//! Layout pruning is lossless: a walk that skips every directory no accepted
//! stream can reach yields exactly the files (and truncation) of the full walk
//! filtered by the same first-match include, for every captured layout.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use rosetta_adapter_support::{GroundTruthDoc, StreamSpec};
use rosetta_capture::{walk_filtered, walk_pruned};

/// splitmix64: deterministic, dependency-free test randomness.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound as u64) as usize
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}

fn groundtruth_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../groundtruth")
}

fn layouts() -> Vec<(PathBuf, GroundTruthDoc)> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(groundtruth_dir()).unwrap() {
        let dir = entry.unwrap().path();
        let canonical = dir.join("groundtruth.json");
        if canonical.is_file() {
            let doc = GroundTruthDoc::parse(&std::fs::read_to_string(&canonical).unwrap()).unwrap();
            out.push((dir, doc));
        }
    }
    out.sort_by(|left, right| left.1.agent.cmp(&right.1.agent));
    assert_eq!(
        out.len(),
        9,
        "one canonical ground truth per captured agent"
    );
    out
}

/// The stream subsets a walk may accept: everything (discovery, meta),
/// each stream alone, and a few random driver-like selections (live poll).
fn subsets(doc: &GroundTruthDoc, rng: &mut Rng) -> Vec<BTreeSet<String>> {
    let ids: Vec<String> = doc.streams.iter().map(|stream| stream.id.clone()).collect();
    let mut out = vec![ids.iter().cloned().collect::<BTreeSet<_>>()];
    out.extend(ids.iter().map(|id| BTreeSet::from([id.clone()])));
    for _ in 0..6 {
        out.push(ids.iter().filter(|_| rng.below(3) == 0).cloned().collect());
    }
    out
}

/// Returns how many directories the pruned walk declined to enter.
fn assert_lossless(
    doc: &GroundTruthDoc,
    root: &Path,
    accepted: &BTreeSet<String>,
    cap: usize,
) -> usize {
    let accept = |stream: &StreamSpec| accepted.contains(&stream.id);
    let include = |path: &str| doc.matching_stream(path).is_some_and(accept);
    let full = walk_filtered(root, cap, include).unwrap();
    let mut declined = 0;
    let pruned = walk_pruned(
        root,
        cap,
        |dir| {
            let enter = doc.may_claim_below(dir, accept);
            declined += usize::from(!enter);
            enter
        },
        include,
    )
    .unwrap();
    assert_eq!(
        pruned.files,
        full.files,
        "{} under {} accepting {accepted:?} (cap {cap})",
        doc.agent,
        root.display()
    );
    assert_eq!(pruned.truncated, full.truncated);
    declined
}

fn directories(root: &Path, out: &mut Vec<PathBuf>) {
    out.push(root.to_path_buf());
    for entry in std::fs::read_dir(root).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_dir() {
            directories(&entry.path(), out);
        }
    }
}

#[test]
fn pruned_walks_of_every_fixture_tree_are_lossless() {
    let mut rng = Rng(0xF1C7_2026);
    for (dir, doc) in layouts() {
        let fixtures = dir.join("fixtures");
        if !fixtures.is_dir() {
            continue;
        }
        // Every directory of the fixture tree stands in as a data root, so
        // the harvested root is covered without naming it here.
        let mut roots = Vec::new();
        directories(&fixtures, &mut roots);
        let subsets = subsets(&doc, &mut rng);
        for root in &roots {
            for accepted in &subsets {
                assert_lossless(&doc, root, accepted, 100_000);
            }
        }
    }
}

/// A concrete path the glob matches: every `*` filled, every `**` expanded
/// to zero, one or two segments. Segments are never empty.
fn instantiate(glob: &str, rng: &mut Rng) -> String {
    let fillers = ["x", "7", "é", "a.b", "-", "日本"];
    let mut segments = Vec::new();
    for segment in glob.split('/') {
        if segment == "**" {
            for _ in 0..rng.below(3) {
                segments.push(format!("d{}", rng.pick(&fillers)));
            }
            continue;
        }
        let pieces: Vec<&str> = segment.split('*').collect();
        let mut filled = pieces[0].to_owned();
        for piece in &pieces[1..] {
            filled.push_str(rng.pick(&fillers));
            filled.push_str(piece);
        }
        segments.push(filled);
    }
    segments.join("/")
}

#[test]
fn pruned_walks_of_synthetic_layout_trees_are_lossless() {
    let mut rng = Rng(0x57A7_E0927);
    for (_, doc) in layouts() {
        let temp = tempfile::TempDir::new().unwrap();
        let root = temp.path();
        // Real matches for every stream, near misses built from the same
        // segments, and noise directories no glob names.
        let mut vocabulary: Vec<String> = doc
            .streams
            .iter()
            .flat_map(|stream| stream.glob.split('/').map(str::to_owned))
            .filter(|segment| !segment.contains('*') && !segment.is_empty())
            .collect();
        vocabulary.extend(["plugins", "cache", "noise", ".hidden", "x.jsonl"].map(str::to_owned));
        let mut paths = Vec::new();
        for stream in &doc.streams {
            for _ in 0..4 {
                paths.push(instantiate(&stream.glob, &mut rng));
            }
        }
        for _ in 0..200 {
            let depth = 1 + rng.below(6);
            let path: Vec<&str> = (0..depth).map(|_| rng.pick(&vocabulary).as_str()).collect();
            paths.push(path.join("/"));
        }
        let mut created = 0;
        for relative in paths.iter().filter(|path| !path.is_empty()) {
            let path = root.join(relative);
            // A path that needs a directory where a file already is (or the
            // reverse) is simply not part of this tree.
            let made = path
                .parent()
                .is_some_and(|parent| std::fs::create_dir_all(parent).is_ok())
                && !path.is_dir()
                && std::fs::write(&path, b"x\n").is_ok();
            created += usize::from(made);
        }
        assert!(created > 50, "{}: synthetic tree too small", doc.agent);

        let accepted_all = subsets(&doc, &mut rng);
        let mut declined = 0;
        for accepted in &accepted_all {
            declined += assert_lossless(&doc, root, accepted, 100_000);
            // The cap counts accepted files only, in traversal order, which
            // pruning leaves intact: truncation lands on the same file.
            let accepted_files = walk_filtered(root, 100_000, |path| {
                doc.matching_stream(path)
                    .is_some_and(|stream| accepted.contains(&stream.id))
            })
            .unwrap()
            .files
            .len();
            if accepted_files > 1 {
                assert_lossless(&doc, root, accepted, accepted_files / 2);
            }
        }
        // Noise directories exist in every tree, so pruning had to act.
        assert!(declined > 0, "{}: nothing was pruned", doc.agent);
    }
}
