//! The library needs catalog access; captured content is scoped to its source
//! session. HTML is delivered as inert bytes, never active daemon-origin HTML.
//! The export door (RO-32) writes one capture, wrapped in the served shell,
//! as a directory another host serves from an origin of its own; the daemon
//! itself never serves it.

use std::ffi::OsString;
use std::io::{ErrorKind, Read, Write};
use std::path::Component;
use std::sync::atomic::{AtomicU64, Ordering};

use axum::extract::rejection::JsonRejection;

use super::*;
use rosetta_core::html_artifacts::{
    HTML_MAX_BYTES, HTML_MEDIA_TYPE, SHELL_HOSTS, SHELL_VERSION, runnable_document,
};
use rosetta_core::timeline::AssetRef;
use rosetta_protocol::{
    ArtifactExportRequest, ArtifactExportResponse, ArtifactListRequest, ArtifactPage,
};
use rosetta_store::html_artifacts::{self, ListError};
use serde::{Deserialize, Serialize};

pub(super) async fn list(
    State(state): State<AppState>,
    Query(request): Query<ArtifactListRequest>,
) -> Result<Json<ArtifactPage>, Failure> {
    let hide_pending = !state.projection.is_published();
    let page = with_read(state.projection.clone(), move |conn| {
        html_artifacts::list(conn, &request, hide_pending).map_err(|error| match error {
            ListError::Invalid => Failure::bad_request("invalid artifact filter or cursor"),
            ListError::Expired => Failure::cursor_expired(),
            ListError::Store(error) => store_err(error),
        })
    })
    .await?;
    Ok(Json(page))
}

/// The capture of one publication, admitted and checked against its media
/// type and bound before any file is opened. The content read and the export
/// door share it, so both refuse exactly the same rows.
async fn captured_asset(
    state: &AppState,
    session_id: String,
    publication_id: String,
) -> Result<AssetRef, Failure> {
    let hide_pending = !state.projection.is_published();
    let asset = with_read(state.projection.clone(), move |conn| {
        if behind_admission(conn, &session_id, hide_pending)? {
            return Err(Failure::not_found("artifact is not available"));
        }
        html_artifacts::get(conn, &session_id, &publication_id)
            .map_err(store_err)?
            .and_then(|entry| entry.asset)
            .ok_or_else(|| Failure::not_found("this publication has no captured HTML"))
    })
    .await?;
    asset
        .validate()
        .map_err(|e| Failure::internal(format!("invalid artifact content: {e}")))?;
    if asset.media_type != HTML_MEDIA_TYPE || asset.byte_len > HTML_MAX_BYTES as u64 {
        return Err(Failure::internal("invalid artifact media type or size"));
    }
    Ok(asset)
}

