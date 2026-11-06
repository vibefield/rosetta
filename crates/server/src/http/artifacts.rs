//! The library needs catalog access; captured content is scoped to its source
//! session. HTML is delivered as inert bytes, never active daemon-origin HTML.
//! The export door (RO-32) places one capture by its digest, its own bytes, as
//! `index.html` in a directory another host serves from an origin of its own:
//! a hard link from the asset store when both share a volume, else a verified
//! copy. It writes the room's shell around the page only when asked
//! (`shell: "room"`). The daemon itself never serves an export.

use std::ffi::OsString;
use std::io::{ErrorKind, Read};
use std::path::Component;

use axum::extract::rejection::JsonRejection;

use super::*;
use rosetta_core::html_artifacts::{
    HTML_MAX_BYTES, HTML_MEDIA_TYPE, SHELL_HOSTS, SHELL_VERSION, runnable_document,
};
use rosetta_core::timeline::AssetRef;
use rosetta_protocol::{
    ArtifactExportRequest, ArtifactExportResponse, ArtifactListRequest, ArtifactPage,
    ExportEnvironment, ExportMethod, ExportShell, ExportedObject, ExportedShell,
};
use rosetta_store::assets::{PlaceError, Placed, VerifiedMember};
use rosetta_store::html_artifacts::{self, ArtifactEntry, ListError};

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

/// One publication's capture, and when rosetta observed it.
struct Captured {
    asset: AssetRef,
    captured_at: Option<i64>,
}

/// The capture of one publication, admitted and checked against its media
/// type and bound before any file is opened. The content read and the export
/// door share it, so both refuse exactly the same rows.
async fn captured(
    state: &AppState,
    session_id: String,
    publication_id: String,
) -> Result<Captured, Failure> {
    let hide_pending = !state.projection.is_published();
    let captured = with_read(state.projection.clone(), move |conn| {
        if behind_admission(conn, &session_id, hide_pending)? {
            return Err(Failure::not_found("artifact is not available"));
        }
        match html_artifacts::get(conn, &session_id, &publication_id).map_err(store_err)? {
            Some(ArtifactEntry {
                asset: Some(asset),
                captured_at,
                ..
            }) => Ok(Captured { asset, captured_at }),
            _ => Err(Failure::not_found("this publication has no captured HTML")),
        }
    })
    .await?;
    let asset = &captured.asset;
    asset
        .validate()
        .map_err(|e| Failure::internal(format!("invalid artifact content: {e}")))?;
    if asset.media_type != HTML_MEDIA_TYPE || asset.byte_len > HTML_MAX_BYTES as u64 {
        return Err(Failure::internal("invalid artifact media type or size"));
    }
    Ok(captured)
}

/// The capture's bytes from the asset store, held to its recorded length and
/// content hash. Blocking: the caller runs it on the blocking pool.
fn read_verified(path: &Path, asset: &AssetRef) -> Result<Vec<u8>, Failure> {
    let metadata = std::fs::symlink_metadata(path)
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
    let Captured { asset, .. } = captured(&state, session_id, publication_id).await?;
    let member = assets::html_member_path(&state.db, &asset.digest);
    let bytes = tokio::task::spawn_blocking(move || read_verified(&member, &asset))
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

/// The page's name in an export directory, so a host serving the directory
/// at `/` serves the page.
const PAGE: &str = "index.html";

/// Exports are rare and write the disk: one at a time per process, so two
/// requests never race to create one directory.
static EXPORTS: Mutex<()> = Mutex::new(());

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
    // A missing content type, malformed JSON, a missing `into` or an unknown
    // `shell` answers the documented envelope, never axum's plain-text
    // rejection.
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
    let Captured { asset, captured_at } = captured(&state, session_id, publication_id).await?;
    let member = assets::html_member_path(&state.db, &asset.digest);
    let shell = request.shell;
    let placement = tokio::task::spawn_blocking(move || match shell {
        None => place_page(root.path(), &parts, &member, &asset),
        Some(ExportShell::Room) => write_room_shell(root.path(), &parts, &member, &asset),
    })
    .await
    .map_err(|e| Failure::internal(format!("artifact export failed: {e}")))??;
    Ok(Json(ArtifactExportResponse {
        page: placement.page,
        members: Vec::new(),
        captured_at,
        method: placement.method,
        environment: ExportEnvironment::room(),
        shell: placement.shell,
    }))
}

/// What an export placed at its page's path.
struct Placement {
    page: ExportedObject,
    method: ExportMethod,
    shell: Option<ExportedShell>,
}

/// The default: the capture's own bytes, linked or copied
/// ([`VerifiedMember::place`]). The capture is verified before any directory
/// is made, so one that fails its size or hash leaves nothing behind.
/// Blocking.
fn place_page(
    root: &Path,
    parts: &[OsString],
    member: &Path,
    asset: &AssetRef,
) -> Result<Placement, Failure> {
    let source = VerifiedMember::open(member, asset).map_err(place_failure)?;
    let _one = EXPORTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let into = prepare_into(root, parts)?;
    let method = match source.place(&into.join(PAGE)).map_err(place_failure)? {
        Placed::Link => ExportMethod::Link,
        Placed::Copy => ExportMethod::Copy,
    };
    Ok(Placement {
        page: ExportedObject {
            path: PAGE.to_owned(),
            blake3: asset.digest.clone(),
            bytes: asset.byte_len,
        },
        method,
        shell: None,
    })
}

