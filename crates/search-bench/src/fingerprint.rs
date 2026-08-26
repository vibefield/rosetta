use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::CAMPAIGN_FORMAT;
use crate::model::{
    ArtifactFingerprint, CampaignManifest, FileFingerprint, LedgerFingerprint, QuerySuite,
};

const HASH_BUFFER_BYTES: usize = 1024 * 1024;

#[cfg(unix)]
fn is_read_only(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o222 == 0
}

#[cfg(not(unix))]
fn is_read_only(metadata: &fs::Metadata) -> bool {
    metadata.permissions().readonly()
}

fn require_read_only(path: &Path, metadata: &fs::Metadata) -> Result<(), String> {
    if is_read_only(metadata) {
        Ok(())
    } else {
        Err(format!(
            "{} is writable; freeze a campaign copy and remove write permissions first",
            path.display()
        ))
    }
}

fn hash_reader(mut reader: impl Read) -> Result<(u64, String), String> {
    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0_u8; HASH_BUFFER_BYTES];
    let mut bytes = 0_u64;
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|error| error.to_string())?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        bytes = bytes
            .checked_add(read as u64)
            .ok_or("artifact length overflow")?;
    }
    Ok((bytes, hasher.finalize().to_hex().to_string()))
}

pub(crate) fn fingerprint_file(
    path: &Path,
    must_be_read_only: bool,
) -> Result<ArtifactFingerprint, String> {
    let before = fs::symlink_metadata(path)
        .map_err(|error| format!("stating {}: {error}", path.display()))?;
    if before.file_type().is_symlink() || !before.is_file() {
        return Err(format!("{} is not a regular file", path.display()));
    }
    if must_be_read_only {
        require_read_only(path, &before)?;
    }
    let file = File::open(path).map_err(|error| format!("opening {}: {error}", path.display()))?;
    let (bytes, blake3) = hash_reader(BufReader::with_capacity(HASH_BUFFER_BYTES, file))?;
    let after = fs::symlink_metadata(path)
        .map_err(|error| format!("restating {}: {error}", path.display()))?;
    if before.len() != after.len() || after.len() != bytes {
        return Err(format!("{} changed while it was hashed", path.display()));
    }
    Ok(ArtifactFingerprint { bytes, blake3 })
}

fn collect_files(root: &Path, directory: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let metadata = fs::symlink_metadata(directory)
        .map_err(|error| format!("stating {}: {error}", directory.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(format!(
            "{} is not a regular directory",
            directory.display()
        ));
    }
    require_read_only(directory, &metadata)?;

    let mut entries = fs::read_dir(directory)
        .map_err(|error| format!("listing {}: {error}", directory.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("listing {}: {error}", directory.display()))?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| format!("stating {}: {error}", path.display()))?;
        if metadata.file_type().is_symlink() {
            return Err(format!("ledger contains symlink {}", path.display()));
        }
        if metadata.is_dir() {
            collect_files(root, &path, out)?;
        } else if metadata.is_file() {
            require_read_only(&path, &metadata)?;
            out.push(path);
        } else {
            return Err(format!(
                "ledger contains non-file artifact {}",
                path.display()
            ));
        }
    }
    // Keep the argument meaningful and catch a future accidental recursion
    // outside the campaign root.
    directory
        .strip_prefix(root)
        .map_err(|_| format!("{} escaped ledger root", directory.display()))?;
    Ok(())
}

pub(crate) fn fingerprint_ledger(root: &Path) -> Result<LedgerFingerprint, String> {
    let canonical_root = root
        .canonicalize()
        .map_err(|error| format!("resolving {}: {error}", root.display()))?;
    let mut paths = Vec::new();
    collect_files(&canonical_root, &canonical_root, &mut paths)?;
    if paths.is_empty() {
        return Err(format!("{} contains no ledger files", root.display()));
    }

    let mut files = Vec::with_capacity(paths.len());
    let mut total = 0_u64;
    for path in paths {
        let artifact = fingerprint_file(&path, true)?;
        let relative = path
            .strip_prefix(&canonical_root)
            .map_err(|_| format!("{} escaped ledger root", path.display()))?
            .to_str()
            .ok_or_else(|| format!("{} is not valid UTF-8", path.display()))?
            .replace(std::path::MAIN_SEPARATOR, "/");
        total = total
            .checked_add(artifact.bytes)
            .ok_or("ledger length overflow")?;
        files.push(FileFingerprint {
            relative_path: relative,
            bytes: artifact.bytes,
            blake3: artifact.blake3,
        });
    }
    files.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));

    let mut hasher = blake3::Hasher::new();
    hasher.update(b"rosetta-frozen-ledger-v1\0");
    for file in &files {
        hasher.update(&(file.relative_path.len() as u64).to_be_bytes());
        hasher.update(file.relative_path.as_bytes());
        hasher.update(&file.bytes.to_be_bytes());
        hasher.update(file.blake3.as_bytes());
    }
    Ok(LedgerFingerprint {
        files,
        bytes: total,
        blake3: hasher.finalize().to_hex().to_string(),
    })
}