/// The capture's bytes from the asset store, held to its recorded length and
/// content hash. Blocking: the caller runs it on the blocking pool.
fn read_verified(root: &Path, asset: &AssetRef) -> Result<Vec<u8>, Failure> {
    let path = root.join(format!("{}.html", asset.digest));
    let metadata = std::fs::symlink_metadata(&path)
        .map_err(|_| Failure::not_found("captured HTML is unavailable"))?;
    if !metadata.is_file() || metadata.len() != asset.byte_len {
        return Err(Failure::internal("captured HTML has the wrong size"));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(|_| Failure::not_found("captured HTML is unavailable"))?
        .take(HTML_MAX_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| Failure::internal(format!("reading captured HTML: {e}")))?;
    if bytes.len() as u64 != asset.byte_len
        || blake3::hash(&bytes).to_hex().as_str() != asset.digest
    {
        return Err(Failure::internal("captured HTML failed its content hash"));
    }
    Ok(bytes)
}

pub(super) async fn content(
    State(state): State<AppState>,
    UrlPath((session_id, publication_id)): UrlPath<(String, String)>,
) -> Result<Response, Failure> {
    let asset = captured_asset(&state, session_id, publication_id).await?;
    let root = assets::asset_root(&state.db);
    let bytes = tokio::task::spawn_blocking(move || read_verified(&root, &asset))
        .await
        .map_err(|e| Failure::internal(format!("artifact read failed: {e}")))??;
    let mut response = Response::new(Body::from(bytes));
    for (name, value) in [
        (header::CONTENT_TYPE, "application/octet-stream"),
        (
            header::CONTENT_DISPOSITION,
            "attachment; filename=artifact.html",
        ),
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        (
            header::CONTENT_SECURITY_POLICY,
            "sandbox; default-src 'none'",
        ),
        (header::CACHE_CONTROL, "private, no-store"),
    ] {
        response
            .headers_mut()
            .insert(name, HeaderValue::from_static(value));
    }
    Ok(response)
}

/// The one directory the export door may write under (`rosettad serve
/// --export-root`, RO-32). Resolved once at startup: the root's own path may
/// pass through links the operator chose (macOS's `/var` is one), but no
/// component a request names below it may be a link.
#[derive(Debug, Clone)]
pub struct ExportRoot {
    /// The path as configured, accepted as a spelling of the root.
    given: PathBuf,
    /// The root with every link resolved: where writes happen.
    real: PathBuf,
}

impl ExportRoot {
    /// Admit an export root: absolute, existing, and a directory.
    pub fn new(path: &Path) -> Result<Self, String> {
        if !path.is_absolute() {
            return Err(format!(
                "--export-root must be absolute: {}",
                path.display()
            ));
        }
        let real = std::fs::canonicalize(path)
            .map_err(|error| format!("--export-root {}: {error}", path.display()))?;
        let metadata = std::fs::metadata(&real)
            .map_err(|error| format!("--export-root {}: {error}", path.display()))?;
        if !metadata.is_dir() {
            return Err(format!(
                "--export-root {} is not a directory",
                path.display()
            ));
        }
        Ok(Self {
            given: path.to_owned(),
            real,
        })
    }

    /// The resolved root.
    pub fn path(&self) -> &Path {
        &self.real
    }

    /// The plain components of `into` below the root: `None` unless `into`
    /// is absolute, starts with the root by either spelling, and continues
    /// with at least one ordinary component and never `..`.
    fn below(&self, into: &Path) -> Option<Vec<OsString>> {
        if !into.is_absolute() {
            return None;
        }
        let rest = into
            .strip_prefix(&self.real)
            .or_else(|_| into.strip_prefix(&self.given))
            .ok()?;
        let mut parts = Vec::new();
        for component in rest.components() {
            match component {
                Component::Normal(part) => parts.push(part.to_owned()),
                _ => return None,
            }
        }
        (!parts.is_empty()).then_some(parts)
    }
}

/// The marker every export directory carries. A directory whose marker names
/// another publication, or that has none and holds anything, is not the
/// door's to replace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ExportMarker {
    publication_id: String,
    source_digest: String,
    shell_version: String,
}

const MARKER: &str = ".rosetta-export.json";
const DOCUMENT: &str = "index.html";
const MARKER_MAX_BYTES: u64 = 4096;
/// Staging and moved-aside siblings: `.rosetta-export-<pid>-<n>`.
const SIBLING_PREFIX: &str = ".rosetta-export-";
/// How old another process's leftover sibling must be before an export sweeps
/// it: a live export holds one for milliseconds.
const LEFTOVER_AGE: std::time::Duration = std::time::Duration::from_secs(3600);

/// Exports are rare and write the disk: one at a time per process, so two
/// requests for one directory never interleave their swaps.
static EXPORTS: Mutex<()> = Mutex::new(());
static STAGING: AtomicU64 = AtomicU64::new(0);

pub(super) async fn export(
    State(state): State<AppState>,
    UrlPath((session_id, publication_id)): UrlPath<(String, String)>,
    request: Result<Json<ArtifactExportRequest>, JsonRejection>,
) -> Result<Json<ArtifactExportResponse>, Failure> {
    let Some(root) = state.export_root.clone() else {
        return Err(Failure::not_ready(
            "this daemon has no export root; start it with --export-root",
        ));
    };
    // A missing content type, malformed JSON or a missing `into` answers the
    // documented envelope, never axum's plain-text rejection.
    let Json(request) = request.map_err(|_| Failure::bad_request("expected a JSON {into}"))?;
    if request.into.is_empty() || request.into.len() > 4096 || request.into.contains('\0') {
        return Err(Failure::bad_request("invalid export directory"));
    }
    let Some(parts) = root.below(Path::new(&request.into)) else {
        return Err(Failure::bad_request(format!(
            "the export directory must be absolute and inside {}",
            root.path().display()
        )));
    };
    let asset = captured_asset(&state, session_id, publication_id.clone()).await?;
    let digest = asset.digest.clone();
    let assets_root = assets::asset_root(&state.db);
    let written = tokio::task::spawn_blocking(move || {
        // Verify first: a capture that fails its hash writes nothing at all.
        let bytes = read_verified(&assets_root, &asset)?;
        // Decoded as the room's TextDecoder does: replacement for invalid
        // UTF-8, and a leading byte-order mark consumed.
        let text = String::from_utf8_lossy(&bytes);
        let document = runnable_document(text.strip_prefix('\u{feff}').unwrap_or(&text));
        let marker = ExportMarker {
            publication_id,
            source_digest: asset.digest,
            shell_version: SHELL_VERSION.to_owned(),
        };
        let _one = EXPORTS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        write_export(root.path(), &parts, &document, &marker)
    })
    .await
    .map_err(|e| Failure::internal(format!("artifact export failed: {e}")))??;
    Ok(Json(ArtifactExportResponse {
        written,
        digest,
        shell_version: SHELL_VERSION.to_owned(),
        hosts: SHELL_HOSTS.all().into_iter().map(str::to_owned).collect(),
    }))
}

