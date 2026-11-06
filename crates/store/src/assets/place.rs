//! Placing one verified member outside the asset root: the export door's
//! placement by digest (RO-32). A member is never rewritten here. It is hard
//! linked when the destination shares its volume, so the two names are one
//! inode, and otherwise copied and verified against the same digest. Whoever
//! holds a linked name must never open it for writing: it replaces it by
//! rename, or it corrupts the asset store's own file.

use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use rosetta_core::timeline::AssetRef;

/// How a member reached its destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placed {
    /// A hard link: the destination is the member's own inode.
    Link,
    /// A file of its own, verified against the member's digest.
    Copy,
}

/// Why nothing was placed. A destination is never left half-written.
#[derive(Debug)]
pub enum PlaceError {
    /// The member is not in the asset root.
    SourceMissing,
    /// The member is not the file its reference records; the reason reads
    /// after "the member", as in "failed its content hash".
    SourceInvalid(&'static str),
    /// A different file already stands at the destination. It is untouched.
    Conflict,
    /// The destination is a symbolic link, a directory or another non-file.
    NotAFile,
    Io(io::Error),
}

impl std::fmt::Display for PlaceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SourceMissing => formatter.write_str("the member is missing"),
            Self::SourceInvalid(reason) => write!(formatter, "the member {reason}"),
            Self::Conflict => formatter.write_str("a different file stands at the destination"),
            Self::NotAFile => formatter.write_str("the destination is not a regular file"),
            Self::Io(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for PlaceError {}

impl From<io::Error> for PlaceError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// One member opened and held to its reference: its recorded length and its
/// BLAKE3 digest, read from the open file before anything is placed. Placing
/// it links or copies exactly that file.
#[derive(Debug)]
pub struct VerifiedMember {
    path: PathBuf,
    file: File,
    len: u64,
    digest: String,
    identity: Option<(u64, u64)>,
}

impl VerifiedMember {
    /// Open `source`, a regular file and never a link, and verify it against
    /// `expected`. A member that fails places nothing, so a caller verifies
    /// first and creates directories after.
    pub fn open(source: &Path, expected: &AssetRef) -> Result<Self, PlaceError> {
        let named = fs::symlink_metadata(source).map_err(missing)?;
        if !named.file_type().is_file() {
            return Err(PlaceError::SourceInvalid("is not a regular file"));
        }
        let file = File::open(source).map_err(missing)?;
        let opened = file.metadata()?;
        if !opened.is_file() || identity(&opened) != identity(&named) {
            return Err(PlaceError::SourceInvalid("changed while it was opened"));
        }
        if opened.len() != expected.byte_len {
            return Err(PlaceError::SourceInvalid("has the wrong size"));
        }
        let (len, digest) = digest_of(&file, expected.byte_len)?;
        if len != expected.byte_len {
            return Err(PlaceError::SourceInvalid("has the wrong size"));
        }
        if digest != expected.digest {
            return Err(PlaceError::SourceInvalid("failed its content hash"));
        }
        Ok(Self {
            path: source.to_owned(),
            file,
            len,
            digest,
            identity: identity(&opened),
        })
    }

    /// Place this member at `destination`: a hard link, or a verified copy
    /// when the link is refused (another volume, a filesystem without hard
    /// links, a link count at its limit). What already stands there decides
    /// first: this member's inode, or a file of the same bytes, answers as
    /// placed; anything else is refused and left alone.
    pub fn place(&self, destination: &Path) -> Result<Placed, PlaceError> {
        self.place_with(destination, hard_link)
    }

    fn place_with(
        &self,
        destination: &Path,
        link: impl Fn(&Path, &Path) -> io::Result<()>,
    ) -> Result<Placed, PlaceError> {
        if let Some(placed) = standing(destination, &self.expected())? {
            return Ok(placed);
        }
        match link(&self.path, destination) {
            Ok(()) => self.linked(destination),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                let found = standing(destination, &self.expected())?;
                found.ok_or(PlaceError::Io(error))
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if fs::symlink_metadata(&self.path).is_err() {
                    Err(PlaceError::SourceMissing)
                } else {
                    Err(PlaceError::Io(error))
                }
            }
            // Another volume, no hard links on this filesystem, a link count
            // at its limit or a link refused: the bytes still go, verified.
            Err(_) => self.copy_to(destination),
        }
    }

    fn expected(&self) -> Expected<'_> {
        Expected {
            len: self.len,
            digest: &self.digest,
            member: self.identity,
        }
    }

    /// A link was made: the destination must now be this member's own inode.
    /// A member renamed over between the open and the link leaves other bytes
    /// there, so the link is taken back and nothing is placed.
    fn linked(&self, destination: &Path) -> Result<Placed, PlaceError> {
        let made = fs::symlink_metadata(destination)?;
        if identity(&made) != self.identity {
            let _ = fs::remove_file(destination);
            return Err(PlaceError::SourceInvalid("changed while it was placed"));
        }
        sync_parent(destination)?;
        Ok(Placed::Link)
    }