/// `shell: "room"`: the room's shell written around the capture, a file of
/// its own and never over another file. The capture is read and verified
/// first, then decoded as the room's `TextDecoder` decodes it: UTF-8 with
/// replacement, a leading byte-order mark consumed. Blocking.
fn write_room_shell(
    root: &Path,
    parts: &[OsString],
    member: &Path,
    asset: &AssetRef,
) -> Result<Placement, Failure> {
    let bytes = read_verified(member, asset)?;
    let text = String::from_utf8_lossy(&bytes);
    let document = runnable_document(text.strip_prefix('\u{feff}').unwrap_or(&text));
    let _one = EXPORTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let into = prepare_into(root, parts)?;
    assets::place_bytes(document.as_bytes(), &into.join(PAGE)).map_err(place_failure)?;
    Ok(Placement {
        page: ExportedObject {
            path: PAGE.to_owned(),
            blake3: blake3::hash(document.as_bytes()).to_hex().to_string(),
            bytes: document.len() as u64,
        },
        method: ExportMethod::Copy,
        shell: Some(ExportedShell {
            version: SHELL_VERSION.to_owned(),
            source_blake3: asset.digest.clone(),
            hosts: SHELL_HOSTS.all().into_iter().map(str::to_owned).collect(),
        }),
    })
}

/// A placement's refusal as the door answers it.
fn place_failure(error: PlaceError) -> Failure {
    match error {
        PlaceError::SourceMissing => Failure::not_found("captured HTML is unavailable"),
        PlaceError::SourceInvalid(reason) => Failure::internal(format!("captured HTML {reason}")),
        PlaceError::Conflict => export_conflict("a different file already stands at index.html"),
        PlaceError::NotAFile => Failure::bad_request("index.html is a link or a directory"),
        PlaceError::Io(error) => io_failure("placing the page", error),
    }
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

/// The export directory `root/parts…`, walked one component at a time from
/// the root. A missing one is created 0700 and its parent synced. A symbolic
/// link on the way, or as the directory itself, is refused (400), and so is a
/// file on the way; a file where the directory belongs is a conflict (409).
/// Called under [`EXPORTS`]. Blocking.
fn prepare_into(root: &Path, parts: &[OsString]) -> Result<PathBuf, Failure> {
    let mut path = root.to_path_buf();
    for (at, part) in parts.iter().enumerate() {
        let last = at + 1 == parts.len();
        path.push(part);
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(Failure::bad_request(
                    "the export directory passes through a symbolic link",
                ));
            }
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) if last => return Err(export_conflict("the export directory is a file")),
            Ok(_) => {
                return Err(Failure::bad_request(
                    "the export directory passes through a file",
                ));
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {
                private_dir(&path).map_err(|e| io_failure("creating a directory", e))?;
                sync_dir(path.parent().unwrap_or(root))
                    .map_err(|e| io_failure("syncing a directory", e))?;
            }
            Err(error) => return Err(io_failure("reading a directory", error)),
        }
    }
    Ok(path)
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

    #[test]
    fn the_walk_creates_private_directories_and_refuses_files_and_links() {
        let temp = tempfile::tempdir().unwrap();
        let root = ExportRoot::new(temp.path()).unwrap();
        let walk = |names: &[&str]| {
            let parts: Vec<OsString> = names.iter().map(OsString::from).collect();
            prepare_into(root.path(), &parts).map_err(|failure| failure.0)
        };

        let made = walk(&["a", "b"]).unwrap();
        assert_eq!(made, root.path().join("a").join("b"));
        assert!(made.is_dir());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for dir in [root.path().join("a"), made.clone()] {
                let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
                assert_eq!(mode, 0o700, "{}", dir.display());
            }
        }
        assert_eq!(
            walk(&["a", "b"]),
            Ok(made),
            "a second walk finds it standing"
        );

        std::fs::write(root.path().join("file"), b"mine").unwrap();
        assert_eq!(
            walk(&["file"]),
            Err(StatusCode::CONFLICT),
            "a file where the directory belongs"
        );
        assert_eq!(
            walk(&["file", "x"]),
            Err(StatusCode::BAD_REQUEST),
            "a file on the way"
        );
        #[cfg(unix)]
        {
            let elsewhere = tempfile::tempdir().unwrap();
            std::os::unix::fs::symlink(elsewhere.path(), root.path().join("link")).unwrap();
            let cases: [&[&str]; 2] = [&["link"], &["link", "x"]];
            for names in cases {
                assert_eq!(walk(names), Err(StatusCode::BAD_REQUEST), "{names:?}");
            }
            assert_eq!(std::fs::read_dir(elsewhere.path()).unwrap().count(), 0);
        }
    }
}
