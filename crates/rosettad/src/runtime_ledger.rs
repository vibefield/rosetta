//! One raw-ledger directory per runtime under the projection's ledger root,
//! and the stamp that names its owner (2026-09-12).
//!
//! A discovered runtime of kind K keeps `<ledger root>/K/` — every existing
//! ledger stays valid and `rebuild` keeps working for it. A supervisor-enrolled
//! runtime uses `<ledger root>/K~<digest>/`, where `<digest>` is the first 12
//! hex characters of blake3 over its absolute data root (`~` never occurs in a
//! kind id, so the two shapes cannot collide). Either directory carries a
//! small `runtime.json` stamp — identifiers only, allowed by iron rule 5 —
//! written additively when the ledger is opened, so `rebuild` reconstructs
//! each runtime's identity and root from the ledger alone. A directory without
//! a stamp is the legacy per-kind convention: kind = directory name, root = the
//! layout's `~` form.

use std::path::{Path, PathBuf};

use rosetta_adapter_support::AgentAdapter;
use rosetta_capture::Ledger;
use rosetta_core::ids;
use rosetta_core::rows::RuntimeRow;
use serde::{Deserialize, Serialize};

use crate::enroll::Enrollment;

pub(crate) const STAMP_FILE: &str = "runtime.json";
const STAMP_FORMAT: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RuntimeStamp {
    pub format: u32,
    pub id: String,
    pub kind: String,
    pub data_root: String,
}

/// The layout-relative form every discovered runtime carries. Layouts never
/// embed machine paths, so an absolute `data_root` can only be an enrolled
/// runtime: this predicate is how the daemon tells the two apart in a
/// projection that has no column for it.
pub(crate) fn is_layout_root(data_root: &str) -> bool {
    data_root == "~" || data_root.starts_with("~/")
}

pub(crate) fn ledger_dir_name(kind: &str, data_root: &str) -> String {
    if is_layout_root(data_root) {
        kind.to_string()
    } else {
        format!("{kind}~{}", ids::root_digest(data_root))
    }
}

pub(crate) fn ledger_dir_for(root: &Path, kind: &str, data_root: &str) -> PathBuf {
    root.join(ledger_dir_name(kind, data_root))
}

pub(crate) fn ledger_dir(root: &Path, runtime: &RuntimeRow) -> PathBuf {
    ledger_dir_for(root, &runtime.kind, &runtime.data_root)
}

/// Open (or create) the runtime's ledger directory and stamp it. A stamp that
/// already names another runtime is a refusal: two identities must never
/// share one ledger.
pub(crate) fn open_runtime_ledger(root: &Path, runtime: &RuntimeRow) -> Result<Ledger, String> {
    let dir = ledger_dir(root, runtime);
    let ledger = Ledger::open(&dir).map_err(|error| error.to_string())?;
    stamp_directory(&dir, runtime)?;
    Ok(ledger)
}

pub(crate) fn stamp_directory(dir: &Path, runtime: &RuntimeRow) -> Result<(), String> {
    let expected = RuntimeStamp {
        format: STAMP_FORMAT,
        id: runtime.id.clone(),
        kind: runtime.kind.clone(),
        data_root: runtime.data_root.clone(),
    };
    if let Some(existing) = read_stamp(dir)? {
        if existing.id != expected.id {
            return Err(format!(
                "ledger {} is stamped for runtime {} ({} {}) but {} ({} {}) wants to write it",
                dir.display(),
                existing.id,
                existing.kind,
                existing.data_root,
                expected.id,
                expected.kind,
                expected.data_root
            ));
        }
        return Ok(());
    }
    let payload = serde_json::to_vec_pretty(&expected)
        .map_err(|error| format!("encoding runtime stamp: {error}"))?;
    let destination = dir.join(STAMP_FILE);
    let temporary = dir.join(format!("{STAMP_FILE}.tmp-{}", std::process::id()));
    std::fs::write(&temporary, payload)
        .map_err(|error| format!("writing runtime stamp {}: {error}", temporary.display()))?;
    std::fs::File::open(&temporary)
        .and_then(|file| file.sync_all())
        .map_err(|error| format!("syncing runtime stamp {}: {error}", temporary.display()))?;
    std::fs::rename(&temporary, &destination).map_err(|error| {
        format!(
            "installing runtime stamp {}: {error}",
            destination.display()
        )
    })?;
    Ok(())
}

