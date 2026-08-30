use std::path::{Path, PathBuf};

use rosetta_core::IngestOp;
use rosetta_core::ids;
use rosetta_core::rows::ProjectRow;

pub(crate) fn project(cwd: &str) -> ProjectRow {
    let root = ids::normalize_root(cwd);
    let git_repo = git_identity(&root);
    ProjectRow {
        id: ids::project_id(&root, git_repo.as_deref()),
        name: ids::project_name(&root),
        root_paths: vec![root],
        git_repo,
    }
}

/// Apply only deterministic, replayable workspace normalization. Local Git
/// discovery belongs to the meta capture (whose resulting ProjectRow is
/// written into the raw ledger); it must not silently enrich a streamed row
/// because that filesystem state is absent during ledger-only rebuild.
pub(crate) fn normalize_workspace_ops(ops: &mut [IngestOp]) {
    for op in ops {
        let IngestOp::WorkspaceObs(row) = op else {
            continue;
        };
        row.cwd = ids::normalize_root(&row.cwd);
        // A native adapter may report a shorthand or a credential-bearing
        // remote. Sanitize what the record actually carried, but never consult
        // today's local checkout while projecting a historical raw record.
        row.git_repo = row.git_repo.take().map(|value| sanitize_remote(&value));
    }
}

/// Best-effort repository identity without invoking git. Prefer a sanitized
/// remote URL; a repository root is still useful when no remote is configured.
fn git_identity(cwd: &str) -> Option<String> {
    let (repo_root, git_dir) = find_git_dir(Path::new(cwd))?;
    let common_dir = common_git_dir(&git_dir).unwrap_or(git_dir);
    let config = std::fs::read_to_string(common_dir.join("config")).ok();
    config
        .as_deref()
        .and_then(remote_url)
        .map(sanitize_remote)
        .or_else(|| Some(ids::normalize_root(&repo_root.to_string_lossy())))
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
    fn workspace_observation_uses_only_replayable_native_identity() {
        let root = TempDir::new().unwrap();
        std::fs::create_dir(root.path().join(".git")).unwrap();
        std::fs::write(
            root.path().join(".git/config"),
            "[remote \"origin\"]\n  url = git@example.com:acme/repo.git\n",
        )
        .unwrap();
        let mut ops = vec![IngestOp::WorkspaceObs(
            rosetta_core::rows::WorkspaceObsRow {
                session_id: "s".to_owned(),
                at: 0,
                cwd: root.path().to_string_lossy().into_owned(),
                git_branch: None,
                git_repo: Some("acme/repo".to_owned()),
            },
        )];
        normalize_workspace_ops(&mut ops);
        let IngestOp::WorkspaceObs(row) = &ops[0] else {
            unreachable!()
        };
        assert_eq!(row.git_repo.as_deref(), Some("acme/repo"));
    }

    #[test]
    fn workspace_observation_never_invents_identity_from_today_checkout() {
        let root = TempDir::new().unwrap();
        std::fs::create_dir(root.path().join(".git")).unwrap();
        std::fs::write(
            root.path().join(".git/config"),
            "[remote \"origin\"]\n  url = git@example.com:acme/repo.git\n",
        )
        .unwrap();
        let mut ops = vec![IngestOp::WorkspaceObs(
            rosetta_core::rows::WorkspaceObsRow {
                session_id: "s".to_owned(),
                at: 0,
                cwd: root.path().to_string_lossy().into_owned(),
                git_branch: None,
                git_repo: None,
            },
        )];

        normalize_workspace_ops(&mut ops);

        let IngestOp::WorkspaceObs(row) = &ops[0] else {
            unreachable!()
        };
        assert_eq!(row.cwd, ids::normalize_root(&root.path().to_string_lossy()));
        assert_eq!(row.git_repo, None);
    }
}
