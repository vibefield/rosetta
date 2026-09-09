//! `serve --announce`: one small file per live daemon, so a sibling app can
//! find a daemon that bound an ephemeral port. The stdout `ready` line only
//! reaches the parent process; this reaches every process on the machine
//! that knows the directory. The file is removed on orderly shutdown, but a
//! reader must still treat it as a hint and probe `/api/status`: a killed
//! daemon leaves its file behind, and the pid inside may have been reused.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Bumped only if the file's shape changes incompatibly; readers that see an
/// unknown schema ignore the file.
pub const ANNOUNCEMENT_SCHEMA: &str = "rosetta-daemon/1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Announcement {
    pub schema: String,
    /// The bound loopback origin, `http://127.0.0.1:<port>`.
    pub origin: String,
    pub pid: u32,
    pub protocol_version: u32,
    /// The projection this daemon serves, absolute, so two daemons over two
    /// projections stay distinguishable.
    pub db: PathBuf,
    pub started_at: i64,
}

/// `$ROSETTA_ANNOUNCE_DIR`, else the platform's per-user state directory:
/// `~/Library/Application Support/rosetta/daemons` on macOS and
/// `$XDG_STATE_HOME/rosetta/daemons` (default `~/.local/state`) elsewhere.
/// `None` only when no home directory can be found.
pub fn default_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("ROSETTA_ANNOUNCE_DIR") {
        let dir = PathBuf::from(dir);
        if dir.is_absolute() {
            return Some(dir);
        }
    }
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    if !home.is_absolute() {
        return None;
    }
    if cfg!(target_os = "macos") {
        Some(home.join("Library/Application Support/rosetta/daemons"))
    } else {
        let state = std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .filter(|dir| dir.is_absolute())
            .unwrap_or_else(|| home.join(".local/state"));
        Some(state.join("rosetta/daemons"))
    }
}

/// Owns the announcement file for the daemon's lifetime; dropping it removes
/// the file, on the normal return path and during unwinding alike.
#[derive(Debug)]
pub struct AnnounceGuard {
    path: PathBuf,
}

impl AnnounceGuard {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for AnnounceGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Write `<dir>/<pid>.json` atomically, owner-readable only on Unix, and hand
/// back the guard that deletes it.
pub fn announce(dir: &Path, announcement: &Announcement) -> Result<AnnounceGuard, String> {
    fs::create_dir_all(dir)
        .map_err(|error| format!("creating announce directory {}: {error}", dir.display()))?;
    let path = dir.join(format!("{}.json", announcement.pid));
    let temporary = dir.join(format!(".{}.json.tmp", announcement.pid));
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temporary)
        .map_err(|error| format!("writing {}: {error}", temporary.display()))?;
    let body = serde_json::to_vec_pretty(announcement)
        .map_err(|error| format!("encoding announcement: {error}"))?;
    file.write_all(&body)
        .and_then(|()| file.write_all(b"\n"))
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("writing {}: {error}", temporary.display()))?;
    drop(file);
    fs::rename(&temporary, &path)
        .map_err(|error| format!("publishing {}: {error}", path.display()))?;
    Ok(AnnounceGuard { path })
}

/// Read one announcement; a file of another schema is not an announcement.
/// Consumers live in other languages, so in this crate only the tests read.
#[cfg(test)]
pub fn read(path: &Path) -> Result<Announcement, String> {
    let text = fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let announcement: Announcement =
        serde_json::from_str(&text).map_err(|error| format!("{}: {error}", path.display()))?;
    if announcement.schema != ANNOUNCEMENT_SCHEMA {
        return Err(format!(
            "{}: schema {} is not {ANNOUNCEMENT_SCHEMA}",
            path.display(),
            announcement.schema
        ));
    }
    Ok(announcement)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Announcement {
        Announcement {
            schema: ANNOUNCEMENT_SCHEMA.into(),
            origin: "http://127.0.0.1:53412".into(),
            pid: 4242,
            protocol_version: rosetta_server::PROTOCOL_VERSION,
            db: PathBuf::from("/tmp/projection.sqlite"),
            started_at: 1_700_000_000_000,
        }
    }

    #[test]
    fn announcement_is_written_readable_and_removed_with_its_guard() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().join("nested").join("daemons");
        let guard = announce(&root, &sample()).unwrap();
        let path = guard.path().to_path_buf();
        assert_eq!(path, root.join("4242.json"));
        assert_eq!(read(&path).unwrap(), sample());
        assert!(!root.join(".4242.json.tmp").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        drop(guard);
        assert!(!path.exists());
    }

    #[test]
    fn another_schema_is_not_an_announcement() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("1.json");
        let mut other = sample();
        other.schema = "rosetta-daemon/99".into();
        fs::write(&path, serde_json::to_vec(&other).unwrap()).unwrap();
        assert!(read(&path).is_err());
    }
}