fn export_conflict(message: impl Into<String>) -> Failure {
    Failure(
        StatusCode::CONFLICT,
        ApiError {
            details: None,
            code: ErrorCode::ExportConflict,
            message: message.into(),
        },
    )
}

fn io_failure(what: &str, error: std::io::Error) -> Failure {
    Failure::internal(format!("artifact export, {what}: {error}"))
}

/// What a target directory already holds.
enum Held {
    Empty,
    Ours(ExportMarker),
    Foreign,
}

/// Write `document` and `marker` as the directory `root/parts…`, which ends
/// as either this export or untouched. Returns false, writing nothing, when
/// the directory already holds this capture in this shell. Blocking.
fn write_export(
    root: &Path,
    parts: &[OsString],
    document: &str,
    marker: &ExportMarker,
) -> Result<bool, Failure> {
    let Some((name, ancestors)) = parts.split_last() else {
        return Err(Failure::bad_request("invalid export directory"));
    };
    let mut parent = root.to_path_buf();
    for part in ancestors {
        parent.push(part);
        match std::fs::symlink_metadata(&parent) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(Failure::bad_request(
                    "the export directory passes through a symbolic link",
                ));
            }
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => {
                return Err(Failure::bad_request("the export directory passes through a file"));
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {
                private_dir(&parent).map_err(|e| io_failure("creating a parent", e))?;
                sync_dir(parent.parent().unwrap_or(root))
                    .map_err(|e| io_failure("syncing a parent", e))?;
            }
            Err(error) => return Err(io_failure("reading a parent", error)),
        }
    }
    let target = parent.join(name);
    let existing = match std::fs::symlink_metadata(&target) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(Failure::bad_request("the export directory is a symbolic link"));
        }
        Ok(metadata) if metadata.is_dir() => true,
        Ok(_) => return Err(export_conflict("the export directory is a file")),
        Err(error) if error.kind() == ErrorKind::NotFound => false,
        Err(error) => return Err(io_failure("reading the directory", error)),
    };
    if existing {
        match held(&target, &marker.publication_id)? {
            Held::Empty => {}
            Held::Ours(held) => {
                let document_present = std::fs::symlink_metadata(target.join(DOCUMENT))
                    .is_ok_and(|metadata| metadata.is_file());
                if held == *marker && document_present {
                    return Ok(false);
                }
            }
            Held::Foreign => {
                return Err(export_conflict(
                    "the export directory holds something that is not this publication's export",
                ));
            }
        }
    }
    sweep_leftovers(&parent);
    let staged = stage(&parent, document, marker)?;
    if let Err(error) = install(&parent, &target, &staged, existing) {
        let _ = std::fs::remove_dir_all(&staged);
        return Err(io_failure("installing", error));
    }
    Ok(true)
}

/// Read a target directory's marker. Only a regular file within the bound
/// that parses and names this publication makes the directory ours.
fn held(target: &Path, publication_id: &str) -> Result<Held, Failure> {
    let mut entries =
        std::fs::read_dir(target).map_err(|e| io_failure("reading the directory", e))?;
    if entries.next().is_none() {
        return Ok(Held::Empty);
    }
    let path = target.join(MARKER);
    let marker = match std::fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_file() && metadata.len() <= MARKER_MAX_BYTES => {
            std::fs::read(&path)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<ExportMarker>(&bytes).ok())
        }
        _ => None,
    };
    Ok(match marker {
        Some(marker) if marker.publication_id == publication_id => Held::Ours(marker),
        _ => Held::Foreign,
    })
}

/// A fresh sibling name for staging or for the replaced copy. Dot-named, so
/// a host serving the parent's listing does not show it.
fn sibling(parent: &Path) -> PathBuf {
    let n = STAGING.fetch_add(1, Ordering::Relaxed);
    parent.join(format!("{SIBLING_PREFIX}{}-{n}", std::process::id()))
}

