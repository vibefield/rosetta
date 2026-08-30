//! Materialization of content-addressed members of the canonical projection.
//!
//! SQLite stores only stable `/api/assets/<digest>.<ext>` references. Decoded
//! media and oversized canonical text live in a content-addressed sibling directory. A file
//! is durable before a live SQLite commit may expose its URL; private rebuilds
//! defer file and directory barriers until their pre-publication gate.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

use rosetta_core::timeline::AssetRef;
use rosetta_core::{IngestOp, ProjectionAsset};

use crate::{Batch, StoreError, elapsed_nanos};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AssetWriteStats {
    pub assets: u64,
    pub files_written: u64,
    pub files_reused: u64,
    pub bytes: u64,
    pub nanos: u64,
}

/// `<projection>.assets`, preserving the full projection filename just as the
/// detached search sidecar preserves it in `<projection>.search`.
pub fn asset_root(projection: &Path) -> PathBuf {
    let mut root: OsString = projection.as_os_str().to_owned();
    root.push(".assets");
    PathBuf::from(root)
}

/// Resolve one already-committed canonical UTF-8 member for detached search.
/// A missing or corrupted member fails the build rather than silently
/// indexing only its preview.
pub fn read_text_member(projection: &Path, asset: &AssetRef) -> Result<String, StoreError> {
    asset.validate().map_err(|error| StoreError::AssetInvalid {
        message: format!("invalid canonical text member: {error}"),
    })?;
    if asset.media_type != "text/plain; charset=utf-8" {
        return Err(StoreError::AssetInvalid {
            message: format!(
                "search prose member has unsupported media type {}",
                asset.media_type
            ),
        });
    }
    let file_name = asset
        .url
        .strip_prefix(rosetta_core::assets::IMAGE_ASSET_URL_PREFIX)
        .ok_or_else(|| StoreError::AssetInvalid {
            message: format!("unsafe canonical member URL {}", asset.url),
        })?;
    let path = asset_root(projection).join(file_name);
    let metadata = fs::symlink_metadata(&path)?;
    if !metadata.file_type().is_file() || metadata.len() != asset.byte_len {
        return Err(StoreError::AssetInvalid {
            message: format!(
                "canonical text member {} has the wrong shape",
                path.display()
            ),
        });
    }
    let bytes = fs::read(&path)?;
    if blake3::hash(&bytes).to_hex().as_str() != asset.digest {
        return Err(StoreError::AssetInvalid {
            message: format!("canonical text member {} failed its hash", path.display()),
        });
    }
    String::from_utf8(bytes).map_err(|error| StoreError::AssetInvalid {
        message: format!(
            "canonical text member {} is not UTF-8: {error}",
            path.display()
        ),
    })
}

#[derive(Debug)]
pub(crate) struct AssetStore {
    root: Option<PathBuf>,
    pending_sync: BTreeSet<PathBuf>,
    root_created: bool,
}

impl AssetStore {
    pub(crate) fn for_projection(projection: &Path) -> Self {
        Self::at(asset_root(projection))
    }

    pub(crate) fn at(root: PathBuf) -> Self {
        Self {
            root: Some(root),
            pending_sync: BTreeSet::new(),
            root_created: false,
        }
    }

    pub(crate) fn in_memory() -> Self {
        Self {
            root: None,
            pending_sync: BTreeSet::new(),
            root_created: false,
        }
    }

    pub(crate) fn materialize_batch(
        &mut self,
        batch: &Batch,
        durable_now: bool,
    ) -> Result<AssetWriteStats, StoreError> {
        let started = Instant::now();
        let mut stats = AssetWriteStats::default();
        for op in &batch.ops {
            let IngestOp::ProjectionAsset { asset } = op else {
                continue;
            };
            stats.assets += 1;
            stats.bytes = stats
                .bytes
                .saturating_add(u64::try_from(asset.bytes.len()).unwrap_or(u64::MAX));
            match self.materialize(asset, durable_now)? {
                Materialized::Written => stats.files_written += 1,
                Materialized::Reused => stats.files_reused += 1,
            }
        }
        stats.nanos = elapsed_nanos(started);
        Ok(stats)
    }

