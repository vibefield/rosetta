//! Tree identity over hand-built rosters: which sessions form one tree, what
//! names it, what its facts are, and that none of it depends on input order.

use rosetta_adapter_support::{FileDiscipline, FileFormat, StreamDriver};

use super::*;

/// A transcript of `session` with census size and time, and no cut.
fn transcript(path: &str, session: &str, size: u64, modified_ms: i64) -> SessionFile {
    SessionFile {
        input: None,
        ledger_only: false,
        relative_path: path.to_owned(),
        stream_id: "transcript".to_owned(),
        file_format: FileFormat::Jsonl,
        discipline: FileDiscipline::AppendOnly,
        driver: StreamDriver::JsonLines,
        session_id: session.to_owned(),
        native_session_id: session.to_owned(),
        size,
        modified_ms,
        agent_version: None,
        source_missing: false,
        tree_parent: None,
        cut_len: None,
        cut_modified_ms: None,
    }
}

/// A transcript as the skim leaves it: census facts plus the skim's cut.
fn skimmed(path: &str, session: &str, census: (u64, i64), cut: (u64, i64)) -> SessionFile {
    SessionFile {
        input: None,
        cut_len: Some(cut.0),
        cut_modified_ms: Some(cut.1),
        ..transcript(path, session, census.0, census.1)
    }
}

/// A ledger-only internal source whose metadata names `parent`.
fn guardian(path: &str, session: &str, parent: &str) -> SessionFile {
    SessionFile {
        input: None,
        ledger_only: true,
        tree_parent: Some(parent.to_owned()),
        ..transcript(path, session, 1, 1)
    }
}

/// A virtual member `<physical>#<session>` of a snapshot database.
fn member(physical: &str, session: &str, size: u64, modified_ms: i64) -> SessionFile {
    SessionFile {
        input: None,
        stream_id: "state-db".to_owned(),
        file_format: FileFormat::Sqlite,
        discipline: FileDiscipline::Database,
        driver: StreamDriver::SqliteSnapshot,
        ..transcript(&format!("{physical}#{session}"), session, size, modified_ms)
    }
}

/// A virtual member `<physical>#<session>` of a sidecar document.
fn document(physical: &str, session: &str, size: u64, modified_ms: i64) -> SessionFile {
    SessionFile {
        input: None,
        stream_id: "subagent-metadata".to_owned(),
        file_format: FileFormat::Json,
        discipline: FileDiscipline::RewriteInPlace,
        driver: StreamDriver::DocumentSnapshot,
        ..member(physical, session, size, modified_ms)
    }
}

fn link(id: &str, parent: Option<&str>, root: &str) -> SessionLink {
    (
        id.to_owned(),
        parent.map(str::to_owned),
        Some(root.to_owned()),
    )
}

/// Each tree's members, in tree order.
fn groups(map: &TreeMap) -> Vec<Vec<&str>> {
    map.trees()
        .iter()
        .map(|tree| tree.sessions.iter().map(String::as_str).collect())
        .collect()
}

/// Deterministic Fisher-Yates over a xorshift stream.
fn shuffle<T>(items: &mut [T], seed: u64) {
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    for at in (1..items.len()).rev() {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        items.swap(at, (state % (at as u64 + 1)) as usize);
    }
}

#[test]
fn a_codex_family_with_immediate_parent_roots_is_one_tree() {
    // Codex states each child's immediate parent as its root, so grouping by
    // root would split this family into {r, c} and {c, g}.
    let files = [
        transcript("r.jsonl", "r", 10, 1),
        transcript("c.jsonl", "c", 10, 2),
        transcript("g.jsonl", "g", 10, 3),
    ];
    let links = [
        link("r", None, "r"),
        link("c", Some("r"), "r"),
        link("g", Some("c"), "c"),
    ];
    let map = TreeMap::build(&files, &links);
    assert_eq!(groups(&map), [["c", "g", "r"]]);
    let tree = map.tree(0);
    assert_eq!(tree.id, "c", "a tree is named by its smallest session id");
    assert_eq!((tree.roster_files, tree.bytes, tree.k), (3, 30, 3));
    for session in ["r", "c", "g"] {
        assert_eq!(map.tree_of(session), Some(0));
    }
    assert_eq!(map.tree_of("unknown"), None);
}

