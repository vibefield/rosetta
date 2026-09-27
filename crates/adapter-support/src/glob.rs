//! The ground-truth layout's small glob language: `/` separates segments,
//! `*` matches within one segment, and a segment that is exactly `**` matches
//! zero or more whole segments.
//!
//! Each stream's glob is compiled once when its layout loads. Discovery tests
//! every walked file against every stream of its layout, and every live poll
//! walks the root again, so matching splits nothing and never allocates.

/// A layout glob split into segments once, with each `*` segment pre-split
/// into the literal pieces between its stars.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Glob {
    segments: Box<[Segment]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Segment {
    /// `**`: zero or more whole segments.
    AnyDepth,
    /// No `*`: compared exactly.
    Literal(Box<str>),
    /// At least one `*`: the pieces between the stars, so two or more.
    Wild(Box<[Box<str>]>),
}

impl Glob {
    pub fn new(pattern: &str) -> Self {
        Self {
            segments: pattern.split('/').map(Segment::parse).collect(),
        }
    }

    /// Whether the `/`-separated relative `path` matches.
    pub fn matches(&self, path: &str) -> bool {
        match_path(&self.segments, Some(path))
    }
}

impl Segment {
    fn parse(text: &str) -> Self {
        if text == "**" {
            return Self::AnyDepth;
        }
        let pieces: Box<[Box<str>]> = text.split('*').map(Box::from).collect();
        if pieces.len() == 1 {
            Self::Literal(text.into())
        } else {
            Self::Wild(pieces)
        }
    }

    /// One path segment against this pattern segment. `**` is resolved by
    /// the caller, which may also let it span several segments.
    fn matches(&self, segment: &str) -> bool {
        match self {
            Self::AnyDepth => true,
            Self::Literal(literal) => **literal == *segment,
            Self::Wild(pieces) => {
                let [first, middle @ .., last] = &pieces[..] else {
                    return false;
                };
                if !segment.starts_with(&**first) || !segment.ends_with(&**last) {
                    return false;
                }
                let mut cursor = first.len();
                for piece in middle {
                    let Some(offset) = segment[cursor..].find(&**piece) else {
                        return false;
                    };
                    cursor += offset + piece.len();
                }
                cursor <= segment.len() - last.len()
            }
        }
    }
}

/// The first segment of a remainder, and the remainder after it. `None`
/// means no segment is left; `Some("")` is one empty segment, exactly as
/// `str::split('/')` would yield it.
fn next_segment(path: &str) -> (&str, Option<&str>) {
    match path.split_once('/') {
        Some((segment, rest)) => (segment, Some(rest)),
        None => (path, None),
    }
}

