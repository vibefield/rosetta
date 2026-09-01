//! Durable ownership of a fresh serve's staged raw ledger.
//!
//! Canonical cold state is intentionally process-local, but the source bytes
//! already copied into Rosetta must survive an app exit. A small manifest
//! binds one named staging directory to its projection, records consent before
//! payload capture, and lets the next process rebuild sealed generations from
//! that ledger before it returns to the source roots for unfinished work.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const FORMAT: u32 = 1;
const MANIFEST: &str = ".cold-generation.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Manifest {
    format: u32,
    projection: String,
    ledger: String,
    enabled_agents: BTreeSet<String>,
    runtime_roots: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ColdGeneration {
    pub(crate) path: PathBuf,
    pub(crate) ledger_published: bool,
    pub(crate) enabled_agents: BTreeSet<String>,
    pub(crate) runtime_roots: BTreeMap<String, String>,
}

#[derive(Debug)]
pub(crate) struct ServeColdStage {
    path: PathBuf,
    destination: PathBuf,
    ledger_published: bool,
    generation: ColdGeneration,
}

impl ServeColdStage {
    /// Find a generation left by an earlier fresh serve. A public ledger with
    /// the manifest means the crash landed after the ledger rename but before
    /// SQLite installation; it is just as resumable as the named stage.
    pub(crate) fn inspect(
        destination: &Path,
        projection: &Path,
    ) -> Result<Option<ColdGeneration>, String> {
        let staged = staged_path(destination)?;
        let staged_exists = path_exists(&staged)?;
        let published_exists = path_exists(destination)?;
        if staged_exists && published_exists {
            return Err(format!(
                "cold ingest has both staged ledger {} and public ledger {}; refusing to choose",
                staged.display(),
                destination.display()
            ));
        }
        let (path, ledger_published) = if staged_exists {
            let manifest = staged.join(MANIFEST);
            if !manifest.is_file() {
                let mut entries = std::fs::read_dir(&staged).map_err(|error| {
                    format!("listing incomplete stage {}: {error}", staged.display())
                })?;
                if entries.next().is_none() {
                    // A crash between mkdir and the manifest rename contains
                    // no evidence. `open` may safely repair this empty shell.
                    return Ok(None);
                }
                return Err(format!(
                    "staged ledger {} has no valid manifest; refusing to discard evidence",
                    staged.display()
                ));
            }
            (staged, false)
        } else if published_exists && destination.join(MANIFEST).is_file() {
            (destination.to_path_buf(), true)
        } else {
            return Ok(None);
        };
        let manifest = read_manifest(&path, destination, projection)?;
        Ok(Some(ColdGeneration {
            path,
            ledger_published,
            enabled_agents: manifest.enabled_agents,
            runtime_roots: manifest.runtime_roots,
        }))
    }