    fn materialize(
        &mut self,
        asset: &ProjectionAsset,
        durable_now: bool,
    ) -> Result<Materialized, StoreError> {
        validate_asset(asset)?;
        let Some(root) = self.root.clone() else {
            // In-memory projections are test-only. Validate the canonical
            // contract but deliberately have no filesystem materialization.
            return Ok(Materialized::Reused);
        };
        let file_name = asset.file_name().ok_or_else(|| StoreError::AssetInvalid {
            message: format!(
                "unsupported projection member media type {}",
                asset.media_type
            ),
        })?;

        match fs::symlink_metadata(&root) {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => {
                return Err(StoreError::AssetInvalid {
                    message: format!("asset root {} is not a directory", root.display()),
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir_all(&root)?;
                self.root_created = true;
            }
            Err(error) => return Err(error.into()),
        }

        let destination = root.join(file_name);
        match fs::symlink_metadata(&destination) {
            Ok(_) => {
                verify_existing(&destination, asset)?;
                return Ok(Materialized::Reused);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }

        let mut temporary = tempfile::Builder::new()
            .prefix(".rosetta-asset-")
            .tempfile_in(&root)?;
        temporary.write_all(&asset.bytes)?;
        if durable_now {
            temporary.as_file().sync_all()?;
        }
        match temporary.persist_noclobber(&destination) {
            Ok(_) => {}
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                verify_existing(&destination, asset)?;
                return Ok(Materialized::Reused);
            }
            Err(error) => return Err(error.error.into()),
        }

        if durable_now {
            sync_directory(&root)?;
            if self.root_created {
                sync_parent(&root)?;
                self.root_created = false;
            }
        } else {
            self.pending_sync.insert(destination);
        }
        Ok(Materialized::Written)
    }

    pub(crate) fn finish_pending(&mut self) -> Result<u64, StoreError> {
        let started = Instant::now();
        for path in &self.pending_sync {
            File::open(path)?.sync_all()?;
        }
        if !self.pending_sync.is_empty()
            && let Some(root) = &self.root
        {
            sync_directory(root)?;
            if self.root_created {
                sync_parent(root)?;
            }
        }
        self.pending_sync.clear();
        self.root_created = false;
        Ok(elapsed_nanos(started))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Materialized {
    Written,
    Reused,
}

fn validate_asset(asset: &ProjectionAsset) -> Result<(), StoreError> {
    if asset.bytes.is_empty() {
        return Err(StoreError::AssetInvalid {
            message: format!(
                "projection asset {} has no materialization bytes",
                asset.digest
            ),
        });
    }
    let actual = blake3::hash(&asset.bytes).to_hex().to_string();
    if actual != asset.digest {
        return Err(StoreError::AssetInvalid {
            message: format!(
                "projection asset digest mismatch: declared {}, decoded {actual}",
                asset.digest
            ),
        });
    }
    if !magic_matches(&asset.media_type, &asset.bytes) {
        return Err(StoreError::AssetInvalid {
            message: format!(
                "projection asset {} does not match media type {}",
                asset.digest, asset.media_type
            ),
        });
    }
    Ok(())
}

fn verify_existing(path: &Path, asset: &ProjectionAsset) -> Result<(), StoreError> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() || metadata.len() != asset.bytes.len() as u64 {
        return Err(StoreError::AssetInvalid {
            message: format!(
                "projection asset {} is not the expected regular file",
                path.display()
            ),
        });
    }
    let existing = fs::read(path)?;
    if blake3::hash(&existing).to_hex().as_str() != asset.digest {
        return Err(StoreError::AssetInvalid {
            message: format!(
                "projection asset {} failed its content hash",
                path.display()
            ),
        });
    }
    Ok(())
}

fn magic_matches(media_type: &str, bytes: &[u8]) -> bool {
    match media_type {
        "image/png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "image/jpeg" => bytes.starts_with(b"\xff\xd8\xff"),
        "image/gif" => bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
        "image/webp" => bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP",
        "application/pdf" => bytes.starts_with(b"%PDF-"),
        "application/vnd.rosetta.canonical-document+json" => {
            serde_json::from_slice::<serde_json::Value>(bytes).is_ok()
        }
        "text/plain; charset=utf-8" => std::str::from_utf8(bytes).is_ok(),
        _ => false,
    }
}

fn sync_directory(path: &Path) -> Result<(), StoreError> {
    File::open(path)?.sync_all()?;
    Ok(())
}

fn sync_parent(path: &Path) -> Result<(), StoreError> {
    if let Some(parent) = path.parent() {
        sync_directory(parent)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use rosetta_core::{IngestOp, ProjectionAsset};

    use super::{AssetStore, asset_root};
    use crate::Batch;

    fn png() -> ProjectionAsset {
        ProjectionAsset::new("image/png", b"\x89PNG\r\n\x1a\nbody".to_vec()).unwrap()
    }

    #[test]
    fn writes_once_and_reuses_by_content_hash() {
        let root = tempfile::tempdir().unwrap();
        let projection = root.path().join("rosetta.db");
        let asset = png();
        let batch = Batch {
            ops: vec![IngestOp::ProjectionAsset {
                asset: asset.clone(),
            }],
            ..Batch::default()
        };
        let mut store = AssetStore::for_projection(&projection);
        let first = store.materialize_batch(&batch, true).unwrap();
        let second = store.materialize_batch(&batch, true).unwrap();

        assert_eq!(first.files_written, 1);
        assert_eq!(second.files_reused, 1);
        assert_eq!(
            std::fs::read(asset_root(&projection).join(asset.file_name().unwrap())).unwrap(),
            asset.bytes
        );
    }

    #[test]
    fn refuses_a_hash_named_file_with_different_bytes() {
        let root = tempfile::tempdir().unwrap();
        let projection = root.path().join("rosetta.db");
        let asset = png();
        let assets = asset_root(&projection);
        std::fs::create_dir(&assets).unwrap();
        std::fs::write(assets.join(asset.file_name().unwrap()), b"same length!!").unwrap();
        let batch = Batch {
            ops: vec![IngestOp::ProjectionAsset { asset }],
            ..Batch::default()
        };

        assert!(
            AssetStore::for_projection(&projection)
                .materialize_batch(&batch, true)
                .is_err()
        );
    }

    #[test]
    fn materializes_utf8_text_members() {
        let root = tempfile::tempdir().unwrap();
        let projection = root.path().join("rosetta.db");
        let asset = ProjectionAsset::text("oversized canonical prose");
        let batch = Batch {
            ops: vec![IngestOp::ProjectionAsset {
                asset: asset.clone(),
            }],
            ..Batch::default()
        };
        AssetStore::for_projection(&projection)
            .materialize_batch(&batch, true)
            .unwrap();
        assert_eq!(
            std::fs::read(asset_root(&projection).join(asset.file_name().unwrap())).unwrap(),
            asset.bytes
        );
    }
}
