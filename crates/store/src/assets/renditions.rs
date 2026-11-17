//! The disposable WebP display cache beside a projection's image members.
//!
//! A base member `<digest>.<ext>` (a recorded original, or the Full WebP of a
//! legacy bundle) owns renditions named `<digest>.image-v<recipe>-<variant>.webp`
//! ([`ImageVariant::file_name`]). SQLite never references them and they are
//! never fsynced: losing one loses nothing, because the next view or the
//! background sweep makes the same bytes again from the base. Every display
//! rendition is a WebP that rosetta-media made from the base's pixels, so a
//! renderer never decodes a recorded original.
//!
//! - Thumbs (rail, tile2x, tile) come from one decode and are written as one
//!   set, tile last.
//! - Preview needs no downscale when the oriented long edge is at most
//!   1600 px ([`rosetta_media::preview_is_full`]). It is then Full's encoding,
//!   stored once: the preview and full names are hard links to one file.
//! - Full (the 1:1 view) of a larger image is encoded on its first request.
//!
//! Reads check the RIFF header against the file length, so a torn or short
//! file is a miss. Writes go to an `.image-` tempfile renamed into place;
//! [`remove_stale_tempfiles`] reclaims what a crash left behind.
//!
//! One process-wide lock per base serializes the encodes of one image. A
//! foreground request takes the lock and then waits for a codec slot; the
//! background sweep holds a slot first and skips a locked image, so neither
//! ever waits for the other.

use std::collections::BTreeMap;
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, TryLockError, Weak};
use std::time::{Duration, SystemTime};

use rosetta_core::assets::{IMAGE_RECIPE_VERSION, ImageVariant, ImageVariantBytes};
use rosetta_core::timeline::Dimensions;
use rosetta_media::{CodecPermit, Lane, MAX_INPUT_BYTES, MediaError, RenditionSet};

/// Image member extensions whose bases own renditions.
pub const IMAGE_EXTENSIONS: [&str; 6] = ["png", "jpg", "gif", "webp", "bmp", "ico"];

const TEMP_PREFIX: &str = ".image-";
const THUMBS: [ImageVariant; 3] = [ImageVariant::Rail, ImageVariant::Tile2x, ImageVariant::Tile];
const VARIANTS: [ImageVariant; 5] = [
    ImageVariant::Full,
    ImageVariant::Rail,
    ImageVariant::Tile,
    ImageVariant::Tile2x,
    ImageVariant::Preview,
];

/// `(digest, extension)` of an image base member name.
pub fn image_base(name: &str) -> Option<(&str, &str)> {
    let (digest, extension) = name.split_once('.')?;
    (is_digest(digest) && IMAGE_EXTENSIONS.contains(&extension)).then_some((digest, extension))
}

/// A rendition cache file name: its base digest, recipe version and variant
/// (`None` for a variant name this build does not know).
pub fn parse_cache_name(name: &str) -> Option<(&str, u32, Option<ImageVariant>)> {
    let (digest, rest) = name.split_once('.')?;
    let (version, variant) = rest
        .strip_prefix("image-v")?
        .strip_suffix(".webp")?
        .split_once('-')?;
    if !is_digest(digest) || version.is_empty() || !version.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let known = VARIANTS.into_iter().find(|v| v.name() == variant);
    Some((digest, version.parse().ok()?, known))
}

/// True for a cache file name of the current recipe.
pub fn is_current_cache_name(name: &str) -> bool {
    parse_cache_name(name)
        .is_some_and(|(_, version, variant)| version == IMAGE_RECIPE_VERSION && variant.is_some())
}

pub(crate) fn is_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[derive(Debug)]
pub enum RenditionError {
    /// No regular base member has that name.
    BaseMissing,
    /// The base member's bytes do not hash to its name; they are never decoded.
    BaseCorrupt,
    /// The base is no image the kernel renders, exceeds a limit or has
    /// unusable pixels; `MediaError::Stopped` when the caller stopped the work.
    Media(MediaError),
    /// The cache directory could not be read or written.
    Io(io::Error),
}
impl fmt::Display for RenditionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BaseMissing => f.write_str("image base is unavailable"),
            Self::BaseCorrupt => f.write_str("projection image failed its content hash"),
            Self::Media(error) => error.fmt(f),
            Self::Io(error) => write!(f, "image cache: {error}"),
        }
    }
}
impl std::error::Error for RenditionError {}
impl From<MediaError> for RenditionError {
    fn from(error: MediaError) -> Self {
        Self::Media(error)
    }
}

