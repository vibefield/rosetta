//! Deterministic ids — blake3 over domain-separated stable inputs, hex.
//! Same input → same id, always: replay is idempotent and dedupe is trivial.

fn derive(domain: &str, parts: &[&str]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(domain.as_bytes());
    hasher.update(&[0]);
    for part in parts {
        hasher.update(part.as_bytes());
        hasher.update(&[0]);
    }
    hasher.finalize().to_hex().to_string()
}

/// One installed agent: kind + data root.
pub fn runtime_id(kind: &str, data_root: &str) -> String {
    derive("rosetta/runtime", &[kind, data_root])
}

/// A short filesystem-safe discriminator for a supervisor-enrolled data root:
/// the first 12 hex characters of blake3 over the absolute root. It names the
/// runtime's ledger directory beside its kind (`<kind>~<digest>`); identity
/// itself is the stamp inside that directory, never the name.
pub fn root_digest(data_root: &str) -> String {
    blake3::hash(data_root.as_bytes()).to_hex()[..12].to_string()
}

/// Rosetta's own project abstraction. `worktree_group` is a local folder path
/// shared only by linked worktrees; ordinary clones use their own root. Both
/// routes intentionally share one id domain so a deleted source checkout and
/// a surviving native ownership hint still converge on the same project.
pub fn project_id(normalized_root: &str, worktree_group: Option<&str>) -> String {
    let identity = worktree_group
        .filter(|identity| !identity.is_empty())
        .unwrap_or(normalized_root);
    derive("rosetta/project/root", &[identity])
}

/// Lexically normalize an agent-reported workspace path without requiring it
/// to still exist. Both meta and stream phases use this exact transform so a
/// cwd transition cannot mint a second spelling of the same project.
pub fn normalize_root(path: &str) -> String {
    let portable = path.replace('\\', "/");
    let absolute = portable.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for part in portable.split('/') {
        match part {
            "" | "." => {}
            ".." if parts.last().is_some_and(|last| *last != "..") => {
                parts.pop();
            }
            ".." if !absolute => parts.push(part),
            ".." => {}
            _ => parts.push(part),
        }
    }
    let joined = parts.join("/");
    if absolute {
        format!("/{joined}")
    } else {
        joined
    }
}

pub fn project_name(normalized_root: &str) -> String {
    normalized_root
        .rsplit('/')
        .find(|segment| !segment.is_empty())
        .unwrap_or(normalized_root)
        .to_string()
}

pub fn session_id(runtime_id: &str, native_session_id: &str) -> String {
    derive("rosetta/session", &[runtime_id, native_session_id])
}

/// Stable timeline row identity. `discriminator` is the adapter's stable name
/// for the row within the session — a native uuid, a `uuid#block-index` pair
/// for split blocks, or a line hash where the agent provides no id.
pub fn timeline_id(session_id: &str, discriminator: &str) -> String {
    derive("rosetta/timeline", &[session_id, discriminator])
}

pub fn task_id(session_id: &str, native_key: &str) -> String {
    derive("rosetta/task", &[session_id, native_key])
}

pub fn usage_id(session_id: &str, native_key: &str) -> String {
    derive("rosetta/usage", &[session_id, native_key])
}

/// Shape hash for drift aggregation: order-insensitive over field paths.
pub fn shape_hash(paths: &[String]) -> String {
    let mut sorted: Vec<&str> = paths.iter().map(String::as_str).collect();
    sorted.sort_unstable();
    sorted.dedup();
    derive("rosetta/shape", &sorted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_and_domain_separated() {
        assert_eq!(session_id("r", "s"), session_id("r", "s"));
        assert_ne!(session_id("r", "s"), timeline_id("r", "s"));
        assert_ne!(task_id("r", "s"), usage_id("r", "s"));
        assert_ne!(session_id("r", "s"), session_id("r", "s2"));
        // Separator injection cannot collide: ("ab","c") != ("a","bc").
        assert_ne!(session_id("ab", "c"), session_id("a", "bc"));
    }

    #[test]
    fn roots_normalize_without_filesystem_access() {
        assert_eq!(normalize_root("/w/./project/../rosetta/"), "/w/rosetta");
        assert_eq!(normalize_root(r"C:\\w\\rosetta"), "C:/w/rosetta");
    }

    #[test]
    fn git_common_dir_merges_only_linked_worktrees() {
        assert_eq!(
            project_id("/work/main", None),
            project_id("/work/linked", Some("/work/main"))
        );
        assert_ne!(project_id("/work/one", None), project_id("/work/two", None));
    }

    #[test]
    fn shape_hash_is_order_insensitive() {
        let a = shape_hash(&["x.y".into(), "a.b".into()]);
        let b = shape_hash(&["a.b".into(), "x.y".into(), "a.b".into()]);
        assert_eq!(a, b);
    }
}