fn match_path(pattern: &[Segment], path: Option<&str>) -> bool {
    match (pattern.split_first(), path) {
        (None, None) => true,
        (Some((Segment::AnyDepth, rest)), _) => {
            match_path(rest, path)
                || path.is_some_and(|path| match_path(pattern, next_segment(path).1))
        }
        (Some((head, rest)), Some(path)) => {
            let (segment, tail) = next_segment(path);
            head.matches(segment) && match_path(rest, tail)
        }
        (None, Some(_)) | (Some(_), None) => false,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::{Path, PathBuf};

    use super::Glob;
    use crate::GroundTruthDoc;

    /// The matcher every layout used before globs were compiled, kept
    /// verbatim as the equivalence oracle.
    mod reference {
        pub fn glob_match(pattern: &str, path: &str) -> bool {
            let pattern: Vec<&str> = pattern.split('/').collect();
            let path: Vec<&str> = path.split('/').collect();
            match_segments(&pattern, &path)
        }

        fn match_segments(pattern: &[&str], path: &[&str]) -> bool {
            match (pattern.first(), path.first()) {
                (None, None) => true,
                (Some(&"**"), _) => {
                    match_segments(&pattern[1..], path)
                        || (!path.is_empty() && match_segments(pattern, &path[1..]))
                }
                (Some(head), Some(segment)) => {
                    match_segment(head, segment) && match_segments(&pattern[1..], &path[1..])
                }
                _ => false,
            }
        }

        fn match_segment(pattern: &str, segment: &str) -> bool {
            let parts: Vec<&str> = pattern.split('*').collect();
            if parts.len() == 1 {
                return pattern == segment;
            }
            if !segment.starts_with(parts[0])
                || !segment.ends_with(parts.last().copied().unwrap_or(""))
            {
                return false;
            }

            let mut cursor = parts[0].len();
            let final_part = parts.len() - 1;
            for part in &parts[1..final_part] {
                let Some(offset) = segment[cursor..].find(part) else {
                    return false;
                };
                cursor += offset + part.len();
            }
            cursor <= segment.len() - parts[final_part].len()
        }
    }

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

    fn repo() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    /// Every embedded canonical ground truth, by agent directory.
    fn ground_truths() -> Vec<GroundTruthDoc> {
        let mut docs = Vec::new();
        for entry in std::fs::read_dir(repo().join("groundtruth")).unwrap() {
            let path = entry.unwrap().path().join("groundtruth.json");
            if path.is_file() {
                docs.push(GroundTruthDoc::parse(&std::fs::read_to_string(&path).unwrap()).unwrap());
            }
        }
        assert_eq!(
            docs.len(),
            9,
            "one canonical ground truth per captured agent"
        );
        docs
    }

    fn files_below(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let kind = entry.file_type().unwrap();
            if kind.is_dir() {
                files_below(&entry.path(), out);
            } else if kind.is_file() {
                out.push(entry.path());
            }
        }
    }

    /// Every fixture file in the repository, as every `/`-joined suffix of
    /// its path: one of them is the path relative to the harvested data root.
    fn fixture_paths() -> BTreeSet<String> {
        let groundtruth = repo().join("groundtruth");
        let mut files = Vec::new();
        for entry in std::fs::read_dir(&groundtruth).unwrap() {
            let fixtures = entry.unwrap().path().join("fixtures");
            if fixtures.is_dir() {
                files_below(&fixtures, &mut files);
            }
        }
        assert!(files.len() > 400, "the fixture corpus is present");
        let mut paths = BTreeSet::new();
        for file in files {
            let segments: Vec<String> = file
                .strip_prefix(&groundtruth)
                .unwrap()
                .components()
                .map(|component| component.as_os_str().to_string_lossy().into_owned())
                .collect();
            for start in 0..segments.len() {
                paths.insert(segments[start..].join("/"));
            }
        }
        paths
    }

    fn assert_equivalent(pattern: &str, path: &str) {
        assert_eq!(
            Glob::new(pattern).matches(path),
            reference::glob_match(pattern, path),
            "pattern {pattern:?} against {path:?}"
        );
    }

    fn assert_same_first_match(doc: &GroundTruthDoc, path: &str) {
        let reference = doc
            .streams
            .iter()
            .find(|stream| reference::glob_match(&stream.glob, path))
            .map(|stream| stream.id.as_str());
        assert_eq!(
            doc.matching_stream(path).map(|stream| stream.id.as_str()),
            reference,
            "{} first match for {path:?}",
            doc.agent
        );
    }

    #[test]
    fn layout_globs() {
        let glob_match = |pattern: &str, path: &str| Glob::new(pattern).matches(path);
        assert!(glob_match("projects/*/*.jsonl", "projects/p/a.jsonl"));
        assert!(!glob_match("projects/*/*.jsonl", "projects/a.jsonl"));
        assert!(glob_match(
            "projects/*/*/subagents/**/agent-*.jsonl",
            "projects/p/s/subagents/x/y/agent-7.jsonl"
        ));
        assert!(glob_match("**/*.jsonl", "a/b/c.jsonl"));
        assert!(glob_match("**/*.jsonl", "c.jsonl"));
        assert!(glob_match("todos/*-agent-*.json", "todos/1-agent-2.json"));
        assert!(!glob_match("agent-*-*.jsonl", "agent-.jsonl"));
    }

    #[test]
    fn compiled_globs_match_every_fixture_path_like_the_reference() {
        let docs = ground_truths();
        let paths = fixture_paths();
        for doc in &docs {
            for path in &paths {
                assert_same_first_match(doc, path);
                for stream in &doc.streams {
                    assert_equivalent(&stream.glob, path);
                }
            }
        }
    }

    #[test]
    fn compiled_globs_match_generated_paths_like_the_reference() {
        let docs = ground_truths();
        // Segments drawn from the layouts themselves, with every `*` filled,
        // so generated paths hit real matches and near misses alike.
        let fillers = ["", "x", "0", "ab", ".", "é", "日本", "a.b", "-", "*"];
        let mut rng = Rng(0x5EED_2026_0927);
        let mut vocabulary: BTreeSet<String> = BTreeSet::new();
        for doc in &docs {
            for stream in &doc.streams {
                for segment in stream.glob.split('/') {
                    vocabulary.insert(segment.to_owned());
                    for _ in 0..3 {
                        let pieces: Vec<&str> = segment.split('*').collect();
                        let mut filled = pieces[0].to_owned();
                        for piece in &pieces[1..] {
                            filled.push_str(rng.pick(&fillers));
                            filled.push_str(piece);
                        }
                        vocabulary.insert(filled);
                    }
                }
            }
        }
        vocabulary.extend(
            [
                "",
                ".",
                "..",
                ".hidden",
                "**",
                "***",
                "a*b",
                "é",
                "日本語",
                "🦀",
                "x.jsonl",
                "agent-",
                "agent-.jsonl",
                ".jsonl",
                "-",
                "_",
            ]
            .map(str::to_owned),
        );
        let vocabulary: Vec<String> = vocabulary.into_iter().collect();

        for _ in 0..3_000 {
            let depth = 1 + rng.below(8);
            let path: Vec<&str> = (0..depth).map(|_| rng.pick(&vocabulary).as_str()).collect();
            let path = path.join("/");
            for doc in &docs {
                assert_same_first_match(doc, &path);
            }
        }

        // Random patterns over the same alphabet exercise `*`/`**` shapes no
        // layout declares yet: leading, trailing and repeated stars, empty
        // pieces, `**` at either end or twice.
        let pattern_segments = [
            "*",
            "**",
            "***",
            "a*",
            "*b",
            "a*b",
            "*a*",
            "a**b",
            "*.jsonl",
            "agent-*.jsonl",
            "a*b*c",
            "",
            ".",
            "é*",
            "*日本",
            "x",
            "projects",
        ];
        let path_segments = [
            "",
            "a",
            "b",
            "ab",
            "abc",
            "acb",
            "aXbYc",
            "ba",
            ".",
            "..",
            "é",
            "éé",
            "日本",
            "x.jsonl",
            "agent-1.jsonl",
            "agent-.jsonl",
            "projects",
            "*",
            "**",
            "a*b",
        ];
        for _ in 0..20_000 {
            let pattern: Vec<&str> = (0..1 + rng.below(5))
                .map(|_| *rng.pick(&pattern_segments))
                .collect();
            let path: Vec<&str> = (0..1 + rng.below(6))
                .map(|_| *rng.pick(&path_segments))
                .collect();
            assert_equivalent(&pattern.join("/"), &path.join("/"));
        }
        for edge in ["", "/", "//", "a/", "/a", "a//b"] {
            for pattern in ["", "*", "**", "**/*", "*/**", "a/**", "**/a", "/", "a//b"] {
                assert_equivalent(pattern, edge);
            }
        }
    }
}