pub(crate) fn read_stamp(dir: &Path) -> Result<Option<RuntimeStamp>, String> {
    let path = dir.join(STAMP_FILE);
    let payload = match std::fs::read(&path) {
        Ok(payload) => payload,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("reading runtime stamp {}: {error}", path.display())),
    };
    let stamp: RuntimeStamp = serde_json::from_slice(&payload)
        .map_err(|error| format!("decoding runtime stamp {}: {error}", path.display()))?;
    if stamp.format != STAMP_FORMAT {
        return Err(format!(
            "runtime stamp {} has unsupported format {}",
            path.display(),
            stamp.format
        ));
    }
    if stamp.id != ids::runtime_id(&stamp.kind, &stamp.data_root) {
        return Err(format!(
            "runtime stamp {} does not match its own kind and data_root",
            path.display()
        ));
    }
    Ok(Some(stamp))
}

/// The runtime a legacy per-kind ledger directory belongs to: the layout root.
pub(crate) fn layout_runtime(adapter: &dyn AgentAdapter) -> RuntimeRow {
    let layout = adapter.layout();
    RuntimeRow {
        id: ids::runtime_id(adapter.kind(), &layout.data_root),
        kind: adapter.kind().to_string(),
        data_root: layout.data_root.clone(),
        enabled: true,
        account: None,
        config: None,
        skills: None,
        versions_seen: None,
        discovered_at: 0,
        label: None,
        provenance: Default::default(),
    }
}

/// One ledger directory and the runtime it replays as.
pub(crate) struct LedgerRuntime {
    pub adapter: &'static dyn AgentAdapter,
    pub runtime: RuntimeRow,
    pub dir: PathBuf,
}

impl LedgerRuntime {
    pub fn dir_name(&self) -> String {
        self.dir
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    }
}

impl std::fmt::Debug for LedgerRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LedgerRuntime")
            .field("kind", &self.runtime.kind)
            .field("data_root", &self.runtime.data_root)
            .field("id", &self.runtime.id)
            .field("dir", &self.dir)
            .finish()
    }
}