    /// Copy the open member into a private file beside `destination`, verify
    /// what landed against the digest, and move it into place only while
    /// nothing stands there.
    fn copy_to(&self, destination: &Path) -> Result<Placed, PlaceError> {
        let mut temporary = temporary_beside(destination)?;
        (&self.file).seek(SeekFrom::Start(0))?;
        let copy = temporary.as_file_mut();
        io::copy(&mut (&self.file).take(self.len.saturating_add(1)), copy)?;
        copy.sync_all()?;
        copy.seek(SeekFrom::Start(0))?;
        let (len, digest) = digest_of(&*copy, self.len)?;
        if len != self.len || digest != self.digest {
            return Err(PlaceError::SourceInvalid("changed while it was copied"));
        }
        persist(temporary, destination, &self.expected())
    }
}

/// Write `bytes` as a new file at `destination` (0600 on unix, synced), never
/// over another file: the same bytes already there count as placed, anything
/// else is refused as [`VerifiedMember::place`] refuses it. For a document
/// the export writes rather than links (the room's shell).
pub fn place_bytes(bytes: &[u8], destination: &Path) -> Result<(), PlaceError> {
    let digest = blake3::hash(bytes).to_hex().to_string();
    let expected = Expected {
        len: bytes.len() as u64,
        digest: &digest,
        member: None,
    };
    if standing(destination, &expected)?.is_some() {
        return Ok(());
    }
    let mut temporary = temporary_beside(destination)?;
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    persist(temporary, destination, &expected)?;
    Ok(())
}

/// What a destination must hold to count as placed: the bytes (their length
/// and digest), or, for a linked member, its very inode.
struct Expected<'a> {
    len: u64,
    digest: &'a str,
    member: Option<(u64, u64)>,
}

fn hard_link(source: &Path, destination: &Path) -> io::Result<()> {
    fs::hard_link(source, destination)
}

fn missing(error: io::Error) -> PlaceError {
    if error.kind() == io::ErrorKind::NotFound {
        PlaceError::SourceMissing
    } else {
        PlaceError::Io(error)
    }
}

fn no_directory() -> PlaceError {
    PlaceError::Io(io::Error::other("the destination names no directory"))
}

/// A file's identity on its device.
#[cfg(unix)]
fn identity(metadata: &fs::Metadata) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    Some((metadata.dev(), metadata.ino()))
}

/// Elsewhere there is no identity to compare: only the bytes decide.
#[cfg(not(unix))]
fn identity(_metadata: &fs::Metadata) -> Option<(u64, u64)> {
    None
}

/// The length and BLAKE3 digest of what `reader` yields, reading at most
/// `bound + 1` bytes, so an overlong file is told apart without reading it all.
fn digest_of(reader: impl Read, bound: u64) -> io::Result<(u64, String)> {
    let mut hasher = blake3::Hasher::new();
    let len = io::copy(&mut reader.take(bound.saturating_add(1)), &mut hasher)?;
    Ok((len, hasher.finalize().to_hex().to_string()))
}

/// What stands at `destination`: nothing, the member itself (its inode), a
/// file of the expected bytes, or what a placement may not replace.
fn standing(destination: &Path, expected: &Expected<'_>) -> Result<Option<Placed>, PlaceError> {
    let named = match fs::symlink_metadata(destination) {
        Ok(named) => named,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(PlaceError::Io(error)),
    };
    if !named.file_type().is_file() {
        return Err(PlaceError::NotAFile);
    }
    if expected.member.is_some() && identity(&named) == expected.member {
        return Ok(Some(Placed::Link));
    }
    if named.len() != expected.len {
        return Err(PlaceError::Conflict);
    }
    let file = File::open(destination)?;
    if identity(&file.metadata()?) != identity(&named) {
        return Err(PlaceError::Conflict);
    }
    let (len, digest) = digest_of(&file, expected.len)?;
    if len == expected.len && digest == expected.digest {
        Ok(Some(Placed::Copy))
    } else {
        Err(PlaceError::Conflict)
    }
}

/// A private temporary file (0600 on unix) in `destination`'s directory,
/// removed unless it is persisted.
fn temporary_beside(destination: &Path) -> Result<tempfile::NamedTempFile, PlaceError> {
    let parent = destination
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(no_directory)?;
    let temporary = tempfile::Builder::new()
        .prefix(".rosetta-place-")
        .tempfile_in(parent)?;
    Ok(temporary)
}

