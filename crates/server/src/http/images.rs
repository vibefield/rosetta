//! Image URLs expose only committed assets or exact canonical file targets.
//! Work stays on a bounded blocking lane; the browser never buffers originals
//! merely to draw a thumbnail. Live-file cache entries contain WebP bytes only.
use super::Failure;
use axum::{
    body::Body,
    http::{HeaderValue, header},
    response::Response,
};
use rosetta_core::assets::{IMAGE_RECIPE_VERSION, ImageVariant};
use rosetta_media::{MAX_INPUT_BYTES, PreparedImage};
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, Weak},
    time::SystemTime,
};

#[derive(Default, Deserialize)]
pub(super) struct ImageQuery {
    pub image: Option<ImageVariant>,
    pub v: Option<u32>,
    pub retry: Option<u64>,
}
impl ImageQuery {
    pub fn variant(&self) -> Result<ImageVariant, Failure> {
        if self.v.is_some_and(|v| v != IMAGE_RECIPE_VERSION) {
            return Err(Failure::bad_request("unsupported image recipe"));
        }
        Ok(self.image.unwrap_or_default())
    }
}

#[derive(Clone, PartialEq, Eq)]
struct Stamp {
    len: u64,
    modified: Option<SystemTime>,
    identity: (u64, u64, i64, i64),
}
fn stamp(metadata: &fs::Metadata) -> Stamp {
    #[cfg(unix)]
    let identity = {
        use std::os::unix::fs::MetadataExt;
        (
            metadata.dev(),
            metadata.ino(),
            metadata.ctime(),
            metadata.ctime_nsec(),
        )
    };
    #[cfg(not(unix))]
    let identity = (0, 0, 0, 0);
    Stamp {
        len: metadata.len(),
        modified: metadata.modified().ok(),
        identity,
    }
}

pub(crate) struct ImageCache {
    root: PathBuf,
    live: PathBuf,
    pub(super) workers: Arc<tokio::sync::Semaphore>,
    locks: Mutex<BTreeMap<PathBuf, Weak<Mutex<()>>>>,
    disk: Mutex<()>,
    files: Mutex<BTreeMap<PathBuf, (Stamp, String)>>,
}

impl ImageCache {
    pub(crate) fn new(db: &Path) -> Self {
        let root = rosetta_store::assets::asset_root(db);
        Self {
            live: root.join(format!(".live-images-v{IMAGE_RECIPE_VERSION}")),
            root,
            workers: Arc::new(tokio::sync::Semaphore::new(4)),
            locks: Mutex::default(),
            disk: Mutex::default(),
            files: Mutex::default(),
        }
    }

    fn lock(&self, key: &Path) -> Arc<Mutex<()>> {
        let mut locks = self.locks.lock().unwrap_or_else(|p| p.into_inner());
        locks.retain(|_, lock| lock.strong_count() > 0);
        if let Some(lock) = locks.get(key).and_then(Weak::upgrade) {
            return lock;
        }
        let lock = Arc::new(Mutex::new(()));
        locks.insert(key.to_owned(), Arc::downgrade(&lock));
        lock
    }

    pub(super) fn asset(
        &self,
        name: &str,
        variant: ImageVariant,
        retry: bool,
    ) -> Result<Response, Failure> {
        let path = self.root.join(name);
        let lock = self.lock(&path);
        let _guard = lock.lock().unwrap_or_else(|p| p.into_inner());
        check_regular(
            &fs::symlink_metadata(&path)
                .map_err(|_| Failure::not_found("image base is unavailable"))?,
        )?;
        let digest = &name[..64]; // the caller checked the closed asset name grammar
        let variant_path = self.root.join(variant.file_name(digest));
        if !retry
            && variant != ImageVariant::Full
            && let Ok(bytes) = read_webp(&variant_path)
        {
            return Ok(response(bytes, true));
        }
        let bytes = read_regular(&path)?;
        if blake3::hash(&bytes).to_hex().as_str() != digest {
            return Err(Failure::internal(
                "projection image failed its content hash",
            ));
        }
        if variant == ImageVariant::Full && name.ends_with(".webp") {
            return Ok(response(bytes, true));
        }
        // Legacy image URLs and an evicted rendition are repaired from the base
        // member. Normal ingestion creates all renditions from original pixels;
        // ledger replay recreates those exact originals if a full rebuild is needed.
        let prepared = rosetta_media::prepare(&bytes).map_err(media_error)?;
        for member in &prepared.asset.image_variants {
            write_cache(
                &self.root.join(member.variant.file_name(digest)),
                &member.bytes,
            )?;
        }
        let selected = selected(&prepared, variant);
        Ok(response(selected, true))
    }

