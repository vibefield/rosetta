//! Root trees: the admission unit of a [`ColdAdmission::RootTree`] runtime.
//!
//! A tree is one connected component of a runtime's bounded metadata links,
//! found by union-find over every session the roster or a `SessionMeta` row
//! names. The links are a session's parent, its root when that differs from
//! itself, and a ledger-only source's [`SessionFile::tree_parent`]. Union
//! alone, not "group by root", is what keeps a family whole: a Codex child's
//! `root_session_id` is its immediate parent, so a grandchild's root is the
//! child, and grouping by the raw root would split one family into two. A
//! guardian review admits no session row at all, so only its source carries
//! its link.
//!
//! The result is a pure function of the set of nodes and links: components
//! do not depend on union order, a tree is named by its smallest session id,
//! and tree ids number the trees in that name's order. Two builds over the
//! same roster and metadata, in any order, are equal.
//!
//! Everything here is RAM only and rebuilt from the metadata plan; nothing is
//! persisted (R1 invariant I3).
//!
//! [`ColdAdmission::RootTree`]: rosetta_adapter_support::ColdAdmission::RootTree

use std::collections::{HashMap, HashSet};

use rosetta_adapter_support::StreamDriver;

use crate::meta::SessionFile;

/// A tree's position in [`TreeMap::trees`]: trees ascend by [`Tree::id`].
pub type TreeId = u32;

/// One session's metadata links, as its `SessionMeta` row states them:
/// `(id, parent_session_id, root_session_id)`. A root equal to the id is no
/// link.
pub type SessionLink = (String, Option<String>, Option<String>);

/// One root tree and the roster facts its admission is planned by.
///
/// A tree's own sources are its JSONL transcripts plus the members of any
/// physical document or snapshot file read only by this tree. Members of a
/// physical file shared by several trees (a Codex `state_N.sqlite`) belong to
/// no tree's facts: the walk reads them before every tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tree {
    /// The smallest member session id: the tree's name and tie-break.
    pub id: String,
    /// Recency key: the newest modification over the tree's own sources, the
    /// skim's cut time where there is one ([`SessionFile::cut_modified_ms`])
    /// and the census time otherwise. A tree with only shared members takes
    /// their census times (a snapshot member's is its session's own update);
    /// a tree with no roster source at all is `i64::MIN`.
    pub k: i64,
    /// Bytes of the tree's own sources: a transcript's cut length (else its
    /// census size), and a single-tree physical file's size once.
    pub bytes: u64,
    /// The largest single own source, measured as for `bytes`.
    pub max_source_bytes: u64,
    /// Every member, ascending: public sessions, ledger-only sources'
    /// sessions, stubs (roster sessions no metadata row named), metadata-only
    /// placeholders and any session a link names. Only a member with a
    /// public roster source is ever admitted; a placeholder joins its tree
    /// but never flips.
    pub sessions: Vec<String>,
    /// Roster sources whose session is a member, shared members included.
    pub roster_files: u32,
}

/// The root trees of one runtime's roster and metadata.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TreeMap {
    tree_of: HashMap<String, TreeId>,
    trees: Vec<Tree>,
}

impl TreeMap {
    /// Union every session `files` and `links` name along their links, then
    /// measure each component's roster facts. A session with no link (a
    /// stub, say) is a tree of its own.
    pub fn build(files: &[SessionFile], links: &[SessionLink]) -> Self {
        let mut sets = Sets::default();
        for file in files {
            let session = sets.node(&file.session_id);
            if let Some(parent) = file.tree_parent.as_deref() {
                let parent = sets.node(parent);
                sets.union(session, parent);
            }
        }
        for (id, parent, root) in links {
            let session = sets.node(id);
            for target in [parent, root].into_iter().flatten() {
                if target != id {
                    let target = sets.node(target);
                    sets.union(session, target);
                }
            }
        }

        let mut components: HashMap<usize, Vec<String>> = HashMap::new();
        for index in 0..sets.names.len() {
            let root = sets.find(index);
            components
                .entry(root)
                .or_default()
                .push(sets.names[index].to_owned());
        }
        let mut members: Vec<Vec<String>> = components
            .into_values()
            .map(|mut sessions| {
                sessions.sort_unstable();
                sessions
            })
            .collect();
        members.sort_unstable_by(|left, right| left[0].cmp(&right[0]));

        let mut tree_of = HashMap::with_capacity(sets.names.len());
        let mut trees = Vec::with_capacity(members.len());
        for (position, sessions) in members.into_iter().enumerate() {
            let id = TreeId::try_from(position).expect("a roster holds fewer than 2^32 trees");
            for session in &sessions {
                tree_of.insert(session.clone(), id);
            }
            trees.push(Tree {
                id: sessions[0].clone(),
                k: i64::MIN,
                bytes: 0,
                max_source_bytes: 0,
                sessions,
                roster_files: 0,
            });
        }
        let mut map = Self { tree_of, trees };
        map.measure(files);
        map
    }