#[test]
fn a_ledger_only_guardian_joins_the_tree_of_the_session_it_reviews() {
    let files = [
        transcript("r.jsonl", "r", 10, 1),
        transcript("c.jsonl", "c", 10, 2),
        transcript("g.jsonl", "g", 10, 3),
        // No row links the guardian: only its source names the grandchild.
        guardian("x.jsonl", "x", "g"),
        transcript("u.jsonl", "u", 10, 4),
    ];
    let links = [
        link("r", None, "r"),
        link("c", Some("r"), "r"),
        link("g", Some("c"), "c"),
        link("u", None, "u"),
    ];
    let map = TreeMap::build(&files, &links);
    assert_eq!(groups(&map), [vec!["c", "g", "r", "x"], vec!["u"]]);
    assert_eq!(map.tree_of_file(&files[3]), map.tree_of("g"));
    assert_eq!(map.tree(0).roster_files, 4);

    // A guardian whose parent has neither a source nor a row still keeps
    // its usage target in its own tree.
    let map = TreeMap::build(&[guardian("x.jsonl", "x", "gone")], &[]);
    assert_eq!(groups(&map), [["gone", "x"]]);
}

#[test]
fn a_claude_nested_subagent_joins_its_parent_agent_and_root() {
    // A sidecar's parentAgentId makes a subagent the parent of another,
    // while both keep the main session as their root.
    let files = [
        transcript("p/m.jsonl", "m", 10, 1),
        transcript("p/m/subagents/agent-a.jsonl", "a", 10, 2),
        document("p/m/subagents/agent-a.meta.json", "a", 1, 2),
        transcript("p/m/subagents/agent-c.jsonl", "c", 10, 3),
        document("p/m/subagents/agent-c.meta.json", "c", 1, 5),
        transcript("p/n.jsonl", "n", 10, 4),
    ];
    let links = [
        link("m", None, "m"),
        link("a", Some("m"), "m"),
        link("c", Some("a"), "m"),
        link("n", None, "n"),
    ];
    let map = TreeMap::build(&files, &links);
    assert_eq!(groups(&map), [vec!["a", "c", "m"], vec!["n"]]);
    let tree = map.tree(0);
    assert_eq!(tree.roster_files, 5);
    // Single-tree sidecars are the tree's own sources: they count in its
    // bytes and its recency (the sidecar of `c` is the newest file).
    assert_eq!((tree.bytes, tree.max_source_bytes, tree.k), (32, 10, 5));
}

#[test]
fn stubs_are_singletons_and_placeholders_join_their_tree() {
    let files = [
        transcript("s.jsonl", "stub", 10, 1),
        transcript("c.jsonl", "child", 10, 2),
    ];
    // `parent` is a metadata-only placeholder: a row but no source.
    let links = [
        link("parent", None, "parent"),
        link("child", Some("parent"), "parent"),
    ];
    let map = TreeMap::build(&files, &links);
    assert_eq!(groups(&map), [vec!["child", "parent"], vec!["stub"]]);
    assert_eq!(map.tree(0).roster_files, 1);
    assert_eq!(map.tree(1).roster_files, 1);

    // A tree of placeholders only has no roster source and no recency.
    let map = TreeMap::build(&[], &[link("lone", None, "lone")]);
    let tree = map.tree(0);
    assert_eq!((tree.roster_files, tree.bytes, tree.k), (0, 0, i64::MIN));
}

#[test]
fn recency_and_size_come_from_the_skim_cut_where_there_is_one() {
    let files = [
        // Census says old and small; the skim saw it grown and touched.
        skimmed("a.jsonl", "a", (10, 1_000), (90, 5_000)),
        transcript("b.jsonl", "a", 20, 2_000),
    ];
    let map = TreeMap::build(&files, &[]);
    let tree = map.tree(0);
    assert_eq!(
        (tree.k, tree.bytes, tree.max_source_bytes),
        (5_000, 110, 90)
    );
}