/// Move a verified temporary file to `destination`, never over another file.
/// One that appeared meanwhile is judged as [`standing`] judges it.
fn persist(
    temporary: tempfile::NamedTempFile,
    destination: &Path,
    expected: &Expected<'_>,
) -> Result<Placed, PlaceError> {
    match temporary.persist_noclobber(destination) {
        Ok(_) => {
            sync_parent(destination)?;
            Ok(Placed::Copy)
        }
        Err(refused) => {
            let error = refused.error;
            drop(refused.file);
            if error.kind() != io::ErrorKind::AlreadyExists {
                return Err(PlaceError::Io(error));
            }
            let found = standing(destination, expected)?;
            found.ok_or(PlaceError::Io(error))
        }
    }
}

/// Make a new directory entry durable.
#[cfg(unix)]
fn sync_parent(path: &Path) -> io::Result<()> {
    match path.parent() {
        Some(parent) => File::open(parent)?.sync_all(),
        None => Ok(()),
    }
}

/// Elsewhere the platform offers no directory handle to sync.
#[cfg(not(unix))]
fn sync_parent(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io;
    use std::path::{Path, PathBuf};

    use rosetta_core::ProjectionAsset;
    use rosetta_core::html_artifacts::HTML_MEDIA_TYPE;
    use rosetta_core::timeline::AssetRef;

    use super::{PlaceError, Placed, VerifiedMember, place_bytes};

    /// One HTML member in `dir`, named by its digest as the asset root names it.
    fn member(dir: &Path, html: &str) -> (PathBuf, AssetRef) {
        let asset = ProjectionAsset::new(HTML_MEDIA_TYPE, html.as_bytes().to_vec()).unwrap();
        let path = dir.join(asset.file_name().unwrap());
        fs::write(&path, &asset.bytes).unwrap();
        (path, asset.reference())
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    fn crosses_devices(_: &Path, _: &Path) -> io::Result<()> {
        Err(io::Error::from(io::ErrorKind::CrossesDevices))
    }

    fn not_found(_: &Path, _: &Path) -> io::Result<()> {
        Err(io::Error::from(io::ErrorKind::NotFound))
    }

    #[cfg(unix)]
    fn inode(path: &Path) -> u64 {
        use std::os::unix::fs::MetadataExt;
        fs::metadata(path).unwrap().ino()
    }

    #[cfg(unix)]
    #[test]
    fn places_by_hard_link_on_one_volume() {
        let root = tempfile::tempdir().unwrap();
        let (source, reference) = member(root.path(), "<h1>Review π</h1>");
        let into = root.path().join("into");
        fs::create_dir(&into).unwrap();
        let page = into.join("index.html");

        let verified = VerifiedMember::open(&source, &reference).unwrap();
        assert_eq!(verified.place(&page).unwrap(), Placed::Link);
        assert_eq!(inode(&page), inode(&source), "one inode under two names");
        assert_eq!(fs::read(&page).unwrap(), fs::read(&source).unwrap());
        assert_eq!(
            verified.place(&page).unwrap(),
            Placed::Link,
            "the member's own inode already there is placed"
        );
        assert_eq!(names(&into), ["index.html"]);
    }

    #[test]
    fn copies_and_verifies_when_the_link_crosses_devices() {
        let root = tempfile::tempdir().unwrap();
        let (source, reference) = member(root.path(), "<h1>Review π</h1>");
        let into = root.path().join("into");
        fs::create_dir(&into).unwrap();
        let page = into.join("index.html");

        let verified = VerifiedMember::open(&source, &reference).unwrap();
        assert_eq!(
            verified.place_with(&page, crosses_devices).unwrap(),
            Placed::Copy
        );
        assert_eq!(fs::read(&page).unwrap(), fs::read(&source).unwrap());
        assert_eq!(names(&into), ["index.html"], "no temporary file is left");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_ne!(inode(&page), inode(&source), "a file of its own");
            let mode = fs::metadata(&page).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        // The same bytes already there count as placed, whichever way the
        // repeat would have gone.
        assert_eq!(
            verified.place_with(&page, crosses_devices).unwrap(),
            Placed::Copy
        );
        assert_eq!(verified.place(&page).unwrap(), Placed::Copy);
        assert_eq!(names(&into), ["index.html"]);
    }

    #[test]
    fn refuses_a_source_that_fails_its_digest_and_places_nothing() {
        let root = tempfile::tempdir().unwrap();
        let (source, reference) = member(root.path(), "<h1>Review</h1>");

        // The same length, other bytes.
        let mut tampered = fs::read(&source).unwrap();
        tampered[0] ^= 1;
        fs::write(&source, &tampered).unwrap();
        assert!(matches!(
            VerifiedMember::open(&source, &reference),
            Err(PlaceError::SourceInvalid("failed its content hash"))
        ));

        // Another length.
        fs::write(&source, b"<h1>Review</h1> and more").unwrap();
        assert!(matches!(
            VerifiedMember::open(&source, &reference),
            Err(PlaceError::SourceInvalid("has the wrong size"))
        ));

        // Not a regular file, and not there at all.
        fs::remove_file(&source).unwrap();
        fs::create_dir(&source).unwrap();
        assert!(matches!(
            VerifiedMember::open(&source, &reference),
            Err(PlaceError::SourceInvalid("is not a regular file"))
        ));
        fs::remove_dir(&source).unwrap();
        assert!(matches!(
            VerifiedMember::open(&source, &reference),
            Err(PlaceError::SourceMissing)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn a_member_reached_through_a_link_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let (source, reference) = member(root.path(), "<h1>Review</h1>");
        let linked = root.path().join("linked.html");
        std::os::unix::fs::symlink(source, &linked).unwrap();
        assert!(matches!(
            VerifiedMember::open(&linked, &reference),
            Err(PlaceError::SourceInvalid("is not a regular file"))
        ));
    }

    #[test]
    fn an_existing_different_file_is_a_conflict_and_untouched() {
        let root = tempfile::tempdir().unwrap();
        let (source, reference) = member(root.path(), "<h1>Review</h1>");
        let verified = VerifiedMember::open(&source, &reference).unwrap();
        let page = root.path().join("index.html");
        // The same length with other bytes, and another length.
        let mines: [&[u8]; 2] = [b"<h1>Mine!!</h1>", b"mine"];
        for mine in mines {
            fs::write(&page, mine).unwrap();
            assert!(matches!(verified.place(&page), Err(PlaceError::Conflict)));
            assert!(matches!(
                verified.place_with(&page, crosses_devices),
                Err(PlaceError::Conflict)
            ));
            assert_eq!(fs::read(&page).unwrap(), mine);
        }
    }

    #[test]
    fn a_symlink_or_directory_at_the_destination_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let (source, reference) = member(root.path(), "<h1>Review</h1>");
        let verified = VerifiedMember::open(&source, &reference).unwrap();

        let directory = root.path().join("directory");
        fs::create_dir(&directory).unwrap();
        assert!(matches!(
            verified.place(&directory),
            Err(PlaceError::NotAFile)
        ));
        #[cfg(unix)]
        {
            // Even a link to the member itself: a placed name is a file.
            let link = root.path().join("index.html");
            std::os::unix::fs::symlink(&source, &link).unwrap();
            assert!(matches!(verified.place(&link), Err(PlaceError::NotAFile)));
            let kept = fs::symlink_metadata(&link).unwrap();
            assert!(kept.file_type().is_symlink(), "the link is left alone");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_link_that_lands_on_other_bytes_is_taken_back() {
        let root = tempfile::tempdir().unwrap();
        let (source, reference) = member(root.path(), "<h1>Review</h1>");
        let verified = VerifiedMember::open(&source, &reference).unwrap();
        // The member's name replaced between the open and the link: the link
        // names another inode, even one with the same bytes.
        let other = root.path().join("other.html");
        fs::write(&other, fs::read(&source).unwrap()).unwrap();
        let elsewhere = |_: &Path, destination: &Path| fs::hard_link(&other, destination);
        let page = root.path().join("index.html");
        assert!(matches!(
            verified.place_with(&page, elsewhere),
            Err(PlaceError::SourceInvalid("changed while it was placed"))
        ));
        assert!(!page.exists(), "the link is taken back");
    }

    #[test]
    fn a_vanished_member_is_missing_and_a_refused_link_otherwise_an_error() {
        let root = tempfile::tempdir().unwrap();
        let (source, reference) = member(root.path(), "<h1>Review</h1>");
        let verified = VerifiedMember::open(&source, &reference).unwrap();
        let page = root.path().join("index.html");
        assert!(matches!(
            verified.place_with(&page, not_found),
            Err(PlaceError::Io(_))
        ));
        fs::remove_file(&source).unwrap();
        assert!(matches!(
            verified.place(&page),
            Err(PlaceError::SourceMissing)
        ));
        assert!(!page.exists());
    }

    #[test]
    fn written_bytes_are_placed_once_and_never_over_another_file() {
        let root = tempfile::tempdir().unwrap();
        let page = root.path().join("index.html");
        place_bytes(b"<!doctype html><p>shell</p>", &page).unwrap();
        assert_eq!(fs::read(&page).unwrap(), b"<!doctype html><p>shell</p>");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&page).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        place_bytes(b"<!doctype html><p>shell</p>", &page).unwrap();
        assert!(matches!(
            place_bytes(b"<!doctype html><p>other</p>", &page),
            Err(PlaceError::Conflict)
        ));
        assert_eq!(fs::read(&page).unwrap(), b"<!doctype html><p>shell</p>");
        assert_eq!(names(root.path()), ["index.html"]);
    }
}
