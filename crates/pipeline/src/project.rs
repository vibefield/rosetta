use std::path::{Path, PathBuf};

use rosetta_core::ids;
use rosetta_core::rows::ProjectRow;

pub(crate) fn project(cwd: &str) -> ProjectRow {
    let cwd = ids::normalize_root(cwd);
    let (root, group_root, git_repo) = git_metadata(&cwd).unwrap_or((cwd, None, None));
    let identity_root = group_root.unwrap_or_else(|| root.clone());
    let mut root_paths = vec![root];
    if !root_paths.contains(&identity_root) {
        root_paths.push(identity_root.clone());
    }
    root_paths.sort();
    ProjectRow {
        id: ids::project_id(&identity_root, None),
        name: ids::project_name(&identity_root),
        root_paths,
        git_repo,
    }
}

pub(crate) fn project_from_hint(cwd: &str, group_root: &str, git_repo: Option<&str>) -> ProjectRow {
    let mut observed = project(cwd);
    let group_root = ids::normalize_root(group_root);
    if !observed.root_paths.contains(&group_root) {
        observed.root_paths.push(group_root.clone());
        observed.root_paths.sort();
        observed.root_paths.dedup();
    }
    observed.id = ids::project_id(&group_root, None);
    observed.name = ids::project_name(&group_root);
    if let Some(git_repo) = git_repo {
        observed.git_repo = Some(sanitize_remote(git_repo));
    }
    observed
}

/// Best-effort repository metadata without invoking git. The common Git
/// directory is the identity seam: it differs between ordinary clones and is
/// shared by linked worktrees. The sanitized remote remains display metadata.
fn git_metadata(cwd: &str) -> Option<(String, Option<String>, Option<String>)> {
    let (repo_root, git_dir) = find_git_dir(Path::new(cwd))?;
    let common_dir = common_git_dir(&git_dir)
        .map(|path| PathBuf::from(ids::normalize_root(&path.to_string_lossy())));
    let metadata_dir = common_dir.as_deref().unwrap_or(&git_dir);
    let config = std::fs::read_to_string(metadata_dir.join("config")).ok();
    let git_repo = config.as_deref().and_then(remote_url).map(sanitize_remote);
    let group_root = common_dir
        .as_deref()
        .map(|common_dir| {
            if common_dir.file_name().is_some_and(|name| name == ".git") {
                common_dir.parent().unwrap_or(common_dir)
            } else {
                common_dir
            }
        })
        .unwrap_or(&repo_root);
    Some((
        ids::normalize_root(&repo_root.to_string_lossy()),
        Some(ids::normalize_root(&group_root.to_string_lossy())),
        git_repo,
    ))
}

fn find_git_dir(cwd: &Path) -> Option<(PathBuf, PathBuf)> {
    for root in cwd.ancestors() {
        let dot_git = root.join(".git");
        if dot_git.is_dir() {
            return Some((root.to_path_buf(), dot_git));
        }
        let Ok(text) = std::fs::read_to_string(&dot_git) else {
            continue;
        };
        let target = text.trim().strip_prefix("gitdir:")?.trim();
        let git_dir = if Path::new(target).is_absolute() {
            PathBuf::from(target)
        } else {
            root.join(target)
        };
        return Some((root.to_path_buf(), git_dir));
    }
    None
}

fn common_git_dir(git_dir: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(git_dir.join("commondir")).ok()?;
    let common = Path::new(text.trim());
    Some(if common.is_absolute() {
        common.to_path_buf()
    } else {
        git_dir.join(common)
    })
}

fn remote_url(config: &str) -> Option<&str> {
    let mut in_origin = false;
    let mut first_remote = None;
    for raw in config.lines() {
        let line = raw.trim();
        if line.starts_with('[') {
            in_origin = line == "[remote \"origin\"]";
            continue;
        }
        let Some(value) = line
            .strip_prefix("url")
            .and_then(|rest| rest.split_once('='))
        else {
            continue;
        };
        let value = value.1.trim();
        if first_remote.is_none() {
            first_remote = Some(value);
        }
        if in_origin {
            return Some(value);
        }
    }
    first_remote
}