pub fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T, String> {
    let file = File::open(path).map_err(|error| format!("opening {}: {error}", path.display()))?;
    serde_json::from_reader(BufReader::new(file))
        .map_err(|error| format!("parsing {}: {error}", path.display()))
}

pub fn write_json_new(path: &Path, value: &impl Serialize) -> Result<(), String> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .map_err(|error| format!("creating {}: {error}", parent.display()))?;
    }
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| format!("creating {}: {error}", path.display()))?;
    let mut writer = BufWriter::new(file);
    serde_json::to_writer_pretty(&mut writer, value)
        .map_err(|error| format!("serializing {}: {error}", path.display()))?;
    writer
        .write_all(b"\n")
        .and_then(|_| writer.flush())
        .map_err(|error| format!("writing {}: {error}", path.display()))?;
    writer
        .get_ref()
        .sync_all()
        .map_err(|error| format!("syncing {}: {error}", path.display()))
}

fn load_suite(path: &Path) -> Result<(QuerySuite, ArtifactFingerprint), String> {
    let artifact = fingerprint_file(path, false)?;
    let suite: QuerySuite = read_json(path)?;
    suite.validate()?;
    Ok((suite, artifact))
}

fn campaign_id(ledger: &LedgerFingerprint, suite: &ArtifactFingerprint) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"rosetta-search-campaign-v1\0");
    hasher.update(ledger.blake3.as_bytes());
    hasher.update(suite.blake3.as_bytes());
    hasher.finalize().to_hex().to_string()
}

pub fn freeze_campaign(ledger_root: &Path, query_suite: &Path) -> Result<CampaignManifest, String> {
    let ledger = fingerprint_ledger(ledger_root)?;
    let (suite, query_suite) = load_suite(query_suite)?;
    Ok(CampaignManifest {
        format: CAMPAIGN_FORMAT,
        campaign_id: campaign_id(&ledger, &query_suite),
        ledger,
        query_suite,
        suite_name: suite.name,
        suite_basis: suite.basis,
    })
}

pub fn verify_campaign(
    manifest: &CampaignManifest,
    ledger_root: &Path,
    query_suite: &Path,
) -> Result<QuerySuite, String> {
    manifest.validate_format()?;
    let ledger = fingerprint_ledger(ledger_root)?;
    if ledger != manifest.ledger {
        return Err("ledger fingerprint differs from the frozen campaign".into());
    }
    let (suite, suite_fingerprint) = load_suite(query_suite)?;
    if suite_fingerprint != manifest.query_suite
        || suite.name != manifest.suite_name
        || suite.basis != manifest.suite_basis
    {
        return Err("query suite differs from the frozen campaign".into());
    }
    if campaign_id(&ledger, &suite_fingerprint) != manifest.campaign_id {
        return Err("campaign id does not match its frozen artifacts".into());
    }
    Ok(suite)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use tempfile::tempdir;

    use super::{freeze_campaign, verify_campaign};

    #[cfg(unix)]
    fn set_mode(path: &Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }

    #[cfg(not(unix))]
    fn set_mode(path: &Path, _mode: u32) {
        let mut permissions = fs::metadata(path).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(path, permissions).unwrap();
    }

    #[test]
    fn campaign_requires_and_rechecks_a_read_only_ledger() {
        let temp = tempdir().unwrap();
        let ledger = temp.path().join("ledger");
        fs::create_dir(&ledger).unwrap();
        let segment = ledger.join("000.rlg");
        fs::write(&segment, b"raw").unwrap();
        let suite = temp.path().join("suite.json");
        fs::write(
            &suite,
            r#"{"format":1,"name":"safe","basis":"diagnostic","cases":[{"label":"broad","class":"broad","query":"one"}]}"#,
        )
        .unwrap();

        assert!(freeze_campaign(&ledger, &suite).is_err());
        set_mode(&segment, 0o444);
        set_mode(&ledger, 0o555);
        let manifest = freeze_campaign(&ledger, &suite).unwrap();
        assert_eq!(
            verify_campaign(&manifest, &ledger, &suite).unwrap().name,
            "safe"
        );

        set_mode(&ledger, 0o755);
        set_mode(&segment, 0o644);
        fs::write(&segment, b"changed").unwrap();
        set_mode(&segment, 0o444);
        set_mode(&ledger, 0o555);
        assert!(verify_campaign(&manifest, &ledger, &suite).is_err());

        // TempDir cleanup needs the directory writable again.
        set_mode(&ledger, 0o755);
        set_mode(&segment, 0o644);
    }
}