    /// The tree a session belongs to; `None` for a session neither the
    /// roster nor the metadata named.
    pub fn tree_of(&self, session: &str) -> Option<TreeId> {
        self.tree_of.get(session).copied()
    }

    /// The tree a roster source belongs to: its session's. A ledger-only
    /// source's session is joined to its `tree_parent`, so this is that
    /// parent's tree.
    pub fn tree_of_file(&self, file: &SessionFile) -> Option<TreeId> {
        self.tree_of(&file.session_id)
    }

    pub fn tree(&self, id: TreeId) -> &Tree {
        &self.trees[id as usize]
    }

    /// Every tree, ascending by [`Tree::id`].
    pub fn trees(&self) -> &[Tree] {
        &self.trees
    }

    /// Fill each tree's roster facts from `files`. Members of a physical
    /// file several trees read are shared: they count as roster sources but
    /// stand in for recency only where a tree has nothing of its own.
    fn measure(&mut self, files: &[SessionFile]) {
        // Which trees read each physical document or snapshot file.
        let mut readers: HashMap<&str, Readers> = HashMap::new();
        for file in files.iter().filter(|file| is_member(file)) {
            let tree = self
                .tree_of_file(file)
                .expect("every roster session is a node");
            readers
                .entry(file.physical_path())
                .and_modify(|seen| {
                    if *seen != Readers::One(tree) {
                        *seen = Readers::Shared;
                    }
                })
                .or_insert(Readers::One(tree));
        }
        // Per tree: whether it has a source of its own, and the newest census
        // time among its shared members.
        let mut own = vec![false; self.trees.len()];
        let mut shared_k = vec![i64::MIN; self.trees.len()];
        // A single-tree physical file's bytes count once, whatever its members.
        let mut counted: HashSet<&str> = HashSet::new();
        for file in files {
            let id = self
                .tree_of_file(file)
                .expect("every roster session is a node") as usize;
            let tree = &mut self.trees[id];
            tree.roster_files = tree.roster_files.saturating_add(1);
            if !is_member(file) {
                let bytes = file.cut_len.unwrap_or(file.size);
                tree.bytes = tree.bytes.saturating_add(bytes);
                tree.max_source_bytes = tree.max_source_bytes.max(bytes);
                tree.k = tree.k.max(file.cut_modified_ms.unwrap_or(file.modified_ms));
                own[id] = true;
                continue;
            }
            let physical = file.physical_path();
            if readers.get(physical) == Some(&Readers::Shared) {
                shared_k[id] = shared_k[id].max(file.modified_ms);
                continue;
            }
            tree.k = tree.k.max(file.modified_ms);
            tree.max_source_bytes = tree.max_source_bytes.max(file.size);
            if counted.insert(physical) {
                tree.bytes = tree.bytes.saturating_add(file.size);
            }
            own[id] = true;
        }
        for ((tree, own), shared_k) in self.trees.iter_mut().zip(own).zip(shared_k) {
            if !own {
                tree.k = shared_k;
            }
        }
    }
}

impl SessionFile {
    /// The physical file this source reads. A snapshot database or document
    /// member is a virtual source (`<path>#<native-id>`) of one shared file;
    /// a transcript is its own file.
    pub fn physical_path(&self) -> &str {
        if !is_member(self) {
            return &self.relative_path;
        }
        self.relative_path
            .strip_suffix(self.native_session_id.as_str())
            .and_then(|path| path.strip_suffix('#'))
            .unwrap_or(&self.relative_path)
    }
}

/// A virtual member of a physical document or snapshot file, as opposed to a
/// transcript, which is its own file.
fn is_member(file: &SessionFile) -> bool {
    file.driver != StreamDriver::JsonLines
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Readers {
    One(TreeId),
    Shared,
}

/// Disjoint sets over interned session ids, with path halving.
#[derive(Default)]
struct Sets<'a> {
    index: HashMap<&'a str, usize>,
    names: Vec<&'a str>,
    parent: Vec<usize>,
}

impl<'a> Sets<'a> {
    fn node(&mut self, name: &'a str) -> usize {
        if let Some(&index) = self.index.get(name) {
            return index;
        }
        let index = self.names.len();
        self.index.insert(name, index);
        self.names.push(name);
        self.parent.push(index);
        index
    }

    fn find(&mut self, mut index: usize) -> usize {
        while self.parent[index] != index {
            self.parent[index] = self.parent[self.parent[index]];
            index = self.parent[index];
        }
        index
    }

    fn union(&mut self, left: usize, right: usize) {
        let (left, right) = (self.find(left), self.find(right));
        if left != right {
            self.parent[left.max(right)] = left.min(right);
        }
    }
}

#[cfg(test)]
pub(crate) mod harness;
#[cfg(test)]
mod tests;