/// One served rendition.
#[derive(Debug)]
pub struct Rendition {
    pub bytes: Vec<u8>,
    /// Cache names this request created, a hard link included.
    pub files_written: u64,
    /// A cache write that failed. The bytes are still right; they are only
    /// not kept, so the next request encodes again.
    pub cache_error: Option<io::Error>,
}

/// What [`ensure`] did for one base.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ensured {
    /// Every rendition the sweep makes was already cached.
    Complete,
    /// A foreground request is making this image's renditions right now.
    Busy,
    /// The missing renditions were written.
    Made { files: u64, bytes: u64 },
}

/// A valid cached rendition, read without the base, a lock or a codec slot.
pub fn cached(root: &Path, digest: &str, variant: ImageVariant) -> Option<Vec<u8>> {
    read_webp(&root.join(variant.file_name(digest))).ok()
}

/// The rendition a viewer asked for, from the cache or encoded now (a
/// foreground codec job). `retry` ignores and replaces cached files. Thumbs
/// are made as a set; Preview and Full share one file when preview needs no
/// downscale, so the other name is then linked without an encode.
pub fn rendition(
    root: &Path,
    name: &str,
    variant: ImageVariant,
    retry: bool,
) -> Result<Rendition, RenditionError> {
    let (digest, _) = image_base(name).ok_or(MediaError::Unsupported)?;
    let lock = base_lock(&root.join(name));
    let _guard = lock.lock().unwrap_or_else(|p| p.into_inner());
    if !retry && let Some(bytes) = cached(root, digest, variant) {
        return Ok(Rendition {
            bytes,
            files_written: 0,
            cache_error: None,
        });
    }
    let base = read_base(root, name, digest)?;
    let shared = rosetta_media::preview_is_full(rosetta_media::probe(&base)?.dimensions);
    let mut writer = CacheWriter::new(root, digest);
    if shared && !retry && matches!(variant, ImageVariant::Preview | ImageVariant::Full) {
        let other = if variant == ImageVariant::Full {
            ImageVariant::Preview
        } else {
            ImageVariant::Full
        };
        if let Some(bytes) = cached(root, digest, other) {
            let error = writer.link(other, variant).err();
            return Ok(writer.served(bytes, error));
        }
    }
    let set = match variant {
        ImageVariant::Rail | ImageVariant::Tile | ImageVariant::Tile2x => RenditionSet::Thumbs,
        ImageVariant::Preview => RenditionSet::Preview,
        ImageVariant::Full => RenditionSet::Full,
    };
    let encoded = {
        let permit = rosetta_media::admit(Lane::Foreground);
        rosetta_media::renditions(&permit, &base, set)?
    };
    drop(base);
    let bytes = encoded
        .iter()
        .find(|v| v.variant == variant)
        .map(|v| v.bytes.clone())
        .ok_or(MediaError::Encode)?;
    let error = writer.write_set(&encoded, shared).err();
    Ok(writer.served(bytes, error))
}

