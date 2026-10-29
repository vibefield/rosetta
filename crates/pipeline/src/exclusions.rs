//! Start-time folder policy. Match recorded cwd spellings without touching
//! the filesystem: historical workspaces need not exist on this machine.

use std::collections::BTreeSet;
use std::path::Path;

use rosetta_core::ids;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CwdExclusions {
    roots: BTreeSet<String>,
}

impl CwdExclusions {
    /// Validate absolute folders and normalize separators, dots and trailing
    /// slashes using the same lexical rules as canonical session cwd.
    pub fn new(paths: impl IntoIterator<Item = String>) -> Result<Self, String> {
        let mut roots = BTreeSet::new();
        for (index, path) in paths.into_iter().enumerate() {
            if !Path::new(&path).is_absolute() || path.contains('\0') {
                return Err(format!(
                    "exclude_cwds[{}] ({path:?}) must be an absolute folder path (`~` forms are refused)",
                    index
                ));
            }
            roots.insert(ids::normalize_root(&path));
        }
        Ok(Self { roots })
    }

    pub fn is_empty(&self) -> bool {
        self.roots.is_empty()
    }

    /// Component boundaries matter: `/work/client` does not match
    /// `/work/client-other`. No case folding or symlink resolution is done.
    pub fn excludes(&self, cwd: &str) -> bool {
        let cwd = ids::normalize_root(cwd);
        self.roots.iter().any(|root| {
            cwd == *root
                || cwd
                    .strip_prefix(root)
                    .is_some_and(|tail| root.ends_with('/') || tail.starts_with('/'))
        })
    }

    /// Check retained metadata when a source is absent from the current skim
    /// (notably a pruned, interrupted cold capture). Missing cwd is unknown,
    /// never evidence that the source was excluded.
    pub fn excludes_session(
        &self,
        store: &rosetta_store::Store,
        session_id: &str,
    ) -> Result<bool, crate::PipelineError> {
        if self.is_empty() {
            return Ok(false);
        }
        let mut pending = vec![session_id.to_owned()];
        let mut visited = BTreeSet::new();
        while let Some(id) = pending.pop() {
            if !visited.insert(id.clone()) {
                continue;
            }
            if let Some(row) = rosetta_store::queries::session(store.conn(), &id)? {
                if row.cwd.as_deref().is_some_and(|cwd| self.excludes(cwd)) {
                    return Ok(true);
                }
                pending.extend(row.parent_session_id);
                pending.push(row.root_session_id);
            }
        }
        Ok(false)
    }
}

/// Preserve the generation fence for a retired, interrupted capture without
/// leaving a staged projection or reading more native bytes. Its checkpoint
/// cannot resume (the reserved epoch is not active); re-admission starts above
/// the archived prefix, so offset zero can never collide with old raw frames.
pub fn reserve_retired_generation(
    adapter: &dyn rosetta_adapter_support::AgentAdapter,
    runtime: &rosetta_core::rows::RuntimeRow,
    store: &mut rosetta_store::Store,
    source: &rosetta_capture::SourceDef,
    generation: u64,
    instrument: &crate::Instrument,
) -> Result<(), crate::PipelineError> {
    use rosetta_store::{Batch, CheckpointUpdate, queries};
    let cursor = queries::cursor_state(store.conn(), &runtime.id, &source.relative_path)?;
    let floor = cursor.as_ref().map_or(0, |cursor| {
        cursor.highest_generation.max(
            crate::PipeCheckpoint::decode(&cursor.checkpoint)
                .map_or(0, |checkpoint| checkpoint.source_generation),
        )
    });
    if generation <= floor {
        return Ok(());
    }
    let mut checkpoint = crate::PipeCheckpoint::fresh(
        &source.session_id,
        &source.stream_id,
        adapter.normalizer_version(),
    );
    checkpoint.source_generation = generation;
    instrument.apply(
        store,
        &Batch {
            ops: vec![],
            checkpoints: vec![CheckpointUpdate {
                runtime_id: runtime.id.clone(),
                stream: source.stream_id.clone(),
                path: source.relative_path.clone(),
                session_id: cursor.as_ref().map_or_else(
                    || source.projected_session_id(),
                    |cursor| cursor.session_id.clone(),
                ),
                expected: cursor.map(|cursor| cursor.checkpoint),
                next: checkpoint.encode(),
                updated_at: None,
            }],
            source: None,
        },
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_normalized_folders_and_descendants_but_not_sibling_prefixes() {
        let policy = CwdExclusions::new(["/work/old/../client//./".into()]).unwrap();
        for cwd in [
            "/work/client",
            "/work/client/",
            "/work/client/sub",
            "/work//client/./sub",
        ] {
            assert!(policy.excludes(cwd), "{cwd}");
        }
        for cwd in [
            "/work",
            "/work/client-other",
            "/work/Client",
            "work/client",
            "/work/client/../other",
            "",
        ] {
            assert!(!policy.excludes(cwd), "{cwd}");
        }
        let root = CwdExclusions::new(["/".into()]).unwrap();
        assert!(root.excludes("/anything"));
        assert!(!root.excludes("relative"));
    }

    #[test]
    fn paths_need_not_exist_but_must_be_absolute() {
        assert!(CwdExclusions::new(["/deleted/workspace".into()]).is_ok());
        for invalid in ["", "work/client", "~/work", "/work/\0client"] {
            let error = CwdExclusions::new(["/valid".into(), invalid.into()]).unwrap_err();
            assert!(error.contains("exclude_cwds[1]"), "{error}");
        }
        assert!(CwdExclusions::default().is_empty());
    }
}