/// Remove what an export that never finished left beside `parent`'s
/// children: a staging directory, or an older export moved aside when a
/// crash came between the two renames of [`install`]. Called under the
/// process's export lock, so nothing of this process is in flight and its
/// own leftovers go whatever their age; another process's go once they are
/// [`LEFTOVER_AGE`] old. Only directories with the sibling prefix, never a
/// link. Best effort: what cannot be removed waits for the next export.
fn sweep_leftovers(parent: &Path) {
    let Ok(entries) = std::fs::read_dir(parent) else {
        return;
    };
    let own = format!("{SIBLING_PREFIX}{}-", std::process::id());
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !name.starts_with(SIBLING_PREFIX) {
            continue;
        }
        let path = entry.path();
        let Ok(metadata) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if !metadata.is_dir() {
            continue;
        }
        let stale = name.starts_with(&own)
            || metadata
                .modified()
                .ok()
                .and_then(|at| at.elapsed().ok())
                .is_some_and(|age| age >= LEFTOVER_AGE);
        if stale {
            let _ = std::fs::remove_dir_all(&path);
        }
    }
}

/// The export written whole in a private sibling directory: the document,
/// then the marker, each synced, then the directory itself.
fn stage(parent: &Path, document: &str, marker: &ExportMarker) -> Result<PathBuf, Failure> {
    let mut staged = None;
    for _ in 0..16 {
        let candidate = sibling(parent);
        match private_dir(&candidate) {
            Ok(()) => {
                staged = Some(candidate);
                break;
            }
            Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(io_failure("staging", error)),
        }
    }
    let Some(staged) = staged else {
        return Err(Failure::internal("artifact export, staging: no free staging name"));
    };
    if let Err(error) = fill(&staged, document, marker) {
        let _ = std::fs::remove_dir_all(&staged);
        return Err(io_failure("writing", error));
    }
    Ok(staged)
}

fn fill(staged: &Path, document: &str, marker: &ExportMarker) -> std::io::Result<()> {
    write_private(&staged.join(DOCUMENT), document.as_bytes())?;
    let marker = serde_json::to_vec(marker).map_err(std::io::Error::other)?;
    write_private(&staged.join(MARKER), &marker)?;
    sync_dir(staged)
}

/// Rename the staged directory into place. A directory already there (empty,
/// or this publication's older export) moves aside first and is removed once
/// the new one stands, whatever the parent's sync then answers; a failed
/// second rename puts it back.
///
/// The two renames are not one atomic swap (std has no portable exchange):
/// a reader between them finds no directory, and a crash between them leaves
/// the old export in a dot-named sibling, which the next export under this
/// parent sweeps ([`sweep_leftovers`]).
fn install(parent: &Path, target: &Path, staged: &Path, existing: bool) -> std::io::Result<()> {
    if !existing {
        std::fs::rename(staged, target)?;
        return sync_dir(parent);
    }
    let aside = sibling(parent);
    std::fs::rename(target, &aside)?;
    if let Err(error) = std::fs::rename(staged, target) {
        if let Err(restore) = std::fs::rename(&aside, target) {
            return Err(std::io::Error::new(
                error.kind(),
                format!(
                    "{error}; restoring the previous export failed ({restore}), it is at {}",
                    aside.display()
                ),
            ));
        }
        return Err(error);
    }
    let swapped = sync_dir(parent);
    let _ = std::fs::remove_dir_all(&aside);
    swapped?;
    sync_dir(parent)
}

/// A directory only its owner may enter (0700 on unix).
fn private_dir(path: &Path) -> std::io::Result<()> {
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)
}

/// A new file only its owner may read (0600 on unix), synced before it closes.
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// Make a directory's entries durable (unix; elsewhere a rename is what the
/// platform offers).
fn sync_dir(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    std::fs::File::open(path)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;

    #[test]
    fn a_request_names_a_directory_strictly_below_the_root_by_either_spelling() {
        let temp = tempfile::tempdir().unwrap();
        let root = ExportRoot::new(temp.path()).unwrap();
        let real = root.path().to_path_buf();
        for (into, parts) in [
            (real.join("a/b"), Some(vec!["a", "b"])),
            (temp.path().join("a"), Some(vec!["a"])),
            (real.join("a/./b/"), Some(vec!["a", "b"])),
            (real.clone(), None),
            (real.join("a/../b"), None),
            (PathBuf::from("relative/a"), None),
            (PathBuf::from("/elsewhere/a"), None),
        ] {
            let expected =
                parts.map(|parts| parts.into_iter().map(OsString::from).collect::<Vec<_>>());
            assert_eq!(root.below(&into), expected, "{}", into.display());
        }
        assert!(ExportRoot::new(Path::new("relative")).is_err());
        assert!(ExportRoot::new(&temp.path().join("absent")).is_err());
        std::fs::write(temp.path().join("file"), b"").unwrap();
        assert!(ExportRoot::new(&temp.path().join("file")).is_err());
    }
}