/// Make whatever the sweep keeps that is missing for one base: the thumbs
/// and the preview (from one decode when both are missing), plus the full
/// name when it is the preview's own file. Full of a larger image is left to
/// its first 1:1 view. `dimensions` (the attachment's, when known) lets a
/// complete base be recognized without reading it. Returns [`Ensured::Busy`]
/// at once while a foreground request holds this image.
pub fn ensure(
    permit: &CodecPermit,
    root: &Path,
    name: &str,
    dimensions: Option<Dimensions>,
    stop: &dyn Fn() -> bool,
) -> Result<Ensured, RenditionError> {
    let (digest, _) = image_base(name).ok_or(MediaError::Unsupported)?;
    let lock = base_lock(&root.join(name));
    let _guard = match lock.try_lock() {
        Ok(guard) => guard,
        Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
        Err(TryLockError::WouldBlock) => return Ok(Ensured::Busy),
    };
    let valid = |variant: ImageVariant| cache_valid(&root.join(variant.file_name(digest)));
    let thumbs = THUMBS.into_iter().all(valid);
    let mut preview = valid(ImageVariant::Preview);
    let full = valid(ImageVariant::Full);
    if thumbs && preview && (full || dimensions.is_some_and(|d| !rosetta_media::preview_is_full(d)))
    {
        return Ok(Ensured::Complete);
    }
    let base = read_base(root, name, digest)?;
    let shared = rosetta_media::preview_is_full(rosetta_media::probe(&base)?.dimensions);
    let mut writer = CacheWriter::new(root, digest);
    if shared && preview != full {
        if preview {
            writer.link(ImageVariant::Preview, ImageVariant::Full)
        } else {
            writer.link(ImageVariant::Full, ImageVariant::Preview)
        }
        .map_err(RenditionError::Io)?;
        preview = true;
    }
    let set = match (thumbs, preview) {
        (false, false) => Some(RenditionSet::All),
        (false, true) => Some(RenditionSet::Thumbs),
        (true, false) => Some(RenditionSet::Preview),
        (true, true) => None,
    };
    if let Some(set) = set {
        let encoded = rosetta_media::renditions_with_stop(permit, &base, set, stop)?;
        drop(base);
        writer
            .write_set(&encoded, shared)
            .map_err(RenditionError::Io)?;
    }
    Ok(match writer.files {
        0 => Ensured::Complete,
        files => Ensured::Made {
            files,
            bytes: writer.bytes,
        },
    })
}

/// Whether the sweep has nothing left to make for a base, by `stat` alone:
/// thumbs and preview, plus the full name when `dimensions` (the
/// attachment's) show that preview is Full. Reads no image bytes.
pub fn is_complete(root: &Path, name: &str, dimensions: Option<Dimensions>) -> bool {
    let Some((digest, _)) = image_base(name) else {
        return false;
    };
    let present = |variant: ImageVariant| {
        fs::symlink_metadata(root.join(variant.file_name(digest)))
            .is_ok_and(|m| m.file_type().is_file() && m.len() >= 12)
    };
    THUMBS.into_iter().all(present)
        && present(ImageVariant::Preview)
        && (dimensions.is_none_or(|d| !rosetta_media::preview_is_full(d))
            || present(ImageVariant::Full))
}

/// Remove rendition tempfiles at least `age` old: what an interrupted write
/// left. Younger ones may belong to a write in flight.
pub fn remove_stale_tempfiles(root: &Path, age: Duration) -> io::Result<u64> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error),
    };
    let now = SystemTime::now();
    let mut removed = 0;
    for entry in entries {
        let entry = entry?;
        if !entry.file_name().to_string_lossy().starts_with(TEMP_PREFIX)
            || !entry.file_type()?.is_file()
        {
            continue;
        }
        let modified = entry.metadata()?.modified()?;
        if now
            .duration_since(modified)
            .is_ok_and(|elapsed| elapsed >= age)
        {
            match fs::remove_file(entry.path()) {
                Ok(()) => removed += 1,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
    }
    Ok(removed)
}

/// A complete WebP cache file: a regular file whose RIFF size matches its
/// length. Shared with the live-file cache.
pub fn read_webp(path: &Path) -> io::Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() || metadata.len() > MAX_INPUT_BYTES as u64 {
        return Err(invalid("image cache member is not a bounded regular file"));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    File::open(path)?
        .take(MAX_INPUT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if !riff_complete(&bytes, bytes.len() as u64) {
        return Err(invalid("image cache member is invalid"));
    }
    Ok(bytes)
}

/// Write a cache file through a tempfile and a rename; never synced.
/// Shared with the live-file cache.
pub fn write_cache(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| invalid("invalid cache path"))?;
    fs::create_dir_all(parent)?;
    let mut temporary = tempfile::Builder::new()
        .prefix(TEMP_PREFIX)
        .tempfile_in(parent)?;
    temporary.write_all(bytes)?;
    temporary.persist(path).map_err(|e| e.error)?;
    Ok(())
}

fn cache_valid(path: &Path) -> bool {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return false;
    };
    let mut header = [0; 12];
    metadata.file_type().is_file()
        && File::open(path)
            .and_then(|mut f| f.read_exact(&mut header))
            .is_ok()
        && riff_complete(&header, metadata.len())
}

fn riff_complete(bytes: &[u8], len: u64) -> bool {
    bytes.len() >= 12
        && bytes.starts_with(b"RIFF")
        && &bytes[8..12] == b"WEBP"
        && u64::from(u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]])) + 8 == len
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_owned())
}