    pub(super) fn file(
        &self,
        path: &Path,
        variant: ImageVariant,
        retry: bool,
    ) -> Result<Response, Failure> {
        let lock = self.lock(path);
        let _guard = lock.lock().unwrap_or_else(|p| p.into_inner());
        let file = fs::File::open(path)
            .map_err(|_| Failure::not_found("the recorded file is no longer available"))?;
        let metadata = file.metadata().map_err(io_error)?;
        check_regular(&metadata)?;
        let current = stamp(&metadata);
        let known = self
            .files
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(path)
            .cloned();
        if !retry
            && let Some((old, digest)) = known
            && old == current
        {
            let cached = self
                .live
                .join(&digest)
                .join(format!("{}.webp", variant.name()));
            if let Ok(bytes) = read_webp(&cached) {
                return Ok(response(bytes, false));
            }
        }
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        (&file)
            .take(MAX_INPUT_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(io_error)?;
        if bytes.len() > MAX_INPUT_BYTES {
            return Err(Failure::bad_request("the recorded image is too large"));
        }
        if file
            .metadata()
            .map(|m| stamp(&m) != current)
            .unwrap_or(true)
        {
            return Err(Failure::bad_request(
                "the recorded image changed while reading; retry",
            ));
        }
        let digest = blake3::hash(&bytes).to_hex().to_string();
        let directory = self.live.join(&digest);
        let selected_path = directory.join(format!("{}.webp", variant.name()));
        let selected_bytes = if !retry && let Ok(cached) = read_webp(&selected_path) {
            cached
        } else {
            let prepared = match rosetta_media::prepare(&bytes) {
                Ok(prepared) => prepared,
                Err(rosetta_media::MediaError::Unsupported) => return browser_fallback(bytes),
                Err(error) => return Err(media_error(error)),
            };
            let _disk = self.disk.lock().unwrap_or_else(|p| p.into_inner());
            write_cache(&directory.join("full.webp"), &prepared.asset.bytes)?;
            for member in &prepared.asset.image_variants {
                write_cache(
                    &directory.join(format!("{}.webp", member.variant.name())),
                    &member.bytes,
                )?;
            }
            self.prune_live(&directory)?;
            selected(&prepared, variant)
        };
        let mut files = self.files.lock().unwrap_or_else(|p| p.into_inner());
        if files.len() >= 512 {
            files.clear();
        }
        files.insert(path.to_owned(), (current, digest));
        Ok(response(selected_bytes, false))
    }

    /// Bound obsolete versions of mutable tool files. Canonical projection
    /// members are outside this directory and are never evicted here.
    fn prune_live(&self, keep: &Path) -> Result<(), Failure> {
        let mut entries = Vec::new();
        let mut total = 0u64;
        for entry in fs::read_dir(&self.live).map_err(io_error)? {
            let entry = entry.map_err(io_error)?;
            if !entry.file_type().map_err(io_error)?.is_dir() {
                continue;
            }
            let size = fs::read_dir(entry.path())
                .map_err(io_error)?
                .filter_map(Result::ok)
                .filter_map(|e| e.metadata().ok())
                .filter(|m| m.is_file())
                .map(|m| m.len())
                .sum::<u64>();
            total += size;
            entries.push((
                entry.metadata().and_then(|m| m.modified()).ok(),
                entry.path(),
                size,
            ));
        }
        entries.sort_by_key(|entry| entry.0);
        let mut count = entries.len();
        for (_, path, size) in entries {
            if total <= 512 * 1024 * 1024 && count <= 512 {
                break;
            }
            if path == keep {
                continue;
            }
            if fs::remove_dir_all(&path).is_ok() {
                total -= size;
                count -= 1;
            }
        }
        Ok(())
    }
}

fn selected(prepared: &PreparedImage, variant: ImageVariant) -> Vec<u8> {
    prepared
        .asset
        .image_variants
        .iter()
        .find(|v| v.variant == variant)
        .map_or_else(|| prepared.asset.bytes.clone(), |v| v.bytes.clone())
}
fn check_regular(metadata: &fs::Metadata) -> Result<(), Failure> {
    if !metadata.is_file() {
        return Err(Failure::not_found("the recorded target is not a file"));
    }
    if metadata.len() > MAX_INPUT_BYTES as u64 {
        return Err(Failure::bad_request("the image is too large to open here"));
    }
    Ok(())
}
fn read_regular(path: &Path) -> Result<Vec<u8>, Failure> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| Failure::not_found("image cache member is unavailable"))?;
    if !metadata.file_type().is_file() {
        return Err(Failure::not_found(
            "image cache member is not a regular file",
        ));
    }
    check_regular(&metadata)?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    fs::File::open(path)
        .map_err(io_error)?
        .take(MAX_INPUT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(io_error)?;
    if bytes.len() > MAX_INPUT_BYTES {
        return Err(Failure::bad_request("image is too large"));
    }
    Ok(bytes)
}
fn read_webp(path: &Path) -> Result<Vec<u8>, Failure> {
    let bytes = read_regular(path)?;
    if !bytes.starts_with(b"RIFF") || bytes.get(8..12) != Some(b"WEBP") {
        return Err(Failure::internal("image cache member is invalid"));
    }
    Ok(bytes)
}
fn write_cache(path: &Path, bytes: &[u8]) -> Result<(), Failure> {
    use std::io::Write;
    let parent = path
        .parent()
        .ok_or_else(|| Failure::internal("invalid cache path"))?;
    fs::create_dir_all(parent).map_err(io_error)?;
    let mut temporary = tempfile::Builder::new()
        .prefix(".image-")
        .tempfile_in(parent)
        .map_err(io_error)?;
    temporary.write_all(bytes).map_err(io_error)?;
    temporary.persist(path).map_err(|e| io_error(e.error))?;
    Ok(())
}
fn response(bytes: Vec<u8>, immutable: bool) -> Response {
    let digest = blake3::hash(&bytes).to_hex();
    let mut response = Response::new(Body::from(bytes));
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("image/webp"));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::ETAG,
        HeaderValue::from_str(&format!("\"{digest}\"")).expect("hex ETag"),
    );
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(if immutable {
            "private, max-age=31536000, immutable"
        } else {
            "private, no-cache"
        }),
    );
    response
}
fn media_error(error: rosetta_media::MediaError) -> Failure {
    Failure::bad_request(error.to_string())
}
fn io_error(error: std::io::Error) -> Failure {
    Failure::internal(format!("image cache: {error}"))
}