#[test]
fn a_physical_file_shared_by_several_trees_is_no_tree_s_own() {
    let db = "state_5.sqlite";
    let files = [
        transcript("r.jsonl", "r", 10, 100),
        member(db, "r", 1_000, 900),
        // `s` has nothing but its row in the shared database.
        member(db, "s", 1_000, 700),
    ];
    let map = TreeMap::build(&files, &[]);
    assert_eq!(groups(&map), [["r"], ["s"]]);
    let (r, s) = (map.tree(0), map.tree(1));
    // The database's bytes and time are no tree's: `r` keeps its own.
    assert_eq!(
        (r.k, r.bytes, r.max_source_bytes, r.roster_files),
        (100, 10, 10, 2)
    );
    // A tree with only shared members is ranked by their census times.
    assert_eq!(
        (s.k, s.bytes, s.max_source_bytes, s.roster_files),
        (700, 0, 0, 1)
    );

    // The same database read by one tree only is that tree's.
    let map = TreeMap::build(&files[..2], &[]);
    let r = map.tree(0);
    assert_eq!((r.k, r.bytes, r.max_source_bytes), (900, 1_010, 1_000));
}

#[test]
fn trees_are_numbered_by_name_whatever_the_input_order() {
    let mut files = vec![
        transcript("z.jsonl", "z", 1, 1),
        transcript("m.jsonl", "m", 1, 1),
        transcript("a.jsonl", "a", 1, 1),
    ];
    let reference = TreeMap::build(&files, &[]);
    assert_eq!(groups(&reference), [["a"], ["m"], ["z"]]);
    files.reverse();
    assert_eq!(TreeMap::build(&files, &[]), reference);
}

#[test]
fn the_map_is_independent_of_union_and_input_order() {
    let db = "state_5.sqlite";
    let mut files = vec![
        skimmed("r.jsonl", "r", (10, 1), (12, 9)),
        transcript("c.jsonl", "c", 10, 2),
        transcript("g.jsonl", "g", 10, 3),
        guardian("x.jsonl", "x", "g"),
        transcript("u.jsonl", "u", 10, 4),
        transcript("v.jsonl", "v", 10, 4),
        member(db, "r", 50, 5),
        member(db, "u", 50, 6),
        document("w.json", "w", 7, 7),
        transcript("stub.jsonl", "stub", 3, 3),
    ];
    let mut links = vec![
        link("r", None, "r"),
        link("c", Some("r"), "r"),
        link("g", Some("c"), "c"),
        link("u", None, "u"),
        link("v", Some("w"), "u"),
        link("w", None, "w"),
        link("placeholder", None, "placeholder"),
        link("orphan", Some("placeholder"), "placeholder"),
    ];
    let reference = TreeMap::build(&files, &links);
    assert_eq!(
        groups(&reference),
        [
            vec!["c", "g", "r", "x"],
            vec!["orphan", "placeholder"],
            vec!["stub"],
            vec!["u", "v", "w"],
        ]
    );
    for seed in 0..64 {
        shuffle(&mut files, seed);
        shuffle(&mut links, seed.wrapping_add(1_000));
        assert_eq!(TreeMap::build(&files, &links), reference, "seed {seed}");
    }
    files.reverse();
    links.reverse();
    assert_eq!(TreeMap::build(&files, &links), reference, "reversed");
    // A link stated from either end is the same link.
    let flipped: Vec<SessionLink> = vec![
        link("r", None, "r"),
        ("r".to_owned(), Some("c".to_owned()), Some("r".to_owned())),
        link("g", Some("c"), "g"),
        link("u", None, "u"),
        ("w".to_owned(), Some("v".to_owned()), Some("u".to_owned())),
        link("placeholder", Some("orphan"), "placeholder"),
    ];
    assert_eq!(
        groups(&TreeMap::build(&files, &flipped)),
        groups(&reference)
    );
}