/// The verified base bytes: absent or not a regular file is BaseMissing, a
/// hash mismatch is BaseCorrupt.
fn read_base(root: &Path, name: &str, digest: &str) -> Result<Vec<u8>, RenditionError> {
    let missing = |error: io::Error| match error.kind() {
        io::ErrorKind::NotFound => RenditionError::BaseMissing,
        _ => RenditionError::Io(error),
    };
    let path = root.join(name);
    let metadata = fs::symlink_metadata(&path).map_err(missing)?;
    if !metadata.file_type().is_file() {
        return Err(RenditionError::BaseMissing);
    }
    if metadata.len() > MAX_INPUT_BYTES as u64 {
        return Err(MediaError::Limit.into());
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    File::open(&path)
        .map_err(missing)?
        .take(MAX_INPUT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(RenditionError::Io)?;
    if bytes.len() > MAX_INPUT_BYTES {
        return Err(MediaError::Limit.into());
    }
    if blake3::hash(&bytes).to_hex().as_str() != digest {
        return Err(RenditionError::BaseCorrupt);
    }
    Ok(bytes)
}

fn base_lock(base: &Path) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<Mutex<BTreeMap<PathBuf, Weak<Mutex<()>>>>> = OnceLock::new();
    let mut locks = LOCKS
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    locks.retain(|_, lock| lock.strong_count() > 0);
    if let Some(lock) = locks.get(base).and_then(Weak::upgrade) {
        return lock;
    }
    let lock = Arc::new(Mutex::new(()));
    locks.insert(base.to_owned(), Arc::downgrade(&lock));
    lock
}

/// Writes one base's cache files and counts them; stops at the first error.
struct CacheWriter<'a> {
    root: &'a Path,
    digest: &'a str,
    files: u64,
    bytes: u64,
}
impl<'a> CacheWriter<'a> {
    fn new(root: &'a Path, digest: &'a str) -> Self {
        Self {
            root,
            digest,
            files: 0,
            bytes: 0,
        }
    }

    fn path(&self, variant: ImageVariant) -> PathBuf {
        self.root.join(variant.file_name(self.digest))
    }

    fn write(&mut self, variant: ImageVariant, bytes: &[u8]) -> io::Result<()> {
        write_cache(&self.path(variant), bytes)?;
        self.files += 1;
        self.bytes += bytes.len() as u64;
        Ok(())
    }

    /// Give `to` the file `from` names. A filesystem without hard links
    /// keeps a second copy instead.
    fn link(&mut self, from: ImageVariant, to: ImageVariant) -> io::Result<()> {
        let (source, destination) = (self.path(from), self.path(to));
        let linked = tempfile::Builder::new()
            .prefix(TEMP_PREFIX)
            .make_in(self.root, |path| fs::hard_link(&source, path))
            .and_then(|link| link.persist(&destination).map_err(|e| e.error));
        if linked.is_err() {
            write_cache(&destination, &read_webp(&source)?)?;
        }
        self.files += 1;
        Ok(())
    }

    /// Thumbs in write order with tile last, then the preview; a preview that
    /// is Full's encoding (and a Full that is the preview's) is one file under
    /// both names.
    fn write_set(&mut self, encoded: &[ImageVariantBytes], shared: bool) -> io::Result<()> {
        let get = |variant: ImageVariant| encoded.iter().find(|v| v.variant == variant);
        for variant in [ImageVariant::Rail, ImageVariant::Tile2x] {
            if let Some(found) = get(variant) {
                self.write(variant, &found.bytes)?;
            }
        }
        for variant in [ImageVariant::Preview, ImageVariant::Full] {
            let Some(found) = get(variant) else {
                continue;
            };
            if shared {
                self.write(ImageVariant::Preview, &found.bytes)?;
                self.link(ImageVariant::Preview, ImageVariant::Full)?;
            } else {
                self.write(variant, &found.bytes)?;
            }
        }
        if let Some(found) = get(ImageVariant::Tile) {
            self.write(ImageVariant::Tile, &found.bytes)?;
        }
        Ok(())
    }

    fn served(self, bytes: Vec<u8>, cache_error: Option<io::Error>) -> Rendition {
        Rendition {
            bytes,
            files_written: self.files,
            cache_error,
        }
    }
}

#[cfg(test)]
mod tests;