/// Every runtime ledger under `ledgers`, in directory-name order. Identity
/// comes from the stamp; a stamp-less directory named after a registered kind
/// is the legacy convention; a stamp-less `K~<digest>` directory can still be
/// named by the enrollment file (the file's entry that maps to that name).
/// Anything else is refused rather than guessed.
pub(crate) fn ledger_runtimes(
    ledgers: &Path,
    registry: &[&'static dyn AgentAdapter],
    enrollment: Option<&Enrollment>,
) -> Result<Vec<LedgerRuntime>, String> {
    let mut names = Vec::new();
    for entry in std::fs::read_dir(ledgers)
        .map_err(|error| format!("listing {}: {error}", ledgers.display()))?
    {
        let entry = entry.map_err(|error| format!("listing {}: {error}", ledgers.display()))?;
        if !entry
            .file_type()
            .map_err(|error| format!("stating {}: {error}", entry.path().display()))?
            .is_dir()
        {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        names.push(name);
    }
    names.sort();

    let mut out = Vec::with_capacity(names.len());
    for name in names {
        let dir = ledgers.join(&name);
        let adapter_of = |kind: &str| {
            registry
                .iter()
                .copied()
                .find(|adapter| adapter.kind() == kind)
        };
        let (adapter, runtime) = if let Some(stamp) = read_stamp(&dir)? {
            let adapter = adapter_of(&stamp.kind).ok_or_else(|| {
                format!(
                    "ledger {} is stamped for unknown agent kind {}",
                    dir.display(),
                    stamp.kind
                )
            })?;
            let expected = ledger_dir_name(&stamp.kind, &stamp.data_root);
            if expected != name {
                return Err(format!(
                    "ledger {} is stamped for {} {} which belongs in {expected}",
                    dir.display(),
                    stamp.kind,
                    stamp.data_root
                ));
            }
            let mut runtime = layout_runtime(adapter);
            runtime.id = stamp.id;
            runtime.data_root = stamp.data_root;
            (adapter, runtime)
        } else if let Some(adapter) = adapter_of(&name) {
            (adapter, layout_runtime(adapter))
        } else if let Some(enrolled) = enrollment.and_then(|enrollment| {
            enrollment.runtimes.iter().find(|enrolled| {
                ledger_dir_name(&enrolled.runtime.kind, &enrolled.runtime.data_root) == name
            })
        }) {
            let mut runtime = enrolled.runtime.clone();
            runtime.enabled = true;
            (enrolled.adapter, runtime)
        } else {
            return Err(format!(
                "ledger directory {} has no runtime stamp and is not a registered agent kind; pass --enroll to name it",
                dir.display()
            ));
        };
        out.push(LedgerRuntime {
            adapter,
            runtime,
            dir,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rosetta_adapter_claude_code::ClaudeCodeAdapter;

    #[test]
    fn discovered_runtimes_keep_the_per_kind_directory_and_enrolled_ones_add_a_digest() {
        assert_eq!(ledger_dir_name("claude-code", "~/.claude"), "claude-code");
        let enrolled = ledger_dir_name("claude-code", "/users/u1/agents/roots/claude-code/9a3d");
        assert!(enrolled.starts_with("claude-code~"), "{enrolled}");
        assert_eq!(enrolled.len(), "claude-code~".len() + 12);
        assert_ne!(
            enrolled,
            ledger_dir_name("claude-code", "/users/u1/agents/roots/claude-code/9a3e")
        );
    }

    #[test]
    fn opening_a_ledger_stamps_it_once_and_refuses_another_identity() {
        let temp = tempfile::tempdir().unwrap();
        let mut runtime = layout_runtime(&ClaudeCodeAdapter);
        runtime.data_root = "/abs/kim".into();
        runtime.id = ids::runtime_id("claude-code", "/abs/kim");
        let ledger = open_runtime_ledger(temp.path(), &runtime).unwrap();
        drop(ledger);
        let dir = ledger_dir(temp.path(), &runtime);
        let stamp = read_stamp(&dir).unwrap().unwrap();
        assert_eq!(stamp.id, runtime.id);
        assert_eq!(stamp.kind, "claude-code");
        assert_eq!(stamp.data_root, "/abs/kim");

        // Idempotent for the same runtime; a refusal for a different one.
        open_runtime_ledger(temp.path(), &runtime).unwrap();
        let other = layout_runtime(&ClaudeCodeAdapter);
        let error = stamp_directory(&dir, &other).unwrap_err();
        assert!(error.contains("is stamped for runtime"), "{error}");
    }

    #[test]
    fn a_stampless_kind_directory_is_the_legacy_discovered_runtime() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("claude-code")).unwrap();
        std::fs::create_dir(temp.path().join(".hidden")).unwrap();
        let runtimes = ledger_runtimes(temp.path(), &[&ClaudeCodeAdapter], None).unwrap();
        assert_eq!(runtimes.len(), 1);
        assert_eq!(runtimes[0].runtime.data_root, "~/.claude");
        assert_eq!(
            runtimes[0].runtime.id,
            layout_runtime(&ClaudeCodeAdapter).id
        );
        assert_eq!(runtimes[0].dir_name(), "claude-code");

        std::fs::create_dir(temp.path().join("claude-code~deadbeef0000")).unwrap();
        let error = ledger_runtimes(temp.path(), &[&ClaudeCodeAdapter], None).unwrap_err();
        assert!(error.contains("has no runtime stamp"), "{error}");
    }
}