fn sanitize_remote(remote: &str) -> String {
    let mut remote = remote.trim().trim_end_matches('/').to_owned();
    if let Some(suffix) = remote.find(['?', '#']) {
        remote.truncate(suffix);
    }
    if let Some(scheme) = remote.find("://") {
        let authority = scheme + 3;
        let end = remote[authority..]
            .find('/')
            .map_or(remote.len(), |offset| authority + offset);
        if let Some(at) = remote[authority..end].rfind('@') {
            remote.replace_range(authority..authority + at + 1, "");
        }
    } else if let Some(at) = remote.find('@') {
        // SCP-like git remotes (`user@host:path`) have no URL authority but
        // the userinfo is still irrelevant to identity and may be sensitive.
        remote.replace_range(..=at, "");
    }
    if remote.ends_with(".git") {
        remote.truncate(remote.len() - 4);
    }
    remote
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn project_finds_and_sanitizes_origin() {
        let root = TempDir::new().unwrap();
        std::fs::create_dir(root.path().join(".git")).unwrap();
        std::fs::write(
            root.path().join(".git/config"),
            "[remote \"origin\"]\n  url = https://token@example.com/acme/repo.git\n",
        )
        .unwrap();
        let row = project(&root.path().join("nested/..").to_string_lossy());
        assert_eq!(
            row.git_repo.as_deref(),
            Some("https://example.com/acme/repo")
        );
        assert_eq!(
            row.root_paths,
            vec![ids::normalize_root(&root.path().to_string_lossy())]
        );
    }

    #[test]
    fn remote_secrets_are_not_part_of_project_identity() {
        assert_eq!(
            sanitize_remote("ssh://user:secret@example.com/acme/repo.git?token=nope"),
            "ssh://example.com/acme/repo"
        );
        assert_eq!(
            sanitize_remote("git@example.com:acme/repo.git"),
            "example.com:acme/repo"
        );
    }

    #[test]
    fn nested_cwd_maps_to_repository_root() {
        let root = TempDir::new().unwrap();
        std::fs::create_dir(root.path().join(".git")).unwrap();
        std::fs::write(
            root.path().join(".git/config"),
            "[remote \"origin\"]\n  url = git@example.com:acme/repo.git\n",
        )
        .unwrap();
        let nested = root.path().join("one/two");
        std::fs::create_dir_all(&nested).unwrap();
        let row = project(&nested.to_string_lossy());
        assert_eq!(row.name, ids::project_name(&root.path().to_string_lossy()));
        assert_eq!(
            row.root_paths,
            vec![ids::normalize_root(&root.path().to_string_lossy())]
        );
        assert_eq!(row.git_repo.as_deref(), Some("example.com:acme/repo"));
    }

    #[test]
    fn ordinary_clones_with_the_same_remote_are_distinct_projects() {
        let temp = TempDir::new().unwrap();
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        for root in [&first, &second] {
            std::fs::create_dir_all(root.join(".git")).unwrap();
            std::fs::write(
                root.join(".git/config"),
                "[remote \"origin\"]\n  url = git@example.com:acme/repo.git\n",
            )
            .unwrap();
        }

        let first = project(&first.to_string_lossy());
        let second = project(&second.to_string_lossy());
        assert_ne!(first.id, second.id);
        assert_eq!(first.git_repo, second.git_repo);
    }

    #[test]
    fn linked_worktrees_share_a_project_without_a_remote() {
        let temp = TempDir::new().unwrap();
        let main = temp.path().join("main");
        let linked = temp.path().join("linked");
        let linked_git_dir = main.join(".git/worktrees/linked");
        std::fs::create_dir_all(&linked_git_dir).unwrap();
        std::fs::create_dir_all(&linked).unwrap();
        std::fs::write(main.join(".git/config"), "[core]\n  bare = false\n").unwrap();
        std::fs::write(linked_git_dir.join("commondir"), "../..\n").unwrap();
        std::fs::write(
            linked.join(".git"),
            format!("gitdir: {}\n", linked_git_dir.display()),
        )
        .unwrap();

        let main_project = project(&main.to_string_lossy());
        let linked_project = project(&linked.to_string_lossy());
        assert_eq!(main_project.id, linked_project.id);
        assert_eq!(
            linked_project.name,
            ids::project_name(&main.to_string_lossy())
        );
        assert!(
            linked_project
                .root_paths
                .contains(&ids::normalize_root(&main.to_string_lossy()))
        );
        assert!(
            linked_project
                .root_paths
                .contains(&ids::normalize_root(&linked.to_string_lossy()))
        );
        assert_eq!(main_project.git_repo, None);
        assert_eq!(linked_project.git_repo, None);
    }

    #[test]
    fn gitfile_without_a_common_dir_keeps_the_checkout_folder_identity() {
        let temp = TempDir::new().unwrap();
        let parent = temp.path().join("parent");
        let child = parent.join("child");
        let child_git_dir = parent.join(".git/modules/child");
        std::fs::create_dir_all(&child_git_dir).unwrap();
        std::fs::create_dir_all(&child).unwrap();
        std::fs::write(
            child.join(".git"),
            format!("gitdir: {}\n", child_git_dir.display()),
        )
        .unwrap();

        let project = project(&child.to_string_lossy());

        assert_eq!(project.name, "child");
        assert_eq!(
            project.root_paths,
            vec![ids::normalize_root(&child.to_string_lossy())]
        );
    }
}
