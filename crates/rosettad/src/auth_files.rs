//! `serve`'s access policy from its flags (docs/AUTH.md): `--token-file`,
//! `--grant-key-file`, `--allow-origin` and `--debug-auth`. Both files are
//! read ONCE, before anything binds, so a supervisor may delete them after
//! the `ready` event; a bad file is a startup refusal it sees at once.

use std::path::Path;
use std::sync::Arc;

use rosetta_server::{AuthPolicy, GrantEvent};

use crate::Flags;

/// Neither secret file is ever larger than this.
const MAX_SECRET_FILE_BYTES: u64 = 4096;

/// The policy `serve` runs under: open (the daemon as it always was) unless a
/// flag says otherwise.
pub(crate) fn load_policy(flags: &Flags) -> Result<AuthPolicy, String> {
    let token = flags
        .token_file
        .as_deref()
        .map(|path| {
            let bytes = read_secret_file(path, "--token-file")?;
            String::from_utf8(bytes)
                .map_err(|_| format!("--token-file {}: the token is not UTF-8", path.display()))
        })
        .transpose()?;
    let key = flags
        .grant_key_file
        .as_deref()
        .map(|path| read_secret_file(path, "--grant-key-file"))
        .transpose()?;
    if key.is_some() && token.is_none() {
        return Err(
            "--grant-key-file needs --token-file: a grant stands in for the bearer on one \
             session's routes, so without a bearer there is nothing for it to stand in for"
                .into(),
        );
    }
    // Validated in stages, so each refusal names the flag that caused it.
    let named = |flag: &'static str| move |error: String| format!("{flag}: {error}");
    AuthPolicy::new(token.as_deref(), None, &[]).map_err(named("--token-file"))?;
    AuthPolicy::new(token.as_deref(), key.clone(), &[]).map_err(named("--grant-key-file"))?;
    let policy = AuthPolicy::new(token.as_deref(), key, &flags.allow_origins)
        .map_err(named("--allow-origin"))?;
    Ok(if flags.debug_auth {
        policy.with_observer(Arc::new(log_grant))
    } else {
        policy
    })
}

/// `--debug-auth`: one stderr line per grant decision. The client id and the
/// route template only — never the grant, its MAC, or the session id.
fn log_grant(event: GrantEvent<'_>) {
    let client = event
        .cid
        .map(|cid| format!(" client={cid:?}"))
        .unwrap_or_default();
    match event.refusal {
        None => eprintln!(
            "rosettad: auth: grant accepted{client} route={}",
            event.route
        ),
        Some(refusal) => eprintln!(
            "rosettad: auth: grant refused ({}){client} route={}",
            refusal.as_str(),
            event.route
        ),
    }
}

/// A secret file's bytes. It must be a regular file no larger than
/// [`MAX_SECRET_FILE_BYTES`] and, on Unix, carry no group or other
/// permission bits at all (mode 0600 or stricter): a secret any other
/// account can read — or replace — is no secret.
fn read_secret_file(path: &Path, flag: &str) -> Result<Vec<u8>, String> {
    let metadata =
        std::fs::metadata(path).map_err(|error| format!("{flag} {}: {error}", path.display()))?;
    if !metadata.is_file() {
        return Err(format!("{flag} {}: not a regular file", path.display()));
    }
    if metadata.len() > MAX_SECRET_FILE_BYTES {
        return Err(format!(
            "{flag} {}: {} bytes is more than a secret file holds ({MAX_SECRET_FILE_BYTES})",
            path.display(),
            metadata.len()
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = metadata.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            return Err(format!(
                "{flag} {}: mode {mode:04o} lets group or others read or write it; chmod 600 it",
                path.display()
            ));
        }
    }
    std::fs::read(path).map_err(|error| format!("{flag} {}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secret(dir: &Path, name: &str, bytes: &[u8], mode: u32) -> std::path::PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        }
        #[cfg(not(unix))]
        let _ = mode;
        path
    }

    fn flags(token: Option<&Path>, key: Option<&Path>, origins: &[&str]) -> Flags {
        let mut args: Vec<String> = Vec::new();
        if let Some(token) = token {
            args.extend(["--token-file".into(), token.display().to_string()]);
        }
        if let Some(key) = key {
            args.extend(["--grant-key-file".into(), key.display().to_string()]);
        }
        for origin in origins {
            args.extend(["--allow-origin".into(), origin.to_string()]);
        }
        crate::parse_flags(&args).unwrap()
    }

    const TOKEN: &[u8] = b"a-bearer-token-of-thirty-two-chars+\n";

    #[test]
    fn no_flags_is_the_open_daemon() {
        let policy = load_policy(&flags(None, None, &[])).unwrap();
        assert!(!policy.bearer_required() && !policy.grants_enabled());
        assert!(policy.extra_origins().is_empty());
    }

    #[test]
    fn owner_only_files_configure_the_bearer_grants_and_origins() {
        let dir = tempfile::TempDir::new().unwrap();
        let token = secret(dir.path(), "token", TOKEN, 0o600);
        let key = secret(dir.path(), "key", &[9u8; 32], 0o400);
        let policy =
            load_policy(&flags(Some(&token), Some(&key), &["vibefield-app://shell"])).unwrap();
        assert!(policy.bearer_required() && policy.grants_enabled());
        assert_eq!(policy.extra_origins(), ["vibefield-app://shell"]);
    }

    #[cfg(unix)]
    #[test]
    fn a_file_others_can_read_or_write_is_refused() {
        let dir = tempfile::TempDir::new().unwrap();
        for mode in [0o644, 0o640, 0o604, 0o620, 0o602, 0o660] {
            let token = secret(dir.path(), "token", TOKEN, mode);
            let error = load_policy(&flags(Some(&token), None, &[])).err().unwrap();
            assert!(
                error.starts_with("--token-file") && error.contains("chmod 600"),
                "{mode:o}: {error}"
            );
            let good = secret(dir.path(), "token-ok", TOKEN, 0o600);
            let key = secret(dir.path(), "key", &[9u8; 32], mode);
            let error = load_policy(&flags(Some(&good), Some(&key), &[]))
                .err()
                .unwrap();
            assert!(
                error.starts_with("--grant-key-file") && error.contains("chmod 600"),
                "{mode:o}: {error}"
            );
        }
    }

    #[test]
    fn bad_secrets_and_origins_name_their_flag() {
        let dir = tempfile::TempDir::new().unwrap();
        let token = secret(dir.path(), "token", TOKEN, 0o600);
        let short = secret(dir.path(), "short", b"too short\n", 0o600);
        let key = secret(dir.path(), "key", &[9u8; 32], 0o600);
        let short_key = secret(dir.path(), "short-key", &[9u8; 31], 0o600);
        let huge = secret(dir.path(), "huge", &[b'x'; 5000], 0o600);
        let cases: [(Flags, &str); 7] = [
            (flags(Some(&short), None, &[]), "--token-file"),
            (flags(Some(&huge), None, &[]), "--token-file"),
            (
                flags(Some(&dir.path().join("absent")), None, &[]),
                "--token-file",
            ),
            (flags(Some(dir.path()), None, &[]), "--token-file"),
            (
                flags(None, Some(&key), &[]),
                "--grant-key-file needs --token-file",
            ),
            (
                flags(Some(&token), Some(&short_key), &[]),
                "--grant-key-file",
            ),
            (flags(Some(&token), None, &["null"]), "--allow-origin"),
        ];
        for (flags, prefix) in cases {
            let error = load_policy(&flags).err().unwrap();
            assert!(error.starts_with(prefix), "{prefix}: {error}");
        }
    }
}