    /// Create or reopen the stable generation. Existing consent is immutable
    /// evidence; explicit CLI consent may add a newly installed runtime, but a
    /// known runtime may not silently move to another data root mid-generation.
    pub(crate) fn open(
        destination: &Path,
        projection: &Path,
        enabled_agents: &BTreeSet<String>,
        runtime_roots: &BTreeMap<String, String>,
    ) -> Result<Self, String> {
        if let Some(mut generation) = Self::inspect(destination, projection)? {
            for (kind, root) in &generation.runtime_roots {
                if let Some(current) = runtime_roots.get(kind)
                    && current != root
                {
                    return Err(format!(
                        "cannot resume {kind}: staged root {root} moved to {current}"
                    ));
                }
            }
            generation
                .enabled_agents
                .extend(enabled_agents.iter().cloned());
            for (kind, root) in runtime_roots {
                if generation.enabled_agents.contains(kind) {
                    generation
                        .runtime_roots
                        .entry(kind.clone())
                        .or_insert_with(|| root.clone());
                }
            }
            let manifest = manifest_for(
                destination,
                projection,
                generation.enabled_agents.clone(),
                generation.runtime_roots.clone(),
            )?;
            write_manifest(&generation.path, &manifest)?;
            return Ok(Self {
                path: generation.path.clone(),
                destination: destination.to_path_buf(),
                ledger_published: generation.ledger_published,
                generation,
            });
        }

        if path_exists(destination)? {
            return Err(format!(
                "projection is missing but ledger {} has no cold-generation manifest; run `rosettad rebuild` explicitly",
                destination.display()
            ));
        }
        let path = staged_path(destination)?;
        if path_exists(&path)? {
            // `inspect` accepts every valid staged directory. Reaching here
            // means a crash happened before its manifest. Only an empty tree
            // is safe to repair automatically.
            let mut entries = std::fs::read_dir(&path)
                .map_err(|error| format!("listing incomplete stage {}: {error}", path.display()))?;
            if entries.next().is_some() {
                return Err(format!(
                    "staged ledger {} has no valid manifest; refusing to discard evidence",
                    path.display()
                ));
            }
            std::fs::remove_dir(&path)
                .map_err(|error| format!("removing empty stage {}: {error}", path.display()))?;
        }
        let parent = artifact_parent(destination);
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("creating {}: {error}", parent.display()))?;
        std::fs::create_dir(&path)
            .map_err(|error| format!("creating cold stage {}: {error}", path.display()))?;
        let manifest = manifest_for(
            destination,
            projection,
            enabled_agents.clone(),
            runtime_roots
                .iter()
                .filter(|(kind, _)| enabled_agents.contains(*kind))
                .map(|(kind, root)| (kind.clone(), root.clone()))
                .collect(),
        )?;
        write_manifest(&path, &manifest)?;
        sync_directory(parent)?;
        let generation = ColdGeneration {
            path: path.clone(),
            ledger_published: false,
            enabled_agents: manifest.enabled_agents,
            runtime_roots: manifest.runtime_roots,
        };
        Ok(Self {
            path,
            destination: destination.to_path_buf(),
            ledger_published: false,
            generation,
        })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Publish the durable ledger exactly once. When startup found it already
    /// at the public path, replay is the missing publication work and this is
    /// deliberately a no-op.
    pub(crate) fn publish(&mut self) -> Result<(), String> {
        if self.ledger_published {
            return Ok(());
        }
        if path_exists(&self.destination)? {
            return Err(format!(
                "ledger destination {} appeared during cold ingest",
                self.destination.display()
            ));
        }
        std::fs::rename(&self.path, &self.destination).map_err(|error| {
            format!(
                "atomically publishing staged ledger {} to {}: {error}",
                self.path.display(),
                self.destination.display()
            )
        })?;
        sync_directory(artifact_parent(&self.destination))?;
        self.path = self.destination.clone();
        self.ledger_published = true;
        self.generation.path = self.destination.clone();
        self.generation.ledger_published = true;
        Ok(())
    }

    /// SQLite is installed and validated, so this generation is no longer a
    /// recovery candidate. Removing the marker is deliberately the final
    /// operation: a crash before it is harmless (startup trusts the complete
    /// projection), while a crash before SQLite publication still finds the
    /// marker and resumes from the public ledger.
    pub(crate) fn complete(self) -> Result<(), String> {
        let manifest = self.path.join(MANIFEST);
        match std::fs::remove_file(&manifest) {
            Ok(()) => sync_directory(&self.path),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!(
                "removing completed cold manifest {}: {error}",
                manifest.display()
            )),
        }
    }
}

fn manifest_for(
    destination: &Path,
    projection: &Path,
    enabled_agents: BTreeSet<String>,
    runtime_roots: BTreeMap<String, String>,
) -> Result<Manifest, String> {
    Ok(Manifest {
        format: FORMAT,
        projection: file_name(projection, "projection")?,
        ledger: file_name(destination, "ledger")?,
        enabled_agents,
        runtime_roots,
    })
}

fn read_manifest(root: &Path, destination: &Path, projection: &Path) -> Result<Manifest, String> {
    let path = root.join(MANIFEST);
    let payload = std::fs::read(&path)
        .map_err(|error| format!("reading cold manifest {}: {error}", path.display()))?;
    let manifest: Manifest = serde_json::from_slice(&payload)
        .map_err(|error| format!("decoding cold manifest {}: {error}", path.display()))?;
    if manifest.format != FORMAT
        || manifest.projection != file_name(projection, "projection")?
        || manifest.ledger != file_name(destination, "ledger")?
        || manifest
            .runtime_roots
            .keys()
            .any(|kind| !manifest.enabled_agents.contains(kind))
    {
        return Err(format!(
            "cold manifest {} does not belong to this projection/ledger",
            path.display()
        ));
    }
    Ok(manifest)
}

fn write_manifest(root: &Path, manifest: &Manifest) -> Result<(), String> {
    let payload = serde_json::to_vec_pretty(manifest)
        .map_err(|error| format!("encoding cold-generation manifest: {error}"))?;
    let destination = root.join(MANIFEST);
    let temporary = root.join(format!("{MANIFEST}.tmp-{}", std::process::id()));
    match std::fs::remove_file(&temporary) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "removing stale manifest temporary {}: {error}",
                temporary.display()
            ));
        }
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|error| format!("creating manifest {}: {error}", temporary.display()))?;
    file.write_all(&payload)
        .and_then(|_| file.sync_all())
        .map_err(|error| format!("syncing manifest {}: {error}", temporary.display()))?;
    drop(file);
    std::fs::rename(&temporary, &destination).map_err(|error| {
        format!(
            "installing cold manifest {} from {}: {error}",
            destination.display(),
            temporary.display()
        )
    })?;
    sync_directory(root)
}