// SVG and AVIF were browser-readable tool sources before the raster pipeline.
// Do not add a native codec dependency or copy their originals into the cache.
fn browser_fallback(bytes: Vec<u8>) -> Result<Response, Failure> {
    let prefix = &bytes[..bytes.len().min(4096)];
    let svg = std::str::from_utf8(prefix).ok().is_some_and(|text| {
        let text = text.trim_start_matches('\u{feff}').trim_start();
        (text.starts_with("<svg") || text.starts_with("<?xml")) && text.contains("<svg")
    });
    let avif = prefix.get(4..8) == Some(b"ftyp")
        && prefix.get(8..).is_some_and(|brands| {
            brands
                .chunks_exact(4)
                .take(16)
                .any(|brand| matches!(brand, b"avif" | b"avis"))
        });
    let mime = if svg {
        "image/svg+xml"
    } else if avif {
        "image/avif"
    } else {
        return Err(media_error(rosetta_media::MediaError::Unsupported));
    };
    let mut response = response(bytes, false);
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(mime));
    response.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("sandbox; default-src 'none'; style-src 'unsafe-inline'"),
    );
    Ok(response)
}

pub(super) fn conditional(mut response: Response, request: &axum::http::HeaderMap) -> Response {
    if let Some(etag) = response
        .headers()
        .get(header::ETAG)
        .and_then(|v| v.to_str().ok())
    {
        let matches = request
            .get(header::IF_NONE_MATCH)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|value| {
                value.split(',').any(|candidate| {
                    let candidate = candidate
                        .trim()
                        .strip_prefix("W/")
                        .unwrap_or(candidate.trim());
                    candidate == etag || candidate == "*"
                })
            });
        if matches {
            *response.status_mut() = axum::http::StatusCode::NOT_MODIFIED;
            *response.body_mut() = Body::empty();
        }
    }
    response
}