fn staged_path(destination: &Path) -> Result<PathBuf, String> {
    let parent = artifact_parent(destination);
    Ok(parent.join(format!("{}.staged", file_name(destination, "ledger")?)))
}

fn artifact_parent(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn file_name(path: &Path, label: &str) -> Result<String, String> {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(str::to_owned)
        .ok_or_else(|| format!("{label} path {} has no UTF-8 file name", path.display()))
}

fn path_exists(path: &Path) -> Result<bool, String> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(format!(
            "refusing cold-generation artifact through symlink {}",
            path.display()
        )),
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!("stating {}: {error}", path.display())),
    }
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<(), String> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("syncing directory {}: {error}", path.display()))
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roots(kind: &str, root: &str) -> BTreeMap<String, String> {
        BTreeMap::from([(kind.to_string(), root.to_string())])
    }

    #[test]
    fn stage_survives_drop_and_reopens_with_enrollment() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("rosetta.db");
        let ledger = dir.path().join("rosetta.ledger");
        let enabled = BTreeSet::from(["claude-code".to_string()]);
        let stage =
            ServeColdStage::open(&ledger, &db, &enabled, &roots("claude-code", "/a")).unwrap();
        let staged = stage.path().to_path_buf();
        std::fs::create_dir(stage.path().join("claude-code")).unwrap();
        std::fs::write(stage.path().join("claude-code/evidence"), b"raw").unwrap();
        drop(stage);

        assert!(staged.is_dir());
        let found = ServeColdStage::inspect(&ledger, &db).unwrap().unwrap();
        assert_eq!(found.enabled_agents, enabled);
        assert_eq!(found.runtime_roots, roots("claude-code", "/a"));
        assert_eq!(
            std::fs::read(found.path.join("claude-code/evidence")).unwrap(),
            b"raw"
        );
    }

    #[test]
    fn published_ledger_remains_resumable_before_projection_install() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("rosetta.db");
        let ledger = dir.path().join("rosetta.ledger");
        let enabled = BTreeSet::from(["codex".to_string()]);
        let mut stage =
            ServeColdStage::open(&ledger, &db, &enabled, &roots("codex", "/c")).unwrap();
        stage.publish().unwrap();
        drop(stage);

        let found = ServeColdStage::inspect(&ledger, &db).unwrap().unwrap();
        assert!(found.ledger_published);
        let mut reopened =
            ServeColdStage::open(&ledger, &db, &enabled, &roots("codex", "/c")).unwrap();
        reopened.publish().unwrap();
        assert!(ledger.is_dir());
    }

    #[test]
    fn completion_removes_the_recovery_marker() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("rosetta.db");
        let ledger = dir.path().join("rosetta.ledger");
        let enabled = BTreeSet::from(["codex".to_string()]);
        let mut stage =
            ServeColdStage::open(&ledger, &db, &enabled, &roots("codex", "/c")).unwrap();
        stage.publish().unwrap();
        stage.complete().unwrap();

        assert_eq!(ServeColdStage::inspect(&ledger, &db).unwrap(), None);
        assert!(ledger.is_dir());
    }

    #[test]
    fn empty_pre_manifest_shell_is_repaired() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("rosetta.db");
        let ledger = dir.path().join("rosetta.ledger");
        let staged = staged_path(&ledger).unwrap();
        std::fs::create_dir(&staged).unwrap();

        let enabled = BTreeSet::from(["codex".to_string()]);
        let stage = ServeColdStage::open(&ledger, &db, &enabled, &roots("codex", "/c")).unwrap();
        assert!(stage.path().join(MANIFEST).is_file());
    }

    #[test]
    fn root_movement_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("rosetta.db");
        let ledger = dir.path().join("rosetta.ledger");
        let enabled = BTreeSet::from(["claude-code".to_string()]);
        drop(ServeColdStage::open(&ledger, &db, &enabled, &roots("claude-code", "/old")).unwrap());
        let error = ServeColdStage::open(&ledger, &db, &enabled, &roots("claude-code", "/new"))
            .unwrap_err();
        assert!(error.contains("staged root /old moved to /new"), "{error}");
    }
}
